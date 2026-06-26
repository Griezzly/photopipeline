use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

use pipeline::config;

// ── CLI definition ─────────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(
    name    = "photopipe",
    version,
    about   = "Local-first RAW photo pipeline: filter, deduplicate, review.",
    long_about = None,
)]
struct Cli {
    /// Path to config file (default: OS config dir / photopipe / photopipe.toml)
    #[arg(long, global = true)]
    config: Option<PathBuf>,

    /// Log verbosity
    #[arg(long, global = true, default_value = "info", value_name = "LEVEL")]
    log_level: String,

    /// Log output format
    #[arg(long, global = true, default_value = "pretty", value_name = "FORMAT")]
    log_format: LogFormat,

    #[command(subcommand)]
    command: Command,
}

#[derive(Clone, ValueEnum)]
enum LogFormat {
    Pretty,
    Json,
}

#[derive(Subcommand)]
enum Command {
    /// Ingest and analyse one or more library roots.
    Scan {
        /// Library root directories to scan.
        #[arg(required = true)]
        paths: Vec<PathBuf>,

        /// Skip all ML inference phases.
        #[arg(long)]
        no_models: bool,

        /// Force re-analysis of already-processed files.
        #[arg(long)]
        reprocess: bool,
    },

    /// Rebuild per-lens sharpness baselines from the catalog.
    Calibrate,

    /// Rebuild duplicate groups using current embeddings.
    Dedupe,

    /// Generate or update the symlink review tree.
    ReviewTree {
        /// Destination directory for the review tree.
        output: PathBuf,

        /// Categories to include (e.g. rejected,duplicates,uncertain).
        #[arg(long, value_delimiter = ',')]
        include: Vec<String>,

        /// Delete the tree and rebuild from scratch.
        #[arg(long)]
        regenerate: bool,
    },

    /// Print all catalog data for a single file as JSON.
    Info { file: PathBuf },

    /// Print catalog summary statistics.
    Stats,

    /// Check configuration, models, database, and system health.
    Doctor,
}

// ── entry point ───────────────────────────────────────────────────────────────

fn main() -> Result<()> {
    let cli = Cli::parse();

    init_tracing(&cli.log_level, &cli.log_format)?;

    let config_path = cli.config.unwrap_or_else(config::default_config_path);

    let cfg = config::load(&config_path)?;
    tracing::debug!(path = %config_path.display(), "config loaded");

    match cli.command {
        Command::Scan {
            paths,
            no_models,
            reprocess,
        } => cmd_scan(paths, no_models, reprocess, &cfg),
        Command::Calibrate => cmd_calibrate(&cfg),
        Command::Dedupe => cmd_dedupe(&cfg),
        Command::ReviewTree {
            output,
            include,
            regenerate,
        } => cmd_review_tree(output, include, regenerate, &cfg),
        Command::Info { file } => cmd_info(file, &cfg),
        Command::Stats => cmd_stats(&cfg),
        Command::Doctor => cmd_doctor(&config_path, &cfg),
    }
}

// ── command handlers ──────────────────────────────────────────────────────────

fn cmd_scan(
    paths: Vec<PathBuf>,
    no_models: bool,
    _reprocess: bool,
    cfg: &config::Config,
) -> Result<()> {
    use pipeline::{
        analyze_defects, analyze_ml, cache::Cache, catalog::Catalog, ingest::ingest_directory,
        models::ModelHub,
    };

    let db_path = &cfg.catalog.db_path;
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let catalog = Catalog::open(db_path).map_err(|e| anyhow::anyhow!("catalog: {}", e))?;
    let cache =
        Cache::open(cfg.catalog.cache_dir.clone()).map_err(|e| anyhow::anyhow!("cache: {}", e))?;

    let hub = if no_models {
        tracing::info!("--no-models: skipping model loading");
        ModelHub::empty()
    } else {
        ModelHub::from_config(&cfg.models).map_err(|e| anyhow::anyhow!("models: {}", e))?
    };

    let report = ingest_directory(&paths, &catalog, &cache, &cfg.ingest)?;
    println!("Scan complete:");
    println!("  Processed : {}", report.processed);
    println!("  Skipped   : {}", report.skipped);
    println!("  Errored   : {}", report.errored);

    let defect_report = analyze_defects(&catalog, &cache, &hub, &cfg.defect)?;
    println!("Defect analysis:");
    println!("  Analyzed             : {}", defect_report.analyzed);
    println!("  Errored              : {}", defect_report.errored);
    println!(
        "  Flagged overexposed  : {}",
        defect_report.flagged_overexposed
    );
    println!(
        "  Flagged underexposed : {}",
        defect_report.flagged_underexposed
    );

    let ml_report = analyze_ml(&catalog, &cache, &hub, cfg.catalog.write_batch_size)?;
    if !hub.is_empty() {
        println!("ML analysis:");
        println!("  Embedded   : {}", ml_report.embedded);
        println!("  IQA scored : {}", ml_report.iqa_scored);
        println!("  Errored    : {}", ml_report.errored);
    }

    Ok(())
}

