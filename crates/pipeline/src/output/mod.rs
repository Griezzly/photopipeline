//! Phase 6 — build the symlink/hardlink review tree from catalog data.

// Items are pub(crate) for use by Task 3 (materialise) and Task 4 (re-export);
// they are not yet referenced outside this module, so suppress the lint.
#![allow(dead_code)]

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::catalog::{ReviewEntry, ReviewGroup};

/// Confidence at or above this is a confident "rejected" flag; below is "uncertain".
const CONFIDENCE_THRESHOLD: f32 = 0.6;

/// Replace a literal `<library>` token in `template` with `scan_root`.
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
