//! Full-pipeline orchestration for the browser analyze flow: ingest → defects
//! → ML → calibrate → dedupe, with progress callbacks. The CLI keeps using the
//! individual phase functions; this is the one-call entry point for `serve`.

use std::path::Path;

use anyhow::Result;

use crate::cache::Cache;
use crate::catalog::Catalog;
use crate::config::{Config, IngestConfig};
use crate::models::ModelHub;

/// Sink the orchestrator reports progress to. Implemented by the server's job
/// state. `Send + Sync` because `inc()` is called from rayon worker threads.
///
/// Each heavy phase reports its own progress: it calls `stage()` (which resets
/// the per-phase counter), then `set_total()` with that phase's item count, then
/// `inc()` once per item. So the bar runs 0→100% *within every phase* rather than
/// filling once during the scan and sitting at 100% while later phases work.
pub trait ProgressSink: Send + Sync {
    /// Begin a new phase (e.g. "scanning", "scoring quality", "done"). Resets the
    /// per-phase file counter so the next `set_total`/`inc` calls start from zero.
    fn stage(&self, stage: &str);
    /// Total items in the current phase. Set once, right after `stage()`.
    fn set_total(&self, total: u64);
    /// One item processed in the current phase.
    fn inc(&self);
    /// Where the current *item* has got to, within one counted phase.
    ///
    /// `stage()` cannot express this: it resets the counter, so a phase that
    /// announced a sub-phase per item would wipe its own "N of M" every time.
    /// A phase like `finish`'s — one photo taking minutes, four internal steps
    /// each worth showing — needs a channel that does not disturb the count.
    /// `item` is a display label for what is being worked on (a filename), not
    /// an identifier.
    ///
    /// Defaulted to a no-op: phases that finish an item in milliseconds have
    /// nothing useful to report here, and neither do sinks that only draw a bar.
    fn step(&self, _step: &str, _item: &str) {}
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

    progress.stage("detecting defects");
    let _defects =
        crate::defect::analyze_defects(catalog, cache, hub, &cfg.defect, Some(progress))?;

    progress.stage("scoring quality");
    let _ml = crate::ml::analyze_ml(
        catalog,
        cache,
        hub,
        cfg.catalog.write_batch_size,
        Some(progress),
    )?;

    progress.stage("calibrating");
    let _cal = crate::calibration::run_calibration(catalog, &cfg.defect)?;

    progress.stage("grouping duplicates");
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
///
/// Shares `collect_ingestable` with the ingest walk rather than reimplementing
/// it, and applies the same sidecar-JPG exclusion. When the two walks disagreed
/// the count could never reach zero, and the UI reported "N new photos" forever
/// (BE-1); sharing the function is what stops that recurring.
pub fn count_pending(folder: &Path, catalog: &Catalog, cfg: &IngestConfig) -> Result<u64> {
    let candidates = crate::ingest::collect_ingestable(folder, cfg);

    let mut pending = 0u64;
    for path in crate::ingest::exclude_sidecar_jpgs(candidates) {
        let path = path.as_path();
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
