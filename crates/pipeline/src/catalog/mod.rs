use std::{path::Path, sync::Mutex};

use duckdb::Connection;

use crate::error::CatalogError;

pub mod queries;
pub mod schema;

/// One row of ML output ready to be persisted.
pub struct MlRow {
    pub file_id: i64,
    /// `(model_name, embedding_vector)` — `None` when embedder was skipped.
    pub embedding: Option<(String, Vec<f32>)>,
    /// `(model_name, score)` — `None` when IQA scorer was skipped.
    pub iqa_score: Option<(String, f32)>,
}

/// One file's sharpness + raw EXIF + optional IQA score, for the reflag pass.
/// Buckets are computed in Rust (`calibration::buckets`) at the call site.
pub struct SharpnessReflagRow {
    pub file_id: i64,
    pub s_subject: Option<f32>,
    pub s_background: Option<f32>,
    pub camera_model: Option<String>,
    pub lens_model: Option<String>,
    pub focal_length_mm: Option<f32>,
    pub aperture: Option<f32>,
    pub iqa_score: Option<f32>,
}

/// Summary of a `rebuild_sharpness_baselines` run.
pub struct RebuildReport {
    /// Count of non-global (per-bucket) baseline rows written.
    pub buckets_built: usize,
    /// Total sample count backing the global fallback row.
    pub global_n_samples: usize,
}

