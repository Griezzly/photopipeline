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

/// While a job is in flight, a concurrent analyze, a concurrent finish, and an
/// open of the same folder are all rejected with 409 (rather than attempting a
/// second DuckDB open, or two runs that each want every core).
///
/// Both directions are covered: analyze and finish share one job slot, so
/// either must reject the other. That single slot is the policy — finish is
/// serial precisely because `rawtherapee-cli` saturates the machine on its own.
#[tokio::test]
async fn busy_job_rejects_concurrent_analyze_and_open() {
    use axum::http::StatusCode;

    let dir = tempfile::TempDir::new().unwrap();
    let folder = dir.path().join("shoot");
    std::fs::create_dir_all(&folder).unwrap();

    let catalog = pipeline::catalog::Catalog::open(&dir.path().join("c.duckdb")).unwrap();
    let cache = pipeline::cache::Cache::open(dir.path().join("cache")).unwrap();
    let state = photopipe::serve::AppState {
        cfg: std::sync::Arc::new(pipeline::config::Config::default()),
        roots: std::sync::Arc::new(pipeline::library::LibraryRoots {
            data: dir.path().join("data"),
            cache: dir.path().join("cache"),
        }),
        // A library *is* open, so a 409 from /api/finish below can only be the
        // busy guard — not the no-library-open guard wearing the same code.
        active: std::sync::Arc::new(Mutex::new(Some(photopipe::serve::ActiveLibrary {
            folder: folder.clone(),
            catalog: std::sync::Arc::new(catalog),
            cache: std::sync::Arc::new(cache),
        }))),
        job: std::sync::Arc::new(Mutex::new(photopipe::serve::JobState::default())),
    };

    // Seed a running job on `folder` (simulates a fresh analyze in flight).
    {
        let mut j = state.job.lock().unwrap();
        j.kind = "analyze".into();
        j.stage = "scanning".into();
        j.folder = folder.to_string_lossy().into_owned();
    }
    let app = photopipe::serve::router(state.clone());

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

    // A finish while the analyze runs is rejected too. Skipped where no real
    // rawtherapee-cli is installed: post_finish probes the renderer *before*
    // the busy guard, on purpose (a setup problem must never take the job
    // slot), so without one this would 400 rather than 409 and prove nothing.
    if pipeline::develop::render::Pp3Renderer::new(&pipeline::config::DevelopConfig::default())
        .probe()
        .is_ok()
    {
        let (s, _) = post_json(app.clone(), "/api/finish", serde_json::json!({})).await;
        assert_eq!(
            s,
            StatusCode::CONFLICT,
            "a develop run must not start while an analysis is in flight"
        );
    }

    // …and the other direction: with a finish in the slot, analyze is rejected.
    {
        let mut j = state.job.lock().unwrap();
        j.kind = "finish".into();
        j.stage = "developing".into();
    }
    let (s, _) = post_json(
        app,
        "/api/analyze",
        serde_json::json!({"folder": folder.to_str().unwrap()}),
    )
    .await;
    assert_eq!(
        s,
        StatusCode::CONFLICT,
        "an analysis must not start while a develop run is in flight"
    );
}

// ── Develop screen endpoints ─────────────────────────────────────────────────

