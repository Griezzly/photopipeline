pub mod exif;
pub mod hash;
pub mod preview;

use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Mutex,
    },
    time::UNIX_EPOCH,
};

use rayon::prelude::*;
use walkdir::WalkDir;

use crate::{
    cache::Cache,
    catalog::Catalog,
    config::{IngestConfig, SidecarJpgMode},
};

pub use exif::ExifData;

// ── file format ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileFormat {
    Arw,
    Cr3,
    Cr2,
    Nef,
    Raf,
    Rw2,
    Dng,
    Jpg,
}

impl FileFormat {
    pub fn from_ext(ext: &str) -> Option<Self> {
        match ext.to_lowercase().as_str() {
            "arw" => Some(Self::Arw),
            "cr3" => Some(Self::Cr3),
            "cr2" => Some(Self::Cr2),
            "nef" => Some(Self::Nef),
            "raf" => Some(Self::Raf),
            "rw2" => Some(Self::Rw2),
            "dng" => Some(Self::Dng),
            "jpg" | "jpeg" => Some(Self::Jpg),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Arw => "arw",
            Self::Cr3 => "cr3",
            Self::Cr2 => "cr2",
            Self::Nef => "nef",
            Self::Raf => "raf",
            Self::Rw2 => "rw2",
            Self::Dng => "dng",
            Self::Jpg => "jpg",
        }
    }

    pub fn is_raw(&self) -> bool {
        !matches!(self, Self::Jpg)
    }
}

// ── ingested file ─────────────────────────────────────────────────────────────

#[derive(Debug)]
pub struct IngestedFile {
    pub path: PathBuf,
    pub content_hash: u128,
    pub size: u64,
    pub mtime_ns: i64,
    pub format: FileFormat,
    pub has_sidecar_jpg: bool,
}

// ── report ────────────────────────────────────────────────────────────────────

#[derive(Debug, Default)]
pub struct IngestReport {
    pub processed: u64,
    pub skipped: u64,
    pub errored: u64,
}

// ── public API ────────────────────────────────────────────────────────────────

/// Walk `roots`, ingest every supported file into `catalog`, and write WebP
/// previews into `cache`.  Returns a summary of what happened.
pub fn ingest_directory(
    roots: &[PathBuf],
    catalog: &Catalog,
    cache: &Cache,
    cfg: &IngestConfig,
    progress: Option<&dyn crate::analyze::ProgressSink>,
) -> anyhow::Result<IngestReport> {
    let batch_size: usize = 64;

    let processed = AtomicU64::new(0);
    let skipped = AtomicU64::new(0);
    let errored = AtomicU64::new(0);

    // ── collect candidate paths ──────────────────────────────────────────────
    let mut paths: Vec<PathBuf> = Vec::new();
    for root in roots {
        for entry in WalkDir::new(root)
            .follow_links(cfg.follow_symlinks)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            if !entry.file_type().is_file() {
                continue;
            }
            let path = entry.path();
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
            if cfg.extensions.iter().any(|x| x.eq_ignore_ascii_case(ext)) {
                paths.push(path.to_owned());
            }
        }
    }

    tracing::info!(total = paths.len(), "ingest: files found");

    // ── drop sidecar JPGs (RAW + same-name JPG → keep RAW only) ─────────────
    let paths = exclude_sidecar_jpgs(paths);

    if let Some(p) = progress {
        p.set_total(paths.len() as u64);
    }

    // ── process in parallel ──────────────────────────────────────────────────
    let batch: Mutex<Vec<(IngestedFile, Option<ExifData>)>> = Mutex::new(Vec::new());

    paths.par_iter().for_each(|path| {
        if let Some(p) = progress {
            p.inc();
        }
        match process_file(path, catalog, cache, cfg) {
            Ok(None) => {
                skipped.fetch_add(1, Ordering::Relaxed);
            }
            Ok(Some((file, exif_data))) => {
                processed.fetch_add(1, Ordering::Relaxed);
                let mut b = batch.lock().unwrap();
                b.push((file, exif_data));
                if b.len() >= batch_size {
                    let to_flush = std::mem::take(&mut *b);
                    drop(b); // release lock before I/O
                    if let Err(e) = catalog.flush_batch(&to_flush) {
                        let n = to_flush.len() as u64;
                        tracing::warn!(
                            error = %e,
                            first_file = %to_flush[0].0.path.display(),
                            batch_size = n,
                            "catalog flush failed; batch counted as errored"
                        );
                        processed.fetch_sub(n, Ordering::Relaxed);
                        errored.fetch_add(n, Ordering::Relaxed);
                    }
                }
            }
            Err(e) => {
                errored.fetch_add(1, Ordering::Relaxed);
                tracing::warn!(path = %path.display(), error = %e, "file processing error");
            }
        }
    });

    // ── flush remaining batch ─────────────────────────────────────────────────
    let remaining = batch.into_inner().unwrap();
    if !remaining.is_empty() {
        if let Err(e) = catalog.flush_batch(&remaining) {
            let n = remaining.len() as u64;
            tracing::warn!(
                error = %e,
                first_file = %remaining[0].0.path.display(),
                batch_size = n,
                "final catalog flush failed; batch counted as errored"
            );
            processed.fetch_sub(n, Ordering::Relaxed);
            errored.fetch_add(n, Ordering::Relaxed);
        }
    }

    Ok(IngestReport {
        processed: processed.load(Ordering::Relaxed),
        skipped: skipped.load(Ordering::Relaxed),
        errored: errored.load(Ordering::Relaxed),
    })
}

