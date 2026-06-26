//! Lens calibration: rebuild per-lens sharpness baselines and re-flag
//! blur / back_focus / low_iqa defects. Driven by `photopipe calibrate`.

pub mod buckets;

use crate::catalog::{BlurFlagRow, Catalog};
use crate::config::DefectConfig;

#[derive(Debug, Default)]
pub struct CalibrationReport {
    pub buckets_built: usize,
    pub global_n_samples: usize,
    pub flags_cleared: usize,
    pub flagged_blur: usize,
    pub flagged_back_focus: usize,
    pub flagged_low_iqa: usize,
    pub blur_confidence_bumped: usize,
}

/// Atomic-in-intent calibration: (1) clear stale blur-related flags,
/// (2) rebuild baselines, (3) reflag every file with sharpness data.
/// `overexposed` / `underexposed` flags are never touched.
pub fn run_calibration(catalog: &Catalog, cfg: &DefectConfig) -> anyhow::Result<CalibrationReport> {
    let min_samples = cfg.blur.min_samples_for_bucket;

    // (1) wipe stale blur/back_focus/low_iqa.
    let flags_cleared = catalog.clear_blur_related_flags()?;

    // (2) rebuild baselines.
    let rebuild = catalog.rebuild_sharpness_baselines(min_samples)?;

    // (3) global fallbacks computed once.
    let iqa_p10 = if cfg.blur.iqa_second_opinion {
        catalog.iqa_global_p10()?
    } else {
        None
    };
    let global_s_p10 = global_sharpness_p10(catalog)?;

    let mut report = CalibrationReport {
        buckets_built: rebuild.buckets_built,
        global_n_samples: rebuild.global_n_samples,
        flags_cleared,
        ..Default::default()
    };

    let rows = catalog.iter_sharpness_for_reflag()?;
    let mut batch: Vec<BlurFlagRow> = Vec::with_capacity(64);

    for row in &rows {
        let s_subject = match row.s_subject {
            Some(s) => s,
            None => continue, // degenerate; no flag.
        };

        // Resolve threshold: per-bucket p10 (if bucket has enough samples) else global.
        let bucket_p10 = match (
            row.camera_model.as_deref(),
            row.lens_model.as_deref(),
            row.focal_length_mm,
            row.aperture,
        ) {
            (Some(cam), Some(lens), Some(focal), Some(ap)) => catalog.bucket_baseline_p10(
                cam,
                lens,
                buckets::focal_bucket(focal),
                buckets::aperture_bucket(ap),
                min_samples,
            )?,
            _ => None,
        };
        let threshold = match bucket_p10.or(global_s_p10) {
            Some(t) => t,
            None => {
                tracing::debug!(
                    file_id = row.file_id,
                    "no baseline available, skipping reflag"
                );
                continue;
            }
        };

        let mut flagged_blur = false;

        if s_subject < threshold {
            let confidence = ((threshold - s_subject) / threshold).clamp(0.01, 1.0);
            let s_bg = row.s_background.unwrap_or(s_subject);
            if s_bg > s_subject * 2.0 {
                batch.push(BlurFlagRow {
                    file_id: row.file_id,
                    flag_type: "back_focus",
                    confidence,
                    reason: format!(
                        "subject {:.1} < p10 {:.1}, background {:.1}x sharper",
                        s_subject,
                        threshold,
                        s_bg / s_subject
                    ),
                });
                report.flagged_back_focus += 1;
            } else {
                batch.push(BlurFlagRow {
                    file_id: row.file_id,
                    flag_type: "blur",
                    confidence,
                    reason: format!("subject {:.1} < p10 {:.1}", s_subject, threshold),
                });
                report.flagged_blur += 1;
                flagged_blur = true;
            }
        }

        // IQA second opinion: independent of subject sharpness.
        let mut flagged_low_iqa = false;
        if let (Some(iqa_p10), Some(score)) = (iqa_p10, row.iqa_score) {
            if score < iqa_p10 {
                let confidence = ((iqa_p10 - score) / iqa_p10).clamp(0.01, 1.0);
                batch.push(BlurFlagRow {
                    file_id: row.file_id,
                    flag_type: "low_iqa",
                    confidence,
                    reason: format!("iqa {:.2} < global p10 {:.2}", score, iqa_p10),
                });
                report.flagged_low_iqa += 1;
                flagged_low_iqa = true;
            }
        }

        // Both blur AND low_iqa → bump the just-pushed blur row's confidence by 0.2 (cap 1.0).
        if flagged_blur && flagged_low_iqa {
            // The blur row is the most recent "blur" entry we pushed for this file.
            if let Some(blur_row) = batch
                .iter_mut()
                .rev()
                .find(|f| f.file_id == row.file_id && f.flag_type == "blur")
            {
                blur_row.confidence = (blur_row.confidence + 0.2).min(1.0);
                blur_row.reason = format!("{} (confirmed by low IQA)", blur_row.reason);
                report.blur_confidence_bumped += 1;
            }
        }

        if batch.len() >= 64 {
            let to_flush = std::mem::take(&mut batch);
            catalog.flush_blur_flag_batch(&to_flush)?;
        }
    }

    if !batch.is_empty() {
        catalog.flush_blur_flag_batch(&batch)?;
    }

    tracing::info!(
        buckets = report.buckets_built,
        cleared = report.flags_cleared,
        blur = report.flagged_blur,
        back_focus = report.flagged_back_focus,
        low_iqa = report.flagged_low_iqa,
        "calibration complete"
    );

    Ok(report)
}

/// Global p10 of `s_subject` across all sharpness rows (the global fallback
/// threshold). Reads the sentinel baseline row written by
/// `rebuild_sharpness_baselines`. `None` when no sharpness data exists.
fn global_sharpness_p10(catalog: &Catalog) -> anyhow::Result<Option<f32>> {
    // The sentinel row is ('*','*',0,0.0); ask for it with a huge min_samples=0.
    Ok(catalog.bucket_baseline_p10("*", "*", 0, 0.0, 0)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{BlurConfig, DefectConfig};

    fn blur_cfg(iqa: bool, min_samples: usize) -> DefectConfig {
        DefectConfig {
            blur: BlurConfig {
                iqa_second_opinion: iqa,
                min_samples_for_bucket: min_samples,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn report_default_is_all_zeros() {
        let r = CalibrationReport::default();
        assert_eq!(r.buckets_built, 0);
        assert_eq!(r.global_n_samples, 0);
        assert_eq!(r.flags_cleared, 0);
        assert_eq!(r.flagged_blur, 0);
        assert_eq!(r.flagged_back_focus, 0);
        assert_eq!(r.flagged_low_iqa, 0);
        assert_eq!(r.blur_confidence_bumped, 0);
    }

    #[test]
    fn run_calibration_empty_catalog_returns_zeroed_report() {
        let catalog = crate::catalog::Catalog::open(std::path::Path::new(":memory:")).unwrap();
        let cfg = blur_cfg(false, 3);
        let report = run_calibration(&catalog, &cfg).unwrap();
        assert_eq!(report.flags_cleared, 0);
        assert_eq!(report.buckets_built, 0);
        assert_eq!(report.flagged_blur, 0);
        assert_eq!(report.flagged_back_focus, 0);
        assert_eq!(report.flagged_low_iqa, 0);
        assert_eq!(report.blur_confidence_bumped, 0);
    }
}
