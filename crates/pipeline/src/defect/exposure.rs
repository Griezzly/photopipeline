#[derive(Debug, Clone, Copy)]
pub struct ExposureResult {
    pub clipped_highlights: f32,
    pub clipped_shadows: f32,
    pub mean_luma: f32,
    pub histogram_skew: f32,
}

/// Compute histogram-based exposure metrics for an image.
pub fn compute_exposure(preview: &image::DynamicImage) -> ExposureResult {
    let rgb = preview.to_rgb8();
    let pixels = rgb.as_raw();
    let total_pixels = (rgb.width() * rgb.height()) as usize;

    if total_pixels == 0 {
        return ExposureResult {
            clipped_highlights: 0.0,
            clipped_shadows: 0.0,
            mean_luma: 0.0,
            histogram_skew: 0.0,
        };
    }

    // Build luminance histogram using Rec.709 coefficients.
    let mut histogram = [0u64; 256];
    for chunk in pixels.chunks_exact(3) {
        let r = chunk[0] as f32;
        let g = chunk[1] as f32;
        let b = chunk[2] as f32;
        let y = 0.2126 * r + 0.7152 * g + 0.0722 * b;
        let bin = y.round().clamp(0.0, 255.0) as usize;
        histogram[bin] += 1;
    }

    let total = total_pixels as f64;

    // Clipped highlights: bins 253, 254, 255.
    let clipped_highlights =
        ((histogram[253] + histogram[254] + histogram[255]) as f64 / total) as f32;

    // Clipped shadows: bins 0, 1, 2.
    let clipped_shadows = ((histogram[0] + histogram[1] + histogram[2]) as f64 / total) as f32;

    // Mean luminance (in 0..255 space).
    let mu: f64 = histogram
        .iter()
        .enumerate()
        .map(|(i, &count)| i as f64 * count as f64)
        .sum::<f64>()
        / total;

    let mean_luma = (mu / 255.0) as f32;

    // Variance and standard deviation.
    let variance: f64 = histogram
        .iter()
        .enumerate()
        .map(|(i, &count)| {
            let diff = i as f64 - mu;
            diff * diff * count as f64
        })
        .sum::<f64>()
        / total;
    let sigma = variance.sqrt();

    // Histogram skew (Pearson's moment coefficient of skewness).
    let histogram_skew: f32 = if sigma > 1e-6 {
        let skew: f64 = histogram
            .iter()
            .enumerate()
            .map(|(i, &count)| {
                let z = (i as f64 - mu) / sigma;
                z * z * z * count as f64
            })
            .sum::<f64>()
            / total;
        skew as f32
    } else {
        0.0
    };

    ExposureResult {
        clipped_highlights,
        clipped_shadows,
        mean_luma,
        histogram_skew,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{DynamicImage, ImageBuffer, Rgb};

    #[test]
    fn pure_white() {
        let img: ImageBuffer<Rgb<u8>, _> =
            ImageBuffer::from_fn(64, 64, |_, _| Rgb([255u8, 255, 255]));
        let dyn_img = DynamicImage::ImageRgb8(img);
        let result = compute_exposure(&dyn_img);
        assert!(
            result.clipped_highlights > 0.99,
            "expected clipped_highlights > 0.99, got {}",
            result.clipped_highlights
        );
        assert!(
            result.clipped_shadows < 0.01,
            "expected clipped_shadows < 0.01, got {}",
            result.clipped_shadows
        );
        assert!(
            result.mean_luma > 0.99,
            "expected mean_luma > 0.99, got {}",
            result.mean_luma
        );
    }

    #[test]
    fn pure_black() {
        let img: ImageBuffer<Rgb<u8>, _> = ImageBuffer::from_fn(64, 64, |_, _| Rgb([0u8, 0, 0]));
        let dyn_img = DynamicImage::ImageRgb8(img);
        let result = compute_exposure(&dyn_img);
        assert!(
            result.clipped_shadows > 0.99,
            "expected clipped_shadows > 0.99, got {}",
            result.clipped_shadows
        );
        assert!(
            result.clipped_highlights < 0.01,
            "expected clipped_highlights < 0.01, got {}",
            result.clipped_highlights
        );
        assert!(
            result.mean_luma < 0.01,
            "expected mean_luma < 0.01, got {}",
            result.mean_luma
        );
    }

    #[test]
    fn mid_grey() {
        let img: ImageBuffer<Rgb<u8>, _> =
            ImageBuffer::from_fn(64, 64, |_, _| Rgb([128u8, 128, 128]));
        let dyn_img = DynamicImage::ImageRgb8(img);
        let result = compute_exposure(&dyn_img);
        assert!(
            result.clipped_highlights < 0.01,
            "expected clipped_highlights < 0.01, got {}",
            result.clipped_highlights
        );
        assert!(
            result.clipped_shadows < 0.01,
            "expected clipped_shadows < 0.01, got {}",
            result.clipped_shadows
        );
        assert!(
            (0.48..=0.52).contains(&result.mean_luma),
            "expected mean_luma in 0.48..0.52, got {}",
            result.mean_luma
        );
    }
}
