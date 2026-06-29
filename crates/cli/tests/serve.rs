use std::sync::Mutex;

/// Build an AppState with `catalog`/`cache` as the active library.
fn app_state_active(
    catalog: pipeline::catalog::Catalog,
    cache: pipeline::cache::Cache,
) -> photopipe::serve::AppState {
    photopipe::serve::AppState {
        cfg: std::sync::Arc::new(pipeline::config::Config::default()),
        roots: std::sync::Arc::new(pipeline::library::LibraryRoots {
            data: std::path::PathBuf::from("/unused"),
            cache: std::path::PathBuf::from("/unused"),
        }),
        active: std::sync::Arc::new(Mutex::new(Some(photopipe::serve::ActiveLibrary {
            folder: std::path::PathBuf::from("/lib"),
            catalog: std::sync::Arc::new(catalog),
            cache: std::sync::Arc::new(cache),
        }))),
        job: std::sync::Arc::new(Mutex::new(photopipe::serve::JobState::default())),
    }
}

#[tokio::test]
async fn health_endpoint_returns_ok() {
    use axum::body::to_bytes;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    let dir = tempfile::TempDir::new().unwrap();
    let catalog = pipeline::catalog::Catalog::open(&dir.path().join("c.duckdb")).unwrap();
    let cache = pipeline::cache::Cache::open(dir.path().join("cache")).unwrap();
    let state = app_state_active(catalog, cache);
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
    let state = app_state_active(catalog, cache);
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
    let state = app_state_active(catalog, cache);
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
    // Access cache through the active library
    {
        let active = state.active.lock().unwrap();
        active
            .as_ref()
            .unwrap()
            .cache
            .write(0xABCD, &preview)
            .unwrap();
    }

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

#[tokio::test]
async fn export_estimate_reports_files_and_bytes() {
    use image::{ImageBuffer, Rgb};
    use pipeline::catalog::Verdict;
    use pipeline::ingest::{FileFormat, IngestedFile};

    let dir = tempfile::TempDir::new().unwrap();
    let lib = dir.path().join("lib");
    std::fs::create_dir_all(&lib).unwrap();
    let p = lib.join("a.jpg");
    let img: ImageBuffer<Rgb<u8>, Vec<u8>> = ImageBuffer::from_fn(16, 16, |_, _| Rgb([1, 2, 3]));
    img.save(&p).unwrap();

    let catalog = pipeline::catalog::Catalog::open(&dir.path().join("c.duckdb")).unwrap();
    let file = IngestedFile {
        path: p.clone(),
        content_hash: 1,
        size: 1,
        mtime_ns: 1,
        format: FileFormat::Jpg,
        has_sidecar_jpg: false,
    };
    let id = catalog.flush_batch(&[(file, None)]).unwrap()[0];
    catalog.set_decision(id, Verdict::Keep, None).unwrap();

    let cache = pipeline::cache::Cache::open(dir.path().join("cache")).unwrap();
    let state = app_state_active(catalog, cache);
    let out = dir.path().join("_keepers");
    let uri = format!("/api/export/estimate?output={}", out.to_str().unwrap());
    let (s, v) = get_json(photopipe::serve::router(state), &uri).await;
    assert_eq!(s, axum::http::StatusCode::OK);
    assert_eq!(v["files"], 1);
    assert!(
        v["bytes"].as_u64().unwrap() > 0,
        "expected nonzero bytes: {v}"
    );
}

#[tokio::test]
async fn analyze_job_runs_to_done_ml_skipped() {
    use axum::http::{Request, StatusCode};
    use image::{ImageBuffer, Rgb};
    use std::sync::Mutex;
    use tower::ServiceExt;

    let dir = tempfile::TempDir::new().unwrap();
    let folder = dir.path().join("photos");
    std::fs::create_dir_all(&folder).unwrap();
    let img: ImageBuffer<Rgb<u8>, Vec<u8>> = ImageBuffer::from_fn(40, 30, |_, _| Rgb([4, 5, 6]));
    img.save(folder.join("a.jpg")).unwrap();

    // App-state with a models-less config (model_dir empty → ModelHub::empty()).
    let mut cfg = pipeline::config::Config::default();
    cfg.models.model_dir = dir.path().join("no-models");
    let state = photopipe::serve::AppState {
        cfg: std::sync::Arc::new(cfg),
        roots: std::sync::Arc::new(pipeline::library::LibraryRoots {
            data: dir.path().join("data"),
            cache: dir.path().join("cache"),
        }),
        active: std::sync::Arc::new(Mutex::new(None)),
        job: std::sync::Arc::new(Mutex::new(photopipe::serve::JobState::default())),
    };
    let app = photopipe::serve::router(state);

    let start = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/analyze")
                .header("content-type", "application/json")
                .body(axum::body::Body::from(format!(
                    "{{\"folder\":{:?}}}",
                    folder.to_str().unwrap()
                )))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(start.status(), StatusCode::ACCEPTED);

    // Poll status until done (bounded).
    let mut stage = String::new();
    for _ in 0..200 {
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/analyze/status")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        stage = v["stage"].as_str().unwrap().to_string();
        if stage == "done" || stage == "failed" {
            assert_eq!(v["ml_ran"], false);
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert_eq!(stage, "done", "analyze did not reach done");
}

#[tokio::test]
async fn review_endpoints_409_when_no_library_open() {
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;
    let dir = tempfile::TempDir::new().unwrap();
    let state = photopipe::serve::AppState {
        cfg: std::sync::Arc::new(pipeline::config::Config::default()),
        roots: std::sync::Arc::new(pipeline::library::LibraryRoots {
            data: dir.path().join("d"),
            cache: dir.path().join("c"),
        }),
        active: std::sync::Arc::new(Mutex::new(None)),
        job: std::sync::Arc::new(Mutex::new(photopipe::serve::JobState::default())),
    };
    let resp = photopipe::serve::router(state)
        .oneshot(
            Request::builder()
                .uri("/api/photos")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn fs_open_and_active_flow() {
    use axum::http::{Request, StatusCode};
    use image::{ImageBuffer, Rgb};
    use std::sync::Mutex;
    use tower::ServiceExt;

    let dir = tempfile::TempDir::new().unwrap();
    let folder = dir.path().join("trip");
    std::fs::create_dir_all(&folder).unwrap();
    let img: ImageBuffer<Rgb<u8>, Vec<u8>> = ImageBuffer::from_fn(20, 20, |_, _| Rgb([1, 2, 3]));
    img.save(folder.join("a.jpg")).unwrap();

    let mut cfg = pipeline::config::Config::default();
    cfg.models.model_dir = dir.path().join("no-models");
    let state = photopipe::serve::AppState {
        cfg: std::sync::Arc::new(cfg),
        roots: std::sync::Arc::new(pipeline::library::LibraryRoots {
            data: dir.path().join("data"),
            cache: dir.path().join("cache"),
        }),
        active: std::sync::Arc::new(Mutex::new(None)),
        job: std::sync::Arc::new(Mutex::new(photopipe::serve::JobState::default())),
    };
    let app = photopipe::serve::router(state);

    // /api/fs over the temp dir lists `trip` with photo_count 0 (folder itself has the jpg; its parent lists trip).
    let (s, v) = get_json(
        app.clone(),
        &format!("/api/fs?path={}", dir.path().to_str().unwrap()),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert!(v["entries"]
        .as_array()
        .unwrap()
        .iter()
        .any(|e| e["name"] == "trip"));

    // analyze the folder so a library exists.
    let _ = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/analyze")
                .header("content-type", "application/json")
                .body(axum::body::Body::from(format!(
                    "{{\"folder\":{:?}}}",
                    folder.to_str().unwrap()
                )))
                .unwrap(),
        )
        .await
        .unwrap();
    for _ in 0..200 {
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/analyze/status")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let b = axum::body::to_bytes(resp.into_body(), 1 << 20)
            .await
            .unwrap();
        if serde_json::from_slice::<serde_json::Value>(&b).unwrap()["stage"] == "done" {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    // /api/libraries shows it.
    let (_s, libs) = get_json(app.clone(), "/api/libraries").await;
    assert!(libs
        .as_array()
        .unwrap()
        .iter()
        .any(|l| l["folder"].as_str().unwrap().contains("trip")));

    // /api/open returns pending_new 0 right after analyze.
    let (s, ov) = post_json(
        app.clone(),
        "/api/open",
        serde_json::json!({"folder": folder.to_str().unwrap()}),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(ov["pending_new"], 0);
}

/// While a job is in flight, both a concurrent analyze and an open of the same
/// folder are rejected with 409 (rather than attempting a second DuckDB open).
#[tokio::test]
async fn busy_job_rejects_concurrent_analyze_and_open() {
    use axum::http::StatusCode;

    let dir = tempfile::TempDir::new().unwrap();
    let folder = dir.path().join("shoot");
    std::fs::create_dir_all(&folder).unwrap();

    let state = photopipe::serve::AppState {
        cfg: std::sync::Arc::new(pipeline::config::Config::default()),
        roots: std::sync::Arc::new(pipeline::library::LibraryRoots {
            data: dir.path().join("data"),
            cache: dir.path().join("cache"),
        }),
        active: std::sync::Arc::new(Mutex::new(None)),
        job: std::sync::Arc::new(Mutex::new(photopipe::serve::JobState::default())),
    };

    // Seed a running job on `folder` (simulates a fresh analyze in flight).
    {
        let mut j = state.job.lock().unwrap();
        j.stage = "scanning".into();
        j.folder = folder.to_string_lossy().into_owned();
    }
    let app = photopipe::serve::router(state);

    // A second analyze (any folder) is rejected.
    let (s, _) = post_json(
        app.clone(),
        "/api/analyze",
        serde_json::json!({"folder": folder.to_str().unwrap()}),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);

    // Opening the in-flight folder is rejected (no second connection attempt).
    let (s, _) = post_json(
        app.clone(),
        "/api/open",
        serde_json::json!({"folder": folder.to_str().unwrap()}),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
}
