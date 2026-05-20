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

/// Encode `img` as a lossless WebP byte buffer.
///
/// Note: `image` 0.25.x only supports lossless WebP encoding; the `quality`
/// parameter is accepted for API compatibility but is not forwarded to the
/// encoder.
pub fn encode_webp(img: &DynamicImage, _quality: u8) -> Result<Vec<u8>, String> {
    use image::codecs::webp::WebPEncoder;
    use image::ImageEncoder;

    let rgb = img.to_rgb8();
    let mut buf = Vec::new();
    let encoder = WebPEncoder::new_lossless(&mut buf);
    encoder
        .write_image(
            rgb.as_raw(),
            rgb.width(),
            rgb.height(),
            image::ExtendedColorType::Rgb8,
        )
        .map_err(|e| e.to_string())?;
    Ok(buf)
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
