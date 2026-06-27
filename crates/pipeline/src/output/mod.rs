//! Phase 6 — build the symlink/hardlink review tree from catalog data.

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

use crate::config::{LinkType, OutputConfig};

/// Summary of a review-tree build.
#[derive(Debug, Default, Clone)]
pub struct ReviewTreeReport {
    pub links_created: u64,
    pub links_removed: u64,
    pub groups: u64,
    pub errors: u64,
}

/// Result of materializing a set of planned links.
#[derive(Debug, Default, Clone)]
pub(crate) struct MaterializeReport {
    pub created: u64,
    pub removed: u64,
    pub errors: u64,
}

/// The single regular file the tool itself writes at the tree root.
const README_NAME: &str = "README.txt";

/// Create one link (symlink or hardlink) at `link_path` pointing to the
/// absolute `original`. Idempotent: if a correct symlink already exists, it
/// is left as-is. Hardlink across filesystems fails loudly (EXDEV).
/// Returns `true` if a new link was created.
fn create_link(link_path: &Path, original: &Path, link_type: LinkType) -> anyhow::Result<bool> {
    use anyhow::Context;

    let abs_original = original
        .canonicalize()
        .with_context(|| format!("original not found: {}", original.display()))?;

    match link_type {
        LinkType::Symlink => {
            if let Ok(existing) = std::fs::read_link(link_path) {
                if existing == abs_original {
                    return Ok(false); // already correct
                }
                std::fs::remove_file(link_path)
                    .with_context(|| format!("replacing stale symlink {}", link_path.display()))?;
            }
            std::os::unix::fs::symlink(&abs_original, link_path).with_context(|| {
                format!(
                    "symlink {} -> {}",
                    link_path.display(),
                    abs_original.display()
                )
            })?;
            Ok(true)
        }
        LinkType::Hardlink => {
            if link_path.exists() {
                // A regular file already present; assume it's our hardlink.
                return Ok(false);
            }
            std::fs::hard_link(&abs_original, link_path).with_context(|| {
                format!(
                    "hardlink {} -> {} (cross-filesystem hardlinks are not supported)",
                    link_path.display(),
                    abs_original.display()
                )
            })?;
            Ok(true)
        }
    }
}

/// The review tree's README body.
fn review_readme_body(db_path_hint: &str) -> String {
    format!(
        "PhotoPipe Review Tree\n\
=====================\n\
\n\
This directory was generated by photopipe.\n\
DO NOT delete files in your photo library; this tree only contains links.\n\
\n\
The catalog at {db} is the source of truth.\n",
        db = db_path_hint,
    )
}

/// Materialize `planned` links under `output_root`, deduping basenames per
/// directory and (when not regenerating) pruning managed symlinks no longer
/// expected. Writes `readme_body` to `README.txt` at the root. This is the
/// shared core used by both the review tree and the keepers tree.
pub(crate) fn materialize_links(
    output_root: &Path,
    planned: &[PlannedLink],
    link_type: LinkType,
    regenerate: bool,
    readme_body: &str,
) -> anyhow::Result<MaterializeReport> {
    use std::collections::{HashMap, HashSet};

    let mut report = MaterializeReport::default();

    if regenerate {
        remove_managed_tree(output_root)?;
        tracing::info!(root = %output_root.display(), "regenerate: removed existing tree");
    }
    std::fs::create_dir_all(output_root)
        .map_err(|e| anyhow::anyhow!("create output root {}: {e}", output_root.display()))?;

    let mut expected: HashSet<PathBuf> = HashSet::new();
    let mut taken_per_dir: HashMap<PathBuf, HashSet<String>> = HashMap::new();

    for link in planned {
        let abs_dir = output_root.join(&link.rel_dir);
        if let Err(e) = std::fs::create_dir_all(&abs_dir) {
            tracing::warn!(dir = %abs_dir.display(), error = %e, "create dir failed");
            report.errors += 1;
            continue;
        }
        let basename = link
            .original
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unnamed")
            .to_string();
        let taken = taken_per_dir.entry(abs_dir.clone()).or_default();
        let name = dedupe_name(taken, &basename);
        let link_path = abs_dir.join(&name);

        match create_link(&link_path, &link.original, link_type) {
            Ok(true) => report.created += 1,
            Ok(false) => {}
            Err(e) => {
                tracing::warn!(link = %link_path.display(), error = %e, "create link failed");
                report.errors += 1;
                continue;
            }
        }
        expected.insert(link_path);
    }

    if !regenerate {
        // prune_stale_links counts removals into a ReviewTreeReport; adapt by
        // using a local ReviewTreeReport and copying the removed count out.
        let mut tmp = ReviewTreeReport::default();
        prune_stale_links(output_root, &expected, &mut tmp)?;
        report.removed = tmp.links_removed;
    }

    std::fs::write(output_root.join(README_NAME), readme_body)
        .map_err(|e| anyhow::anyhow!("write README.txt: {e}"))?;

    Ok(report)
}

