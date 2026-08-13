//! Local review web server.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::Router;

use pipeline::cache::Cache;
use pipeline::catalog::Catalog;
use pipeline::config::Config;
use pipeline::library::{library_key, open_existing_library, open_or_create_library, LibraryRoots};

pub mod handlers;

/// The currently-open library the review endpoints operate on.
#[derive(Clone)]
pub struct ActiveLibrary {
    pub folder: PathBuf,
    pub catalog: Arc<Catalog>,
    pub cache: Arc<Cache>,
}

/// Counts from a finished `finish` run, for the Develop screen's summary.
///
/// Lives on `JobState` rather than behind a second status endpoint: there is
/// one job slot, so there is one place to read a job's outcome from.
#[derive(Clone, Default, serde::Serialize)]
pub struct FinishSummary {
    pub rendered: u64,
    pub skipped: u64,
    pub errored: u64,
    pub skipped_unsupported: u64,
    pub pruned: u64,
    /// Where the JPEGs landed, so the screen can name it without re-asking.
    pub out_dir: String,
}

/// Live state of the (single) background job — analyze *or* finish.
///
/// Deliberately one slot. Both saturate the machine (finish is serial precisely
/// because `rawtherapee-cli` uses every core internally and each intermediate
/// TIFF is ~145 MB), so letting them overlap would thrash. "One job at a time,
/// 409 otherwise" is that policy, and it is why finish reuses this rather than
/// adding a second slot.
#[derive(Clone, serde::Serialize)]
pub struct JobState {
    /// Which command is running, so a screen polling the shared slot can tell
    /// whether the job in flight is the one it started: analyze | finish | "".
    pub kind: String,
    // analyze: idle | scanning | detecting defects | scoring quality |
    //          calibrating | grouping duplicates | done | failed
    // finish:  idle | developing | pruning | done | failed
    pub stage: String,
    /// Where the current photo has got to, within the `developing` phase:
    /// measuring | rendering | applying look | encoding. Empty for analyze,
    /// whose per-item work is too fast to be worth naming. The strings are the
    /// contract with `assets/develop.js`; `finish_folder` documents the order.
    pub step: String,
    /// Filename of the photo `step` refers to. Display only.
    pub item: String,
    pub files_done: u64,
    pub files_total: u64,
    pub ml_ran: bool,
    pub folder: String,
    pub message: String,
    pub error: Option<String>,
    /// Present only once a finish run has completed successfully.
    pub summary: Option<FinishSummary>,
}

impl Default for JobState {
    fn default() -> Self {
        Self {
            kind: String::new(),
            stage: "idle".into(),
            step: String::new(),
            item: String::new(),
            files_done: 0,
            files_total: 0,
            ml_ran: false,
            folder: String::new(),
            message: String::new(),
            error: None,
            summary: None,
        }
    }
}

impl JobState {
    /// True while a run is in flight — any stage that is not a terminal or the
    /// initial idle state. (Kept name-agnostic so new phase labels stay covered.)
    pub fn running(&self) -> bool {
        !matches!(self.stage.as_str(), "idle" | "done" | "failed")
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

/// Build the axum router. Routes are added across Tasks 8–10; the static
/// index and health check are wired here.
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/health", get(handlers::health))
        .route("/api/photos", get(handlers::list_photos))
        .route("/api/photos/:id", get(handlers::photo_detail))
        .route("/api/groups", get(handlers::list_groups))
        .route("/api/clusters", get(handlers::list_clusters))
        .route("/thumb/:id", get(handlers::thumb))
        .route("/preview/:id", get(handlers::preview))
        .route("/", get(handlers::index))
        .route("/api/decisions", post(handlers::post_decision))
        .route("/api/counts", get(handlers::get_counts))
        .route("/api/export", post(handlers::post_export))
        .route("/api/export/estimate", get(handlers::get_export_estimate))
        .route("/api/analyze", post(handlers::post_analyze))
        .route("/api/analyze/status", get(handlers::get_analyze_status))
        .route("/api/finish", post(handlers::post_finish))
        .route("/api/finish/status", get(handlers::get_finish_status))
        .route("/api/finish/estimate", get(handlers::get_finish_estimate))
        .route("/api/fs", get(handlers::get_fs))
        .route("/api/libraries", get(handlers::get_libraries))
        .route("/api/active", get(handlers::get_active))
        .route("/api/open", post(handlers::post_open))
        .route("/:file", get(handlers::static_asset))
        .with_state(state)
}

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

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(async move {
        let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
        let listener = tokio::net::TcpListener::bind(addr).await?;
        tracing::info!(%addr, "review server listening — open http://{addr}/");
        axum::serve(listener, router(state)).await?;
        Ok::<(), anyhow::Error>(())
    })
}
