use std::path::Path;

use anyhow::Result;
use image::DynamicImage;

use crate::models::Iqa;

pub struct ClipIqaScorer {
    session: ort::session::Session,
}

impl ClipIqaScorer {
    pub fn load(path: &Path) -> Result<Self> {
        let session = crate::models::build_session(path)?;
        Ok(Self { session })
    }
}

impl Iqa for ClipIqaScorer {
    fn score(&self, _img: &DynamicImage) -> Result<f32> {
        anyhow::bail!("ClipIqaScorer::score not yet implemented (sub-task 7)")
    }

    fn name(&self) -> &str {
        "clip-iqa"
    }
}
