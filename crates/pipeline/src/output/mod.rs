//! Phase 6 — build the copy-based review tree from catalog data.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::catalog::{ReviewEntry, ReviewGroup};

/// Confidence at or above this is a confident "rejected" flag; below is "uncertain".
const CONFIDENCE_THRESHOLD: f32 = 0.6;

/// Replace a literal `<library>` token in `template` with `scan_root`.
#[allow(dead_code)]
pub(crate) fn substitute_library(template: &str, scan_root: &Path) -> PathBuf {
    PathBuf::from(template.replace("<library>", &scan_root.to_string_lossy()))
}

/// Map a flag to `(top_level_category, rejected_subfolder)`.
/// Returns `None` for unknown flag types. For "uncertain" the subfolder is "".
pub(crate) fn category_of(
    flag_type: &str,
    confidence: f32,
) -> Option<(&'static str, &'static str)> {
    // Known flag types and their rejected-subfolder names.
    let subfolder = match flag_type {
        "blur" => "blur",
        "back_focus" => "back_focus",
        "overexposed" => "overexposed",
        "underexposed" => "underexposed",
        "low_iqa" => "low_quality",
        _ => return None,
    };
    if confidence >= CONFIDENCE_THRESHOLD {
        Some(("rejected", subfolder))
    } else {
        Some(("uncertain", ""))
    }
}

/// A single link the tree should contain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlannedLink {
    /// Directory relative to the output root, e.g. "rejected/blur/2023-06".
    pub rel_dir: PathBuf,
    /// Absolute path to the original photo this link points at.
    pub original: PathBuf,
}

/// Build the full list of links the tree should contain, honoring `include`
/// (empty = all top-level categories).
pub(crate) fn plan_links(
    entries: &[ReviewEntry],
    groups: &[ReviewGroup],
    include: &[String],
) -> Vec<PlannedLink> {
    let wants = |top: &str| include.is_empty() || include.iter().any(|c| c == top);

    let mut links = Vec::new();

    for e in entries {
        let Some((top, subfolder)) = category_of(&e.flag_type, e.confidence) else {
            continue;
        };
        if !wants(top) {
            continue;
        }
        let rel_dir = if subfolder.is_empty() {
            // uncertain/<YYYY-MM>
            PathBuf::from(top).join(&e.year_month)
        } else {
            // rejected/<subfolder>/<YYYY-MM>
            PathBuf::from(top).join(subfolder).join(&e.year_month)
        };
        links.push(PlannedLink {
            rel_dir,
            original: e.path.clone(),
        });
    }

    if wants("duplicates") {
        for g in groups {
            let group_dir =
                PathBuf::from("duplicates").join(format!("group_{:05}_{}", g.group_id, g.date));
            if let Some(keeper) = &g.keeper {
                links.push(PlannedLink {
                    rel_dir: group_dir.join("_keeper"),
                    original: keeper.clone(),
                });
            }
            for other in &g.others {
                links.push(PlannedLink {
                    rel_dir: group_dir.join("_others"),
                    original: other.clone(),
                });
            }
        }
    }

    links
}

/// The marker file written at the root of every photopipe-managed tree.
const TREE_MARKER: &str = ".photopipe-tree";

/// Summary of a review-tree build.
#[derive(Debug, Default, Clone)]
pub struct ReviewTreeReport {
    pub files_copied: u64,
    pub files_skipped: u64,
    pub files_removed: u64,
    pub bytes_copied: u64,
    pub groups: u64,
    pub errors: u64,
}

/// Bytes/files a copy would write (excludes already-current destinations).
#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct CopyEstimate {
    pub files: u64,
    pub bytes: u64,
}

/// Internal result of materializing a set of planned copies.
#[derive(Debug, Default, Clone)]
struct CopyReport {
    copied: u64,
    skipped: u64,
    removed: u64,
    bytes: u64,
    errors: u64,
}

