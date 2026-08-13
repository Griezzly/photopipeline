//! Automatic RAW development: measure → decide → render → look.
//!
//! Stage boundaries are deliberate. `measure` touches pixels but makes no
//! decisions; `decide` makes every decision but never touches a pixel; `render`
//! and `pp3` translate a decision into RawTherapee's vocabulary. Keeping those
//! separate is what makes the tuning logic testable over plain numbers.

pub mod decide;
pub mod illuminant;
pub mod lut;
pub mod lut_apply;
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
use crate::config::{DefectConfig, DevelopConfig, OutputSubdirs};
use crate::develop::decide::{
    decide, relative_sharpness, Sharpness, DECIDER_VERSION, NEUTRAL_RELATIVE_SHARPNESS,
};
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
    /// Files deleted by the prune pass — outputs whose photo is no longer a
    /// keeper, plus anything else left in the managed tree.
    pub pruned: u64,
}

/// True when the look should be kept.
///
/// Fails open: when either score is missing the guard cannot judge, and
/// rejecting every look because the IQA model is absent would be a far more
/// confusing failure than keeping one.
pub fn guard_verdict(before: Option<f32>, after: Option<f32>, margin: f32) -> bool {
    match (before, after) {
        (Some(b), Some(a)) => a >= b - margin,
        _ => true,
    }
}

/// Everything one `finish` run needs. A struct rather than eight positional
/// parameters, which is both unreadable at the call site and past the limit
/// clippy enforces.
pub struct FinishRequest<'a> {
    pub catalog: &'a Catalog,
    pub cfg: &'a DevelopConfig,
    /// Supplies `min_samples_for_bucket` for the sharpening baseline lookup.
    pub defect_cfg: &'a DefectConfig,
    /// Supplies the look predictor and the IQA model backing its guard. An
    /// empty hub means baseline-only, which is a supported mode, not an error.
    pub hub: &'a crate::models::ModelHub,
    /// The library's cache root. Content-addressed `.cube` files go under
    /// `luts/` here, so a burst of similar frames shares one file.
    pub cache_dir: &'a Path,
    pub out_dir: &'a Path,
    /// Delete the finished tree and rebuild it rather than updating in place.
    pub regenerate: bool,
}

/// Develop every kept photo into `out_dir`.
///
/// Deliberately serial. Unlike `scan`, this stage must not fan out with rayon:
/// `rawtherapee-cli` exposes no thread flag and saturates all cores internally,
/// and a 16-bit TIFF of a 24MP raw is roughly 145 MB, so several in flight at
/// once would thrash memory for no throughput gain (spec §8).
pub fn finish_folder(
    req: FinishRequest<'_>,
    progress: &dyn ProgressSink,
) -> anyhow::Result<FinishReport> {
    let FinishRequest {
        catalog,
        cfg,
        defect_cfg,
        hub,
        cache_dir,
        out_dir,
        regenerate,
    } = req;
    if regenerate {
        crate::output::remove_managed_tree(out_dir)?;
        tracing::info!(root = %out_dir.display(), "regenerate: removed existing finished tree");
    }

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

    // Progress contract with the Develop screen (crates/cli/assets/develop.js).
    //
    // One counted phase for the whole run — `developing`, `set_total(n)`, one
    // `inc()` per photo — with the per-photo detail carried by `step()`, which
    // deliberately does not disturb that count. The steps are, in order:
    //
    //   measuring · rendering · applying look · encoding
    //
    // then `pruning` and `done` as phases once the loop is over. `applying look`
    // is emitted even when no predictor is loaded: the phase exists either way,
    // it simply completes instantly, and a fixed step list is a far easier
    // contract for the UI than one whose shape depends on which models happen to
    // be installed. A photo that is already current, or is not a RAW, never gets
    // past `measuring` — both are fast, so neither leaves the screen looking hung.
    progress.stage("developing");
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

    let ctx = FinishCtx {
        catalog,
        cfg,
        defect_cfg,
        hub,
        renderer: &renderer,
        cache_dir,
        out_dir,
        tmp_dir: tmp.path(),
        progress,
    };

    // Every path this run vouches for. Anything else under a managed tree is an
    // orphan from an earlier run and gets pruned below.
    let mut expected: HashSet<std::path::PathBuf> = HashSet::new();

    for item in &work {
        let label = item
            .path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("photo")
            .to_string();
        progress.step("measuring", &label);
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
        match finish_one(&ctx, item, &label, &mut taken_by_dir) {
            Ok(Outcome::Rendered(dest)) => {
                report.rendered += 1;
                expected.insert(dest.with_extension("pp3"));
                expected.insert(dest);
            }
            Ok(Outcome::Skipped(dest)) => {
                report.skipped += 1;
                expected.insert(dest.with_extension("pp3"));
                expected.insert(dest);
            }
            Err(e) => {
                // One corrupt file must never abort a full run.
                tracing::warn!(path = %item.path.display(), error = %e, "develop failed; skipping");
                report.errored += 1;
            }
        }
        progress.inc();
    }

    progress.stage("pruning");
    report.pruned = prune_finished_tree(catalog, out_dir, &expected)?;

    progress.stage("done");
    tracing::info!(
        rendered = report.rendered,
        skipped = report.skipped,
        skipped_unsupported = report.skipped_unsupported,
        errored = report.errored,
        pruned = report.pruned,
        "finish complete"
    );
    Ok(report)
}

