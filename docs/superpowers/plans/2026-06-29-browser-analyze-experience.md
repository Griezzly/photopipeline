# Browser Analyze Experience Implementation Plan (Spec 2 of 2)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `photopipe serve` an out-of-the-box web app: Home (recent libraries) → Browse (server-side folder picker) → Analyze (one background job running scan→calibrate→dedupe with live progress) → Review (the existing grid).

**Architecture:** A new `pipeline::analyze_folder` orchestrates the full chain and reports progress via a `ProgressSink` callback. The `serve` AppState gains an **active-library** slot (all review endpoints scope to it) and a **job** slot; new endpoints add a folder browser, a background analyze job (polled), and library list/open. A zero-build vanilla SPA ties the four screens together.

**Tech Stack:** Rust (edition 2021), DuckDB, axum/tokio, rust-embed; zero-build vanilla JS. No new dependencies.

## Global Constraints

Apply to **every** task (from the spec + `CLAUDE.md`):

- **DuckDB only. No SQLite. No Python at runtime. No new dependencies.**
- **One DuckDB connection per file per process.** The analyze job and `open` MUST reuse the active library's catalog/cache when the target folder is already active; opening a second connection to the same file fails on DuckDB's file lock.
- **Non-destructive:** libraries live in app-data; originals only read. Server binds **`127.0.0.1`** only.
- **Single analyze job at a time:** a concurrent `POST /api/analyze` returns `409`. No cancel in v1.
- **Models missing → analyze anyway, ML skipped**, `ml_ran=false` surfaced; per-file failures `warn!`+count+continue, never abort the job; a fatal job error sets `stage=failed`+`error`, never poisons the server.
- **Review endpoints scope to the active library**, returning `409` ("no library open") when none is active.
- `anyhow::Result`/`StatusCode` at HTTP boundaries; `thiserror` inside `pipeline`; `tracing` not `println!` except user-facing CLI output.
- The CLI commands (`scan`/`calibrate`/`dedupe`/etc.) are **unchanged**.
- **Each task ends with `cargo fmt` run + committed** (avoids the fmt-drift that recurred in prior phases), and a green build + tests.
- Before done: `cargo fmt`, `cargo clippy --all-targets --all-features -- -D warnings`, `cargo test --all` green.
- One git commit per task, conventional-commit style.

---

## File Structure

- `crates/pipeline/src/analyze.rs` — **new**: `ProgressSink`, `AnalyzeReport`, `analyze_folder`, `count_pending`. (Task 1)
- `crates/pipeline/src/ingest/mod.rs` — `ingest_directory` gains an optional progress hook. (Task 1)
- `crates/pipeline/src/lib.rs` — re-exports. (Task 1)
- `crates/cli/src/serve/mod.rs` — `AppState` (active + job slots, accessors), `serve::run(folder: Option)`, routes. (Tasks 2–4)
- `crates/cli/src/serve/handlers.rs` — review handlers scope to active; new fs/libraries/open/active/analyze handlers. (Tasks 2–4)
- `crates/cli/src/main.rs` — `Command::Serve { folder: Option<PathBuf>, port }`. (Task 2)
- `crates/cli/assets/` — `index.html`, `app.js` (router), `home.js`, `browse.js`, `analyze.js`, `review.js`, `style.css`. (Tasks 5–6)
- `crates/cli/tests/serve.rs` — updated + new endpoint tests. (Tasks 2–4)
- `crates/pipeline/tests/analyze.rs` — `analyze_folder`/`count_pending` tests. (Task 1)
- `README.md` — browser workflow. (Task 7)

---

## Task 1: `pipeline::analyze` — orchestration + progress + pending count

**Files:**
- Create: `crates/pipeline/src/analyze.rs`
- Modify: `crates/pipeline/src/ingest/mod.rs` (`ingest_directory` signature)
- Modify: `crates/cli/src/main.rs` (`cmd_scan` call passes `None`)
- Modify: `crates/pipeline/src/lib.rs` (re-exports)
- Test: `crates/pipeline/tests/analyze.rs` (create)

**Interfaces:**
- Consumes: `ingest_directory`, `analyze_defects`, `analyze_ml`, `run_calibration`, `run_dedupe`, `models::ModelHub` (`is_empty`), `catalog::Catalog` (`needs_processing`, `set_last_analyzed`), `config::Config`.
- Produces (re-exported from `lib.rs`):
  - `pub trait ProgressSink: Send + Sync { fn stage(&self, stage: &str); fn set_total(&self, total: u64); fn inc(&self); }`
  - `pub struct AnalyzeReport { pub ml_ran: bool, pub processed: u64, pub skipped: u64, pub errored: u64, pub groups: u64 }`
  - `pub fn analyze_folder(folder: &Path, catalog: &Catalog, cache: &Cache, hub: &ModelHub, cfg: &Config, progress: &dyn ProgressSink) -> anyhow::Result<AnalyzeReport>`
  - `pub fn count_pending(folder: &Path, catalog: &Catalog, cfg: &IngestConfig) -> anyhow::Result<u64>`
  - `ingest_directory(roots, catalog, cache, cfg, progress: Option<&dyn ProgressSink>)`

- [ ] **Step 1: Write the failing test**

Create `crates/pipeline/tests/analyze.rs`:

