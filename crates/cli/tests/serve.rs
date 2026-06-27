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
        .oneshot(
            Request::builder()
                .uri("/api/health")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = to_bytes(resp.into_body(), 1024).await.unwrap();
    assert_eq!(&body[..], b"ok");
}

async fn get_json(app: axum::Router, uri: &str) -> (axum::http::StatusCode, serde_json::Value) {
    use axum::body::to_bytes;
    use axum::http::Request;
    use tower::ServiceExt;
    let resp = app
        .oneshot(
            Request::builder()
                .uri(uri)
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), 1 << 20).await.unwrap();
    let val = if bytes.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
    };
    (status, val)
}

fn state_with_one_file() -> (tempfile::TempDir, photopipe::serve::AppState, i64) {
    use pipeline::ingest::{FileFormat, IngestedFile};
    use std::sync::Arc;
    let dir = tempfile::TempDir::new().unwrap();
    let catalog = pipeline::catalog::Catalog::open(&dir.path().join("c.duckdb")).unwrap();
    let file = IngestedFile {
        path: std::path::PathBuf::from("/lib/a.jpg"),
        content_hash: 0xABCD,
        size: 1,
        mtime_ns: 1,
        format: FileFormat::Jpg,
        has_sidecar_jpg: false,
    };
    let id = catalog.flush_batch(&[(file, None)]).unwrap()[0];
    let cache = pipeline::cache::Cache::open(dir.path().join("cache")).unwrap();
    let state = photopipe::serve::AppState {
        catalog: Arc::new(catalog),
        cache: Arc::new(cache),
        cfg: Arc::new(pipeline::config::Config::default()),
    };
    (dir, state, id)
}

#[tokio::test]
async fn photos_and_detail_and_groups() {
    let (_dir, state, id) = state_with_one_file();
    let app = photopipe::serve::router(state);

    let (s, v) = get_json(app.clone(), "/api/photos").await;
    assert_eq!(s, axum::http::StatusCode::OK);
    assert_eq!(v.as_array().unwrap().len(), 1);
    assert_eq!(v[0]["file_id"], id);
    assert_eq!(v[0]["content_hash"], "0000000000000000000000000000abcd");

    let (s, v) = get_json(app.clone(), &format!("/api/photos/{id}")).await;
    assert_eq!(s, axum::http::StatusCode::OK);
    assert_eq!(v["file"]["id"], id);

    let (s, _) = get_json(app.clone(), "/api/photos/999999").await;
    assert_eq!(s, axum::http::StatusCode::NOT_FOUND);

    let (s, v) = get_json(app, "/api/groups").await;
    assert_eq!(s, axum::http::StatusCode::OK);
    assert!(v.as_array().unwrap().is_empty());
}

#[tokio::test]
async fn thumb_renders_from_real_jpg_and_caches() {
    use axum::body::to_bytes;
    use axum::http::{Request, StatusCode};
    use image::{ImageBuffer, Rgb};
    use pipeline::ingest::{FileFormat, IngestedFile};
    use std::sync::Arc;
    use tower::ServiceExt;

    let dir = tempfile::TempDir::new().unwrap();
    let lib = dir.path().join("lib");
    std::fs::create_dir_all(&lib).unwrap();
    let p = lib.join("a.jpg");
    let img: ImageBuffer<Rgb<u8>, Vec<u8>> = ImageBuffer::from_fn(40, 30, |_, _| Rgb([1, 2, 3]));
    img.save(&p).unwrap();

    let catalog = pipeline::catalog::Catalog::open(&dir.path().join("c.duckdb")).unwrap();
    let file = IngestedFile {
        path: p,
        content_hash: 0x55,
        size: 1,
        mtime_ns: 1,
        format: FileFormat::Jpg,
        has_sidecar_jpg: false,
    };
    let id = catalog.flush_batch(&[(file, None)]).unwrap()[0];
    let cache = pipeline::cache::Cache::open(dir.path().join("cache")).unwrap();
    let state = photopipe::serve::AppState {
        catalog: Arc::new(catalog),
        cache: Arc::new(cache),
        cfg: Arc::new(pipeline::config::Config::default()),
    };
    let app = photopipe::serve::router(state);

    let resp = app
        .oneshot(
            Request::builder()
                .uri(format!("/thumb/{id}"))
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let ct = resp
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert_eq!(ct, "image/webp");
    let body = to_bytes(resp.into_body(), 1 << 20).await.unwrap();
    assert_eq!(&body[0..4], b"RIFF");
}

#[tokio::test]
async fn thumb_for_missing_file_returns_svg_placeholder() {
    let (_dir, state, _id) = state_with_one_file();
    let app = photopipe::serve::router(state);
    let (status, ct) = {
        use axum::http::Request;
        use tower::ServiceExt;
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/thumb/999999")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let ct = resp
            .headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        (resp.status(), ct)
    };
    assert_eq!(status, axum::http::StatusCode::OK);
    assert!(ct.starts_with("image/svg+xml"));
}

async fn post_json(
    app: axum::Router,
    uri: &str,
    body: serde_json::Value,
) -> (axum::http::StatusCode, serde_json::Value) {
    use axum::body::to_bytes;
    use axum::http::Request;
    use tower::ServiceExt;
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header("content-type", "application/json")
                .body(axum::body::Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), 1 << 20).await.unwrap();
    let val = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, val)
}