/// Delete outputs the catalog no longer vouches for, and the `edits` rows that
/// pointed at them.
///
/// Two things go stale when a verdict flips away from `keep`: the JPEG and its
/// `.pp3` on disk, and the `edits` row still naming them. Leaving the row is the
/// more dangerous half — with the row in place but the photo no longer in the
/// keeper set, a *different* photo can later be handed that same output path by
/// `dedupe_name` (which only knows about names taken during the current run) and
/// overwrite the file, leaving the old row pointing at another photo's pixels.
/// Removing both together is what makes that unreachable.
///
/// Only ever touches a directory carrying the `.photopipe-tree` marker. A tree
/// this run created is marked here; a pre-existing unmarked directory is left
/// strictly alone, because `--out` may point at somewhere with real photos in it.
fn prune_finished_tree(
    catalog: &Catalog,
    out_dir: &Path,
    expected: &HashSet<std::path::PathBuf>,
) -> anyhow::Result<u64> {
    use crate::output::{dir_is_empty, is_managed_tree, TREE_MARKER};

    if !out_dir.exists() {
        return Ok(0);
    }
    // Adopt a tree that is demonstrably ours: one we just wrote into, an empty
    // directory, or one holding outputs the catalog already claims (a finished
    // tree from a release that predates the marker). Anything else is left
    // untouched — `--out` can point anywhere, including at real photos.
    if !is_managed_tree(out_dir) {
        let ours = dir_is_empty(out_dir)
            || !expected.is_empty()
            || catalog
                .all_edit_outputs()?
                .iter()
                .any(|p| Path::new(p).starts_with(out_dir));
        if !ours {
            tracing::warn!(
                root = %out_dir.display(),
                marker = TREE_MARKER,
                "not a photopipe tree and nothing here is recorded as ours; \
                 skipping the prune pass rather than deleting files we did not write"
            );
            return Ok(0);
        }
        std::fs::write(out_dir.join(TREE_MARKER), b"photopipe managed tree\n")
            .map_err(|e| anyhow::anyhow!("write marker: {e}"))?;
    }

    // ① drop rows whose photo is no longer a keeper, deleting their files first
    // so a row never outlives the JPEG it names.
    let orphans = catalog.orphaned_edits()?;
    let mut pruned = 0u64;
    let mut to_forget = Vec::with_capacity(orphans.len());
    for (file_id, output_path) in orphans {
        if let Some(p) = output_path.as_deref().map(Path::new) {
            // Guard against deleting something this run just wrote: a photo can
            // legitimately reuse a path an orphan used to hold.
            if !expected.contains(p) {
                for victim in [p.to_path_buf(), p.with_extension("pp3")] {
                    match std::fs::remove_file(&victim) {
                        Ok(()) => pruned += 1,
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                        Err(e) => {
                            tracing::warn!(file = %victim.display(), error = %e, "prune remove failed")
                        }
                    }
                }
            }
        }
        to_forget.push(file_id);
    }
    let rows = catalog.delete_edits(&to_forget)?;
    if rows > 0 {
        tracing::info!(
            rows,
            "pruned edits rows for photos that are no longer keepers"
        );
    }

    // ② sweep anything else left in the tree — outputs from a run whose `edits`
    // rows were since deleted, or files renamed by a change in `output_subdirs`.
    sweep_unexpected(out_dir, out_dir, expected, &mut pruned);
    Ok(pruned)
}

