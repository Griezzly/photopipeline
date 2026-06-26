use image::{imageops, DynamicImage, ImageBuffer, Rgb};
use pipeline::catalog::{Catalog, MlRow};
use pipeline::config::DefectConfig;
use pipeline::defect::{compute_sharpness, SharpnessResult};
use pipeline::ingest::{ExifData, FileFormat, IngestedFile};
use std::path::PathBuf;
use tempfile::TempDir;

// ── config ───────────────────────────────────────────────────────────────────

/// DefectConfig with a tiny bucket sample floor so synthetic suites calibrate.
fn test_cfg() -> DefectConfig {
    let mut cfg = DefectConfig::default();
    cfg.blur.min_samples_for_bucket = 3;
    cfg
}

// ── EXIF: every fixture lands in one bucket (TestModel/TestLens/50mm/f2.8) ────

fn bucket_exif() -> ExifData {
    ExifData {
        captured_at: Some(1_686_830_400),
        camera_make: Some("TestMake".into()),
        camera_model: Some("TestModel".into()),
        lens_model: Some("TestLens 50mm".into()),
        focal_length_mm: Some(50.0),
        aperture: Some(2.8),
        iso: Some(200),
        shutter_seconds: Some(0.01),
        width: Some(256),
        height: Some(256),
        orientation: Some(1),
    }
}

// ── synthetic images ──────────────────────────────────────────────────────────

/// 256x256 high-frequency checkerboard (sharp).
fn sharp_checkerboard() -> DynamicImage {
    let img: ImageBuffer<Rgb<u8>, Vec<u8>> = ImageBuffer::from_fn(256, 256, |x, y| {
        if (x / 2 + y / 2) % 2 == 0 {
            Rgb([0, 0, 0])
        } else {
            Rgb([255, 255, 255])
        }
    });
    DynamicImage::ImageRgb8(img)
}

/// Strongly Gaussian-blurred checkerboard (genuinely blurry everywhere).
#[allow(dead_code)]
fn blurry_checkerboard() -> DynamicImage {
    let sharp = sharp_checkerboard().to_rgb8();
    let b1 = imageops::blur(&sharp, 6.0);
    let b2 = imageops::blur(&b1, 6.0);
    DynamicImage::ImageRgb8(b2)
}

/// Center region blurry, surround sharp (back-focus: subject soft, bg sharp).
#[allow(dead_code)]
fn back_focus_image() -> DynamicImage {
    let sharp = sharp_checkerboard().to_rgb8();
    let blurred = imageops::blur(&imageops::blur(&sharp, 6.0), 6.0);
    // Start from the sharp surround, paste the blurred center crop in.
    let mut out = sharp.clone();
    let (w, h) = (out.width(), out.height());
    let (x0, y0, x1, y1) = (w * 3 / 10, h * 3 / 10, w * 7 / 10, h * 7 / 10);
    for y in y0..y1 {
        for x in x0..x1 {
            out.put_pixel(x, y, *blurred.get_pixel(x, y));
        }
    }
    DynamicImage::ImageRgb8(out)
}

/// Center region sharp, surround blurry (shallow DoF / bokeh: subject crisp).
#[allow(dead_code)]
fn shallow_dof_image() -> DynamicImage {
    let sharp = sharp_checkerboard().to_rgb8();
    let blurred = imageops::blur(&imageops::blur(&sharp, 6.0), 6.0);
    let mut out = blurred.clone();
    let (w, h) = (out.width(), out.height());
    let (x0, y0, x1, y1) = (w * 3 / 10, h * 3 / 10, w * 7 / 10, h * 7 / 10);
    for y in y0..y1 {
        for x in x0..x1 {
            out.put_pixel(x, y, *sharp.get_pixel(x, y));
        }
    }
    DynamicImage::ImageRgb8(out)
}

/// Run the center-crop sharpness path on an in-memory image (no detector),
/// returning (s_subject, s_background). Mirrors what analyze_defects records.
fn sharpness_of(img: &DynamicImage) -> SharpnessResult {
    let cfg = DefectConfig::default();
    compute_sharpness(img, None, None, &cfg.blur)
}

