use std::path::Path;

use anyhow::Result;
use image::DynamicImage;

use crate::models::Embedder;

pub struct DinoV2Embedder {
    session: ort::session::Session,
}

impl DinoV2Embedder {
    pub fn load(path: &Path) -> Result<Self> {
        let session = crate::models::build_session(path)?;
        Ok(Self { session })
    }
}

impl Embedder for DinoV2Embedder {
    fn embed(&self, _img: &DynamicImage) -> Result<Vec<f32>> {
        anyhow::bail!("DinoV2Embedder::embed not yet implemented (sub-task 5)")
    }

    fn dim(&self) -> usize {
        768
    }

    fn name(&self) -> &str {
        "dinov2-base"
    }
}
