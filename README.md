# photopipe

Local-first command-line tool that ingests a directory of RAW (and JPG) photos and
produces (a) a **DuckDB catalog** of per-file metadata, defect flags, and
duplicate-group assignments, and (b) a **symlink "review tree"** you browse with
your OS file manager. Strictly non-destructive — your originals are never moved,
modified, or deleted.

## Install

Requires a stable Rust toolchain (edition 2021).

```bash
git clone <repo-url> photopipe
cd photopipe
cargo build --release
# binary at ./target/release/photopipe
```

ML inference uses ONNX Runtime. On Linux with an NVIDIA GPU the CUDA execution
provider is used automatically; otherwise it falls back to CPU. On macOS it runs
on CPU (CoreML is disabled pending an ONNX Runtime fix). Place the ONNX model
files under `./models/` (see `models/README.md`).

## Configuration

Copy the example config and edit it:

```bash
mkdir -p ~/.config/photopipe
cp photopipe.example.toml ~/.config/photopipe/photopipe.toml
```

Every key has a built-in default, so the file is optional. Pass a different path
with `--config <path>` on any command. See `photopipe.example.toml` for all keys
and their defaults.

## Common workflows

```bash
# 1. Ingest + analyse one or more library roots (catalog + previews + defects + ML).
photopipe scan ~/Photos/2024 ~/Photos/2025

# Skip ML inference (faster; classical defect checks only):
photopipe scan ~/Photos/2024 --no-models

# 2. Build per-lens sharpness baselines once you've scanned enough frames per lens.
photopipe calibrate

# 3. Group near-duplicate frames using the current embeddings.
photopipe dedupe

# 4. Generate the symlink review tree to browse in your file manager.
photopipe review-tree ~/Photos/_review --include rejected,duplicates,uncertain
```

## Inspect the catalog

```bash
# Summary: file counts, flag counts, duplicate groups, per-camera/per-lens
# breakdown, and catalog/cache disk usage.
photopipe stats

# Everything the catalog knows about one file, as JSON.
photopipe info ~/Photos/2024/DSC01234.arw

# Health check: DB schema, model presence/loadability, ORT provider,
# cache writability, free disk space. Exits non-zero if something critical
# is wrong.
photopipe doctor
```

## Command reference

| Command | Purpose |
|---------|---------|
| `scan <PATH>...` | Ingest + analyse library roots. `--no-models`, `--reprocess`. |
| `calibrate` | Rebuild per-lens sharpness baselines from the catalog. |
| `dedupe` | Rebuild duplicate groups from current embeddings. |
| `review-tree <OUTPUT>` | Generate/update the symlink review tree. `--include`, `--regenerate`. |
| `serve` | Launch the local review web UI. `--port` (default 8787). |
| `export-keepers <OUTPUT>` | Materialize a keepers export tree from recorded decisions. `--regenerate`. |
| `info <FILE>` | Print all catalog data for one file as JSON. |
| `stats` | Print catalog summary statistics. |
| `doctor` | Diagnose config, models, DB, and system health. |

All commands accept `--config <path>`, `--log-level <level>`, and `--log-format <pretty|json>`.

## Review UI

After scanning and deduping, launch the local web UI to triage your photos:

```bash
photopipe serve --port 8787
# then open http://127.0.0.1:8787/ in your browser
```

The grid shows all photos **flagged-first** (defects and duplicates before clean
shots). Use the flag filter (blur, back_focus, overexposed, underexposed,
low_iqa) and the decided filter (all / undecided / decided) to focus your
review.

**Keyboard shortcuts**

| Key | Action |
|-----|--------|
| `j` / `→` | Move to next photo |
| `k` / `←` | Move to previous photo |
| `Space` / `Enter` | Mark keep |
| `x` | Mark reject |
| `u` | Mark undecide |
| `K` | Pick as group keeper |
| `f` | Open detail view |
| `Esc` / `f` | Close detail view |

**Exporting keepers**

Once you have reviewed your photos, export keepers via the **Export keepers**
button in the UI or directly from the command line:

```bash
photopipe export-keepers ~/Photos/_keepers
```

This builds a links-only `<output>/YYYY-MM/` tree — originals are never moved
or modified. Pass `--regenerate` to delete and rebuild the tree from scratch.

## Guarantees

- **Non-destructive:** originals are only read; outputs are a separate DuckDB file and a tree of symlinks.
- **Idempotent:** re-running `scan` on unchanged inputs does no new work.
