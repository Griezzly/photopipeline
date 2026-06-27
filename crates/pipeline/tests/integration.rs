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

    // Use a multi-colour image so lossy encoding has something to compress.
    let jpg = input_dir.path().join("test.jpg");
    let img: ImageBuffer<Rgb<u8>, Vec<u8>> = ImageBuffer::from_fn(128, 128, |x, y| {
        Rgb([(x % 256) as u8, (y % 256) as u8, 128u8])
    });
    img.save(&jpg).unwrap();

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

    // The lossy-encoded WebP must be smaller than the source JPEG, confirming
    // that compression is actually happening (not lossless passthrough).
    let source_size = fs::metadata(&jpg).unwrap().len();
    let webp_size = fs::metadata(webps[0].path()).unwrap().len();
    assert!(
        webp_size < source_size,
        "webp ({webp_size} B) should be smaller than source jpg ({source_size} B)"
    );
}

// ── EXIF round-trip ───────────────────────────────────────────────────────────

/// Build a JPEG with a hand-crafted TIFF/EXIF APP1 segment injected after the SOI.
///
/// Known embedded values (used by `exif_round_trip_jpg` assertions):
///   Make = "TestMake", Model = "TestModel", LensModel = "TestLens 50mm",
///   DateTimeOriginal = "2023:06:15 12:00:00" (→ unix 1686830400),
///   FocalLength = 50/1 mm, FNumber = 28/10 (f/2.8),
///   ISOSpeedRatings = 200, ExposureTime = 1/100 s, Orientation = 1.
fn make_jpeg_with_known_exif(path: &PathBuf) {
    // Tiny JPEG body via image crate.
    let img: ImageBuffer<Rgb<u8>, Vec<u8>> =
        ImageBuffer::from_fn(8, 8, |_, _| Rgb([100u8, 100u8, 100u8]));
    let mut jpeg_bytes = Vec::new();
    {
        use image::codecs::jpeg::JpegEncoder;
        use image::ImageEncoder;
        JpegEncoder::new_with_quality(&mut jpeg_bytes, 50)
            .write_image(
                img.as_raw(),
                img.width(),
                img.height(),
                image::ExtendedColorType::Rgb8,
            )
            .unwrap();
    }

    // Inject APP1 after SOI (first 2 bytes = FF D8).
    assert_eq!(&jpeg_bytes[0..2], &[0xFF, 0xD8]);
    let app1 = build_exif_app1();
    let mut output = Vec::new();
    output.extend_from_slice(&[0xFF, 0xD8]);
    output.extend_from_slice(&app1);
    output.extend_from_slice(&jpeg_bytes[2..]);
    fs::write(path, output).unwrap();
}

/// Wrap TIFF data in a JPEG APP1 (FF E1) segment.
fn build_exif_app1() -> Vec<u8> {
    let tiff = build_tiff_data();
    // APP1 payload = "Exif\0\0" (6 bytes) + tiff data
    // Segment length field (2 bytes) includes itself.
    let seg_len = (6 + tiff.len() + 2) as u16;
    let mut app1 = vec![0xFF, 0xE1, (seg_len >> 8) as u8, seg_len as u8];
    app1.extend_from_slice(b"Exif\x00\x00");
    app1.extend_from_slice(&tiff);
    app1
}

