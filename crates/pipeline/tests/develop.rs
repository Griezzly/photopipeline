use pipeline::catalog::Catalog;
use pipeline::develop::decide::EditRecipe;
use pipeline::develop::measure::RawStats;
use pipeline::develop::render::Pp3Renderer;

fn temp_catalog() -> (tempfile::TempDir, Catalog) {
    let dir = tempfile::TempDir::new().unwrap();
    let cat = Catalog::open(&dir.path().join("catalog.duckdb")).unwrap();
    (dir, cat)
}

#[test]
fn migration_v4_creates_develop_tables() {
    let (_dir, cat) = temp_catalog();
    assert_eq!(cat.schema_version().unwrap(), 4);
}

#[test]
fn migration_v4_tables_accept_rows() {
    let (_dir, cat) = temp_catalog();
    // A raw_stats row needs a real files row to satisfy the FK.
    let conn = cat.raw_conn_for_test();
    conn.execute_batch(
        "INSERT INTO files (path, content_hash, size_bytes, mtime_ns, file_format, last_processed)
         VALUES ('/tmp/a.arw', 'deadbeef', 100, 0, 'arw', 0);
         INSERT INTO raw_stats VALUES
             ((SELECT id FROM files WHERE path='/tmp/a.arw'),
              0.01, 0.18, 0.90, 0.95, 0.001, 0.002, 2.0, 1.0, 1.5, NULL, NULL, NULL);",
    )
    .unwrap();
}

fn sample_stats() -> RawStats {
    RawStats {
        p1: 0.01,
        p50: 0.18,
        p99: 0.90,
        p999: 0.95,
        clipped_frac: 0.002,
        black_frac: 0.004,
        wb_r: 2.1,
        wb_g: 1.0,
        wb_b: 1.6,
        illum_r: Some(0.4),
        illum_g: Some(0.5),
        illum_b: Some(0.3),
    }
}

/// Seed one file with a decision and return its id.
fn seed_file(cat: &Catalog, path: &str, verdict: &str) -> i64 {
    let conn = cat.raw_conn_for_test();
    conn.execute(
        "INSERT INTO files (path, content_hash, size_bytes, mtime_ns, file_format, last_processed)
         VALUES (?, 'hash-a', 100, 0, 'arw', 0)",
        duckdb::params![path],
    )
    .unwrap();
    let id: i64 = conn
        .query_row(
            "SELECT id FROM files WHERE path = ?",
            duckdb::params![path],
            |r| r.get(0),
        )
        .unwrap();
    conn.execute(
        "INSERT INTO decisions (file_id, verdict, is_keeper, note, decided_at)
         VALUES (?, ?, false, NULL, 0)",
        duckdb::params![id, verdict],
    )
    .unwrap();
    id
}

#[test]
fn raw_stats_round_trip() {
    let (_dir, cat) = temp_catalog();
    let id = seed_file(&cat, "/tmp/a.arw", "keep");
    cat.upsert_raw_stats(id, &sample_stats()).unwrap();
    let got = cat.get_raw_stats(id).unwrap().expect("row should exist");
    assert_eq!(got, sample_stats());
}

/// Upsert must overwrite, not duplicate — file_id is the primary key and a
/// re-measure after a config change has to replace the old row.
#[test]
fn raw_stats_upsert_overwrites() {
    let (_dir, cat) = temp_catalog();
    let id = seed_file(&cat, "/tmp/a.arw", "keep");
    cat.upsert_raw_stats(id, &sample_stats()).unwrap();
    let mut second = sample_stats();
    second.p50 = 0.42;
    cat.upsert_raw_stats(id, &second).unwrap();
    assert_eq!(cat.get_raw_stats(id).unwrap().unwrap().p50, 0.42);
}

/// NULL illuminant columns must survive the round trip as None, not 0.0 —
/// decide() branches on whether the estimate exists at all.
#[test]
fn raw_stats_null_illuminant_round_trips_as_none() {
    let (_dir, cat) = temp_catalog();
    let id = seed_file(&cat, "/tmp/a.arw", "keep");
    let mut s = sample_stats();
    s.illum_r = None;
    s.illum_g = None;
    s.illum_b = None;
    cat.upsert_raw_stats(id, &s).unwrap();
    let got = cat.get_raw_stats(id).unwrap().unwrap();
    assert_eq!(got.illum_r, None);
}