// ── per-file processing ───────────────────────────────────────────────────────

fn process_file(
    path: &Path,
    catalog: &Catalog,
    cache: &Cache,
    cfg: &IngestConfig,
) -> Result<Option<(IngestedFile, Option<ExifData>)>, crate::error::IngestError> {
    let meta = std::fs::metadata(path).map_err(|e| crate::error::IngestError::Io {
        path: path.to_owned(),
        source: e,
    })?;
    let size = meta.len();
    let mtime_ns = meta
        .modified()
        .map_err(|e| crate::error::IngestError::Io {
            path: path.to_owned(),
            source: e,
        })?
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0);

    // ── idempotency check ─────────────────────────────────────────────────────
    let needs = catalog
        .needs_processing(path, mtime_ns, size)
        .map_err(|e| crate::error::IngestError::Io {
            path: path.to_owned(),
            source: std::io::Error::other(e.to_string()),
        })?;
    if !needs {
        return Ok(None);
    }

    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let format = FileFormat::from_ext(ext).ok_or_else(|| crate::error::IngestError::Io {
        path: path.to_owned(),
        source: std::io::Error::new(std::io::ErrorKind::InvalidInput, "unsupported extension"),
    })?;

    // ── hash ──────────────────────────────────────────────────────────────────
    let content_hash = hash::hash_file(path).map_err(|e| crate::error::IngestError::Io {
        path: path.to_owned(),
        source: e,
    })?;

    // ── sidecar detection ─────────────────────────────────────────────────────
    let has_sidecar_jpg = detect_sidecar_jpg(path);

    // ── EXIF ──────────────────────────────────────────────────────────────────
    let mut exif_data: Option<ExifData> = if format.is_raw() {
        exif::read_exif_raw(path).ok()
    } else {
        exif::read_exif_jpg(path).ok()
    };

    // ── preview extraction ────────────────────────────────────────────────────
    let preview_result: Result<image::DynamicImage, _> = {
        if has_sidecar_jpg && cfg.sidecar_jpg == SidecarJpgMode::Prefer {
            let sidecar = find_sidecar_jpg(path).unwrap();
            preview::extract_preview_jpg(&sidecar, cfg.preview_max_long_edge)
        } else if format.is_raw() {
            preview::extract_preview_raw(path, cfg.preview_max_long_edge)
        } else {
            preview::extract_preview_jpg(path, cfg.preview_max_long_edge)
        }
    };

    match preview_result {
        Ok(img) => {
            // Backfill dimensions from the preview.
            if let Some(ref mut ed) = exif_data {
                if ed.width.is_none() {
                    ed.width = Some(img.width());
                }
                if ed.height.is_none() {
                    ed.height = Some(img.height());
                }
            }
            // Write to cache only if not already present.
            if !cache.has(content_hash) {
                match preview::encode_webp(&img, cfg.preview_quality) {
                    Ok(bytes) => {
                        if let Err(e) = cache.write(content_hash, &bytes) {
                            tracing::warn!(
                                path = %path.display(),
                                error = %e,
                                "cache write failed"
                            );
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            path = %path.display(),
                            error = %e,
                            "webp encode failed"
                        );
                    }
                }
            }
        }
        Err(e) => {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "preview extraction failed"
            );
        }
    }

    Ok(Some((
        IngestedFile {
            path: path.to_owned(),
            content_hash,
            size,
            mtime_ns,
            format,
            has_sidecar_jpg,
        },
        exif_data,
    )))
}

// ── sidecar helpers ───────────────────────────────────────────────────────────

/// Returns `true` if a JPEG sidecar for `path` exists alongside it.
pub fn detect_sidecar_jpg(path: &Path) -> bool {
    find_sidecar_jpg(path).is_some()
}

fn find_sidecar_jpg(path: &Path) -> Option<PathBuf> {
    let stem = path.file_stem()?;
    let dir = path.parent()?;
    for ext in &["jpg", "jpeg", "JPG", "JPEG"] {
        let candidate = dir.join(format!("{}.{}", stem.to_string_lossy(), ext));
        if candidate.exists() && candidate != path {
            return Some(candidate);
        }
    }
    None
}

// ── sidecar JPG exclusion ─────────────────────────────────────────────────────

