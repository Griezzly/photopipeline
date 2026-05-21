use std::path::Path;

use anyhow::Result;
use image::DynamicImage;

use crate::models::{DetectedSubject, SubjectDetector};

/// RT-DETR based subject detector.
///
/// NOTE: export of rt_detr_l.onnx is currently deferred due to an ORT
/// Cos(int64) NOT_IMPLEMENTED error in the positional encodings.  This struct
/// exists so the ModelHub type-checks, but it will never be instantiated until
/// the export blocker is resolved.
pub struct RtDetrDetector {
    session: ort::session::Session,
}

impl RtDetrDetector {
    pub fn load(path: &Path) -> Result<Self> {
        let session = crate::models::build_session(path)?;
        Ok(Self { session })
    }
}

impl SubjectDetector for RtDetrDetector {
    fn detect(&self, _img: &DynamicImage) -> Result<Vec<DetectedSubject>> {
        anyhow::bail!("RtDetrDetector::detect not yet implemented (deferred)")
    }

    fn name(&self) -> &str {
        "rt-detr-l"
    }
}
