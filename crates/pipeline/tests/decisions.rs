use pipeline::catalog::Catalog;
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