#[tokio::test]
async fn decision_roundtrip_updates_counts() {
    let (_dir, state, id) = state_with_one_file();
    let app = photopipe::serve::router(state);

    let (s, v) = post_json(
        app.clone(),
        "/api/decisions",
        serde_json::json!({ "file_id": id, "action": "reject" }),
    )
    .await;
    assert_eq!(s, axum::http::StatusCode::OK);
    assert_eq!(v["rejected"], 1);
    assert_eq!(v["kept"], 0);

    let (_s, v) = post_json(
        app.clone(),
        "/api/decisions",
        serde_json::json!({ "file_id": id, "action": "keep" }),
    )
    .await;
    assert_eq!(v["kept"], 1);
    assert_eq!(v["rejected"], 0);

    let (_s, v) = post_json(
        app,
        "/api/decisions",
        serde_json::json!({ "file_id": id, "action": "undecide" }),
    )
    .await;
    assert_eq!(v["kept"], 0);
    assert_eq!(v["undecided"], 1);

    // read-only counts endpoint reflects the same state
    let (s, v) = get_json(
        photopipe::serve::router(state_with_one_file().1),
        "/api/counts",
    )
    .await;
    assert_eq!(s, axum::http::StatusCode::OK);
    assert_eq!(v["undecided"], 1);
    assert_eq!(v["kept"], 0);
}

#[tokio::test]
async fn thumb_derives_from_preview_cache_when_original_unrenderable() {
    // Regression: state_with_one_file inserts content_hash 0xABCD at a path that
    // does not exist on disk, so rendering the original would fail. With the
    // preview cache pre-populated, /thumb must downscale that preview rather
    // than fall back to the placeholder.
    use axum::body::to_bytes;
    use axum::http::{Request, StatusCode};
    use image::{ImageBuffer, Rgb};
    use tower::ServiceExt;

    let (dir, state, id) = state_with_one_file();

    // Produce a real preview webp and store it in the PREVIEW cache slot (0xABCD).
    let jpg = dir.path().join("seed.jpg");
    let img: ImageBuffer<Rgb<u8>, Vec<u8>> =
        ImageBuffer::from_fn(120, 90, |x, _| Rgb([(x % 256) as u8, 1, 2]));
    img.save(&jpg).unwrap();
    let preview = pipeline::render_webp(&jpg, 2048, 85).unwrap();
    state.cache.write(0xABCD, &preview).unwrap();

    let app = photopipe::serve::router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri(format!("/thumb/{id}"))
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let ct = resp
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert_eq!(ct, "image/webp"); // derived from preview cache, not the placeholder
    let body = to_bytes(resp.into_body(), 1 << 20).await.unwrap();
    assert_eq!(&body[0..4], b"RIFF");
}