/// Recursively remove files under `dir` that are not in `expected`, then any
/// directory left empty. The root marker is preserved.
fn sweep_unexpected(
    root: &Path,
    dir: &Path,
    expected: &HashSet<std::path::PathBuf>,
    pruned: &mut u64,
) {
    use crate::output::{dir_is_empty, TREE_MARKER};

    let read = match std::fs::read_dir(dir) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(dir = %dir.display(), error = %e, "prune read_dir failed");
            return;
        }
    };
    for entry in read.flatten() {
        let path = entry.path();
        if path.parent() == Some(root)
            && path.file_name().and_then(|n| n.to_str()) == Some(TREE_MARKER)
        {
            continue;
        }
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.file_type().is_dir() {
            sweep_unexpected(root, &path, expected, pruned);
            if dir_is_empty(&path) {
                let _ = std::fs::remove_dir(&path);
            }
        } else if !expected.contains(&path) {
            match std::fs::remove_file(&path) {
                Ok(()) => *pruned += 1,
                Err(e) => {
                    tracing::warn!(file = %path.display(), error = %e, "prune remove failed")
                }
            }
        }
    }
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
    /// Rendered afresh; carries the JPEG's path so the caller can vouch for
    /// it (and its `.pp3`) against the prune pass.
    Rendered(std::path::PathBuf),
    /// Already up to date; still vouched for, or the prune pass would delete
    /// every output the moment nothing needed re-rendering.
    Skipped(std::path::PathBuf),
}

/// Place this frame's `s_subject` on the 0..1 scale `decide()` expects, using
/// the calibrated `sharpness_baseline`.
///
/// Falls back in the same order the blur flagger does — the per-bucket row for
/// this camera/lens/focal/aperture, then the global sentinel `('*','*',0,0.0)`
/// that `rebuild_sharpness_baselines` writes whenever there is any sample at
/// all — and finally to [`NEUTRAL_RELATIVE_SHARPNESS`] when neither exists or
/// the frame has no `s_subject`. A library that has never been calibrated
/// therefore gets even-handed sharpening rather than a value derived from an
/// absent comparison.
///
/// The sentinel is queried with `min_samples = 0` on purpose: it is a whole-
/// library aggregate, so the per-bucket sample floor does not apply to it.
fn resolve_relative_sharpness(
    catalog: &Catalog,
    defect_cfg: &DefectConfig,
    exif: &crate::ingest::ExifData,
    s_subject: Option<f32>,
) -> anyhow::Result<f32> {
    let Some(s) = s_subject else {
        return Ok(NEUTRAL_RELATIVE_SHARPNESS);
    };
    let min_samples = defect_cfg.blur.min_samples_for_bucket;

    let bucket_span = match (
        exif.camera_model.as_deref(),
        exif.lens_model.as_deref(),
        exif.focal_length_mm,
        exif.aperture,
    ) {
        (Some(cam), Some(lens), Some(focal), Some(ap)) => catalog.bucket_baseline_span(
            cam,
            lens,
            crate::calibration::buckets::focal_bucket(focal),
            crate::calibration::buckets::aperture_bucket(ap),
            min_samples,
        )?,
        _ => None,
    };

    let span = match bucket_span {
        Some(s) => Some(s),
        None => catalog.bucket_baseline_span("*", "*", 0, 0.0, 0)?,
    };

    Ok(match span {
        Some((p10, p90)) => relative_sharpness(s, p10, p90),
        None => NEUTRAL_RELATIVE_SHARPNESS,
    })
}

/// Everything `finish_one` needs that does not change between photos. Grouped
/// so the per-photo call takes the run context, the item, and the name ledger
/// rather than eight positional arguments.
struct FinishCtx<'a> {
    catalog: &'a Catalog,
    cfg: &'a DevelopConfig,
    defect_cfg: &'a DefectConfig,
    hub: &'a crate::models::ModelHub,
    renderer: &'a Pp3Renderer,
    cache_dir: &'a Path,
    out_dir: &'a Path,
    tmp_dir: &'a Path,
    /// Where the per-photo `step()` transitions go. A photo takes minutes, so
    /// the screen needs to hear from inside this function, not only between
    /// calls to it.
    progress: &'a dyn ProgressSink,
}

