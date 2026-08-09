use pipeline::catalog::Catalog;

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
              0.01, 0.18, 0.95, 0.001, 0.002, 2.0, 1.0, 1.5, NULL, NULL, NULL);",
    )
    .unwrap();
}
