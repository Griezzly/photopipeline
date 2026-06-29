use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

use photopipe::serve;
use pipeline::config;
use pipeline::library::LibraryRoots;
use pipeline::models::ModelHub;

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
    Calibrate { folder: PathBuf },

    /// Rebuild duplicate groups using current embeddings.
    ///
    /// Compares DINOv2 embeddings across all scanned files using brute-force KNN
    /// (the DuckDB vss/HNSW backend is not yet implemented). Run after `scan` to
    /// assign duplicate-group IDs and elect one keeper per group.
    Dedupe { folder: PathBuf },

    /// Generate or update the review tree (copies flagged photos for browsing).
    ReviewTree {
        /// Folder whose library to build the review tree from.
        folder: PathBuf,
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

    /// Print catalog summary statistics for a folder's library.
    Stats { folder: PathBuf },

    /// Check configuration, models, database, and system health.
    Doctor,

    /// List analyzed libraries (folder, last-analyzed, photo count).
    Libraries,

    /// Launch the local review web server for a folder's library.
    Serve {
        /// Folder whose library to serve.
        folder: PathBuf,
        /// Port to bind on 127.0.0.1.
        #[arg(long, default_value_t = 8787)]
        port: u16,
    },

    /// Materialize a keepers export tree from recorded decisions.
    ExportKeepers {
        folder: PathBuf,
        output: PathBuf,
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

    let roots = LibraryRoots::from_dirs()?;

    match cli.command {
        Command::Scan {
            paths,
            no_models,
            reprocess,
        } => cmd_scan(paths, no_models, reprocess, &cfg, &roots),
        Command::Calibrate { folder } => cmd_calibrate(&folder, &cfg, &roots),
        Command::Dedupe { folder } => cmd_dedupe(&folder, &cfg, &roots),
        Command::ReviewTree {
            folder,
            output,
            include,
            regenerate,
        } => cmd_review_tree(&folder, output, include, regenerate, &cfg, &roots),
        Command::Info { file } => cmd_info(file, &cfg, &roots),
        Command::Stats { folder } => cmd_stats(&folder, &cfg, &roots),
        Command::Doctor => cmd_doctor(&config_path, &cfg, &roots),
        Command::Libraries => cmd_libraries(&roots),
        Command::Serve { folder, port } => serve::run(&cfg, &folder, port),
        Command::ExportKeepers {
            folder,
            output,
            regenerate,
        } => cmd_export_keepers(&folder, output, regenerate, &cfg, &roots),
    }
}

// ── command handlers ──────────────────────────────────────────────────────────

fn cmd_scan(
    paths: Vec<PathBuf>,
    no_models: bool,
    _reprocess: bool,
    cfg: &config::Config,
    roots: &LibraryRoots,
) -> Result<()> {
    use pipeline::{
        analyze_defects, analyze_ml, ingest::ingest_directory, library::open_or_create_library,
        models::ModelHub,
    };

    let hub = if no_models {
        tracing::info!("--no-models: skipping model loading");
        ModelHub::empty()
    } else {
        ModelHub::from_config(&cfg.models).map_err(|e| anyhow::anyhow!("models: {}", e))?
    };

    for folder in &paths {
        let folder = config::expand_tilde(folder);
        println!("== {} ==", folder.display());
        let lib = open_or_create_library(roots, &folder)?;

        let report = ingest_directory(
            std::slice::from_ref(&folder),
            &lib.catalog,
            &lib.cache,
            &cfg.ingest,
            None,
        )?;
        println!("Scan complete:");
        println!("  Processed : {}", report.processed);
        println!("  Skipped   : {}", report.skipped);
        println!("  Errored   : {}", report.errored);

        let defect_report = analyze_defects(&lib.catalog, &lib.cache, &hub, &cfg.defect)?;
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

        let ml_report = analyze_ml(&lib.catalog, &lib.cache, &hub, cfg.catalog.write_batch_size)?;
        if !hub.is_empty() {
            println!("ML analysis:");
            println!("  Embedded   : {}", ml_report.embedded);
            println!("  IQA scored : {}", ml_report.iqa_scored);
            println!("  Errored    : {}", ml_report.errored);
        }

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        lib.catalog
            .set_last_analyzed(now)
            .map_err(|e| anyhow::anyhow!("library_meta: {e}"))?;
    }
    Ok(())
}

/// Open the library for `folder`, or bail with a clear message.
fn require_library(
    roots: &LibraryRoots,
    folder: &std::path::Path,
) -> Result<pipeline::library::Library> {
    let folder = config::expand_tilde(folder);
    match pipeline::library::open_existing_library(roots, &folder)? {
        Some(lib) => Ok(lib),
        None => anyhow::bail!(
            "no library for {} — run 'photopipe scan {}' first",
            folder.display(),
            folder.display()
        ),
    }
}

fn cmd_calibrate(
    folder: &std::path::Path,
    cfg: &config::Config,
    roots: &LibraryRoots,
) -> Result<()> {
    let lib = require_library(roots, folder)?;
    let report = pipeline::run_calibration(&lib.catalog, &cfg.defect)?;
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

fn cmd_dedupe(folder: &std::path::Path, cfg: &config::Config, roots: &LibraryRoots) -> Result<()> {
    let lib = require_library(roots, folder)?;
    if cfg.catalog.enable_vss {
        tracing::warn!(
            "catalog.enable_vss = true, but the DuckDB vss/HNSW backend is not \
             implemented yet — falling back to brute-force KNN"
        );
    }
    let report = pipeline::run_dedupe(&lib.catalog, &cfg.dedupe)?;
    println!("Dedupe complete:");
    println!("  Groups  : {}", report.groups);
    println!("  Members : {}", report.members);
    println!("  Keepers : {}", report.keepers);
    Ok(())
}

fn cmd_review_tree(
    folder: &std::path::Path,
    output: PathBuf,
    include: Vec<String>,
    regenerate: bool,
    cfg: &config::Config,
    roots: &LibraryRoots,
) -> Result<()> {
    let _ = cfg;
    let lib = require_library(roots, folder)?;
    let output = config::expand_tilde(&output);
    let est = pipeline::estimate_review_copy(&lib.catalog, &output, &include)?;
    println!(
        "Copying {} files ({}) → {} …",
        est.files,
        pipeline::humanize_bytes(est.bytes),
        output.display()
    );
    let report = pipeline::build_review_tree(&lib.catalog, &output, &include, regenerate)?;
    println!("Review tree: {}", output.display());
    println!(
        "  Copied  : {} files ({})",
        report.files_copied,
        pipeline::humanize_bytes(report.bytes_copied)
    );
    println!("  Skipped : {}", report.files_skipped);
    println!("  Removed : {}", report.files_removed);
    println!("  Groups  : {}", report.groups);
    println!("  Errors  : {}", report.errors);
    Ok(())
}

fn cmd_export_keepers(
    folder: &std::path::Path,
    output: PathBuf,
    regenerate: bool,
    cfg: &config::Config,
    roots: &LibraryRoots,
) -> Result<()> {
    let _ = cfg;
    let lib = require_library(roots, folder)?;
    let out = config::expand_tilde(&output);
    let est = pipeline::estimate_keepers_copy(&lib.catalog, &out)?;
    println!(
        "Copying {} files ({}) → {} …",
        est.files,
        pipeline::humanize_bytes(est.bytes),
        out.display()
    );
    let report = pipeline::build_keepers_tree(&lib.catalog, &out, regenerate)?;
    println!(
        "Copied {} files ({}), {} skipped, {} removed, {} errors → {}",
        report.files_copied,
        pipeline::humanize_bytes(report.bytes_copied),
        report.files_skipped,
        report.files_removed,
        report.errors,
        out.display()
    );
    Ok(())
}

fn cmd_info(file: PathBuf, cfg: &config::Config, roots: &LibraryRoots) -> Result<()> {
    let _ = cfg;
    let file = config::expand_tilde(&file);
    let folder = pipeline::library::find_library_for_file(roots, &file)?.ok_or_else(|| {
        anyhow::anyhow!(
            "no analyzed library contains {} — run scan first",
            file.display()
        )
    })?;
    let lib = pipeline::library::open_existing_library(roots, &folder)?
        .ok_or_else(|| anyhow::anyhow!("library for {} disappeared", folder.display()))?;
    match lib
        .catalog
        .dump_file(&file)
        .map_err(|e| anyhow::anyhow!("info: {}", e))?
    {
        Some(dump) => {
            println!("{}", serde_json::to_string_pretty(&dump)?);
            Ok(())
        }
        None => anyhow::bail!("no catalog entry for {}", file.display()),
    }
}

fn cmd_stats(folder: &std::path::Path, cfg: &config::Config, roots: &LibraryRoots) -> Result<()> {
    let _ = cfg;
    let lib = require_library(roots, folder)?;
    let catalog = &lib.catalog;
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
    println!("PhotoPipe Stats — {}", lib.folder.display());
    println!("===============");
    println!("Total files          : {}", s.total_files);
    println!("Duplicate groups     : {}", s.duplicate_group_count);
    println!("Files in groups      : {}", s.grouped_file_count);
    println!("Embeddings           : {}", s.embedding_count);
    println!("IQA scores           : {}", s.iqa_count);
    println!();
    println!("Defect flags");
    println!("------------");
    if flags.is_empty() {
        println!("  (none)");
    } else {
        for (k, n) in &flags {
            println!("  {k:<14} {n}");
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
    Ok(())
}

fn cmd_libraries(roots: &LibraryRoots) -> Result<()> {
    let libs = pipeline::library::list_libraries(roots)?;
    if libs.is_empty() {
        println!("No analyzed libraries yet. Run `photopipe scan <folder>`.");
        return Ok(());
    }
    println!("Analyzed libraries:");
    for l in &libs {
        let last = match l.last_analyzed {
            Some(ts) => ts.to_string(),
            None => "never".to_string(),
        };
        println!(
            "  {}  ({} photos, last analyzed {})",
            l.folder.display(),
            l.photo_count,
            last
        );
    }
    Ok(())
}

fn cmd_doctor(
    config_path: &std::path::Path,
    cfg: &config::Config,
    roots: &LibraryRoots,
) -> Result<()> {
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
    checks.push(doctor_check_cache_writable(&roots.cache));
    checks.push(doctor_check_disk_free(&roots.data));

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
