//! Catalog access for the develop stage: `raw_stats` persistence and the
//! keeper work list. Split out of `catalog/mod.rs` to keep that file from
//! growing further; as a child module it still reaches `Catalog`'s private
//! connection.

use std::path::PathBuf;

use super::optional_row;
use super::Catalog;
use crate::develop::decide::EditRecipe;
use crate::develop::measure::RawStats;
use crate::error::CatalogError;
use crate::ingest::exif::ExifData;

/// One photo the user kept, with everything `finish` needs to place its output.
#[derive(Debug, Clone)]
pub struct KeeperToDevelop {
    pub file_id: i64,
    pub path: PathBuf,
    pub content_hash: String,
    /// `YYYY-MM` from `captured_at`, or `"unknown-date"`.
    pub year_month: String,
    /// Lowercase extension as recorded at ingest (e.g. `"arw"`, `"jpg"`).
    /// `keepers_to_develop()` applies no format filter — `jpg`/`jpeg` are
    /// supported ingest formats too — so callers must check this before
    /// treating the file as a raw.
    pub file_format: String,
}

impl Catalog {
    /// Every photo with `verdict = 'keep'`.
    ///
    /// Deliberately NOT `is_keeper`: that column is written only by
    /// `pick_keeper()` and means "best shot of a duplicate group", so a photo
    /// with no duplicates would never be developed (spec A8).
    pub fn keepers_to_develop(&self) -> Result<Vec<KeeperToDevelop>, CatalogError> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| CatalogError::Db("mutex poisoned".into()))?;
        let mut stmt = conn
            .prepare(
                "SELECT f.id, f.path, f.content_hash,
                        COALESCE(
                            strftime(CAST(to_timestamp(e.captured_at) AS TIMESTAMP), '%Y-%m'),
                            'unknown-date') AS ym,
                        f.file_format
                 FROM decisions dec
                 JOIN files f ON f.id = dec.file_id
                 LEFT JOIN exif e ON e.file_id = f.id
                 WHERE dec.verdict = 'keep'
                 ORDER BY f.path",
            )
            .map_err(|e| CatalogError::Db(e.to_string()))?;
        let rows = stmt
            .query_map([], |r| {
                Ok(KeeperToDevelop {
                    file_id: r.get(0)?,
                    path: PathBuf::from(r.get::<_, String>(1)?),
                    content_hash: r.get(2)?,
                    year_month: r.get(3)?,
                    file_format: r.get(4)?,
                })
            })
            .map_err(|e| CatalogError::Db(e.to_string()))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| CatalogError::Db(e.to_string()))?);
        }
        Ok(out)
    }

    /// Insert or replace the raw-linear statistics for one file.
    pub fn upsert_raw_stats(&self, file_id: i64, s: &RawStats) -> Result<(), CatalogError> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| CatalogError::Db("mutex poisoned".into()))?;
        conn.execute(
            "INSERT INTO raw_stats
                (file_id, p1, p50, p99, p999, clipped_frac, black_frac,
                 wb_r, wb_g, wb_b, illum_r, illum_g, illum_b)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT (file_id) DO UPDATE SET
                 p1 = excluded.p1, p50 = excluded.p50, p99 = excluded.p99,
                 p999 = excluded.p999,
                 clipped_frac = excluded.clipped_frac, black_frac = excluded.black_frac,
                 wb_r = excluded.wb_r, wb_g = excluded.wb_g, wb_b = excluded.wb_b,
                 illum_r = excluded.illum_r, illum_g = excluded.illum_g,
                 illum_b = excluded.illum_b",
            duckdb::params![
                file_id,
                s.p1,
                s.p50,
                s.p99,
                s.p999,
                s.clipped_frac,
                s.black_frac,
                s.wb_r,
                s.wb_g,
                s.wb_b,
                s.illum_r,
                s.illum_g,
                s.illum_b
            ],
        )
        .map_err(|e| CatalogError::Db(e.to_string()))?;
        Ok(())
    }

    pub fn get_raw_stats(&self, file_id: i64) -> Result<Option<RawStats>, CatalogError> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| CatalogError::Db("mutex poisoned".into()))?;
        let row = conn.query_row(
            "SELECT p1, p50, p99, p999, clipped_frac, black_frac, wb_r, wb_g, wb_b,
                    illum_r, illum_g, illum_b
             FROM raw_stats WHERE file_id = ?",
            duckdb::params![file_id],
            |r| {
                Ok(RawStats {
                    p1: r.get(0)?,
                    p50: r.get(1)?,
                    p99: r.get(2)?,
                    p999: r.get(3)?,
                    clipped_frac: r.get(4)?,
                    black_frac: r.get(5)?,
                    wb_r: r.get(6)?,
                    wb_g: r.get(7)?,
                    wb_b: r.get(8)?,
                    illum_r: r.get(9)?,
                    illum_g: r.get(10)?,
                    illum_b: r.get(11)?,
                })
            },
        );
        optional_row(row)
    }

    /// EXIF plus subject sharpness for one file — the non-raw inputs to
    /// `decide()`. Missing rows are not an error: `decide()` has a defined
    /// answer for absent ISO and absent sharpness.
    ///
    /// Returns `s_subject` rather than `s_global` because the sharpening
    /// decision places it against `sharpness_baseline`, whose percentiles are
    /// computed over `s_subject`. `None` means "no comparison possible", which
    /// the caller turns into `NEUTRAL_RELATIVE_SHARPNESS`; it is not the same as
    /// zero, which would mean "measured, and as soft as this lens ever gets".
    ///
    /// `focal_length_mm` and `aperture` are selected because they key the
    /// baseline bucket, alongside camera and lens.
    pub fn develop_inputs(&self, file_id: i64) -> Result<(ExifData, Option<f32>), CatalogError> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| CatalogError::Db("mutex poisoned".into()))?;
        let row = conn.query_row(
            "SELECT e.iso, e.lens_model, e.camera_model, s.s_subject,
                    e.focal_length_mm, e.aperture
             FROM files f
             LEFT JOIN exif e ON e.file_id = f.id
             LEFT JOIN sharpness s ON s.file_id = f.id
             WHERE f.id = ?",
            duckdb::params![file_id],
            |r| {
                let iso: Option<i32> = r.get(0)?;
                let lens_model: Option<String> = r.get(1)?;
                let camera_model: Option<String> = r.get(2)?;
                let s_subject: Option<f32> = r.get(3)?;
                let focal_length_mm: Option<f32> = r.get(4)?;
                let aperture: Option<f32> = r.get(5)?;
                Ok((
                    ExifData {
                        iso: iso.map(|v| v as u32),
                        lens_model,
                        camera_model,
                        focal_length_mm,
                        aperture,
                        ..Default::default()
                    },
                    s_subject,
                ))
            },
        );
        Ok(optional_row(row)?.unwrap_or((ExifData::default(), None)))
    }
}

