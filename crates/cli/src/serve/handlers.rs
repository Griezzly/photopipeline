//! HTTP handlers for the review server.

use axum::extract::{Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use pipeline::catalog::{ReviewFilter, ReviewGroup, ReviewListItem};
use rust_embed::RustEmbed;
use serde::Deserialize;

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
    state
        .catalog
        .review_list(&filter)
        .map(Json)
        .map_err(|e| {
            tracing::warn!(error = %e, "review_list failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })
}

pub async fn photo_detail(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Response {
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

/// Locate the original by id, serve a cached webp if present, else render and
/// cache it. Any failure falls back to the SVG placeholder. Runs the blocking
/// DB + image work on a blocking thread.
async fn render_asset(state: AppState, id: i64, is_thumb: bool) -> Response {
    let result = tokio::task::spawn_blocking(move || -> Option<Vec<u8>> {
        let loc = state.catalog.lookup_file(id).ok()??;
        let hash = loc.content_hash;
        let (edge, quality) = if is_thumb {
            (THUMB_EDGE, THUMB_QUALITY)
        } else {
            (PREVIEW_EDGE, PREVIEW_QUALITY)
        };

        // Cache hit?
        let cached_path = if is_thumb { state.cache.thumb_path(hash) } else { state.cache.path(hash) };
        let cached = if is_thumb { state.cache.has_thumb(hash) } else { state.cache.has(hash) };
        if cached {
            if let Ok(bytes) = std::fs::read(&cached_path) {
                return Some(bytes);
            }
        }

        // Render on demand.
        match pipeline::render_webp(&loc.path, edge, quality) {
            Ok(bytes) => {
                let _ = if is_thumb {
                    state.cache.write_thumb(hash, &bytes)
                } else {
                    state.cache.write(hash, &bytes)
                };
                Some(bytes)
            }
            Err(e) => {
                tracing::warn!(path = %loc.path.display(), error = %e, "render failed");
                None
            }
        }
    })
    .await
    .unwrap_or(None);

    match result {
        Some(bytes) => webp_response(bytes),
        None => placeholder(),
    }
}