/// Recursively delete a tree the tool manages. Refuses (errors, deletes
/// nothing) if it encounters any regular file that is NOT a managed
/// `README.txt` at the root — protecting against pointing the output at a
/// real directory. Symlinks and directories are removed.
fn remove_managed_tree(output_root: &Path) -> anyhow::Result<()> {
    if !output_root.exists() {
        return Ok(());
    }
    // First pass: verify there are no foreign regular files anywhere.
    check_no_foreign_files(output_root, output_root)?;
    // Safe to delete the whole subtree.
    std::fs::remove_dir_all(output_root)
        .map_err(|e| anyhow::anyhow!("remove tree {}: {e}", output_root.display()))
}

/// Walk `dir`; error if any entry is a regular (non-symlink) file other than
/// the root-level README.txt.
fn check_no_foreign_files(root: &Path, dir: &Path) -> anyhow::Result<()> {
    for entry in
        std::fs::read_dir(dir).map_err(|e| anyhow::anyhow!("read_dir {}: {e}", dir.display()))?
    {
        let entry = entry.map_err(|e| anyhow::anyhow!("dir entry: {e}"))?;
        let path = entry.path();
        // symlink_metadata does NOT follow symlinks.
        let meta = std::fs::symlink_metadata(&path)
            .map_err(|e| anyhow::anyhow!("stat {}: {e}", path.display()))?;
        let ft = meta.file_type();
        if ft.is_symlink() {
            continue; // managed link
        }
        if ft.is_dir() {
            check_no_foreign_files(root, &path)?;
            continue;
        }
        // Regular file: only a root-level README.txt is allowed.
        let is_root_readme = path.parent() == Some(root)
            && path.file_name().and_then(|n| n.to_str()) == Some(README_NAME);
        if !is_root_readme {
            anyhow::bail!(
                "refusing to delete {}: contains a non-symlink regular file not created by photopipe",
                root.display()
            );
        }
    }
    Ok(())
}

/// Build (or incrementally update) the review tree at `output_root`.
///
/// `regenerate = true` deletes the managed tree first, then rebuilds.
/// `regenerate = false` ensures every expected link exists and prunes any
/// managed symlink whose target is no longer expected (incremental).
/// Non-destructive: only links/dirs inside `output_root` are ever touched.
pub fn build_review_tree(
    catalog: &crate::catalog::Catalog,
    output_root: &Path,
    cfg: &OutputConfig,
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
    let core = materialize_links(output_root, &planned, cfg.link_type, regenerate, &readme)?;

    let report = ReviewTreeReport {
        links_created: core.created,
        links_removed: core.removed,
        groups: group_count,
        errors: core.errors,
    };
    tracing::info!(
        created = report.links_created,
        removed = report.links_removed,
        groups = report.groups,
        errors = report.errors,
        "review tree built"
    );
    Ok(report)
}

/// Summary of a keepers-tree build.
#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct KeepersReport {
    pub links_created: u64,
    pub links_removed: u64,
    pub errors: u64,
}