```rust
use std::sync::{Arc, Mutex};

use image::{ImageBuffer, Rgb};
use pipeline::analyze::{analyze_folder, count_pending, ProgressSink};
use pipeline::config::Config;
use pipeline::library::{open_or_create_library, LibraryRoots};
use pipeline::models::ModelHub;
use tempfile::TempDir;

#[derive(Default)]
struct RecordingSink {
    stages: Mutex<Vec<String>>,
    total: Mutex<u64>,
    ticks: Mutex<u64>,
}
impl ProgressSink for RecordingSink {
    fn stage(&self, s: &str) { self.stages.lock().unwrap().push(s.to_string()); }
    fn set_total(&self, t: u64) { *self.total.lock().unwrap() = t; }
    fn inc(&self) { *self.ticks.lock().unwrap() += 1; }
}

fn make_jpg(dir: &std::path::Path, name: &str) {
    let img: ImageBuffer<Rgb<u8>, Vec<u8>> = ImageBuffer::from_fn(48, 32, |x, _| Rgb([(x % 255) as u8, 1, 2]));
    img.save(dir.join(name)).unwrap();
}

#[test]
fn analyze_folder_runs_chain_ml_skipped_and_is_idempotent() {
    let d = TempDir::new().unwrap();
    let roots = LibraryRoots { data: d.path().join("data"), cache: d.path().join("cache") };
    let folder = d.path().join("photos");
    std::fs::create_dir_all(&folder).unwrap();
    make_jpg(&folder, "a.jpg");
    make_jpg(&folder, "b.jpg");

    let lib = open_or_create_library(&roots, &folder).unwrap();
    let cfg = Config::default();
    let hub = ModelHub::empty();
    let sink = Arc::new(RecordingSink::default());

    // count_pending sees both files before scanning.
    assert_eq!(count_pending(&folder, &lib.catalog, &cfg.ingest).unwrap(), 2);

    let report = analyze_folder(&folder, &lib.catalog, &lib.cache, &hub, &cfg, sink.as_ref()).unwrap();
    assert!(!report.ml_ran);
    assert_eq!(report.processed, 2);

    let stages = sink.stages.lock().unwrap().clone();
    assert!(stages.contains(&"scanning".to_string()));
    assert!(stages.contains(&"calibrating".to_string()));
    assert!(stages.contains(&"deduping".to_string()));
    assert_eq!(*sink.total.lock().unwrap(), 2);
    assert_eq!(*sink.ticks.lock().unwrap(), 2);

    // last_analyzed stamped.
    assert!(lib.catalog.library_meta().unwrap().unwrap().2.is_some());

    // idempotent: nothing pending, re-run processes 0.
    assert_eq!(count_pending(&folder, &lib.catalog, &cfg.ingest).unwrap(), 0);
    let sink2 = Arc::new(RecordingSink::default());
    let r2 = analyze_folder(&folder, &lib.catalog, &lib.cache, &hub, &cfg, sink2.as_ref()).unwrap();
    assert_eq!(r2.processed, 0);
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `. ~/.cargo/env && cargo test -p pipeline --test analyze`
Expected: FAIL — `pipeline::analyze` doesn't exist.

- [ ] **Step 3: Add the progress hook to `ingest_directory`**

In `crates/pipeline/src/ingest/mod.rs`, change the signature and add the ticks. Change the function signature to:

```rust
pub fn ingest_directory(
    roots: &[PathBuf],
    catalog: &Catalog,
    cache: &Cache,
    cfg: &IngestConfig,
    progress: Option<&dyn crate::analyze::ProgressSink>,
) -> anyhow::Result<IngestReport> {
```

After the line `let paths = exclude_sidecar_jpgs(paths);`, add:

```rust
    if let Some(p) = progress {
        p.set_total(paths.len() as u64);
    }
```

Inside the `paths.par_iter().for_each(|path| { ... })` closure, as the **first** line of the closure body, add a tick (it fires once per file regardless of outcome):

```rust
        if let Some(p) = progress {
            p.inc();
        }
```

(`progress` is `Option<&dyn ProgressSink>` which is `Copy`/`Send+Sync`, so it can be used inside the rayon closure.)

- [ ] **Step 4: Update the existing `ingest_directory` caller**

In `crates/cli/src/main.rs`, `cmd_scan`'s `ingest_directory(...)` call: add a trailing `None` argument:

```rust
        let report = ingest_directory(std::slice::from_ref(&folder), &lib.catalog, &lib.cache, &cfg.ingest, None)?;
```

(There is exactly one `ingest_directory` call site in the CLI — in `cmd_scan`. If a test or example also calls it, add `None` there too.)

- [ ] **Step 5: Create the analyze module**

Create `crates/pipeline/src/analyze.rs`:

```rust
//! Full-pipeline orchestration for the browser analyze flow: ingest → defects
//! → ML → calibrate → dedupe, with progress callbacks. The CLI keeps using the
//! individual phase functions; this is the one-call entry point for `serve`.

use std::path::Path;

use anyhow::Result;
use walkdir::WalkDir;

use crate::cache::Cache;
use crate::catalog::Catalog;
use crate::config::{Config, IngestConfig};
use crate::models::ModelHub;

/// Sink the orchestrator reports progress to. Implemented by the server's job
/// state. `Send + Sync` because `inc()` is called from rayon worker threads.
pub trait ProgressSink: Send + Sync {
    /// A coarse stage transition: "scanning" | "calibrating" | "deduping" | "done".
    fn stage(&self, stage: &str);
    /// Total files to ingest (set once, early in the scan stage).
    fn set_total(&self, total: u64);
    /// One file processed (called per ingested file).
    fn inc(&self);
}

/// Summary of a full analyze run.
#[derive(Debug, Clone)]
pub struct AnalyzeReport {
    pub ml_ran: bool,
    pub processed: u64,
    pub skipped: u64,
    pub errored: u64,
    pub groups: u64,
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

/// Run the full pipeline against `folder`'s library. Reports stage transitions
/// and per-file ingest progress through `progress`. Stamps `last_analyzed`.
pub fn analyze_folder(
    folder: &Path,
    catalog: &Catalog,
    cache: &Cache,
    hub: &ModelHub,
    cfg: &Config,
    progress: &dyn ProgressSink,
) -> Result<AnalyzeReport> {
    progress.stage("scanning");
    let ingest = crate::ingest::ingest_directory(
        std::slice::from_ref(&folder.to_path_buf()),
        catalog,
        cache,
        &cfg.ingest,
        Some(progress),
    )?;

    let _defects = crate::defect::analyze_defects(catalog, cache, hub, &cfg.defect)?;
    let _ml = crate::ml::analyze_ml(catalog, cache, hub, cfg.catalog.write_batch_size)?;

    progress.stage("calibrating");
    let _cal = crate::calibration::run_calibration(catalog, &cfg.defect)?;

    progress.stage("deduping");
    let dedupe = crate::dedupe::run_dedupe(catalog, &cfg.dedupe)?;

    catalog
        .set_last_analyzed(now_secs())
        .map_err(|e| anyhow::anyhow!("library_meta: {e}"))?;

    progress.stage("done");
    Ok(AnalyzeReport {
        ml_ran: !hub.is_empty(),
        processed: ingest.processed,
        skipped: ingest.skipped,
        errored: ingest.errored,
        groups: dedupe.groups,
    })
}

/// Count files under `folder` (by ingest extension) that the catalog reports as
/// new or changed — i.e. how much a re-analyze would process. Walk only; no decode.
pub fn count_pending(folder: &Path, catalog: &Catalog, cfg: &IngestConfig) -> Result<u64> {
    let mut pending = 0u64;
    for entry in WalkDir::new(folder)
        .follow_links(cfg.follow_symlinks)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        if !cfg.extensions.iter().any(|x| x.eq_ignore_ascii_case(ext)) {
            continue;
        }
        let Ok(meta) = std::fs::metadata(path) else { continue };
        let mtime_ns = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_nanos() as i64)
            .unwrap_or(0);
        if catalog
            .needs_processing(path, mtime_ns, meta.len())
            .unwrap_or(true)
        {
            pending += 1;
        }
    }
    Ok(pending)
}
```

Confirm the exact signatures of `analyze_defects`, `analyze_ml`, `run_calibration`, `run_dedupe`, `IngestReport`'s fields (`processed`/`skipped`/`errored`), `DedupeReport.groups`, and `Catalog::needs_processing(path: &Path, mtime_ns: i64, size: u64)` against the source; adapt the calls if any differ.

- [ ] **Step 6: Register + re-export**

In `crates/pipeline/src/lib.rs`, add `pub mod analyze;` to the module list and add a re-export:

```rust
pub use analyze::{analyze_folder, count_pending, AnalyzeReport, ProgressSink};
```

- [ ] **Step 7: Run tests + fmt + commit**

Run:
```bash
. ~/.cargo/env
cargo test -p pipeline --test analyze
cargo test -p pipeline
cargo fmt
cargo clippy -p pipeline --all-targets -- -D warnings
```
Expected: PASS, clippy clean.

```bash
git add crates/pipeline/src/analyze.rs crates/pipeline/src/ingest/mod.rs crates/pipeline/src/lib.rs crates/cli/src/main.rs crates/pipeline/tests/analyze.rs
git commit -m "feat(pipeline): analyze_folder orchestration + progress hook + count_pending"
```

---

## Task 2: Active-library AppState refactor + `serve` folder-optional

**Files:**
- Modify: `crates/cli/src/serve/mod.rs`
- Modify: `crates/cli/src/serve/handlers.rs`
- Modify: `crates/cli/src/main.rs`
- Test: `crates/cli/tests/serve.rs`

**Interfaces:**
- Consumes: `pipeline::library::{LibraryRoots, library_key, open_existing_library, open_or_create_library}`, `Catalog`, `Cache`, `Config`.
- Produces:
  - `AppState { cfg: Arc<Config>, roots: Arc<LibraryRoots>, active: Arc<Mutex<Option<ActiveLibrary>>>, job: Arc<Mutex<JobState>> }` (derives `Clone`).
  - `ActiveLibrary { folder: PathBuf, catalog: Arc<Catalog>, cache: Arc<Cache> }` (derives `Clone`).
  - `JobState` (struct + `Default` = idle) — fields used by Task 3; defined here so `AppState` is stable.
  - `AppState::active(&self) -> Result<ActiveLibrary, StatusCode>` (409 when none).
  - `AppState::set_active(&self, lib: ActiveLibrary)`.
  - `AppState::resolve_library(&self, folder: &Path, create: bool) -> anyhow::Result<ActiveLibrary>` — reuses the active library if `folder` matches (DuckDB single-connection rule), else opens (`open_or_create` when `create`, else `open_existing`, erroring if absent).
  - `serve::run(cfg: &Config, folder: Option<PathBuf>, port: u16)`.

- [ ] **Step 1: Write the failing test**

In `crates/cli/tests/serve.rs`, the helpers currently build `AppState { catalog, cache, cfg }` directly. Replace the `AppState`-construction helper with one that builds the new shape with an active library, and add a "no active → 409" test. Add near the top:

```rust
use std::sync::Mutex;

