use std::path::Path;
use std::sync::Mutex;

use anyhow::Result;
use image::DynamicImage;
use ndarray::Array4;

use crate::models::Iqa;

// CLIP normalisation constants (openai/clip-vit-base-patch32).
#[allow(clippy::excessive_precision)]
const CLIP_MEAN: [f32; 3] = [0.48145466, 0.4578275, 0.40821073];
#[allow(clippy::excessive_precision)]
const CLIP_STD: [f32; 3] = [0.26862954, 0.26130258, 0.27577711];

pub struct ClipIqaScorer {
    session: Mutex<ort::session::Session>,
}

impl ClipIqaScorer {
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

        // Layout: (1, 3, 224, 224), CLIP-normalised.
        // The Rust side applies normalisation; the ONNX model expects this space.
        let mut arr = Array4::<f32>::zeros((1, 3, 224, 224));
        for y in 0..224_usize {
            for x in 0..224_usize {
                let px = rgb.get_pixel(x as u32, y as u32);
                for c in 0..3_usize {
                    let raw = px[c] as f32 / 255.0;
                    arr[[0, c, y, x]] = (raw - CLIP_MEAN[c]) / CLIP_STD[c];
                }
            }
        }
        arr
    }
}

impl Iqa for ClipIqaScorer {
    fn score(&self, img: &DynamicImage) -> Result<f32> {
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

        let (_, data) = outputs["iqa_score"]
            .try_extract_tensor::<f32>()
            .map_err(|e| anyhow::anyhow!("{e}"))?;

        let raw = *data
            .first()
            .ok_or_else(|| anyhow::anyhow!("iqa_score output is empty"))?;
        Ok(raw.clamp(0.0, 1.0))
    }

    fn name(&self) -> &str {
        "clip-iqa"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::skip_if_no_model;

    #[test]
    fn clip_iqa_returns_unit_interval() {
        // CARGO_MANIFEST_DIR is crates/pipeline; models/ is at workspace root.
        let model_path = std::path::PathBuf::from(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../models/clip_iqa.onnx"
        ));
        if skip_if_no_model(&model_path) {
            return;
        }

        let scorer = ClipIqaScorer::load(&model_path).expect("load failed");

        let img = image::DynamicImage::ImageRgb8(image::ImageBuffer::from_fn(32, 32, |x, y| {
            image::Rgb([(x % 256) as u8, (y % 256) as u8, 128u8])
        }));

        let score = scorer.score(&img).expect("score failed");
        assert!(
            (0.0..=1.0).contains(&score),
            "IQA score {score} outside [0, 1]"
        );
        assert!(score.is_finite(), "IQA score is not finite");
    }
}