/// A complete `edits` row. Doubles as the audit record: for any finished JPEG
/// it answers which recipe and model version produced it and whether the look
/// survived the quality guard.
#[derive(Debug, Clone)]
pub struct EditRow {
    pub file_id: i64,
    /// Denormalised from `files` so the row survives moves and renames.
    pub content_hash: String,
    pub recipe: EditRecipe,
    pub recipe_hash: String,
    pub decider_version: String,
    pub renderer: String,
    pub look_model: Option<String>,
    pub look_version: Option<String>,
    /// Retained even when `look_applied` is false, so a rejected look is still
    /// reproducible for inspection.
    pub lut_hash: Option<String>,
    pub look_applied: bool,
    pub iqa_before: Option<f32>,
    pub iqa_after: Option<f32>,
    pub output_path: Option<String>,
    pub output_size_bytes: Option<i64>,
    pub rendered_at: i64,
}

/// The subset of an `edits` row that decides whether a re-render is needed.
#[derive(Debug, Clone, PartialEq)]
pub struct EditIdentity {
    pub content_hash: String,
    pub recipe_hash: String,
    pub decider_version: String,
    pub renderer: String,
    pub look_model: Option<String>,
    pub look_version: Option<String>,
}

impl Catalog {
    pub fn upsert_edit(&self, row: &EditRow) -> Result<(), CatalogError> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| CatalogError::Db("mutex poisoned".into()))?;
        let r = &row.recipe;
        conn.execute(
            "INSERT INTO edits
                (file_id, content_hash, exposure_ev,
                 highlight_recovery, shadow_lift, denoise_luma, denoise_chroma,
                 sharpen_amount, lens_correct, recipe_hash, decider_version,
                 renderer, look_model, look_version, lut_hash, look_applied,
                 iqa_before, iqa_after, output_path, output_size_bytes, rendered_at)
             VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)
             ON CONFLICT (file_id) DO UPDATE SET
                 content_hash = excluded.content_hash,
                 exposure_ev = excluded.exposure_ev,
                 highlight_recovery = excluded.highlight_recovery,
                 shadow_lift = excluded.shadow_lift,
                 denoise_luma = excluded.denoise_luma,
                 denoise_chroma = excluded.denoise_chroma,
                 sharpen_amount = excluded.sharpen_amount,
                 lens_correct = excluded.lens_correct,
                 recipe_hash = excluded.recipe_hash,
                 decider_version = excluded.decider_version,
                 renderer = excluded.renderer,
                 look_model = excluded.look_model,
                 look_version = excluded.look_version,
                 lut_hash = excluded.lut_hash,
                 look_applied = excluded.look_applied,
                 iqa_before = excluded.iqa_before,
                 iqa_after = excluded.iqa_after,
                 output_path = excluded.output_path,
                 output_size_bytes = excluded.output_size_bytes,
                 rendered_at = excluded.rendered_at",
            duckdb::params![
                row.file_id,
                row.content_hash,
                r.exposure_ev,
                r.highlight_recovery,
                r.shadow_lift,
                r.denoise_luma,
                r.denoise_chroma,
                r.sharpen_amount,
                r.lens_correct,
                row.recipe_hash,
                row.decider_version,
                row.renderer,
                row.look_model,
                row.look_version,
                row.lut_hash,
                row.look_applied,
                row.iqa_before,
                row.iqa_after,
                row.output_path,
                row.output_size_bytes,
                row.rendered_at
            ],
        )
        .map_err(|e| CatalogError::Db(e.to_string()))?;
        Ok(())
    }

    /// The identity of the last render plus where it landed and how big it was.
    #[allow(clippy::type_complexity)]
    pub fn edit_identity(
        &self,
        file_id: i64,
    ) -> Result<Option<(EditIdentity, Option<String>, Option<i64>)>, CatalogError> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| CatalogError::Db("mutex poisoned".into()))?;
        let row = conn.query_row(
            "SELECT content_hash, recipe_hash, decider_version, renderer,
                    look_model, look_version, output_path, output_size_bytes
             FROM edits WHERE file_id = ?",
            duckdb::params![file_id],
            |r| {
                Ok((
                    EditIdentity {
                        content_hash: r.get(0)?,
                        recipe_hash: r.get(1)?,
                        decider_version: r.get(2)?,
                        renderer: r.get(3)?,
                        look_model: r.get(4)?,
                        look_version: r.get(5)?,
                    },
                    r.get(6)?,
                    r.get(7)?,
                ))
            },
        );
        optional_row(row)
    }

    /// Every `edits` row whose photo is no longer a keeper, as
    /// `(file_id, output_path)`.
    ///
    /// These are the rows `finish` orphaned by a verdict flipping away from
    /// `keep`: the JPEG and `.pp3` stay on disk and the row keeps pointing at
    /// them, so the finished tree only ever grows. Mirrors the `verdict = 'keep'`
    /// test in `keepers_to_develop`, so the two can never disagree about what
    /// belongs in the tree.
    pub fn orphaned_edits(&self) -> Result<Vec<(i64, Option<String>)>, CatalogError> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| CatalogError::Db("mutex poisoned".into()))?;
        let mut stmt = conn
            .prepare(
                "SELECT e.file_id, e.output_path
                 FROM edits e
                 LEFT JOIN decisions d ON d.file_id = e.file_id
                 WHERE d.verdict IS DISTINCT FROM 'keep'",
            )
            .map_err(|e| CatalogError::Db(e.to_string()))?;
        let rows = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .map_err(|e| CatalogError::Db(e.to_string()))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| CatalogError::Db(e.to_string()))?);
        }
        Ok(out)
    }

    /// Every non-null `edits.output_path`. Lets `finish` recognise a finished
    /// tree it wrote during a release that predates the `.photopipe-tree`
    /// marker, instead of refusing to prune it forever.
    pub fn all_edit_outputs(&self) -> Result<Vec<String>, CatalogError> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| CatalogError::Db("mutex poisoned".into()))?;
        let mut stmt = conn
            .prepare("SELECT output_path FROM edits WHERE output_path IS NOT NULL")
            .map_err(|e| CatalogError::Db(e.to_string()))?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| CatalogError::Db(e.to_string()))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| CatalogError::Db(e.to_string()))?);
        }
        Ok(out)
    }

    /// Drop `edits` rows by `file_id`. Used by `finish`'s prune pass once the
    /// corresponding outputs are off disk, so a row never outlives its JPEG.
    pub fn delete_edits(&self, file_ids: &[i64]) -> Result<usize, CatalogError> {
        if file_ids.is_empty() {
            return Ok(0);
        }
        let mut conn = self
            .conn
            .lock()
            .map_err(|_| CatalogError::Db("mutex poisoned".into()))?;
        let tx = conn
            .transaction()
            .map_err(|e| CatalogError::Db(e.to_string()))?;
        let mut removed = 0usize;
        {
            // One parameterised DELETE per id rather than an `IN (…)` list:
            // binding a list is the DuckDB footgun this project has hit before,
            // and the prune set is small by construction.
            let mut stmt = tx
                .prepare("DELETE FROM edits WHERE file_id = ?")
                .map_err(|e| CatalogError::Db(e.to_string()))?;
            for id in file_ids {
                removed += stmt
                    .execute(duckdb::params![id])
                    .map_err(|e| CatalogError::Db(e.to_string()))?;
            }
        }
        tx.commit().map_err(|e| CatalogError::Db(e.to_string()))?;
        Ok(removed)
    }
}