/// Like `post_json`, but keeps the body as text. `post_finish`'s renderer
/// refusal is a plain-string explanation, and asserting on it is the point of
/// `post_finish_reports_a_clear_error_when_the_renderer_is_missing`.
async fn post_text(
    app: axum::Router,
    uri: &str,
    body: serde_json::Value,
) -> (axum::http::StatusCode, String) {
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
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

/// An active library holding two kept RAWs and one kept JPG, so the estimate's
/// `keepers` and `raw_keepers` cannot be confused with one another.
fn state_with_keepers() -> (tempfile::TempDir, photopipe::serve::AppState) {
    use pipeline::catalog::Verdict;
    use pipeline::ingest::{FileFormat, IngestedFile};

    let dir = tempfile::TempDir::new().unwrap();
    let catalog = pipeline::catalog::Catalog::open(&dir.path().join("c.duckdb")).unwrap();
    let seeded: Vec<_> = [
        ("/lib/a.arw", FileFormat::Arw),
        ("/lib/b.arw", FileFormat::Arw),
        ("/lib/c.jpg", FileFormat::Jpg),
    ]
    .into_iter()
    .enumerate()
    .map(|(i, (path, format))| {
        (
            IngestedFile {
                path: std::path::PathBuf::from(path),
                content_hash: 0x100 + i as u128,
                size: 1,
                mtime_ns: 1,
                format,
                has_sidecar_jpg: false,
            },
            None,
        )
    })
    .collect();
    for id in catalog.flush_batch(&seeded).unwrap() {
        catalog.set_decision(id, Verdict::Keep, None).unwrap();
    }
    let cache = pipeline::cache::Cache::open(dir.path().join("cache")).unwrap();
    (dir, app_state_active(catalog, cache))
}

#[tokio::test]
async fn finish_estimate_reports_keepers_and_output_dir() {
    let (_dir, state) = state_with_keepers();
    let app = photopipe::serve::router(state);

    let (s, v) = get_json(app, "/api/finish/estimate").await;
    assert_eq!(s, axum::http::StatusCode::OK);
    assert_eq!(v["keepers"], 3);
    assert_eq!(
        v["raw_keepers"], 2,
        "a kept JPG is not developable — finish only develops RAWs"
    );
    // `<library>` resolved against the active library, not left as a template.
    assert_eq!(v["out_dir"], "/lib/_finished");
    // Present either way, and both are booleans rather than absent: the screen
    // branches on them before it will start a run.
    assert!(v["renderer_available"].is_boolean());
    assert!(v["look_available"].is_boolean());
}

#[tokio::test]
async fn finish_estimate_409_without_a_library() {
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
    let (s, _) = get_json(photopipe::serve::router(state), "/api/finish/estimate").await;
    assert_eq!(s, axum::http::StatusCode::CONFLICT);
}

/// A missing renderer is a setup problem. It must be refused up front with an
/// explanation, not accepted as a job that starts and dies on its first call —
/// which would also occupy the single job slot for no reason.
#[tokio::test]
async fn post_finish_reports_a_clear_error_when_the_renderer_is_missing() {
    let (_dir, mut state) = state_with_keepers();
    let mut cfg = pipeline::config::Config::default();
    cfg.develop.rawtherapee_path = "/nonexistent/rawtherapee-cli".into();
    state.cfg = std::sync::Arc::new(cfg);
    let job = state.job.clone();
    let app = photopipe::serve::router(state);

    let (s, body) = post_text(app, "/api/finish", serde_json::json!({"regenerate": false})).await;
    assert_eq!(s, axum::http::StatusCode::BAD_REQUEST);
    assert!(
        body.contains("rawtherapee"),
        "the refusal must name the missing dependency: {body}"
    );
    assert_eq!(
        job.lock().unwrap().stage,
        "idle",
        "a refused start must leave the job slot free"
    );
}

/// The embedded font must be served as a font, not application/octet-stream —
/// `static_asset` matches on extension and had no arm for `ttf`.
#[tokio::test]
async fn font_is_served_with_font_content_type() {
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    let dir = tempfile::TempDir::new().unwrap();
    let catalog = pipeline::catalog::Catalog::open(&dir.path().join("c.duckdb")).unwrap();
    let cache = pipeline::cache::Cache::open(dir.path().join("cache")).unwrap();
    let app = photopipe::serve::router(app_state_active(catalog, cache));

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/Manrope.ttf")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers()
            .get(axum::http::header::CONTENT_TYPE)
            .unwrap(),
        "font/ttf"
    );
}

/// Every asset `index.html` references must resolve through the `/:file` route.
/// Catches a renamed or forgotten module before it ships as a blank screen.
#[tokio::test]
async fn every_asset_referenced_by_index_resolves() {
    use axum::body::to_bytes;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    let dir = tempfile::TempDir::new().unwrap();
    let catalog = pipeline::catalog::Catalog::open(&dir.path().join("c.duckdb")).unwrap();
    let cache = pipeline::cache::Cache::open(dir.path().join("cache")).unwrap();
    let state = app_state_active(catalog, cache);

    let index = {
        let app = photopipe::serve::router(state.clone());
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        String::from_utf8(to_bytes(resp.into_body(), 1 << 20).await.unwrap().to_vec()).unwrap()
    };

    // Pull every root-relative href/src out of index.html.
    let mut refs: Vec<String> = Vec::new();
    for attr in ["href=\"/", "src=\"/"] {
        let mut rest = index.as_str();
        while let Some(i) = rest.find(attr) {
            rest = &rest[i + attr.len()..];
            let end = rest
                .find('"')
                .expect("unterminated attribute in index.html");
            let path = &rest[..end];
            if !path.is_empty() && !path.starts_with("api/") {
                refs.push(path.to_string());
            }
            rest = &rest[end..];
        }
    }
    assert!(
        refs.len() >= 3,
        "expected index.html to reference the stylesheets and app.js, found {refs:?}"
    );

    for path in refs {
        let app = photopipe::serve::router(state.clone());
        let resp = app
            .oneshot(
                Request::builder()
                    .uri(format!("/{path}"))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "/{path} did not resolve");
    }
}

