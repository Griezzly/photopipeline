//! HTTP handlers for the review server.

use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use rust_embed::RustEmbed;

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

/// `_state` is unused for now; later handlers consume it.
#[allow(dead_code)]
fn touch(_state: &AppState) {}