/// The single regular file the tool itself writes at the tree root.
const README_NAME: &str = "README.txt";

/// Outcome of a single `copy_file`.
enum CopyOutcome {
    /// File was copied; carries the byte count written.
    Copied(u64),
    /// Destination already current; nothing written.
    Skipped,
}

/// True if `dest` is missing or differs from `src` (size, or `src` is newer).
fn needs_copy(dest: &Path, src: &Path) -> bool {
    let (Ok(dm), Ok(sm)) = (std::fs::metadata(dest), std::fs::metadata(src)) else {
        return true; // dest missing (or src unstattable → attempt; copy will error & be counted)
    };
    if dm.len() != sm.len() {
        return true;
    }
    match (sm.modified(), dm.modified()) {
        (Ok(s), Ok(d)) => s > d,
        _ => false,
    }
}

/// Copy `src` to `dest` unless `dest` is already current. Originals are only
/// read. Returns the outcome (with byte count when copied).
fn copy_file(dest: &Path, src: &Path) -> anyhow::Result<CopyOutcome> {
    use anyhow::Context;
    let abs_src = src
        .canonicalize()
        .with_context(|| format!("original not found: {}", src.display()))?;
    if dest.exists() && !needs_copy(dest, &abs_src) {
        return Ok(CopyOutcome::Skipped);
    }
    let bytes = std::fs::copy(&abs_src, dest)
        .with_context(|| format!("copy {} -> {}", abs_src.display(), dest.display()))?;
    Ok(CopyOutcome::Copied(bytes))
}

/// Resolve the destination path for each planned item, de-duplicating basenames
/// per directory. Returns `(dest, src)` pairs. Pure — touches no filesystem.
fn resolve_dests(output_root: &Path, planned: &[PlannedLink]) -> Vec<(PathBuf, PathBuf)> {
    use std::collections::{HashMap, HashSet};
    let mut taken_per_dir: HashMap<PathBuf, HashSet<String>> = HashMap::new();
    let mut out = Vec::with_capacity(planned.len());
    for link in planned {
        let abs_dir = output_root.join(&link.rel_dir);
        let basename = link
            .original
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unnamed")
            .to_string();
        let taken = taken_per_dir.entry(abs_dir.clone()).or_default();
        let name = dedupe_name(taken, &basename);
        out.push((abs_dir.join(&name), link.original.clone()));
    }
    out
}

/// Sum the files/bytes a copy of `planned` into `output_root` would write,
/// excluding destinations that are already current.
fn estimate_copy(output_root: &Path, planned: &[PlannedLink]) -> CopyEstimate {
    let mut est = CopyEstimate::default();
    for (dest, src) in resolve_dests(output_root, planned) {
        let Ok(abs_src) = src.canonicalize() else {
            continue;
        };
        if needs_copy(&dest, &abs_src) {
            est.files += 1;
            est.bytes += std::fs::metadata(&abs_src).map(|m| m.len()).unwrap_or(0);
        }
    }
    est
}

/// True if `root` is a photopipe-managed tree (has the marker at its root).
fn is_managed_tree(root: &Path) -> bool {
    root.join(TREE_MARKER).exists()
}

/// True if `dir` exists and contains no entries.
fn dir_is_empty(dir: &Path) -> bool {
    std::fs::read_dir(dir)
        .map(|mut r| r.next().is_none())
        .unwrap_or(false)
}

/// Refuse to write into an existing non-empty directory that is not a
/// photopipe-managed tree (protects against pointing `--output` at real photos).
fn ensure_safe_to_write(output_root: &Path) -> anyhow::Result<()> {
    if !output_root.exists() || is_managed_tree(output_root) || dir_is_empty(output_root) {
        return Ok(());
    }
    anyhow::bail!(
        "refusing to write into {}: not a photopipe tree (missing {} marker). \
         Delete it manually if you intend to replace it.",
        output_root.display(),
        TREE_MARKER
    )
}

