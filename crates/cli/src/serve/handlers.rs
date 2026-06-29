//! HTTP handlers for the review server.

use axum::extract::{Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use pipeline::catalog::{DecisionCounts, ReviewFilter, ReviewGroup, ReviewListItem, Verdict};
use pipeline::config::expand_tilde;
use pipeline::{build_keepers_tree, KeepersReport};
use rust_embed::RustEmbed;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use super::{ActiveLibrary, AppState};

#[derive(RustEmbed)]
#[folder = "assets/"]
struct Assets;

pub async fn health() -> &'static str {
    "ok"
}

/// Serve the embedded index.html.
pub async fn index() -> Response {
    match Assets::get("index.html") {
        Some(f) => (
            [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
            f.data.into_owned(),
        )
            .into_response(),
        None => (StatusCode::INTERNAL_SERVER_ERROR, "index.html missing").into_response(),
    }
}

/// Serve any embedded asset by path (e.g. `app.js`, `style.css`).
pub async fn static_asset(Path(file): Path<String>) -> Response {
    match Assets::get(&file) {
        Some(f) => {
            let ct = match file.rsplit('.').next() {
                Some("js") => "text/javascript; charset=utf-8",
                Some("css") => "text/css; charset=utf-8",
                Some("html") => "text/html; charset=utf-8",
                _ => "application/octet-stream",
            };
            ([(header::CONTENT_TYPE, ct)], f.data.into_owned()).into_response()
        }
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// Query string for `/api/photos`.
#[derive(Debug, Deserialize)]
pub struct PhotoQuery {
    pub flag_type: Option<String>,
    pub decided: Option<bool>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

pub async fn list_photos(
    State(state): State<AppState>,
    Query(q): Query<PhotoQuery>,
) -> Result<Json<Vec<ReviewListItem>>, StatusCode> {
    let lib = state.active()?;
    let filter = ReviewFilter {
        flag_type: q.flag_type,
        decided: q.decided,
        limit: q.limit.unwrap_or(200),
        offset: q.offset.unwrap_or(0),
    };
    lib.catalog.review_list(&filter).map(Json).map_err(|e| {
        tracing::warn!(error = %e, "review_list failed");
        StatusCode::INTERNAL_SERVER_ERROR
    })
}

pub async fn photo_detail(State(state): State<AppState>, Path(id): Path<i64>) -> Response {
    let lib = match state.active() {
        Ok(l) => l,
        Err(s) => return s.into_response(),
    };
    match lib.catalog.dump_file_by_id(id) {
        Ok(Some(dump)) => Json(dump).into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => {
            tracing::warn!(error = %e, "dump_file_by_id failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

pub async fn list_groups(
    State(state): State<AppState>,
) -> Result<Json<Vec<ReviewGroup>>, StatusCode> {
    let lib = state.active()?;
    lib.catalog
        .duplicate_groups_for_review()
        .map(Json)
        .map_err(|e| {
            tracing::warn!(error = %e, "duplicate_groups_for_review failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })
}

const THUMB_EDGE: u32 = 320;
const THUMB_QUALITY: u8 = 78;
const PREVIEW_EDGE: u32 = 2048;
const PREVIEW_QUALITY: u8 = 85;

const PLACEHOLDER_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="320" height="240"><rect width="100%" height="100%" fill="#222"/><text x="50%" y="50%" fill="#888" font-family="sans-serif" font-size="16" text-anchor="middle" dominant-baseline="middle">no preview</text></svg>"##;

fn placeholder() -> Response {
    (
        [(header::CONTENT_TYPE, "image/svg+xml; charset=utf-8")],
        PLACEHOLDER_SVG,
    )
        .into_response()
}

fn webp_response(bytes: Vec<u8>) -> Response {
    ([(header::CONTENT_TYPE, "image/webp")], bytes).into_response()
}

pub async fn thumb(State(state): State<AppState>, Path(id): Path<i64>) -> Response {
    let lib = match state.active() {
        Ok(l) => l,
        Err(s) => return s.into_response(),
    };
    render_asset(lib, id, true).await
}

pub async fn preview(State(state): State<AppState>, Path(id): Path<i64>) -> Response {
    let lib = match state.active() {
        Ok(l) => l,
        Err(s) => return s.into_response(),
    };
    render_asset(lib, id, false).await
}

/// Locate the original by id and serve a WebP. Previews are served from (or
/// rendered into) the preview cache; thumbnails are derived by downscaling the
/// preview — never by re-decoding the original — so RAW formats whose embedded
/// preview cannot be re-extracted on demand still get a thumbnail from the
/// preview `scan` produced. Any failure falls back to the SVG placeholder.
/// Runs the blocking DB + image work on a blocking thread.
async fn render_asset(lib: ActiveLibrary, id: i64, is_thumb: bool) -> Response {
    let result = tokio::task::spawn_blocking(move || -> Option<Vec<u8>> {
        let loc = lib.catalog.lookup_file(id).ok()??;
        let hash = loc.content_hash;

        if is_thumb {
            // Thumbnail already cached?
            if lib.cache.has_thumb(hash) {
                if let Ok(bytes) = std::fs::read(lib.cache.thumb_path(hash)) {
                    return Some(bytes);
                }
            }
            // Derive the thumbnail from the preview rather than the original.
            let preview = preview_bytes(&lib, &loc.path, hash)?;
            match pipeline::downscale_webp(&preview, THUMB_EDGE, THUMB_QUALITY) {
                Ok(thumb) => {
                    let _ = lib.cache.write_thumb(hash, &thumb);
                    Some(thumb)
                }
                Err(e) => {
                    tracing::warn!(path = %loc.path.display(), error = %e, "thumb downscale failed");
                    None
                }
            }
        } else {
            preview_bytes(&lib, &loc.path, hash)
        }
    })
    .await
    .unwrap_or(None);

    match result {
        Some(bytes) => webp_response(bytes),
        None => placeholder(),
    }
}

/// Return the preview WebP for `hash`: the cached copy when present, otherwise
/// rendered from the original and cached. `None` (with a warning) when the
/// original cannot be rendered.
fn preview_bytes(lib: &ActiveLibrary, original: &std::path::Path, hash: u128) -> Option<Vec<u8>> {
    if lib.cache.has(hash) {
        if let Ok(bytes) = std::fs::read(lib.cache.path(hash)) {
            return Some(bytes);
        }
    }
    match pipeline::render_webp(original, PREVIEW_EDGE, PREVIEW_QUALITY) {
        Ok(bytes) => {
            let _ = lib.cache.write(hash, &bytes);
            Some(bytes)
        }
        Err(e) => {
            tracing::warn!(path = %original.display(), error = %e, "render failed");
            None
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct DecisionRequest {
    pub file_id: i64,
    /// "keep" | "reject" | "undecide" | "keeper"
    pub action: String,
    pub note: Option<String>,
}

pub async fn post_decision(
    State(state): State<AppState>,
    Json(req): Json<DecisionRequest>,
) -> Result<Json<DecisionCounts>, StatusCode> {
    let lib = state.active()?;
    let r = match req.action.as_str() {
        "keep" => lib
            .catalog
            .set_decision(req.file_id, Verdict::Keep, req.note.as_deref()),
        "reject" => lib
            .catalog
            .set_decision(req.file_id, Verdict::Reject, req.note.as_deref()),
        "undecide" => lib.catalog.clear_decision(req.file_id),
        "keeper" => lib.catalog.pick_keeper(req.file_id),
        _ => return Err(StatusCode::BAD_REQUEST),
    };
    r.map_err(|e| {
        tracing::warn!(error = %e, "decision write failed");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    lib.catalog
        .decision_counts()
        .map(Json)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

/// Read-only current counts (frontend loads this on startup).
pub async fn get_counts(State(state): State<AppState>) -> Result<Json<DecisionCounts>, StatusCode> {
    let lib = state.active()?;
    lib.catalog
        .decision_counts()
        .map(Json)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

#[derive(Debug, Deserialize)]
pub struct ExportRequest {
    /// Output directory. Defaults to `<cwd>/_keepers` when omitted.
    pub output: Option<String>,
    #[serde(default)]
    pub regenerate: bool,
}

pub async fn post_export(
    State(state): State<AppState>,
    Json(req): Json<ExportRequest>,
) -> Result<Json<KeepersReport>, StatusCode> {
    let lib = state.active()?;
    let out: PathBuf = req
        .output
        .map(|s| expand_tilde(&PathBuf::from(s)))
        .unwrap_or_else(|| PathBuf::from("_keepers"));
    let catalog = lib.catalog.clone();
    let regenerate = req.regenerate;
    tokio::task::spawn_blocking(move || build_keepers_tree(&catalog, &out, regenerate))
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .map(Json)
        .map_err(|e| {
            tracing::warn!(error = %e, "export failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })
}

#[derive(Debug, Deserialize)]
pub struct EstimateQuery {
    pub output: Option<String>,
}

/// ProgressSink that writes into the shared JobState.
struct JobProgress(Arc<Mutex<super::JobState>>);
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
) -> Result<(StatusCode, Json<super::JobState>), StatusCode> {
    let folder = expand_tilde(&PathBuf::from(req.folder));

    // Guard: reject if a job is in flight; otherwise seed the job state.
    {
        let mut job = state.job.lock().unwrap();
        if job.running() {
            return Err(StatusCode::CONFLICT);
        }
        *job = super::JobState {
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
        // catch_unwind so a panic deep in the pipeline sets stage=failed rather
        // than leaving the job stuck "running" (which would 409 every future
        // analyze until the server restarts).
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            (|| -> anyhow::Result<ActiveLibrary> {
                let lib = state2.resolve_library(&folder, true)?;
                let hub = pipeline::models::ModelHub::from_config(&state2.cfg.models)
                    .unwrap_or_else(|_| pipeline::models::ModelHub::empty());
                state2.job.lock().unwrap().ml_ran = !hub.is_empty();
                let progress = JobProgress(state2.job.clone());
                pipeline::analyze_folder(
                    &lib.folder,
                    &lib.catalog,
                    &lib.cache,
                    &hub,
                    &state2.cfg,
                    &progress,
                )?;
                Ok(lib)
            })()
        }));
        match outcome {
            Ok(Ok(lib)) => {
                state2.set_active(lib);
                let mut j = state2.job.lock().unwrap();
                j.stage = "done".into();
                j.message = "complete".into();
            }
            Ok(Err(e)) => {
                tracing::warn!(error = %e, "analyze job failed");
                let mut j = state2.job.lock().unwrap();
                j.stage = "failed".into();
                j.error = Some(e.to_string());
            }
            Err(panic) => {
                let msg = panic
                    .downcast_ref::<&str>()
                    .map(|s| s.to_string())
                    .or_else(|| panic.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "analyze panicked".into());
                tracing::error!(panic = %msg, "analyze job panicked");
                let mut j = state2.job.lock().unwrap();
                j.stage = "failed".into();
                j.error = Some(format!("internal error: {msg}"));
            }
        }
    });

    Ok((
        StatusCode::ACCEPTED,
        Json(state.job.lock().unwrap().clone()),
    ))
}

/// Current analyze job state (polled by the UI).
pub async fn get_analyze_status(State(state): State<AppState>) -> Json<super::JobState> {
    Json(state.job.lock().unwrap().clone())
}

/// Read-only estimate of the keepers copy (files + bytes that would be written).
pub async fn get_export_estimate(
    State(state): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<EstimateQuery>,
) -> Result<axum::Json<pipeline::CopyEstimate>, StatusCode> {
    let lib = state.active()?;
    let out: PathBuf = q
        .output
        .map(|s| expand_tilde(&PathBuf::from(s)))
        .unwrap_or_else(|| PathBuf::from("_keepers"));
    let catalog = lib.catalog.clone();
    tokio::task::spawn_blocking(move || pipeline::estimate_keepers_copy(&catalog, &out))
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .map(axum::Json)
        .map_err(|e| {
            tracing::warn!(error = %e, "export estimate failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })
}

// ── Folder browser, libraries, open, active ──────────────────────────────────

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
                    entries.push(FsEntry {
                        name: root.clone(),
                        path: root,
                        photo_count: 0,
                    });
                }
            }
            #[cfg(unix)]
            {
                if let Some(home) = dirs::home_dir() {
                    entries.push(FsEntry {
                        name: "~".into(),
                        path: home.to_string_lossy().into_owned(),
                        photo_count: 0,
                    });
                }
                entries.push(FsEntry {
                    name: "/".into(),
                    path: "/".into(),
                    photo_count: 0,
                });
            }
            Ok(Json(FsListing {
                path: None,
                parent: None,
                entries,
            }))
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
            entries.sort_by_key(|a| a.name.to_lowercase());
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

pub async fn get_libraries(
    State(state): State<AppState>,
) -> Result<Json<Vec<LibraryEntry>>, StatusCode> {
    let roots = (*state.roots).clone();
    let libs = tokio::task::spawn_blocking(move || pipeline::list_libraries(&roots))
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
    Json(state.active.lock().unwrap().as_ref().map(|l| ActiveInfo {
        folder: l.folder.to_string_lossy().into_owned(),
    }))
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

    // If an analyze job is in flight on this same folder, the catalog is held by
    // the analyze thread — opening a second connection would fail. Surface that
    // as "busy" (409) rather than attempting the open and reporting a stale 404.
    {
        let job = state.job.lock().unwrap();
        if job.running()
            && pipeline::library_key(std::path::Path::new(&job.folder))
                == pipeline::library_key(&folder)
        {
            return Err(StatusCode::CONFLICT);
        }
    }

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
            Ok(Json(OpenResponse {
                folder: folder_str,
                pending_new,
            }))
        }
        Err(e) => {
            tracing::warn!(folder = %folder.display(), error = %e, "post_open failed");
            Err(StatusCode::NOT_FOUND)
        }
    }
}
