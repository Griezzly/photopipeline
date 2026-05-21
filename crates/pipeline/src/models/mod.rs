pub mod detector;
pub mod embedder;
pub mod iqa;

use std::sync::Arc;

use anyhow::Result;
use image::DynamicImage;

// ── geometry ──────────────────────────────────────────────────────────────────

/// Axis-aligned bounding box, normalised to [0.0, 1.0] in both axes.
#[derive(Debug, Clone, Copy)]
pub struct BBox {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

// ── detection types ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubjectClass {
    Person,
    Animal,
    Vehicle,
    Object,
    Other,
}

#[derive(Debug, Clone)]
pub struct DetectedSubject {
    pub bbox: BBox,
    pub class: SubjectClass,
    pub confidence: f32,
}

// ── traits ────────────────────────────────────────────────────────────────────

pub trait Embedder: Send + Sync {
    fn embed(&self, img: &DynamicImage) -> Result<Vec<f32>>;
    fn dim(&self) -> usize;
    fn name(&self) -> &str;
}

pub trait Iqa: Send + Sync {
    fn score(&self, img: &DynamicImage) -> Result<f32>;
    fn name(&self) -> &str;
}

pub trait SubjectDetector: Send + Sync {
    fn detect(&self, img: &DynamicImage) -> Result<Vec<DetectedSubject>>;
    fn name(&self) -> &str;
}

// ── ModelHub ──────────────────────────────────────────────────────────────────

pub struct ModelHub {
    pub embedder: Option<Arc<dyn Embedder>>,
    pub iqa: Option<Arc<dyn Iqa>>,
    pub detector: Option<Arc<dyn SubjectDetector>>,
    /// Human-readable name of the ORT execution provider in use.
    pub provider: String,
}

impl ModelHub {
    /// Load models from `cfg.model_dir`.  Missing model files are not errors;
    /// the corresponding slot is left as `None` and a notice is logged.
    ///
    /// Execution provider probe order (highest priority first):
    /// TensorRT (if compiled with `--features tensorrt`) → CUDA → CoreML → CPU.
    pub fn from_config(cfg: &crate::config::ModelsConfig) -> Result<Self> {
        let provider = detect_provider(cfg.device);
        tracing::info!(provider = %provider, "ORT execution provider selected");

        // CoreML is disabled in build_session on macOS (ort rc.12 bug with external-data
        // models).  Log once at startup so the user knows they're on CPU.
        #[cfg(target_os = "macos")]
        tracing::info!(
            "CoreML EP disabled on macOS (ort rc.12 incompatibility with external-data models); \
             using CPU — revisit when ort ≥ 2.0.0 stable"
        );

        let embedder: Option<Arc<dyn Embedder>> = {
            let path = cfg.model_dir.join("dinov2_base.onnx");
            if path.exists() {
                match embedder::DinoV2Embedder::load(&path) {
                    Ok(e) => {
                        tracing::info!("loaded embedder: {}", e.name());
                        Some(Arc::new(e))
                    }
                    Err(err) => {
                        tracing::warn!(path = %path.display(), error = %err, "failed to load embedder");
                        None
                    }
                }
            } else {
                tracing::info!(path = %path.display(), "skipping embedder — file not found");
                None
            }
        };

        let iqa: Option<Arc<dyn Iqa>> = {
            let path = cfg.model_dir.join("clip_iqa.onnx");
            if path.exists() {
                match iqa::ClipIqaScorer::load(&path) {
                    Ok(s) => {
                        tracing::info!("loaded iqa: {}", s.name());
                        Some(Arc::new(s))
                    }
                    Err(err) => {
                        tracing::warn!(path = %path.display(), error = %err, "failed to load iqa");
                        None
                    }
                }
            } else {
                tracing::info!(path = %path.display(), "skipping iqa — file not found");
                None
            }
        };

        let detector: Option<Arc<dyn SubjectDetector>> = {
            let path = cfg.model_dir.join("rt_detr_l.onnx");
            if path.exists() {
                match detector::RtDetrDetector::load(&path) {
                    Ok(d) => {
                        tracing::info!("loaded detector: {}", d.name());
                        Some(Arc::new(d))
                    }
                    Err(err) => {
                        tracing::warn!(path = %path.display(), error = %err, "failed to load detector");
                        None
                    }
                }
            } else {
                tracing::info!(
                    path = %path.display(),
                    "skipping detector — file not found (RT-DETR export deferred)"
                );
                None
            }
        };

        Ok(Self {
            embedder,
            iqa,
            detector,
            provider,
        })
    }

