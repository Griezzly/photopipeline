use std::path::Path;
use std::sync::Mutex;

use anyhow::Result;
use image::DynamicImage;
use ndarray::Array4;

use crate::models::Embedder;

pub struct DinoV2Embedder {
    session: Mutex<ort::session::Session>,
}

impl DinoV2Embedder {
    pub fn load(path: &Path) -> Result<Self> {
        let session = crate::models::build_session(path)?;
        Ok(Self {
            session: Mutex::new(session),
        })
    }

    fn preprocess(img: &DynamicImage) -> Array4<f32> {
        // Resize to 224×224 with Lanczos3, convert to RGB.
        let rgb = img
            .resize_exact(224, 224, image::imageops::FilterType::Lanczos3)
            .to_rgb8();

        // Layout: (1, 3, 224, 224).  Values in [0, 1]; the ONNX wrapper
        // applies ImageNet normalisation internally.
        let mut arr = Array4::<f32>::zeros((1, 3, 224, 224));
        for y in 0..224_usize {
            for x in 0..224_usize {
                let px = rgb.get_pixel(x as u32, y as u32);
                arr[[0, 0, y, x]] = px[0] as f32 / 255.0;
                arr[[0, 1, y, x]] = px[1] as f32 / 255.0;
                arr[[0, 2, y, x]] = px[2] as f32 / 255.0;
            }
        }
        arr
    }
}

impl Embedder for DinoV2Embedder {
    fn embed(&self, img: &DynamicImage) -> Result<Vec<f32>> {
        let input = Self::preprocess(img);
        let tensor =
            ort::value::Tensor::<f32>::from_array(input).map_err(|e| anyhow::anyhow!("{e}"))?;

        let mut session = self
            .session
            .lock()
            .map_err(|_| anyhow::anyhow!("session mutex poisoned"))?;

        let outputs = session
            .run(ort::inputs!["image" => &tensor])
            .map_err(|e| anyhow::anyhow!("{e}"))?;

        let (_, data) = outputs["embedding"]
            .try_extract_tensor::<f32>()
            .map_err(|e| anyhow::anyhow!("{e}"))?;

        Ok(data.to_vec())
    }

    fn dim(&self) -> usize {
        768
    }

    fn name(&self) -> &str {
        "dinov2-base"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::skip_if_no_model;

    #[test]
    fn dinov2_embed_returns_correct_dim() {
        // CARGO_MANIFEST_DIR is crates/pipeline; models/ is at workspace root.
        let model_path = std::path::PathBuf::from(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../models/dinov2_base.onnx"
        ));
        if skip_if_no_model(&model_path) {
            return;
        }

        let embedder = DinoV2Embedder::load(&model_path).expect("load failed");
        assert_eq!(embedder.dim(), 768);

        // 32×32 synthetic gradient image.
        let img = image::DynamicImage::ImageRgb8(image::ImageBuffer::from_fn(32, 32, |x, y| {
            image::Rgb([(x % 256) as u8, (y % 256) as u8, 128u8])
        }));

        let embedding = embedder.embed(&img).expect("embed failed");
        assert_eq!(embedding.len(), 768, "embedding length mismatch");

        let norm: f32 = embedding.iter().map(|v| v * v).sum::<f32>().sqrt();
        assert!(norm.is_finite() && norm > 0.0, "embedding norm is {norm}");
    }
}
