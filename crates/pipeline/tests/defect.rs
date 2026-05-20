use image::{ImageBuffer, Rgb};
use pipeline::{
    cache::Cache,
    catalog::Catalog,
    config::{DefectConfig, IngestConfig},
    defect::analyze_defects,
    ingest::ingest_directory,
};
use std::path::PathBuf;
use tempfile::TempDir;

fn make_jpg(path: &PathBuf, r: u8, g: u8, b: u8) {
    let img: ImageBuffer<Rgb<u8>, _> = ImageBuffer::from_fn(64, 64, |_, _| Rgb([r, g, b]));
    img.save(path).unwrap();
}

fn setup() -> (TempDir, TempDir, TempDir) {
    (
        TempDir::new().unwrap(),
        TempDir::new().unwrap(),
        TempDir::new().unwrap(),
    )
}

#[test]
fn exposure_flags_correctly() {
    let (input_dir, db_dir, cache_dir) = setup();

    // Create 3 JPGs: white (overexposed), black (underexposed), grey (ok).
    make_jpg(&input_dir.path().join("white.jpg"), 255, 255, 255);
    make_jpg(&input_dir.path().join("black.jpg"), 0, 0, 0);
    make_jpg(&input_dir.path().join("grey.jpg"), 128, 128, 128);

    let catalog = Catalog::open(&db_dir.path().join("test.duckdb")).unwrap();
    let cache = Cache::open(cache_dir.path().to_owned()).unwrap();
    let ingest_cfg = IngestConfig::default();
    let defect_cfg = DefectConfig::default();

    ingest_directory(
        &[input_dir.path().to_owned()],
        &catalog,
        &cache,
        &ingest_cfg,
    )
    .unwrap();
    analyze_defects(&catalog, &cache, &defect_cfg).unwrap();

    assert_eq!(
        catalog.count_defect_flags("overexposed").unwrap(),
        1,
        "expected exactly 1 overexposed flag"
    );
    assert_eq!(
        catalog.count_defect_flags("underexposed").unwrap(),
        1,
        "expected exactly 1 underexposed flag"
    );
}

#[test]
fn sharpness_rows_written_for_all_files() {
    let (input_dir, db_dir, cache_dir) = setup();

    // Create 5 gradient JPGs (each slightly different).
    for i in 0u32..5 {
        let img: ImageBuffer<Rgb<u8>, _> = ImageBuffer::from_fn(64, 64, |x, y| {
            Rgb([(x % 256) as u8, (y % 256) as u8, (i * 50) as u8])
        });
        img.save(input_dir.path().join(format!("grad{i}.jpg")))
            .unwrap();
    }

    let catalog = Catalog::open(&db_dir.path().join("test.duckdb")).unwrap();
    let cache = Cache::open(cache_dir.path().to_owned()).unwrap();
    let ingest_cfg = IngestConfig::default();
    let defect_cfg = DefectConfig::default();

    ingest_directory(
        &[input_dir.path().to_owned()],
        &catalog,
        &cache,
        &ingest_cfg,
    )
    .unwrap();
    let report = analyze_defects(&catalog, &cache, &defect_cfg).unwrap();

    assert_eq!(
        catalog.sharpness_count().unwrap(),
        5,
        "expected 5 sharpness rows"
    );
    assert_eq!(report.analyzed, 5, "expected report.analyzed == 5");
}

#[test]
fn analyze_defects_idempotent() {
    let (input_dir, db_dir, cache_dir) = setup();

    // Create 2 JPGs.
    make_jpg(&input_dir.path().join("a.jpg"), 100, 100, 100);
    make_jpg(&input_dir.path().join("b.jpg"), 150, 150, 150);

    let catalog = Catalog::open(&db_dir.path().join("test.duckdb")).unwrap();
    let cache = Cache::open(cache_dir.path().to_owned()).unwrap();
    let ingest_cfg = IngestConfig::default();
    let defect_cfg = DefectConfig::default();

    ingest_directory(
        &[input_dir.path().to_owned()],
        &catalog,
        &cache,
        &ingest_cfg,
    )
    .unwrap();

    // First run.
    let report1 = analyze_defects(&catalog, &cache, &defect_cfg).unwrap();
    assert_eq!(report1.analyzed, 2, "first run should analyze 2 files");

    // Second run: no new work.
    let report2 = analyze_defects(&catalog, &cache, &defect_cfg).unwrap();
    assert_eq!(
        report2.analyzed, 0,
        "second run should analyze 0 files (idempotent)"
    );
}
