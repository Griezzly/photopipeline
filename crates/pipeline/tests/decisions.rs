use pipeline::catalog::{Catalog, ReviewFilter};
use tempfile::TempDir;

#[test]
fn migration_creates_decisions_table_at_v2() {
    let dir = TempDir::new().unwrap();
    let catalog = Catalog::open(&dir.path().join("c.duckdb")).unwrap();
    assert_eq!(catalog.schema_version().unwrap(), 2);
    // count_decisions() is added in Task 2; here we only assert the version,
    // which proves the v2 migration ran without error.
}

use pipeline::catalog::{DuplicateMember, Verdict};
use pipeline::ingest::{ExifData, FileFormat, IngestedFile};
use std::path::PathBuf;

fn add_file(catalog: &Catalog, name: &str, hash: u128) -> i64 {
    let file = IngestedFile {
        path: PathBuf::from(format!("/lib/{name}")),
        content_hash: hash,
        size: 100,
        mtime_ns: 1,
        format: FileFormat::Jpg,
        has_sidecar_jpg: false,
    };
    catalog.flush_batch(&[(file, None)]).unwrap()[0]
}

#[test]
fn set_get_clear_decision() {
    let dir = TempDir::new().unwrap();
    let catalog = Catalog::open(&dir.path().join("c.duckdb")).unwrap();
    let id = add_file(&catalog, "a.jpg", 1);

    assert!(catalog.get_decision(id).unwrap().is_none());

    catalog.set_decision(id, Verdict::Reject, Some("soft")).unwrap();
    let d = catalog.get_decision(id).unwrap().unwrap();
    assert_eq!(d.verdict, Verdict::Reject);
    assert_eq!(d.note.as_deref(), Some("soft"));
    assert!(!d.is_keeper);

    // upsert overwrites verdict
    catalog.set_decision(id, Verdict::Keep, None).unwrap();
    assert_eq!(catalog.get_decision(id).unwrap().unwrap().verdict, Verdict::Keep);

    catalog.clear_decision(id).unwrap();
    assert!(catalog.get_decision(id).unwrap().is_none());
}

#[test]
fn pick_keeper_keeps_one_rejects_siblings() {
    let dir = TempDir::new().unwrap();
    let catalog = Catalog::open(&dir.path().join("c.duckdb")).unwrap();
    let a = add_file(&catalog, "a.jpg", 1);
    let b = add_file(&catalog, "b.jpg", 2);
    let c = add_file(&catalog, "c.jpg", 3);

    let gid = catalog.insert_duplicate_group("time+embed", 0).unwrap();
    catalog
        .insert_duplicate_members(
            gid,
            &[
                DuplicateMember { file_id: a, is_suggested_keeper: true, quality_score: 1.0 },
                DuplicateMember { file_id: b, is_suggested_keeper: false, quality_score: 0.5 },
                DuplicateMember { file_id: c, is_suggested_keeper: false, quality_score: 0.4 },
            ],
        )
        .unwrap();

    // user overrides the suggestion: pick b
    catalog.pick_keeper(b).unwrap();

    let db = catalog.get_decision(b).unwrap().unwrap();
    assert_eq!(db.verdict, Verdict::Keep);
    assert!(db.is_keeper);
    assert_eq!(catalog.get_decision(a).unwrap().unwrap().verdict, Verdict::Reject);
    assert_eq!(catalog.get_decision(c).unwrap().unwrap().verdict, Verdict::Reject);
    assert!(!catalog.get_decision(a).unwrap().unwrap().is_keeper);
}

#[test]
fn decision_counts_partition_total() {
    let dir = TempDir::new().unwrap();
    let catalog = Catalog::open(&dir.path().join("c.duckdb")).unwrap();
    let a = add_file(&catalog, "a.jpg", 1);
    let b = add_file(&catalog, "b.jpg", 2);
    let _c = add_file(&catalog, "c.jpg", 3);

    catalog.set_decision(a, Verdict::Keep, None).unwrap();
    catalog.set_decision(b, Verdict::Reject, None).unwrap();

    let counts = catalog.decision_counts().unwrap();
    assert_eq!(counts.kept, 1);
    assert_eq!(counts.rejected, 1);
    assert_eq!(counts.undecided, 1);
}

use pipeline::defect::DefectFlag;

#[test]
fn review_list_orders_flagged_first_and_filters() {
    let dir = TempDir::new().unwrap();
    let catalog = Catalog::open(&dir.path().join("c.duckdb")).unwrap();
    // clean file (no flag), captured earlier
    let clean = {
        let file = IngestedFile {
            path: PathBuf::from("/lib/clean.jpg"),
            content_hash: 10,
            size: 1,
            mtime_ns: 1,
            format: FileFormat::Jpg,
            has_sidecar_jpg: false,
        };
        let exif = Some(ExifData { captured_at: Some(1000), ..Default::default() });
        catalog.flush_batch(&[(file, exif)]).unwrap()[0]
    };
    // flagged file, captured later
    let flagged = {
        let file = IngestedFile {
            path: PathBuf::from("/lib/blurry.jpg"),
            content_hash: 11,
            size: 1,
            mtime_ns: 1,
            format: FileFormat::Jpg,
            has_sidecar_jpg: false,
        };
        let exif = Some(ExifData { captured_at: Some(2000), ..Default::default() });
        catalog.flush_batch(&[(file, exif)]).unwrap()[0]
    };
    // Real DefectFlag has no file_id field and reason: String (not Option<String>).
    // Use upsert_defect_flag which takes (file_id, &DefectFlag).
    catalog
        .upsert_defect_flag(
            flagged,
            &DefectFlag {
                flag_type: "blur".into(),
                confidence: 0.9,
                reason: "test".into(),
            },
        )
        .unwrap();

    // unfiltered: flagged comes first despite later capture time
    let all = catalog.review_list(&ReviewFilter::default()).unwrap();
    assert_eq!(all.len(), 2);
    assert_eq!(all[0].file_id, flagged);
    assert_eq!(all[0].flags, vec!["blur".to_string()]);
    assert_eq!(all[1].file_id, clean);
    assert!(all[1].flags.is_empty());

    // filter by flag_type
    let only_blur = catalog
        .review_list(&ReviewFilter { flag_type: Some("blur".into()), ..Default::default() })
        .unwrap();
    assert_eq!(only_blur.len(), 1);
    assert_eq!(only_blur[0].file_id, flagged);

    // filter by decided=false (neither has a decision yet → both)
    let undecided = catalog
        .review_list(&ReviewFilter { decided: Some(false), ..Default::default() })
        .unwrap();
    assert_eq!(undecided.len(), 2);
}

#[test]
fn lookup_file_returns_path_and_hash() {
    let dir = TempDir::new().unwrap();
    let catalog = Catalog::open(&dir.path().join("c.duckdb")).unwrap();
    let id = add_file(&catalog, "a.jpg", 0xABCD);

    let loc = catalog.lookup_file(id).unwrap().unwrap();
    assert_eq!(loc.path, PathBuf::from("/lib/a.jpg"));
    assert_eq!(loc.content_hash, 0xABCD);

    assert!(catalog.lookup_file(999_999).unwrap().is_none());

    // dump_file_by_id resolves the same row dump as dump_file(path)
    let dump = catalog.dump_file_by_id(id).unwrap().unwrap();
    assert_eq!(dump.file.id, id);
}
