use pipeline::catalog::{Catalog, EditIdentity, EditRow};
use pipeline::config::{DefectConfig, DevelopConfig};
use pipeline::develop::decide::{EditRecipe, DECIDER_VERSION};
use pipeline::develop::is_up_to_date;
use pipeline::develop::measure::RawStats;
use pipeline::develop::render::Pp3Renderer;
use pipeline::develop::{finish_folder, FinishReport};
use pipeline::ProgressSink;

fn temp_catalog() -> (tempfile::TempDir, Catalog) {
    let dir = tempfile::TempDir::new().unwrap();
    let cat = Catalog::open(&dir.path().join("catalog.duckdb")).unwrap();
    (dir, cat)
}

/// Serializes the two tests in this file that drive a real `rawtherapee-cli`
/// against a real RAW file. `end_to_end_finish_is_idempotent` temporarily
/// redirects the process-wide temp-dir env var to observe its own cleanup;
/// without this lock, `real_renderer_produces_a_tiff_and_touches_nothing_beside_the_source`
/// running concurrently would have its own temp directories swept into that
/// redirected root and misread as leftovers.
static REAL_RENDER_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn migration_creates_develop_tables_at_current_version() {
    let (_dir, cat) = temp_catalog();
    assert_eq!(cat.schema_version().unwrap(), 5);
}

/// Reproduces the real bug: a catalog created while migration v4 had its
/// *original* shape (raw_stats without `p99`, edits with `wb_temp_k` /
/// `wb_green`) and `schema_version` already at 4. Because `Catalog::open`
/// only ever applies migrations whose version exceeds the recorded one,
/// such a catalog would be stuck forever without migration v5.
///
/// This test builds exactly that on-disk shape, then reopens the same file
/// through `Catalog::open` (the real code path every `photopipe` invocation
/// takes) and asserts the v5 migration ran and brought both tables to their
/// intended final shape.
#[test]
fn migration_v5_repairs_catalogs_stuck_at_original_v4_shape() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("catalog.duckdb");

    // Open once so every table up to the *current* shipped schema exists,
    // then downgrade raw_stats/edits in place to the original (pre-amendment)
    // v4 DDL and roll schema_version back to 4 — simulating a catalog created
    // before the v4 amendments landed.
    {
        let cat = Catalog::open(&path).unwrap();
        let conn = cat.raw_conn_for_test();
        conn.execute_batch(
            "DROP TABLE raw_stats;
             DROP TABLE edits;
             CREATE TABLE raw_stats (
                 file_id           BIGINT PRIMARY KEY REFERENCES files(id),
                 p1                REAL NOT NULL,
                 p50               REAL NOT NULL,
                 p999              REAL NOT NULL,
                 clipped_frac      REAL NOT NULL,
                 black_frac        REAL NOT NULL,
                 wb_r              REAL NOT NULL,
                 wb_g              REAL NOT NULL,
                 wb_b              REAL NOT NULL,
                 illum_r           REAL,
                 illum_g           REAL,
                 illum_b           REAL
             );
             CREATE TABLE edits (
                 file_id            BIGINT PRIMARY KEY REFERENCES files(id),
                 content_hash       VARCHAR NOT NULL,
                 exposure_ev        REAL NOT NULL,
                 wb_temp_k          REAL NOT NULL,
                 wb_green           REAL NOT NULL,
                 highlight_recovery REAL NOT NULL,
                 shadow_lift        REAL NOT NULL,
                 denoise_luma       REAL NOT NULL,
                 denoise_chroma     REAL NOT NULL,
                 sharpen_amount     REAL NOT NULL,
                 lens_correct       BOOLEAN NOT NULL,
                 recipe_hash        VARCHAR NOT NULL,
                 decider_version    VARCHAR NOT NULL,
                 renderer           VARCHAR NOT NULL,
                 look_model         VARCHAR,
                 look_version       VARCHAR,
                 lut_hash           VARCHAR,
                 look_applied       BOOLEAN NOT NULL,
                 iqa_before         REAL,
                 iqa_after          REAL,
                 output_path        VARCHAR,
                 output_size_bytes  BIGINT,
                 rendered_at        BIGINT
             );
             CREATE INDEX idx_edits_hash ON edits(content_hash);
             DELETE FROM schema_version;
             INSERT INTO schema_version VALUES (4);",
        )
        .unwrap();
        drop(conn);
        // Drop the Catalog to release the file so it can be reopened below.
        drop(cat);
    }

    // Reopen through the real entry point. This must apply migration v5.
    let cat = Catalog::open(&path).unwrap();
    assert_eq!(cat.schema_version().unwrap(), 5);

    let conn = cat.raw_conn_for_test();
    let raw_stats_columns: Vec<String> = conn
        .prepare(
            "SELECT column_name FROM information_schema.columns
             WHERE table_name = 'raw_stats' ORDER BY column_name",
        )
        .unwrap()
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    assert!(
        raw_stats_columns.iter().any(|c| c == "p99"),
        "raw_stats should have a p99 column after migrating, got {raw_stats_columns:?}"
    );

    let edits_columns: Vec<String> = conn
        .prepare(
            "SELECT column_name FROM information_schema.columns
             WHERE table_name = 'edits' ORDER BY column_name",
        )
        .unwrap()
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    assert!(
        !edits_columns
            .iter()
            .any(|c| c == "wb_temp_k" || c == "wb_green"),
        "edits should no longer have wb_temp_k/wb_green after migrating, got {edits_columns:?}"
    );
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

