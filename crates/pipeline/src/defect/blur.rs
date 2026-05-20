use crate::config::BlurConfig;
use crate::defect::BBox;

#[derive(Debug, Clone, Copy)]
pub struct SharpnessResult {
    pub s_global: f32,
    pub s_subject: Option<f32>,
    pub s_background: Option<f32>,
    pub subject_ratio: Option<f32>,
    pub detector_used: &'static str,
}

/// Compute Laplacian-based sharpness metrics for an image.
///
/// Returns a [`SharpnessResult`] with global variance and optional
/// subject/background split based on provided ROIs or a center-crop fallback.
pub fn compute_sharpness(
    preview: &image::DynamicImage,
    subject_rois: Option<&[BBox]>,
    cfg: &BlurConfig,
) -> SharpnessResult {
    let gray = preview.to_luma8();
    let width = gray.width() as usize;
    let height = gray.height() as usize;

    // Degenerate images: return zeros.
    if width <= 2 || height <= 2 {
        return SharpnessResult {
            s_global: 0.0,
            s_subject: None,
            s_background: None,
            subject_ratio: None,
            detector_used: "center-crop-fallback",
        };
    }

    let raw = gray.as_raw();

    // Compute Laplacian for all interior pixels (skip 1-px border).
    // Kernel: [[0,1,0],[1,-4,1],[0,1,0]]
    // We accumulate the laplacian values and compute variance (Welford online).

    // Determine qualifying ROIs (area >= subject_min_area_ratio).
    let qualifying_rois: Option<Vec<&BBox>> = subject_rois.map(|rois| {
        rois.iter()
            .filter(|b| b.w * b.h >= cfg.subject_min_area_ratio)
            .collect()
    });

    let has_qualifying = qualifying_rois
        .as_ref()
        .map(|v| !v.is_empty())
        .unwrap_or(false);

    if has_qualifying {
        // ROI branch: single pass over interior pixels.
        let rois = qualifying_rois.as_ref().unwrap();

        let mut global_count: u64 = 0;
        let mut global_mean: f64 = 0.0;
        let mut global_m2: f64 = 0.0;

        let mut subj_count: u64 = 0;
        let mut subj_mean: f64 = 0.0;
        let mut subj_m2: f64 = 0.0;

        let mut bg_count: u64 = 0;
        let mut bg_mean: f64 = 0.0;
        let mut bg_m2: f64 = 0.0;

        let mut total_interior: u64 = 0;

        for y in 1..height - 1 {
            for x in 1..width - 1 {
                let px = |dy: isize, dx: isize| -> i32 {
                    raw[(y as isize + dy) as usize * width + (x as isize + dx) as usize] as i32
                };
                let lap = px(0, 0) * (-4) + px(-1, 0) + px(1, 0) + px(0, -1) + px(0, 1);
                let val = lap as f64;

                // Welford global.
                global_count += 1;
                let delta = val - global_mean;
                global_mean += delta / global_count as f64;
                let delta2 = val - global_mean;
                global_m2 += delta * delta2;

                total_interior += 1;

                // Subject vs background classification.
                let fx = x as f32 / width as f32;
                let fy = y as f32 / height as f32;
                let in_subject = rois
                    .iter()
                    .any(|b| fx >= b.x && fx <= b.x + b.w && fy >= b.y && fy <= b.y + b.h);

                if in_subject {
                    subj_count += 1;
                    let d = val - subj_mean;
                    subj_mean += d / subj_count as f64;
                    subj_m2 += d * (val - subj_mean);
                } else {
                    bg_count += 1;
                    let d = val - bg_mean;
                    bg_mean += d / bg_count as f64;
                    bg_m2 += d * (val - bg_mean);
                }
            }
        }

        let s_global = if global_count > 1 {
            (global_m2 / global_count as f64) as f32
        } else {
            0.0
        };
        let s_subject = if subj_count > 1 {
            Some((subj_m2 / subj_count as f64) as f32)
        } else {
            None
        };
        let s_background = if bg_count > 1 {
            Some((bg_m2 / bg_count as f64) as f32)
        } else {
            None
        };
        let subject_ratio = if total_interior > 0 {
            Some(subj_count as f32 / total_interior as f32)
        } else {
            None
        };

        SharpnessResult {
            s_global,
            s_subject,
            s_background,
            subject_ratio,
            detector_used: "rt-detr-l",
        }
    } else {
        // Fallback center-crop branch.
        let crop = cfg.fallback_center_crop;
        let x0 = ((width as f32 * (0.5 - crop / 2.0)) as usize)
            .max(1)
            .min(width - 1);
        let x1 = ((width as f32 * (0.5 + crop / 2.0)) as usize)
            .max(1)
            .min(width - 1);
        let y0 = ((height as f32 * (0.5 - crop / 2.0)) as usize)
            .max(1)
            .min(height - 1);
        let y1 = ((height as f32 * (0.5 + crop / 2.0)) as usize)
            .max(1)
            .min(height - 1);

        let mut global_count: u64 = 0;
        let mut global_mean: f64 = 0.0;
        let mut global_m2: f64 = 0.0;

        let mut subj_count: u64 = 0;
        let mut subj_mean: f64 = 0.0;
        let mut subj_m2: f64 = 0.0;

        let mut bg_count: u64 = 0;
        let mut bg_mean: f64 = 0.0;
        let mut bg_m2: f64 = 0.0;

        for y in 1..height - 1 {
            for x in 1..width - 1 {
                let px = |dy: isize, dx: isize| -> i32 {
                    raw[(y as isize + dy) as usize * width + (x as isize + dx) as usize] as i32
                };
                let lap = px(0, 0) * (-4) + px(-1, 0) + px(1, 0) + px(0, -1) + px(0, 1);
                let val = lap as f64;

                // Welford global.
                global_count += 1;
                let delta = val - global_mean;
                global_mean += delta / global_count as f64;
                let delta2 = val - global_mean;
                global_m2 += delta * delta2;

                // Subject: inside crop region.
                let in_subject = x >= x0 && x < x1 && y >= y0 && y < y1;
                if in_subject {
                    subj_count += 1;
                    let d = val - subj_mean;
                    subj_mean += d / subj_count as f64;
                    subj_m2 += d * (val - subj_mean);
                } else {
                    bg_count += 1;
                    let d = val - bg_mean;
                    bg_mean += d / bg_count as f64;
                    bg_m2 += d * (val - bg_mean);
                }
            }
        }

        let s_global = if global_count > 1 {
            (global_m2 / global_count as f64) as f32
        } else {
            0.0
        };
        let s_subject = if subj_count > 1 {
            Some((subj_m2 / subj_count as f64) as f32)
        } else {
            None
        };
        let s_background = if bg_count > 1 {
            Some((bg_m2 / bg_count as f64) as f32)
        } else {
            None
        };

        // subject_ratio is geometric (crop * crop), not pixel-count-based.
        let subject_ratio = Some(crop * crop);

        SharpnessResult {
            s_global,
            s_subject,
            s_background,
            subject_ratio,
            detector_used: "center-crop-fallback",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{DynamicImage, ImageBuffer, Luma, Rgb};

    fn default_cfg() -> BlurConfig {
        BlurConfig::default()
    }

    #[test]
    fn uniform_grey_variance_near_zero() {
        let img: ImageBuffer<Luma<u8>, _> = ImageBuffer::from_fn(64, 64, |_, _| Luma([128u8]));
        let dyn_img = DynamicImage::ImageLuma8(img);
        let result = compute_sharpness(&dyn_img, None, &default_cfg());
        assert!(
            result.s_global < 1.0,
            "expected s_global < 1.0 for uniform grey, got {}",
            result.s_global
        );
    }

    #[test]
    fn checkerboard_high_variance() {
        let img: ImageBuffer<Luma<u8>, _> = ImageBuffer::from_fn(64, 64, |x, y| {
            if (x + y) % 2 == 0 {
                Luma([0u8])
            } else {
                Luma([255u8])
            }
        });
        let dyn_img = DynamicImage::ImageLuma8(img);
        let result = compute_sharpness(&dyn_img, None, &default_cfg());
        assert!(
            result.s_global > 1000.0,
            "expected s_global > 1000.0 for checkerboard, got {}",
            result.s_global
        );
    }

    #[test]
    fn blurred_checkerboard_lower_than_sharp() {
        let sharp_img: ImageBuffer<Rgb<u8>, _> = ImageBuffer::from_fn(64, 64, |x, y| {
            if (x + y) % 2 == 0 {
                Rgb([0u8, 0, 0])
            } else {
                Rgb([255u8, 255, 255])
            }
        });
        let sharp_dyn = DynamicImage::ImageRgb8(sharp_img);
        let blurred_dyn = DynamicImage::ImageRgb8(image::imageops::blur(&sharp_dyn.to_rgb8(), 3.0));

        let sharp_result = compute_sharpness(&sharp_dyn, None, &default_cfg());
        let blurred_result = compute_sharpness(&blurred_dyn, None, &default_cfg());

        assert!(
            blurred_result.s_global < sharp_result.s_global / 4.0,
            "blurred ({}) should be < sharp ({}) / 4",
            blurred_result.s_global,
            sharp_result.s_global
        );
    }

    #[test]
    fn center_crop_ratio_matches_config() {
        // 100x100 gradient image.
        let img: ImageBuffer<Rgb<u8>, _> = ImageBuffer::from_fn(100, 100, |x, y| {
            Rgb([(x % 256) as u8, (y % 256) as u8, 128u8])
        });
        let dyn_img = DynamicImage::ImageRgb8(img);
        let cfg = default_cfg();
        let result = compute_sharpness(&dyn_img, None, &cfg);
        let expected = cfg.fallback_center_crop * cfg.fallback_center_crop;
        let actual = result.subject_ratio.expect("subject_ratio should be Some");
        assert!(
            (actual - expected).abs() < 0.01,
            "subject_ratio {} should be close to {} (crop^2)",
            actual,
            expected
        );
    }
}