/// The work list is `verdict = 'keep'` (spec A8), NOT is_keeper. A plain keep
/// writes is_keeper = false, so keying on it would skip every photo outside a
/// duplicate group.
#[test]
fn work_list_selects_keeps_not_just_group_keepers() {
    let (_dir, cat) = temp_catalog();
    seed_file(&cat, "/tmp/keep.arw", "keep");
    seed_file(&cat, "/tmp/reject.arw", "reject");
    let work = cat.keepers_to_develop().unwrap();
    assert_eq!(work.len(), 1, "only the kept file belongs in the work list");
    assert_eq!(work[0].path, std::path::PathBuf::from("/tmp/keep.arw"));
    assert_eq!(work[0].content_hash, "hash-a");
    // No exif row was seeded, so the month falls back.
    assert_eq!(work[0].year_month, "unknown-date");
}

/// End-to-end render through the real `rawtherapee-cli`. Gated: skips cleanly
/// on a machine without RawTherapee installed, in the style of
/// `pipeline::models::skip_if_no_model`.
///
/// Run for real with:
/// ```text
/// PHOTOPIPE_TEST_RAWTHERAPEE=/opt/homebrew/bin/rawtherapee-cli \
/// PHOTOPIPE_TEST_RAW="$PWD/example-pictures/DSC03073.ARW" \
/// cargo test -p pipeline --test develop -- --nocapture
/// ```
#[test]
fn real_renderer_produces_a_tiff_and_touches_nothing_beside_the_source() {
    let exe = match std::env::var("PHOTOPIPE_TEST_RAWTHERAPEE") {
        Ok(v) => v,
        Err(_) => {
            eprintln!(
                "skipping: PHOTOPIPE_TEST_RAWTHERAPEE not set (no rawtherapee-cli configured)"
            );
            return;
        }
    };
    let raw_path = match std::env::var("PHOTOPIPE_TEST_RAW") {
        Ok(v) => v,
        Err(_) => {
            eprintln!("skipping: PHOTOPIPE_TEST_RAW not set (no sample RAW configured)");
            return;
        }
    };
    let raw = std::path::PathBuf::from(&raw_path);
    assert!(
        raw.exists(),
        "PHOTOPIPE_TEST_RAW does not exist: {raw_path}"
    );

    let cfg = pipeline::config::DevelopConfig {
        rawtherapee_path: exe,
        ..Default::default()
    };
    let renderer = Pp3Renderer::new(&cfg);
    renderer
        .probe()
        .expect("probe should succeed against a real rawtherapee-cli");

    let recipe = EditRecipe {
        exposure_ev: 0.3,
        highlight_recovery: 0.2,
        shadow_lift: 0.1,
        denoise_luma: 0.1,
        denoise_chroma: 0.12,
        sharpen_amount: 0.4,
        lens_correct: false,
    };

    // Snapshot the source directory's listing so we can prove nothing new
    // landed beside the original RAW — RawTherapee's own convention is to
    // write `photo.raw.pp3` next to the source, which our non-destructive
    // contract forbids.
    let source_dir = raw.parent().expect("raw path must have a parent dir");
    let before: std::collections::BTreeSet<_> = std::fs::read_dir(source_dir)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();

    let tmp = tempfile::TempDir::new().unwrap();
    let rendered = renderer
        .render(&raw, &recipe, tmp.path())
        .expect("render should succeed");

    assert!(rendered.tiff.exists(), "TIFF was not created");
    let tiff_len = std::fs::metadata(&rendered.tiff).unwrap().len();
    eprintln!(
        "rendered TIFF: {} ({tiff_len} bytes)",
        rendered.tiff.display()
    );
    assert!(
        tiff_len > 100_000_000,
        "expected a 16-bit TIFF of a 24MP+ frame to exceed 100MB, got {tiff_len} bytes"
    );

    assert!(rendered.pp3.exists(), ".pp3 was not created");

    let after: std::collections::BTreeSet<_> = std::fs::read_dir(source_dir)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(
        before, after,
        "rendering must never write anything beside the original RAW"
    );

    // Render the SAME raw a second time into the SAME parent tmp_dir — the
    // exact shape of the stem-collision hazard the fix guards against: the
    // orchestrator hands one shared temp directory to every call, and both
    // renders here derive their output names from the identical filename
    // stem. If render() wrote directly into tmp_dir rather than a private
    // scratch subdirectory, the second render would silently overwrite the
    // first's TIFF and profile. Asserting the two paths differ, and that both
    // existed at once, proves the scratch-directory fix is load-bearing.
    let rendered2 = renderer
        .render(&raw, &recipe, tmp.path())
        .expect("second render should succeed");
    assert_ne!(
        rendered.tiff, rendered2.tiff,
        "two renders of the same stem into the same tmp_dir must not collide"
    );
    assert!(
        rendered.tiff.exists(),
        "first render's TIFF must still exist after the second render completes"
    );
    assert!(rendered2.tiff.exists(), "second render's TIFF must exist");
}
