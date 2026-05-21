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

    /// Upsert a single sharpness row.
    pub fn upsert_sharpness(
        &self,
        file_id: i64,
        r: &crate::defect::SharpnessResult,
    ) -> Result<(), CatalogError> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| CatalogError::Db("mutex poisoned".into()))?;
        conn.execute(
            "INSERT INTO sharpness (file_id, s_global, s_subject, s_background, subject_ratio, detector_used)
             VALUES (?, ?, ?, ?, ?, ?)
             ON CONFLICT (file_id) DO UPDATE SET
                 s_global       = excluded.s_global,
                 s_subject      = excluded.s_subject,
                 s_background   = excluded.s_background,
                 subject_ratio  = excluded.subject_ratio,
                 detector_used  = excluded.detector_used",
            duckdb::params![
                file_id,
                r.s_global,
                r.s_subject,
                r.s_background,
                r.subject_ratio,
                r.detector_used,
            ],
        )
        .map_err(|e| CatalogError::Db(e.to_string()))?;
        Ok(())
    }

    /// Upsert a single exposure row.
    pub fn upsert_exposure(
        &self,
        file_id: i64,
        r: &crate::defect::ExposureResult,
    ) -> Result<(), CatalogError> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| CatalogError::Db("mutex poisoned".into()))?;
        conn.execute(
            "INSERT INTO exposure (file_id, clipped_highlights, clipped_shadows, mean_luma, histogram_skew)
             VALUES (?, ?, ?, ?, ?)
             ON CONFLICT (file_id) DO UPDATE SET
                 clipped_highlights = excluded.clipped_highlights,
                 clipped_shadows    = excluded.clipped_shadows,
                 mean_luma          = excluded.mean_luma,
                 histogram_skew     = excluded.histogram_skew",
            duckdb::params![
                file_id,
                r.clipped_highlights,
                r.clipped_shadows,
                r.mean_luma,
                r.histogram_skew,
            ],
        )
        .map_err(|e| CatalogError::Db(e.to_string()))?;
        Ok(())
    }

    /// Upsert a single defect flag row.
    pub fn upsert_defect_flag(
        &self,
        file_id: i64,
        flag: &crate::defect::DefectFlag,
    ) -> Result<(), CatalogError> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| CatalogError::Db("mutex poisoned".into()))?;
        conn.execute(
            "INSERT INTO defect_flags (file_id, flag_type, confidence, reason)
             VALUES (?, ?, ?, ?)
             ON CONFLICT (file_id, flag_type) DO UPDATE SET
                 confidence = excluded.confidence,
                 reason     = excluded.reason",
            duckdb::params![file_id, flag.flag_type, flag.confidence, flag.reason,],
        )
        .map_err(|e| CatalogError::Db(e.to_string()))?;
        Ok(())
    }

    /// Bulk-upsert a batch of defect rows (sharpness + exposure + flags) in a single transaction.
    pub fn flush_defect_batch(
        &self,
        rows: &[crate::defect::DefectRow],
    ) -> Result<(), CatalogError> {
        if rows.is_empty() {
            return Ok(());
        }

        let mut conn = self
            .conn
            .lock()
            .map_err(|_| CatalogError::Db("mutex poisoned".into()))?;

        let tx = conn
            .transaction()
            .map_err(|e| CatalogError::Db(e.to_string()))?;

        {
            let mut sharpness_stmt = tx
                .prepare(
                    "INSERT INTO sharpness (file_id, s_global, s_subject, s_background, subject_ratio, detector_used)
                     VALUES (?, ?, ?, ?, ?, ?)
                     ON CONFLICT (file_id) DO UPDATE SET
                         s_global       = excluded.s_global,
                         s_subject      = excluded.s_subject,
                         s_background   = excluded.s_background,
                         subject_ratio  = excluded.subject_ratio,
                         detector_used  = excluded.detector_used",
                )
                .map_err(|e| CatalogError::Db(e.to_string()))?;

            for row in rows {
                let s = &row.sharpness;
                sharpness_stmt
                    .execute(duckdb::params![
                        row.file_id,
                        s.s_global,
                        s.s_subject,
                        s.s_background,
                        s.subject_ratio,
                        s.detector_used,
                    ])
                    .map_err(|e| CatalogError::Db(e.to_string()))?;
            }
        }

        {
            let mut exposure_stmt = tx
                .prepare(
                    "INSERT INTO exposure (file_id, clipped_highlights, clipped_shadows, mean_luma, histogram_skew)
                     VALUES (?, ?, ?, ?, ?)
                     ON CONFLICT (file_id) DO UPDATE SET
                         clipped_highlights = excluded.clipped_highlights,
                         clipped_shadows    = excluded.clipped_shadows,
                         mean_luma          = excluded.mean_luma,
                         histogram_skew     = excluded.histogram_skew",
                )
                .map_err(|e| CatalogError::Db(e.to_string()))?;

            for row in rows {
                let e = &row.exposure;
                exposure_stmt
                    .execute(duckdb::params![
                        row.file_id,
                        e.clipped_highlights,
                        e.clipped_shadows,
                        e.mean_luma,
                        e.histogram_skew,
                    ])
                    .map_err(|e| CatalogError::Db(e.to_string()))?;
            }
        }

        {
            let mut flag_stmt = tx
                .prepare(
                    "INSERT INTO defect_flags (file_id, flag_type, confidence, reason)
                     VALUES (?, ?, ?, ?)
                     ON CONFLICT (file_id, flag_type) DO UPDATE SET
                         confidence = excluded.confidence,
                         reason     = excluded.reason",
                )
                .map_err(|e| CatalogError::Db(e.to_string()))?;

            for row in rows {
                for flag in &row.flags {
                    flag_stmt
                        .execute(duckdb::params![
                            row.file_id,
                            flag.flag_type,
                            flag.confidence,
                            flag.reason,
                        ])
                        .map_err(|e| CatalogError::Db(e.to_string()))?;
                }
            }
        }

        tx.commit().map_err(|e| CatalogError::Db(e.to_string()))?;
        Ok(())
    }

    /// Return all files that have not yet had defect analysis run on them.
    ///
    /// Returns `(file_id, path, content_hash_u128)`.
    pub fn files_needing_defect_analysis(
        &self,
    ) -> Result<Vec<(i64, std::path::PathBuf, u128)>, CatalogError> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| CatalogError::Db("mutex poisoned".into()))?;
        let mut stmt = conn
            .prepare(
                "SELECT f.id, f.path, f.content_hash
                 FROM files f
                 LEFT JOIN sharpness s ON s.file_id = f.id
                 WHERE s.file_id IS NULL",
            )
            .map_err(|e| CatalogError::Db(e.to_string()))?;

        let rows = stmt
            .query_map([], |row| {
                let id: i64 = row.get(0)?;
                let path_str: String = row.get(1)?;
                let hash_hex: String = row.get(2)?;
                Ok((id, path_str, hash_hex))
            })
            .map_err(|e| CatalogError::Db(e.to_string()))?;

        let mut result = Vec::new();
        for row in rows {
            let (id, path_str, hash_hex) = row.map_err(|e| CatalogError::Db(e.to_string()))?;
            let path = std::path::PathBuf::from(path_str);
            let hash = u128::from_str_radix(&hash_hex, 16).unwrap_or(0);
            result.push((id, path, hash));
        }
        Ok(result)
    }

    /// Count defect flags of a given type.
    pub fn count_defect_flags(&self, flag_type: &str) -> Result<i64, CatalogError> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| CatalogError::Db("mutex poisoned".into()))?;
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM defect_flags WHERE flag_type = ?",
                duckdb::params![flag_type],
                |r| r.get(0),
            )
            .map_err(|e| CatalogError::Db(e.to_string()))?;
        Ok(count)
    }

    /// Count the total number of sharpness rows in the catalog.
    pub fn sharpness_count(&self) -> Result<i64, CatalogError> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| CatalogError::Db("mutex poisoned".into()))?;
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM sharpness", [], |r| r.get(0))
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

    #[test]
    fn upsert_sharpness_round_trip() {
        use crate::defect::SharpnessResult;
        use crate::ingest::{ExifData, FileFormat, IngestedFile};

        let (catalog, _dir) = make_catalog();
        let path = PathBuf::from("/some/sharp.jpg");
        let file = IngestedFile {
            path: path.clone(),
            content_hash: 0xaabbccdd,
            size: 1024,
            mtime_ns: 1000,
            format: FileFormat::Jpg,
            has_sidecar_jpg: false,
        };
        let ids = catalog.flush_batch(&[(file, None::<ExifData>)]).unwrap();
        let file_id = ids[0];

        let sharpness = SharpnessResult {
            s_global: 42.5,
            s_subject: Some(55.0),
            s_background: Some(30.0),
            subject_ratio: Some(0.16),
            detector_used: "center-crop-fallback".into(),
        };
        catalog.upsert_sharpness(file_id, &sharpness).unwrap();

        // Verify round-trip by querying s_global.
        let conn = catalog.conn.lock().unwrap();
        let s_global: f32 = conn
            .query_row(
                "SELECT s_global FROM sharpness WHERE file_id = ?",
                duckdb::params![file_id],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            (s_global - 42.5).abs() < 0.001,
            "s_global mismatch: {s_global}"
        );
    }

    #[test]
    fn files_needing_defect_analysis_filters_correctly() {
        use crate::defect::SharpnessResult;
        use crate::ingest::{ExifData, FileFormat, IngestedFile};

        let (catalog, _dir) = make_catalog();

        // Insert 3 files one by one.
        let mut ids = Vec::new();
        for i in 0..3i64 {
            let file = IngestedFile {
                path: PathBuf::from(format!("/test/file{i}.jpg")),
                content_hash: i as u128,
                size: 100 + i as u64,
                mtime_ns: i,
                format: FileFormat::Jpg,
                has_sidecar_jpg: false,
            };
            let batch_ids = catalog.flush_batch(&[(file, None::<ExifData>)]).unwrap();
            ids.push(batch_ids[0]);
        }

        // Upsert sharpness for files 0 and 1 only.
        let sharpness = SharpnessResult {
            s_global: 10.0,
            s_subject: None,
            s_background: None,
            subject_ratio: None,
            detector_used: "center-crop-fallback".into(),
        };
        catalog.upsert_sharpness(ids[0], &sharpness).unwrap();
        catalog.upsert_sharpness(ids[1], &sharpness).unwrap();

        // Only file 2 should need defect analysis.
        let needing = catalog.files_needing_defect_analysis().unwrap();
        assert_eq!(needing.len(), 1, "expected exactly 1 file needing analysis");
        assert_eq!(needing[0].0, ids[2], "expected file_id of the third file");
    }
}