/// Build (or incrementally update) the keepers export tree at `output_root`:
/// `keepers/<YYYY-MM>/<original-name>` links to every file the user kept.
pub fn build_keepers_tree(
    catalog: &crate::catalog::Catalog,
    output_root: &Path,
    cfg: &OutputConfig,
    regenerate: bool,
) -> anyhow::Result<KeepersReport> {
    let kept = catalog.keeper_files()?;
    let planned: Vec<PlannedLink> = kept
        .into_iter()
        .map(|k| PlannedLink {
            rel_dir: PathBuf::from(&k.year_month),
            original: k.path,
        })
        .collect();

    let readme = "PhotoPipe Keepers Export\n\
========================\n\
\n\
Links to the photos you kept in the review UI, organized by capture month.\n\
This tree is regenerated from the catalog and is safe to delete.\n";

    let core = materialize_links(output_root, &planned, cfg.link_type, regenerate, readme)?;
    let report = KeepersReport {
        links_created: core.created,
        links_removed: core.removed,
        errors: core.errors,
    };
    tracing::info!(
        created = report.links_created,
        removed = report.links_removed,
        errors = report.errors,
        "keepers tree built"
    );
    Ok(report)
}

/// Walk the tree; remove any symlink whose absolute path is not in `expected`.
/// Then remove now-empty directories. Never removes regular files.
fn prune_stale_links(
    output_root: &Path,
    expected: &std::collections::HashSet<PathBuf>,
    report: &mut ReviewTreeReport,
) -> anyhow::Result<()> {
    prune_dir(output_root, output_root, expected, report)
}

fn prune_dir(
    _root: &Path,
    dir: &Path,
    expected: &std::collections::HashSet<PathBuf>,
    report: &mut ReviewTreeReport,
) -> anyhow::Result<()> {
    let read = match std::fs::read_dir(dir) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(dir = %dir.display(), error = %e, "prune read_dir failed");
            report.errors += 1;
            return Ok(());
        }
    };
    for entry in read {
        let entry = entry.map_err(|e| anyhow::anyhow!("dir entry: {e}"))?;
        let path = entry.path();
        let meta = std::fs::symlink_metadata(&path)
            .map_err(|e| anyhow::anyhow!("stat {}: {e}", path.display()))?;
        let ft = meta.file_type();
        if ft.is_symlink() {
            if !expected.contains(&path) {
                if let Err(e) = std::fs::remove_file(&path) {
                    tracing::warn!(link = %path.display(), error = %e, "prune remove failed");
                    report.errors += 1;
                } else {
                    report.links_removed += 1;
                }
            }
        } else if ft.is_dir() {
            prune_dir(_root, &path, expected, report)?;
            // Remove the directory if it is now empty.
            if let Ok(mut it) = std::fs::read_dir(&path) {
                if it.next().is_none() {
                    let _ = std::fs::remove_dir(&path);
                }
            }
        }
        // Regular files (e.g. root README.txt) are left untouched here.
    }
    Ok(())
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
    fn remove_managed_tree_refuses_foreign_regular_file() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path().join("_review");
        std::fs::create_dir_all(root.join("rejected/blur/2023-06")).unwrap();
        // A foreign regular file the tool did not create.
        std::fs::write(root.join("rejected/blur/2023-06/notes.txt"), b"hi").unwrap();

        let err = remove_managed_tree(&root).unwrap_err();
        assert!(
            err.to_string().contains("non-symlink regular file"),
            "unexpected error: {err}"
        );
        // The foreign file must still exist.
        assert!(root.join("rejected/blur/2023-06/notes.txt").exists());
    }

    #[test]
    fn remove_managed_tree_deletes_symlinks_and_dirs() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path().join("_review");
        let target = dir.path().join("orig.jpg");
        std::fs::write(&target, b"jpeg").unwrap();
        std::fs::create_dir_all(root.join("rejected/blur/2023-06")).unwrap();
        std::os::unix::fs::symlink(&target, root.join("rejected/blur/2023-06/orig.jpg")).unwrap();
        std::fs::write(root.join("README.txt"), b"readme").unwrap();

        remove_managed_tree(&root).unwrap();
        assert!(!root.exists(), "tree root should be gone");
        assert!(target.exists(), "original must be untouched");
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
