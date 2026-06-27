//! Local review web server.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::routing::get;
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
        .route("/", get(handlers::index))
        .with_state(state)
}

/// Open the catalog + cache and serve on `127.0.0.1:port` until Ctrl-C.
pub fn run(cfg: &Config, port: u16) -> anyhow::Result<()> {
    let catalog = Catalog::open(&cfg.catalog.db_path)?;
    let cache = Cache::open(cfg.catalog.cache_dir.clone())?;
    let state = AppState {
        catalog: Arc::new(catalog),
        cache: Arc::new(cache),
        cfg: Arc::new(cfg.clone()),
    };

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(async move {
        let addr = SocketAddr::from(([127, 0, 0, 1], port));
        let listener = tokio::net::TcpListener::bind(addr).await?;
        tracing::info!(%addr, "review server listening — open http://{addr}/ in your browser");
        axum::serve(listener, router(state)).await?;
        Ok::<(), anyhow::Error>(())
    })
}
