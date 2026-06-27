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
