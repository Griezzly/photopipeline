use std::sync::Arc;

#[tokio::test]
async fn health_endpoint_returns_ok() {
    use axum::body::to_bytes;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    let dir = tempfile::TempDir::new().unwrap();
    let catalog = pipeline::catalog::Catalog::open(&dir.path().join("c.duckdb")).unwrap();
    let cache = pipeline::cache::Cache::open(dir.path().join("cache")).unwrap();
    let state = photopipe::serve::AppState {
        catalog: Arc::new(catalog),
        cache: Arc::new(cache),
        cfg: Arc::new(pipeline::config::Config::default()),
    };
    let app = photopipe::serve::router(state);

    let resp = app
        .oneshot(Request::builder().uri("/api/health").body(axum::body::Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = to_bytes(resp.into_body(), 1024).await.unwrap();
    assert_eq!(&body[..], b"ok");
}
