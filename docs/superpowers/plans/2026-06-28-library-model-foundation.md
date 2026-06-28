# Library-model foundation + CLI migration — Implementation Plan (Spec 1 of 2)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the single config-defined catalog with **per-folder libraries** resolved from the folder path and stored in OS app-data (self-describing via a `library_meta` table), and migrate every CLI command onto this model.

**Architecture:** A new `pipeline::library` module maps a folder → `xxh3(canonical path)` → `<data_dir>/photopipe/libraries/<key>/catalog.duckdb` (+ preview cache under `<cache_dir>/…`). Each catalog stores its own folder path in a `library_meta` row (schema v3). CLI commands take a `<folder>` and resolve its library; `[catalog] db_path`/`cache_dir` are retired. Storage roots are passed as an explicit `LibraryRoots` value (production uses `from_dirs()`, tests use temp roots).

**Tech Stack:** Rust (edition 2021), DuckDB, existing deps `dirs` + `xxhash-rust` (xxh3) — **no new dependencies**.

## Global Constraints

Apply to **every** task (from the spec + `CLAUDE.md`):

- **DuckDB only. No SQLite. No Python at runtime. No new dependencies** (reuse `dirs`, `xxhash-rust`).
- **Persistent state is 100% DuckDB** — no JSON index files. Libraries are self-describing via `library_meta`.
- **Non-destructive:** libraries live entirely in OS app-data; nothing is ever written into the photo folder.
- Schema migrations are atomic (`BEGIN TRANSACTION; … COMMIT;`); bulk inserts use the Appender; never leave half-written rows.
- `anyhow::Result` at the CLI boundary; `thiserror`-based `CatalogError`/`IngestError` inside `pipeline`. `tracing` for logs; `println!` **only** for user-facing CLI output.
- Read-only commands on a not-yet-scanned folder must error clearly (`no library for <folder> — run 'photopipe scan <folder>' first`) and exit non-zero — never panic, never create an empty library.
- Removing config keys must not break existing configs (keep `#[serde(default)]`, do **not** add `deny_unknown_fields`).
- **Out of scope (spec 2):** server folder-browser, background analyze job/progress, active-library switching, the home/browse/analyze SPA. `serve <folder>` here just opens one library and serves the existing review UI.
- Before done: `cargo fmt`, `cargo clippy --all-targets --all-features -- -D warnings`, `cargo test --all` green.
- One git commit per task, conventional-commit style.

---

## File Structure

- `crates/pipeline/src/catalog/schema.rs` — append migration v3 (`library_meta`). (Task 1)
- `crates/pipeline/src/catalog/mod.rs` — `set_library_meta` / `set_last_analyzed` / `library_meta`. (Task 1)
- `crates/cli/src/main.rs:13` — bump `EXPECTED_SCHEMA_VERSION` to 3 (Task 1); migrate handlers (Tasks 3–4); remove the const + schema check (Task 3).
- `crates/pipeline/src/library.rs` — **new** resolver module. (Task 2)
- `crates/pipeline/src/lib.rs` — `pub mod library;` + re-exports. (Task 2)
- `crates/cli/src/serve/mod.rs` — `serve <folder>` opens the resolved library. (Task 3)
- `crates/pipeline/src/config.rs` — drop `db_path`/`cache_dir` from `CatalogConfig`. (Task 5)
- `photopipe.example.toml`, `README.md` — docs. (Task 5)
- `crates/cli/tests/cli.rs` — rewrite to the library model (sandboxed via temp `XDG_*`). (Tasks 3–4)

---

## Task 1: `library_meta` table (schema v3) + catalog methods

**Files:**
- Modify: `crates/pipeline/src/catalog/schema.rs`
- Modify: `crates/pipeline/src/catalog/mod.rs`
- Modify: `crates/cli/src/main.rs:13`
- Test: `crates/pipeline/tests/library_meta.rs` (create)

**Interfaces:**
- Consumes: existing `Catalog::open`, `optional_row`, `Catalog::schema_version`.
- Produces:
  - `Catalog::set_library_meta(&self, folder_path: &str, created_at: i64) -> Result<(), CatalogError>` (inserts the single row only if none exists).
  - `Catalog::set_last_analyzed(&self, ts: i64) -> Result<(), CatalogError>`.
  - `Catalog::library_meta(&self) -> Result<Option<(String, i64, Option<i64>)>, CatalogError>` → `(folder_path, created_at, last_analyzed)`.
  - Catalog opens at schema version 3.

- [ ] **Step 1: Write the failing test**

Create `crates/pipeline/tests/library_meta.rs`:

```rust
use pipeline::catalog::Catalog;
use tempfile::TempDir;

#[test]
fn library_meta_roundtrips_at_v3() {
    let dir = TempDir::new().unwrap();
    let catalog = Catalog::open(&dir.path().join("c.duckdb")).unwrap();
    assert_eq!(catalog.schema_version().unwrap(), 3);

    // No meta yet.
    assert!(catalog.library_meta().unwrap().is_none());

    catalog.set_library_meta("/photos/trip", 1000).unwrap();
    // Second call must NOT overwrite the existing row.
    catalog.set_library_meta("/photos/other", 2000).unwrap();
    let (folder, created, last) = catalog.library_meta().unwrap().unwrap();
    assert_eq!(folder, "/photos/trip");
    assert_eq!(created, 1000);
    assert_eq!(last, None);

    catalog.set_last_analyzed(5555).unwrap();
    assert_eq!(catalog.library_meta().unwrap().unwrap().2, Some(5555));
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `. ~/.cargo/env && cargo test -p pipeline --test library_meta`
Expected: FAIL — `library_meta`/`set_library_meta` don't exist and schema is 2.

- [ ] **Step 3: Add migration v3**

In `crates/pipeline/src/catalog/schema.rs`, append a third element to the `MIGRATIONS` array (after the version-2 string):

```rust
    // version 3 — per-folder library identity
    "BEGIN TRANSACTION;
     INSERT INTO schema_version VALUES (3);
     CREATE TABLE library_meta (
         folder_path   VARCHAR NOT NULL,
         created_at    BIGINT  NOT NULL,
         last_analyzed BIGINT
     );
     COMMIT;",