/// Like `seed_file`, but with an explicit `file_format` — used to seed a
/// non-RAW keeper (e.g. `jpg`), which `keepers_to_develop()` does not filter
/// out.
fn seed_file_with_format(cat: &Catalog, path: &str, verdict: &str, file_format: &str) -> i64 {
    let conn = cat.raw_conn_for_test();
    conn.execute(
        "INSERT INTO files (path, content_hash, size_bytes, mtime_ns, file_format, last_processed)
         VALUES (?, 'hash-a', 100, 0, ?, 0)",
        duckdb::params![path, file_format],
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

    let _guard = REAL_RENDER_LOCK.lock().unwrap_or_else(|e| e.into_inner());

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

fn sample_recipe() -> EditRecipe {
    EditRecipe {
        exposure_ev: 0.5,
        highlight_recovery: 0.2,
        shadow_lift: 0.1,
        denoise_luma: 0.0,
        denoise_chroma: 0.0,
        sharpen_amount: 0.4,
        lens_correct: true,
    }
}

fn sample_edit(file_id: i64, out: &std::path::Path, size: i64) -> EditRow {
    let recipe = sample_recipe();
    EditRow {
        file_id,
        content_hash: "hash-a".into(),
        recipe_hash: recipe.recipe_hash(),
        recipe,
        decider_version: DECIDER_VERSION.into(),
        renderer: "rawtherapee".into(),
        look_model: None,
        look_version: None,
        lut_hash: None,
        look_applied: false,
        iqa_before: None,
        iqa_after: None,
        output_path: Some(out.display().to_string()),
        output_size_bytes: Some(size),
        rendered_at: 1_700_000_000,
    }
}

#[test]
fn edit_round_trips() {
    let (_dir, cat) = temp_catalog();
    let id = seed_file(&cat, "/tmp/a.arw", "keep");
    let out = std::path::PathBuf::from("/tmp/out/a.jpg");
    cat.upsert_edit(&sample_edit(id, &out, 4096)).unwrap();
    let (identity, path, size) = cat.edit_identity(id).unwrap().expect("row should exist");
    assert_eq!(identity.content_hash, "hash-a");
    assert_eq!(identity.decider_version, DECIDER_VERSION);
    assert_eq!(identity.renderer, "rawtherapee");
    assert_eq!(identity.look_model, None);
    assert_eq!(path.as_deref(), Some("/tmp/out/a.jpg"));
    assert_eq!(size, Some(4096));
}

#[test]
fn edit_upsert_overwrites() {
    let (_dir, cat) = temp_catalog();
    let id = seed_file(&cat, "/tmp/a.arw", "keep");
    let out = std::path::PathBuf::from("/tmp/out/a.jpg");
    cat.upsert_edit(&sample_edit(id, &out, 4096)).unwrap();
    let mut second = sample_edit(id, &out, 8192);
    second.look_applied = true;
    cat.upsert_edit(&second).unwrap();
    let (_, _, size) = cat.edit_identity(id).unwrap().unwrap();
    assert_eq!(size, Some(8192));
}

#[test]
fn no_edit_row_for_an_unrendered_file() {
    let (_dir, cat) = temp_catalog();
    let id = seed_file(&cat, "/tmp/a.arw", "keep");
    assert!(cat.edit_identity(id).unwrap().is_none());
}

/// Every nullable field of `EditRow` gets a distinct, recognisable value so a
/// positional transposition between same-typed neighbours (e.g. `iqa_before`
/// vs `iqa_after`, `look_model` vs `look_version`) is caught. The brief's
/// `edit_round_trips` test only checks a few fields; this checks all of them,
/// individually, against `edit_identity`'s output AND a direct SQL read of
/// the row's remaining columns (look_applied, lut_hash, output_size_bytes).
#[test]
fn edit_round_trips_field_exact_with_all_nullable_fields_populated() {
    let (_dir, cat) = temp_catalog();
    let id = seed_file(&cat, "/tmp/a.arw", "keep");
    let out = std::path::PathBuf::from("/tmp/out/a.jpg");
    let recipe = sample_recipe();
    let row = EditRow {
        file_id: id,
        content_hash: "hash-a".into(),
        recipe_hash: recipe.recipe_hash(),
        recipe,
        decider_version: DECIDER_VERSION.into(),
        renderer: "rawtherapee".into(),
        look_model: Some("lut3d-fivek".into()),
        look_version: Some("look-v3".into()),
        lut_hash: Some("lutsha-deadbeef".into()),
        look_applied: true,
        iqa_before: Some(0.31),
        iqa_after: Some(0.87),
        output_path: Some(out.display().to_string()),
        output_size_bytes: Some(123_456),
        rendered_at: 1_700_000_123,
    };
    cat.upsert_edit(&row).unwrap();

    let (identity, path, size) = cat.edit_identity(id).unwrap().expect("row should exist");
    assert_eq!(identity.content_hash, "hash-a");
    assert_eq!(identity.recipe_hash, row.recipe_hash);
    assert_eq!(identity.decider_version, DECIDER_VERSION);
    assert_eq!(identity.renderer, "rawtherapee");
    assert_eq!(identity.look_model, Some("lut3d-fivek".to_string()));
    assert_eq!(identity.look_version, Some("look-v3".to_string()));
    assert_eq!(path, Some(out.display().to_string()));
    assert_eq!(size, Some(123_456));

    // Columns not covered by `edit_identity` — read directly so
    // look_applied/lut_hash/iqa_before/iqa_after/output_size_bytes are each
    // checked individually against their own distinct value.
    let conn = cat.raw_conn_for_test();
    let (lut_hash, look_applied, iqa_before, iqa_after, output_size_bytes): (
        Option<String>,
        bool,
        Option<f32>,
        Option<f32>,
        Option<i64>,
    ) = conn
        .query_row(
            "SELECT lut_hash, look_applied, iqa_before, iqa_after, output_size_bytes
             FROM edits WHERE file_id = ?",
            duckdb::params![id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .unwrap();
    assert_eq!(lut_hash, Some("lutsha-deadbeef".to_string()));
    assert!(look_applied);
    assert_eq!(iqa_before, Some(0.31));
    assert_eq!(iqa_after, Some(0.87));
    assert_eq!(output_size_bytes, Some(123_456));
}

/// Every `Option` field must round-trip a `None` as `None`, not a zero or
/// empty string — `look_applied = false` with `look_model = NULL` is the
/// legitimate "baseline only" state that the idempotency key compares.
#[test]
fn edit_round_trips_none_for_every_optional_field() {
    let (_dir, cat) = temp_catalog();
    let id = seed_file(&cat, "/tmp/a.arw", "keep");
    let recipe = sample_recipe();
    let row = EditRow {
        file_id: id,
        content_hash: "hash-a".into(),
        recipe_hash: recipe.recipe_hash(),
        recipe,
        decider_version: DECIDER_VERSION.into(),
        renderer: "rawtherapee".into(),
        look_model: None,
        look_version: None,
        lut_hash: None,
        look_applied: false,
        iqa_before: None,
        iqa_after: None,
        output_path: None,
        output_size_bytes: None,
        rendered_at: 1_700_000_000,
    };
    cat.upsert_edit(&row).unwrap();

    let (identity, path, size) = cat.edit_identity(id).unwrap().expect("row should exist");
    assert_eq!(identity.look_model, None);
    assert_eq!(identity.look_version, None);
    assert_eq!(path, None);
    assert_eq!(size, None);

    let conn = cat.raw_conn_for_test();
    let (lut_hash, look_applied, iqa_before, iqa_after): (
        Option<String>,
        bool,
        Option<f32>,
        Option<f32>,
    ) = conn
        .query_row(
            "SELECT lut_hash, look_applied, iqa_before, iqa_after
             FROM edits WHERE file_id = ?",
            duckdb::params![id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert_eq!(lut_hash, None);
    assert!(!look_applied);
    assert_eq!(iqa_before, None);
    assert_eq!(iqa_after, None);
}

fn identity(recipe_hash: &str) -> EditIdentity {
    EditIdentity {
        content_hash: "hash-a".into(),
        recipe_hash: recipe_hash.into(),
        decider_version: DECIDER_VERSION.into(),
        renderer: "rawtherapee".into(),
        look_model: None,
        look_version: None,
    }
}

#[test]
fn identical_identity_with_present_output_is_up_to_date() {
    let dir = tempfile::TempDir::new().unwrap();
    let out = dir.path().join("a.jpg");
    std::fs::write(&out, vec![0u8; 100]).unwrap();
    assert!(is_up_to_date(
        &identity("r1"),
        &identity("r1"),
        Some(&out),
        Some(100)
    ));
}

/// Any identity component changing forces a re-render. A tuning change must
/// not leave stale JPEGs behind. Covers all six `EditIdentity` fields
/// individually (the brief's own snippet only flipped four), plus the
/// `Some -> Some` transitions for the two `Option<String>` look fields —
/// the realistic case of swapping look models or bumping a look version,
/// which a `None -> Some` transition alone cannot distinguish from a sloppy
/// "both present" comparison.
#[test]
fn each_identity_component_forces_a_rerender() {
    let dir = tempfile::TempDir::new().unwrap();
    let out = dir.path().join("a.jpg");
    std::fs::write(&out, vec![0u8; 100]).unwrap();
    let base = identity("r1");

    // Positive control: without this, every negative assertion below would
    // still pass if `is_up_to_date` simply always returned `false`.
    assert!(
        is_up_to_date(&base, &base.clone(), Some(&out), Some(100)),
        "an unmodified identity with the output present at the recorded size must be up to date"
    );

    let mut recipe_changed = base.clone();
    recipe_changed.recipe_hash = "r2".into();
    assert!(
        !is_up_to_date(&base, &recipe_changed, Some(&out), Some(100)),
        "a recipe_hash change must force a re-render"
    );

    let mut decider_changed = base.clone();
    decider_changed.decider_version = "some-other-decider".into();
    assert!(
        !is_up_to_date(&base, &decider_changed, Some(&out), Some(100)),
        "a decider_version change must force a re-render"
    );

    let mut content_changed = base.clone();
    content_changed.content_hash = "hash-b".into();
    assert!(
        !is_up_to_date(&base, &content_changed, Some(&out), Some(100)),
        "a content_hash change must force a re-render"
    );

    let mut renderer_changed = base.clone();
    renderer_changed.renderer = "vkdt".into();
    assert!(
        !is_up_to_date(&base, &renderer_changed, Some(&out), Some(100)),
        "a renderer change must force a re-render"
    );

    let mut look_model_changed = base.clone();
    look_model_changed.look_model = Some("lut3d-fivek".into());
    assert!(
        !is_up_to_date(&base, &look_model_changed, Some(&out), Some(100)),
        "a look_model change (None -> Some) must force a re-render"
    );

    let mut look_version_changed = base.clone();
    look_version_changed.look_version = Some("2".into());
    assert!(
        !is_up_to_date(&base, &look_version_changed, Some(&out), Some(100)),
        "a look_version change (None -> Some) must force a re-render"
    );

    // Some -> Some, not just None -> Some: swapping look models or bumping a
    // look version must invalidate, and both fields are Option<String> where
    // a sloppy comparison could treat "both present" as equal.
    let mut from = base.clone();
    from.look_model = Some("lut3d-fivek".into());
    from.look_version = Some("1".into());

    let mut to_other_model = from.clone();
    to_other_model.look_model = Some("lut3d-own".into());
    assert!(
        !is_up_to_date(&from, &to_other_model, Some(&out), Some(100)),
        "a different look model must force a re-render"
    );

    let mut to_other_version = from.clone();
    to_other_version.look_version = Some("2".into());
    assert!(
        !is_up_to_date(&from, &to_other_version, Some(&out), Some(100)),
        "a different look version must force a re-render"
    );
}

/// A deleted or truncated output must be rebuilt even when the identity matches.
#[test]
fn missing_or_resized_output_forces_a_rerender() {
    let dir = tempfile::TempDir::new().unwrap();
    let out = dir.path().join("a.jpg");
    assert!(!is_up_to_date(
        &identity("r1"),
        &identity("r1"),
        Some(&out),
        Some(100)
    ));

    std::fs::write(&out, vec![0u8; 50]).unwrap();
    assert!(!is_up_to_date(
        &identity("r1"),
        &identity("r1"),
        Some(&out),
        Some(100)
    ));

    // Never rendered before: no recorded path at all.
    assert!(!is_up_to_date(&identity("r1"), &identity("r1"), None, None));
}

// ── finish_folder orchestration ──

/// A sink that records what it was told, so stage order is assertable.
#[derive(Default)]
struct RecordingSink {
    stages: std::sync::Mutex<Vec<String>>,
}

impl ProgressSink for RecordingSink {
    fn stage(&self, stage: &str) {
        self.stages.lock().unwrap().push(stage.to_string());
    }
    fn set_total(&self, _total: u64) {}
    fn inc(&self) {}
}

#[test]
fn empty_work_list_renders_nothing_and_still_reports_done() {
    let (_dir, cat) = temp_catalog();
    let out = tempfile::TempDir::new().unwrap();
    let sink = RecordingSink::default();
    let report: FinishReport = finish_folder(
        &cat,
        &DevelopConfig::default(),
        &DefectConfig::default(),
        out.path(),
        false,
        &sink,
    )
    .unwrap();
    assert_eq!(report.rendered, 0);
    assert_eq!(report.errored, 0);
    let stages = sink.stages.lock().unwrap().clone();
    assert_eq!(stages.last().map(String::as_str), Some("done"));
}

/// A missing renderer must fail the run up front with one clear error, not
/// produce one warning per photo deep into a long run.
#[test]
fn missing_renderer_fails_before_any_work() {
    let (_dir, cat) = temp_catalog();
    seed_file(&cat, "/tmp/a.arw", "keep");
    let out = tempfile::TempDir::new().unwrap();
    let cfg = DevelopConfig {
        rawtherapee_path: "/nonexistent/rawtherapee-cli".into(),
        ..Default::default()
    };
    let err = finish_folder(
        &cat,
        &cfg,
        &DefectConfig::default(),
        out.path(),
        false,
        &RecordingSink::default(),
    )
    .expect_err("a missing renderer should abort the run");
    assert!(
        err.to_string().contains("rawtherapee"),
        "error should name the missing dependency: {err}"
    );
}

/// Builds a stand-in `rawtherapee-cli` for tests that need `Pp3Renderer::probe()`
/// to succeed but do not need a real render. `probe()` authenticates on the
/// `RawTherapee` version banner (see Task 9), so a plain stub like `/usr/bin/true`
/// cannot get past it — this script prints one.
///
/// For anything other than `--version` it parses out `-o <dir>` and `-c <input>`
/// and drops a few-byte stub at `<dir>/<input-stem>.tif`. That stub is enough to
/// satisfy `Pp3Renderer::render`'s own "did the file appear" check, but it is
/// deliberately NOT a decodable TIFF — a real render is what Task 12's gated
/// end-to-end test is for.
///
/// Returns the owning `TempDir` (keep it alive for as long as the script path is
/// used) and the script path itself.
#[cfg(unix)]
fn fake_rawtherapee() -> (tempfile::TempDir, std::path::PathBuf) {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::TempDir::new().unwrap();
    let script = dir.path().join("rawtherapee-cli");
    std::fs::write(
        &script,
        r#"#!/bin/sh
if [ "$1" = "--version" ]; then
    echo "RawTherapee, version 5.13 (fake)"
    exit 0
fi

outdir=""
input=""
while [ $# -gt 0 ]; do
    case "$1" in
        -o)
            outdir="$2"
            shift 2
            ;;
        -c)
            input="$2"
            shift 2
            ;;
        *)
            shift
            ;;
    esac
done

stem=$(basename "$input")
stem="${stem%.*}"
printf 'fake tiff\n' > "$outdir/$stem.tif"
exit 0
"#,
    )
    .unwrap();
    let mut perms = std::fs::metadata(&script).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&script, perms).unwrap();
    (dir, script)
}

/// One unreadable raw must not abort the run, and must leave no edits row —
/// a half-recorded render would make the next run believe it succeeded.
///
/// Uses the fake `rawtherapee-cli` above so this runs on a bare machine: the
/// only thing that needs a real RawTherapee elsewhere is `probe()`'s version
/// banner check, which the fake satisfies. The failure this test is actually
/// about happens earlier, in `measure_raw` on a nonexistent RAW, so the fake
/// renderer is never even reached.
#[cfg(unix)]
#[test]
fn unreadable_raw_is_skipped_without_an_edits_row() {
    let (_rt_dir, rt) = fake_rawtherapee();
    let (_dir, cat) = temp_catalog();
    let id = seed_file(&cat, "/tmp/definitely-missing.arw", "keep");
    let out = tempfile::TempDir::new().unwrap();
    let cfg = DevelopConfig {
        rawtherapee_path: rt.to_string_lossy().into_owned(),
        ..Default::default()
    };
    let report = finish_folder(
        &cat,
        &cfg,
        &DefectConfig::default(),
        out.path(),
        false,
        &RecordingSink::default(),
    )
    .unwrap();
    assert_eq!(report.errored, 1);
    assert_eq!(report.rendered, 0);
    assert!(
        cat.edit_identity(id).unwrap().is_none(),
        "no row on failure"
    );
}

/// A plain JPEG keeper must be counted as `skipped_unsupported`, not
/// `errored`. Before this fix, `finish_one` called `measure_raw`
/// unconditionally and a JPEG produced one useless `Decode` error per file
/// instead of a clean skip.
#[cfg(unix)]
#[test]
fn jpg_keeper_is_counted_as_skipped_unsupported_not_errored() {
    let (_rt_dir, rt) = fake_rawtherapee();
    let (_dir, cat) = temp_catalog();
    seed_file_with_format(&cat, "/tmp/a.jpg", "keep", "jpg");
    let out = tempfile::TempDir::new().unwrap();
    let cfg = DevelopConfig {
        rawtherapee_path: rt.to_string_lossy().into_owned(),
        ..Default::default()
    };
    let report = finish_folder(
        &cat,
        &cfg,
        &DefectConfig::default(),
        out.path(),
        false,
        &RecordingSink::default(),
    )
    .unwrap();
    assert_eq!(report.skipped_unsupported, 1);
    assert_eq!(report.errored, 0);
    assert_eq!(report.rendered, 0);
}

/// Exercises the fake renderer against a real, decodable RAW so `measure_raw`
/// and `decide` genuinely run. The fixture is local, gitignored sample data
/// (`example-pictures/` is never checked into git — see `.gitignore`), never a
/// fabricated one; this test skips cleanly when it is absent, e.g. on a fresh
/// clone or in CI. The fake's stub `.tif` is not a decodable image, so the
/// JPEG-encode step fails after the (fake) render — a real happy-path render
/// can only be produced by real RawTherapee. That means this test cannot
/// demonstrate "second run renders nothing" the way the gated end-to-end test
/// can: with no successful render, there is no recorded edit for a second run
/// to find as up to date, so a second run repeats the same work rather than
/// skipping it. What this test does prove: the renderer ran (via the fake),
/// the failure isolation extends past the render call into encoding, and no
/// half-recorded `edits` row is left behind — the same invariant
/// `unreadable_raw_is_skipped_without_an_edits_row` checks, but with the
/// renderer actually invoked rather than never reached.
#[cfg(unix)]
#[test]
fn fake_renderer_runs_but_stub_output_cannot_complete_the_happy_path() {
    let raw = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../example-pictures/DSC03073.ARW");
    if !raw.exists() {
        eprintln!(
            "skipping: {} not present (example-pictures/ is gitignored local sample data)",
            raw.display()
        );
        return;
    }

    let (_rt_dir, rt) = fake_rawtherapee();
    let (_dir, cat) = temp_catalog();
    let id = seed_file(&cat, raw.to_str().unwrap(), "keep");
    let out = tempfile::TempDir::new().unwrap();
    let cfg = DevelopConfig {
        rawtherapee_path: rt.to_string_lossy().into_owned(),
        ..Default::default()
    };

    let report = finish_folder(
        &cat,
        &cfg,
        &DefectConfig::default(),
        out.path(),
        false,
        &RecordingSink::default(),
    )
    .unwrap();
    assert_eq!(
        report.errored, 1,
        "the stub .tif is not a decodable image, so encode must fail"
    );
    assert_eq!(report.rendered, 0);
    assert!(
        cat.edit_identity(id).unwrap().is_none(),
        "a failed encode must leave no edits row"
    );
}

// ── real end-to-end: render, idempotency, and non-destruction together ──

/// Restores an environment variable to its previous value (or removes it) on
/// drop, so redirecting `TMPDIR` for the duration of a single test cannot leak
/// into any test that runs after it.
struct EnvGuard {
    key: &'static str,
    previous: Option<std::ffi::OsString>,
}

impl EnvGuard {
    fn set(key: &'static str, value: &std::path::Path) -> Self {
        let previous = std::env::var_os(key);
        std::env::set_var(key, value);
        Self { key, previous }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match &self.previous {
            Some(v) => std::env::set_var(self.key, v),
            None => std::env::remove_var(self.key),
        }
    }
}

/// The only test in this suite that proves a real render and idempotency
/// together, against a real RAW file through a real `rawtherapee-cli`. Every
/// other test in this file uses either no renderer or the fake stub from
/// `fake_rawtherapee`, which cannot produce a decodable JPEG.
///
/// Gated on two environment variables so `cargo test --all` stays green on a
/// bare checkout: skips with a message (never fails, never `#[ignore]`) when
/// either is absent.
#[test]
fn end_to_end_finish_is_idempotent() {
    let Some(rt) = std::env::var_os("PHOTOPIPE_TEST_RAWTHERAPEE") else {
        eprintln!("skipping: set PHOTOPIPE_TEST_RAWTHERAPEE to the rawtherapee-cli path");
        return;
    };
    let Some(raw) = std::env::var_os("PHOTOPIPE_TEST_RAW") else {
        eprintln!("skipping: set PHOTOPIPE_TEST_RAW to a real RAW file");
        return;
    };
    let raw = std::path::PathBuf::from(raw);
    assert!(
        raw.exists(),
        "PHOTOPIPE_TEST_RAW points at a file that does not exist: {}",
        raw.display()
    );

    let _guard = REAL_RENDER_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    // Snapshot the source directory: nothing may be written beside the
    // original, ever — not even a sidecar. `keepers_to_develop` never touches
    // this directory itself, so any change here means the renderer strayed.
    let source_dir = raw.parent().unwrap().to_path_buf();
    let source_entries_before = std::fs::read_dir(&source_dir).unwrap().count();

    // Everything the test itself owns for the whole run — the catalog and the
    // output tree — is created against the *real* system temp dir, before
    // TMPDIR is redirected below. Only `finish_folder`'s own internal
    // scratch directories should ever land inside the redirected root.
    let (_dir, cat) = temp_catalog();
    let id = {
        let conn = cat.raw_conn_for_test();
        conn.execute(
            "INSERT INTO files (path, content_hash, size_bytes, mtime_ns, file_format, last_processed)
             VALUES (?, 'e2e-hash', 0, 0, 'arw', 0)",
            duckdb::params![raw.to_string_lossy()],
        )
        .unwrap();
        let id: i64 = conn
            .query_row(
                "SELECT id FROM files WHERE content_hash = 'e2e-hash'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        conn.execute(
            "INSERT INTO decisions (file_id, verdict, is_keeper, note, decided_at)
             VALUES (?, 'keep', false, NULL, 0)",
            duckdb::params![id],
        )
        .unwrap();
        id
    };
    let out = tempfile::TempDir::new().unwrap();
    let cfg = DevelopConfig {
        rawtherapee_path: rt.to_string_lossy().into_owned(),
        ..Default::default()
    };

    // Redirect the system temp dir into a scratch directory we control, so we
    // can assert nothing survives finish_folder's own cleanup. Restored on
    // drop regardless of how the test exits.
    let tmp_root = tempfile::TempDir::new().unwrap();
    #[cfg(unix)]
    let _tmpdir_guard = EnvGuard::set("TMPDIR", tmp_root.path());
    #[cfg(windows)]
    let _tmp_guard = EnvGuard::set("TMP", tmp_root.path());
    #[cfg(windows)]
    let _temp_guard = EnvGuard::set("TEMP", tmp_root.path());

    // ── first run: a real render ──
    let first = finish_folder(
        &cat,
        &cfg,
        &DefectConfig::default(),
        out.path(),
        false,
        &RecordingSink::default(),
    )
    .unwrap();
    assert_eq!(first.rendered, 1, "first run should render");
    assert_eq!(first.skipped, 0);
    assert_eq!(first.errored, 0, "first run should not error");

    let (_, path, _) = cat.edit_identity(id).unwrap().unwrap();
    let jpeg = std::path::PathBuf::from(path.unwrap());
    assert!(jpeg.exists(), "the JPEG should exist at {}", jpeg.display());

    // Decodable, and a plausible size — not merely "the file is non-empty".
    let decoded = image::open(&jpeg)
        .unwrap_or_else(|e| panic!("output JPEG at {} is not decodable: {e}", jpeg.display()));
    let (w, h) = (decoded.width(), decoded.height());
    assert!(
        w > 200 && h > 200,
        "output JPEG dimensions look implausible for a real photo: {w}x{h}"
    );

    // The .pp3 escape hatch sits beside the JPEG.
    let pp3 = jpeg.with_extension("pp3");
    assert!(pp3.exists(), "the .pp3 escape hatch should sit beside it");

    // Non-destructive contract: nothing was written beside the source RAW.
    // RawTherapee's own convention (`photo.raw.pp3`) violates this, which is
    // exactly why this needs an explicit assertion rather than trusting intent.
    assert!(
        !raw.with_extension("pp3").exists(),
        "nothing may be written beside the original"
    );
    let source_entries_after_first = std::fs::read_dir(&source_dir).unwrap().count();
    assert_eq!(
        source_entries_before, source_entries_after_first,
        "the source directory's file count must not change"
    );

    // Output landed in a YYYY-MM subdirectory (or unknown-date, if the RAW
    // carries no captured_at) under `out`, per the default `output_subdirs =
    // "month"`.
    let rel = jpeg.strip_prefix(out.path()).unwrap();
    let subdir = rel
        .components()
        .next()
        .and_then(|c| c.as_os_str().to_str())
        .unwrap();
    assert!(
        subdir == "unknown-date" || (subdir.len() == 7 && subdir.as_bytes()[4] == b'-'),
        "expected a YYYY-MM or unknown-date subdirectory, got {subdir}"
    );

    // ── second run: idempotency is a correctness requirement, not a perf goal ──
    let second = finish_folder(
        &cat,
        &cfg,
        &DefectConfig::default(),
        out.path(),
        false,
        &RecordingSink::default(),
    )
    .unwrap();
    assert_eq!(second.rendered, 0, "second run must render nothing");
    assert_eq!(second.skipped, 1);
    assert_eq!(second.errored, 0);

    // ── third run: corrupt the recorded state the way a real change would,
    // and confirm the idempotency check actually notices. Without this, the
    // second-run assertion above could pass simply because the code always
    // skips regardless of whether the output still matches. ──
    std::fs::File::create(&jpeg).unwrap(); // truncate to 0 bytes
    let third = finish_folder(
        &cat,
        &cfg,
        &DefectConfig::default(),
        out.path(),
        false,
        &RecordingSink::default(),
    )
    .unwrap();
    assert_eq!(
        third.rendered, 1,
        "a truncated output must be detected as stale and re-rendered"
    );
    assert_eq!(third.skipped, 0);
    assert_eq!(third.errored, 0);
    assert!(
        image::open(&jpeg).is_ok(),
        "the re-rendered JPEG should be decodable again"
    );

    // Final non-destruction check, after three runs' worth of opportunity to
    // slip.
    let source_entries_final = std::fs::read_dir(&source_dir).unwrap().count();
    assert_eq!(
        source_entries_before, source_entries_final,
        "the source directory's file count must still be unchanged after 3 runs"
    );

    // No temp files left behind: the run-level temp directory finish_folder
    // creates, and every per-render scratch directory inside it, are each
    // owned by a `tempfile::TempDir` that is dropped before finish_folder
    // returns. With TMPDIR redirected into `tmp_root`, that means nothing
    // should remain in it once all three calls above have returned.
    let leftovers: Vec<_> = std::fs::read_dir(tmp_root.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .collect();
    assert!(
        leftovers.is_empty(),
        "temp scratch directories were left behind: {leftovers:?}"
    );
}

// ── KI-2: the finished tree is pruned ─────────────────────────────────────────

/// Write an `edits` row that claims `dest`, plus the JPEG and `.pp3` on disk,
/// as an earlier successful run would have left them.
fn seed_rendered_output(cat: &Catalog, file_id: i64, dest: &std::path::Path) {
    std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
    std::fs::write(dest, b"pretend jpeg").unwrap();
    std::fs::write(dest.with_extension("pp3"), b"pretend pp3").unwrap();
    let size = std::fs::metadata(dest).unwrap().len() as i64;
    cat.upsert_edit(&EditRow {
        file_id,
        content_hash: "hash-a".into(),
        recipe: EditRecipe {
            exposure_ev: 0.0,
            highlight_recovery: 0.0,
            shadow_lift: 0.0,
            denoise_luma: 0.0,
            denoise_chroma: 0.0,
            sharpen_amount: 0.4,
            lens_correct: false,
        },
        recipe_hash: "recipe-1".into(),
        decider_version: DECIDER_VERSION.into(),
        renderer: "rawtherapee".into(),
        look_model: None,
        look_version: None,
        lut_hash: None,
        look_applied: false,
        iqa_before: None,
        iqa_after: None,
        output_path: Some(dest.display().to_string()),
        output_size_bytes: Some(size),
        rendered_at: 0,
    })
    .unwrap();
}

/// KI-2: flipping a verdict from keep to reject used to leave the JPEG, the
/// `.pp3` and the `edits` row in place forever, so `_finished/` only grew.
///
/// The renderer is never reached — with no keepers left there is nothing to
/// render — so this needs no RawTherapee.
#[cfg(unix)]
#[test]
fn rejecting_a_keeper_prunes_its_output_and_its_edits_row() {
    let (_rt_dir, rt) = fake_rawtherapee();
    let (_dir, cat) = temp_catalog();
    let id = seed_file(&cat, "/tmp/rejected.arw", "reject");
    let out = tempfile::TempDir::new().unwrap();
    let dest = out.path().join("2024-05/DSC1.jpg");
    seed_rendered_output(&cat, id, &dest);

    let cfg = DevelopConfig {
        rawtherapee_path: rt.to_string_lossy().into_owned(),
        ..Default::default()
    };
    let report = finish_folder(
        &cat,
        &cfg,
        &DefectConfig::default(),
        out.path(),
        false,
        &RecordingSink::default(),
    )
    .unwrap();

    assert_eq!(report.rendered, 0, "nothing to render");
    assert_eq!(report.pruned, 2, "the JPEG and its .pp3 should both go");
    assert!(!dest.exists(), "stale JPEG survived the prune");
    assert!(!dest.with_extension("pp3").exists(), "stale .pp3 survived");
    assert!(
        cat.edit_identity(id).unwrap().is_none(),
        "the edits row must not outlive the file it names"
    );
    // The now-empty capture-month directory goes too.
    assert!(
        !out.path().join("2024-05").exists(),
        "empty dir left behind"
    );
}

/// A keeper's output must survive the prune pass — the obvious way to get the
/// test above passing is to delete everything, and this is what stops that.
#[cfg(unix)]
#[test]
fn a_still_kept_photos_output_is_not_pruned() {
    let (_rt_dir, rt) = fake_rawtherapee();
    let (_dir, cat) = temp_catalog();
    let keep_id = seed_file(&cat, "/tmp/kept.arw", "keep");
    let reject_id = seed_file(&cat, "/tmp/gone.arw", "reject");
    let out = tempfile::TempDir::new().unwrap();

    // The kept photo's recorded output must match where `finish` would put it,
    // or it is pruned as unexpected and re-rendered instead.
    let kept_dest = out.path().join("unknown-date/kept.jpg");
    seed_rendered_output(&cat, keep_id, &kept_dest);
    let gone_dest = out.path().join("2024-05/gone.jpg");
    seed_rendered_output(&cat, reject_id, &gone_dest);

    let cfg = DevelopConfig {
        rawtherapee_path: rt.to_string_lossy().into_owned(),
        ..Default::default()
    };
    let report = finish_folder(
        &cat,
        &cfg,
        &DefectConfig::default(),
        out.path(),
        false,
        &RecordingSink::default(),
    )
    .unwrap();

    assert!(!gone_dest.exists(), "the rejected photo's JPEG should go");
    assert!(
        cat.edit_identity(reject_id).unwrap().is_none(),
        "the rejected photo's row should go"
    );
    assert!(
        cat.edit_identity(keep_id).unwrap().is_some(),
        "the kept photo's row must survive, report was {report:?}"
    );
}

/// Refuse to prune a directory photopipe cannot show it wrote. `--out` can be
/// pointed anywhere, and deleting a stranger's files would be unforgivable.
#[cfg(unix)]
#[test]
fn an_unmarked_directory_full_of_strangers_is_never_pruned() {
    let (_rt_dir, rt) = fake_rawtherapee();
    let (_dir, cat) = temp_catalog();
    seed_file(&cat, "/tmp/whatever.arw", "reject");
    let out = tempfile::TempDir::new().unwrap();
    let precious = out.path().join("holiday.jpg");
    std::fs::write(&precious, b"someone's actual photo").unwrap();

    let cfg = DevelopConfig {
        rawtherapee_path: rt.to_string_lossy().into_owned(),
        ..Default::default()
    };
    let report = finish_folder(
        &cat,
        &cfg,
        &DefectConfig::default(),
        out.path(),
        false,
        &RecordingSink::default(),
    )
    .unwrap();

    assert_eq!(report.pruned, 0);
    assert!(
        precious.exists(),
        "a file photopipe never wrote was deleted"
    );
    assert!(
        !out.path().join(".photopipe-tree").exists(),
        "an unowned directory must not be claimed as managed"
    );
}

// ── KI-5: a zero-work run does not decode ─────────────────────────────────────

/// KI-5: `finish_one` used to measure — a full raw decode — before consulting
/// the idempotency check, so an unchanged library paid a decode per photo only
/// to skip it.
///
/// The RAW here does not exist. Under the old order that is an immediate
/// `measure_raw` failure and `errored == 1`; if the persisted `raw_stats` are
/// reused, the file is never opened and the photo skips cleanly. So this asserts
/// the absence of a decode by making a decode impossible.
#[cfg(unix)]
#[test]
fn an_up_to_date_photo_is_skipped_without_decoding_its_raw() {
    use pipeline::develop::decide::{decide, Sharpness, NEUTRAL_RELATIVE_SHARPNESS};
    use pipeline::ingest::exif::ExifData;

    let (_rt_dir, rt) = fake_rawtherapee();
    let (_dir, cat) = temp_catalog();
    let id = seed_file(&cat, "/tmp/definitely-not-here.arw", "keep");
    let out = tempfile::TempDir::new().unwrap();

    // Persist stats as a previous successful run would have.
    let stats = sample_stats();
    cat.upsert_raw_stats(id, &stats).unwrap();

    // The recorded identity has to be the one this run will ask for, or the
    // photo is stale and re-renders. With no `sharpness` row and no baseline,
    // `resolve_relative_sharpness` yields the neutral value.
    let recipe = decide(
        &stats,
        &ExifData::default(),
        &Sharpness {
            s_relative: NEUTRAL_RELATIVE_SHARPNESS,
        },
    );
    let dest = out.path().join("unknown-date/definitely-not-here.jpg");
    std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
    std::fs::write(&dest, b"pretend jpeg").unwrap();
    let size = std::fs::metadata(&dest).unwrap().len() as i64;
    cat.upsert_edit(&EditRow {
        file_id: id,
        content_hash: "hash-a".into(),
        recipe_hash: recipe.recipe_hash(),
        recipe,
        decider_version: DECIDER_VERSION.into(),
        renderer: "rawtherapee".into(),
        look_model: None,
        look_version: None,
        lut_hash: None,
        look_applied: false,
        iqa_before: None,
        iqa_after: None,
        output_path: Some(dest.display().to_string()),
        output_size_bytes: Some(size),
        rendered_at: 0,
    })
    .unwrap();

    let cfg = DevelopConfig {
        rawtherapee_path: rt.to_string_lossy().into_owned(),
        ..Default::default()
    };
    let report = finish_folder(
        &cat,
        &cfg,
        &DefectConfig::default(),
        out.path(),
        false,
        &RecordingSink::default(),
    )
    .unwrap();

    assert_eq!(
        report.errored, 0,
        "the raw was opened: only a decode attempt can fail on a missing file"
    );
    assert_eq!(report.skipped, 1, "should skip on the recorded identity");
    assert_eq!(report.rendered, 0);
    assert!(dest.exists(), "the output must survive its own prune pass");
}

/// The complement: when the file's content hash has moved on, the cached stats
/// must NOT be trusted, because they describe the old bytes.
#[cfg(unix)]
#[test]
fn changed_content_forces_a_fresh_measurement() {
    let (_rt_dir, rt) = fake_rawtherapee();
    let (_dir, cat) = temp_catalog();
    let id = seed_file(&cat, "/tmp/also-not-here.arw", "keep");
    let out = tempfile::TempDir::new().unwrap();
    cat.upsert_raw_stats(id, &sample_stats()).unwrap();
    // An edits row recorded against *different* bytes than files.content_hash.
    seed_rendered_output(&cat, id, &out.path().join("unknown-date/x.jpg"));
    {
        let conn = cat.raw_conn_for_test();
        conn.execute(
            "UPDATE edits SET content_hash = 'stale-hash' WHERE file_id = ?",
            duckdb::params![id],
        )
        .unwrap();
    }

    let cfg = DevelopConfig {
        rawtherapee_path: rt.to_string_lossy().into_owned(),
        ..Default::default()
    };
    let report = finish_folder(
        &cat,
        &cfg,
        &DefectConfig::default(),
        out.path(),
        false,
        &RecordingSink::default(),
    )
    .unwrap();

    assert_eq!(
        report.errored, 1,
        "stale stats must not be reused; the missing raw should be measured and fail"
    );
    assert_eq!(report.skipped, 0);
}

/// A photo recorded as finished *somewhere else* is not current here. Without
/// this, `finish --out somewhere-new` reported every photo as already current
/// and left the new directory empty, because the idempotency check only ever
/// looked at the path the previous run recorded.
#[cfg(unix)]
#[test]
fn changing_the_output_directory_forces_a_rerender() {
    let (_rt_dir, rt) = fake_rawtherapee();
    let (_dir, cat) = temp_catalog();
    let id = seed_file(&cat, "/tmp/missing-on-purpose.arw", "keep");
    let old_out = tempfile::TempDir::new().unwrap();
    seed_rendered_output(&cat, id, &old_out.path().join("unknown-date/x.jpg"));

    // A different destination. The raw does not exist, so a re-render attempt
    // fails at `measure_raw` — which is precisely the observable proving the
    // photo was *not* treated as already current.
    let new_out = tempfile::TempDir::new().unwrap();
    let cfg = DevelopConfig {
        rawtherapee_path: rt.to_string_lossy().into_owned(),
        ..Default::default()
    };
    let report = finish_folder(
        &cat,
        &cfg,
        &DefectConfig::default(),
        new_out.path(),
        false,
        &RecordingSink::default(),
    )
    .unwrap();

    assert_eq!(report.skipped, 0, "must not claim to be already current");
    assert_eq!(report.errored, 1, "it should have tried to render again");
}