/// Build an AppState with `catalog`/`cache` as the active library.
fn app_state_active(
    catalog: pipeline::catalog::Catalog,
    cache: pipeline::cache::Cache,
) -> photopipe::serve::AppState {
    photopipe::serve::AppState {
        cfg: std::sync::Arc::new(pipeline::config::Config::default()),
        roots: std::sync::Arc::new(pipeline::library::LibraryRoots {
            data: std::path::PathBuf::from("/unused"),
            cache: std::path::PathBuf::from("/unused"),
        }),
        active: std::sync::Arc::new(Mutex::new(Some(photopipe::serve::ActiveLibrary {
            folder: std::path::PathBuf::from("/lib"),
            catalog: std::sync::Arc::new(catalog),
            cache: std::sync::Arc::new(cache),
        }))),
        job: std::sync::Arc::new(Mutex::new(photopipe::serve::JobState::default())),
    }
}
```

Update every existing test's `AppState { ... }` literal and `state_with_one_file()` to use `app_state_active(catalog, cache)` (they pass the catalog + cache they build). Then add:

```rust
#[tokio::test]
async fn review_endpoints_409_when_no_library_open() {
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;
    let dir = tempfile::TempDir::new().unwrap();
    let state = photopipe::serve::AppState {
        cfg: std::sync::Arc::new(pipeline::config::Config::default()),
        roots: std::sync::Arc::new(pipeline::library::LibraryRoots {
            data: dir.path().join("d"),
            cache: dir.path().join("c"),
        }),
        active: std::sync::Arc::new(Mutex::new(None)),
        job: std::sync::Arc::new(Mutex::new(photopipe::serve::JobState::default())),
    };
    let resp = photopipe::serve::router(state)
        .oneshot(Request::builder().uri("/api/photos").body(axum::body::Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `. ~/.cargo/env && cargo test -p photopipe --test serve`
Expected: FAIL to compile — `AppState` shape changed / `ActiveLibrary`/`JobState` don't exist.

- [ ] **Step 3: Rewrite `AppState` + accessors in `serve/mod.rs`**

Replace the `AppState` struct + add the new types/methods:

```rust
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use axum::http::StatusCode;
use pipeline::cache::Cache;
use pipeline::catalog::Catalog;
use pipeline::config::Config;
use pipeline::library::{library_key, open_existing_library, open_or_create_library, LibraryRoots};

/// The currently-open library the review endpoints operate on.
#[derive(Clone)]
pub struct ActiveLibrary {
    pub folder: PathBuf,
    pub catalog: Arc<Catalog>,
    pub cache: Arc<Cache>,
}

/// Live state of the (single) background analyze job. `idle` until one runs.
#[derive(Clone, serde::Serialize)]
pub struct JobState {
    pub stage: String, // idle | scanning | calibrating | deduping | done | failed
    pub files_done: u64,
    pub files_total: u64,
    pub ml_ran: bool,
    pub folder: String,
    pub message: String,
    pub error: Option<String>,
}

impl Default for JobState {
    fn default() -> Self {
        Self {
            stage: "idle".into(),
            files_done: 0,
            files_total: 0,
            ml_ran: false,
            folder: String::new(),
            message: String::new(),
            error: None,
        }
    }
}

impl JobState {
    /// True while a run is in flight.
    pub fn running(&self) -> bool {
        matches!(self.stage.as_str(), "scanning" | "calibrating" | "deduping")
    }
}

/// Shared, cheaply-cloneable server state.
#[derive(Clone)]
pub struct AppState {
    pub cfg: Arc<Config>,
    pub roots: Arc<LibraryRoots>,
    pub active: Arc<Mutex<Option<ActiveLibrary>>>,
    pub job: Arc<Mutex<JobState>>,
}

impl AppState {
    /// The active library, or `409` when none is open.
    pub fn active(&self) -> Result<ActiveLibrary, StatusCode> {
        self.active
            .lock()
            .unwrap()
            .clone()
            .ok_or(StatusCode::CONFLICT)
    }

    pub fn set_active(&self, lib: ActiveLibrary) {
        *self.active.lock().unwrap() = Some(lib);
    }

    /// Resolve a library for `folder`, REUSING the active library if `folder`
    /// is already the active one (DuckDB allows one connection per file). Does
    /// not change the active slot. `create` chooses open_or_create vs open_existing.
    pub fn resolve_library(&self, folder: &Path, create: bool) -> anyhow::Result<ActiveLibrary> {
        if let Some(active) = self.active.lock().unwrap().clone() {
            if library_key(&active.folder) == library_key(folder) {
                return Ok(active);
            }
        }
        if create {
            let lib = open_or_create_library(&self.roots, folder)?;
            Ok(ActiveLibrary {
                folder: lib.folder,
                catalog: Arc::new(lib.catalog),
                cache: Arc::new(lib.cache),
            })
        } else {
            match open_existing_library(&self.roots, folder)? {
                Some(lib) => Ok(ActiveLibrary {
                    folder: lib.folder,
                    catalog: Arc::new(lib.catalog),
                    cache: Arc::new(lib.cache),
                }),
                None => anyhow::bail!("no library for {}", folder.display()),
            }
        }
    }
}
```

(Keep the `mod handlers;` declaration and remove the old `Catalog`/`Cache` direct imports if now unused — they're used via `pipeline::` paths and `ActiveLibrary`.)

- [ ] **Step 4: Rewrite `serve::run` (folder optional)**

```rust
/// Boot the web app on `127.0.0.1:port`. With `Some(folder)`, open that
/// library and make it active (Review opens directly); with `None`, start on
/// the Home screen (no active library).
pub fn run(cfg: &Config, folder: Option<PathBuf>, port: u16) -> anyhow::Result<()> {
    let roots = LibraryRoots::from_dirs()?;
    let state = AppState {
        cfg: Arc::new(cfg.clone()),
        roots: Arc::new(roots),
        active: Arc::new(Mutex::new(None)),
        job: Arc::new(Mutex::new(JobState::default())),
    };
    if let Some(folder) = folder {
        let folder = pipeline::config::expand_tilde(&folder);
        let lib = state.resolve_library(&folder, true)?;
        state.set_active(lib);
    }

    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    rt.block_on(async move {
        let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
        let listener = tokio::net::TcpListener::bind(addr).await?;
        tracing::info!(%addr, "review server listening — open http://{addr}/");
        axum::serve(listener, router(state)).await?;
        Ok::<(), anyhow::Error>(())
    })
}
```

(Adjust the `use axum::routing::{get, post};` / `Router` imports as needed; `router` is unchanged structurally in this task.)

- [ ] **Step 5: Point every review handler at the active library**

In `crates/cli/src/serve/handlers.rs`, replace `state.catalog`/`state.cache` usage. The pattern:
- Handlers returning `Result<_, StatusCode>`: add `let lib = state.active()?;` first, then use `lib.catalog`/`lib.cache`.
- Handlers returning `Response`: add `let lib = match state.active() { Ok(l) => l, Err(s) => return s.into_response() };` first.

Apply to each:

`list_photos` (Result): first line `let lib = state.active()?;`, change `state.catalog.review_list(...)` → `lib.catalog.review_list(...)`.

`photo_detail` (Response): first line `let lib = match state.active() { Ok(l) => l, Err(s) => return s.into_response() };`, change `state.catalog.dump_file_by_id(id)` → `lib.catalog.dump_file_by_id(id)`.

`list_groups` (Result): `let lib = state.active()?;`, `lib.catalog.duplicate_groups_for_review()`.

`thumb`/`preview`: resolve active then pass the library into `render_asset`. Change their bodies to:
```rust
pub async fn thumb(State(state): State<AppState>, Path(id): Path<i64>) -> Response {
    let lib = match state.active() { Ok(l) => l, Err(s) => return s.into_response() };
    render_asset(lib, id, true).await
}
pub async fn preview(State(state): State<AppState>, Path(id): Path<i64>) -> Response {
    let lib = match state.active() { Ok(l) => l, Err(s) => return s.into_response() };
    render_asset(lib, id, false).await
}
```
And change `render_asset(state: AppState, ...)` → `render_asset(lib: ActiveLibrary, ...)`, and inside it/`preview_bytes` replace `state.catalog`/`state.cache` with `lib.catalog`/`lib.cache` (and change `preview_bytes(state: &AppState, ...)` → `preview_bytes(lib: &ActiveLibrary, ...)`). Import `ActiveLibrary` via `use super::{ActiveLibrary, AppState};`.

`post_decision` (Result): `let lib = state.active()?;`, replace the four `state.catalog.<method>` with `lib.catalog.<method>`, and `state.catalog.decision_counts()` → `lib.catalog.decision_counts()`.

`get_counts` (Result): `let lib = state.active()?;`, `lib.catalog.decision_counts()`.

`post_export` (Result): `let lib = state.active()?;`, change `let catalog = state.catalog.clone();` → `let catalog = lib.catalog.clone();`.

`get_export_estimate` (Result): `let lib = state.active()?;`, change `let catalog = state.catalog.clone();` → `let catalog = lib.catalog.clone();`.

(`health`, `index`, `static_asset` don't touch the catalog — unchanged.)

- [ ] **Step 6: Make `Command::Serve` folder optional**

In `crates/cli/src/main.rs`, change the `Serve` variant + match arm:

```rust
    /// Launch the local review web app. With a folder, opens its library
    /// directly; without one, starts on the Home screen.
    Serve {
        /// Optional folder whose library to open on startup.
        folder: Option<PathBuf>,
        /// Port to bind on 127.0.0.1.
        #[arg(long, default_value_t = 8787)]
        port: u16,
    },
```
```rust
        Command::Serve { folder, port } => serve::run(&cfg, folder, port),
```

- [ ] **Step 7: Run tests + fmt + commit**

Run:
```bash
. ~/.cargo/env
cargo test -p photopipe --test serve
cargo test --all
cargo fmt
cargo clippy --all-targets --all-features -- -D warnings
```
Expected: all serve tests (updated helper + the new 409 test) and the full suite pass; clippy clean.

```bash
git add crates/cli/src/serve/mod.rs crates/cli/src/serve/handlers.rs crates/cli/src/main.rs crates/cli/tests/serve.rs
git commit -m "refactor(serve): active-library AppState; review endpoints scope to it; serve folder optional"
```

---

## Task 3: Background analyze job + endpoints

**Files:**
- Modify: `crates/cli/src/serve/handlers.rs`
- Modify: `crates/cli/src/serve/mod.rs` (routes)
- Test: `crates/cli/tests/serve.rs`

**Interfaces:**
- Consumes: `AppState` (`resolve_library`, `set_active`, `job`, `cfg`, `roots`), `pipeline::{analyze_folder, ProgressSink}`, `pipeline::models::ModelHub`.
- Produces: `POST /api/analyze` (start; 409 if running) and `GET /api/analyze/status` (`Json<JobState>`).

- [ ] **Step 1: Write the failing test**

Append to `crates/cli/tests/serve.rs`:

```rust
#[tokio::test]
async fn analyze_job_runs_to_done_ml_skipped() {
    use axum::http::{Request, StatusCode};
    use image::{ImageBuffer, Rgb};
    use std::sync::Mutex;
    use tower::ServiceExt;

    let dir = tempfile::TempDir::new().unwrap();
    let folder = dir.path().join("photos");
    std::fs::create_dir_all(&folder).unwrap();
    let img: ImageBuffer<Rgb<u8>, Vec<u8>> = ImageBuffer::from_fn(40, 30, |_, _| Rgb([4, 5, 6]));
    img.save(folder.join("a.jpg")).unwrap();

    // App-state with a models-less config (model_dir empty → ModelHub::empty()).
    let mut cfg = pipeline::config::Config::default();
    cfg.models.model_dir = dir.path().join("no-models");
    let state = photopipe::serve::AppState {
        cfg: std::sync::Arc::new(cfg),
        roots: std::sync::Arc::new(pipeline::library::LibraryRoots {
            data: dir.path().join("data"),
            cache: dir.path().join("cache"),
        }),
        active: std::sync::Arc::new(Mutex::new(None)),
        job: std::sync::Arc::new(Mutex::new(photopipe::serve::JobState::default())),
    };
    let app = photopipe::serve::router(state);

    let start = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/analyze")
                .header("content-type", "application/json")
                .body(axum::body::Body::from(format!("{{\"folder\":{:?}}}", folder.to_str().unwrap())))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(start.status(), StatusCode::ACCEPTED);

    // Poll status until done (bounded).
    let mut stage = String::new();
    for _ in 0..200 {
        let resp = app
            .clone()
            .oneshot(Request::builder().uri("/api/analyze/status").body(axum::body::Body::empty()).unwrap())
            .await
            .unwrap();
        let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        stage = v["stage"].as_str().unwrap().to_string();
        if stage == "done" || stage == "failed" {
            assert_eq!(v["ml_ran"], false);
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert_eq!(stage, "done", "analyze did not reach done");
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `. ~/.cargo/env && cargo test -p photopipe --test serve analyze_job_runs_to_done_ml_skipped`
Expected: FAIL — `/api/analyze` route missing (404 / ACCEPTED assertion fails).

- [ ] **Step 3: Add the job progress sink + handlers**

In `crates/cli/src/serve/handlers.rs`, add:

```rust
use super::{ActiveLibrary, JobState};
use std::sync::{Arc, Mutex};

/// ProgressSink that writes into the shared JobState.
struct JobProgress(Arc<Mutex<JobState>>);
impl pipeline::ProgressSink for JobProgress {
    fn stage(&self, stage: &str) {
        let mut j = self.0.lock().unwrap();
        j.stage = stage.to_string();
        j.message = format!("{stage}…");
    }
    fn set_total(&self, total: u64) {
        self.0.lock().unwrap().files_total = total;
    }
    fn inc(&self) {
        self.0.lock().unwrap().files_done += 1;
    }
}

#[derive(Debug, Deserialize)]
pub struct AnalyzeRequest {
    pub folder: String,
}

/// Start the background analyze job for `folder`. 409 if one is already running.
pub async fn post_analyze(
    State(state): State<AppState>,
    Json(req): Json<AnalyzeRequest>,
) -> Result<(StatusCode, Json<JobState>), StatusCode> {
    let folder = expand_tilde(&PathBuf::from(req.folder));

    // Guard: reject if a job is in flight; otherwise seed the job state.
    {
        let mut job = state.job.lock().unwrap();
        if job.running() {
            return Err(StatusCode::CONFLICT);
        }
        *job = JobState {
            stage: "scanning".into(),
            files_done: 0,
            files_total: 0,
            ml_ran: false,
            folder: folder.to_string_lossy().into_owned(),
            message: "starting…".into(),
            error: None,
        };
    }

    let state2 = state.clone();
    std::thread::spawn(move || {
        let result = (|| -> anyhow::Result<ActiveLibrary> {
            let lib = state2.resolve_library(&folder, true)?;
            let hub = pipeline::models::ModelHub::from_config(&state2.cfg.models)
                .unwrap_or_else(|_| pipeline::models::ModelHub::empty());
            state2.job.lock().unwrap().ml_ran = !hub.is_empty();
            let progress = JobProgress(state2.job.clone());
            pipeline::analyze_folder(&lib.folder, &lib.catalog, &lib.cache, &hub, &state2.cfg, &progress)?;
            Ok(lib)
        })();
        match result {
            Ok(lib) => {
                state2.set_active(lib);
                let mut j = state2.job.lock().unwrap();
                j.stage = "done".into();
                j.message = "complete".into();
            }
            Err(e) => {
                tracing::warn!(error = %e, "analyze job failed");
                let mut j = state2.job.lock().unwrap();
                j.stage = "failed".into();
                j.error = Some(e.to_string());
            }
        }
    });

    Ok((StatusCode::ACCEPTED, Json(state.job.lock().unwrap().clone())))
}

/// Current analyze job state (polled by the UI).
pub async fn get_analyze_status(State(state): State<AppState>) -> Json<JobState> {
    Json(state.job.lock().unwrap().clone())
}
```

(`render_asset`/`preview_bytes` already imported `Arc` via this `use`? If `Arc`/`Mutex` are already imported at the top, drop the duplicate `use`.)

- [ ] **Step 4: Add routes**

In `crates/cli/src/serve/mod.rs`, add to `router` (before the `/:file` catch-all):

```rust
        .route("/api/analyze", post(handlers::post_analyze))
        .route("/api/analyze/status", get(handlers::get_analyze_status))
```

- [ ] **Step 5: Run tests + fmt + commit**

Run:
```bash
. ~/.cargo/env
cargo test -p photopipe --test serve analyze_job_runs_to_done_ml_skipped
cargo test -p photopipe --test serve
cargo fmt
cargo clippy --all-targets --all-features -- -D warnings
```
Expected: PASS.

```bash
git add crates/cli/src/serve/handlers.rs crates/cli/src/serve/mod.rs crates/cli/tests/serve.rs
git commit -m "feat(serve): background analyze job + status polling"
```

---

## Task 4: Folder browser + libraries + open + active endpoints

**Files:**
- Modify: `crates/cli/src/serve/handlers.rs`
- Modify: `crates/cli/src/serve/mod.rs` (routes)
- Test: `crates/cli/tests/serve.rs`

**Interfaces:**
- Consumes: `AppState` (`roots`, `cfg`, `resolve_library`, `set_active`, `active`), `pipeline::library::list_libraries`, `pipeline::count_pending`.
- Produces: `GET /api/fs`, `GET /api/libraries`, `GET /api/active`, `POST /api/open`.

- [ ] **Step 1: Write the failing test**

Append to `crates/cli/tests/serve.rs`:

```rust
#[tokio::test]
async fn fs_open_and_active_flow() {
    use axum::http::{Request, StatusCode};
    use image::{ImageBuffer, Rgb};
    use std::sync::Mutex;
    use tower::ServiceExt;

    let dir = tempfile::TempDir::new().unwrap();
    let folder = dir.path().join("trip");
    std::fs::create_dir_all(&folder).unwrap();
    let img: ImageBuffer<Rgb<u8>, Vec<u8>> = ImageBuffer::from_fn(20, 20, |_, _| Rgb([1, 2, 3]));
    img.save(folder.join("a.jpg")).unwrap();

    let mut cfg = pipeline::config::Config::default();
    cfg.models.model_dir = dir.path().join("no-models");
    let state = photopipe::serve::AppState {
        cfg: std::sync::Arc::new(cfg),
        roots: std::sync::Arc::new(pipeline::library::LibraryRoots { data: dir.path().join("data"), cache: dir.path().join("cache") }),
        active: std::sync::Arc::new(Mutex::new(None)),
        job: std::sync::Arc::new(Mutex::new(photopipe::serve::JobState::default())),
    };
    let app = photopipe::serve::router(state);

    // /api/fs over the temp dir lists `trip` with photo_count 0 (folder itself has the jpg; its parent lists trip).
    let (s, v) = get_json(app.clone(), &format!("/api/fs?path={}", dir.path().to_str().unwrap())).await;
    assert_eq!(s, StatusCode::OK);
    assert!(v["entries"].as_array().unwrap().iter().any(|e| e["name"] == "trip"));

    // analyze the folder so a library exists.
    let _ = app.clone().oneshot(Request::builder().method("POST").uri("/api/analyze")
        .header("content-type","application/json")
        .body(axum::body::Body::from(format!("{{\"folder\":{:?}}}", folder.to_str().unwrap()))).unwrap()).await.unwrap();
    for _ in 0..200 {
        let resp = app.clone().oneshot(Request::builder().uri("/api/analyze/status").body(axum::body::Body::empty()).unwrap()).await.unwrap();
        let b = axum::body::to_bytes(resp.into_body(), 1<<20).await.unwrap();
        if serde_json::from_slice::<serde_json::Value>(&b).unwrap()["stage"] == "done" { break; }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    // /api/libraries shows it.
    let (_s, libs) = get_json(app.clone(), "/api/libraries").await;
    assert!(libs.as_array().unwrap().iter().any(|l| l["folder"].as_str().unwrap().contains("trip")));

    // /api/open returns pending_new 0 right after analyze.
    let (s, ov) = post_json(app.clone(), "/api/open", serde_json::json!({"folder": folder.to_str().unwrap()})).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(ov["pending_new"], 0);
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `. ~/.cargo/env && cargo test -p photopipe --test serve fs_open_and_active_flow`
Expected: FAIL — `/api/fs`/`/api/libraries`/`/api/open` routes missing.

- [ ] **Step 3: Add the handlers**

In `crates/cli/src/serve/handlers.rs`, add:

```rust
use serde::Serialize;

#[derive(Serialize)]
pub struct FsEntry {
    pub name: String,
    pub path: String,
    pub photo_count: u64,
}
#[derive(Serialize)]
pub struct FsListing {
    pub path: Option<String>,
    pub parent: Option<String>,
    pub entries: Vec<FsEntry>,
}
#[derive(Debug, Deserialize)]
pub struct FsQuery {
    pub path: Option<String>,
}

/// List subdirectories of `path` (or roots when absent), each with a
/// non-recursive photo count. Directories only.
pub async fn get_fs(
    State(state): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<FsQuery>,
) -> Result<Json<FsListing>, StatusCode> {
    let exts = state.cfg.ingest.extensions.clone();
    let count_photos = move |dir: &std::path::Path| -> u64 {
        let mut n = 0u64;
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_file() {
                    let ext = p.extension().and_then(|x| x.to_str()).unwrap_or("");
                    if exts.iter().any(|x| x.eq_ignore_ascii_case(ext)) {
                        n += 1;
                    }
                }
            }
        }
        n
    };

    match q.path {
        None => {
            // Roots: drive letters on Windows, home + "/" on Unix.
            let mut entries = Vec::new();
            #[cfg(windows)]
            for letter in b'A'..=b'Z' {
                let root = format!("{}:\\", letter as char);
                if std::path::Path::new(&root).exists() {
                    entries.push(FsEntry { name: root.clone(), path: root, photo_count: 0 });
                }
            }
            #[cfg(unix)]
            {
                if let Some(home) = dirs::home_dir() {
                    entries.push(FsEntry { name: "~".into(), path: home.to_string_lossy().into_owned(), photo_count: 0 });
                }
                entries.push(FsEntry { name: "/".into(), path: "/".into(), photo_count: 0 });
            }
            Ok(Json(FsListing { path: None, parent: None, entries }))
        }
        Some(p) => {
            let dir = expand_tilde(&PathBuf::from(&p));
            let rd = std::fs::read_dir(&dir).map_err(|_| StatusCode::FORBIDDEN)?;
            let mut entries: Vec<FsEntry> = rd
                .flatten()
                .filter(|e| e.path().is_dir())
                .map(|e| {
                    let path = e.path();
                    FsEntry {
                        name: e.file_name().to_string_lossy().into_owned(),
                        photo_count: count_photos(&path),
                        path: path.to_string_lossy().into_owned(),
                    }
                })
                .collect();
            entries.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
            Ok(Json(FsListing {
                parent: dir.parent().map(|p| p.to_string_lossy().into_owned()),
                path: Some(dir.to_string_lossy().into_owned()),
                entries,
            }))
        }
    }
}

#[derive(Serialize)]
pub struct LibraryEntry {
    pub folder: String,
    pub photo_count: i64,
    pub last_analyzed: Option<i64>,
}

pub async fn get_libraries(State(state): State<AppState>) -> Result<Json<Vec<LibraryEntry>>, StatusCode> {
    let roots = (*state.roots).clone();
    let libs = tokio::task::spawn_blocking(move || pipeline::library::list_libraries(&roots))
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(
        libs.into_iter()
            .map(|l| LibraryEntry {
                folder: l.folder.to_string_lossy().into_owned(),
                photo_count: l.photo_count,
                last_analyzed: l.last_analyzed,
            })
            .collect(),
    ))
}

#[derive(Serialize)]
pub struct ActiveInfo {
    pub folder: String,
}

/// The active library, or `null` (200) when none.
pub async fn get_active(State(state): State<AppState>) -> Json<Option<ActiveInfo>> {
    Json(
        state
            .active
            .lock()
            .unwrap()
            .as_ref()
            .map(|l| ActiveInfo { folder: l.folder.to_string_lossy().into_owned() }),
    )
}

#[derive(Debug, Deserialize)]
pub struct OpenRequest {
    pub folder: String,
}
#[derive(Serialize)]
pub struct OpenResponse {
    pub folder: String,
    pub pending_new: u64,
}

/// Open an existing library, make it active, and report how many folder files
/// are new/changed (drives the "Re-analyze" nudge). 404 if no library exists.
pub async fn post_open(
    State(state): State<AppState>,
    Json(req): Json<OpenRequest>,
) -> Result<Json<OpenResponse>, StatusCode> {
    let folder = expand_tilde(&PathBuf::from(&req.folder));
    let cfg = state.cfg.clone();
    let s2 = state.clone();
    let folder2 = folder.clone();
    let res = tokio::task::spawn_blocking(move || -> anyhow::Result<(ActiveLibrary, u64)> {
        let lib = s2.resolve_library(&folder2, false)?;
        let pending = pipeline::count_pending(&lib.folder, &lib.catalog, &cfg.ingest)?;
        Ok((lib, pending))
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    match res {
        Ok((lib, pending_new)) => {
            let folder_str = lib.folder.to_string_lossy().into_owned();
            state.set_active(lib);
            Ok(Json(OpenResponse { folder: folder_str, pending_new }))
        }
        Err(_) => Err(StatusCode::NOT_FOUND),
    }
}
```

(On Unix, `get_fs` uses `dirs::home_dir()` — `dirs` is already a dependency of the pipeline crate but the CLI crate may need it added under `[dependencies]`; if `dirs` is not available in the cli crate, add `dirs = { workspace = true }` to `crates/cli/Cargo.toml`. Otherwise compute the home dir via `std::env::var("HOME")`.)

- [ ] **Step 4: Add routes**

In `crates/cli/src/serve/mod.rs` `router`, add (before `/:file`):

```rust
        .route("/api/fs", get(handlers::get_fs))
        .route("/api/libraries", get(handlers::get_libraries))
        .route("/api/active", get(handlers::get_active))
        .route("/api/open", post(handlers::post_open))
```

- [ ] **Step 5: Run tests + fmt + commit**

Run:
```bash
. ~/.cargo/env
cargo test -p photopipe --test serve fs_open_and_active_flow
cargo test --all
cargo fmt
cargo clippy --all-targets --all-features -- -D warnings
```
Expected: PASS.

```bash
git add crates/cli/src/serve/handlers.rs crates/cli/src/serve/mod.rs crates/cli/tests/serve.rs crates/cli/Cargo.toml
git commit -m "feat(serve): folder-browser, libraries, open, and active endpoints"
```

---

## Task 5: Frontend — router scaffold + Home + Browse

**Files:**
- Overwrite: `crates/cli/assets/index.html`
- Create: `crates/cli/assets/app.js` (router + shared helpers), `crates/cli/assets/home.js`, `crates/cli/assets/browse.js`
- Modify: `crates/cli/assets/style.css`

**Interfaces:**
- Consumes API: `GET /api/active`, `GET /api/libraries`, `GET /api/fs`, `POST /api/open`, `POST /api/analyze`.
- Produces: a view-router with `home` and `browse` views wired; `review` and `analyze` views are stubbed (filled in Task 6). **No automated test** — verified by build + curl + the Task 7 Playwright smoke.

- [ ] **Step 1: Write the asset files**

Overwrite `crates/cli/assets/index.html`:

```html
<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>photopipe</title>
  <link rel="stylesheet" href="/style.css">
</head>
<body>
  <div id="view-home" class="view"></div>
  <div id="view-browse" class="view hidden"></div>
  <div id="view-analyze" class="view hidden"></div>
  <div id="view-review" class="view hidden">
    <header>
      <button id="home-btn">← Home</button>
      <strong id="review-title">Review</strong>
      <span class="spacer"></span>
      <label>flag <select id="flag-filter">
        <option value="">any</option><option value="blur">blur</option>
        <option value="back_focus">back_focus</option><option value="overexposed">overexposed</option>
        <option value="underexposed">underexposed</option><option value="low_iqa">low_iqa</option>
      </select></label>
      <label>state <select id="decided-filter">
        <option value="">all</option><option value="false">undecided</option><option value="true">decided</option>
      </select></label>
      <button id="export-btn">Export keepers</button>
    </header>
    <div id="banners"></div>
    <main id="grid" class="grid"></main>
    <div id="detail" class="detail hidden"><img id="detail-img" alt=""><aside id="detail-meta"></aside></div>
    <footer id="counts"></footer>
  </div>
  <script type="module" src="/app.js"></script>
</body>
</html>
```

Create `crates/cli/assets/app.js`:

```js
// Shared fetch helper + tiny view router.
export async function api(method, url, body) {
  const opts = { method };
  if (body !== undefined) {
    opts.headers = { 'content-type': 'application/json' };
    opts.body = JSON.stringify(body);
  }
  const r = await fetch(url, opts);
  if (!r.ok) throw new Error(`${method} ${url} → ${r.status}`);
  const ct = r.headers.get('content-type') || '';
  return ct.includes('application/json') ? r.json() : r.text();
}

export function humanBytes(n) {
  const u = ['B', 'KB', 'MB', 'GB', 'TB'];
  let v = n, i = 0;
  while (v >= 1024 && i < u.length - 1) { v /= 1024; i++; }
  return i ? `${v.toFixed(1)} ${u[i]}` : `${n} B`;
}

const VIEWS = ['home', 'browse', 'analyze', 'review'];
export function show(view) {
  for (const v of VIEWS) {
    document.getElementById(`view-${v}`).classList.toggle('hidden', v !== view);
  }
}

import { renderHome } from '/home.js';
import { renderBrowse } from '/browse.js';
import { startAnalyze } from '/analyze.js';
import { openReview } from '/review.js';

// Exposed so views can navigate without circular imports.
window.pp = { api, humanBytes, show, renderHome, renderBrowse, startAnalyze, openReview };

async function boot() {
  // If a library is already active (serve <folder>), go straight to Review.
  try {
    const active = await api('GET', '/api/active');
    if (active && active.folder) { await openReview(active.folder); return; }
  } catch (_) { /* fall through to home */ }
  await renderHome();
}
boot();
```

Create `crates/cli/assets/home.js`:

```js
import { api, show } from '/app.js';

export async function renderHome() {
  show('home');
  const el = document.getElementById('view-home');
  el.innerHTML = `<div class="home">
    <h1>photopipe</h1>
    <button id="analyze-new" class="primary">Analyze a folder…</button>
    <h2>Recent libraries</h2>
    <div id="lib-list" class="lib-list">Loading…</div>
  </div>`;
  document.getElementById('analyze-new').onclick = () => window.pp.renderBrowse(null);

  const list = document.getElementById('lib-list');
  try {
    const libs = await api('GET', '/api/libraries');
    if (!libs.length) { list.textContent = 'None yet — analyze a folder to get started.'; return; }
    list.innerHTML = '';
    for (const l of libs) {
      const when = l.last_analyzed ? new Date(l.last_analyzed * 1000).toLocaleString() : 'never';
      const card = document.createElement('button');
      card.className = 'lib-card';
      card.innerHTML = `<div class="lib-folder">${l.folder}</div>
        <div class="lib-meta">${l.photo_count} photos · analyzed ${when}</div>`;
      card.onclick = async () => { await api('POST', '/api/open', { folder: l.folder }); window.pp.openReview(l.folder); };
      list.appendChild(card);
    }
  } catch (e) { list.textContent = `Failed to load libraries: ${e.message}`; }
}
```

Create `crates/cli/assets/browse.js`:

```js
import { api, show } from '/app.js';

export async function renderBrowse(path) {
  show('browse');
  const el = document.getElementById('view-browse');
  el.innerHTML = `<div class="browse">
    <header><button id="browse-home">← Home</button><span id="crumb" class="crumb"></span></header>
    <div id="fs-list" class="fs-list">Loading…</div>
    <footer><button id="analyze-here" class="primary" disabled>Analyze this folder</button></footer>
  </div>`;
  document.getElementById('browse-home').onclick = () => window.pp.renderHome();

  const q = path ? `?path=${encodeURIComponent(path)}` : '';
  let listing;
  try { listing = await api('GET', `/api/fs${q}`); }
  catch (e) { document.getElementById('fs-list').textContent = `Cannot read folder: ${e.message}`; return; }

  document.getElementById('crumb').textContent = listing.path || 'Pick a drive / location';
  const analyzeBtn = document.getElementById('analyze-here');
  if (listing.path) {
    analyzeBtn.disabled = false;
    analyzeBtn.onclick = () => window.pp.startAnalyze(listing.path);
  }

  const list = document.getElementById('fs-list');
  list.innerHTML = '';
  if (listing.parent) {
    const up = document.createElement('button');
    up.className = 'fs-row';
    up.textContent = '⬆ ..';
    up.onclick = () => renderBrowse(listing.parent);
    list.appendChild(up);
  }
  for (const e of listing.entries) {
    const row = document.createElement('button');
    row.className = 'fs-row';
    row.innerHTML = `<span class="fs-name">📁 ${e.name}</span><span class="fs-count">${e.photo_count ? e.photo_count + ' photos' : ''}</span>`;
    row.onclick = () => renderBrowse(e.path);
    list.appendChild(row);
  }
}
```

Append to `crates/cli/assets/style.css`:

```css
.view.hidden { display: none; }
.home, .browse { max-width: 880px; margin: 0 auto; padding: 24px; }
.home h1 { margin: 0 0 16px; }
button.primary { background: #2a6; color: #fff; border: none; padding: 10px 16px; border-radius: 6px; font-size: 15px; cursor: pointer; }
.lib-list { display: grid; gap: 8px; margin-top: 12px; }
.lib-card, .fs-row { display: flex; justify-content: space-between; gap: 12px; width: 100%; text-align: left;
  background: #1b1b1b; color: #ddd; border: 1px solid #333; border-radius: 6px; padding: 10px 12px; cursor: pointer; }
.lib-folder { font-weight: 600; } .lib-meta, .fs-count { color: #888; font-size: 13px; }
.browse header, .browse footer { display: flex; gap: 12px; align-items: center; padding: 8px 0; }
.crumb { color: #aaa; font-family: monospace; }
.fs-list { display: grid; gap: 4px; }
#banners { display: flex; flex-direction: column; }
.banner { padding: 8px 12px; background: #3a3a1b; color: #eed; border-bottom: 1px solid #553; display: flex; gap: 12px; align-items: center; }
.banner button { margin-left: auto; }
```

Create a minimal `crates/cli/assets/analyze.js` and `crates/cli/assets/review.js` **stubs** so the imports resolve (filled in Task 6):

`analyze.js`:
```js
import { api, show } from '/app.js';
export async function startAnalyze(folder) {
  show('analyze');
  document.getElementById('view-analyze').innerHTML = `<div class="browse"><p>Starting analysis of ${folder}…</p></div>`;
  await api('POST', '/api/analyze', { folder });
  // Progress polling + transition to review are implemented in Task 6.
}
```
`review.js`:
```js
export async function openReview(folder) {
  window.pp.show('review');
  document.getElementById('review-title').textContent = folder;
  // Grid/detail/keyboard/export are implemented in Task 6.
}
```

- [ ] **Step 2: Build + asset smoke**

Run:
```bash
. ~/.cargo/env && cargo build -p photopipe
XDG_DATA_HOME=$(mktemp -d) XDG_CACHE_HOME=$(mktemp -d) ./target/debug/photopipe serve --port 8806 &
SRV=$!; sleep 2
curl -s localhost:8806/ | grep -c view-home
curl -s localhost:8806/app.js | grep -c renderHome
curl -s localhost:8806/home.js | grep -c '/api/libraries'
curl -s localhost:8806/api/libraries
kill $SRV
```
Expected: index served with the four view divs; `app.js`/`home.js` served; `/api/libraries` returns `[]`.

- [ ] **Step 3: fmt + commit**

```bash
. ~/.cargo/env && cargo fmt && cargo test -p photopipe --test serve
git add crates/cli/assets
git commit -m "feat(ui): SPA router + Home + Browse views"
```

---

## Task 6: Frontend — Analyze progress + Review integration + banners

**Files:**
- Overwrite: `crates/cli/assets/analyze.js`, `crates/cli/assets/review.js`
- Modify: `crates/cli/assets/app.js` (if needed for shared state)

**Interfaces:**
- Consumes API: `GET /api/analyze/status`, `GET /api/photos`, `GET /api/photos/:id`, `GET /thumb/:id`, `GET /preview/:id`, `POST /api/decisions`, `GET /api/counts`, `POST /api/export`, `GET /api/export/estimate`, `POST /api/open`.
- Produces: the working Analyze progress screen and the full Review view (the previous review-UI behavior), with the ML-skipped and "N new — Re-analyze" banners.

- [ ] **Step 1: Implement the Analyze progress view**

Overwrite `crates/cli/assets/analyze.js`:

```js
import { api, show } from '/app.js';

export async function startAnalyze(folder) {
  show('analyze');
  const el = document.getElementById('view-analyze');
  el.innerHTML = `<div class="browse">
    <h2>Analyzing</h2>
    <div class="crumb">${folder}</div>
    <div id="an-stage">starting…</div>
    <div class="bar"><div id="an-fill" class="bar-fill"></div></div>
    <div id="an-detail"></div>
  </div>`;

  try { await api('POST', '/api/analyze', { folder }); }
  catch (e) {
    if (String(e.message).includes('409')) { document.getElementById('an-stage').textContent = 'An analysis is already running.'; return; }
    document.getElementById('an-stage').textContent = `Failed to start: ${e.message}`; return;
  }

  const poll = async () => {
    let s;
    try { s = await api('GET', '/api/analyze/status'); } catch (_) { setTimeout(poll, 1000); return; }
    document.getElementById('an-stage').textContent = s.message || s.stage;
    const pct = s.files_total ? Math.round((s.files_done / s.files_total) * 100) : 0;
    document.getElementById('an-fill').style.width = `${pct}%`;
    document.getElementById('an-detail').textContent =
      s.stage === 'scanning' ? `${s.files_done} / ${s.files_total} files` : '';
    if (s.stage === 'done') { window.pp.openReview(s.folder, { ml_ran: s.ml_ran }); return; }
    if (s.stage === 'failed') { document.getElementById('an-stage').textContent = `Failed: ${s.error || 'unknown error'}`; return; }
    setTimeout(poll, 1000);
  };
  poll();
}
```

Add to `style.css`:
```css
.bar { height: 14px; background: #222; border-radius: 7px; overflow: hidden; margin: 10px 0; }
.bar-fill { height: 100%; width: 0; background: #2a6; transition: width .3s; }
```

- [ ] **Step 2: Implement the full Review view**

Overwrite `crates/cli/assets/review.js` with the full review behavior (this is the previous `app.js` review code, now a module operating on the `#view-review` elements). It loads the grid, handles keyboard verdicts, detail view, filters, export, and shows the two banners:

```js
import { api, humanBytes, show } from '/app.js';

let photos = [], sel = 0, detailOpen = false, activeFolder = null;

const grid = () => document.getElementById('grid');
const countsEl = () => document.getElementById('counts');
const flagFilter = () => document.getElementById('flag-filter');
const decidedFilter = () => document.getElementById('decided-filter');

export async function openReview(folder, opts = {}) {
  show('review');
  activeFolder = folder;
  document.getElementById('review-title').textContent = folder;
  renderBanners(opts.ml_ran);
  wireChrome();
  await loadPhotos();
}

async function renderBanners(mlRan) {
  const b = document.getElementById('banners');
  b.innerHTML = '';
  if (mlRan === false) {
    const d = document.createElement('div');
    d.className = 'banner';
    d.textContent = 'Models not found — quality, subject-aware blur, and duplicate detection were skipped.';
    b.appendChild(d);
  }
  // Staleness: re-open to get pending_new.
  try {
    const o = await api('POST', '/api/open', { folder: activeFolder });
    if (o.pending_new > 0) {
      const d = document.createElement('div');
      d.className = 'banner';
      d.innerHTML = `<span>${o.pending_new} new photo(s) found.</span><button id="reanalyze">Re-analyze</button>`;
      d.querySelector('#reanalyze').onclick = () => window.pp.startAnalyze(activeFolder);
      b.appendChild(d);
    }
  } catch (_) {}
}

function wireChrome() {
  document.getElementById('home-btn').onclick = () => window.pp.renderHome();
  flagFilter().onchange = loadPhotos;
  decidedFilter().onchange = loadPhotos;
  document.getElementById('export-btn').onclick = onExport;
  document.onkeydown = onKey;
}

function qs() {
  const p = new URLSearchParams();
  if (flagFilter().value) p.set('flag_type', flagFilter().value);
  if (decidedFilter().value) p.set('decided', decidedFilter().value);
  const s = p.toString();
  return s ? `?${s}` : '';
}

async function loadPhotos() {
  photos = await api('GET', `/api/photos${qs()}`);
  if (sel >= photos.length) sel = Math.max(0, photos.length - 1);
  renderGrid();
  refreshCounts();
}

function tileClass(p) {
  let c = 'tile';
  if (p.verdict === 'keep') c += ' keep';
  else if (p.verdict === 'reject') c += ' reject';
  return c;
}

function renderGrid() {
  const g = grid();
  g.innerHTML = '';
  photos.forEach((p, i) => {
    const el = document.createElement('div');
    el.className = tileClass(p) + (i === sel ? ' sel' : '');
    const flags = p.flags.length ? p.flags.join(', ') : (p.group_id != null ? 'dup' : 'clean');
    el.innerHTML = `<img loading="lazy" src="/thumb/${p.file_id}" alt="">
      <span class="badge">${flags}${p.iqa_score != null ? ` · iqa ${p.iqa_score.toFixed(2)}` : ''}</span>`;
    el.addEventListener('click', () => { sel = i; openDetail(); });
    g.appendChild(el);
  });
  const selEl = g.querySelector('.tile.sel');
  if (selEl) selEl.scrollIntoView({ block: 'nearest' });
}

function showCounts(c) { countsEl().textContent = `keep ${c.kept} · reject ${c.rejected} · undecided ${c.undecided}`; }
async function refreshCounts() { try { showCounts(await api('GET', '/api/counts')); } catch { countsEl().textContent = ''; } }

async function setVerdict(action) {
  const p = photos[sel];
  if (!p) return;
  const c = await api('POST', '/api/decisions', { file_id: p.file_id, action });
  if (action === 'keep' || action === 'keeper') p.verdict = 'keep';
  else if (action === 'reject') p.verdict = 'reject';
  else if (action === 'undecide') { p.verdict = null; p.is_keeper = false; }
  showCounts(c);
  renderGrid();
  if (detailOpen) renderDetailMeta(p);
}

async function openDetail() {
  const p = photos[sel];
  if (!p) return;
  detailOpen = true;
  document.getElementById('detail').classList.remove('hidden');
  document.getElementById('detail-img').src = `/preview/${p.file_id}`;
  renderDetailMeta(p);
}
function closeDetail() { detailOpen = false; document.getElementById('detail').classList.add('hidden'); renderGrid(); }
function renderDetailMeta(p) {
  document.getElementById('detail-meta').innerHTML = `<dl>
    <dt>path</dt><dd>${p.path}</dd>
    <dt>flags</dt><dd>${p.flags.join(', ') || '—'}</dd>
    <dt>iqa</dt><dd>${p.iqa_score != null ? p.iqa_score.toFixed(3) : '—'}</dd>
    <dt>group</dt><dd>${p.group_id != null ? p.group_id : '—'}</dd>
    <dt>verdict</dt><dd>${p.verdict || 'undecided'}</dd>
  </dl><p>Space/Enter keep · X reject · U undecide · K keeper · F/Esc back</p>`;
}

function move(d) { if (!photos.length) return; sel = Math.min(photos.length - 1, Math.max(0, sel + d)); if (detailOpen) openDetail(); else renderGrid(); }

function onKey(e) {
  if (document.getElementById('view-review').classList.contains('hidden')) return;
  switch (e.key) {
    case 'j': case 'ArrowRight': move(1); break;
    case 'k': case 'ArrowLeft': move(-1); break;
    case ' ': case 'Enter': e.preventDefault(); setVerdict('keep'); break;
    case 'x': case 'X': setVerdict('reject'); break;
    case 'u': case 'U': setVerdict('undecide'); break;
    case 'K': setVerdict('keeper'); break;
    case 'f': case 'F': detailOpen ? closeDetail() : openDetail(); break;
    case 'Escape': if (detailOpen) closeDetail(); break;
  }
}

async function onExport() {
  try {
    const est = await api('GET', '/api/export/estimate');
    if (!confirm(`This will copy ${est.files} photo(s) (${humanBytes(est.bytes)}) to the "_keepers" folder (relative to where 'photopipe serve' was started). Continue?`)) return;
    const r = await api('POST', '/api/export', { regenerate: false });
    alert(`Copied ${r.files_copied} photo(s) (${humanBytes(r.bytes_copied)}), ${r.errors} error(s).`);
  } catch (err) { alert(`Export failed: ${err.message}`); }
}
```

- [ ] **Step 3: Build + asset smoke**

Run:
```bash
. ~/.cargo/env && cargo build -p photopipe && cargo test -p photopipe --test serve
XDG_DATA_HOME=$(mktemp -d) XDG_CACHE_HOME=$(mktemp -d) ./target/debug/photopipe serve --port 8807 &
SRV=$!; sleep 2
curl -s localhost:8807/analyze.js | grep -c '/api/analyze/status'
curl -s localhost:8807/review.js | grep -c '/api/photos'
kill $SRV
```
Expected: both modules served with the expected API calls; serve tests still pass.

- [ ] **Step 4: fmt + commit**

```bash
. ~/.cargo/env && cargo fmt
git add crates/cli/assets
git commit -m "feat(ui): analyze progress + full review view + banners"
```

---

## Task 7: Docs + final verification + browser smoke

**Files:**
- Modify: `README.md`

- [ ] **Step 1: Update the README**

Add a "Browser app" section near the top of the usage docs: `photopipe serve` (no folder) opens the app at `http://127.0.0.1:8787/`; the Home screen lists previously-analyzed folders and an "Analyze a folder" button; pick a folder in the browser, watch it analyze (scan → calibrate → dedupe), then review and export — all without the CLI. Note that `serve <folder>` still opens that folder's library directly, that analysis runs ML when models are present (and tells you in a banner when they're not), and that re-opening a folder offers a one-click incremental "Re-analyze" when new photos are found. Keep the CLI reference; cross-link the two. Verify all commands/flags mentioned against `crates/cli/src/main.rs`.

- [ ] **Step 2: Full verification sweep**

Run:
```bash
. ~/.cargo/env
cargo fmt
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all
cargo build -p photopipe && ./target/debug/photopipe doctor; echo "doctor exit: $?"
```
Expected: fmt clean; clippy 0 warnings; all tests pass; `doctor` exits 0.

- [ ] **Step 3: Browser smoke (Playwright, if available)**

Build a tiny photo folder, start the server with redirected app-data, and drive Home → Browse → Analyze → Review:
```bash
TMP=$(mktemp -d); mkdir -p "$TMP/shoot"
# (place or generate a few small .jpg files in "$TMP/shoot")
XDG_DATA_HOME=$(mktemp -d) XDG_CACHE_HOME=$(mktemp -d) ./target/release/photopipe serve --port 8808
```
Open `http://127.0.0.1:8808/`, click **Analyze a folder**, browse to the shoot, **Analyze this folder**, confirm the progress bar reaches done and the grid renders (with the "models skipped" banner if no models), then exercise keep/reject and **Export keepers**. Capture the result. (If models are absent, ML-skipped is expected.) Note any issue; fix before final sign-off.

- [ ] **Step 4: Commit**

```bash
git add README.md
git commit -m "docs: browser analyze workflow (Home → Browse → Analyze → Review)"
```

---

## Self-Review (completed during planning)

- **Spec coverage:** folder browser `GET /api/fs` (Task 4) ✓; background analyze job + polling, ML-skipped (`ml_ran`), single-job 409, no cancel (Task 3) ✓; auto-chain scan→calibrate→dedupe via `analyze_folder` (Task 1) ✓; active-library refactor + review endpoints 409 (Task 2) ✓; `POST /api/open` + `pending_new` staleness + incremental re-analyze (Tasks 1+4+6) ✓; `GET /api/libraries`/`GET /api/active` (Task 4) ✓; SPA Home→Browse→Analyze→Review + banners (Tasks 5–6) ✓; `serve` folder-optional (Task 2) ✓; non-destructive/127.0.0.1/DuckDB-only/no-deps (Global Constraints) ✓; per-task `cargo fmt`+commit (every task) ✓.
- **DuckDB single-connection rule:** `AppState::resolve_library` reuses the active library's connection when the target folder matches; the analyze job and `open` both go through it, so no second connection to an open file. Covered.
- **Type consistency:** `AppState { cfg, roots, active, job }`, `ActiveLibrary { folder, catalog, cache }`, `JobState { stage, files_done, files_total, ml_ran, folder, message, error }`, `ProgressSink { stage, set_total, inc }`, `AnalyzeReport { ml_ran, processed, skipped, errored, groups }`, `analyze_folder(folder, catalog, cache, hub, cfg, progress)`, `count_pending(folder, catalog, cfg)` — used consistently across tasks and the re-export list. Frontend module API (`api`/`humanBytes`/`show` exported from `app.js`; `window.pp` for cross-view nav) is consistent between `home.js`/`browse.js`/`analyze.js`/`review.js`.
- **Placeholder scan:** none; every code step carries full code or a precise edit. Task 5 ships intentional `analyze.js`/`review.js` stubs that Task 6 overwrites (explicitly noted).
- **Sequencing:** `AppState` settles in Task 2 (incl. the `job` field used by Task 3), so Task 3 adds handlers without re-shaping state. Every task ends green; the frontend (Tasks 5–6) builds on the Task 3–4 endpoints.
- **Known follow-up (not blocking):** progress bar is driven by the ingest stage (the slow decode part); the fast defects/ML sub-steps run within "scanning" without advancing the bar — acceptable for v1, noted so it isn't mistaken for a stall.
