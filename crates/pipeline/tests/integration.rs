use image::{ImageBuffer, Rgb};
use pipeline::{cache::Cache, catalog::Catalog, config::IngestConfig, ingest::ingest_directory};
use std::{fs, path::PathBuf};
use tempfile::TempDir;
use walkdir::WalkDir;

fn make_synthetic_jpg(path: &PathBuf, r: u8, g: u8, b: u8) {
    let img: ImageBuffer<Rgb<u8>, Vec<u8>> = ImageBuffer::from_fn(64, 64, |_, _| Rgb([r, g, b]));
    img.save(path).expect("save test jpg");
}

#[test]
fn first_scan_processes_all_second_skips() {
    let input_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let cache_dir = TempDir::new().unwrap();

    // Create 3 synthetic JPGs.
    let jpg1 = input_dir.path().join("red.jpg");
    let jpg2 = input_dir.path().join("green.jpg");
    let jpg3 = input_dir.path().join("blue.jpg");
    make_synthetic_jpg(&jpg1, 255, 0, 0);
    make_synthetic_jpg(&jpg2, 0, 255, 0);
    make_synthetic_jpg(&jpg3, 0, 0, 255);

    let db_path = db_dir.path().join("catalog.duckdb");
    let catalog = Catalog::open(&db_path).unwrap();
    let cache = Cache::open(cache_dir.path().to_owned()).unwrap();
    let cfg = IngestConfig::default();

    // First scan — all 3 should be processed.
    let report1 = ingest_directory(&[input_dir.path().to_owned()], &catalog, &cache, &cfg).unwrap();
    assert_eq!(
        report1.processed, 3,
        "first scan should process all 3 files"
    );
    assert_eq!(report1.errored, 0);

    // Second scan — must be a no-op (idempotency).
    let report2 = ingest_directory(&[input_dir.path().to_owned()], &catalog, &cache, &cfg).unwrap();
    assert_eq!(report2.processed, 0, "second scan must be a no-op");
    assert_eq!(report2.skipped, 3);

    // Verify catalog row count via a fresh connection.
    let catalog2 = Catalog::open(&db_path).unwrap();
    let count = catalog2.file_count().unwrap();
    assert_eq!(count, 3);
}

#[test]
fn corrupt_file_logs_and_continues() {
    let input_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let cache_dir = TempDir::new().unwrap();

    // One valid JPG and one corrupt "JPG".
    let good = input_dir.path().join("good.jpg");
    let bad = input_dir.path().join("bad.jpg");
    make_synthetic_jpg(&good, 128, 128, 128);
    fs::write(&bad, b"not a jpeg").unwrap();

    let catalog = Catalog::open(&db_dir.path().join("catalog.duckdb")).unwrap();
    let cache = Cache::open(cache_dir.path().to_owned()).unwrap();
    let cfg = IngestConfig::default();

    let report = ingest_directory(&[input_dir.path().to_owned()], &catalog, &cache, &cfg).unwrap();

    // The scan must not panic or return a hard error.
    // The corrupt file may be counted as processed (hash OK, preview failed with warning)
    // or errored; both are acceptable.
    assert_eq!(
        report.processed + report.errored,
        2,
        "both files should be accounted for"
    );
}

#[test]
fn cache_contains_webp_at_expected_path() {
    let input_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let cache_dir = TempDir::new().unwrap();

    let jpg = input_dir.path().join("test.jpg");
    make_synthetic_jpg(&jpg, 100, 100, 100);

    let catalog = Catalog::open(&db_dir.path().join("catalog.duckdb")).unwrap();
    let cache = Cache::open(cache_dir.path().to_owned()).unwrap();
    let cfg = IngestConfig::default();

    ingest_directory(&[input_dir.path().to_owned()], &catalog, &cache, &cfg).unwrap();

    // There should be exactly one .webp file in the cache.
    let webps: Vec<_> = WalkDir::new(cache_dir.path())
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().map(|x| x == "webp").unwrap_or(false))
        .collect();
    assert_eq!(webps.len(), 1, "should have 1 webp preview");
}
