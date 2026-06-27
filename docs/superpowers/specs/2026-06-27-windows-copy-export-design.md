# Native Windows support via copy-always export — Design Spec

**Date:** 2026-06-27
**Status:** Approved (brainstorm) — ready for implementation planning
**Scope:** Make photopipe build and run natively on Windows (no WSL) by replacing the symlink/hardlink review and keepers trees with a single cross-platform **copy** strategy, fixing platform-aware path resolution, and surfacing how much data a copy will write.

## 1. Motivation

photopipe currently only builds on Unix because the review/keepers trees are
materialized with `std::os::unix::fs::symlink`. Native Windows also fails at
runtime because default catalog/cache/config paths are derived from `$HOME` and
`XDG_*`, which Windows does not set.

Rather than port symlinks (which on Windows need Developer Mode/admin and break
across volumes as hardlinks), we **always copy**. Copy behaves identically on
every OS, produces real files any tool (Lightroom, Explorer, Finder) can consume,
and removes the only Unix-specific code. The web review UI remains the primary,
zero-copy review path; the on-disk trees exist to hand a curated set to other
tools.

Originals are never moved or modified — copy reads originals and writes new files
elsewhere. The non-destructive contract is unchanged.

## 2. Decisions locked during brainstorming

| Decision | Choice |
|---|---|
| Link strategy | **Copy always.** Remove `symlink` and `hardlink` modes entirely. |
| `LinkType` enum + `[output] link_type` key | Removed (old configs still parse — serde ignores unknown fields). |
| Default paths | Platform-aware via the `dirs` crate. |
| Safety guard | Root **marker file** (`.photopipe-tree`), replacing the symlink-based guard. |
| Data-volume awareness | Pre-flight message (CLI) + confirm-with-estimate (web UI), always. |
| Windows validation | Compile-checked here via cross-target `cargo check`; runtime validation deferred to a real Windows machine. |

## 3. Behavior change: copy-always

`review-tree` and `export-keepers` copy each planned file to
`<output>/<rel_dir>/<name>` (review tree: `rejected/<reason>/<YYYY-MM>/…`,
`uncertain/…`, `duplicates/group_NNNNN/…`; keepers: `<YYYY-MM>/…`). Basename
collisions within a directory are de-duplicated by the existing `dedupe_name`
helper (suffix before the extension).

- **Copy semantics (`copy_file`)**: copy the source to the destination only when
  the destination is missing or differs (source `size` != dest `size`, or source
  `mtime` newer than dest `mtime`). Otherwise skip. This keeps re-runs cheap.
- Removes `create_link`, the `LinkType` match, and `std::os::unix::fs::symlink`.
  This is the change that makes the crate compile on Windows.
- **Tradeoff (accepted):** copy duplicates image data — e.g. 3000 × 25 MB RAWs ≈
  75 GB. Mitigated by the data-volume messaging (§6) and by the web UI being the
  zero-copy review path.

## 4. Cross-platform path resolution

Replace the hand-rolled `xdg_config_home` / `xdg_data_home` / `xdg_cache_home`
helpers in `config.rs` with the `dirs` crate (MIT/Apache-2.0):

- config file: `dirs::config_dir()/photopipe/photopipe.toml`
- catalog db: `dirs::data_dir()/photopipe/catalog.duckdb`
- cache: `dirs::cache_dir()/photopipe`

Resulting defaults per OS:

| | Linux | macOS | Windows |
|---|---|---|---|
| config | `~/.config/photopipe` | `~/Library/Application Support/photopipe` | `%APPDATA%\photopipe` |
| data | `~/.local/share/photopipe` | `~/Library/Application Support/photopipe` | `%APPDATA%\photopipe` |
| cache | `~/.cache/photopipe` | `~/Library/Caches/photopipe` | `%LOCALAPPDATA%\photopipe` |

`expand_tilde` is retained for explicit `~/…` values in user configs; it uses
`dirs::home_dir()` so it also resolves on Windows (to the user profile).

New dependency: `dirs` (added to the workspace and the `pipeline` crate).

## 5. Safety guard: root marker file

The current guard (`check_no_foreign_files` refuses to delete a tree containing
any non-symlink regular file) assumed managed entries are symlinks. Copies are
regular files, indistinguishable from a user's real photos, so the guard is
reworked around an explicit marker:

- On every build, write a marker file `<output>/.photopipe-tree` at the tree root
  (alongside the existing `README.txt`).
- **`--regenerate`**: if `<output>` exists and contains the marker → safe to
  remove the whole subtree and rebuild. If `<output>` exists, is non-empty, and
  lacks the marker → **refuse with an error** (it may be a real directory).
- **Incremental (non-regenerate)**: create/refresh into the tree; prune files and
  now-empty directories that are not in the current expected set — but only within
  a directory that has the marker. If `<output>` exists, is non-empty, and lacks
  the marker → refuse.
- A fresh/empty `<output>` is always fine: create it and write the marker.

Consequence: review/keepers trees created by the *previous* (symlink) version lack
the marker, so the new version refuses to touch them. The error message instructs
the user to delete the old tree manually (these trees are disposable). Documented
in the README.

