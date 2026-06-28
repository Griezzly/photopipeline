//! Per-folder library resolution: maps a photo folder to its DuckDB catalog
//! and preview cache in OS app-data, and lists/locates libraries.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use xxhash_rust::xxh3::xxh3_128;

use crate::cache::Cache;
use crate::catalog::Catalog;

/// Root directories under which all libraries live. Production uses
/// `from_dirs()`; tests pass explicit temp roots.
#[derive(Debug, Clone)]
pub struct LibraryRoots {
    /// Holds catalogs (precious): `<data>/libraries/<key>/catalog.duckdb`.
    pub data: PathBuf,
    /// Holds preview caches (regenerable): `<cache>/libraries/<key>/`.
    pub cache: PathBuf,
}

impl LibraryRoots {
    /// OS-appropriate roots: data dir + cache dir, each under `photopipe/`.
    pub fn from_dirs() -> Result<Self> {
        let data = dirs::data_dir().context("cannot determine OS data dir")?.join("photopipe");
        let cache = dirs::cache_dir().context("cannot determine OS cache dir")?.join("photopipe");
        Ok(Self { data, cache })
    }

    fn catalog_path(&self, key: &str) -> PathBuf {
        self.data.join("libraries").join(key).join("catalog.duckdb")
    }
    fn cache_dir(&self, key: &str) -> PathBuf {
        self.cache.join("libraries").join(key)
    }
    fn libraries_dir(&self) -> PathBuf {
        self.data.join("libraries")
    }
}

/// An opened library: its folder plus the catalog and preview cache.
pub struct Library {
    pub folder: PathBuf,
    pub catalog: Catalog,
    pub cache: Cache,
}

/// Summary of a library, for listing.
#[derive(Debug, Clone)]
pub struct LibraryInfo {
    pub folder: PathBuf,
    pub key: String,
    pub created_at: i64,
    pub last_analyzed: Option<i64>,
    pub photo_count: i64,
}

/// Normalize a folder path to a stable absolute form for hashing.
fn canonical_path(folder: &Path) -> PathBuf {
    if let Ok(c) = std::fs::canonicalize(folder) {
        return c;
    }
    if folder.is_absolute() {
        return folder.to_path_buf();
    }
    match std::env::current_dir() {
        Ok(cwd) => cwd.join(folder),
        Err(_) => folder.to_path_buf(),
    }
}

/// Stable per-folder key: 128-bit xxh3 of the canonical path, lowercase hex.
pub fn library_key(folder: &Path) -> String {
    let canon = canonical_path(folder);
    format!("{:032x}", xxh3_128(canon.to_string_lossy().as_bytes()))
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

/// Open (creating if needed) the library for `folder`, recording its path.
pub fn open_or_create_library(roots: &LibraryRoots, folder: &Path) -> Result<Library> {
    let key = library_key(folder);
    let catalog_path = roots.catalog_path(&key);
    if let Some(parent) = catalog_path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let catalog = Catalog::open(&catalog_path).map_err(|e| anyhow::anyhow!("catalog: {e}"))?;
    let cache = Cache::open(roots.cache_dir(&key)).context("cache")?;
    let folder_str = canonical_path(folder).to_string_lossy().into_owned();
    catalog
        .set_library_meta(&folder_str, now_secs())
        .map_err(|e| anyhow::anyhow!("library_meta: {e}"))?;
    Ok(Library { folder: folder.to_path_buf(), catalog, cache })
}

/// Open the library for `folder` only if it already exists.
pub fn open_existing_library(roots: &LibraryRoots, folder: &Path) -> Result<Option<Library>> {
    let key = library_key(folder);
    let catalog_path = roots.catalog_path(&key);
    if !catalog_path.exists() {
        return Ok(None);
    }
    let catalog = Catalog::open(&catalog_path).map_err(|e| anyhow::anyhow!("catalog: {e}"))?;
    let cache = Cache::open(roots.cache_dir(&key)).context("cache")?;
    Ok(Some(Library { folder: folder.to_path_buf(), catalog, cache }))
}

/// List all libraries by reading each catalog's `library_meta`.
pub fn list_libraries(roots: &LibraryRoots) -> Result<Vec<LibraryInfo>> {
    let mut out = Vec::new();
    let rd = match std::fs::read_dir(roots.libraries_dir()) {
        Ok(rd) => rd,
        Err(_) => return Ok(out), // no libraries yet
    };
    for entry in rd.flatten() {
        let key = entry.file_name().to_string_lossy().into_owned();
        let catalog_path = entry.path().join("catalog.duckdb");
        if !catalog_path.exists() {
            continue;
        }
        let catalog = match Catalog::open(&catalog_path) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(dir = %entry.path().display(), error = %e, "skipping unreadable library");
                continue;
            }
        };
        let Some((folder_path, created_at, last_analyzed)) = catalog.library_meta().ok().flatten()
        else {
            continue;
        };
        let photo_count = catalog.file_count().unwrap_or(0);
        out.push(LibraryInfo {
            folder: PathBuf::from(folder_path),
            key,
            created_at,
            last_analyzed,
            photo_count,
        });
    }
    out.sort_by_key(|b| std::cmp::Reverse(b.last_analyzed));
    Ok(out)
}

/// Find the nearest ancestor of `file` that has a library.
pub fn find_library_for_file(roots: &LibraryRoots, file: &Path) -> Result<Option<PathBuf>> {
    let mut cur = if file.is_dir() { Some(file) } else { file.parent() };
    while let Some(dir) = cur {
        if roots.catalog_path(&library_key(dir)).exists() {
            return Ok(Some(dir.to_path_buf()));
        }
        cur = dir.parent();
    }
    Ok(None)
}