/// One blur-related defect flag ready to persist. `flag_type` is one of
/// `"blur"`, `"back_focus"`, `"low_iqa"`.
pub struct BlurFlagRow {
    pub file_id: i64,
    pub flag_type: &'static str,
    pub confidence: f32,
    pub reason: String,
}

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

    /// Count defect flags of `flag_type` for a single file. Test/inspection helper.
    pub fn count_file_flag(&self, file_id: i64, flag_type: &str) -> Result<i64, CatalogError> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| CatalogError::Db("mutex poisoned".into()))?;
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM defect_flags WHERE file_id = ? AND flag_type = ?",
                duckdb::params![file_id, flag_type],
                |r| r.get(0),
            )
            .map_err(|e| CatalogError::Db(e.to_string()))?;
        Ok(n)
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

    /// Files that have no embedding row yet.  Returns `(file_id, path, hash)`.
    pub fn files_needing_embedding(
        &self,
    ) -> Result<Vec<(i64, std::path::PathBuf, u128)>, CatalogError> {
        self.files_missing_ml_row("embeddings")
    }

    /// Files that have no IQA row yet.  Returns `(file_id, path, hash)`.
    pub fn files_needing_iqa(&self) -> Result<Vec<(i64, std::path::PathBuf, u128)>, CatalogError> {
        self.files_missing_ml_row("iqa")
    }

    fn files_missing_ml_row(
        &self,
        table: &str,
    ) -> Result<Vec<(i64, std::path::PathBuf, u128)>, CatalogError> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| CatalogError::Db("mutex poisoned".into()))?;
        let sql = format!(
            "SELECT f.id, f.path, f.content_hash
             FROM files f
             LEFT JOIN {table} t ON t.file_id = f.id
             WHERE t.file_id IS NULL"
        );
        let mut stmt = conn
            .prepare(&sql)
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

    /// Bulk-upsert a batch of ML rows (embeddings + IQA scores) in one transaction.
    pub fn flush_ml_batch(&self, rows: &[MlRow]) -> Result<(), CatalogError> {
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
            // DuckDB's Rust crate does not implement ToSql for Value::List, so we
            // serialize the vector as a JSON array string and use CAST to FLOAT[].
            let mut emb_stmt = tx
                .prepare(
                    "INSERT INTO embeddings (file_id, model, vector)
                     VALUES (?, ?, CAST(? AS FLOAT[]))
                     ON CONFLICT (file_id) DO UPDATE SET
                         model  = excluded.model,
                         vector = excluded.vector",
                )
                .map_err(|e| CatalogError::Db(e.to_string()))?;

            for row in rows {
                if let Some((model, vec)) = &row.embedding {
                    use std::fmt::Write as _;
                    let mut json = String::with_capacity(vec.len() * 12 + 2);
                    json.push('[');
                    for (i, v) in vec.iter().enumerate() {
                        if i > 0 {
                            json.push(',');
                        }
                        write!(json, "{v}").unwrap();
                    }
                    json.push(']');
                    emb_stmt
                        .execute(duckdb::params![row.file_id, model, json])
                        .map_err(|e| CatalogError::Db(e.to_string()))?;
                }
            }
        }

        {
            let mut iqa_stmt = tx
                .prepare(
                    "INSERT INTO iqa (file_id, model, score)
                     VALUES (?, ?, ?)
                     ON CONFLICT (file_id) DO UPDATE SET
                         model = excluded.model,
                         score = excluded.score",
                )
                .map_err(|e| CatalogError::Db(e.to_string()))?;

            for row in rows {
                if let Some((model, score)) = &row.iqa_score {
                    iqa_stmt
                        .execute(duckdb::params![row.file_id, model, score])
                        .map_err(|e| CatalogError::Db(e.to_string()))?;
                }
            }
        }

        tx.commit().map_err(|e| CatalogError::Db(e.to_string()))?;
        Ok(())
    }

    /// Count the total number of embedding rows.
    pub fn embedding_count(&self) -> Result<i64, CatalogError> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| CatalogError::Db("mutex poisoned".into()))?;
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM embeddings", [], |r| r.get(0))
            .map_err(|e| CatalogError::Db(e.to_string()))?;
        Ok(count)
    }

    /// Count the total number of IQA rows.
    pub fn iqa_count(&self) -> Result<i64, CatalogError> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| CatalogError::Db("mutex poisoned".into()))?;
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM iqa", [], |r| r.get(0))
            .map_err(|e| CatalogError::Db(e.to_string()))?;
        Ok(count)
    }

    /// Delete all blur-related defect flags (`blur`, `back_focus`, `low_iqa`),
    /// leaving exposure flags untouched. Returns the number of rows deleted.
    pub fn clear_blur_related_flags(&self) -> Result<usize, CatalogError> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| CatalogError::Db("mutex poisoned".into()))?;
        let n = conn
            .execute(
                "DELETE FROM defect_flags
                 WHERE flag_type IN ('blur', 'back_focus', 'low_iqa')",
                [],
            )
            .map_err(|e| CatalogError::Db(e.to_string()))?;
        Ok(n)
    }

    /// Global 10th-percentile IQA score across the whole `iqa` table.
    /// Returns `None` when the table is empty.
    pub fn iqa_global_p10(&self) -> Result<Option<f32>, CatalogError> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| CatalogError::Db("mutex poisoned".into()))?;
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM iqa", [], |r| r.get(0))
            .map_err(|e| CatalogError::Db(e.to_string()))?;
        if count == 0 {
            return Ok(None);
        }
        let p10: f64 = conn
            .query_row("SELECT quantile_cont(score, 0.10) FROM iqa", [], |r| {
                r.get(0)
            })
            .map_err(|e| CatalogError::Db(e.to_string()))?;
        Ok(Some(p10 as f32))
    }

    /// One row per file that has a sharpness record, joined to EXIF (raw,
    /// un-bucketed) and the optional IQA score. Used by the reflag pass to
    /// avoid N+1 queries.
    pub fn iter_sharpness_for_reflag(&self) -> Result<Vec<SharpnessReflagRow>, CatalogError> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| CatalogError::Db("mutex poisoned".into()))?;
        let mut stmt = conn
            .prepare(
                "SELECT s.file_id, s.s_subject, s.s_background,
                        e.camera_model, e.lens_model, e.focal_length_mm, e.aperture,
                        i.score
                 FROM sharpness s
                 LEFT JOIN exif e ON e.file_id = s.file_id
                 LEFT JOIN iqa  i ON i.file_id = s.file_id",
            )
            .map_err(|e| CatalogError::Db(e.to_string()))?;

        let rows = stmt
            .query_map([], |row| {
                Ok(SharpnessReflagRow {
                    file_id: row.get(0)?,
                    s_subject: row.get(1)?,
                    s_background: row.get(2)?,
                    camera_model: row.get(3)?,
                    lens_model: row.get(4)?,
                    focal_length_mm: row.get(5)?,
                    aperture: row.get(6)?,
                    iqa_score: row.get(7)?,
                })
            })
            .map_err(|e| CatalogError::Db(e.to_string()))?;

        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| CatalogError::Db(e.to_string()))?);
        }
        Ok(out)
    }

    /// Per-bucket p10 for `(camera, lens, focal_bucket, aperture_bucket)`, but
    /// only when that baseline row has `n_samples >= min_samples`. Otherwise
    /// `None` (caller should fall back to the global sentinel).
    pub fn bucket_baseline_p10(
        &self,
        camera_model: &str,
        lens_model: &str,
        focal_bucket: i32,
        aperture_bucket: f32,
        min_samples: usize,
    ) -> Result<Option<f32>, CatalogError> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| CatalogError::Db("mutex poisoned".into()))?;
        let result = conn.query_row(
            "SELECT s_subject_p10, n_samples FROM sharpness_baseline
             WHERE camera_model = ? AND lens_model = ?
               AND focal_bucket = ? AND aperture_bucket = ?",
            duckdb::params![camera_model, lens_model, focal_bucket, aperture_bucket],
            |r| {
                let p10: f32 = r.get(0)?;
                let n: i64 = r.get(1)?;
                Ok((p10, n))
            },
        );
        match result {
            Ok((p10, n)) if (n as usize) >= min_samples => Ok(Some(p10)),
            Ok(_) => Ok(None),
            Err(duckdb::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(CatalogError::Db(e.to_string())),
        }
    }

    /// Bulk-write blur-related flags in one transaction. Uses prepared
    /// `INSERT … ON CONFLICT (file_id, flag_type)` (NOT the Appender): the
    /// `defect_flags` table has a `DEFAULT nextval()` id and a UNIQUE
    /// constraint, which the positional Appender cannot satisfy. Matches the
    /// `flush_defect_batch` pattern; one transaction per batch.
    pub fn flush_blur_flag_batch(&self, flags: &[BlurFlagRow]) -> Result<(), CatalogError> {
        if flags.is_empty() {
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
            let mut stmt = tx
                .prepare(
                    "INSERT INTO defect_flags (file_id, flag_type, confidence, reason)
                     VALUES (?, ?, ?, ?)
                     ON CONFLICT (file_id, flag_type) DO UPDATE SET
                         confidence = excluded.confidence,
                         reason     = excluded.reason",
                )
                .map_err(|e| CatalogError::Db(e.to_string()))?;
            for f in flags {
                stmt.execute(duckdb::params![
                    f.file_id,
                    f.flag_type,
                    f.confidence,
                    f.reason
                ])
                .map_err(|e| CatalogError::Db(e.to_string()))?;
            }
        }
        tx.commit().map_err(|e| CatalogError::Db(e.to_string()))?;
        Ok(())
    }

    /// Rebuild `sharpness_baseline` from current `sharpness`+`exif` data.
    ///
    /// Buckets are computed in Rust (no DuckDB UDF). Per-bucket rows are written
    /// only when the bucket has `>= min_samples` samples; a global sentinel row
    /// `('*','*',0,0.0)` is always written (when any sample exists) with the
    /// total population's percentiles. All writes happen in one transaction;
    /// the table is fully replaced (old rows deleted first) for idempotency.
    pub fn rebuild_sharpness_baselines(
        &self,
        min_samples: usize,
    ) -> Result<RebuildReport, CatalogError> {
        use crate::calibration::buckets::{aperture_bucket, focal_bucket};
        use std::collections::HashMap;

        // Phase 1: read qualifying raw samples (lock released before the write tx).
        struct Raw {
            camera: String,
            lens: String,
            focal: f32,
            aperture: f32,
            s_subject: f32,
        }
        let raws: Vec<Raw> = {
            let conn = self
                .conn
                .lock()
                .map_err(|_| CatalogError::Db("mutex poisoned".into()))?;
            let mut stmt = conn
                .prepare(
                    "SELECT e.camera_model, e.lens_model, e.focal_length_mm, e.aperture,
                            s.s_subject
                     FROM sharpness s
                     JOIN exif e ON e.file_id = s.file_id
                     WHERE s.s_subject IS NOT NULL
                       AND e.camera_model IS NOT NULL
                       AND e.lens_model IS NOT NULL
                       AND e.focal_length_mm IS NOT NULL
                       AND e.aperture IS NOT NULL",
                )
                .map_err(|e| CatalogError::Db(e.to_string()))?;
            let rows = stmt
                .query_map([], |row| {
                    Ok(Raw {
                        camera: row.get::<_, String>(0)?,
                        lens: row.get::<_, String>(1)?,
                        focal: row.get::<_, f32>(2)?,
                        aperture: row.get::<_, f32>(3)?,
                        s_subject: row.get::<_, f32>(4)?,
                    })
                })
                .map_err(|e| CatalogError::Db(e.to_string()))?;
            let mut v = Vec::new();
            for r in rows {
                v.push(r.map_err(|e| CatalogError::Db(e.to_string()))?);
            }
            v
        };

        // Group by (camera, lens, focal_bucket, aperture_bucket-as-bits) in Rust.
        let mut groups: HashMap<(String, String, i32, u32), Vec<f32>> = HashMap::new();
        let mut global: Vec<f32> = Vec::with_capacity(raws.len());
        for r in &raws {
            global.push(r.s_subject);
            let fb = focal_bucket(r.focal);
            let ab = aperture_bucket(r.aperture);
            groups
                .entry((r.camera.clone(), r.lens.clone(), fb, ab.to_bits()))
                .or_default()
                .push(r.s_subject);
        }

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let global_n = global.len();

        // Phase 2: write everything in one transaction (delete + reinsert).
        let mut conn = self
            .conn
            .lock()
            .map_err(|_| CatalogError::Db("mutex poisoned".into()))?;
        let tx = conn
            .transaction()
            .map_err(|e| CatalogError::Db(e.to_string()))?;

        tx.execute("DELETE FROM sharpness_baseline", [])
            .map_err(|e| CatalogError::Db(e.to_string()))?;

        let mut buckets_built = 0usize;
        {
            let mut stmt = tx
                .prepare(
                    "INSERT INTO sharpness_baseline
                        (camera_model, lens_model, focal_bucket, aperture_bucket,
                         s_subject_p10, s_subject_p50, s_subject_p90, n_samples, last_updated)
                     VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
                     ON CONFLICT (camera_model, lens_model, focal_bucket, aperture_bucket)
                     DO UPDATE SET
                         s_subject_p10 = excluded.s_subject_p10,
                         s_subject_p50 = excluded.s_subject_p50,
                         s_subject_p90 = excluded.s_subject_p90,
                         n_samples     = excluded.n_samples,
                         last_updated  = excluded.last_updated",
                )
                .map_err(|e| CatalogError::Db(e.to_string()))?;

            for ((camera, lens, fb, ab_bits), mut samples) in groups {
                if samples.len() < min_samples {
                    continue;
                }
                let ab = f32::from_bits(ab_bits);
                let (p10, p50, p90) = percentiles(&mut samples);
                stmt.execute(duckdb::params![
                    camera,
                    lens,
                    fb,
                    ab,
                    p10,
                    p50,
                    p90,
                    samples.len() as i32,
                    now,
                ])
                .map_err(|e| CatalogError::Db(e.to_string()))?;
                buckets_built += 1;
            }

            // Global sentinel row (only when there is any sample at all).
            if global_n > 0 {
                let mut g = global;
                let (p10, p50, p90) = percentiles(&mut g);
                stmt.execute(duckdb::params![
                    "*",
                    "*",
                    0i32,
                    0.0f32,
                    p10,
                    p50,
                    p90,
                    global_n as i32,
                    now,
                ])
                .map_err(|e| CatalogError::Db(e.to_string()))?;
            }
        }

        tx.commit().map_err(|e| CatalogError::Db(e.to_string()))?;

        Ok(RebuildReport {
            buckets_built,
            global_n_samples: global_n,
        })
    }
}

