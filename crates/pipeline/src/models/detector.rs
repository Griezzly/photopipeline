use std::path::Path;
use std::sync::Mutex;

use anyhow::Result;
use image::DynamicImage;

use crate::models::{DetectedSubject, SubjectDetector};

/// RT-DETR based subject detector.
///
/// NOTE: export of rt_detr_l.onnx is deferred.  ORT's CPU kernel does not
/// implement `Cos` for int64 inputs, and the PekingU/rtdetr_r50vd positional
/// encodings emit that op on both the legacy and dynamo ONNX export paths.
/// This struct exists so the ModelHub type-checks, but `RtDetrDetector::load`
/// will never succeed until the export blocker is resolved.
/// See models/README.md for the full deferral note and potential fixes.
pub struct RtDetrDetector {
    #[allow(dead_code)]
    session: Mutex<ort::session::Session>,
}

impl RtDetrDetector {
    pub fn load(path: &Path) -> Result<Self> {
        let session = crate::models::build_session(path)?;
        Ok(Self { session: Mutex::new(session) })
    }
}

impl SubjectDetector for RtDetrDetector {
    fn detect(&self, _img: &DynamicImage) -> Result<Vec<DetectedSubject>> {
        anyhow::bail!("RtDetrDetector::detect not yet implemented (deferred — see models/README.md)")
    }

    fn name(&self) -> &str {
        "rt-detr-l"
    }
}