fn finish_one(
    ctx: &FinishCtx<'_>,
    item: &crate::catalog::KeeperToDevelop,
    // Display label for `step()` — the photo's filename.
    label: &str,
    taken_by_dir: &mut HashMap<std::path::PathBuf, HashSet<String>>,
) -> anyhow::Result<Outcome> {
    // `cache_dir` is read through `ctx` by the look stage rather than unpacked
    // here.
    let FinishCtx {
        catalog,
        cfg,
        defect_cfg,
        hub,
        renderer,
        out_dir,
        tmp_dir,
        ..
    } = *ctx;

    // ① measure — but only when the answer cannot already be known.
    //
    // Decoding the raw sensor plane is by far the most expensive step here, so
    // on an unchanged library every photo used to pay a full decode purely to be
    // skipped moments later. The old order had a reason: `recipe_hash` is needed
    // to build the identity, and the recipe needs the stats.
    //
    // The persisted `raw_stats` row closes that circle. It carries no
    // `content_hash` of its own, so it cannot be trusted alone — but an `edits`
    // row whose `content_hash` still matches the file proves those stats were
    // measured from exactly these bytes, since `finish_one` writes both in the
    // same pass. Every other case (no `edits` row, a file that changed, a
    // missing `raw_stats` row) falls through to a real measurement.
    let existing_edit = catalog.edit_identity(item.file_id)?;
    let content_unchanged = existing_edit
        .as_ref()
        .is_some_and(|(e, _, _)| e.content_hash == item.content_hash);
    let stats = match if content_unchanged {
        catalog.get_raw_stats(item.file_id)?
    } else {
        None
    } {
        Some(cached) => {
            tracing::debug!(path = %item.path.display(), "reusing persisted raw_stats");
            cached
        }
        None => {
            let measured = crate::develop::measure::measure_raw(&item.path)?;
            catalog.upsert_raw_stats(item.file_id, &measured)?;
            measured
        }
    };

    // ② decide
    let (exif, s_subject) = catalog.develop_inputs(item.file_id)?;
    let s_relative = resolve_relative_sharpness(catalog, defect_cfg, &exif, s_subject)?;
    let recipe = decide(&stats, &exif, &Sharpness { s_relative });
    let recipe_hash = recipe.recipe_hash();

    let wanted = EditIdentity {
        content_hash: item.content_hash.clone(),
        recipe_hash: recipe_hash.clone(),
        decider_version: DECIDER_VERSION.into(),
        renderer: RENDERER_NAME.into(),
        // Taken from the hub and the config, deliberately *not* from the
        // prediction result further down: the identity has to describe what
        // this run intends to produce, or turning the look off would leave
        // every existing looked JPEG looking current.
        look_model: look_identity(cfg, hub).map(|(name, _)| name),
        look_version: look_identity(cfg, hub).map(|(_, version)| version),
    };

    let dest = output_path_for(cfg, out_dir, item, taken_by_dir);

    // Idempotency: skip when the recorded render still satisfies what we want.
    //
    // "Where we were asked to write" is part of that. `is_up_to_date` checks the
    // *recorded* path, so on its own it would call a photo current because an
    // earlier run's JPEG still sits in a different directory — and `finish --out
    // somewhere-new` would report "already current" while leaving the new
    // directory empty.
    let recorded_here = existing_edit
        .as_ref()
        .and_then(|(_, p, _)| p.as_deref())
        .is_some_and(|p| Path::new(p) == dest);
    if let Some((existing, path, size)) = existing_edit {
        if recorded_here && is_up_to_date(&existing, &wanted, path.as_deref().map(Path::new), size)
        {
            tracing::debug!(path = %item.path.display(), "already finished; skipping");
            return Ok(Outcome::Skipped(dest));
        }
    }

    // ③ baseline render
    ctx.progress.step("rendering", label);
    let rendered = renderer.render(&item.path, &recipe, tmp_dir)?;

    // ④ look
    let baseline = image::open(&rendered.tiff)
        .with_context(|| format!("cannot read rendered TIFF {}", rendered.tiff.display()))?;

    ctx.progress.step("applying look", label);
    let mut look = LookOutcome::default();
    let final_image = apply_look(ctx, item, &baseline, &mut look);

    // ⑥ encode
    ctx.progress.step("encoding", label);
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("cannot create {}", parent.display()))?;
    }
    encode_jpeg(
        final_image.as_ref().unwrap_or(&baseline),
        &dest,
        cfg.jpeg_quality,
    )?;

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
        // The identity fields must match `wanted` exactly, or the next run
        // reads this row back as stale and re-renders forever.
        look_model: wanted.look_model.clone(),
        look_version: wanted.look_version.clone(),
        lut_hash: look.lut_hash,
        look_applied: look.applied,
        iqa_before: look.iqa_before,
        iqa_after: look.iqa_after,
        output_path: Some(dest.display().to_string()),
        output_size_bytes: size,
        rendered_at: now_secs(),
    })?;

    Ok(Outcome::Rendered(dest))
}

