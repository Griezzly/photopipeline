use image::{ImageBuffer, Rgb};
use pipeline::{
    analyze_ml, cache::Cache, catalog::Catalog, config::IngestConfig, ingest::ingest_directory,
    models::ModelHub,
};
use std::path::PathBuf;
use tempfile::TempDir;

fn make_jpg(path: &PathBuf, r: u8, g: u8, b: u8) {
    let img: ImageBuffer<Rgb<u8>, _> = ImageBuffer::from_fn(64, 64, |_, _| Rgb([r, g, b]));
    img.save(path).unwrap();
}

fn setup_with_files(n: usize) -> (Catalog, Cache, TempDir, TempDir, TempDir) {
    let input_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let cache_dir = TempDir::new().unwrap();

    let catalog = Catalog::open(&db_dir.path().join("test.duckdb")).unwrap();
    let cache = Cache::open(cache_dir.path().to_owned()).unwrap();

    for i in 0..n {
        let v = (i * 30 % 256) as u8;
        make_jpg(&input_dir.path().join(format!("img{i}.jpg")), v, v, v);
    }
    ingest_directory(
        &[input_dir.path().to_owned()],
        &catalog,
        &cache,
        &IngestConfig::default(),
    )
    .unwrap();

    (catalog, cache, input_dir, db_dir, cache_dir)
}

#[test]
fn analyze_ml_no_models_is_noop() {
    let (catalog, cache, _input, _db, _cache_dir) = setup_with_files(3);
    let hub = ModelHub::empty();

    let report = analyze_ml(&catalog, &cache, &hub, 64).unwrap();

    assert_eq!(report.embedded, 0, "no embedder → embedded == 0");
    assert_eq!(report.iqa_scored, 0, "no IQA → iqa_scored == 0");
    assert_eq!(report.errored, 0);
    assert_eq!(catalog.embedding_count().unwrap(), 0);
    assert_eq!(catalog.iqa_count().unwrap(), 0);
}

#[test]
fn analyze_ml_idempotent_no_models() {
    let (catalog, cache, _input, _db, _cache_dir) = setup_with_files(2);
    let hub = ModelHub::empty();

    let r1 = analyze_ml(&catalog, &cache, &hub, 64).unwrap();
    let r2 = analyze_ml(&catalog, &cache, &hub, 64).unwrap();

    // Both runs should be no-ops with an empty hub.
    assert_eq!(r1.embedded + r1.iqa_scored + r1.errored, 0);
    assert_eq!(r2.embedded + r2.iqa_scored + r2.errored, 0);
}

// Live model tests — only run when the ONNX files are present.
#[test]
fn analyze_ml_with_live_models_idempotent() {
    let emb_path = std::path::PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../models/dinov2_base.onnx"
    ));
    let iqa_path = std::path::PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../models/clip_iqa.onnx"
    ));
    if pipeline::models::skip_if_no_model(&emb_path)
        || pipeline::models::skip_if_no_model(&iqa_path)
    {
        return;
    }

    let hub = ModelHub::from_config(&pipeline::config::ModelsConfig {
        model_dir: emb_path.parent().unwrap().to_path_buf(),
        ..Default::default()
    })
    .expect("ModelHub::from_config failed");

    let (catalog, cache, _input, _db, _cache_dir) = setup_with_files(2);

    // First pass: should embed + score everything.
    let r1 = analyze_ml(&catalog, &cache, &hub, 64).unwrap();
    assert_eq!(r1.embedded, 2, "first pass: should embed 2 files");
    assert_eq!(r1.iqa_scored, 2, "first pass: should IQA-score 2 files");
    assert_eq!(r1.errored, 0);

    // Second pass: no new work (idempotency).
    let r2 = analyze_ml(&catalog, &cache, &hub, 64).unwrap();
    assert_eq!(r2.embedded, 0, "second pass: nothing to embed");
    assert_eq!(r2.iqa_scored, 0, "second pass: nothing to score");

    // Catalog counts should reflect both files.
    assert_eq!(catalog.embedding_count().unwrap(), 2);
    assert_eq!(catalog.iqa_count().unwrap(), 2);
}