/// Build a little-endian TIFF block with:
///
/// IFD0  (offset 8):
///   0x010F Make        ASCII  9  → offset  62  "TestMake\0"
///   0x0110 Model       ASCII 10  → offset  71  "TestModel\0"
///   0x0112 Orientation SHORT  1  → inline  1
///   0x8769 ExifIFD     LONG   1  → offset  81
///
/// ExifIFD (offset 81):
///   0x829A ExposureTime   RATIONAL 1  → offset 159  (1/100)
///   0x829D FNumber        RATIONAL 1  → offset 167  (28/10)
///   0x8827 ISOSpeedRatings SHORT   1  → inline  200
///   0x9003 DateTimeOrig   ASCII   20  → offset 175  "2023:06:15 12:00:00\0"
///   0x920A FocalLength    RATIONAL 1  → offset 195  (50/1)
///   0xA434 LensModel      ASCII   14  → offset 203  "TestLens 50mm\0"
///
/// Data area: offsets 159-216 (58 bytes).  Total TIFF: 217 bytes.
fn build_tiff_data() -> Vec<u8> {
    let mut t: Vec<u8> = Vec::with_capacity(217);

    // ── TIFF header (8 bytes) ────────────────────────────────────────────────
    t.extend_from_slice(&[0x49, 0x49]); // II (little-endian)
    t.extend_from_slice(&[0x2A, 0x00]); // magic
    t.extend_from_slice(&8u32.to_le_bytes()); // IFD0 at offset 8

    // ── IFD0 (offset 8, 54 bytes) ────────────────────────────────────────────
    t.extend_from_slice(&4u16.to_le_bytes()); // 4 entries
                                              // Make
    ifd_entry(&mut t, 0x010F, 2, 9, 62);
    // Model
    ifd_entry(&mut t, 0x0110, 2, 10, 71);
    // Orientation (SHORT, inline value 1)
    t.extend_from_slice(&0x0112u16.to_le_bytes()); // tag
    t.extend_from_slice(&3u16.to_le_bytes()); // type SHORT
    t.extend_from_slice(&1u32.to_le_bytes()); // count
    t.extend_from_slice(&1u32.to_le_bytes()); // value (inline, padded to 4)
                                              // ExifIFD pointer
    ifd_entry(&mut t, 0x8769, 4, 1, 81);
    t.extend_from_slice(&0u32.to_le_bytes()); // next IFD = none

    // ── IFD0 data (offsets 62–80) ─────────────────────────────────────────────
    assert_eq!(t.len(), 62);
    t.extend_from_slice(b"TestMake\x00"); // 9 bytes
    assert_eq!(t.len(), 71);
    t.extend_from_slice(b"TestModel\x00"); // 10 bytes

    // ── ExifIFD (offset 81, 78 bytes) ────────────────────────────────────────
    assert_eq!(t.len(), 81);
    t.extend_from_slice(&6u16.to_le_bytes()); // 6 entries
                                              // ExposureTime
    ifd_entry(&mut t, 0x829A, 5, 1, 159);
    // FNumber
    ifd_entry(&mut t, 0x829D, 5, 1, 167);
    // ISOSpeedRatings (SHORT, inline 200)
    t.extend_from_slice(&0x8827u16.to_le_bytes());
    t.extend_from_slice(&3u16.to_le_bytes());
    t.extend_from_slice(&1u32.to_le_bytes());
    t.extend_from_slice(&200u32.to_le_bytes()); // inline SHORT padded to 4
                                                // DateTimeOriginal
    ifd_entry(&mut t, 0x9003, 2, 20, 175);
    // FocalLength
    ifd_entry(&mut t, 0x920A, 5, 1, 195);
    // LensModel
    ifd_entry(&mut t, 0xA434, 2, 14, 203);
    t.extend_from_slice(&0u32.to_le_bytes()); // next IFD = none

    // ── Data (offsets 159–216) ────────────────────────────────────────────────
    assert_eq!(t.len(), 159);
    t.extend_from_slice(&1u32.to_le_bytes()); // ExposureTime numerator
    t.extend_from_slice(&100u32.to_le_bytes()); // ExposureTime denominator
    assert_eq!(t.len(), 167);
    t.extend_from_slice(&28u32.to_le_bytes()); // FNumber numerator
    t.extend_from_slice(&10u32.to_le_bytes()); // FNumber denominator
    assert_eq!(t.len(), 175);
    t.extend_from_slice(b"2023:06:15 12:00:00\x00"); // 20 bytes
    assert_eq!(t.len(), 195);
    t.extend_from_slice(&50u32.to_le_bytes()); // FocalLength numerator
    t.extend_from_slice(&1u32.to_le_bytes()); // FocalLength denominator
    assert_eq!(t.len(), 203);
    t.extend_from_slice(b"TestLens 50mm\x00"); // 14 bytes
    assert_eq!(t.len(), 217);
    t
}

/// Write one 12-byte IFD entry with an offset/inline value.
fn ifd_entry(buf: &mut Vec<u8>, tag: u16, typ: u16, count: u32, value_or_offset: u32) {
    buf.extend_from_slice(&tag.to_le_bytes());
    buf.extend_from_slice(&typ.to_le_bytes());
    buf.extend_from_slice(&count.to_le_bytes());
    buf.extend_from_slice(&value_or_offset.to_le_bytes());
}