/// What the look stage did, for the audit record in `edits`.
#[derive(Debug, Default)]
struct LookOutcome {
    lut_hash: Option<String>,
    applied: bool,
    iqa_before: Option<f32>,
    iqa_after: Option<f32>,
}

/// The look this run *intends* to apply, as `(model, version)`.
///
/// Depends only on config and which models loaded — never on whether a given
/// prediction succeeded — because it feeds the idempotency identity. A photo
/// whose look failed must still compare equal on the next run, or it re-renders
/// forever; the `edits.look_applied` flag is where "we tried and shipped the
/// baseline" is recorded.
fn look_identity(cfg: &DevelopConfig, hub: &crate::models::ModelHub) -> Option<(String, String)> {
    if !cfg.look.enable {
        return None;
    }
    hub.look
        .as_ref()
        .map(|p| (p.name().to_string(), p.version().to_string()))
}

/// Predict and apply the look, subject to the quality guard.
///
/// Returns `None` when the baseline should ship unchanged — the look is
/// disabled, no predictor loaded, prediction failed, or the guard rejected the
/// result. A look failure is never a render failure: the photo still gets its
/// technically-corrected JPEG.
fn apply_look(
    ctx: &FinishCtx<'_>,
    item: &crate::catalog::KeeperToDevelop,
    baseline: &image::DynamicImage,
    out: &mut LookOutcome,
) -> Option<image::DynamicImage> {
    if !ctx.cfg.look.enable {
        return None;
    }
    let predictor = ctx.hub.look.as_ref()?;

    let lut = match predictor.predict(baseline) {
        Ok(lut) => lut,
        Err(e) => {
            tracing::warn!(path = %item.path.display(), error = %e, "look prediction failed; baseline only");
            return None;
        }
    };

    // Content-addressed in the cache, so a burst of near-identical frames
    // shares one file instead of each writing its own. Kept as `.cube` rather
    // than in memory alone so a look stays inspectable and reproducible by hand
    // in RawTherapee or darktable.
    let hash = lut.content_hash();
    let lut_dir = ctx.cache_dir.join("luts");
    if let Err(e) = std::fs::create_dir_all(&lut_dir) {
        tracing::warn!(dir = %lut_dir.display(), error = %e, "cannot create LUT cache directory");
    } else {
        let cube = lut_dir.join(format!("{hash}.cube"));
        if !cube.exists() {
            if let Err(e) = std::fs::write(&cube, lut.to_cube()) {
                tracing::warn!(path = %cube.display(), error = %e, "cannot write .cube");
            }
        }
    }
    out.lut_hash = Some(hash);

    let looked = crate::develop::lut_apply::apply_lut(baseline, &lut);

    // ⑤ guard
    if ctx.cfg.look.guard_iqa {
        if let Some(iqa) = ctx.hub.iqa.as_ref() {
            out.iqa_before = iqa.score(baseline).ok();
            out.iqa_after = iqa.score(&looked).ok();
        }
    }

    if guard_verdict(out.iqa_before, out.iqa_after, ctx.cfg.look.guard_margin) {
        out.applied = true;
        Some(looked)
    } else {
        tracing::info!(
            path = %item.path.display(),
            before = ?out.iqa_before,
            after = ?out.iqa_after,
            "look lowered quality past the margin; keeping baseline"
        );
        None
    }
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