/// Return (p10, p50, p90) of `samples` using linear interpolation between
/// order statistics. Sorts `samples` in place. `samples` must be non-empty.
fn percentiles(samples: &mut [f32]) -> (f32, f32, f32) {
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    (
        percentile_sorted(samples, 0.10),
        percentile_sorted(samples, 0.50),
        percentile_sorted(samples, 0.90),
    )
}

fn percentile_sorted(sorted: &[f32], q: f32) -> f32 {
    let n = sorted.len();
    if n == 1 {
        return sorted[0];
    }
    let rank = q * (n as f32 - 1.0);
    let lo = rank.floor() as usize;
    let hi = rank.ceil() as usize;
    let frac = rank - lo as f32;
    sorted[lo] + (sorted[hi] - sorted[lo]) * frac
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

    #[test]
    fn clear_blur_related_flags_only_removes_blur_kinds() {
        use crate::defect::DefectFlag;
        use crate::ingest::{ExifData, FileFormat, IngestedFile};

        let (catalog, _dir) = make_catalog();
        let file = IngestedFile {
            path: PathBuf::from("/c/clear.jpg"),
            content_hash: 1,
            size: 1,
            mtime_ns: 1,
            format: FileFormat::Jpg,
            has_sidecar_jpg: false,
        };
        let id = catalog.flush_batch(&[(file, None::<ExifData>)]).unwrap()[0];

        for ft in [
            "overexposed",
            "underexposed",
            "blur",
            "back_focus",
            "low_iqa",
        ] {
            catalog
                .upsert_defect_flag(
                    id,
                    &DefectFlag {
                        flag_type: ft.to_string(),
                        confidence: 0.5,
                        reason: "t".into(),
                    },
                )
                .unwrap();
        }

        let deleted = catalog.clear_blur_related_flags().unwrap();
        assert_eq!(deleted, 3, "should delete blur/back_focus/low_iqa only");
        assert_eq!(catalog.count_defect_flags("overexposed").unwrap(), 1);
        assert_eq!(catalog.count_defect_flags("underexposed").unwrap(), 1);
        assert_eq!(catalog.count_defect_flags("blur").unwrap(), 0);
        assert_eq!(catalog.count_defect_flags("back_focus").unwrap(), 0);
        assert_eq!(catalog.count_defect_flags("low_iqa").unwrap(), 0);
    }

    #[test]
    fn rebuild_baselines_builds_bucket_and_global() {
        use crate::defect::SharpnessResult;
        use crate::ingest::{ExifData, FileFormat, IngestedFile};

        let (catalog, _dir) = make_catalog();

        // 4 files, identical EXIF bucket (TestModel / TestLens / 50mm / f2.8),
        // s_subject = 10, 20, 30, 40.
        for (i, s) in [10.0f32, 20.0, 30.0, 40.0].into_iter().enumerate() {
            let file = IngestedFile {
                path: PathBuf::from(format!("/b/{i}.jpg")),
                content_hash: i as u128,
                size: 1,
                mtime_ns: i as i64,
                format: FileFormat::Jpg,
                has_sidecar_jpg: false,
            };
            let exif = ExifData {
                captured_at: Some(1000),
                camera_make: Some("TestMake".into()),
                camera_model: Some("TestModel".into()),
                lens_model: Some("TestLens 50mm".into()),
                focal_length_mm: Some(50.0),
                aperture: Some(2.8),
                iso: Some(200),
                shutter_seconds: Some(0.01),
                width: Some(64),
                height: Some(64),
                orientation: Some(1),
            };
            let id = catalog.flush_batch(&[(file, Some(exif))]).unwrap()[0];
            catalog
                .upsert_sharpness(
                    id,
                    &SharpnessResult {
                        s_global: s,
                        s_subject: Some(s),
                        s_background: Some(s),
                        subject_ratio: Some(0.16),
                        detector_used: "rt-detr-l".into(),
                    },
                )
                .unwrap();
        }

        // min_samples = 3 → the 4-sample bucket qualifies.
        let report = catalog.rebuild_sharpness_baselines(3).unwrap();
        assert_eq!(report.buckets_built, 1, "one qualifying bucket");
        assert_eq!(report.global_n_samples, 4, "global counts all 4 samples");

        // The global sentinel row exists.
        let conn = catalog.conn.lock().unwrap();
        let global_n: i64 = conn
            .query_row(
                "SELECT n_samples FROM sharpness_baseline
                 WHERE camera_model = '*' AND lens_model = '*'
                   AND focal_bucket = 0 AND aperture_bucket = 0.0",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(global_n, 4);
    }

    #[test]
    fn iter_sharpness_for_reflag_returns_raw_exif() {
        use crate::defect::SharpnessResult;
        use crate::ingest::{ExifData, FileFormat, IngestedFile};

        let (catalog, _dir) = make_catalog();
        let file = IngestedFile {
            path: PathBuf::from("/r/0.jpg"),
            content_hash: 7,
            size: 1,
            mtime_ns: 1,
            format: FileFormat::Jpg,
            has_sidecar_jpg: false,
        };
        let exif = ExifData {
            captured_at: Some(1000),
            camera_make: Some("TestMake".into()),
            camera_model: Some("TestModel".into()),
            lens_model: Some("TestLens 50mm".into()),
            focal_length_mm: Some(50.0),
            aperture: Some(2.8),
            iso: Some(200),
            shutter_seconds: Some(0.01),
            width: Some(64),
            height: Some(64),
            orientation: Some(1),
        };
        let id = catalog.flush_batch(&[(file, Some(exif))]).unwrap()[0];
        catalog
            .upsert_sharpness(
                id,
                &SharpnessResult {
                    s_global: 12.0,
                    s_subject: Some(12.0),
                    s_background: Some(30.0),
                    subject_ratio: Some(0.16),
                    detector_used: "rt-detr-l".into(),
                },
            )
            .unwrap();

        let rows = catalog.iter_sharpness_for_reflag().unwrap();
        assert_eq!(rows.len(), 1);
        let r = &rows[0];
        assert_eq!(r.file_id, id);
        assert_eq!(r.s_subject, Some(12.0));
        assert_eq!(r.s_background, Some(30.0));
        assert_eq!(r.camera_model.as_deref(), Some("TestModel"));
        assert_eq!(r.focal_length_mm, Some(50.0));
        assert_eq!(r.aperture, Some(2.8));
    }

    #[test]
    fn flush_blur_flag_batch_inserts_and_upserts() {
        use crate::catalog::BlurFlagRow;
        use crate::ingest::{ExifData, FileFormat, IngestedFile};

        let (catalog, _dir) = make_catalog();
        let file = IngestedFile {
            path: PathBuf::from("/f/0.jpg"),
            content_hash: 3,
            size: 1,
            mtime_ns: 1,
            format: FileFormat::Jpg,
            has_sidecar_jpg: false,
        };
        let id = catalog.flush_batch(&[(file, None::<ExifData>)]).unwrap()[0];

        catalog
            .flush_blur_flag_batch(&[
                BlurFlagRow {
                    file_id: id,
                    flag_type: "blur",
                    confidence: 0.4,
                    reason: "r".into(),
                },
                BlurFlagRow {
                    file_id: id,
                    flag_type: "low_iqa",
                    confidence: 0.5,
                    reason: "r2".into(),
                },
            ])
            .unwrap();
        assert_eq!(catalog.count_defect_flags("blur").unwrap(), 1);
        assert_eq!(catalog.count_defect_flags("low_iqa").unwrap(), 1);

        // Re-flush the same (file_id, flag_type) with a new confidence → upsert,
        // not a UNIQUE violation.
        catalog
            .flush_blur_flag_batch(&[BlurFlagRow {
                file_id: id,
                flag_type: "blur",
                confidence: 0.9,
                reason: "r3".into(),
            }])
            .unwrap();
        assert_eq!(
            catalog.count_defect_flags("blur").unwrap(),
            1,
            "still one blur row"
        );
        let conn = catalog.conn.lock().unwrap();
        let conf: f32 = conn
            .query_row(
                "SELECT confidence FROM defect_flags WHERE file_id = ? AND flag_type = 'blur'",
                duckdb::params![id],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            (conf - 0.9).abs() < 1e-5,
            "confidence upserted to 0.9, got {conf}"
        );
    }

    #[test]
    fn bucket_baseline_p10_respects_min_samples() {
        use crate::defect::SharpnessResult;
        use crate::ingest::{ExifData, FileFormat, IngestedFile};

        let (catalog, _dir) = make_catalog();
        for (i, s) in [10.0f32, 20.0, 30.0].into_iter().enumerate() {
            let file = IngestedFile {
                path: PathBuf::from(format!("/p/{i}.jpg")),
                content_hash: i as u128,
                size: 1,
                mtime_ns: i as i64,
                format: FileFormat::Jpg,
                has_sidecar_jpg: false,
            };
            let exif = ExifData {
                captured_at: Some(1),
                camera_make: Some("TestMake".into()),
                camera_model: Some("TestModel".into()),
                lens_model: Some("TestLens 50mm".into()),
                focal_length_mm: Some(50.0),
                aperture: Some(2.8),
                iso: Some(200),
                shutter_seconds: Some(0.01),
                width: Some(64),
                height: Some(64),
                orientation: Some(1),
            };
            let id = catalog.flush_batch(&[(file, Some(exif))]).unwrap()[0];
            catalog
                .upsert_sharpness(
                    id,
                    &SharpnessResult {
                        s_global: s,
                        s_subject: Some(s),
                        s_background: Some(s),
                        subject_ratio: Some(0.16),
                        detector_used: "rt-detr-l".into(),
                    },
                )
                .unwrap();
        }
        catalog.rebuild_sharpness_baselines(3).unwrap();

        // The bucket has 3 samples. min_samples=3 → Some; min_samples=4 → None.
        let fb = crate::calibration::buckets::focal_bucket(50.0);
        let ab = crate::calibration::buckets::aperture_bucket(2.8);
        let got = catalog
            .bucket_baseline_p10("TestModel", "TestLens 50mm", fb, ab, 3)
            .unwrap();
        assert!(got.is_some(), "3 >= 3 → Some");
        let none = catalog
            .bucket_baseline_p10("TestModel", "TestLens 50mm", fb, ab, 4)
            .unwrap();
        assert!(none.is_none(), "3 < 4 → None");
    }

    #[test]
    fn iqa_global_p10_none_when_empty_some_when_populated() {
        use crate::catalog::MlRow;
        use crate::ingest::{ExifData, FileFormat, IngestedFile};

        let (catalog, _dir) = make_catalog();
        assert!(catalog.iqa_global_p10().unwrap().is_none(), "empty → None");

        // Insert 10 files with iqa scores 0.0..=0.9.
        for i in 0..10i64 {
            let file = IngestedFile {
                path: PathBuf::from(format!("/iqa/{i}.jpg")),
                content_hash: i as u128,
                size: 1,
                mtime_ns: i,
                format: FileFormat::Jpg,
                has_sidecar_jpg: false,
            };
            let id = catalog.flush_batch(&[(file, None::<ExifData>)]).unwrap()[0];
            catalog
                .flush_ml_batch(&[MlRow {
                    file_id: id,
                    embedding: None,
                    iqa_score: Some(("clip-iqa".into(), i as f32 / 10.0)),
                }])
                .unwrap();
        }
        let p10 = catalog.iqa_global_p10().unwrap().expect("should be Some");
        // quantile_cont(0.10) over 0.0..0.9 is ~0.09.
        assert!((0.0..=0.2).contains(&p10), "p10 {p10} out of expected band");
    }
}