/// Remove a photopipe-managed tree wholesale. Refuses a non-empty directory
/// that lacks the marker.
fn remove_managed_tree(output_root: &Path) -> anyhow::Result<()> {
    if !output_root.exists() {
        return Ok(());
    }
    if !is_managed_tree(output_root) && !dir_is_empty(output_root) {
        anyhow::bail!(
            "refusing to delete {}: not a photopipe tree (missing {} marker). \
             Delete it manually if you intend to replace it.",
            output_root.display(),
            TREE_MARKER
        );
    }
    std::fs::remove_dir_all(output_root)
        .map_err(|e| anyhow::anyhow!("remove tree {}: {e}", output_root.display()))
}

/// Materialize `planned` as copies under `output_root`. Writes the marker +
/// `README.txt` at the root; when not regenerating, prunes entries no longer
/// expected. Shared core for the review and keepers trees.
fn materialize_copies(
    output_root: &Path,
    planned: &[PlannedLink],
    regenerate: bool,
    readme_body: &str,
) -> anyhow::Result<CopyReport> {
    use std::collections::HashSet;

    let mut report = CopyReport::default();

    if regenerate {
        remove_managed_tree(output_root)?;
        tracing::info!(root = %output_root.display(), "regenerate: removed existing tree");
    } else {
        ensure_safe_to_write(output_root)?;
    }
    std::fs::create_dir_all(output_root)
        .map_err(|e| anyhow::anyhow!("create output root {}: {e}", output_root.display()))?;

    let dests = resolve_dests(output_root, planned);
    let mut expected: HashSet<PathBuf> = HashSet::new();

    for (dest, src) in &dests {
        if let Some(parent) = dest.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                tracing::warn!(dir = %parent.display(), error = %e, "create dir failed");
                report.errors += 1;
                continue;
            }
        }
        match copy_file(dest, src) {
            Ok(CopyOutcome::Copied(b)) => {
                report.copied += 1;
                report.bytes += b;
            }
            Ok(CopyOutcome::Skipped) => report.skipped += 1,
            Err(e) => {
                tracing::warn!(dest = %dest.display(), error = %e, "copy failed");
                report.errors += 1;
                continue;
            }
        }
        expected.insert(dest.clone());
    }

    // Write the marker + README before pruning so they are preserved.
    std::fs::write(output_root.join(TREE_MARKER), b"photopipe managed tree\n")
        .map_err(|e| anyhow::anyhow!("write marker: {e}"))?;
    std::fs::write(output_root.join(README_NAME), readme_body)
        .map_err(|e| anyhow::anyhow!("write README.txt: {e}"))?;

    if !regenerate {
        prune_stale(output_root, &expected, &mut report);
    }

    Ok(report)
}

/// Within a managed tree, remove files (copies or stray symlinks) not in
/// `expected` and now-empty directories; the root marker and README are kept.
fn prune_stale(
    output_root: &Path,
    expected: &std::collections::HashSet<PathBuf>,
    report: &mut CopyReport,
) {
    prune_dir(output_root, output_root, expected, report);
}

fn prune_dir(
    root: &Path,
    dir: &Path,
    expected: &std::collections::HashSet<PathBuf>,
    report: &mut CopyReport,
) {
    let read = match std::fs::read_dir(dir) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(dir = %dir.display(), error = %e, "prune read_dir failed");
            report.errors += 1;
            return;
        }
    };
    for entry in read.flatten() {
        let path = entry.path();
        // Preserve the root-level marker and README.
        let is_root_meta = path.parent() == Some(root)
            && matches!(
                path.file_name().and_then(|n| n.to_str()),
                Some(TREE_MARKER) | Some(README_NAME)
            );
        if is_root_meta {
            continue;
        }
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.file_type().is_dir() {
            prune_dir(root, &path, expected, report);
            if dir_is_empty(&path) {
                let _ = std::fs::remove_dir(&path);
            }
        } else if !expected.contains(&path) {
            match std::fs::remove_file(&path) {
                Ok(()) => report.removed += 1,
                Err(e) => {
                    tracing::warn!(file = %path.display(), error = %e, "prune remove failed");
                    report.errors += 1;
                }
            }
        }
    }
}