fn cmd_calibrate(_cfg: &config::Config) -> Result<()> {
    tracing::info!("calibrate — not yet implemented");
    eprintln!("photopipe calibrate: not yet implemented (Phase 4)");
    Ok(())
}

fn cmd_dedupe(_cfg: &config::Config) -> Result<()> {
    tracing::info!("dedupe — not yet implemented");
    eprintln!("photopipe dedupe: not yet implemented (Phase 5)");
    Ok(())
}

fn cmd_review_tree(
    output: PathBuf,
    _include: Vec<String>,
    _regenerate: bool,
    _cfg: &config::Config,
) -> Result<()> {
    tracing::info!(?output, "review-tree — not yet implemented");
    eprintln!("photopipe review-tree: not yet implemented (Phase 6)");
    Ok(())
}

fn cmd_info(_file: PathBuf, _cfg: &config::Config) -> Result<()> {
    tracing::info!("info — not yet implemented");
    eprintln!("photopipe info: not yet implemented (Phase 7)");
    Ok(())
}

fn cmd_stats(_cfg: &config::Config) -> Result<()> {
    tracing::info!("stats — not yet implemented");
    eprintln!("photopipe stats: not yet implemented (Phase 7)");
    Ok(())
}

fn cmd_doctor(config_path: &std::path::Path, cfg: &config::Config) -> Result<()> {
    println!("PhotoPipe Doctor");
    println!("================");
    println!();
    println!(
        "OS:           {} ({})",
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    println!("Family:       {}", std::env::consts::FAMILY);
    println!("Config file:  {}", config_path.display());
    println!("Exists:       {}", config_path.exists());
    println!();

    println!("Models");
    println!("------");
    println!("Model dir : {}", cfg.models.model_dir.display());

    // Show the provider that would be selected (without actually loading models).
    let provider = doctor_provider(cfg.models.device);
    println!("Provider  : {provider}");

    #[cfg(target_os = "macos")]
    println!(
        "  [macOS] CoreML EP disabled (ort rc.12 incompatibility with external-data models); \
         using CPU — revisit when ort ≥ 2.0.0 stable"
    );

    println!();
    doctor_model_file("dinov2_base.onnx", &cfg.models.model_dir, "embedder");
    doctor_model_file("clip_iqa.onnx", &cfg.models.model_dir, "iqa");
    println!("  rt_detr_l.onnx  — deferred (ORT Cos(int64) not implemented; see models/README.md)");
    println!();

    println!("Effective configuration:");
    println!("------------------------");
    println!("{}", toml::to_string_pretty(cfg)?);
    Ok(())
}

fn doctor_model_file(filename: &str, model_dir: &std::path::Path, role: &str) {
    let path = model_dir.join(filename);
    let data_path = model_dir.join(format!("{filename}.data"));
    if path.exists() {
        let graph_kb = std::fs::metadata(&path)
            .map(|m| m.len() / 1024)
            .unwrap_or(0);
        let data_mb = if data_path.exists() {
            std::fs::metadata(&data_path)
                .map(|m| format!(" + {:.0} MB data", m.len() as f64 / 1_048_576.0))
                .unwrap_or_default()
        } else {
            String::new()
        };
        println!("  {filename}  [{role}] ✓ present ({graph_kb} KB{data_mb})");
    } else {
        println!("  {filename}  [{role}] ✗ not found — run tools/export_{role}.py");
    }
}

fn doctor_provider(device: config::DeviceChoice) -> &'static str {
    match device {
        config::DeviceChoice::Cpu => "CPUExecutionProvider",
        config::DeviceChoice::CoreMl => "CoreMLExecutionProvider (overridden → CPU on macOS)",
        config::DeviceChoice::Cuda => "CUDAExecutionProvider",
        config::DeviceChoice::TensorRt => "TensorRtExecutionProvider",
        config::DeviceChoice::Auto => {
            #[cfg(target_os = "macos")]
            return "CPUExecutionProvider (auto; CoreML disabled in ort rc.12)";
            #[cfg(not(target_os = "macos"))]
            return "CUDAExecutionProvider (if available) else CPUExecutionProvider";
        }
    }
}

// ── tracing setup ─────────────────────────────────────────────────────────────

fn init_tracing(level: &str, format: &LogFormat) -> Result<()> {
    // RUST_LOG overrides --log-level when set
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(format!("photopipe={level},pipeline={level}")));

    match format {
        LogFormat::Pretty => {
            tracing_subscriber::registry()
                .with(filter)
                .with(fmt::layer().pretty())
                .init();
        }
        LogFormat::Json => {
            tracing_subscriber::registry()
                .with(filter)
                .with(fmt::layer().json())
                .init();
        }
    }

    Ok(())
}
