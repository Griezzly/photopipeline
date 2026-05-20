use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

// ── top-level ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub catalog: CatalogConfig,
    pub ingest: IngestConfig,
    pub models: ModelsConfig,
    pub defect: DefectConfig,
    pub dedupe: DedupeConfig,
    pub output: OutputConfig,
}

// ── catalog ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CatalogConfig {
    pub db_path: PathBuf,
    pub cache_dir: PathBuf,
    pub write_batch_size: usize,
    pub enable_vss: bool,
}

impl Default for CatalogConfig {
    fn default() -> Self {
        Self {
            db_path: xdg_data_home().join("photopipe/catalog.duckdb"),
            cache_dir: xdg_cache_home().join("photopipe"),
            write_batch_size: 64,
            enable_vss: false,
        }
    }
}

// ── ingest ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct IngestConfig {
    pub extensions: Vec<String>,
    pub follow_symlinks: bool,
    /// 0 = use all logical cores
    pub threads: usize,
    pub sidecar_jpg: SidecarJpgMode,
    pub preview_max_long_edge: u32,
    pub preview_quality: u8,
}

impl Default for IngestConfig {
    fn default() -> Self {
        Self {
            extensions: vec![
                "arw".into(),
                "cr3".into(),
                "cr2".into(),
                "nef".into(),
                "raf".into(),
                "rw2".into(),
                "dng".into(),
                "jpg".into(),
                "jpeg".into(),
            ],
            follow_symlinks: false,
            threads: 0,
            sidecar_jpg: SidecarJpgMode::Prefer,
            preview_max_long_edge: 2048,
            preview_quality: 85,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SidecarJpgMode {
    Prefer,
    Ignore,
    Require,
}

// ── models ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelsConfig {
    pub device: DeviceChoice,
    pub embedder: String,
    pub iqa: String,
    pub detector: String,
    pub model_dir: PathBuf,
}

impl Default for ModelsConfig {
    fn default() -> Self {
        Self {
            device: DeviceChoice::Auto,
            embedder: "dinov2-base".into(),
            iqa: "clip-iqa".into(),
            detector: "rt-detr-l".into(),
            model_dir: PathBuf::from("./models"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DeviceChoice {
    Auto,
    CoreMl,
    Cuda,
    TensorRt,
    Cpu,
}

// ── defect ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct DefectConfig {
    pub blur: BlurConfig,
    pub exposure: ExposureConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct BlurConfig {
    pub enable: bool,
    pub subject_min_area_ratio: f32,
    pub fallback_center_crop: f32,
    pub iqa_second_opinion: bool,
    pub percentile_threshold: f32,
    pub min_samples_for_bucket: usize,
}

impl Default for BlurConfig {
    fn default() -> Self {
        Self {
            enable: true,
            subject_min_area_ratio: 0.02,
            fallback_center_crop: 0.4,
            iqa_second_opinion: true,
            percentile_threshold: 0.10,
            min_samples_for_bucket: 30,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ExposureConfig {
    pub enable: bool,
    pub clipped_highlights_threshold: f32,
    pub clipped_shadows_threshold: f32,
}

impl Default for ExposureConfig {
    fn default() -> Self {
        Self {
            enable: true,
            clipped_highlights_threshold: 0.05,
            clipped_shadows_threshold: 0.10,
        }
    }
}

// ── dedupe ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DedupeConfig {
    pub enable: bool,
    pub time_window_seconds: u64,
    pub cosine_threshold_within_window: f32,
    pub cosine_threshold_global: f32,
    pub knn_k: usize,
    pub min_group_size: usize,
}

impl Default for DedupeConfig {
    fn default() -> Self {
        Self {
            enable: true,
            time_window_seconds: 60,
            cosine_threshold_within_window: 0.92,
            cosine_threshold_global: 0.97,
            knn_k: 10,
            min_group_size: 2,
        }
    }
}

// ── output ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct OutputConfig {
    /// Literal `<library>` is substituted with the scan root at runtime.
    pub review_tree: String,
    pub link_type: LinkType,
    pub keeper_strategy: KeeperStrategy,
}

impl Default for OutputConfig {
    fn default() -> Self {
        Self {
            review_tree: "<library>/_review".into(),
            link_type: LinkType::Symlink,
            keeper_strategy: KeeperStrategy::Iqa,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LinkType {
    Symlink,
    Hardlink,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeeperStrategy {
    Iqa,
    Sharpness,
    IqaThenSharpness,
}

// ── loading ───────────────────────────────────────────────────────────────────

/// Default config-file path: `$XDG_CONFIG_HOME/photopipe/photopipe.toml`.
pub fn default_config_path() -> PathBuf {
    xdg_config_home().join("photopipe/photopipe.toml")
}

/// Load config from `path`, falling back to built-in defaults if the file
/// doesn't exist.  Returns an error only if the file exists but is malformed.
pub fn load(path: &Path) -> anyhow::Result<Config> {
    if !path.exists() {
        tracing::debug!(path = %path.display(), "config file not found, using defaults");
        return Ok(Config::default());
    }
    let raw = std::fs::read_to_string(path)?;
    let cfg: Config = toml::from_str(&raw)
        .map_err(|e| anyhow::anyhow!("config parse error in {}: {}", path.display(), e))?;
    Ok(cfg)
}

// ── helpers ───────────────────────────────────────────────────────────────────

/// Expand a leading `~/` to the real home directory.
pub fn expand_tilde(p: &Path) -> PathBuf {
    let s = p.to_string_lossy();
    if let Some(rest) = s.strip_prefix("~/") {
        if let Some(home) = home_dir() {
            return home.join(rest);
        }
    }
    p.to_path_buf()
}

fn home_dir() -> Option<PathBuf> {
    std::env::var("HOME").ok().map(PathBuf::from)
}

fn xdg_config_home() -> PathBuf {
    std::env::var("XDG_CONFIG_HOME")
        .ok()
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            home_dir()
                .unwrap_or_else(|| PathBuf::from("~"))
                .join(".config")
        })
}

fn xdg_data_home() -> PathBuf {
    std::env::var("XDG_DATA_HOME")
        .ok()
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            home_dir()
                .unwrap_or_else(|| PathBuf::from("~"))
                .join(".local/share")
        })
}

fn xdg_cache_home() -> PathBuf {
    std::env::var("XDG_CACHE_HOME")
        .ok()
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            home_dir()
                .unwrap_or_else(|| PathBuf::from("~"))
                .join(".cache")
        })
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_round_trip() {
        let cfg = Config::default();
        let serialized = toml::to_string_pretty(&cfg).expect("serialize");
        let parsed: Config = toml::from_str(&serialized).expect("deserialize");
        // spot-check a few fields
        assert_eq!(parsed.ingest.preview_quality, 85);
        assert_eq!(parsed.dedupe.knn_k, 10);
        assert!(!parsed.catalog.enable_vss);
    }

    #[test]
    fn partial_override() {
        let toml_str = r#"
            [ingest]
            preview_quality = 90
        "#;
        let cfg: Config = toml::from_str(toml_str).expect("parse");
        assert_eq!(cfg.ingest.preview_quality, 90);
        // other fields remain at defaults
        assert_eq!(cfg.ingest.preview_max_long_edge, 2048);
    }
}
