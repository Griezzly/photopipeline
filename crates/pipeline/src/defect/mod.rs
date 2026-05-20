/// Axis-aligned bounding box, normalized to [0.0, 1.0].
/// Phase 3 (subject detection) will populate it; Phase 2 passes None.
#[derive(Debug, Clone, Copy)]
pub struct BBox {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

pub mod blur;
pub mod exposure;

pub use blur::{compute_sharpness, SharpnessResult};
pub use exposure::{compute_exposure, ExposureResult};

pub struct DefectFlag {
    pub flag_type: String,
    pub confidence: f32,
    pub reason: String,
}

pub struct DefectRow {
    pub file_id: i64,
    pub sharpness: SharpnessResult,
    pub exposure: ExposureResult,
    pub flags: Vec<DefectFlag>,
}

#[derive(Debug, Default)]
pub struct DefectReport {
    pub analyzed: u64,
    pub skipped: u64,
    pub errored: u64,
    pub flagged_overexposed: u64,
    pub flagged_underexposed: u64,
}

pub fn analyze_defects(
    catalog: &crate::catalog::Catalog,
    cache: &crate::cache::Cache,
    cfg: &crate::config::DefectConfig,
) -> anyhow::Result<DefectReport> {
    use rayon::prelude::*;
    use std::sync::{
        atomic::{AtomicU64, Ordering},
        Mutex,
    };

    let needing = catalog.files_needing_defect_analysis()?;

    let analyzed = AtomicU64::new(0);
    let errored = AtomicU64::new(0);
    let flagged_overexposed = AtomicU64::new(0);
    let flagged_underexposed = AtomicU64::new(0);

    let batch: Mutex<Vec<DefectRow>> = Mutex::new(Vec::new());

    needing.par_iter().for_each(|(file_id, path, hash)| {
        let preview_path = cache.path(*hash);

        if !preview_path.exists() {
            tracing::warn!(path = %path.display(), "preview not found in cache, skipping defect analysis");
            errored.fetch_add(1, Ordering::Relaxed);
            return;
        }

        let img = match image::open(&preview_path) {
            Ok(img) => img,
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "failed to open preview for defect analysis");
                errored.fetch_add(1, Ordering::Relaxed);
                return;
            }
        };

        let sharpness = compute_sharpness(&img, None, &cfg.blur);
        let exposure = compute_exposure(&img);

        let mut flags = Vec::new();

        let clipped = exposure.clipped_highlights;
        if clipped > cfg.exposure.clipped_highlights_threshold {
            let confidence = (clipped / cfg.exposure.clipped_highlights_threshold).min(1.0);
            flags.push(DefectFlag {
                flag_type: "overexposed".to_string(),
                confidence,
                reason: format!("{:.1}% pixels at ≥ 0.99", clipped * 100.0),
            });
            flagged_overexposed.fetch_add(1, Ordering::Relaxed);
        }

        let shadows = exposure.clipped_shadows;
        if shadows > cfg.exposure.clipped_shadows_threshold {
            let confidence = (shadows / cfg.exposure.clipped_shadows_threshold).min(1.0);
            flags.push(DefectFlag {
                flag_type: "underexposed".to_string(),
                confidence,
                reason: format!("{:.1}% pixels at ≤ 0.01", shadows * 100.0),
            });
            flagged_underexposed.fetch_add(1, Ordering::Relaxed);
        }

        analyzed.fetch_add(1, Ordering::Relaxed);

        let row = DefectRow {
            file_id: *file_id,
            sharpness,
            exposure,
            flags,
        };

        let mut locked = batch.lock().expect("batch mutex poisoned");
        locked.push(row);
        if locked.len() >= 64 {
            let to_flush: Vec<DefectRow> = std::mem::take(&mut *locked);
            drop(locked);
            let n = to_flush.len() as u64;
            if let Err(e) = catalog.flush_defect_batch(&to_flush) {
                tracing::warn!(error = %e, "failed to flush defect batch");
                analyzed.fetch_sub(n, Ordering::Relaxed);
                errored.fetch_add(n, Ordering::Relaxed);
            }
        }
    });

    // Final flush of remaining rows.
    let remaining: Vec<DefectRow> =
        std::mem::take(&mut *batch.lock().expect("batch mutex poisoned"));
    if !remaining.is_empty() {
        let n = remaining.len() as u64;
        if let Err(e) = catalog.flush_defect_batch(&remaining) {
            tracing::warn!(error = %e, "failed to flush final defect batch");
            analyzed.fetch_sub(n, Ordering::Relaxed);
            errored.fetch_add(n, Ordering::Relaxed);
        }
    }

    Ok(DefectReport {
        analyzed: analyzed.load(Ordering::Relaxed),
        skipped: 0,
        errored: errored.load(Ordering::Relaxed),
        flagged_overexposed: flagged_overexposed.load(Ordering::Relaxed),
        flagged_underexposed: flagged_underexposed.load(Ordering::Relaxed),
    })
}
