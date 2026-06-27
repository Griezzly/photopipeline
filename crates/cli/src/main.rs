use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

use photopipe::serve;
use pipeline::catalog::Catalog;
use pipeline::config;
use pipeline::models::ModelHub;

/// Schema version the binary expects the catalog to be at.
const EXPECTED_SCHEMA_VERSION: u32 = 2;
const MIN_FREE_DISK_GB: u64 = 5;

#[derive(Clone, Copy, PartialEq, Eq)]
enum CheckStatus {
    Ok,
    Warn,
    Fail,
}

impl CheckStatus {
    fn glyph(self) -> &'static str {
        match self {
            CheckStatus::Ok => "[ ok ]",
            CheckStatus::Warn => "[warn]",
            CheckStatus::Fail => "[fail]",
        }
    }
}

/// One diagnostic line. `critical` checks that `Fail` make `doctor` exit non-zero.
struct DoctorCheck {
    label: String,
    status: CheckStatus,
    detail: String,
    critical: bool,
}

impl DoctorCheck {
    fn ok(label: &str, detail: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            status: CheckStatus::Ok,
            detail: detail.into(),
            critical: false,
        }
    }
    fn warn(label: &str, detail: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            status: CheckStatus::Warn,
            detail: detail.into(),
            critical: false,
        }
    }
    fn fail_critical(label: &str, detail: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            status: CheckStatus::Fail,
            detail: detail.into(),
            critical: true,
        }
    }
    fn print(&self) {
        println!("{} {:<22} {}", self.status.glyph(), self.label, self.detail);
    }
}

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

    /// Launch the local review web server.
    Serve {
        /// Port to bind on 127.0.0.1.
        #[arg(long, default_value_t = 8787)]
        port: u16,
    },

    /// Materialize a keepers export tree from recorded decisions.
    ExportKeepers {
        /// Destination directory for the keepers tree.
        output: PathBuf,
        /// Delete the tree and rebuild from scratch.
        #[arg(long)]
        regenerate: bool,
    },
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
        Command::Serve { port } => serve::run(&cfg, port),
        Command::ExportKeepers { output, regenerate } => {
            cmd_export_keepers(output, regenerate, &cfg)
        }
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
    let catalog =
        Catalog::open(&cfg.catalog.db_path).map_err(|e| anyhow::anyhow!("catalog: {}", e))?;

    tracing::info!(output = %output.display(), regenerate, "building review tree");
    let report = build_review_tree(&catalog, &output, &include, regenerate)?;

    println!("Review tree: {}", output.display());
    println!("  Copied  : {} files ({})", report.files_copied, pipeline::humanize_bytes(report.bytes_copied));
    println!("  Skipped : {}", report.files_skipped);
    println!("  Removed : {}", report.files_removed);
    println!("  Groups  : {}", report.groups);
    println!("  Errors  : {}", report.errors);
    Ok(())
}

fn cmd_export_keepers(output: PathBuf, regenerate: bool, cfg: &config::Config) -> Result<()> {
    let catalog =
        Catalog::open(&cfg.catalog.db_path).map_err(|e| anyhow::anyhow!("catalog: {}", e))?;
    let out = config::expand_tilde(&output);
    let report = pipeline::build_keepers_tree(&catalog, &out, regenerate)?;
    println!(
        "Keepers tree: {} copied ({}), {} skipped, {} removed, {} errors → {}",
        report.files_copied,
        pipeline::humanize_bytes(report.bytes_copied),
        report.files_skipped,
        report.files_removed,
        report.errors,
        out.display()
    );
    Ok(())
}

fn cmd_info(file: PathBuf, cfg: &config::Config) -> Result<()> {
    let catalog =
        Catalog::open(&cfg.catalog.db_path).map_err(|e| anyhow::anyhow!("catalog: {}", e))?;
    match catalog
        .dump_file(&file)
        .map_err(|e| anyhow::anyhow!("info: {}", e))?
    {
        Some(dump) => {
            let json = serde_json::to_string_pretty(&dump)?;
            println!("{json}");
            Ok(())
        }
        None => {
            anyhow::bail!("no catalog entry for {}", file.display());
        }
    }
}

