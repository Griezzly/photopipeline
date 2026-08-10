use pipeline::catalog::{Catalog, EditIdentity, EditRow};
use pipeline::config::DevelopConfig;
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
    decider_changed.decider_version = "decide-2".into();
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
    let report: FinishReport =
        finish_folder(&cat, &DevelopConfig::default(), out.path(), &sink).unwrap();
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
    let err = finish_folder(&cat, &cfg, out.path(), &RecordingSink::default())
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
    let report = finish_folder(&cat, &cfg, out.path(), &RecordingSink::default()).unwrap();
    assert_eq!(report.errored, 1);
    assert_eq!(report.rendered, 0);
    assert!(
        cat.edit_identity(id).unwrap().is_none(),
        "no row on failure"
    );
}

/// Exercises the fake renderer against a real, decodable RAW (one of the
/// checked-in `example-pictures/`, never a fabricated fixture) so `measure_raw`
/// and `decide` genuinely run. The fake's stub `.tif` is not a decodable image,
/// so the JPEG-encode step fails after the (fake) render — a real happy-path
/// render can only be produced by real RawTherapee. That means this test
/// cannot demonstrate "second run renders nothing" the way the gated
/// end-to-end test can: with no successful render, there is no recorded edit
/// for a second run to find as up to date, so a second run repeats the same
/// work rather than skipping it. What this test does prove: the renderer ran
/// (via the fake), the failure isolation extends past the render call into
/// encoding, and no half-recorded `edits` row is left behind — the same
/// invariant `unreadable_raw_is_skipped_without_an_edits_row` checks, but with
/// the renderer actually invoked rather than never reached.
#[cfg(unix)]
#[test]
fn fake_renderer_runs_but_stub_output_cannot_complete_the_happy_path() {
    let raw = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../example-pictures/DSC03073.ARW");
    assert!(raw.exists(), "fixture missing: {}", raw.display());

    let (_rt_dir, rt) = fake_rawtherapee();
    let (_dir, cat) = temp_catalog();
    let id = seed_file(&cat, raw.to_str().unwrap(), "keep");
    let out = tempfile::TempDir::new().unwrap();
    let cfg = DevelopConfig {
        rawtherapee_path: rt.to_string_lossy().into_owned(),
        ..Default::default()
    };

    let report = finish_folder(&cat, &cfg, out.path(), &RecordingSink::default()).unwrap();
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