```

- [ ] **Step 4: Add the catalog methods**

In `crates/pipeline/src/catalog/mod.rs`, inside `impl Catalog`, add:

```rust
    /// Record the library's folder path once. No-op if a row already exists.
    pub fn set_library_meta(&self, folder_path: &str, created_at: i64) -> Result<(), CatalogError> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| CatalogError::Db("mutex poisoned".into()))?;
        conn.execute(
            "INSERT INTO library_meta (folder_path, created_at, last_analyzed)
             SELECT ?, ?, NULL WHERE NOT EXISTS (SELECT 1 FROM library_meta)",
            duckdb::params![folder_path, created_at],
        )
        .map_err(|e| CatalogError::Db(e.to_string()))?;
        Ok(())
    }

    /// Update the `last_analyzed` timestamp on the (single) meta row.
    pub fn set_last_analyzed(&self, ts: i64) -> Result<(), CatalogError> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| CatalogError::Db("mutex poisoned".into()))?;
        conn.execute("UPDATE library_meta SET last_analyzed = ?", duckdb::params![ts])
            .map_err(|e| CatalogError::Db(e.to_string()))?;
        Ok(())
    }

    /// Read `(folder_path, created_at, last_analyzed)` if present.
    pub fn library_meta(&self) -> Result<Option<(String, i64, Option<i64>)>, CatalogError> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| CatalogError::Db("mutex poisoned".into()))?;
        let row = conn.query_row(
            "SELECT folder_path, created_at, last_analyzed FROM library_meta LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        );
        optional_row(row)
    }
```

- [ ] **Step 5: Bump the expected schema version**

In `crates/cli/src/main.rs` line 13, change:

```rust
const EXPECTED_SCHEMA_VERSION: u32 = 3;
```

- [ ] **Step 6: Run tests**

Run: `. ~/.cargo/env && cargo test -p pipeline --test library_meta && cargo test -p photopipe`
Expected: PASS (new test passes; existing CLI/doctor tests still pass — catalogs migrate to v3 and `EXPECTED_SCHEMA_VERSION` matches).

- [ ] **Step 7: Commit**

```bash
git add crates/pipeline/src/catalog/schema.rs crates/pipeline/src/catalog/mod.rs crates/cli/src/main.rs crates/pipeline/tests/library_meta.rs
git commit -m "feat(catalog): library_meta table (schema v3)"
```

---

## Task 2: `pipeline::library` resolver module

**Files:**
- Create: `crates/pipeline/src/library.rs`
- Modify: `crates/pipeline/src/lib.rs`
- Test: `crates/pipeline/tests/library.rs` (create)

**Interfaces:**
- Consumes: `Catalog`, `Cache`, `Catalog::{set_library_meta, library_meta, file_count}`, `dirs`, `xxhash_rust::xxh3::xxh3_128`.
- Produces (all re-exported from `lib.rs`):
  - `pub struct LibraryRoots { pub data: PathBuf, pub cache: PathBuf }` with `LibraryRoots::from_dirs() -> anyhow::Result<Self>`.
  - `pub struct Library { pub folder: PathBuf, pub catalog: Catalog, pub cache: Cache }`.
  - `pub struct LibraryInfo { pub folder: PathBuf, pub key: String, pub created_at: i64, pub last_analyzed: Option<i64>, pub photo_count: i64 }`.
  - `pub fn library_key(folder: &Path) -> String`.
  - `pub fn open_or_create_library(roots: &LibraryRoots, folder: &Path) -> anyhow::Result<Library>`.
  - `pub fn open_existing_library(roots: &LibraryRoots, folder: &Path) -> anyhow::Result<Option<Library>>`.
  - `pub fn list_libraries(roots: &LibraryRoots) -> anyhow::Result<Vec<LibraryInfo>>`.
  - `pub fn find_library_for_file(roots: &LibraryRoots, file: &Path) -> anyhow::Result<Option<PathBuf>>`.

- [ ] **Step 1: Write the failing tests**

Create `crates/pipeline/tests/library.rs`:

```rust
use pipeline::library::{
    find_library_for_file, library_key, list_libraries, open_existing_library,
    open_or_create_library, LibraryRoots,
};
use std::path::PathBuf;
use tempfile::TempDir;

fn temp_roots(d: &TempDir) -> LibraryRoots {
    LibraryRoots { data: d.path().join("data"), cache: d.path().join("cache") }
}

#[test]
fn key_is_stable_and_path_normalized() {
    let d = TempDir::new().unwrap();
    let lib = d.path().join("Photos");
    std::fs::create_dir_all(&lib).unwrap();
    // Same folder via a trailing slash → same key (both canonicalize).
    let with_slash = PathBuf::from(format!("{}/", lib.display()));
    assert_eq!(library_key(&lib), library_key(&with_slash));
    // A different folder → different key.
    let other = d.path().join("Other");
    std::fs::create_dir_all(&other).unwrap();
    assert_ne!(library_key(&lib), library_key(&other));
}