fn cmd_stats(cfg: &config::Config) -> Result<()> {
    let catalog =
        Catalog::open(&cfg.catalog.db_path).map_err(|e| anyhow::anyhow!("catalog: {}", e))?;

    let s = catalog
        .stats()
        .map_err(|e| anyhow::anyhow!("stats: {}", e))?;
    let flags = catalog
        .flag_counts()
        .map_err(|e| anyhow::anyhow!("flags: {}", e))?;
    let cameras = catalog
        .per_camera_counts()
        .map_err(|e| anyhow::anyhow!("cameras: {}", e))?;
    let lenses = catalog
        .per_lens_counts()
        .map_err(|e| anyhow::anyhow!("lenses: {}", e))?;

    let db_size = file_size(&cfg.catalog.db_path);
    let cache_size = dir_size(&cfg.catalog.cache_dir);

    println!("PhotoPipe Stats");
    println!("===============");
    println!("Total files          : {}", s.total_files);
    println!("Embeddings           : {}", s.embedding_count);
    println!("IQA scores           : {}", s.iqa_count);
    println!("Duplicate groups     : {}", s.duplicate_group_count);
    println!("Files in groups      : {}", s.grouped_file_count);
    println!();
    println!("Defect flags");
    println!("------------");
    if flags.is_empty() {
        println!("  (none)");
    } else {
        for (ft, n) in &flags {
            println!("  {ft:<14} {n}");
        }
    }
    println!();
    println!("Per camera");
    println!("----------");
    if cameras.is_empty() {
        println!("  (no EXIF)");
    } else {
        for (cam, n) in &cameras {
            println!("  {cam:<28} {n}");
        }
    }
    println!();
    println!("Per lens");
    println!("--------");
    if lenses.is_empty() {
        println!("  (no EXIF)");
    } else {
        for (cam, lens, n) in &lenses {
            println!("  {cam} / {lens:<28} {n}");
        }
    }
    println!();
    println!("Disk usage");
    println!("----------");
    println!(
        "  Catalog : {:.1} MB ({})",
        db_size as f64 / 1_048_576.0,
        cfg.catalog.db_path.display()
    );
    println!(
        "  Cache   : {:.1} MB ({})",
        cache_size as f64 / 1_048_576.0,
        cfg.catalog.cache_dir.display()
    );
    Ok(())
}

