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

use std::collections::HashMap;
use std::collections::HashSet;
use std::path::Path;

use crate::catalog::EditIdentity;
use crate::ingest::FileFormat;

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
use crate::output::dedupe_name;

/// Summary of a `finish` run.
#[derive(Debug, Clone, Default)]
pub struct FinishReport {
    pub rendered: u64,
    pub skipped: u64,
    pub errored: u64,
    /// Keepers that are not RAW files (e.g. a plain JPEG). `finish` only
    /// develops RAWs; these are counted separately from `errored` so a mixed
    /// library does not produce a wall of decode-failure noise for files that
    /// were never going to decode as a raw in the first place.
    pub skipped_unsupported: u64,
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

    // Basenames already handed out, keyed by output directory, so two keepers
    // that share a stem in the same capture month (different source
    // subfolders, two cards, a wrapped camera counter, or plain coincidence)
    // land on distinct files instead of one overwriting the other's JPEG and
    // .pp3. `keepers_to_develop()` is `ORDER BY f.path`, and this loop is
    // serial, so the same input assigns the same names on every run.
    let mut taken_by_dir: HashMap<std::path::PathBuf, HashSet<String>> = HashMap::new();

    for item in &work {
        if !is_raw_format(&item.file_format) {
            tracing::info!(
                path = %item.path.display(),
                format = %item.file_format,
                "keeper is not a RAW file; `finish` only develops RAWs, skipping"
            );
            report.skipped_unsupported += 1;
            progress.inc();
            continue;
        }
        match finish_one(
            catalog,
            cfg,
            &renderer,
            out_dir,
            tmp.path(),
            item,
            &mut taken_by_dir,
        ) {
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
        skipped_unsupported = report.skipped_unsupported,
        errored = report.errored,
        "finish complete"
    );
    Ok(report)
}

/// True when `file_format` (the lowercase extension recorded at ingest) is a
/// RAW format `finish` knows how to develop. `jpg`/`jpeg` are supported
/// ingest formats but are not RAWs, so `measure_raw` cannot decode them.
fn is_raw_format(file_format: &str) -> bool {
    FileFormat::from_ext(file_format)
        .map(|f| f.is_raw())
        .unwrap_or(false)
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
    taken_by_dir: &mut HashMap<std::path::PathBuf, HashSet<String>>,
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

    let dest = output_path_for(cfg, out_dir, item, taken_by_dir);

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
    if let Err(e) = std::fs::copy(&rendered.pp3, &pp3_dest) {
        tracing::warn!(path = %pp3_dest.display(), error = %e, "could not write the .pp3 escape hatch");
    }

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
///
/// Two keepers can share a stem within the same output directory — different
/// source subfolders, two cards, a wrapped camera counter, or plain
/// coincidence — and `output_subdirs = "flat"` makes that collision far more
/// likely. Left unhandled, the second render would silently overwrite the
/// first's JPEG and `.pp3`; worse, the first photo's `edits` row would still
/// record its own byte size, which would no longer match what's on disk, so
/// `is_up_to_date` would report it stale forever and the two keepers would
/// ping-pong re-renders indefinitely. `dedupe_name` (shared with the review
/// and keepers trees) resolves the collision deterministically.
fn output_path_for(
    cfg: &DevelopConfig,
    out_dir: &Path,
    item: &crate::catalog::KeeperToDevelop,
    taken_by_dir: &mut HashMap<std::path::PathBuf, HashSet<String>>,
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
    let taken = taken_by_dir.entry(dir.clone()).or_default();
    let basename = dedupe_name(taken, &format!("{stem}.jpg"));
    dir.join(basename)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::KeeperToDevelop;

    fn keeper(file_id: i64, path: &str, year_month: &str) -> KeeperToDevelop {
        KeeperToDevelop {
            file_id,
            path: std::path::PathBuf::from(path),
            content_hash: format!("hash-{file_id}"),
            year_month: year_month.into(),
            file_format: "arw".into(),
        }
    }

    #[test]
    fn is_raw_format_accepts_raws_and_rejects_jpg() {
        assert!(is_raw_format("arw"));
        assert!(is_raw_format("ARW"));
        assert!(is_raw_format("dng"));
        assert!(!is_raw_format("jpg"));
        assert!(!is_raw_format("jpeg"));
        assert!(!is_raw_format("bogus"));
    }

    /// Two keepers from different source subfolders that happen to share a
    /// stem, in the same capture month, must land on distinct output paths —
    /// otherwise the second render silently overwrites the first's JPEG.
    #[test]
    fn same_stem_same_month_produces_distinct_output_paths() {
        let cfg = DevelopConfig::default();
        let out = Path::new("/out");
        let a = keeper(1, "/cardA/subdir/DSC001.arw", "2024-05");
        let b = keeper(2, "/cardB/otherdir/DSC001.arw", "2024-05");

        let mut taken: HashMap<std::path::PathBuf, HashSet<String>> = HashMap::new();
        let path_a = output_path_for(&cfg, out, &a, &mut taken);
        let path_b = output_path_for(&cfg, out, &b, &mut taken);

        assert_ne!(
            path_a, path_b,
            "colliding stems must not overwrite each other"
        );
        assert_eq!(path_a, Path::new("/out/2024-05/DSC001.jpg"));
        assert_eq!(path_b, Path::new("/out/2024-05/DSC001 (2).jpg"));
    }

    /// `keepers_to_develop()` is `ORDER BY f.path` and the finish loop is
    /// strictly serial, so the same input must assign the same output names
    /// on every run — that determinism is what keeps idempotency stable.
    #[test]
    fn output_path_assignment_is_deterministic_across_runs() {
        let cfg = DevelopConfig::default();
        let out = Path::new("/out");
        let items = vec![
            keeper(1, "/cardA/DSC001.arw", "2024-05"),
            keeper(2, "/cardB/DSC001.arw", "2024-05"),
            keeper(3, "/cardC/DSC002.arw", "2024-05"),
            keeper(4, "/cardA/DSC001.arw", "2024-06"),
        ];

        let assign = |items: &[KeeperToDevelop]| -> Vec<std::path::PathBuf> {
            let mut taken: HashMap<std::path::PathBuf, HashSet<String>> = HashMap::new();
            items
                .iter()
                .map(|item| output_path_for(&cfg, out, item, &mut taken))
                .collect()
        };

        let first_run = assign(&items);
        let second_run = assign(&items);
        assert_eq!(
            first_run, second_run,
            "the same ordered input must assign identical output paths every run"
        );
    }
}
