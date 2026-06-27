# Native Windows via Copy-Always Export — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make photopipe build and run natively on Windows by replacing the symlink/hardlink review & keepers trees with a single cross-platform **copy** strategy, fixing platform-aware path resolution, and surfacing how much data a copy will write (before it runs).

**Architecture:** The only Unix-specific code is `std::os::unix::fs::symlink` in `output/mod.rs`; removing symlink/hardlink modes in favor of `std::fs::copy` makes the crate portable. Default paths move from hand-rolled XDG to the `dirs` crate. Because copied trees contain real files (not symlinks), the "never delete a non-photopipe dir" guard switches from a symlink check to a root **marker file** (`.photopipe-tree`). A pre-flight estimate sums the bytes that will actually be copied and is surfaced on the CLI (printed) and in the web UI (a confirm dialog).

**Tech Stack:** Rust (edition 2021), DuckDB, axum/tokio, `image`/`webp`, new dep `dirs` (MIT/Apache). Frontend: zero-build vanilla JS.

## Global Constraints

Apply to **every** task (from the spec and `CLAUDE.md`):

- **No mutation of original photo files** — originals are only read; copy writes new files elsewhere.
- **DuckDB only. No SQLite. No Python at runtime.**
- **No AGPL deps.** New dep `dirs` is MIT/Apache.
- **Copy always:** `review-tree` and `export-keepers` copy files; symlink/hardlink modes and the `LinkType` enum / `[output] link_type` key are removed. Removing the key must not break existing configs (serde ignores unknown fields — do **not** add `#[serde(deny_unknown_fields)]`).
- **Non-destructive guard via marker:** a managed tree has a `.photopipe-tree` file at its root. `--regenerate` and pruning act only on a directory that is empty or carries the marker; otherwise they refuse with an error.
- **Data-volume awareness, always:** CLI prints `Copying <N> files (<H>) → <output> …` before copying; the web Export button confirms with an estimate first. `<H>` is human-readable via `humanize_bytes`.
- **A single corrupt/failed file never aborts a run:** per-file errors `tracing::warn!(path, error)` and increment an error counter; continue.
- **Errors:** `anyhow::Result` at CLI/HTTP boundaries; `thiserror` inside the pipeline crate. `tracing` not `println!` except user-facing CLI output.
- **Server binds `127.0.0.1` only.** The estimate endpoint is read-only (writes nothing).
- **Windows is compile-validated only here:** `cargo check --target x86_64-pc-windows-gnu` must pass; native-Windows *runtime* is a deferred follow-up.
- **Before done:** `cargo fmt`, `cargo clippy --all-targets --all-features -- -D warnings`, `cargo test --all` green.
- **One git commit per task**, conventional-commit style.

---

## File Structure

- `Cargo.toml` (workspace) + `crates/pipeline/Cargo.toml` — add `dirs`.
- `crates/pipeline/src/config.rs` — portable paths via `dirs` (Task 1); remove `LinkType` + `OutputConfig.link_type` (Task 3).
- `crates/pipeline/src/output/mod.rs` — copy-always core, marker guard, estimate + humanize helpers, report structs (Task 2).
- `crates/pipeline/src/lib.rs` — re-export new symbols (Task 2).
- `crates/pipeline/tests/review_tree.rs`, `crates/pipeline/tests/decisions.rs` — port to copy semantics (Task 2); drop `LinkType` (Task 3 — actually removed in Task 2 by dropping the `cfg` arg).
- `crates/cli/src/main.rs` — call-site updates (Task 2); pre-flight estimate + report (Task 4).
- `crates/cli/src/serve/handlers.rs` — call-site + report (Task 2); estimate endpoint (Task 5).
- `crates/cli/src/serve/mod.rs` — estimate route (Task 5).
- `crates/cli/assets/app.js` — export confirm-with-estimate (Task 6).
- `crates/cli/tests/cli.rs`, `crates/cli/tests/serve.rs` — updated assertions (Tasks 4, 5).
- `photopipe.example.toml`, `README.md` — docs (Task 7).

---

## Task 1: Add `dirs` + portable path resolution

**Files:**
- Modify: `Cargo.toml` (workspace `[workspace.dependencies]`)
- Modify: `crates/pipeline/Cargo.toml`
- Modify: `crates/pipeline/src/config.rs`
- Test: `crates/pipeline/src/config.rs` (inline `#[cfg(test)]`)

**Interfaces:**
- Consumes: nothing new.
- Produces: `default_config_path()`, `CatalogConfig::default()`, and `expand_tilde()` now resolve via `dirs`. No signature changes.

- [ ] **Step 1: Add the dependency**

In `Cargo.toml` under `[workspace.dependencies]` add:

```toml
dirs        = "5"
```

In `crates/pipeline/Cargo.toml` under `[dependencies]` add:

