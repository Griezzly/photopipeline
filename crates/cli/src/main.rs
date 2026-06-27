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

    /// Rebuild per-lens sharpness baselines and re-flag blur/back-focus/low-IQA.
    ///
    /// Run after a meaningful number of photos per lens have been scanned
    /// (~30+ per lens is the default sample threshold). Leaves over/underexposed
    /// flags untouched.
    Calibrate,

    /// Rebuild duplicate groups using current embeddings.
    ///
    /// Compares DINOv2 embeddings across all scanned files using brute-force KNN
    /// (the DuckDB vss/HNSW backend is not yet implemented). Run after `scan` to
    /// assign duplicate-group IDs and elect one keeper per group.
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

fn cmd_calibrate(cfg: &config::Config) -> Result<()> {
    use pipeline::catalog::Catalog;

    let catalog =
        Catalog::open(&cfg.catalog.db_path).map_err(|e| anyhow::anyhow!("catalog: {}", e))?;

    let report = pipeline::run_calibration(&catalog, &cfg.defect)?;

    println!("Calibration complete:");
    println!("  Buckets built          : {}", report.buckets_built);
    println!("  Global sample count    : {}", report.global_n_samples);
    println!("  Stale flags cleared    : {}", report.flags_cleared);
    println!("  Flagged blur           : {}", report.flagged_blur);
    println!("  Flagged back-focus     : {}", report.flagged_back_focus);
    println!("  Flagged low-IQA        : {}", report.flagged_low_iqa);
    println!(
        "  Blur confidence bumped : {}",
        report.blur_confidence_bumped
    );
    Ok(())
}

fn cmd_dedupe(cfg: &config::Config) -> Result<()> {
    use pipeline::{catalog::Catalog, run_dedupe};

    let db_path = &cfg.catalog.db_path;
    let catalog = Catalog::open(db_path).map_err(|e| anyhow::anyhow!("catalog: {}", e))?;

    // Brute-force KNN only this phase; surface the vss omission rather than
    // silently cap, when the user has opted into it via config.
    if cfg.catalog.enable_vss {
        tracing::warn!(
            "catalog.enable_vss = true, but the DuckDB vss/HNSW backend is not \
             implemented yet — falling back to brute-force KNN"
        );
    }

    let report = run_dedupe(&catalog, &cfg.dedupe)?;
    println!("Dedupe complete:");
    println!("  Groups  : {}", report.groups);
    println!("  Members : {}", report.members);
    println!("  Keepers : {}", report.keepers);
    Ok(())
}

fn cmd_review_tree(
    output: PathBuf,
    include: Vec<String>,
    regenerate: bool,
    cfg: &config::Config,
) -> Result<()> {
    use pipeline::{build_review_tree, catalog::Catalog};

    // The positional <OUTPUT> arg always wins as the destination root.
    // cfg.output.review_tree (with its <library> token) is only a fallback
    // default for callers/config; the CLI requires the arg directly.
    // We still pass cfg.output because it carries link_type.
    let catalog =
        Catalog::open(&cfg.catalog.db_path).map_err(|e| anyhow::anyhow!("catalog: {}", e))?;

    tracing::info!(output = %output.display(), regenerate, "building review tree");
    let report = build_review_tree(&catalog, &output, &cfg.output, &include, regenerate)?;

    println!("Review tree: {}", output.display());
    println!("  Links created : {}", report.links_created);
    println!("  Links removed : {}", report.links_removed);
    println!("  Groups        : {}", report.groups);
    println!("  Errors        : {}", report.errors);
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
    doctor_model_file("rt_detr_l.onnx", &cfg.models.model_dir, "detector");
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
