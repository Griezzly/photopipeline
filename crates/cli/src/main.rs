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
    _no_models: bool,
    _reprocess: bool,
    cfg: &config::Config,
) -> Result<()> {
    use pipeline::{cache::Cache, catalog::Catalog, ingest::ingest_directory};

    let db_path = &cfg.catalog.db_path;
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let catalog = Catalog::open(db_path).map_err(|e| anyhow::anyhow!("catalog: {}", e))?;
    let cache =
        Cache::open(cfg.catalog.cache_dir.clone()).map_err(|e| anyhow::anyhow!("cache: {}", e))?;

    let report = ingest_directory(&paths, &catalog, &cache, &cfg.ingest)?;

    println!("Scan complete:");
    println!("  Processed : {}", report.processed);
    println!("  Skipped   : {}", report.skipped);
    println!("  Errored   : {}", report.errored);
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
    println!("Effective configuration:");
    println!("------------------------");
    println!("{}", toml::to_string_pretty(cfg)?);
    Ok(())
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