```toml
dirs         = { workspace = true }
```

- [ ] **Step 2: Write the failing test**

Add to the `#[cfg(test)] mod tests` in `crates/pipeline/src/config.rs`:

```rust
    #[test]
    fn default_paths_are_absolute_and_namespaced() {
        let cfg = Config::default();
        assert!(cfg.catalog.db_path.is_absolute(), "db_path: {:?}", cfg.catalog.db_path);
        assert!(cfg.catalog.cache_dir.is_absolute(), "cache_dir: {:?}", cfg.catalog.cache_dir);
        assert!(cfg.catalog.db_path.to_string_lossy().contains("photopipe"));
        assert!(cfg.catalog.cache_dir.to_string_lossy().contains("photopipe"));
        assert!(default_config_path().to_string_lossy().contains("photopipe"));
        assert!(default_config_path().to_string_lossy().ends_with("photopipe.toml"));
    }
```

- [ ] **Step 3: Run test to verify it fails**

Run: `. ~/.cargo/env && cargo test -p pipeline --lib config::tests::default_paths_are_absolute_and_namespaced`
Expected: FAIL — defaults currently fall back to a relative `~` path when `HOME` resolution differs (and on the CI/sandbox the assertion exercises the new code that doesn't exist yet). (If it happens to pass, proceed — Step 4 still replaces the implementation.)

- [ ] **Step 4: Replace the XDG helpers with `dirs`**

In `crates/pipeline/src/config.rs`, replace the three `xdg_*` functions and `home_dir` with `dirs`-backed versions, and update the two call sites (`default_config_path`, `CatalogConfig::default`) and `expand_tilde`.

Replace the bodies of `default_config_path`:

```rust
/// Default config-file path: `<config dir>/photopipe/photopipe.toml`.
pub fn default_config_path() -> PathBuf {
    config_root().join("photopipe/photopipe.toml")
}
```

Replace `CatalogConfig::default` paths:

```rust
impl Default for CatalogConfig {
    fn default() -> Self {
        Self {
            db_path: data_root().join("photopipe/catalog.duckdb"),
            cache_dir: cache_root().join("photopipe"),
            write_batch_size: 64,
            enable_vss: false,
        }
    }
}
```

Replace `expand_tilde` + the helper functions (delete `xdg_config_home`, `xdg_data_home`, `xdg_cache_home`, `home_dir`) with:

```rust
/// Expand a leading `~/` to the real home directory.
pub fn expand_tilde(p: &Path) -> PathBuf {
    let s = p.to_string_lossy();
    if let Some(rest) = s.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    p.to_path_buf()
}

/// Per-OS config dir (Linux `~/.config`, macOS `~/Library/Application Support`,
/// Windows `%APPDATA%`); falls back to the current dir if undeterminable.
fn config_root() -> PathBuf {
    dirs::config_dir().unwrap_or_else(|| PathBuf::from("."))
}

/// Per-OS data dir (Linux `~/.local/share`, macOS `~/Library/Application Support`,
/// Windows `%APPDATA%`).
fn data_root() -> PathBuf {
    dirs::data_dir().unwrap_or_else(|| PathBuf::from("."))
}

/// Per-OS cache dir (Linux `~/.cache`, macOS `~/Library/Caches`,
/// Windows `%LOCALAPPDATA%`).
fn cache_root() -> PathBuf {
    dirs::cache_dir().unwrap_or_else(|| PathBuf::from("."))
}
```

(`use std::path::{Path, PathBuf};` is already at the top of the file.)

- [ ] **Step 5: Run tests to verify they pass**

Run: `. ~/.cargo/env && cargo test -p pipeline --lib config:: && cargo clippy -p pipeline --all-targets -- -D warnings`
Expected: PASS, clippy clean (no leftover unused-function warnings from the deleted helpers).

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml crates/pipeline/Cargo.toml crates/pipeline/src/config.rs
git commit -m "feat(config): portable default paths via dirs (Windows/macOS/Linux)"
```

---

## Task 2: Copy-always materialization core

**Files:**
- Modify: `crates/pipeline/src/output/mod.rs`
- Modify: `crates/pipeline/src/lib.rs`
- Modify: `crates/cli/src/main.rs` (call sites only — compile)
- Modify: `crates/cli/src/serve/handlers.rs` (call site only — compile)
- Test: `crates/pipeline/src/output/mod.rs` (inline), `crates/pipeline/tests/review_tree.rs`, `crates/pipeline/tests/decisions.rs`

**Interfaces:**
- Consumes: `PlannedLink`, `plan_links`, `dedupe_name`, `Catalog::{review_entries, duplicate_groups_for_review, keeper_files}`.
- Produces (public):
  - `pub struct ReviewTreeReport { pub files_copied: u64, pub files_skipped: u64, pub files_removed: u64, pub bytes_copied: u64, pub groups: u64, pub errors: u64 }`
  - `pub struct KeepersReport { pub files_copied: u64, pub files_skipped: u64, pub files_removed: u64, pub bytes_copied: u64, pub errors: u64 }` (derives `serde::Serialize`)
  - `pub struct CopyEstimate { pub files: u64, pub bytes: u64 }` (derives `Default, serde::Serialize`)
  - `pub fn build_review_tree(catalog: &Catalog, output_root: &Path, include: &[String], regenerate: bool) -> anyhow::Result<ReviewTreeReport>`
  - `pub fn build_keepers_tree(catalog: &Catalog, output_root: &Path, regenerate: bool) -> anyhow::Result<KeepersReport>`
  - `pub fn estimate_review_copy(catalog: &Catalog, output_root: &Path, include: &[String]) -> anyhow::Result<CopyEstimate>`
  - `pub fn estimate_keepers_copy(catalog: &Catalog, output_root: &Path) -> anyhow::Result<CopyEstimate>`
  - `pub fn humanize_bytes(n: u64) -> String`
  - Re-exported from `lib.rs`.

- [ ] **Step 1: Replace the link machinery with copy machinery**

In `crates/pipeline/src/output/mod.rs`:

(a) Change the import line `use crate::config::{LinkType, OutputConfig};` to:

```rust
use crate::config::OutputConfig;
```

(`OutputConfig` import may become unused after this task — if clippy flags it, remove it entirely. The build functions no longer take it.)

(b) Replace the `ReviewTreeReport` + `MaterializeReport` structs (the block starting `/// Summary of a review-tree build.` through the end of `MaterializeReport`) with:

```rust
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
```

(c) Replace the entire `create_link` function with the copy primitives:

```rust
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
        let Ok(abs_src) = src.canonicalize() else { continue };
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
    std::fs::read_dir(dir).map(|mut r| r.next().is_none()).unwrap_or(false)
}
```

- [ ] **Step 2: Replace the guard + materialization + prune functions**

Still in `output/mod.rs`, replace `materialize_links`, `remove_managed_tree`, `check_no_foreign_files`, `prune_stale_links`, and `prune_dir` with the copy versions:

```rust
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
fn prune_stale(output_root: &Path, expected: &std::collections::HashSet<PathBuf>, report: &mut CopyReport) {
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
```

- [ ] **Step 3: Update the README bodies and build/estimate functions + `humanize_bytes`**

Update `review_readme_body` to mention copies, and replace `build_review_tree`, `build_keepers_tree`, and the `KeepersReport` struct; add the estimate functions and `humanize_bytes`. Replace from `fn review_readme_body` through the end of `build_keepers_tree` with:

```rust
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
```

- [ ] **Step 4: Update the inline unit tests in `output/mod.rs`**

Replace the two existing tests `remove_managed_tree_refuses_foreign_regular_file` and `remove_managed_tree_deletes_symlinks_and_dirs` with marker-based + copy tests:

```rust
    #[test]
    fn remove_managed_tree_refuses_unmarked_dir() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path().join("_review");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("sub/photo.jpg"), b"real").unwrap();
        // No .photopipe-tree marker -> refuse.
        let err = remove_managed_tree(&root).unwrap_err();
        assert!(err.to_string().contains("not a photopipe tree"), "unexpected: {err}");
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
        assert!(matches!(copy_file(&dest, &src).unwrap(), CopyOutcome::Skipped));
    }

    #[test]
    fn humanize_bytes_formats() {
        assert_eq!(humanize_bytes(0), "0 B");
        assert_eq!(humanize_bytes(512), "512 B");
        assert_eq!(humanize_bytes(1024), "1.0 KB");
        assert_eq!(humanize_bytes(1536), "1.5 KB");
        assert_eq!(humanize_bytes(5 * 1024 * 1024), "5.0 MB");
    }
```

- [ ] **Step 5: Re-export new symbols from `lib.rs`**

In `crates/pipeline/src/lib.rs`, change the output re-export line to:

```rust
pub use output::{
    build_keepers_tree, build_review_tree, estimate_keepers_copy, estimate_review_copy,
    humanize_bytes, CopyEstimate, KeepersReport, ReviewTreeReport,
};
```

- [ ] **Step 6: Fix call sites so the workspace compiles (minimal — Tasks 4/5 enrich them)**

In `crates/cli/src/main.rs`, `cmd_review_tree`: replace the `build_review_tree(...)` call and the print block with:

```rust
    let report = build_review_tree(&catalog, &output, &include, regenerate)?;

    println!("Review tree: {}", output.display());
    println!("  Copied  : {} files ({})", report.files_copied, pipeline::humanize_bytes(report.bytes_copied));
    println!("  Skipped : {}", report.files_skipped);
    println!("  Removed : {}", report.files_removed);
    println!("  Groups  : {}", report.groups);
    println!("  Errors  : {}", report.errors);
    Ok(())
```

(Also delete the stale comment `// We still pass cfg.output because it carries link_type.`)

In `cmd_export_keepers`, replace the `build_keepers_tree(...)` call + print with:

```rust
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
```

In `crates/cli/src/serve/handlers.rs`, `post_export`: change the `build_keepers_tree` call to drop the `&cfg.output` argument:

```rust
    tokio::task::spawn_blocking(move || build_keepers_tree(&catalog, &out, regenerate))
```

(`cfg`/`state.cfg` may now be unused inside `post_export` — if clippy flags it, remove the `let cfg = state.cfg.clone();` line.)

- [ ] **Step 7: Port the integration tests to copy semantics**

In `crates/pipeline/tests/review_tree.rs`:

Change the `out_cfg` helper and its callers — since `build_review_tree` no longer takes a config, delete `out_cfg` and the `LinkType`/`KeeperStrategy`/`OutputConfig` import, and change every `build_review_tree(&catalog, &out, &out_cfg(LinkType::Symlink), &[], false)` to `build_review_tree(&catalog, &out, &[], false)` (and the `..., &[], true)` regenerate call likewise). Update the import line:

```rust
use pipeline::{
    build_review_tree,
    catalog::{Catalog, DuplicateMember},
    defect::DefectFlag,
    ingest::{ExifData, FileFormat, IngestedFile},
};
```

Update the symlink-specific assertions:

In the main tree test, replace the `blur_link` symlink assertions with a copy + byte-equality check:

```rust
    // rejected/blur/2023-06/blur.jpg is a COPY of the original (byte-identical).
    let blur_link = out.join("rejected/blur/2023-06/blur.jpg");
    assert!(blur_link.is_file());
    assert!(!std::fs::symlink_metadata(&blur_link).unwrap().file_type().is_symlink());
    assert_eq!(std::fs::read(&blur_link).unwrap(), std::fs::read(&blur_path).unwrap());
```

Replace the keeper canonicalize assertion with a content check:

```rust
    let kdir = out.join(&group_dir).join("_keeper");
    assert!(kdir.join("keeper.jpg").is_file());
    assert_eq!(
        std::fs::read(kdir.join("keeper.jpg")).unwrap(),
        std::fs::read(&keeper_path).unwrap()
    );
```

Replace the `report.links_created`/`links_removed` field reads: `report.links_created` → `report.files_copied`, `r1.links_created` → `r1.files_copied`, `r2.links_created` → `r2.files_copied`, `r.links_removed` → `r.files_removed`.

In `non_destructive_originals_unchanged`, replace the "delete a symlink" tail with a copy-tree version:

```rust
    // Deleting a copy in the tree leaves the original intact.
    let copy_in_tree = out.join("rejected/blur/2023-06/a.jpg");
    assert!(copy_in_tree.is_file());
    std::fs::remove_file(&copy_in_tree).unwrap();
    assert!(orig.exists(), "deleting a copy must not delete the original");
    assert_eq!(std::fs::read(&orig).unwrap().len() > 0, true);
```

In `incremental_prunes_stale_links`, replace the planted-stale-symlink with a planted-stale-copy (a regular file), and update the field name:

```rust
    // Plant a stale COPY the planner would never produce.
    let stale_dir = out.join("rejected/blur/2099-01");
    fs::create_dir_all(&stale_dir).unwrap();
    fs::write(stale_dir.join("ghost.jpg"), b"stale").unwrap();

    let r = build_review_tree(&catalog, &out, &[], false).unwrap();
    assert_eq!(r.files_removed, 1, "stale copy should be pruned");
    assert!(!stale_dir.join("ghost.jpg").exists());
```

For any other `out_cfg(LinkType::Symlink)` occurrences (the collision and include-filter and unknown-date tests), apply the same `build_review_tree(&catalog, &out, &[], false)` substitution and `links_created`→`files_copied` rename. The collision test asserts two distinct destination files exist — those assertions (`.exists()`) are unchanged.

In `crates/pipeline/tests/decisions.rs`:

Update the keepers test `keepers_tree_links_only_kept_files`: drop the `out_cfg()`/`LinkType`/`KeeperStrategy`/`OutputConfig` import and the `out_cfg` helper; change the two `build_keepers_tree(&catalog, &out, &out_cfg(), false)` calls to `build_keepers_tree(&catalog, &out, false)`; change `report.links_created`/`report2.links_created` to `report.files_copied`/`report2.files_copied`. The assertion that `found == vec!["keep.jpg"]` stays, but note the tree now also contains `.photopipe-tree` + `README.txt` at the root — the test only reads month-subdir entries, so it is unaffected.

- [ ] **Step 8: Run the full pipeline suite + dependent build**

Run:
```bash
. ~/.cargo/env
cargo test -p pipeline
cargo build -p photopipe
cargo clippy -p pipeline --all-targets -- -D warnings
```
Expected: all pipeline lib + integration tests pass; the CLI crate compiles; clippy clean. (Note: `LinkType` still exists in `config.rs` — removed in Task 3 — but nothing references it now except `config.rs` itself and possibly an unused import there; that is handled in Task 3.)

- [ ] **Step 9: Commit**

```bash
git add crates/pipeline/src/output/mod.rs crates/pipeline/src/lib.rs \
        crates/pipeline/tests/review_tree.rs crates/pipeline/tests/decisions.rs \
        crates/cli/src/main.rs crates/cli/src/serve/handlers.rs
git commit -m "feat(output): copy-always trees with marker guard, estimate, humanize_bytes"
```

---

## Task 3: Remove the `LinkType` enum + `link_type` config key

**Files:**
- Modify: `crates/pipeline/src/config.rs`
- Test: `crates/pipeline/src/config.rs` (inline)

**Interfaces:**
- Consumes: nothing.
- Produces: `OutputConfig` without a `link_type` field; `LinkType` no longer exists.

- [ ] **Step 1: Write the failing test**

Add to `config.rs` tests:

```rust
    #[test]
    fn legacy_link_type_key_is_ignored() {
        // Old configs carried [output] link_type — it must parse without error now.
        let toml_str = r#"
            [output]
            link_type = "hardlink"
            review_tree = "<library>/_review"
        "#;
        let cfg: Config = toml::from_str(toml_str).expect("legacy link_type should be ignored");
        assert_eq!(cfg.output.review_tree, "<library>/_review");
    }
```

- [ ] **Step 2: Run it to verify it fails**

Run: `. ~/.cargo/env && cargo test -p pipeline --lib config::tests::legacy_link_type_key_is_ignored`
Expected: FAIL to compile or parse while `link_type` is still a field (serde maps the key) — actually it currently parses *because* the field exists; the test passes today but will keep passing after removal (proving the no-breakage requirement). Run it now to confirm it passes; it is a guard for Step 3.

- [ ] **Step 3: Remove `LinkType` and the field**

In `crates/pipeline/src/config.rs`:

Remove `link_type` from `OutputConfig`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct OutputConfig {
    /// Literal `<library>` is substituted with the scan root at runtime.
    pub review_tree: String,
    pub keeper_strategy: KeeperStrategy,
}

impl Default for OutputConfig {
    fn default() -> Self {
        Self {
            review_tree: "<library>/_review".into(),
            keeper_strategy: KeeperStrategy::Iqa,
        }
    }
}
```

Delete the entire `LinkType` enum definition:

```rust
// DELETE:
// #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
// #[serde(rename_all = "lowercase")]
// pub enum LinkType { Symlink, Hardlink }
```

- [ ] **Step 4: Run tests**

Run: `. ~/.cargo/env && cargo test -p pipeline --lib config:: && cargo build --workspace`
Expected: PASS; the workspace compiles (no remaining `LinkType` references — Task 2 removed them all from `output/`, tests, and call sites).

- [ ] **Step 5: Commit**

```bash
git add crates/pipeline/src/config.rs
git commit -m "refactor(config): drop LinkType enum and link_type key (copy-always)"
```

---

## Task 4: CLI pre-flight estimate + report

**Files:**
- Modify: `crates/cli/src/main.rs`
- Test: `crates/cli/tests/cli.rs`

**Interfaces:**
- Consumes: `pipeline::{estimate_review_copy, estimate_keepers_copy, humanize_bytes, build_review_tree, build_keepers_tree}`, `config::expand_tilde`.
- Produces: `cmd_review_tree` / `cmd_export_keepers` print `Copying <N> files (<H>) → <output> …` before copying, then the report.

- [ ] **Step 1: Write the failing test**

In `crates/cli/tests/cli.rs`, update `export_keepers_creates_tree` (or add a new assertion) to check the pre-flight message and that the kept file is a real copied file. Replace the command-run + assertions with:

```rust
    let output_run = Command::new(env!("CARGO_BIN_EXE_photopipe"))
        .args(["--config", cfg_path.to_str().unwrap(), "export-keepers", out.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(output_run.status.success());
    let stdout = String::from_utf8_lossy(&output_run.stdout);
    assert!(stdout.contains("Copying"), "expected a pre-flight estimate line, got: {stdout}");
    assert!(stdout.contains("Copied"), "expected a final report line, got: {stdout}");

    // a.jpg is a real copied file (not a symlink), byte-identical to the original.
    let entries = walkdir_like(&out);
    assert!(entries.iter().any(|n| n == "a.jpg"));
    let copied = find_file(&out, "a.jpg").expect("a.jpg copied");
    assert!(!std::fs::symlink_metadata(&copied).unwrap().file_type().is_symlink());
```

Add this helper next to `walkdir_like` in `cli.rs`:

```rust
fn find_file(root: &std::path::Path, name: &str) -> Option<std::path::PathBuf> {
    for e in std::fs::read_dir(root).ok()?.flatten() {
        let p = e.path();
        if p.is_dir() {
            if let Some(found) = find_file(&p, name) {
                return Some(found);
            }
        } else if p.file_name().and_then(|n| n.to_str()) == Some(name) {
            return Some(p);
        }
    }
    None
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `. ~/.cargo/env && cargo test -p photopipe --test cli export_keepers_creates_tree`
Expected: FAIL — current output prints "Keepers tree:" not "Copying"/"Copied".

- [ ] **Step 3: Add the estimate pre-flight to both handlers**

In `crates/cli/src/main.rs`, `cmd_review_tree`, insert before the `build_review_tree` call (after opening the catalog):

```rust
    let est = pipeline::estimate_review_copy(&catalog, &output, &include)?;
    println!(
        "Copying {} files ({}) → {} …",
        est.files,
        pipeline::humanize_bytes(est.bytes),
        output.display()
    );
```

In `cmd_export_keepers`, insert before the `build_keepers_tree` call (after `let out = ...`):

```rust
    let est = pipeline::estimate_keepers_copy(&catalog, &out)?;
    println!(
        "Copying {} files ({}) → {} …",
        est.files,
        pipeline::humanize_bytes(est.bytes),
        out.display()
    );
```

Keep the post-build report prints from Task 2 (they already start with "Copied"/"Keepers tree" — adjust the export print's first word to "Copied" so the test's `contains("Copied")` matches; the review print already has "Copied  :").

Make `cmd_export_keepers`'s report print start with `Copied`:

```rust
    let report = pipeline::build_keepers_tree(&catalog, &out, regenerate)?;
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
```

- [ ] **Step 4: Run tests**

Run: `. ~/.cargo/env && cargo test -p photopipe --test cli`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/cli/src/main.rs crates/cli/tests/cli.rs
git commit -m "feat(cli): pre-flight copy-size estimate + report for review-tree/export-keepers"
```

---

## Task 5: Web export estimate endpoint

**Files:**
- Modify: `crates/cli/src/serve/handlers.rs`
- Modify: `crates/cli/src/serve/mod.rs`
- Test: `crates/cli/tests/serve.rs`

**Interfaces:**
- Consumes: `pipeline::{estimate_keepers_copy, CopyEstimate}`, `config::expand_tilde`.
- Produces: `GET /api/export/estimate` → `Json<CopyEstimate>` (`{ files, bytes }`); `POST /api/export` still returns `Json<KeepersReport>` (now with `files_copied`/`bytes_copied`/…).

- [ ] **Step 1: Write the failing test**

Append to `crates/cli/tests/serve.rs`:

```rust
#[tokio::test]
async fn export_estimate_reports_files_and_bytes() {
    use std::sync::Arc;
    use image::{ImageBuffer, Rgb};
    use pipeline::catalog::Verdict;
    use pipeline::ingest::{FileFormat, IngestedFile};

    let dir = tempfile::TempDir::new().unwrap();
    let lib = dir.path().join("lib");
    std::fs::create_dir_all(&lib).unwrap();
    let p = lib.join("a.jpg");
    let img: ImageBuffer<Rgb<u8>, Vec<u8>> = ImageBuffer::from_fn(16, 16, |_, _| Rgb([1, 2, 3]));
    img.save(&p).unwrap();

    let catalog = pipeline::catalog::Catalog::open(&dir.path().join("c.duckdb")).unwrap();
    let file = IngestedFile { path: p.clone(), content_hash: 1, size: 1, mtime_ns: 1,
        format: FileFormat::Jpg, has_sidecar_jpg: false };
    let id = catalog.flush_batch(&[(file, None)]).unwrap()[0];
    catalog.set_decision(id, Verdict::Keep, None).unwrap();

    let cache = pipeline::cache::Cache::open(dir.path().join("cache")).unwrap();
    let state = photopipe::serve::AppState {
        catalog: Arc::new(catalog), cache: Arc::new(cache),
        cfg: Arc::new(pipeline::config::Config::default()),
    };
    let out = dir.path().join("_keepers");
    let uri = format!("/api/export/estimate?output={}", out.to_str().unwrap());
    let (s, v) = get_json(photopipe::serve::router(state), &uri).await;
    assert_eq!(s, axum::http::StatusCode::OK);
    assert_eq!(v["files"], 1);
    assert!(v["bytes"].as_u64().unwrap() > 0, "expected nonzero bytes: {v}");
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `. ~/.cargo/env && cargo test -p photopipe --test serve export_estimate_reports_files_and_bytes`
Expected: FAIL — route missing (404 → JSON parse → assertion fail).

- [ ] **Step 3: Add the handler**

In `crates/cli/src/serve/handlers.rs`, add (near `post_export`):

```rust
#[derive(Debug, Deserialize)]
pub struct EstimateQuery {
    pub output: Option<String>,
}

/// Read-only estimate of the keepers copy (files + bytes that would be written).
pub async fn get_export_estimate(
    State(state): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<EstimateQuery>,
) -> Result<axum::Json<pipeline::CopyEstimate>, StatusCode> {
    let out: PathBuf = q
        .output
        .map(|s| expand_tilde(&PathBuf::from(s)))
        .unwrap_or_else(|| PathBuf::from("_keepers"));
    let catalog = state.catalog.clone();
    tokio::task::spawn_blocking(move || pipeline::estimate_keepers_copy(&catalog, &out))
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .map(axum::Json)
        .map_err(|e| {
            tracing::warn!(error = %e, "export estimate failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })
}
```

(`Query`, `Json`, `expand_tilde`, `PathBuf`, `pipeline::CopyEstimate` — ensure imports exist; `Query`/`Json` may need `use axum::extract::Query;` / they are already imported for other handlers. Use fully-qualified paths as written to be safe.)

- [ ] **Step 4: Register the route**

In `crates/cli/src/serve/mod.rs`, add to `router` (near the other `/api/export` route):

```rust
        .route("/api/export/estimate", get(handlers::get_export_estimate))
```

- [ ] **Step 5: Run tests**

Run: `. ~/.cargo/env && cargo test -p photopipe --test serve`
Expected: PASS (new estimate test + all existing serve tests).

- [ ] **Step 6: Commit**

```bash
git add crates/cli/src/serve/handlers.rs crates/cli/src/serve/mod.rs crates/cli/tests/serve.rs
git commit -m "feat(cli): GET /api/export/estimate (read-only copy size)"
```

---

## Task 6: Frontend — confirm-with-estimate before export

**Files:**
- Modify: `crates/cli/assets/app.js`

**Interfaces:**
- Consumes: `GET /api/export/estimate` → `{ files, bytes }`; `POST /api/export` → `{ files_copied, bytes_copied, errors, ... }`.
- Produces: the Export button estimates, confirms, exports, and reports the copied amount. **No automated test** — verified by the manual smoke test in Step 3.

- [ ] **Step 1: Update the export handler + add a byte formatter**

In `crates/cli/assets/app.js`, replace the existing export-button click handler with:

```js
function humanBytes(n) {
  const u = ['B', 'KB', 'MB', 'GB', 'TB'];
  let v = n, i = 0;
  while (v >= 1024 && i < u.length - 1) { v /= 1024; i++; }
  return i ? `${v.toFixed(1)} ${u[i]}` : `${n} B`;
}

document.getElementById('export-btn').addEventListener('click', async () => {
  try {
    const est = await api('GET', '/api/export/estimate');
    const ok = confirm(
      `This will copy ${est.files} photo(s) (${humanBytes(est.bytes)}) to the "_keepers" ` +
      `folder (relative to where 'photopipe serve' was started). Continue?`
    );
    if (!ok) return;
    const r = await api('POST', '/api/export', { regenerate: false });
    alert(`Copied ${r.files_copied} photo(s) (${humanBytes(r.bytes_copied)}), ${r.errors} error(s).`);
  } catch (err) {
    alert(`Export failed: ${err.message}`);
  }
});
```

- [ ] **Step 2: Build**

Run: `. ~/.cargo/env && cargo build -p photopipe && cargo test -p photopipe --test serve`
Expected: compiles (rust-embed re-embeds `app.js`); serve tests still pass (static-asset serving unchanged).

- [ ] **Step 3: Manual smoke test**

With a populated catalog: `./target/debug/photopipe serve --port 8787`, open `http://127.0.0.1:8787/`, click **Export keepers**. Confirm a dialog appears stating the file count and size (e.g. "copy 3 photo(s) (40.2 MB)"), and on confirm a `_keepers/` tree of **real files** appears, with a success alert reporting the copied amount. Cancel should copy nothing.

- [ ] **Step 4: Commit**

```bash
git add crates/cli/assets/app.js
git commit -m "feat(cli): export button confirms copy size before copying"
```

---

## Task 7: Docs, example config, and Windows compile check

**Files:**
- Modify: `photopipe.example.toml`
- Modify: `README.md`

**Interfaces:** none — docs + verification.

- [ ] **Step 1: Update the example config**

In `photopipe.example.toml`, in the `[output]` section, remove the `link_type` line and replace the comment block with:

```toml
[output]
# review-tree and export-keepers always COPY files (cross-platform; real files
# any tool can read). Originals are never moved or modified.
review_tree    = "<library>/_review"
keeper_strategy = "iqa"   # "iqa" | "sharpness" | "iqa_then_sharpness"
```

(If `[output]` had other keys, keep them; only `link_type` is removed.)

- [ ] **Step 2: Update the README**

In `README.md`:

- In the platform note near the top, change "On Windows, run it inside WSL2" to state that photopipe now builds and runs **natively on Windows** (and still works under WSL2).
- Replace the "Windows (PC with an NVIDIA GPU) — via WSL2" section with a native-Windows section: prerequisites are **Visual Studio Build Tools (C++ workload)**, **Rust (MSVC toolchain via rustup)**, the **NVIDIA driver + CUDA runtime/cuDNN** for the GPU provider (falls back to CPU; `photopipe doctor` shows which), and **Python only for the one-time model export**. Build with `cargo build --release`. Note default data lives under `%APPDATA%\photopipe` / cache under `%LOCALAPPDATA%\photopipe`.
- In "Exporting keepers" and "Symlink review tree": change wording from symlinks to **copies** — the trees contain real copied files; originals untouched. Mention the CLI prints how much data will be copied and the web UI confirms first. Rename the "Symlink review tree" heading to "Review tree (file-manager browsing)".
- In the config section, remove the `link_type` bullet; note trees always copy.
- In "Guarantees", keep non-destructive/idempotent; add that copied trees carry a `.photopipe-tree` marker and the tool refuses to delete a directory lacking it.

(Keep the existing macOS and Linux sections; they are unchanged except wording about copies.)

- [ ] **Step 3: Windows cross-compile check**

Run:
```bash
. ~/.cargo/env
rustup target add x86_64-pc-windows-gnu
cargo check -p pipeline --target x86_64-pc-windows-gnu
```
Expected: compiles for the Windows target (proves no `std::os::unix` / portability regressions in the pipeline crate). If `cargo check` for the full workspace fails only at the *link* stage for the binary (no MSVC/mingw linker present), that is acceptable — the goal is type/cfg validation of the portable code; note it in the commit message. If it fails to **compile** (not link), fix the portability issue before committing.

- [ ] **Step 4: Final verification sweep**

Run each and confirm green:
```bash
. ~/.cargo/env
cargo fmt
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all
./target/debug/photopipe doctor; echo "doctor exit: $?"
```
Expected: fmt clean; clippy 0 warnings; all tests pass; `doctor` exits 0.

- [ ] **Step 5: Commit**

```bash
git add README.md photopipe.example.toml
git commit -m "docs: native Windows guide + copy-always trees; example config"
```

---

## Self-Review (completed during planning)

- **Spec coverage:** copy-always (Task 2) ✓; remove `LinkType`/`link_type` (Task 3, with no-breakage test) ✓; portable paths via `dirs` (Task 1) ✓; marker-file guard + prune of real files (Task 2) ✓; data-volume awareness on CLI (Task 4) and web UI confirm-with-estimate (Tasks 5–6) ✓; reports carry `files_copied`/`files_skipped`/`bytes_copied` (Task 2) ✓; `humanize_bytes` (Task 2) ✓; Windows compile check + docs (Task 7) ✓; non-destructive/idempotent preserved (copy idempotency in Task 2) ✓; deferred native-Windows runtime validation noted (Task 7 Step 3 + Global Constraints) ✓.
- **Type consistency:** `CopyEstimate { files, bytes }`, `KeepersReport`/`ReviewTreeReport { files_copied, files_skipped, files_removed, bytes_copied, [groups], errors }`, `build_review_tree(catalog, output_root, include, regenerate)`, `build_keepers_tree(catalog, output_root, regenerate)`, `estimate_review_copy(catalog, output_root, include)`, `estimate_keepers_copy(catalog, output_root)`, `humanize_bytes(u64)->String` — used consistently across Tasks 2/4/5/6 and the re-export list.
- **Placeholder scan:** none; all code steps carry full code or precise edits.
- **Sequencing note:** Task 2 removes every `LinkType` *usage* (output, tests, call sites) by dropping the config argument, so Task 3 only deletes the now-unreferenced enum + field. Between Task 2 and Task 3 the workspace compiles and tests pass (the enum/field simply linger unused but serde-referenced, so no dead-code warning).
- **Known follow-up (not blocking):** native-Windows *runtime* (MSVC build of bundled DuckDB + libwebp, `%APPDATA%` behavior, CUDA DLL discovery, the UI in a Windows browser) is validated only by `cargo check` here; functional testing needs a Windows machine.
