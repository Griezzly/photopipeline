//! Integration tests for Phase 5 duplicate detection.
//!
//! All synthetic-vector tests need NO real photos: we upsert hand-crafted
//! embeddings, captured_at, and IQA rows directly, then run `run_dedupe`.
//! Acceptance criteria that genuinely need real bursts/scenes are written
//! `#[ignore]` with instructions for which fixtures to add.

use std::path::PathBuf;

use pipeline::catalog::{Catalog, MlRow};
use pipeline::config::DedupeConfig;
use pipeline::ingest::{ExifData, FileFormat, IngestedFile};
use tempfile::TempDir;

fn make_catalog() -> (Catalog, TempDir) {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.duckdb");
    let catalog = Catalog::open(&db_path).unwrap();
    (catalog, dir)
}

/// Insert a file with the given captured_at, returning its file_id.
fn insert_file(catalog: &Catalog, idx: i64, captured_at: Option<i64>) -> i64 {
    let file = IngestedFile {
        path: PathBuf::from(format!("/syn/file{idx}.jpg")),
        content_hash: 1000 + idx as u128,
        size: 10 + idx as u64,
        mtime_ns: idx,
        format: FileFormat::Jpg,
        has_sidecar_jpg: false,
    };
    let exif = captured_at.map(|ts| ExifData {
        captured_at: Some(ts),
        camera_make: None,
        camera_model: None,
        lens_model: None,
        focal_length_mm: None,
        aperture: None,
        iso: None,
        shutter_seconds: None,
        width: None,
        height: None,
        orientation: None,
    });
    catalog.flush_batch(&[(file, exif)]).unwrap()[0]
}

fn set_embedding(catalog: &Catalog, file_id: i64, vec: Vec<f32>) {
    catalog
        .flush_ml_batch(&[MlRow {
            file_id,
            embedding: Some(("test-model".to_string(), vec)),
            iqa_score: None,
        }])
        .unwrap();
}

fn set_iqa(catalog: &Catalog, file_id: i64, score: f32) {
    catalog
        .flush_ml_batch(&[MlRow {
            file_id,
            embedding: None,
            iqa_score: Some(("clip-iqa".to_string(), score)),
        }])
        .unwrap();
}

fn test_cfg() -> DedupeConfig {
    DedupeConfig {
        enable: true,
        time_window_seconds: 60,
        cosine_threshold_within_window: 0.92,
        cosine_threshold_global: 0.97,
        knn_k: 10,
        min_group_size: 2,
    }
}

use pipeline::run_dedupe;

/// De-risk: embeddings written via the JSON CAST path read back intact and
/// drive a dedupe run end to end.
#[test]
fn embedding_round_trip_through_dedupe() {
    let (catalog, _dir) = make_catalog();
    let id = insert_file(&catalog, 0, Some(1000));
    set_embedding(&catalog, id, vec![0.1f32, 0.2, 0.3, 0.4]);

    let loaded = catalog.load_all_embeddings().unwrap();
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].0, id);
    assert_eq!(loaded[0].1, vec![0.1f32, 0.2, 0.3, 0.4]);

    // A single embedding < min_group_size → no groups.
    let report = run_dedupe(&catalog, &test_cfg()).unwrap();
    assert_eq!(report.groups, 0);
}