#[test]
fn open_existing_is_none_until_created() {
    let d = TempDir::new().unwrap();
    let roots = temp_roots(&d);
    let folder = d.path().join("lib");
    std::fs::create_dir_all(&folder).unwrap();
    assert!(open_existing_library(&roots, &folder).unwrap().is_none());

    let lib = open_or_create_library(&roots, &folder).unwrap();
    // Catalog landed under the data root, cache under the cache root.
    assert!(d.path().join("data/libraries").exists());
    assert!(d.path().join("cache/libraries").exists());
    // Meta records the canonical folder path.
    let (fp, _, last) = lib.catalog.library_meta().unwrap().unwrap();
    assert_eq!(fp, std::fs::canonicalize(&folder).unwrap().to_string_lossy());
    assert_eq!(last, None);
    assert!(open_existing_library(&roots, &folder).unwrap().is_some());
}

#[test]
fn list_and_find() {
    let d = TempDir::new().unwrap();
    let roots = temp_roots(&d);
    let a = d.path().join("a");
    let b = d.path().join("b/inner");
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    open_or_create_library(&roots, &a).unwrap();
    open_or_create_library(&roots, &b).unwrap();

    let libs = list_libraries(&roots).unwrap();
    assert_eq!(libs.len(), 2);
    assert!(libs.iter().any(|l| l.folder == std::fs::canonicalize(&a).unwrap()));

    // A file under `a` resolves to `a`; the deepest library wins.
    let f = a.join("sub/x.jpg");
    std::fs::create_dir_all(f.parent().unwrap()).unwrap();
    std::fs::write(&f, b"x").unwrap();
    let found = find_library_for_file(&roots, &f).unwrap().unwrap();
    assert_eq!(found, std::fs::canonicalize(&a).unwrap());

    // A file under no library → None.
    let outside = d.path().join("nope/y.jpg");
    std::fs::create_dir_all(outside.parent().unwrap()).unwrap();
    std::fs::write(&outside, b"y").unwrap();
    assert!(find_library_for_file(&roots, &outside).unwrap().is_none());
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `. ~/.cargo/env && cargo test -p pipeline --test library`
Expected: FAIL — the `pipeline::library` module doesn't exist.

- [ ] **Step 3: Create the module**

Create `crates/pipeline/src/library.rs`:

```rust
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
    out.sort_by(|a, b| b.last_analyzed.cmp(&a.last_analyzed));
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
```

- [ ] **Step 4: Register + re-export the module**

In `crates/pipeline/src/lib.rs`, add `pub mod library;` to the module list, and add a re-export line:

```rust
pub use library::{
    find_library_for_file, library_key, list_libraries, open_existing_library,
    open_or_create_library, Library, LibraryInfo, LibraryRoots,
};
```

- [ ] **Step 5: Run tests**

Run: `. ~/.cargo/env && cargo test -p pipeline --test library && cargo clippy -p pipeline --all-targets -- -D warnings`
Expected: PASS, clippy clean.

- [ ] **Step 6: Commit**

```bash
git add crates/pipeline/src/library.rs crates/pipeline/src/lib.rs crates/pipeline/tests/library.rs
git commit -m "feat(library): per-folder library resolver (paths, open/list/find)"
```

---

## Task 3: Migrate `scan`, `serve`, `doctor`; add `libraries`; sandbox the CLI tests

**Files:**
- Modify: `crates/cli/src/main.rs`
- Modify: `crates/cli/src/serve/mod.rs`
- Test: `crates/cli/tests/cli.rs`

**Interfaces:**
- Consumes: `pipeline::library::{LibraryRoots, open_or_create_library, list_libraries}`, `Catalog::set_last_analyzed`.
- Produces: `scan <folder>...` creates one library per folder; `serve <folder>` opens that library; `libraries` lists; `doctor` no longer checks a fixed catalog. `main` builds one `LibraryRoots` and passes `&roots` to handlers. Read-only commands `calibrate/dedupe/stats/info/review-tree/export-keepers` are unchanged in this task (still use `cfg.catalog.db_path`, which still exists) — they are migrated in Task 4.

- [ ] **Step 1: Write/rewrite the failing tests**

Replace the contents of `crates/cli/tests/cli.rs` with the library-model test harness below (it sandboxes app-data via `XDG_DATA_HOME`/`XDG_CACHE_HOME` on the spawned process, and drives the binary end-to-end with `scan --no-models`):

```rust
use std::path::Path;
use std::process::{Command, Output};

use image::{ImageBuffer, Rgb};

/// A config that only sets the model dir (catalog paths are no longer config).
fn write_config(dir: &Path) -> std::path::PathBuf {
    let cfg_path = dir.join("photopipe.toml");
    std::fs::write(
        &cfg_path,
        format!("[models]\nmodel_dir = \"{}\"\n", dir.join("models").display()),
    )
    .unwrap();
    cfg_path
}

/// Run the binary with app-data redirected into `appdata` (so no real
/// libraries are touched). `cfg` is the config path.
fn run_pp(appdata: &Path, cfg: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_photopipe"))
        .env("XDG_DATA_HOME", appdata.join("data"))
        .env("XDG_CACHE_HOME", appdata.join("cache"))
        .args(["--config", cfg.to_str().unwrap()])
        .args(args)
        .output()
        .expect("spawn photopipe")
}

/// Make a folder with one tiny real JPEG; return the folder + the image path.
fn photo_folder(root: &Path, name: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let folder = root.join(name);
    std::fs::create_dir_all(&folder).unwrap();
    let img = folder.join("DSC0001.jpg");
    let buf: ImageBuffer<Rgb<u8>, Vec<u8>> = ImageBuffer::from_fn(48, 32, |_, _| Rgb([7, 8, 9]));
    buf.save(&img).unwrap();
    (folder, img)
}

#[test]
fn scan_then_stats_and_libraries() {
    let t = tempfile::TempDir::new().unwrap();
    let cfg = write_config(t.path());
    let appdata = t.path().join("app");
    let (folder, _img) = photo_folder(t.path(), "trip");

    let scan = run_pp(&appdata, &cfg, &["scan", "--no-models", folder.to_str().unwrap()]);
    assert!(scan.status.success(), "scan failed: {}", String::from_utf8_lossy(&scan.stderr));

    let stats = run_pp(&appdata, &cfg, &["stats", folder.to_str().unwrap()]);
    // NOTE: `stats <folder>` is wired in Task 4; until then this asserts only
    // that `scan` created a library (see libraries below). Re-enable the
    // stats success assertion after Task 4.
    let _ = stats;

    let libs = run_pp(&appdata, &cfg, &["libraries"]);
    assert!(libs.status.success());
    let out = String::from_utf8_lossy(&libs.stdout);
    assert!(out.contains("trip"), "libraries output missing folder: {out}");
}

#[test]
fn doctor_runs_without_a_catalog() {
    let t = tempfile::TempDir::new().unwrap();
    let cfg = write_config(t.path());
    let appdata = t.path().join("app");
    // Empty model dir → models check fails → doctor exits non-zero, but it must
    // not reference any catalog and must not panic.
    std::fs::create_dir_all(t.path().join("models")).unwrap();
    let out = run_pp(&appdata, &cfg, &["doctor"]);
    let combined = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert!(!combined.to_lowercase().contains("db schema"), "doctor should not check a fixed catalog: {combined}");
}
```

Add `image` to `crates/cli/Cargo.toml` `[dev-dependencies]` if not already present:

```toml
image = { workspace = true }
```

- [ ] **Step 2: Run to verify it fails**

Run: `. ~/.cargo/env && cargo test -p photopipe --test cli scan_then_stats_and_libraries`
Expected: FAIL — `libraries` subcommand and folder-based `scan` library creation don't exist yet (and the old tests referencing `db_path` are gone).

- [ ] **Step 3: Add `LibraryRoots` to `main` and the `Libraries` command**

In `crates/cli/src/main.rs`:

Add to the imports near the top:

```rust
use pipeline::library::LibraryRoots;
```

Add a `Libraries` variant to `enum Command` (after `Doctor`):

```rust
    /// List analyzed libraries (folder, last-analyzed, photo count).
    Libraries,
```

Change `serve` and add the folder to it:

```rust
    /// Launch the local review web server for a folder's library.
    Serve {
        /// Folder whose library to serve.
        folder: PathBuf,
        /// Port to bind on 127.0.0.1.
        #[arg(long, default_value_t = 8787)]
        port: u16,
    },
```

In `main()`, build the roots once and update the match arms for the commands this task migrates (leave the others calling their existing handlers):

```rust
    let roots = LibraryRoots::from_dirs()?;

    match cli.command {
        Command::Scan { paths, no_models, reprocess } => cmd_scan(paths, no_models, reprocess, &cfg, &roots),
        Command::Calibrate => cmd_calibrate(&cfg),
        Command::Dedupe => cmd_dedupe(&cfg),
        Command::ReviewTree { output, include, regenerate } => cmd_review_tree(output, include, regenerate, &cfg),
        Command::Info { file } => cmd_info(file, &cfg),
        Command::Stats => cmd_stats(&cfg),
        Command::Doctor => cmd_doctor(&config_path, &cfg, &roots),
        Command::Libraries => cmd_libraries(&roots),
        Command::Serve { folder, port } => serve::run(&cfg, &folder, port),
        Command::ExportKeepers { output, regenerate } => cmd_export_keepers(output, regenerate, &cfg),
    }
```

- [ ] **Step 4: Rewrite `cmd_scan` for per-folder libraries**

Replace `cmd_scan` with:

```rust
fn cmd_scan(
    paths: Vec<PathBuf>,
    no_models: bool,
    _reprocess: bool,
    cfg: &config::Config,
    roots: &LibraryRoots,
) -> Result<()> {
    use pipeline::{
        analyze_defects, analyze_ml, ingest::ingest_directory, library::open_or_create_library,
        models::ModelHub,
    };

    let hub = if no_models {
        tracing::info!("--no-models: skipping model loading");
        ModelHub::empty()
    } else {
        ModelHub::from_config(&cfg.models).map_err(|e| anyhow::anyhow!("models: {}", e))?
    };

    for folder in &paths {
        let folder = config::expand_tilde(folder);
        println!("== {} ==", folder.display());
        let lib = open_or_create_library(roots, &folder)?;

        let report = ingest_directory(std::slice::from_ref(&folder), &lib.catalog, &lib.cache, &cfg.ingest)?;
        println!("Scan complete:");
        println!("  Processed : {}", report.processed);
        println!("  Skipped   : {}", report.skipped);
        println!("  Errored   : {}", report.errored);

        let defect_report = analyze_defects(&lib.catalog, &lib.cache, &hub, &cfg.defect)?;
        println!("Defect analysis:");
        println!("  Analyzed             : {}", defect_report.analyzed);
        println!("  Errored              : {}", defect_report.errored);
        println!("  Flagged overexposed  : {}", defect_report.flagged_overexposed);
        println!("  Flagged underexposed : {}", defect_report.flagged_underexposed);

        let ml_report = analyze_ml(&lib.catalog, &lib.cache, &hub, cfg.catalog.write_batch_size)?;
        if !hub.is_empty() {
            println!("ML analysis:");
            println!("  Embedded   : {}", ml_report.embedded);
            println!("  IQA scored : {}", ml_report.iqa_scored);
            println!("  Errored    : {}", ml_report.errored);
        }

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        lib.catalog
            .set_last_analyzed(now)
            .map_err(|e| anyhow::anyhow!("library_meta: {e}"))?;
    }
    Ok(())
}
```

- [ ] **Step 5: Add `cmd_libraries`**

Add (near the other handlers):

```rust
fn cmd_libraries(roots: &LibraryRoots) -> Result<()> {
    let libs = pipeline::library::list_libraries(roots)?;
    if libs.is_empty() {
        println!("No analyzed libraries yet. Run `photopipe scan <folder>`.");
        return Ok(());
    }
    println!("Analyzed libraries:");
    for l in &libs {
        let last = match l.last_analyzed {
            Some(ts) => ts.to_string(),
            None => "never".to_string(),
        };
        println!("  {}  ({} photos, last analyzed {})", l.folder.display(), l.photo_count, last);
    }
    Ok(())
}
```

- [ ] **Step 6: Migrate `doctor` off the fixed catalog**

Replace the three `checks.push(...)` lines in `cmd_doctor` (and its signature) so it takes `roots` and drops the schema check:

Signature: `fn cmd_doctor(config_path: &std::path::Path, cfg: &config::Config, roots: &LibraryRoots) -> Result<()>`.

Replace the check block:

```rust
    checks.push(doctor_check_cache_writable(&roots.cache));
    checks.push(doctor_check_disk_free(&roots.data));
```

Delete the `doctor_check_schema` function entirely, and delete the now-unused `const EXPECTED_SCHEMA_VERSION` (its only user was `doctor_check_schema`). If the `use pipeline::catalog::Catalog;` import at the top of `main.rs` becomes unused after this, remove it.

- [ ] **Step 7: Migrate `serve` to open a folder's library**

In `crates/cli/src/serve/mod.rs`, change `run`:

```rust
pub fn run(cfg: &Config, folder: &std::path::Path, port: u16) -> anyhow::Result<()> {
    let roots = pipeline::library::LibraryRoots::from_dirs()?;
    let lib = pipeline::library::open_or_create_library(&roots, folder)?;
    let state = AppState {
        catalog: Arc::new(lib.catalog),
        cache: Arc::new(lib.cache),
        cfg: Arc::new(cfg.clone()),
    };

    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    rt.block_on(async move {
        let addr = SocketAddr::from(([127, 0, 0, 1], port));
        let listener = tokio::net::TcpListener::bind(addr).await?;
        tracing::info!(%addr, folder = %folder.display(), "review server listening — open http://{addr}/");
        axum::serve(listener, router(state)).await?;
        Ok::<(), anyhow::Error>(())
    })
}
```

Remove the now-unused `Catalog`/`Cache`/`Config` imports only if they become unused (Cache/Catalog are referenced via `pipeline::library`; `Config` is still used in the signature). Adjust imports to compile cleanly.

- [ ] **Step 8: Run tests + build**

Run:
```bash
. ~/.cargo/env
cargo test -p photopipe --test cli scan_then_stats_and_libraries
cargo test -p photopipe --test cli doctor_runs_without_a_catalog
cargo build -p photopipe
cargo clippy -p photopipe --all-targets -- -D warnings
```
Expected: the two new tests pass; the binary builds (the still-`db_path` commands compile because `CatalogConfig.db_path` still exists); clippy clean. (The old `serve.rs` handler tests are unaffected — `serve::run` changed but the handler/router tests construct `AppState` directly.)

- [ ] **Step 9: Commit**

```bash
git add crates/cli/src/main.rs crates/cli/src/serve/mod.rs crates/cli/tests/cli.rs crates/cli/Cargo.toml
git commit -m "feat(cli): scan/serve/doctor on per-folder libraries; add libraries command"
```

---

## Task 4: Migrate the per-folder read-only commands

**Files:**
- Modify: `crates/cli/src/main.rs`
- Test: `crates/cli/tests/cli.rs`

**Interfaces:**
- Consumes: `pipeline::library::{LibraryRoots, open_existing_library, find_library_for_file}`.
- Produces: `calibrate <folder>`, `dedupe <folder>`, `stats <folder>`, `review-tree <folder> <output>`, `export-keepers <folder> <output>` each resolve the folder's library; `info <file>` resolves via ancestor walk. All take `&roots`.

- [ ] **Step 1: Write the failing test**

Append to `crates/cli/tests/cli.rs`:

```rust
#[test]
fn read_only_commands_resolve_library() {
    let t = tempfile::TempDir::new().unwrap();
    let cfg = write_config(t.path());
    let appdata = t.path().join("app");
    let (folder, img) = photo_folder(t.path(), "trip");

    run_pp(&appdata, &cfg, &["scan", "--no-models", folder.to_str().unwrap()]);

    // stats <folder> succeeds and shows the one file.
    let stats = run_pp(&appdata, &cfg, &["stats", folder.to_str().unwrap()]);
    assert!(stats.status.success(), "stats failed: {}", String::from_utf8_lossy(&stats.stderr));
    assert!(String::from_utf8_lossy(&stats.stdout).contains("Total files"));

    // stats on an un-scanned folder errors non-zero.
    let other = t.path().join("unscanned");
    std::fs::create_dir_all(&other).unwrap();
    let bad = run_pp(&appdata, &cfg, &["stats", other.to_str().unwrap()]);
    assert!(!bad.status.success(), "stats on un-scanned folder should fail");
    assert!(String::from_utf8_lossy(&bad.stderr).contains("no library"), "expected 'no library' message");

    // info <file> resolves the library by walking up to the folder.
    let info = run_pp(&appdata, &cfg, &["info", img.to_str().unwrap()]);
    assert!(info.status.success(), "info failed: {}", String::from_utf8_lossy(&info.stderr));
    let v: serde_json::Value = serde_json::from_slice(&info.stdout).expect("info JSON");
    assert_eq!(v["file"]["path"], img.to_string_lossy().as_ref());
}
```

Add `serde_json` to `crates/cli/Cargo.toml` `[dev-dependencies]` if not already present:

```toml
serde_json = { workspace = true }
```

- [ ] **Step 2: Run to verify it fails**

Run: `. ~/.cargo/env && cargo test -p photopipe --test cli read_only_commands_resolve_library`
Expected: FAIL — `stats`/`info` don't take a folder/resolve a library yet.

- [ ] **Step 3: Update the `Command` variants + match arms**

In `crates/cli/src/main.rs`, change these variants to take a positional `folder`:

```rust
    /// Rebuild per-lens sharpness baselines and re-flag blur/back-focus/low-IQA.
    Calibrate { folder: PathBuf },

    /// Rebuild duplicate groups using current embeddings.
    Dedupe { folder: PathBuf },

    /// Generate or update the review tree (copies flagged photos for browsing).
    ReviewTree {
        folder: PathBuf,
        output: PathBuf,
        #[arg(long, value_delimiter = ',')]
        include: Vec<String>,
        #[arg(long)]
        regenerate: bool,
    },

    /// Print all catalog data for a single file as JSON.
    Info { file: PathBuf },

    /// Print catalog summary statistics for a folder's library.
    Stats { folder: PathBuf },

    /// Materialize a keepers export tree from recorded decisions.
    ExportKeepers {
        folder: PathBuf,
        output: PathBuf,
        #[arg(long)]
        regenerate: bool,
    },
```

Update the match arms in `main()`:

```rust
        Command::Calibrate { folder } => cmd_calibrate(&folder, &cfg, &roots),
        Command::Dedupe { folder } => cmd_dedupe(&folder, &cfg, &roots),
        Command::ReviewTree { folder, output, include, regenerate } => cmd_review_tree(&folder, output, include, regenerate, &cfg, &roots),
        Command::Info { file } => cmd_info(file, &cfg, &roots),
        Command::Stats { folder } => cmd_stats(&folder, &cfg, &roots),
        Command::ExportKeepers { folder, output, regenerate } => cmd_export_keepers(&folder, output, regenerate, &cfg, &roots),
```

- [ ] **Step 4: Add a resolver helper + rewrite the six handlers**

Add a small helper near the handlers:

```rust
/// Open the library for `folder`, or bail with a clear message.
fn require_library(
    roots: &LibraryRoots,
    folder: &std::path::Path,
) -> Result<pipeline::library::Library> {
    let folder = config::expand_tilde(folder);
    match pipeline::library::open_existing_library(roots, &folder)? {
        Some(lib) => Ok(lib),
        None => anyhow::bail!(
            "no library for {} — run 'photopipe scan {}' first",
            folder.display(),
            folder.display()
        ),
    }
}
```

Rewrite the six handlers to resolve the library (only the catalog-acquisition lines change; the bodies are otherwise as today):

```rust
fn cmd_calibrate(folder: &std::path::Path, cfg: &config::Config, roots: &LibraryRoots) -> Result<()> {
    let lib = require_library(roots, folder)?;
    let report = pipeline::run_calibration(&lib.catalog, &cfg.defect)?;
    println!("Calibration complete:");
    println!("  Buckets built          : {}", report.buckets_built);
    println!("  Global sample count    : {}", report.global_n_samples);
    println!("  Stale flags cleared    : {}", report.flags_cleared);
    println!("  Flagged blur           : {}", report.flagged_blur);
    println!("  Flagged back-focus     : {}", report.flagged_back_focus);
    println!("  Flagged low-IQA        : {}", report.flagged_low_iqa);
    println!("  Blur confidence bumped : {}", report.blur_confidence_bumped);
    Ok(())
}

fn cmd_dedupe(folder: &std::path::Path, cfg: &config::Config, roots: &LibraryRoots) -> Result<()> {
    let lib = require_library(roots, folder)?;
    if cfg.catalog.enable_vss {
        tracing::warn!(
            "catalog.enable_vss = true, but the DuckDB vss/HNSW backend is not \
             implemented yet — falling back to brute-force KNN"
        );
    }
    let report = pipeline::run_dedupe(&lib.catalog, &cfg.dedupe)?;
    println!("Dedupe complete:");
    println!("  Groups  : {}", report.groups);
    println!("  Members : {}", report.members);
    println!("  Keepers : {}", report.keepers);
    Ok(())
}

fn cmd_stats(folder: &std::path::Path, cfg: &config::Config, roots: &LibraryRoots) -> Result<()> {
    let _ = cfg;
    let lib = require_library(roots, folder)?;
    let catalog = &lib.catalog;
    let s = catalog.stats().map_err(|e| anyhow::anyhow!("stats: {}", e))?;
    let flags = catalog.flag_counts().map_err(|e| anyhow::anyhow!("flags: {}", e))?;
    let cameras = catalog.per_camera_counts().map_err(|e| anyhow::anyhow!("cameras: {}", e))?;
    let lenses = catalog.per_lens_counts().map_err(|e| anyhow::anyhow!("lenses: {}", e))?;
    // … keep the existing printing block verbatim, but drop the db/cache disk-usage
    // lines that referenced cfg.catalog.db_path / cache_dir (those paths no longer
    // belong to a single catalog). Print library folder + counts instead:
    println!("PhotoPipe Stats — {}", lib.folder.display());
    println!("===============");
    println!("Total files          : {}", s.total_files);
    println!("Duplicate groups     : {}", s.duplicate_group_count);
    println!("Files in groups      : {}", s.grouped_file_count);
    println!("Embeddings           : {}", s.embedding_count);
    println!("IQA scores           : {}", s.iqa_count);
    println!();
    println!("Defect flags");
    println!("------------");
    if flags.is_empty() {
        println!("  (none)");
    } else {
        for (k, n) in &flags {
            println!("  {k:<14} {n}");
        }
    }
    println!();
    println!("Per camera");
    println!("----------");
    if cameras.is_empty() {
        println!("  (no EXIF)");
    } else {
        for (cam, n) in &cameras {
            println!("  {cam:<28} {n}");
        }
    }
    println!();
    println!("Per lens");
    println!("--------");
    if lenses.is_empty() {
        println!("  (no EXIF)");
    } else {
        for (cam, lens, n) in &lenses {
            println!("  {cam} / {lens:<28} {n}");
        }
    }
    Ok(())
}

fn cmd_info(file: PathBuf, cfg: &config::Config, roots: &LibraryRoots) -> Result<()> {
    let _ = cfg;
    let file = config::expand_tilde(&file);
    let folder = pipeline::library::find_library_for_file(roots, &file)?
        .ok_or_else(|| anyhow::anyhow!("no analyzed library contains {} — run scan first", file.display()))?;
    let lib = pipeline::library::open_existing_library(roots, &folder)?
        .ok_or_else(|| anyhow::anyhow!("library for {} disappeared", folder.display()))?;
    match lib.catalog.dump_file(&file).map_err(|e| anyhow::anyhow!("info: {}", e))? {
        Some(dump) => {
            println!("{}", serde_json::to_string_pretty(&dump)?);
            Ok(())
        }
        None => anyhow::bail!("no catalog entry for {}", file.display()),
    }
}

fn cmd_review_tree(
    folder: &std::path::Path,
    output: PathBuf,
    include: Vec<String>,
    regenerate: bool,
    cfg: &config::Config,
    roots: &LibraryRoots,
) -> Result<()> {
    let _ = cfg;
    let lib = require_library(roots, folder)?;
    let output = config::expand_tilde(&output);
    let est = pipeline::estimate_review_copy(&lib.catalog, &output, &include)?;
    println!("Copying {} files ({}) → {} …", est.files, pipeline::humanize_bytes(est.bytes), output.display());
    let report = pipeline::build_review_tree(&lib.catalog, &output, &include, regenerate)?;
    println!("Review tree: {}", output.display());
    println!("  Copied  : {} files ({})", report.files_copied, pipeline::humanize_bytes(report.bytes_copied));
    println!("  Skipped : {}", report.files_skipped);
    println!("  Removed : {}", report.files_removed);
    println!("  Groups  : {}", report.groups);
    println!("  Errors  : {}", report.errors);
    Ok(())
}

fn cmd_export_keepers(
    folder: &std::path::Path,
    output: PathBuf,
    regenerate: bool,
    cfg: &config::Config,
    roots: &LibraryRoots,
) -> Result<()> {
    let _ = cfg;
    let lib = require_library(roots, folder)?;
    let out = config::expand_tilde(&output);
    let est = pipeline::estimate_keepers_copy(&lib.catalog, &out)?;
    println!("Copying {} files ({}) → {} …", est.files, pipeline::humanize_bytes(est.bytes), out.display());
    let report = pipeline::build_keepers_tree(&lib.catalog, &out, regenerate)?;
    println!(
        "Copied {} files ({}), {} skipped, {} removed, {} errors → {}",
        report.files_copied, pipeline::humanize_bytes(report.bytes_copied),
        report.files_skipped, report.files_removed, report.errors, out.display()
    );
    Ok(())
}
```

(If the existing `cmd_stats` prints differently, replace its body wholesale with the block above; the goal is library-folder-scoped stats with no `cfg.catalog.db_path`/`cache_dir` reference. Remove the now-unused `file_size`/`dir_size` helpers if nothing else uses them, or leave them if they do.)

- [ ] **Step 5: Re-enable the stats assertion in the Task-3 test**

In `scan_then_stats_and_libraries`, replace the `let _ = stats;` placeholder with a real assertion:

```rust
    assert!(stats.status.success(), "stats failed: {}", String::from_utf8_lossy(&stats.stderr));
    assert!(String::from_utf8_lossy(&stats.stdout).contains("Total files"));
```

- [ ] **Step 6: Run tests + build**

Run:
```bash
. ~/.cargo/env
cargo test -p photopipe --test cli
cargo build --workspace
cargo clippy --all-targets --all-features -- -D warnings
```
Expected: all CLI tests pass; workspace builds; clippy clean.

- [ ] **Step 7: Commit**

```bash
git add crates/cli/src/main.rs crates/cli/tests/cli.rs crates/cli/Cargo.toml
git commit -m "feat(cli): per-folder library resolution for calibrate/dedupe/stats/info/review-tree/export-keepers"
```

---

## Task 5: Retire `db_path`/`cache_dir`; docs; final sweep

**Files:**
- Modify: `crates/pipeline/src/config.rs`
- Modify: `photopipe.example.toml`
- Modify: `README.md`
- Test: `crates/pipeline/src/config.rs` (inline)

**Interfaces:**
- Consumes: nothing new.
- Produces: `CatalogConfig` without `db_path`/`cache_dir`; old keys silently ignored.

- [ ] **Step 1: Write the failing test**

Add to the `#[cfg(test)] mod tests` in `crates/pipeline/src/config.rs`:

```rust
    #[test]
    fn legacy_catalog_paths_are_ignored() {
        // Old configs carried db_path/cache_dir — they must parse without error.
        let toml_str = r#"
            [catalog]
            db_path = "/old/catalog.duckdb"
            cache_dir = "/old/cache"
            write_batch_size = 32
        "#;
        let cfg: Config = toml::from_str(toml_str).expect("legacy keys should be ignored");
        assert_eq!(cfg.catalog.write_batch_size, 32);
    }
```

- [ ] **Step 2: Run it (it should compile-fail or pass-but-guard)**

Run: `. ~/.cargo/env && cargo test -p pipeline --lib config::tests::legacy_catalog_paths_are_ignored`
Expected: passes today (fields still exist); it is the guard that removal in Step 3 preserves no-breakage. Run it now to confirm green before the change.

- [ ] **Step 3: Remove the fields**

In `crates/pipeline/src/config.rs`, drop `db_path` and `cache_dir` from `CatalogConfig` and its `Default`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CatalogConfig {
    pub write_batch_size: usize,
    pub enable_vss: bool,
}

impl Default for CatalogConfig {
    fn default() -> Self {
        Self { write_batch_size: 64, enable_vss: false }
    }
}
```

If `data_root`/`cache_root` helper fns in `config.rs` are now unused (they were only used by the removed defaults), delete them. (`expand_tilde` stays.)

- [ ] **Step 4: Update the example config**

In `photopipe.example.toml`, remove the `db_path` and `cache_dir` lines from `[catalog]` and add a short comment:

```toml
[catalog]
# Catalogs are per-folder libraries stored in OS app-data (data dir for the
# catalog, cache dir for previews) — keyed by the analyzed folder's path.
# There is no configurable catalog path.
write_batch_size = 64
enable_vss       = false
```

- [ ] **Step 5: Update the README**

In `README.md`:
- In the command examples and reference, update the CLI invocations to the new shapes: `photopipe calibrate <folder>`, `dedupe <folder>`, `stats <folder>`, `export-keepers <folder> <output>`, `review-tree <folder> <output>`, `serve <folder>`, and the new `photopipe libraries`.
- Add a short "Libraries" note: each analyzed folder is its own library stored in OS app-data (catalog in the data dir, previews in the cache dir), keyed by the folder path; `photopipe libraries` lists them; nothing is written into the photo folder.
- Remove any mention of a configurable `db_path`/`cache_dir`.
- Note that pre-existing single catalogs from older builds (e.g. `…/photopipe/catalog.duckdb`) are no longer used and can be deleted.

- [ ] **Step 6: Final verification sweep**

Run:
```bash
. ~/.cargo/env
cargo fmt
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all
```
Expected: fmt clean; clippy 0 warnings; all tests pass.

- [ ] **Step 7: Commit**

```bash
git add crates/pipeline/src/config.rs photopipe.example.toml README.md
git commit -m "refactor(config): retire db_path/cache_dir (per-folder libraries); docs"
```

---

## Self-Review (completed during planning)

- **Spec coverage:** resolver module with paths/hashing/open/list/find (Task 2) ✓; `library_meta` schema v3 + methods (Task 1) ✓; per-command CLI migration incl. positional `<folder>` and `info` ancestor-walk (Tasks 3–4) ✓; new `libraries` command (Task 3) ✓; `doctor` drops fixed-catalog check (Task 3) ✓; `serve <folder>` (Task 3) ✓; retire `db_path`/`cache_dir` with no-breakage (Task 5) ✓; DuckDB-only/self-describing/non-destructive/no-new-deps (Global Constraints) ✓; sandboxed CLI tests via temp `XDG_*` (Tasks 3–4) ✓.
- **Type consistency:** `LibraryRoots{data,cache}`, `Library{folder,catalog,cache}`, `LibraryInfo{folder,key,created_at,last_analyzed,photo_count}`, `library_key`, `open_or_create_library`/`open_existing_library`/`list_libraries`/`find_library_for_file`, and `Catalog::{set_library_meta,set_last_analyzed,library_meta}` are used consistently across Tasks 1–4 and the re-export list. Handlers uniformly take `&LibraryRoots`.
- **Placeholder scan:** none; every code step carries full code or a precise edit. The one intentional cross-task seam (Task 3's `let _ = stats;` placeholder) is explicitly replaced in Task 4 Step 5.
- **Sequencing:** config keeps `db_path`/`cache_dir` through Tasks 1–4 (un-migrated handlers still compile); Task 5 removes them once nothing references them. `EXPECTED_SCHEMA_VERSION` is bumped in Task 1 (keeps `doctor` green in the interim) and removed in Task 3 with the schema check. Each task ends with a green build + tests.
- **Known follow-up (next spec):** the browser folder-picker, background analyze job + progress, active-library switching, and the SPA are spec 2 and intentionally absent here.
