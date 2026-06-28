//! Local review web server.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::routing::{get, post};
use axum::Router;

use pipeline::cache::Cache;
use pipeline::catalog::Catalog;
use pipeline::config::Config;

pub mod handlers;

/// Shared, cheaply-cloneable server state.
#[derive(Clone)]
pub struct AppState {
    pub catalog: Arc<Catalog>,
    pub cache: Arc<Cache>,
    pub cfg: Arc<Config>,
}

/// Build the axum router. Routes are added across Tasks 8–10; the static
/// index and health check are wired here.
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/health", get(handlers::health))
        .route("/api/photos", get(handlers::list_photos))
        .route("/api/photos/:id", get(handlers::photo_detail))
        .route("/api/groups", get(handlers::list_groups))
        .route("/thumb/:id", get(handlers::thumb))
        .route("/preview/:id", get(handlers::preview))
        .route("/", get(handlers::index))
        .route("/api/decisions", post(handlers::post_decision))
        .route("/api/counts", get(handlers::get_counts))
        .route("/api/export", post(handlers::post_export))
        .route("/api/export/estimate", get(handlers::get_export_estimate))
        .route("/:file", get(handlers::static_asset))
        .with_state(state)
}

/// Open the folder's library and serve on `127.0.0.1:port` until Ctrl-C.
pub fn run(cfg: &Config, folder: &std::path::Path, port: u16) -> anyhow::Result<()> {
    let roots = pipeline::library::LibraryRoots::from_dirs()?;
    let lib = pipeline::library::open_or_create_library(&roots, folder)?;
    let state = AppState {
        catalog: Arc::new(lib.catalog),
        cache: Arc::new(lib.cache),
        cfg: Arc::new(cfg.clone()),
    };

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(async move {
        let addr = SocketAddr::from(([127, 0, 0, 1], port));
        let listener = tokio::net::TcpListener::bind(addr).await?;
        tracing::info!(%addr, folder = %folder.display(), "review server listening — open http://{addr}/");
        axum::serve(listener, router(state)).await?;
        Ok::<(), anyhow::Error>(())
    })
}