/// The review tree's README body.
fn review_readme_body(db_path_hint: &str) -> String {
    format!(
        "PhotoPipe Review Tree\n\
=====================\n\
\n\
This directory was generated by photopipe and contains COPIES of flagged photos.\n\
Your originals are untouched; the catalog at {db} is the source of truth.\n\
This tree is safe to delete and can be rebuilt.\n",
        db = db_path_hint,
    )
}

/// Build (or incrementally update) the review tree at `output_root` by copying
/// flagged photos. Non-destructive: originals are only read.
pub fn build_review_tree(
    catalog: &crate::catalog::Catalog,
    output_root: &Path,
    include: &[String],
    regenerate: bool,
) -> anyhow::Result<ReviewTreeReport> {
    let entries = catalog.review_entries()?;
    let groups = catalog.duplicate_groups_for_review()?;
    let planned = plan_links(&entries, &groups, include);

    let group_count = groups
        .iter()
        .filter(|_| include.is_empty() || include.iter().any(|c| c == "duplicates"))
        .count() as u64;

    let readme = review_readme_body("the photopipe catalog");
    let core = materialize_copies(output_root, &planned, regenerate, &readme)?;

    let report = ReviewTreeReport {
        files_copied: core.copied,
        files_skipped: core.skipped,
        files_removed: core.removed,
        bytes_copied: core.bytes,
        groups: group_count,
        errors: core.errors,
    };
    tracing::info!(
        copied = report.files_copied,
        skipped = report.files_skipped,
        removed = report.files_removed,
        bytes = report.bytes_copied,
        groups = report.groups,
        errors = report.errors,
        "review tree built"
    );
    Ok(report)
}

/// Estimate the copy a `build_review_tree` would perform.
pub fn estimate_review_copy(
    catalog: &crate::catalog::Catalog,
    output_root: &Path,
    include: &[String],
) -> anyhow::Result<CopyEstimate> {
    let entries = catalog.review_entries()?;
    let groups = catalog.duplicate_groups_for_review()?;
    let planned = plan_links(&entries, &groups, include);
    Ok(estimate_copy(output_root, &planned))
}

/// Summary of a keepers-tree build.
#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct KeepersReport {
    pub files_copied: u64,
    pub files_skipped: u64,
    pub files_removed: u64,
    pub bytes_copied: u64,
    pub errors: u64,
}

/// Plan the keepers copy set (kept files → `<YYYY-MM>/<name>`).
fn keepers_plan(catalog: &crate::catalog::Catalog) -> anyhow::Result<Vec<PlannedLink>> {
    Ok(catalog
        .keeper_files()?
        .into_iter()
        .map(|k| PlannedLink {
            rel_dir: PathBuf::from(&k.year_month),
            original: k.path,
        })
        .collect())
}

const KEEPERS_README: &str = "PhotoPipe Keepers Export\n\
========================\n\
\n\
COPIES of the photos you kept in the review UI, organized by capture month.\n\
Your originals are untouched. This tree is regenerated from the catalog and\n\
is safe to delete.\n";

/// Build (or incrementally update) the keepers export tree at `output_root` by
/// copying kept photos. Non-destructive: originals are only read.
pub fn build_keepers_tree(
    catalog: &crate::catalog::Catalog,
    output_root: &Path,
    regenerate: bool,
) -> anyhow::Result<KeepersReport> {
    let planned = keepers_plan(catalog)?;
    let core = materialize_copies(output_root, &planned, regenerate, KEEPERS_README)?;
    let report = KeepersReport {
        files_copied: core.copied,
        files_skipped: core.skipped,
        files_removed: core.removed,
        bytes_copied: core.bytes,
        errors: core.errors,
    };
    tracing::info!(
        copied = report.files_copied,
        skipped = report.files_skipped,
        removed = report.files_removed,
        bytes = report.bytes_copied,
        errors = report.errors,
        "keepers tree built"
    );
    Ok(report)
}

