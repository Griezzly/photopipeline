//! Automatic RAW development: measure → decide → render → look.
//!
//! Stage boundaries are deliberate. `measure` touches pixels but makes no
//! decisions; `decide` makes every decision but never touches a pixel; `render`
//! and `pp3` translate a decision into RawTherapee's vocabulary. Keeping those
//! separate is what makes the tuning logic testable over plain numbers.

pub mod decide;
pub mod illuminant;
pub mod measure;
pub mod pp3;
pub mod render;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum DevelopError {
    #[error("raw decode failed for {path}: {reason}")]
    Decode {
        path: std::path::PathBuf,
        reason: String,
    },
    #[error("renderer failed for {path}: {reason}")]
    Render {
        path: std::path::PathBuf,
        reason: String,
    },
    #[error("IO error for {path}: {source}")]
    Io {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
}

use std::path::Path;

use crate::catalog::EditIdentity;

/// True when the recorded render still satisfies what we now want.
///
/// Mirrors the "missing or differs" semantics of `output::copy_file`: the
/// identity must match *and* the output must still be on disk at the recorded
/// size. Re-running `finish` over an unchanged library must do zero work — a
/// correctness requirement, not a perf goal.
pub fn is_up_to_date(
    existing: &EditIdentity,
    wanted: &EditIdentity,
    output_path: Option<&Path>,
    output_size: Option<i64>,
) -> bool {
    if existing != wanted {
        return false;
    }
    let (Some(path), Some(size)) = (output_path, output_size) else {
        return false;
    };
    match std::fs::metadata(path) {
        Ok(m) => m.len() == size as u64,
        Err(_) => false,
    }
}

use anyhow::Context;

use crate::analyze::ProgressSink;
use crate::catalog::{Catalog, EditRow};
use crate::config::{DevelopConfig, OutputSubdirs};
use crate::develop::decide::{decide, Sharpness, DECIDER_VERSION};
use crate::develop::render::{Pp3Renderer, RENDERER_NAME};

/// Summary of a `finish` run.
#[derive(Debug, Clone, Default)]
pub struct FinishReport {
    pub rendered: u64,
    pub skipped: u64,
    pub errored: u64,
}

/// Develop every kept photo into `out_dir`.
///
/// Deliberately serial. Unlike `scan`, this stage must not fan out with rayon:
/// `rawtherapee-cli` exposes no thread flag and saturates all cores internally,
/// and a 16-bit TIFF of a 24MP raw is roughly 145 MB, so several in flight at
/// once would thrash memory for no throughput gain (spec §8).
pub fn finish_folder(
    catalog: &Catalog,
    cfg: &DevelopConfig,
    out_dir: &Path,
    progress: &dyn ProgressSink,
) -> anyhow::Result<FinishReport> {
    let renderer = Pp3Renderer::new(cfg);
    // Probe once, before any work. A missing dependency should fail the run
    // immediately rather than produce one identical warning per photo.
    let version = renderer.probe().with_context(|| {
        "rawtherapee-cli is required by `photopipe finish` but could not be run. \
         Install RawTherapee and set [develop] rawtherapee_path, then check \
         `photopipe doctor`"
            .to_string()
    })?;
    tracing::info!(renderer = %version, "renderer ready");

    let work = catalog.keepers_to_develop()?;
    tracing::info!(count = work.len(), "keepers to develop");

    let mut report = FinishReport::default();

    progress.stage("measuring");
    progress.set_total(work.len() as u64);

    // One temp dir for the whole run; each intermediate is deleted as soon as
    // its JPEG is encoded, so peak disk stays at roughly one TIFF.
    let tmp = tempfile::TempDir::new().context("cannot create temp render directory")?;

    for item in &work {
        match finish_one(catalog, cfg, &renderer, out_dir, tmp.path(), item) {
            Ok(Outcome::Rendered) => report.rendered += 1,
            Ok(Outcome::Skipped) => report.skipped += 1,
            Err(e) => {
                // One corrupt file must never abort a full run.
                tracing::warn!(path = %item.path.display(), error = %e, "develop failed; skipping");
                report.errored += 1;
            }
        }
        progress.inc();
    }

    progress.stage("done");
    tracing::info!(
        rendered = report.rendered,
        skipped = report.skipped,
        errored = report.errored,
        "finish complete"
    );
    Ok(report)
}

enum Outcome {
    Rendered,
    Skipped,
}

fn finish_one(
    catalog: &Catalog,
    cfg: &DevelopConfig,
    renderer: &Pp3Renderer,
    out_dir: &Path,
    tmp_dir: &Path,
    item: &crate::catalog::KeeperToDevelop,
) -> anyhow::Result<Outcome> {
    // ① measure
    let stats = crate::develop::measure::measure_raw(&item.path)?;
    catalog.upsert_raw_stats(item.file_id, &stats)?;

    // ② decide
    let (exif, s_global) = catalog.develop_inputs(item.file_id)?;
    let recipe = decide(&stats, &exif, &Sharpness { s_global });
    let recipe_hash = recipe.recipe_hash();

    let wanted = EditIdentity {
        content_hash: item.content_hash.clone(),
        recipe_hash: recipe_hash.clone(),
        decider_version: DECIDER_VERSION.into(),
        renderer: RENDERER_NAME.into(),
        // Phase 2 fills these in; baseline-only renders carry no look.
        look_model: None,
        look_version: None,
    };

    let dest = output_path_for(cfg, out_dir, item);

    // Idempotency: skip when the recorded render still satisfies what we want.
    if let Some((existing, path, size)) = catalog.edit_identity(item.file_id)? {
        if is_up_to_date(&existing, &wanted, path.as_deref().map(Path::new), size) {
            tracing::debug!(path = %item.path.display(), "already finished; skipping");
            return Ok(Outcome::Skipped);
        }
    }

    // ③ baseline render
    let rendered = renderer.render(&item.path, &recipe, tmp_dir)?;

    // ④ encode. Phase 2 inserts the look between these two steps.
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("cannot create {}", parent.display()))?;
    }
    let img = image::open(&rendered.tiff)
        .with_context(|| format!("cannot read rendered TIFF {}", rendered.tiff.display()))?;
    encode_jpeg(&img, &dest, cfg.jpeg_quality)?;

    // The .pp3 sits beside the JPEG as an escape hatch for reopening the photo
    // in RawTherapee. Never beside the original raw. Copy it before `rendered`
    // is dropped, since the drop removes the scratch directory.
    let pp3_dest = dest.with_extension("pp3");
    let _ = std::fs::copy(&rendered.pp3, &pp3_dest);

    // Dropping `rendered` removes its scratch directory, which is what deletes
    // the 16-bit TIFF — roughly 145 MB for a 24 MP frame, and the largest thing
    // in the pipeline. Explicit rather than incidental, so peak disk stays at
    // about one TIFF regardless of how many photos the run processes.
    drop(rendered);

    let size = std::fs::metadata(&dest).map(|m| m.len() as i64).ok();
    catalog.upsert_edit(&EditRow {
        file_id: item.file_id,
        content_hash: item.content_hash.clone(),
        recipe,
        recipe_hash,
        decider_version: DECIDER_VERSION.into(),
        renderer: RENDERER_NAME.into(),
        look_model: None,
        look_version: None,
        lut_hash: None,
        look_applied: false,
        iqa_before: None,
        iqa_after: None,
        output_path: Some(dest.display().to_string()),
        output_size_bytes: size,
        rendered_at: now_secs(),
    })?;

    Ok(Outcome::Rendered)
}

/// Where one photo's JPEG lands.
fn output_path_for(
    cfg: &DevelopConfig,
    out_dir: &Path,
    item: &crate::catalog::KeeperToDevelop,
) -> std::path::PathBuf {
    let stem = item
        .path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("photo");
    let dir = match cfg.output_subdirs {
        OutputSubdirs::Month => out_dir.join(&item.year_month),
        OutputSubdirs::Flat => out_dir.to_path_buf(),
    };
    dir.join(format!("{stem}.jpg"))
}

fn encode_jpeg(img: &image::DynamicImage, dest: &Path, quality: u8) -> anyhow::Result<()> {
    let file =
        std::fs::File::create(dest).with_context(|| format!("cannot create {}", dest.display()))?;
    let mut w = std::io::BufWriter::new(file);
    let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut w, quality);
    enc.encode_image(&img.to_rgb8())
        .with_context(|| format!("cannot encode {}", dest.display()))?;
    Ok(())
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
