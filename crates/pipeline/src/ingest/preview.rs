use std::path::Path;

use image::{imageops::FilterType, DynamicImage};

/// Extract a preview image from a RAW file using rawler.
///
/// Tries three rawler entry points in order and takes the first that yields an
/// image: `preview_image`, `thumbnail_image`, then `full_image`.
///
/// The third one matters. `rawler` 0.7.2 wires each decoder's embedded JPEG to
/// whichever of these three names its author picked, and the choice is not
/// consistent between formats — the ARW decoder implements only `full_image`
/// (its doc comment calls it "return the embedded JPEG preview"), so Sony files
/// answered `None` to both of the other two and got no preview at all, which
/// silently disabled every downstream ML step for those photos. Despite the
/// name, no decoder's `full_image` develops the sensor plane: each one reads an
/// already-encoded image out of a tag (embedded JPEG, or an uncompressed RGB
/// strip for CR2), and the trait default returns `None`. Ordering it last keeps
/// formats that already had a preview on exactly the path they were on.
///
/// Each step is tried even if an earlier one failed, rather than aborting on
/// the first error: a decoder that errors on one entry point can still return a
/// usable image from another, and a real preview beats a propagated error.
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

    // Each source is decoded only if the previous one came up empty — the array
    // form would eagerly decode all three on every file.
    let mut last_err: Option<String> = None;
    let mut found: Option<DynamicImage> = None;
    #[allow(clippy::type_complexity)]
    let sources: [&dyn Fn() -> rawler::Result<Option<DynamicImage>>; 3] = [
        &|| decoder.preview_image(&raw_source, &params),
        &|| decoder.thumbnail_image(&raw_source, &params),
        &|| decoder.full_image(&raw_source, &params),
    ];
    for source in sources {
        match source() {
            Ok(Some(img)) => {
                found = Some(img);
                break;
            }
            Ok(None) => {}
            Err(e) => last_err = Some(e.to_string()),
        }
    }

    let img = found.ok_or_else(|| crate::error::IngestError::Preview {
        path: path.to_owned(),
        reason: match last_err {
            Some(e) => format!("no preview, thumbnail or embedded image available: {e}"),
            None => "no preview, thumbnail or embedded image available".into(),
        },
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

/// Downscale already-encoded WebP bytes into a smaller WebP (e.g. a cached
/// preview into a grid thumbnail). Decodes from memory, resizes so the long
/// edge is at most `max_long_edge`, and re-encodes at `quality`. This lets the
/// review server reuse the preview `scan` already produced instead of
/// re-decoding the original — which also avoids RAW formats whose embedded
/// preview cannot be re-extracted on demand.
pub fn downscale_webp(
    webp_bytes: &[u8],
    max_long_edge: u32,
    quality: u8,
) -> Result<Vec<u8>, String> {
    let img = image::load_from_memory(webp_bytes).map_err(|e| e.to_string())?;
    let resized = resize_to_long_edge(img, max_long_edge);
    encode_webp(&resized, quality)
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

    #[test]
    fn downscale_webp_shrinks_and_stays_webp() {
        // Encode a 200px-wide source webp, then downscale to a 40px long edge.
        let src: ImageBuffer<Rgb<u8>, Vec<u8>> =
            ImageBuffer::from_fn(200, 150, |x, _| Rgb([(x % 256) as u8, 0, 0]));
        let webp = encode_webp(&DynamicImage::ImageRgb8(src), 85).unwrap();

        let thumb = downscale_webp(&webp, 40, 78).unwrap();
        assert_eq!(&thumb[0..4], b"RIFF");
        assert_eq!(&thumb[8..12], b"WEBP");
        // decoding the result back confirms the long edge was capped at 40
        let decoded = image::load_from_memory(&thumb).unwrap();
        assert!(decoded.width().max(decoded.height()) <= 40);
    }

    #[test]
    fn downscale_webp_rejects_garbage() {
        assert!(downscale_webp(b"not a webp", 40, 78).is_err());
    }

    /// KI-1 regression: a Sony ARW must yield a preview. `rawler` 0.7.2 answers
    /// `None` from both `preview_image` and `thumbnail_image` for these files
    /// and only exposes the embedded JPEG through `full_image`, so before the
    /// third fallback this returned `Preview { reason: "no preview or thumbnail
    /// available" }` and every ML stage downstream of the preview silently did
    /// nothing for this camera.
    ///
    /// The fixture is local, gitignored sample data (`example-pictures/` — see
    /// `.gitignore`), never a fabricated one; the test skips cleanly when it is
    /// absent, e.g. on a fresh clone or in CI.
    #[test]
    fn arw_preview_falls_back_to_the_embedded_jpeg() {
        let raw = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../example-pictures/DSC03073.ARW");
        if !raw.exists() {
            eprintln!(
                "skipping: {} not present (example-pictures/ is gitignored local sample data)",
                raw.display()
            );
            return;
        }

        let img = extract_preview_raw(&raw, 1024).expect("ARW preview extraction");
        // `resize` fits within the box preserving aspect, so the long edge can
        // land a pixel under the cap.
        let long_edge = img.width().max(img.height());
        assert!(
            (1023..=1024).contains(&long_edge),
            "resized to long edge, got {long_edge}"
        );
        // The embedded preview is 1616x1080; the 160x120 thumbnail would land
        // far off this aspect ratio, so this also pins down which one we got.
        assert!(
            (img.width() as f64 / img.height() as f64 - 1616.0 / 1080.0).abs() < 0.01,
            "expected the 1616x1080 embedded preview, got {}x{}",
            img.width(),
            img.height()
        );
    }
}