## 6. Data-volume awareness

A pre-flight pass sums the byte size of the files that *would actually be copied*
(planned set, excluding destinations already up to date).

- **CLI** (`export-keepers`, `review-tree`): before copying, print one line —
  `Copying <N> files (<H>) → <output> …` where `<H>` is human-readable
  (e.g. `9.7 GB`). After copying, print the final report —
  `Copied <N> files (<H>), <skipped> skipped, <errors> errors.` Because copying a
  large set takes time, the user can abort with Ctrl-C after seeing the estimate.
- **Web UI**: the **Export keepers** button first calls `GET /api/export/estimate`
  (read-only — computes the planned copy set and its byte total without writing),
  then shows `confirm("This will copy <N> photos (<H>) to \"<output>\". Continue?")`.
  Only on confirm does it `POST /api/export`; the result alert reports the actual
  amount copied (`Copied <N> photos (<H>), <errors> error(s).`).

Reports carry `files_copied`, `files_skipped`, `bytes_copied`, and `errors`. A
small `humanize_bytes` helper formats sizes (B/KB/MB/GB).

## 7. Architecture & affected components

- **`crates/pipeline/src/config.rs`** — remove `LinkType` + `OutputConfig.link_type`;
  replace XDG helpers with `dirs`-based resolution; keep `expand_tilde` (via
  `dirs::home_dir`).
- **`crates/pipeline/src/output/mod.rs`** — replace `create_link` with
  `copy_file` (copy + skip-if-current); rework the materialization core to copy,
  prune, and write the `.photopipe-tree` marker + `README.txt`; rework
  `remove_managed_tree` / the guard around the marker; add a planned-copy estimate
  helper (`estimate_copy_bytes`) returning `(files, bytes)`; extend
  `ReviewTreeReport` / `KeepersReport` with `files_skipped` and `bytes_copied`.
- **`crates/cli/src/main.rs`** — `cmd_review_tree` and `cmd_export_keepers` print
  the pre-flight estimate line and the final report; drop `LinkType` references.
- **`crates/cli/src/serve/handlers.rs`** — `post_export` returns the byte-aware
  report; add `get_export_estimate` (`GET /api/export/estimate`).
- **`crates/cli/src/serve/mod.rs`** — register the estimate route.
- **`crates/cli/assets/app.js`** — Export button: estimate → confirm → export →
  alert with the copied amount.
- **`photopipe.example.toml`** — remove `link_type`; note copy behavior and that
  `serve` reads `catalog.db_path` / `catalog.cache_dir`.
- **`Cargo.toml` (workspace) + `crates/pipeline/Cargo.toml`** — add `dirs`.
- **`README.md`** — add a native-Windows section (no WSL); document copy behavior,
  the data-volume message, and the marker-file note.

## 8. Error handling

- Per-file copy failures log `tracing::warn!(path, error)` and increment the error
  count; a single failure never aborts the run (matches existing behavior).
- `--regenerate`/prune refuse (hard error) on an unmarked non-empty `<output>`.
- Disk-full during copy surfaces as per-file errors in the report.
- `anyhow::Result` at CLI/HTTP boundaries; `tracing` not `println!` except
  user-facing CLI output (the estimate/report lines and existing command output).
- `GET /api/export/estimate` is read-only and writes nothing.

## 9. Testing

- **Pipeline (runnable on Linux — copy is cross-platform):**
  - copy creates files; re-run skips up-to-date files (idempotent); a changed
    source is re-copied.
  - prune removes files no longer expected within a marked tree.
  - `--regenerate` rebuilds a marked tree; refuses an unmarked non-empty dir.
  - `estimate_copy_bytes` returns the correct file count and byte total, excluding
    already-current destinations.
  - originals are byte-identical and untouched after a copy run.
- **CLI:** `export-keepers` prints the estimate + report and produces a real file
  tree (extend the existing `tests/cli.rs` case).
- **Serve:** `GET /api/export/estimate` returns the expected counts/bytes; `POST
  /api/export` returns `bytes_copied`.
- **Windows compile check:** `rustup target add x86_64-pc-windows-gnu` then
  `cargo check --target x86_64-pc-windows-gnu` (catches `cfg`/portability
  regressions without a Windows host; `check` does not link).
- `cargo fmt`, `cargo clippy --all-targets --all-features -- -D warnings`, and
  `cargo test --all` green before done.

## 10. Out of scope / deferred

- **Runtime validation on native Windows** (MSVC build of bundled DuckDB + libwebp,
  `%APPDATA%` path behavior, CUDA/cuDNN DLL discovery, the review UI in a Windows
  browser). The build will be compile-validated here; functional Windows testing
  requires a Windows machine and is a follow-up.
- Symlink/hardlink export modes (removed; not reintroduced as options).
- Migrating pre-existing symlink trees (user deletes them manually).
- Pre-copy free-disk-space check / abort threshold (the estimate message is the
  v1 mechanism; a hard guard can come later).
