use pipeline::catalog::Catalog;
use tempfile::TempDir;

#[test]
fn library_meta_roundtrips_at_v3() {
    let dir = TempDir::new().unwrap();
    let catalog = Catalog::open(&dir.path().join("c.duckdb")).unwrap();
    assert_eq!(catalog.schema_version().unwrap(), 3);

    // No meta yet.
    assert!(catalog.library_meta().unwrap().is_none());

    catalog.set_library_meta("/photos/trip", 1000).unwrap();
    // Second call must NOT overwrite the existing row.
    catalog.set_library_meta("/photos/other", 2000).unwrap();
    let (folder, created, last) = catalog.library_meta().unwrap().unwrap();
    assert_eq!(folder, "/photos/trip");
    assert_eq!(created, 1000);
    assert_eq!(last, None);

    catalog.set_last_analyzed(5555).unwrap();
    assert_eq!(catalog.library_meta().unwrap().unwrap().2, Some(5555));
}