// ── catalog plumbing ───────────────────────────────────────────────────────────

fn make_catalog() -> (Catalog, TempDir) {
    let dir = TempDir::new().unwrap();
    let catalog = Catalog::open(&dir.path().join("c.duckdb")).unwrap();
    (catalog, dir)
}

/// Insert one file with bucket EXIF + the given sharpness; returns file_id.
fn insert(catalog: &Catalog, idx: usize, sharp: &SharpnessResult) -> i64 {
    let file = IngestedFile {
        path: PathBuf::from(format!("/cal/{idx}.jpg")),
        content_hash: idx as u128,
        size: 1,
        mtime_ns: idx as i64,
        format: FileFormat::Jpg,
        has_sidecar_jpg: false,
    };
    let id = catalog.flush_batch(&[(file, Some(bucket_exif()))]).unwrap()[0];
    catalog.upsert_sharpness(id, sharp).unwrap();
    id
}

/// Insert five sharp baseline-population files; return their ids.
fn insert_sharp_population(catalog: &Catalog) -> Vec<i64> {
    let s = sharpness_of(&sharp_checkerboard());
    (0..5).map(|i| insert(catalog, i, &s)).collect()
}

/// Per-file flag presence, via the public `Catalog::count_file_flag` helper.
/// (The `Catalog` connection is private and DuckDB is single-writer, so a test
/// cannot open a second connection to the same DB — hence a catalog method,
/// added in Step 2 below.)
fn has_flag(catalog: &Catalog, file_id: i64, flag_type: &str) -> bool {
    catalog.count_file_flag(file_id, flag_type).unwrap() > 0
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[test]
fn sharp_not_flagged() {
    let (catalog, _dir) = make_catalog();
    let ids = insert_sharp_population(&catalog);
    let report = pipeline::run_calibration(&catalog, &test_cfg()).unwrap();
    assert_eq!(report.buckets_built, 1);
    for id in ids {
        assert!(
            !has_flag(&catalog, id, "blur"),
            "sharp file {id} must not be blur-flagged"
        );
        assert!(!has_flag(&catalog, id, "back_focus"));
    }
}

#[test]
fn genuinely_blurry_flagged() {
    let (catalog, _dir) = make_catalog();
    let sharp_ids = insert_sharp_population(&catalog);
    // Hand-set: subject and background both uniformly low (globally blurry image).
    // s_bg must be < s_subject * 2 so the back_focus path is NOT taken.
    // The sharp population all have s_subject ≈ 260100, so p10 ≈ 260100 and
    // 1000 is far below threshold.
    let blurry = SharpnessResult {
        s_global: 1000.0,
        s_subject: Some(1000.0),
        s_background: Some(1500.0),
        subject_ratio: Some(0.16),
        detector_used: "center-crop-fallback".into(),
    };
    let blur_id = insert(&catalog, 100, &blurry);

    pipeline::run_calibration(&catalog, &test_cfg()).unwrap();

    assert!(
        has_flag(&catalog, blur_id, "blur"),
        "blurry file must be blur-flagged"
    );
    for id in sharp_ids {
        assert!(
            !has_flag(&catalog, id, "blur"),
            "sharp file {id} must not be blur-flagged"
        );
    }
}

#[test]
fn back_focus_flagged() {
    let (catalog, _dir) = make_catalog();
    insert_sharp_population(&catalog);
    // Hand-set sharpness so subject is clearly soft and background >2x sharper.
    let bf = SharpnessResult {
        s_global: 50.0,
        s_subject: Some(5.0),
        s_background: Some(80.0),
        subject_ratio: Some(0.16),
        detector_used: "rt-detr-l".into(),
    };
    let bf_id = insert(&catalog, 100, &bf);

    pipeline::run_calibration(&catalog, &test_cfg()).unwrap();

    assert!(
        has_flag(&catalog, bf_id, "back_focus"),
        "must be back_focus"
    );
    assert!(!has_flag(&catalog, bf_id, "blur"), "must NOT be plain blur");
}

#[test]
fn shallow_dof_not_flagged() {
    let (catalog, _dir) = make_catalog();
    insert_sharp_population(&catalog);
    // Sharp subject (high s_subject), blurry background — the false-positive
    // case Phase 4 fixes. Subject is at/above the baseline → not flagged.
    // The sharp population s_subject ≈ 260100; p10 ≈ 260100, so s_subject
    // must be >= 260100 to avoid a blur flag. Use 300000 to be clearly above.
    let dof = SharpnessResult {
        s_global: 50000.0,
        s_subject: Some(300000.0),
        s_background: Some(8.0),
        subject_ratio: Some(0.16),
        detector_used: "rt-detr-l".into(),
    };
    let dof_id = insert(&catalog, 100, &dof);

    pipeline::run_calibration(&catalog, &test_cfg()).unwrap();

    assert!(
        !has_flag(&catalog, dof_id, "blur"),
        "sharp-subject bokeh must not be blur"
    );
    assert!(
        !has_flag(&catalog, dof_id, "back_focus"),
        "must not be back_focus"
    );
}

#[test]
fn falls_back_to_global() {
    // Single file in its bucket (below min_samples=3). Calibration must not
    // crash; the file is reflagged against the global sentinel instead.
    let (catalog, _dir) = make_catalog();
    let s = sharpness_of(&sharp_checkerboard());
    let id = insert(&catalog, 0, &s);

    let report = pipeline::run_calibration(&catalog, &test_cfg()).unwrap();
    // The lone bucket has 1 sample < 3 → no per-bucket row, but a global row exists.
    assert_eq!(
        report.buckets_built, 0,
        "undersized bucket builds no per-bucket row"
    );
    assert_eq!(report.global_n_samples, 1, "global counts the one sample");
    // Presence/absence of a flag is acceptable; assert only that it didn't error
    // and the flag state is well-defined (a count >= 0 always holds → query works).
    let _ = has_flag(&catalog, id, "blur");
}

#[test]
fn calibrate_is_idempotent() {
    let (catalog, _dir) = make_catalog();
    let pop = insert_sharp_population(&catalog);
    // Hand-set: uniformly blurry (both low, s_bg < s_subject*2 → "blur" not "back_focus").
    let blurry = SharpnessResult {
        s_global: 1000.0,
        s_subject: Some(1000.0),
        s_background: Some(1500.0),
        subject_ratio: Some(0.16),
        detector_used: "center-crop-fallback".into(),
    };
    let blur_id = insert(&catalog, 100, &blurry);
    // Add an IQA score so the low_iqa + bump path is exercised across both runs.
    catalog
        .flush_ml_batch(&[MlRow {
            file_id: blur_id,
            embedding: None,
            iqa_score: Some(("clip-iqa".into(), 0.01)),
        }])
        .unwrap();
    // A few higher IQA scores so 0.01 is in the bottom decile.
    for &fid in pop.iter() {
        catalog
            .flush_ml_batch(&[MlRow {
                file_id: fid,
                embedding: None,
                iqa_score: Some(("clip-iqa".into(), 0.8)),
            }])
            .unwrap();
    }

    let r1 = pipeline::run_calibration(&catalog, &test_cfg()).unwrap();
    let r2 = pipeline::run_calibration(&catalog, &test_cfg()).unwrap();

    assert_eq!(r1.flagged_blur, r2.flagged_blur);
    assert_eq!(r1.flagged_back_focus, r2.flagged_back_focus);
    assert_eq!(r1.flagged_low_iqa, r2.flagged_low_iqa);
    assert_eq!(r1.blur_confidence_bumped, r2.blur_confidence_bumped);
    assert_eq!(r1.buckets_built, r2.buckets_built);
    // The blur file still has exactly one blur row after the second run.
    assert_eq!(catalog.count_file_flag(blur_id, "blur").unwrap(), 1);
}