/// Estimate the copy a `build_keepers_tree` would perform.
pub fn estimate_keepers_copy(
    catalog: &crate::catalog::Catalog,
    output_root: &Path,
) -> anyhow::Result<CopyEstimate> {
    let planned = keepers_plan(catalog)?;
    Ok(estimate_copy(output_root, &planned))
}

/// Format a byte count as a short human-readable string (e.g. `9.7 GB`).
pub fn humanize_bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[i])
    }
}

/// Return a collision-free link name for `basename` within a folder.
/// First use is the basename verbatim; subsequent collisions get " (2)",
/// " (3)", … inserted before the extension. Mutates `taken` to record the
/// returned name.
pub(crate) fn dedupe_name(taken: &mut HashSet<String>, basename: &str) -> String {
    if taken.insert(basename.to_string()) {
        return basename.to_string();
    }
    // Split into stem + extension (extension includes the leading dot).
    let (stem, ext) = match basename.rfind('.') {
        Some(idx) if idx > 0 => (&basename[..idx], &basename[idx..]),
        _ => (basename, ""),
    };
    let mut n = 2u32;
    loop {
        let candidate = format!("{stem} ({n}){ext}");
        if taken.insert(candidate.clone()) {
            return candidate;
        }
        n += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::{ReviewEntry, ReviewGroup};
    use std::collections::HashSet;
    use std::path::{Path, PathBuf};

    #[test]
    fn remove_managed_tree_refuses_unmarked_dir() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path().join("_review");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("sub/photo.jpg"), b"real").unwrap();
        // No .photopipe-tree marker -> refuse.
        let err = remove_managed_tree(&root).unwrap_err();
        assert!(
            err.to_string().contains("not a photopipe tree"),
            "unexpected: {err}"
        );
        assert!(root.join("sub/photo.jpg").exists());
    }

    #[test]
    fn remove_managed_tree_removes_marked_tree() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path().join("_review");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("sub/copy.jpg"), b"copy").unwrap();
        std::fs::write(root.join(TREE_MARKER), b"x").unwrap();
        remove_managed_tree(&root).unwrap();
        assert!(!root.exists());
    }

    #[test]
    fn copy_file_copies_then_skips() {
        let dir = tempfile::TempDir::new().unwrap();
        let src = dir.path().join("src.bin");
        std::fs::write(&src, b"hello world").unwrap();
        let dest = dir.path().join("out/dest.bin");
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();

        match copy_file(&dest, &src).unwrap() {
            CopyOutcome::Copied(n) => assert_eq!(n, 11),
            CopyOutcome::Skipped => panic!("first copy should write"),
        }
        assert_eq!(std::fs::read(&dest).unwrap(), b"hello world");
        // Second call: destination current -> skip.
        assert!(matches!(
            copy_file(&dest, &src).unwrap(),
            CopyOutcome::Skipped
        ));
    }

    #[test]
    fn humanize_bytes_formats() {
        assert_eq!(humanize_bytes(0), "0 B");
        assert_eq!(humanize_bytes(512), "512 B");
        assert_eq!(humanize_bytes(1024), "1.0 KB");
        assert_eq!(humanize_bytes(1536), "1.5 KB");
        assert_eq!(humanize_bytes(5 * 1024 * 1024), "5.0 MB");
    }

    #[test]
    fn substitute_library_replaces_token() {
        let p = substitute_library("<library>/_review", Path::new("/photos/2023"));
        assert_eq!(p, PathBuf::from("/photos/2023/_review"));
        let q = substitute_library("/fixed/out", Path::new("/photos/2023"));
        assert_eq!(q, PathBuf::from("/fixed/out"));
    }

    #[test]
    fn category_mapping() {
        assert_eq!(category_of("blur", 0.8), Some(("rejected", "blur")));
        assert_eq!(
            category_of("low_iqa", 0.9),
            Some(("rejected", "low_quality"))
        );
        assert_eq!(
            category_of("overexposed", 0.7),
            Some(("rejected", "overexposed"))
        );
        assert_eq!(category_of("blur", 0.4), Some(("uncertain", "")));
        assert_eq!(category_of("low_iqa", 0.59), Some(("uncertain", "")));
        assert_eq!(category_of("nonsense", 0.9), None);
    }

    #[test]
    fn plan_links_buckets_correctly() {
        let entries = vec![
            ReviewEntry {
                file_id: 1,
                path: PathBuf::from("/lib/a.jpg"),
                flag_type: "blur".into(),
                confidence: 0.8,
                year_month: "2023-06".into(),
            },
            ReviewEntry {
                file_id: 2,
                path: PathBuf::from("/lib/b.jpg"),
                flag_type: "low_iqa".into(),
                confidence: 0.9,
                year_month: "2023-06".into(),
            },
            ReviewEntry {
                file_id: 3,
                path: PathBuf::from("/lib/c.jpg"),
                flag_type: "blur".into(),
                confidence: 0.4,
                year_month: "unknown-date".into(),
            },
        ];
        let groups = vec![ReviewGroup {
            group_id: 42,
            date: "2023-06-15".into(),
            keeper: Some(PathBuf::from("/lib/k.jpg")),
            others: vec![PathBuf::from("/lib/o1.jpg"), PathBuf::from("/lib/o2.jpg")],
        }];
        let links = plan_links(&entries, &groups, &[]);
        let dirs: Vec<String> = links
            .iter()
            .map(|l| l.rel_dir.to_string_lossy().into_owned())
            .collect();
        assert!(dirs.contains(&"rejected/blur/2023-06".to_string()));
        assert!(dirs.contains(&"rejected/low_quality/2023-06".to_string()));
        assert!(dirs.contains(&"uncertain/unknown-date".to_string()));
        assert!(dirs.contains(&"duplicates/group_00042_2023-06-15/_keeper".to_string()));
        assert!(
            dirs.iter()
                .filter(|d| **d == "duplicates/group_00042_2023-06-15/_others")
                .count()
                == 2
        );
    }

    #[test]
    fn plan_links_include_filter() {
        let entries = vec![ReviewEntry {
            file_id: 1,
            path: PathBuf::from("/lib/a.jpg"),
            flag_type: "blur".into(),
            confidence: 0.8,
            year_month: "2023-06".into(),
        }];
        let groups = vec![ReviewGroup {
            group_id: 1,
            date: "2023-06-15".into(),
            keeper: Some(PathBuf::from("/lib/k.jpg")),
            others: vec![],
        }];
        let links = plan_links(&entries, &groups, &["duplicates".to_string()]);
        assert!(links.iter().all(|l| l.rel_dir.starts_with("duplicates")));
        assert!(!links.is_empty());
    }

    #[test]
    fn dedupe_name_suffixes_collisions() {
        let mut taken = HashSet::new();
        assert_eq!(dedupe_name(&mut taken, "IMG_1.ARW"), "IMG_1.ARW");
        assert_eq!(dedupe_name(&mut taken, "IMG_1.ARW"), "IMG_1 (2).ARW");
        assert_eq!(dedupe_name(&mut taken, "IMG_1.ARW"), "IMG_1 (3).ARW");
        assert_eq!(dedupe_name(&mut taken, "noext"), "noext");
        assert_eq!(dedupe_name(&mut taken, "noext"), "noext (2)");
    }
}
