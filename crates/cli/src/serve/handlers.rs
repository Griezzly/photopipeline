//! HTTP handlers for the review server.

use axum::extract::{Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use pipeline::catalog::{DecisionCounts, ReviewFilter, ReviewGroup, ReviewListItem, Verdict};
use pipeline::config::expand_tilde;
use pipeline::{build_keepers_tree, KeepersReport};
use rust_embed::RustEmbed;
use serde::Deserialize;
use std::path::PathBuf;

use super::AppState;

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
    let filter = ReviewFilter {
        flag_type: q.flag_type,
        decided: q.decided,
        limit: q.limit.unwrap_or(200),
        offset: q.offset.unwrap_or(0),
    };
    state.catalog.review_list(&filter).map(Json).map_err(|e| {
        tracing::warn!(error = %e, "review_list failed");
        StatusCode::INTERNAL_SERVER_ERROR
    })
}

pub async fn photo_detail(State(state): State<AppState>, Path(id): Path<i64>) -> Response {
    match state.catalog.dump_file_by_id(id) {
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
    state
        .catalog
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
    render_asset(state, id, true).await
}

pub async fn preview(State(state): State<AppState>, Path(id): Path<i64>) -> Response {
    render_asset(state, id, false).await
}

/// Locate the original by id and serve a WebP. Previews are served from (or
/// rendered into) the preview cache; thumbnails are derived by downscaling the
/// preview — never by re-decoding the original — so RAW formats whose embedded
/// preview cannot be re-extracted on demand still get a thumbnail from the
/// preview `scan` produced. Any failure falls back to the SVG placeholder.
/// Runs the blocking DB + image work on a blocking thread.
async fn render_asset(state: AppState, id: i64, is_thumb: bool) -> Response {
    let result = tokio::task::spawn_blocking(move || -> Option<Vec<u8>> {
        let loc = state.catalog.lookup_file(id).ok()??;
        let hash = loc.content_hash;

        if is_thumb {
            // Thumbnail already cached?
            if state.cache.has_thumb(hash) {
                if let Ok(bytes) = std::fs::read(state.cache.thumb_path(hash)) {
                    return Some(bytes);
                }
            }
            // Derive the thumbnail from the preview rather than the original.
            let preview = preview_bytes(&state, &loc.path, hash)?;
            match pipeline::downscale_webp(&preview, THUMB_EDGE, THUMB_QUALITY) {
                Ok(thumb) => {
                    let _ = state.cache.write_thumb(hash, &thumb);
                    Some(thumb)
                }
                Err(e) => {
                    tracing::warn!(path = %loc.path.display(), error = %e, "thumb downscale failed");
                    None
                }
            }
        } else {
            preview_bytes(&state, &loc.path, hash)
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
fn preview_bytes(state: &AppState, original: &std::path::Path, hash: u128) -> Option<Vec<u8>> {
    if state.cache.has(hash) {
        if let Ok(bytes) = std::fs::read(state.cache.path(hash)) {
            return Some(bytes);
        }
    }
    match pipeline::render_webp(original, PREVIEW_EDGE, PREVIEW_QUALITY) {
        Ok(bytes) => {
            let _ = state.cache.write(hash, &bytes);
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
    let r = match req.action.as_str() {
        "keep" => state
            .catalog
            .set_decision(req.file_id, Verdict::Keep, req.note.as_deref()),
        "reject" => state
            .catalog
            .set_decision(req.file_id, Verdict::Reject, req.note.as_deref()),
        "undecide" => state.catalog.clear_decision(req.file_id),
        "keeper" => state.catalog.pick_keeper(req.file_id),
        _ => return Err(StatusCode::BAD_REQUEST),
    };
    r.map_err(|e| {
        tracing::warn!(error = %e, "decision write failed");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    state
        .catalog
        .decision_counts()
        .map(Json)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

/// Read-only current counts (frontend loads this on startup).
pub async fn get_counts(State(state): State<AppState>) -> Result<Json<DecisionCounts>, StatusCode> {
    state
        .catalog
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
    let out: PathBuf = req
        .output
        .map(|s| expand_tilde(&PathBuf::from(s)))
        .unwrap_or_else(|| PathBuf::from("_keepers"));
    let cfg = state.cfg.clone();
    let catalog = state.catalog.clone();
    let regenerate = req.regenerate;
    tokio::task::spawn_blocking(move || build_keepers_tree(&catalog, &out, &cfg.output, regenerate))
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .map(Json)
        .map_err(|e| {
            tracing::warn!(error = %e, "export failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })
}