/// Drop any `.jpg`/`.jpeg` whose same-stem RAW sibling is also in `paths`
/// (same parent dir, case-insensitive stem). RAWs and standalone JPGs are kept.
fn exclude_sidecar_jpgs(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    use std::collections::HashSet;

    // Build a set of (parent_dir, lowercased_stem) for every RAW path.
    let raw_keys: HashSet<(PathBuf, String)> = paths
        .iter()
        .filter(|p| {
            let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
            FileFormat::from_ext(ext)
                .map(|f| f.is_raw())
                .unwrap_or(false)
        })
        .filter_map(|p| {
            let parent = p.parent()?.to_owned();
            let stem = p.file_stem()?.to_string_lossy().to_ascii_lowercase();
            Some((parent, stem))
        })
        .collect();

    let before = paths.len();
    let result: Vec<PathBuf> = paths
        .into_iter()
        .filter(|p| {
            let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
            let is_jpg = matches!(ext.to_lowercase().as_str(), "jpg" | "jpeg");
            if !is_jpg {
                return true;
            }
            // Keep this JPG only if there is no RAW sibling in the same dir.
            let key = p.parent().and_then(|parent| {
                let stem = p.file_stem()?.to_string_lossy().to_ascii_lowercase();
                Some((parent.to_owned(), stem))
            });
            match key {
                Some(k) => !raw_keys.contains(&k),
                None => true,
            }
        })
        .collect();

    let excluded = before - result.len();
    if excluded > 0 {
        tracing::info!(
            kept = result.len(),
            excluded,
            "ingest: sidecar JPGs excluded from catalog"
        );
    }

    result
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn sidecar_detection() {
        let dir = TempDir::new().unwrap();
        let raw = dir.path().join("IMG_001.ARW");
        let jpg = dir.path().join("IMG_001.jpg");
        fs::write(&raw, b"fake").unwrap();
        assert!(!detect_sidecar_jpg(&raw));
        fs::write(&jpg, b"fake").unwrap();
        assert!(detect_sidecar_jpg(&raw));
    }

    #[test]
    fn file_format_from_ext() {
        assert_eq!(FileFormat::from_ext("arw"), Some(FileFormat::Arw));
        assert_eq!(FileFormat::from_ext("ARW"), Some(FileFormat::Arw));
        assert_eq!(FileFormat::from_ext("JPEG"), Some(FileFormat::Jpg));
        assert_eq!(FileFormat::from_ext("xyz"), None);
    }

    // ── exclude_sidecar_jpgs tests ────────────────────────────────────────────

    #[test]
    fn exclude_drops_jpg_when_raw_sibling_present() {
        let paths = vec![
            PathBuf::from("/photos/X.ARW"),
            PathBuf::from("/photos/X.JPG"),
        ];
        let result = exclude_sidecar_jpgs(paths);
        assert_eq!(result, vec![PathBuf::from("/photos/X.ARW")]);
    }

    #[test]
    fn exclude_keeps_standalone_jpg() {
        let paths = vec![PathBuf::from("/photos/Z.JPG")];
        let result = exclude_sidecar_jpgs(paths);
        assert_eq!(result, vec![PathBuf::from("/photos/Z.JPG")]);
    }

    #[test]
    fn exclude_case_insensitive_stem_and_ext() {
        // ARW + lowercase jpg: jpg dropped
        let paths = vec![
            PathBuf::from("/photos/DSC1.ARW"),
            PathBuf::from("/photos/DSC1.jpg"),
        ];
        let result = exclude_sidecar_jpgs(paths);
        assert!(result.contains(&PathBuf::from("/photos/DSC1.ARW")));
        assert!(!result.contains(&PathBuf::from("/photos/DSC1.jpg")));

        // ARW + uppercase JPG: also dropped
        let paths2 = vec![
            PathBuf::from("/photos/DSC2.ARW"),
            PathBuf::from("/photos/DSC2.JPG"),
        ];
        let result2 = exclude_sidecar_jpgs(paths2);
        assert!(result2.contains(&PathBuf::from("/photos/DSC2.ARW")));
        assert!(!result2.contains(&PathBuf::from("/photos/DSC2.JPG")));
    }

    #[test]
    fn exclude_different_dirs_same_stem_both_kept() {
        // a.ARW in dir1, a.JPG in dir2 — not siblings, both kept
        let paths = vec![PathBuf::from("/dir1/a.ARW"), PathBuf::from("/dir2/a.JPG")];
        let result = exclude_sidecar_jpgs(paths);
        assert!(result.contains(&PathBuf::from("/dir1/a.ARW")));
        assert!(result.contains(&PathBuf::from("/dir2/a.JPG")));
    }

    #[test]
    fn exclude_always_keeps_the_raw() {
        let paths = vec![
            PathBuf::from("/photos/IMG_5000.CR3"),
            PathBuf::from("/photos/IMG_5000.jpg"),
            PathBuf::from("/photos/IMG_5001.NEF"),
        ];
        let result = exclude_sidecar_jpgs(paths);
        assert!(result.contains(&PathBuf::from("/photos/IMG_5000.CR3")));
        assert!(result.contains(&PathBuf::from("/photos/IMG_5001.NEF")));
        assert!(!result.contains(&PathBuf::from("/photos/IMG_5000.jpg")));
    }
}
