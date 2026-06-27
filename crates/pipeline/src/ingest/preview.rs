use std::path::Path;

use image::{imageops::FilterType, DynamicImage};

/// Extract a preview image from a RAW file using rawler.
///
/// Falls back to the thumbnail if no full preview is available.
pub fn extract_preview_raw(
    path: &Path,
    max_long_edge: u32,
) -> Result<DynamicImage, crate::error::IngestError> {
    use rawler::{decoders::RawDecodeParams, rawsource::RawSource};

    let raw_source = RawSource::new(path).map_err(|e| crate::error::IngestError::Preview {
        path: path.to_owned(),
        reason: e.to_string(),
    })?;
    let decoder =
        rawler::get_decoder(&raw_source).map_err(|e| crate::error::IngestError::Preview {
            path: path.to_owned(),
            reason: e.to_string(),
        })?;
    let params = RawDecodeParams::default();

    // Try full preview first, fall back to thumbnail.
    let img = decoder
        .preview_image(&raw_source, &params)
        .map_err(|e| crate::error::IngestError::Preview {
            path: path.to_owned(),
            reason: e.to_string(),
        })?
        .or_else(|| {
            decoder
                .thumbnail_image(&raw_source, &params)
                .unwrap_or(None)
        })
        .ok_or_else(|| crate::error::IngestError::Preview {
            path: path.to_owned(),
            reason: "no preview or thumbnail available".into(),
        })?;

    Ok(resize_to_long_edge(img, max_long_edge))
}

/// Load and optionally downscale a JPEG file.
pub fn extract_preview_jpg(
    path: &Path,
    max_long_edge: u32,
) -> Result<DynamicImage, crate::error::IngestError> {
    let img = image::open(path).map_err(|e| crate::error::IngestError::Preview {
        path: path.to_owned(),
        reason: e.to_string(),
    })?;
    Ok(resize_to_long_edge(img, max_long_edge))
}

/// Encode `img` as a lossy WebP byte buffer at the given quality (0–100).
pub fn encode_webp(img: &DynamicImage, quality: u8) -> Result<Vec<u8>, String> {
    let rgb = img.to_rgb8();
    let encoder = webp::Encoder::from_rgb(rgb.as_raw(), rgb.width(), rgb.height());
    Ok(encoder.encode(quality as f32).to_vec())
}

/// Render an original photo to WebP bytes at the given size/quality.
///
/// Chooses the JPEG path for `.jpg`/`.jpeg` (case-insensitive) and the RAW
/// preview-extraction path otherwise. Used by the review server to produce
/// thumbnails and previews on demand.
pub fn render_webp(
    path: &Path,
    max_long_edge: u32,
    quality: u8,
) -> Result<Vec<u8>, crate::error::IngestError> {
    let is_jpg = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("jpg") || e.eq_ignore_ascii_case("jpeg"))
        .unwrap_or(false);
    let img = if is_jpg {
        extract_preview_jpg(path, max_long_edge)?
    } else {
        extract_preview_raw(path, max_long_edge)?
    };
    encode_webp(&img, quality).map_err(|reason| crate::error::IngestError::Preview {
        path: path.to_owned(),
        reason,
    })
}

/// Resize `img` so its longest edge is at most `max_long_edge` pixels.
///
/// If the image is already small enough, it is returned unchanged.
fn resize_to_long_edge(img: DynamicImage, max_long_edge: u32) -> DynamicImage {
    let (w, h) = (img.width(), img.height());
    let long_edge = w.max(h);
    if long_edge <= max_long_edge {
        return img;
    }
    let scale = max_long_edge as f64 / long_edge as f64;
    let new_w = ((w as f64 * scale).round() as u32).max(1);
    let new_h = ((h as f64 * scale).round() as u32).max(1);
    img.resize(new_w, new_h, FilterType::Lanczos3)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageBuffer, Rgb};

    #[test]
    fn render_webp_from_jpg() {
        let dir = tempfile::TempDir::new().unwrap();
        let p = dir.path().join("x.jpg");
        let img: ImageBuffer<Rgb<u8>, Vec<u8>> =
            ImageBuffer::from_fn(64, 48, |_, _| Rgb([10, 20, 30]));
        img.save(&p).unwrap();

        let bytes = render_webp(&p, 32, 80).unwrap();
        assert!(!bytes.is_empty());
        // RIFF/WEBP magic
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(&bytes[8..12], b"WEBP");
    }
}