/// GET `/{path}` through a fresh router, asserting 200, and return the body as
/// text. Used by the asset-manifest tests below.
async fn fetch_asset(state: &photopipe::serve::AppState, path: &str) -> String {
    use axum::body::to_bytes;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    let app = photopipe::serve::router(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .uri(format!("/{path}"))
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "/{path} is not embedded");
    String::from_utf8(to_bytes(resp.into_body(), 1 << 22).await.unwrap().to_vec()).unwrap()
}

/// Every `import('/x.js')` in app.js, in source order. app.js is the single
/// place the screen modules are listed, so parsing it is the whole manifest —
/// a module added there and not embedded fails the test that calls this
/// without anyone having to remember to update a list.
fn dynamic_imports_of(app_js: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = app_js;
    while let Some(i) = rest.find("import('/") {
        rest = &rest[i + "import('/".len()..];
        let end = rest.find('\'').expect("unterminated import() in app.js");
        out.push(rest[..end].to_string());
        rest = &rest[end..];
    }
    out
}

/// Every module the app dynamically imports must be embedded. `index.html`
/// only references app.js, so a missing screen module would otherwise surface
/// as a blank view at runtime rather than a failing build.
///
/// The manifest is *derived* from app.js's own `import()` calls rather than
/// hardcoded here: a hardcoded copy is exactly the drift this test exists to
/// catch. `index.html`'s own references — the two stylesheets and app.js — are
/// covered by `every_asset_referenced_by_index_resolves`. The font is *not*
/// among them: `tokens.css` requests `/Manrope.ttf` from its `@font-face`, so
/// nothing in `index.html` names it. `font_is_served_with_font_content_type` is
/// what covers the font.
#[tokio::test]
async fn every_screen_module_is_embedded() {
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    let dir = tempfile::TempDir::new().unwrap();
    let catalog = pipeline::catalog::Catalog::open(&dir.path().join("c.duckdb")).unwrap();
    let cache = pipeline::cache::Cache::open(dir.path().join("cache")).unwrap();
    let state = app_state_active(catalog, cache);

    let app_js = fetch_asset(&state, "app.js").await;
    let modules = dynamic_imports_of(&app_js);
    assert!(
        modules.len() >= 8,
        "expected app.js to dynamically import the screen modules, found {modules:?}"
    );

    for m in &modules {
        // fetch_asset asserts 200 for us.
        let body = fetch_asset(&state, m).await;
        assert!(!body.is_empty(), "/{m} is embedded but empty");
    }

    // icons.js is imported statically by the screen modules rather than
    // dynamically by app.js, so it never appears in the manifest above.
    fetch_asset(&state, "icons.js").await;

    // And nothing stale is left behind from the previous UI.
    for gone in ["home.js", "browse.js"] {
        let app = photopipe::serve::router(state.clone());
        let resp = app
            .oneshot(
                Request::builder()
                    .uri(format!("/{gone}"))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::NOT_FOUND,
            "/{gone} should have been deleted"
        );
    }
}

/// The `{ ... }` block that follows the first occurrence of `needle`, or None.
/// Brace-counted from the first `{` after the needle; the blocks it is used on
/// below contain no braces inside string literals or comments.
fn block_after<'a>(src: &'a str, needle: &str) -> Option<&'a str> {
    let start = src.find(needle)?;
    let open = start + src[start..].find('{')?;
    let mut depth = 0usize;
    for (i, c) in src[open..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&src[open..open + i + 1]);
                }
            }
            _ => {}
        }
    }
    None
}

