//! Catalog access for the develop stage: `raw_stats` persistence and the
//! keeper work list. Split out of `catalog/mod.rs` to keep that file from
//! growing further; as a child module it still reaches `Catalog`'s private
//! connection.

use std::path::PathBuf;

use super::optional_row;
use super::Catalog;
use crate::develop::measure::RawStats;
use crate::error::CatalogError;

/// One photo the user kept, with everything `finish` needs to place its output.
#[derive(Debug, Clone)]
pub struct KeeperToDevelop {
    pub file_id: i64,
    pub path: PathBuf,
    pub content_hash: String,
    /// `YYYY-MM` from `captured_at`, or `"unknown-date"`.
    pub year_month: String,
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
                            'unknown-date') AS ym
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
}