    /// An empty hub with no models loaded; useful for tests and no-models paths.
    pub fn empty() -> Self {
        Self {
            embedder: None,
            iqa: None,
            detector: None,
            provider: "CPUExecutionProvider".into(),
        }
    }

    /// Returns `true` when no model slot is loaded.
    pub fn is_empty(&self) -> bool {
        self.embedder.is_none() && self.iqa.is_none() && self.detector.is_none()
    }
}

// ── provider detection ────────────────────────────────────────────────────────

fn detect_provider(device: crate::config::DeviceChoice) -> String {
    use crate::config::DeviceChoice;

    match device {
        DeviceChoice::Cpu => return "CPUExecutionProvider".into(),
        DeviceChoice::CoreMl => return "CoreMLExecutionProvider".into(),
        DeviceChoice::Cuda => return "CUDAExecutionProvider".into(),
        DeviceChoice::TensorRt => return "TensorRtExecutionProvider".into(),
        DeviceChoice::Auto => {}
    }

    // Probe in priority order.  Platform-conditional blocks ensure that only
    // the EPs compiled into ort are referenced.
    #[cfg(all(not(target_os = "macos"), feature = "tensorrt"))]
    {
        use ort::ep::ExecutionProvider;
        if ort::ep::TensorRT::default().is_available().unwrap_or(false) {
            return "TensorRtExecutionProvider".into();
        }
    }

    #[cfg(not(target_os = "macos"))]
    {
        use ort::ep::ExecutionProvider;
        if ort::ep::CUDA::default().is_available().unwrap_or(false) {
            return "CUDAExecutionProvider".into();
        }
    }

    // CoreML EP is disabled in build_session (see note there); report CPU on macOS.
    #[cfg(target_os = "macos")]
    let _ = ();  // nothing to probe

    "CPUExecutionProvider".into()
}

/// Build an ORT session for `path` using the best available execution provider.
///
/// CoreML EP is intentionally excluded on macOS: ort rc.12's CoreML integration
/// crashes (SIGSEGV) or panics ("model_path must not be empty") when the model
/// uses the ONNX external-data format (.onnx + .onnx.data).  CPU fallback works
/// correctly on all platforms.
#[allow(clippy::vec_init_then_push)] // conditional #[cfg] pushes can't use vec![]
pub(crate) fn build_session(path: &std::path::Path) -> Result<ort::session::Session> {
    use ort::ep::ExecutionProviderDispatch;

    // Canonicalize to eliminate `..` components.  ORT's C++ layer derives the
    // external-data directory from the model path; unresolved `..` can produce
    // an empty parent, triggering "model_path must not be empty".
    let path = path
        .canonicalize()
        .map_err(|e| anyhow::anyhow!("cannot canonicalize model path {}: {e}", path.display()))?;

    let mut eps: Vec<ExecutionProviderDispatch> = Vec::new();

    #[cfg(all(not(target_os = "macos"), feature = "tensorrt"))]
    eps.push(ort::ep::TensorRT::default().build());

    #[cfg(not(target_os = "macos"))]
    eps.push(ort::ep::CUDA::default().build());

    eps.push(ort::ep::CPU::default().build());

    // Error<SessionBuilder> is not Send so we can't use ? with anyhow directly;
    // map_err discards the recovery type.
    let mut builder = ort::session::Session::builder()
        .map_err(|e| anyhow::anyhow!("{e}"))?
        .with_execution_providers(eps)
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    builder
        .commit_from_file(&path)
        .map_err(|e| anyhow::anyhow!("{e}"))
}

// ── test helper ───────────────────────────────────────────────────────────────

/// Returns `true` when `path` does not exist, printing a skip notice.
/// Use at the top of any test that requires a live ONNX model:
///
/// ```ignore
/// if skip_if_no_model(path) { return; }
/// ```
#[allow(dead_code)]
pub fn skip_if_no_model(path: &std::path::Path) -> bool {
    if !path.exists() {
        eprintln!("skipping: model not present at {}", path.display());
        return true;
    }
    false
}