/// A deliberately crude grep-over-the-embedded-assets test.
///
/// Normally a string search for a code pattern is a bad test. It earns its
/// place here because one specific defect — module-level state from the
/// previously-open library surviving a switch, and a keypress in the load
/// window then writing a decision with the old library's `file_id` against the
/// new library's catalog (every catalog numbers `file_id` and `group_id` from
/// 1, so the stale id hits a real, unrelated photo) — shipped five separate
/// times on this branch and was missed by every per-module review. It is
/// invisible to a serial UI test, because it only exists between the
/// synchronous `show()`/repaint and the awaited `load()`. It *is* trivially
/// visible to grep. So: grep.
///
/// Two invariants, both stated positively so a module that stops matching
/// shows up as a dropped count rather than a silent pass:
///
///  1. A module that registers a document-level `keydown` handler and can
///     write a decision must carry a stand-down guard, and — if it has a
///     `loading` flag of its own — must also stand down while that is set.
///  2. Every `!== lastFolder` block must clear the rows it cached for the
///     previous library, not just the UI state layered on top of them.
///
/// These match on exact source strings. If a future refactor renames the
/// guards, update the markers here in the same commit — do not delete the
/// test.
#[tokio::test]
async fn keyboard_modules_carry_cross_library_stand_down_guards() {
    let dir = tempfile::TempDir::new().unwrap();
    let catalog = pipeline::catalog::Catalog::open(&dir.path().join("c.duckdb")).unwrap();
    let cache = pipeline::cache::Cache::open(dir.path().join("cache")).unwrap();
    let state = app_state_active(catalog, cache);

    let app_js = fetch_asset(&state, "app.js").await;
    let modules = dynamic_imports_of(&app_js);

    let mut guarded: Vec<&str> = Vec::new();
    let mut loading_aware: Vec<&str> = Vec::new();
    let mut reset: Vec<&str> = Vec::new();

    for m in &modules {
        if !m.ends_with(".js") {
            continue;
        }
        let src = fetch_asset(&state, m).await;

        // (1) Anything that can turn a keystroke into a catalog write.
        let listens = src.contains("document.addEventListener('keydown'");
        let writes = src.contains("reviewApply") || src.contains("/api/decisions");
        if listens && writes {
            assert!(
                src.contains("host.lastElementChild !== root")
                    || src.contains("host.children.length")
                    || src.contains("state.view !=="),
                "{m} turns keys into catalog writes but has no stand-down guard: \
                 it must ignore keys when it is not the layer/view in focus"
            );
            if src.contains("let loading") {
                assert!(
                    src.contains("if (loading || loadError) return;"),
                    "{m} has its own loading flag but its keydown handler does not stand \
                     down on it — keys pressed during the load window would act on the \
                     rows of whichever library was open before"
                );
                loading_aware.push(m);
            }
            guarded.push(m);
        }

        // (2) Switching libraries must drop the previous library's data, not
        // just its filters.
        if let Some(block) = block_after(&src, "!== lastFolder") {
            assert!(
                block.contains("= [];"),
                "{m}'s folder-change block does not clear its cached rows; \
                 the previous library's file_id/group_id values would stay live \
                 until the new library's fetch resolves"
            );
            assert!(
                block.contains("loading = true;"),
                "{m}'s folder-change block does not set loading, so the screen \
                 (and its keydown guard) would not know the data is stale"
            );
            reset.push(m);
        }
    }

    // Guards against the whole test passing vacuously because a marker string
    // drifted: these are the modules that must be covered by each invariant.
    guarded.sort_unstable();
    assert_eq!(
        guarded,
        ["compare.js", "detail.js", "duplicates.js", "review.js"],
        "unexpected set of decision-writing keyboard modules"
    );
    // The `let loading` check above is the strongest thing this test asserts, and
    // it is gated on a variable name. Pin the set it fires on: renaming `loading`
    // would otherwise skip the gate silently and leave the test green.
    loading_aware.sort_unstable();
    assert_eq!(
        loading_aware,
        ["duplicates.js", "review.js"],
        "unexpected set of decision-writing keyboard modules with their own load \
         window — a rename here disables the stand-down assertion above"
    );
    reset.sort_unstable();
    assert_eq!(
        reset,
        ["duplicates.js", "review.js"],
        "unexpected set of modules with a lastFolder comparison"
    );
}

/// The SPA is served from rust-embed, so a file that exists on disk but was
/// not embedded 404s at runtime with no build-time warning. Assert both that
/// router.js ships and that app.js actually pulls it in — a router nobody
/// imports is the failure mode this catches.
#[tokio::test]
async fn router_asset_is_served_and_imported() {
    use axum::body::to_bytes;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    let dir = tempfile::TempDir::new().unwrap();
    let catalog = pipeline::catalog::Catalog::open(&dir.path().join("c.duckdb")).unwrap();
    let cache = pipeline::cache::Cache::open(dir.path().join("cache")).unwrap();
    let state = app_state_active(catalog, cache);

    let resp = photopipe::serve::router(state.clone())
        .oneshot(
            Request::builder()
                .uri("/router.js")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers()
            .get(axum::http::header::CONTENT_TYPE)
            .unwrap(),
        "text/javascript; charset=utf-8"
    );

    let resp = photopipe::serve::router(state)
        .oneshot(
            Request::builder()
                .uri("/app.js")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
    let app_js = String::from_utf8(body.to_vec()).unwrap();
    assert!(
        app_js.contains("/router.js"),
        "app.js does not import /router.js"
    );
    assert!(
        app_js.contains("startRouter"),
        "app.js never starts the router"
    );
}