#[test]
fn exif_round_trip_jpg() {
    let input_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let cache_dir = TempDir::new().unwrap();

    let jpg = input_dir.path().join("exif_test.jpg");
    make_jpeg_with_known_exif(&jpg);

    let db_path = db_dir.path().join("catalog.duckdb");
    let catalog = Catalog::open(&db_path).unwrap();
    let cache = Cache::open(cache_dir.path().to_owned()).unwrap();
    let cfg = IngestConfig::default();

    let report = ingest_directory(&[input_dir.path().to_owned()], &catalog, &cache, &cfg).unwrap();
    assert_eq!(report.processed, 1, "should process the EXIF test JPEG");
    assert_eq!(report.errored, 0);

    // Query via a fresh catalog connection to confirm the row was committed.
    let catalog2 = Catalog::open(&db_path).unwrap();
    let exif = catalog2
        .get_exif_by_path(&jpg)
        .expect("get_exif_by_path should not error")
        .expect("exif row should exist");

    // DateTimeOriginal "2023:06:15 12:00:00" → unix epoch 1686830400.
    assert_eq!(
        exif.captured_at,
        Some(1_686_830_400i64),
        "captured_at round-trip"
    );

    // String fields — exact equality; no wrapping quotes, no trailing whitespace.
    assert_eq!(
        exif.camera_make.as_deref(),
        Some("TestMake"),
        "camera_make must be exactly 'TestMake' (no quote-wrapping)"
    );
    assert_eq!(
        exif.camera_model.as_deref(),
        Some("TestModel"),
        "camera_model must be exactly 'TestModel' (no quote-wrapping)"
    );
    assert_eq!(
        exif.lens_model.as_deref(),
        Some("TestLens 50mm"),
        "lens_model must be exactly 'TestLens 50mm' (no quote-wrapping)"
    );

    // Numeric EXIF fields.
    assert_eq!(exif.iso, Some(200), "iso round-trip");
    assert_eq!(exif.orientation, Some(1), "orientation round-trip");

    let eps = 1e-4_f32;
    let focal = exif.focal_length_mm.expect("focal_length_mm");
    assert!((focal - 50.0).abs() < eps, "focal_length_mm {focal} ≠ 50.0");
    let aperture = exif.aperture.expect("aperture");
    assert!((aperture - 2.8).abs() < eps, "aperture {aperture} ≠ 2.8");
    let shutter = exif.shutter_seconds.expect("shutter_seconds");
    assert!(
        (shutter - 0.01).abs() < eps,
        "shutter_seconds {shutter} ≠ 0.01"
    );
}

// ── Flush-failure error counting ──────────────────────────────────────────────

#[test]
fn flush_failure_counts_as_errored() {
    let input_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let cache_dir = TempDir::new().unwrap();
    let db_path = db_dir.path().join("catalog.duckdb");

    // Seed: scan one file so the schema is fully initialised.
    let jpg1 = input_dir.path().join("first.jpg");
    make_synthetic_jpg(&jpg1, 100, 100, 100);
    {
        let catalog = Catalog::open(&db_path).unwrap();
        let cache = Cache::open(cache_dir.path().to_owned()).unwrap();
        let report = ingest_directory(
            &[input_dir.path().to_owned()],
            &catalog,
            &cache,
            &IngestConfig::default(),
        )
        .unwrap();
        assert_eq!(report.processed, 1, "seed scan");
    } // connection drops here

    // Add a second file so the next scan has something to process.
    let jpg2 = input_dir.path().join("second.jpg");
    make_synthetic_jpg(&jpg2, 200, 200, 200);

    // Open catalog with flush-error injection enabled: every flush_batch call
    // returns Err without touching the DB.
    let catalog = Catalog::open(&db_path).unwrap();
    catalog.simulate_flush_error();
    let cache = Cache::open(cache_dir.path().to_owned()).unwrap();
    let report = ingest_directory(
        &[input_dir.path().to_owned()],
        &catalog,
        &cache,
        &IngestConfig::default(),
    )
    .unwrap();

    assert_eq!(
        report.processed, 0,
        "processed must not be inflated on flush failure"
    );
    assert!(report.errored >= 1, "flush failure must increment errored");
    // jpg1 is unchanged, so it should be skipped rather than re-processed.
    assert!(report.skipped >= 1, "unchanged file must be skipped");
}
