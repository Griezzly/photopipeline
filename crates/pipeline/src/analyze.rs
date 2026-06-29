//! Full-pipeline orchestration for the browser analyze flow: ingest → defects
//! → ML → calibrate → dedupe, with progress callbacks. The CLI keeps using the
//! individual phase functions; this is the one-call entry point for `serve`.

use std::path::Path;

use anyhow::Result;
use walkdir::WalkDir;

use crate::cache::Cache;
use crate::catalog::Catalog;
use crate::config::{Config, IngestConfig};
use crate::models::ModelHub;

/// Sink the orchestrator reports progress to. Implemented by the server's job
/// state. `Send + Sync` because `inc()` is called from rayon worker threads.
pub trait ProgressSink: Send + Sync {
    /// A coarse stage transition: "scanning" | "calibrating" | "deduping" | "done".
    fn stage(&self, stage: &str);
    /// Total files to ingest (set once, early in the scan stage).
    fn set_total(&self, total: u64);
    /// One file processed (called per ingested file).
    fn inc(&self);
}

/// Summary of a full analyze run.
#[derive(Debug, Clone)]
pub struct AnalyzeReport {
    pub ml_ran: bool,
    pub processed: u64,
    pub skipped: u64,
    pub errored: u64,
    pub groups: u64,
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

/// Run the full pipeline against `folder`'s library. Reports stage transitions
/// and per-file ingest progress through `progress`. Stamps `last_analyzed`.
pub fn analyze_folder(
    folder: &Path,
    catalog: &Catalog,
    cache: &Cache,
    hub: &ModelHub,
    cfg: &Config,
    progress: &dyn ProgressSink,
) -> Result<AnalyzeReport> {
    progress.stage("scanning");
    let ingest = crate::ingest::ingest_directory(
        std::slice::from_ref(&folder.to_path_buf()),
        catalog,
        cache,
        &cfg.ingest,
        Some(progress),
    )?;

    let _defects = crate::defect::analyze_defects(catalog, cache, hub, &cfg.defect)?;
    let _ml = crate::ml::analyze_ml(catalog, cache, hub, cfg.catalog.write_batch_size)?;

    progress.stage("calibrating");
    let _cal = crate::calibration::run_calibration(catalog, &cfg.defect)?;

    progress.stage("deduping");
    let dedupe = crate::dedupe::run_dedupe(catalog, &cfg.dedupe)?;

    catalog
        .set_last_analyzed(now_secs())
        .map_err(|e| anyhow::anyhow!("library_meta: {e}"))?;

    progress.stage("done");
    Ok(AnalyzeReport {
        ml_ran: !hub.is_empty(),
        processed: ingest.processed,
        skipped: ingest.skipped,
        errored: ingest.errored,
        groups: dedupe.groups,
    })
}

/// Count files under `folder` (by ingest extension) that the catalog reports as
/// new or changed — i.e. how much a re-analyze would process. Walk only; no decode.
pub fn count_pending(folder: &Path, catalog: &Catalog, cfg: &IngestConfig) -> Result<u64> {
    let mut pending = 0u64;
    for entry in WalkDir::new(folder)
        .follow_links(cfg.follow_symlinks)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        if !cfg.extensions.iter().any(|x| x.eq_ignore_ascii_case(ext)) {
            continue;
        }
        let Ok(meta) = std::fs::metadata(path) else {
            continue;
        };
        let mtime_ns = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_nanos() as i64)
            .unwrap_or(0);
        if catalog
            .needs_processing(path, mtime_ns, meta.len())
            .unwrap_or(true)
        {
            pending += 1;
        }
    }
    Ok(pending)
}