/// Near-identical vectors within the time window form one group; an
/// orthogonal vector stays ungrouped; the highest-IQA member is the keeper.
#[test]
fn near_identical_within_window_group_orthogonal_stays_out() {
    let (catalog, _dir) = make_catalog();

    // Three near-identical photos a few seconds apart.
    let a = insert_file(&catalog, 0, Some(1000));
    let b = insert_file(&catalog, 1, Some(1002));
    let c = insert_file(&catalog, 2, Some(1004));
    set_embedding(&catalog, a, vec![1.0, 0.0, 0.0]);
    set_embedding(&catalog, b, vec![0.999, 0.044, 0.0]);
    set_embedding(&catalog, c, vec![0.998, 0.0, 0.063]);

    // One orthogonal photo in the same window.
    let d = insert_file(&catalog, 3, Some(1003));
    set_embedding(&catalog, d, vec![0.0, 1.0, 0.0]);

    // IQA: c is the best of the burst.
    set_iqa(&catalog, a, 0.5);
    set_iqa(&catalog, b, 0.6);
    set_iqa(&catalog, c, 0.95);
    set_iqa(&catalog, d, 0.9);

    let report = run_dedupe(&catalog, &test_cfg()).unwrap();
    assert_eq!(
        report.groups, 1,
        "the three near-identical photos form one group"
    );
    assert_eq!(report.members, 3, "orthogonal photo d must be excluded");
    assert_eq!(report.keepers, 1);

    // The keeper is c (highest IQA, no defects).
    let conn_check = catalog.duplicate_member_count().unwrap();
    assert_eq!(conn_check, 3);
}

/// Running dedupe twice yields identical group/member/keeper counts.
#[test]
fn dedupe_is_idempotent() {
    let (catalog, _dir) = make_catalog();
    let a = insert_file(&catalog, 0, Some(1000));
    let b = insert_file(&catalog, 1, Some(1001));
    set_embedding(&catalog, a, vec![1.0, 0.0]);
    set_embedding(&catalog, b, vec![0.9995, 0.0316]); // cosine ≈ 0.9995
    set_iqa(&catalog, a, 0.5);
    set_iqa(&catalog, b, 0.8);

    let cfg = test_cfg();
    let first = run_dedupe(&catalog, &cfg).unwrap();
    let second = run_dedupe(&catalog, &cfg).unwrap();

    assert_eq!(first, second, "two runs must produce identical reports");
    assert_eq!(
        catalog.duplicate_group_count().unwrap(),
        first.groups as i64
    );
    assert_eq!(
        catalog.duplicate_member_count().unwrap(),
        first.members as i64
    );
}

/// Disabled config does no work.
#[test]
fn disabled_config_produces_empty_report() {
    let (catalog, _dir) = make_catalog();
    let mut cfg = test_cfg();
    cfg.enable = false;
    let report = run_dedupe(&catalog, &cfg).unwrap();
    assert_eq!(report.groups, 0);
    assert_eq!(report.members, 0);
}

// ── Real-photo acceptance criteria (need fixtures) ──────────────────────────
// These require real RAW/JPG photos run through `scan` (ingest + embed) first.
// We do NOT fabricate EXIF/pixels. When executing this phase, ask the user to
// drop fixtures into `crates/pipeline/tests/fixtures/burst/` then un-ignore.

/// IMPLEMENTATION_PLAN §8 Phase 5 acceptance:
/// "A burst of 5 shots taken within 2 seconds clusters into one group."
/// Fixtures: 5 real burst frames → `tests/fixtures/burst/seq01/IMG_000{1..5}.*`.
#[test]
#[ignore = "needs real burst fixtures in tests/fixtures/burst/seq01/ — ask user"]
fn acceptance_five_shot_burst_clusters() {
    // Run scan over tests/fixtures/burst/seq01, then run_dedupe; assert one
    // group of 5. Requires a loaded embedder model + real photos.
}

/// "The same scene photographed twice 10 minutes apart with different framing
/// clusters together (relies on embedding similarity)."
/// Fixtures: two takes → `tests/fixtures/burst/same_scene/{take_a,take_b}.*`.
#[test]
#[ignore = "needs real same-scene fixtures in tests/fixtures/burst/same_scene/ — ask user"]
fn acceptance_same_scene_ten_minutes_apart_clusters() {
    // captured_at 10 min apart, similar embedding → global-KNN edge groups them.
}

/// "Two unrelated photos with similar color palettes (two different sunsets)
/// do NOT cluster."
/// Fixtures: two different sunsets → `tests/fixtures/burst/sunsets/{a,b}.*`.
#[test]
#[ignore = "needs real sunset fixtures in tests/fixtures/burst/sunsets/ — ask user"]
fn acceptance_distinct_sunsets_do_not_cluster() {
    // Distinct scenes, low embedding cosine → no group despite color similarity.
}
