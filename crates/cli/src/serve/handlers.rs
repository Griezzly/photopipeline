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