/// Size in bytes of a single file, or 0 if it can't be read.
fn file_size(path: &std::path::Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

/// Recursive byte size of a directory tree, ignoring entries it can't read.
fn dir_size(dir: &std::path::Path) -> u64 {
    let mut total = 0;
    let read = match std::fs::read_dir(dir) {
        Ok(r) => r,
        Err(_) => return 0,
    };
    for entry in read.flatten() {
        let Ok(meta) = entry.metadata() else { continue };
        if meta.is_dir() {
            total += dir_size(&entry.path());
        } else {
            total += meta.len();
        }
    }
    total
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
    println!("Model dir:    {}", cfg.models.model_dir.display());
    println!("Provider:     {}", doctor_provider(cfg.models.device));

    #[cfg(target_os = "macos")]
    println!(
        "  [macOS] CoreML EP disabled (ort rc.12 incompatibility with external-data models); \
         using CPU — revisit when ort ≥ 2.0.0 stable"
    );
    println!();

    println!("Health checks");
    println!("-------------");

    let mut checks: Vec<DoctorCheck> = Vec::new();
    checks.push(doctor_check_schema(&cfg.catalog.db_path));
    checks.push(doctor_check_cache_writable(&cfg.catalog.cache_dir));
    checks.push(doctor_check_disk_free(&cfg.catalog.db_path));

    // Actually attempt to load models so we report which slots came up.
    match ModelHub::from_config(&cfg.models) {
        Ok(hub) => {
            println!("(ORT execution provider in use: {})", hub.provider);
            checks.extend(doctor_check_models(&cfg.models, &hub));
        }
        Err(e) => {
            checks.push(DoctorCheck::fail_critical(
                "Models",
                format!("ModelHub::from_config failed: {e}"),
            ));
        }
    }

    for c in &checks {
        c.print();
    }
    println!();

    let failed = checks
        .iter()
        .any(|c| c.critical && c.status == CheckStatus::Fail);
    if failed {
        println!("Result: UNHEALTHY — fix the [fail] items above.");
        anyhow::bail!("doctor: one or more critical checks failed");
    }
    println!("Result: healthy.");
    Ok(())
}

/// Open the catalog and verify its schema version matches what we expect.
fn doctor_check_schema(db_path: &std::path::Path) -> DoctorCheck {
    if !db_path.exists() {
        // Not yet created is fine — `scan` creates it. Report, don't fail.
        return DoctorCheck::warn(
            "DB schema",
            format!(
                "no catalog yet at {} (run `scan` to create)",
                db_path.display()
            ),
        );
    }
    match Catalog::open(db_path) {
        Ok(catalog) => match catalog.schema_version() {
            Ok(v) if v == EXPECTED_SCHEMA_VERSION => {
                DoctorCheck::ok("DB schema", format!("version {v}"))
            }
            Ok(v) => DoctorCheck::fail_critical(
                "DB schema",
                format!(
                    "version {v}, expected {EXPECTED_SCHEMA_VERSION} — DB is from a different photopipe build"
                ),
            ),
            Err(e) => {
                DoctorCheck::fail_critical("DB schema", format!("cannot read schema version: {e}"))
            }
        },
        Err(e) => DoctorCheck::fail_critical("DB schema", format!("cannot open catalog: {e}")),
    }
}

/// Verify the cache directory exists (creating it) and is writable by
/// creating then removing a probe file.
fn doctor_check_cache_writable(cache_dir: &std::path::Path) -> DoctorCheck {
    if let Err(e) = std::fs::create_dir_all(cache_dir) {
        return DoctorCheck::fail_critical(
            "Cache writable",
            format!("cannot create {}: {e}", cache_dir.display()),
        );
    }
    let probe = cache_dir.join(".photopipe-doctor-probe");
    match std::fs::write(&probe, b"ok") {
        Ok(()) => {
            let _ = std::fs::remove_file(&probe);
            DoctorCheck::ok("Cache writable", cache_dir.display().to_string())
        }
        Err(e) => DoctorCheck::fail_critical(
            "Cache writable",
            format!("cannot write under {}: {e}", cache_dir.display()),
        ),
    }
}

/// Report free space on the filesystem that holds `path`. Non-critical:
/// warns when below MIN_FREE_DISK_GB but never fails the run.
fn doctor_check_disk_free(path: &std::path::Path) -> DoctorCheck {
    use sysinfo::Disks;

    let disks = Disks::new_with_refreshed_list();
    // Pick the disk whose mount point is the longest prefix of `path`
    // (the most specific mount). Fall back to the max available if none match.
    let target = path.to_path_buf();
    let best = disks
        .list()
        .iter()
        .filter(|d| target.starts_with(d.mount_point()))
        .max_by_key(|d| d.mount_point().as_os_str().len())
        .or_else(|| disks.list().iter().max_by_key(|d| d.available_space()));

    match best {
        Some(d) => {
            let free_gb = d.available_space() / 1_073_741_824;
            let detail = format!("{free_gb} GB free on {}", d.mount_point().display());
            if free_gb >= MIN_FREE_DISK_GB {
                DoctorCheck::ok("Disk free", detail)
            } else {
                DoctorCheck::warn("Disk free", format!("{detail} (< {MIN_FREE_DISK_GB} GB)"))
            }
        }
        None => DoctorCheck::warn("Disk free", "could not determine free space".to_string()),
    }
}

/// For each model configured by name, check the ONNX file is present
/// (critical) and whether the loaded hub populated the slot (non-critical).
fn doctor_check_models(cfg: &config::ModelsConfig, hub: &ModelHub) -> Vec<DoctorCheck> {
    // (config name, expected filename, slot-loaded predicate, role label)
    let specs: [(&str, &str, bool); 3] = [
        (
            cfg.embedder.as_str(),
            "dinov2_base.onnx",
            hub.embedder.is_some(),
        ),
        (cfg.iqa.as_str(), "clip_iqa.onnx", hub.iqa.is_some()),
        (
            cfg.detector.as_str(),
            "rt_detr_l.onnx",
            hub.detector.is_some(),
        ),
    ];
    let roles = ["embedder", "iqa", "detector"];

    let mut checks = Vec::new();
    for ((name, filename, loaded), role) in specs.into_iter().zip(roles) {
        let label = format!("Model {role}");
        let path = cfg.model_dir.join(filename);
        if !path.exists() {
            checks.push(DoctorCheck::fail_critical(
                &label,
                format!("'{name}' configured but {} missing", path.display()),
            ));
        } else if loaded {
            checks.push(DoctorCheck::ok(
                &label,
                format!("'{name}' loaded ({filename})"),
            ));
        } else {
            checks.push(DoctorCheck::warn(
                &label,
                format!("'{name}' file present but failed to load ({filename})"),
            ));
        }
    }
    checks
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
