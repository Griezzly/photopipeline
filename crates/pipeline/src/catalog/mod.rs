use std::{path::Path, sync::Mutex};

use duckdb::Connection;

use crate::error::CatalogError;

pub mod queries;
pub mod schema;

pub struct Catalog {
    conn: Mutex<Connection>,
    #[doc(hidden)]
    inject_flush_error: std::sync::atomic::AtomicBool,
}

impl Catalog {
    pub fn open(path: &Path) -> Result<Self, CatalogError> {
        // Create parent directories if they don't exist.
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| CatalogError::Db(format!("create dirs: {e}")))?;
            }
        }

        let conn = Connection::open(path).map_err(|e| CatalogError::Db(e.to_string()))?;

        // Ensure schema_version tracking table exists.
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS schema_version (version INTEGER PRIMARY KEY);",
        )
        .map_err(|e| CatalogError::Db(e.to_string()))?;

        // Determine current schema version.
        let current_version: u32 = conn
            .query_row(
                "SELECT COALESCE(MAX(version), 0) FROM schema_version",
                [],
                |r| r.get(0),
            )
            .map_err(|e| CatalogError::Db(e.to_string()))?;

        // Apply any pending migrations.
        for (i, migration_sql) in schema::MIGRATIONS.iter().enumerate() {
            let migration_version = (i + 1) as u32;
            if migration_version > current_version {
                conn.execute_batch(migration_sql)
                    .map_err(|e| CatalogError::Migration {
                        version: migration_version,
                        reason: e.to_string(),
                    })?;
            }
        }

        Ok(Self {
            conn: Mutex::new(conn),
            inject_flush_error: std::sync::atomic::AtomicBool::new(false),
        })
    }

    /// Enable flush-error injection for integration tests.
    ///
    /// After calling this, every subsequent `flush_batch` call on this
    /// `Catalog` instance will return `Err` without touching the database.
    #[doc(hidden)]
    pub fn simulate_flush_error(&self) {
        self.inject_flush_error
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// Returns `true` if the file at `path` needs (re-)processing.
    ///
    /// A file is considered already processed when a row exists with the
    /// same path, mtime_ns, and size_bytes.
    pub fn needs_processing(
        &self,
        path: &Path,
        mtime_ns: i64,
        size: u64,
    ) -> Result<bool, CatalogError> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| CatalogError::Db("mutex poisoned".into()))?;
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM files WHERE path = ? AND mtime_ns = ? AND size_bytes = ?",
                duckdb::params![path.to_string_lossy().as_ref(), mtime_ns, size as i64,],
                |r| r.get(0),
            )
            .map_err(|e| CatalogError::Db(e.to_string()))?;
        // Already processed when count > 0.
        Ok(count == 0)
    }

    /// Upsert a single file row and return its `id`.
    pub fn upsert_file(&self, file: &crate::ingest::IngestedFile) -> Result<i64, CatalogError> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| CatalogError::Db("mutex poisoned".into()))?;
        let hash_hex = format!("{:032x}", file.content_hash);
        let format_str = file.format.as_str();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let id: i64 = conn
            .query_row(
                "INSERT INTO files (path, content_hash, size_bytes, mtime_ns, file_format,
                                    has_sidecar_jpg, last_processed)
                 VALUES (?, ?, ?, ?, ?, ?, ?)
                 ON CONFLICT (path) DO UPDATE SET
                     content_hash    = excluded.content_hash,
                     size_bytes      = excluded.size_bytes,
                     mtime_ns        = excluded.mtime_ns,
                     file_format     = excluded.file_format,
                     has_sidecar_jpg = excluded.has_sidecar_jpg,
                     last_processed  = excluded.last_processed
                 RETURNING id",
                duckdb::params![
                    file.path.to_string_lossy().as_ref(),
                    hash_hex,
                    file.size as i64,
                    file.mtime_ns,
                    format_str,
                    file.has_sidecar_jpg,
                    now,
                ],
                |r| r.get(0),
            )
            .map_err(|e| CatalogError::Db(e.to_string()))?;
        Ok(id)
    }

    /// Upsert a single EXIF row for an already-persisted file.
    pub fn upsert_exif(
        &self,
        file_id: i64,
        exif: &crate::ingest::ExifData,
    ) -> Result<(), CatalogError> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| CatalogError::Db("mutex poisoned".into()))?;
        conn.execute(
            "INSERT INTO exif (file_id, captured_at, camera_make, camera_model, lens_model,
                               focal_length_mm, aperture, iso, shutter_seconds, width, height,
                               orientation)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT (file_id) DO UPDATE SET
                 captured_at     = excluded.captured_at,
                 camera_make     = excluded.camera_make,
                 camera_model    = excluded.camera_model,
                 lens_model      = excluded.lens_model,
                 focal_length_mm = excluded.focal_length_mm,
                 aperture        = excluded.aperture,
                 iso             = excluded.iso,
                 shutter_seconds = excluded.shutter_seconds,
                 width           = excluded.width,
                 height          = excluded.height,
                 orientation     = excluded.orientation",
            duckdb::params![
                file_id,
                exif.captured_at,
                exif.camera_make.as_deref(),
                exif.camera_model.as_deref(),
                exif.lens_model.as_deref(),
                exif.focal_length_mm,
                exif.aperture,
                exif.iso.map(|v| v as i32),
                exif.shutter_seconds,
                exif.width.map(|v| v as i32),
                exif.height.map(|v| v as i32),
                exif.orientation.map(|v| v as i16),
            ],
        )
        .map_err(|e| CatalogError::Db(e.to_string()))?;
        Ok(())
    }

    /// Bulk-upsert a batch of files + optional EXIF in a single transaction.
    ///
    /// Returns the database IDs in the same order as `batch`.
    pub fn flush_batch(
        &self,
        batch: &[(crate::ingest::IngestedFile, Option<crate::ingest::ExifData>)],
    ) -> Result<Vec<i64>, CatalogError> {
        if self
            .inject_flush_error
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            return Err(CatalogError::Db(
                "simulated flush error (test injection)".into(),
            ));
        }

        if batch.is_empty() {
            return Ok(Vec::new());
        }

        let mut conn = self
            .conn
            .lock()
            .map_err(|_| CatalogError::Db("mutex poisoned".into()))?;

        let tx = conn
            .transaction()
            .map_err(|e| CatalogError::Db(e.to_string()))?;

        let mut file_ids = Vec::with_capacity(batch.len());

        {
            let mut stmt = tx
                .prepare(
                    "INSERT INTO files (path, content_hash, size_bytes, mtime_ns, file_format,
                                        has_sidecar_jpg, last_processed)
                     VALUES (?, ?, ?, ?, ?, ?, ?)
                     ON CONFLICT (path) DO UPDATE SET
                         content_hash    = excluded.content_hash,
                         size_bytes      = excluded.size_bytes,
                         mtime_ns        = excluded.mtime_ns,
                         file_format     = excluded.file_format,
                         has_sidecar_jpg = excluded.has_sidecar_jpg,
                         last_processed  = excluded.last_processed
                     RETURNING id",
                )
                .map_err(|e| CatalogError::Db(e.to_string()))?;

            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs() as i64;

            for (file, _exif) in batch {
                let hash_hex = format!("{:032x}", file.content_hash);
                let format_str = file.format.as_str();
                let id: i64 = stmt
                    .query_row(
                        duckdb::params![
                            file.path.to_string_lossy().as_ref(),
                            hash_hex,
                            file.size as i64,
                            file.mtime_ns,
                            format_str,
                            file.has_sidecar_jpg,
                            now,
                        ],
                        |r| r.get(0),
                    )
                    .map_err(|e| CatalogError::Db(e.to_string()))?;
                file_ids.push(id);
            }
        }

        {
            let mut exif_stmt = tx
                .prepare(
                    "INSERT INTO exif (file_id, captured_at, camera_make, camera_model,
                                       lens_model, focal_length_mm, aperture, iso,
                                       shutter_seconds, width, height, orientation)
                     VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                     ON CONFLICT (file_id) DO UPDATE SET
                         captured_at     = excluded.captured_at,
                         camera_make     = excluded.camera_make,
                         camera_model    = excluded.camera_model,
                         lens_model      = excluded.lens_model,
                         focal_length_mm = excluded.focal_length_mm,
                         aperture        = excluded.aperture,
                         iso             = excluded.iso,
                         shutter_seconds = excluded.shutter_seconds,
                         width           = excluded.width,
                         height          = excluded.height,
                         orientation     = excluded.orientation",
                )
                .map_err(|e| CatalogError::Db(e.to_string()))?;

            for (i, (_, exif_opt)) in batch.iter().enumerate() {
                if let Some(exif) = exif_opt {
                    let file_id = file_ids[i];
                    exif_stmt
                        .execute(duckdb::params![
                            file_id,
                            exif.captured_at,
                            exif.camera_make.as_deref(),
                            exif.camera_model.as_deref(),
                            exif.lens_model.as_deref(),
                            exif.focal_length_mm,
                            exif.aperture,
                            exif.iso.map(|v| v as i32),
                            exif.shutter_seconds,
                            exif.width.map(|v| v as i32),
                            exif.height.map(|v| v as i32),
                            exif.orientation.map(|v| v as i16),
                        ])
                        .map_err(|e| CatalogError::Db(e.to_string()))?;
                }
            }
        }

        tx.commit().map_err(|e| CatalogError::Db(e.to_string()))?;
        Ok(file_ids)
    }

    /// Return the EXIF row for the file at `path`, or `None` if not present.
    pub fn get_exif_by_path(
        &self,
        path: &Path,
    ) -> Result<Option<crate::ingest::ExifData>, CatalogError> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| CatalogError::Db("mutex poisoned".into()))?;
        let result = conn.query_row(
            "SELECT e.captured_at, e.camera_make, e.camera_model, e.lens_model,
                    e.focal_length_mm, e.aperture, e.iso, e.shutter_seconds,
                    e.width, e.height, e.orientation
             FROM exif e
             JOIN files f ON f.id = e.file_id
             WHERE f.path = ?",
            duckdb::params![path.to_string_lossy().as_ref()],
            |row| {
                Ok(crate::ingest::ExifData {
                    captured_at: row.get(0)?,
                    camera_make: row.get(1)?,
                    camera_model: row.get(2)?,
                    lens_model: row.get(3)?,
                    focal_length_mm: row.get(4)?,
                    aperture: row.get(5)?,
                    iso: row.get::<_, Option<i32>>(6)?.map(|v| v as u32),
                    shutter_seconds: row.get(7)?,
                    width: row.get::<_, Option<i32>>(8)?.map(|v| v as u32),
                    height: row.get::<_, Option<i32>>(9)?.map(|v| v as u32),
                    orientation: row.get::<_, Option<i16>>(10)?.map(|v| v as u16),
                })
            },
        );
        match result {
            Ok(exif) => Ok(Some(exif)),
            Err(duckdb::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(CatalogError::Db(e.to_string())),
        }
    }

    /// Count the total number of file rows in the catalog.
    pub fn file_count(&self) -> Result<i64, CatalogError> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| CatalogError::Db("mutex poisoned".into()))?;
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))
            .map_err(|e| CatalogError::Db(e.to_string()))?;
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn make_catalog() -> (Catalog, TempDir) {
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("test.duckdb");
        let catalog = Catalog::open(&db_path).unwrap();
        (catalog, dir)
    }

    #[test]
    fn open_creates_schema() {
        let (_catalog, _dir) = make_catalog();
        // If open() succeeds, migrations ran correctly.
    }

    #[test]
    fn needs_processing_unknown_file() {
        let (catalog, _dir) = make_catalog();
        let path = PathBuf::from("/nonexistent/file.arw");
        assert!(catalog.needs_processing(&path, 12345, 1024).unwrap());
    }

    #[test]
    fn needs_processing_false_after_insert() {
        use crate::ingest::{ExifData, FileFormat, IngestedFile};

        let (catalog, _dir) = make_catalog();
        let path = PathBuf::from("/some/test.jpg");
        let file = IngestedFile {
            path: path.clone(),
            content_hash: 0xdeadbeef,
            size: 512,
            mtime_ns: 9999,
            format: FileFormat::Jpg,
            has_sidecar_jpg: false,
        };
        catalog.flush_batch(&[(file, None::<ExifData>)]).unwrap();
        // Same mtime/size → should no longer need processing.
        assert!(!catalog.needs_processing(&path, 9999, 512).unwrap());
        // Different mtime → needs processing.
        assert!(catalog.needs_processing(&path, 8888, 512).unwrap());
    }
}
