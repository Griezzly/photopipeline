# photopipe

Local-first command-line tool that ingests a directory of RAW (and JPG) photos and
produces:

- a **DuckDB catalog** of per-file metadata, defect flags (blur, back-focus, over/
  under-exposure, low quality), and duplicate-group assignments;
- a **local web review UI** (`photopipe serve`) to triage and cull, plus a
  **review tree** of copied files you can browse in your file manager; and
- a **keepers export tree** of the photos you chose to keep.

Strictly **non-destructive** — your originals are only ever read. Outputs are a
separate DuckDB file and trees of real copied files; nothing is moved, modified,
or deleted.

> **Platform note:** photopipe builds and runs natively on **Linux**, **macOS**,
> and **Windows** (see the Windows guide below). WSL2 also works on Windows.

---

## How it works

A library goes through a few stages, each its own command:

```
scan ──> calibrate ──> dedupe ──> serve / review-tree ──> export-keepers
(ingest,   (per-lens     (group      (triage & record         (materialize the
 defects,   sharpness     near-        keep/reject              kept set as links)
 ML)        baselines)    duplicates)  decisions)
```

- **`scan`** — walk the directories, hash each file, extract EXIF, render a cached
  WebP preview, run the classical defect checks (blur / exposure) and the ML
  models (DINOv2 embeddings for dedupe, CLIP-IQA quality score, RT-DETR subject
  detection for blur ROI). Idempotent: re-scanning unchanged files does no work.
- **`calibrate`** — build per-lens sharpness baselines so blur flags adapt to each
  lens (run after you've scanned a few dozen frames per lens).
- **`dedupe`** — group near-identical frames from the embeddings and elect a
  suggested keeper per group.
- **`serve`** — the review UI: a grid of your photos (flagged/duplicates first),
  keyboard-driven keep/reject, written through to the catalog.
- **`export-keepers`** — build a `keepers/YYYY-MM/` tree of copies of everything
  you kept, ready to hand to Lightroom / Capture One / a backup.

---

## Quick start

```bash
# build
cargo build --release            # binary at ./target/release/photopipe
alias photopipe=./target/release/photopipe

# one-time: export the ML models (see "Model setup" below)
./models/download.sh

# verify your environment (toolchain, models, GPU provider, disk)
photopipe doctor

# ingest + analyse a library
photopipe scan ~/Photos/2024

# (optional) per-lens blur calibration once enough frames are scanned
photopipe calibrate ~/Photos/2024

# group near-duplicates
photopipe dedupe ~/Photos/2024

# review in the browser
photopipe serve ~/Photos/2024 --port 8787      # open http://127.0.0.1:8787/

# export what you kept
photopipe export-keepers ~/Photos/2024 ~/Photos/_keepers
```

You can run `scan` without the ML models for a quick pass (classical defect
checks only): `photopipe scan ~/Photos/2024 --no-models`.

---

## Platform setup

photopipe needs a stable **Rust** toolchain (edition 2021). **Python is only used
once**, to export the ONNX model files — the shipped binary has no Python
dependency at runtime.

### Windows (PC with an NVIDIA GPU) — native build

photopipe builds and runs natively on Windows. WSL2 also works (see the
note at the end of this section).

**Prerequisites:**

1. **Visual Studio Build Tools** — install from
   <https://visualstudio.microsoft.com/downloads/> (select the
   **Desktop development with C++** workload). This provides the MSVC linker
   and Windows SDK that Rust's MSVC toolchain requires.
2. **Rust (MSVC toolchain)** — install from <https://rustup.rs/>. The installer
   auto-selects the `x86_64-pc-windows-msvc` target when VS Build Tools are
   present.
3. **NVIDIA driver + CUDA runtime + cuDNN** — install the latest
   Game-Ready or Studio driver from <https://www.nvidia.com/drivers>. Then
   install the CUDA Toolkit and the matching cuDNN from the NVIDIA developer
   site. ONNX Runtime's CUDA execution provider discovers them via `PATH`/
   `CUDA_PATH`. If they are absent, photopipe still runs — it falls back to the
   CPU provider. `photopipe doctor` shows which provider was selected.
4. **ONNX model files** — copy the three `.onnx` files into `models\` (see
   "Model setup" below). The easiest path on Windows is to copy pre-exported
   files from another machine (e.g. your Linux/WSL install) into `models\` —
   then you need no Python on Windows at all. Python is only required if you
   want to export the models from scratch.

**Build and run** (in a Developer Command Prompt or regular PowerShell after
sourcing the VS environment):

```powershell
git clone <repo-url> photopipe
cd photopipe
cargo build --release
.\target\release\photopipe.exe doctor
```

**Default data locations on Windows:**

Each analyzed folder gets its own per-folder library stored in OS app-data
(keyed by the folder path). `photopipe libraries` lists all known libraries.

| Purpose | Default root |
|---------|-------------|
| Catalog (DuckDB) | `%APPDATA%\photopipe\` |
| Preview cache | `%LOCALAPPDATA%\photopipe\` |

Catalog paths are not configurable — each folder library is managed
automatically. The config file lives at
`%APPDATA%\photopipe\photopipe.toml` by default. Pass `--config <path>`
to any command to override.

**Reviewing on Windows:** run `photopipe serve <folder>` and open
`http://127.0.0.1:8787/` in your browser. The review and keepers trees
contain real **copied** files, so they open correctly in Windows Explorer and
any photo tool.

> **WSL2 also works** if you prefer a Linux environment on Windows. The
> NVIDIA driver for Windows already exposes the GPU to WSL2 (CUDA-on-WSL) — no
> separate Linux driver is needed inside WSL. Build and use photopipe inside
> Ubuntu/WSL exactly as on Linux.

### macOS (Apple Silicon — M1 or newer)

macOS runs on the **CPU** execution provider. CoreML is disabled in the pinned
ONNX Runtime (it crashes on the external-data model format these models use —
see `models/README.md`). On M-series chips, DINOv2 + CLIP-IQA run roughly
200–400 ms/image, so the ML phase of a 10k-photo library takes ~30–60 minutes.
Everything else (ingest, defects, dedupe, UI, export) is fast.

1. **Install the prerequisites** (with [Homebrew](https://brew.sh)):
   ```bash
   xcode-select --install            # C toolchain
   brew install python               # Python is only needed for model export
   # Rust (official installer):
   curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
   source "$HOME/.cargo/env"
   ```
2. **Build and run:**
   ```bash
   git clone <repo-url> photopipe && cd photopipe
   cargo build --release
   ```
3. Leave `device = "auto"` (or set `"cpu"`) in the config — there is no GPU path
   on macOS today.

### Linux (with an NVIDIA GPU)

Install a stable Rust toolchain, `build-essential`/`pkg-config`, and the NVIDIA
driver plus the CUDA runtime + cuDNN that ONNX Runtime expects. `cargo build
--release`, then `photopipe doctor` to confirm the CUDA provider was selected
(it falls back to CPU if the CUDA libraries aren't found).

---

## Model setup (one-time)

The ONNX model files are **not** committed. The fastest way to get them onto a
new machine is to **copy the three `.onnx` files** (`dinov2_base.onnx`,
`clip_iqa.onnx`, `rt_detr_l.onnx`) from an existing install into `models/` —
they are plain files with no per-machine state, so this needs no toolchain.

To produce them from scratch, export them once with the Python scripts in
`tools/` (Python is needed *only* here, never at runtime):

```bash
./models/download.sh            # runs all three exporters into ./models/
# — or manually —
cd tools
python3 -m venv .venv && source .venv/bin/activate
pip install -r requirements.txt
python export_dinov2.py         # → ../models/dinov2_base.onnx (~330 MB)
python export_clip_iqa.py       # → ../models/clip_iqa.onnx   (~340 MB)
python export_rt_detr.py        # → ../models/rt_detr_l.onnx  (~175 MB)
```

If the model files are absent, `scan --no-models` still works, and the detector
slot degrades gracefully (blur analysis falls back to a center-crop ROI). See
`models/README.md` for details and the execution-provider order.

---

## Configuration

Every setting has a built-in default, so config is optional. To customize:

```bash
mkdir -p ~/.config/photopipe
cp photopipe.example.toml ~/.config/photopipe/photopipe.toml
```

Pass a different file with `--config <path>` on any command. See
`photopipe.example.toml` for all keys. A few worth knowing:

- `[models] device` — `"auto"` (default) probes TensorRT → CUDA → CoreML → CPU and
  falls back; force one with `"cuda"`, `"cpu"`, etc. (`"coreml"` is overridden to
  CPU on macOS).
- `[ingest] extensions` — file types to ingest (RAW: `arw cr3 cr2 nef raf rw2 dng`,
  plus `jpg jpeg`).
- `[ingest] sidecar_jpg` — `"prefer"` (use the JPG next to a RAW for previews),
  `"ignore"`, or `"require"`.
- `[output] review_tree` / `keeper_strategy` — destination pattern for the review
  tree and which selection strategy to use for keeper elections. The trees always
  contain real **copied** files (works on all platforms without symlink permissions).

---

## Reviewing your photos

### Web UI (recommended)

```bash
photopipe serve ~/Photos/2024 --port 8787      # then open http://127.0.0.1:8787/
```

The grid shows every photo **flagged-first** (defects and duplicates before clean
shots). Filter by flag (`blur`, `back_focus`, `overexposed`, `underexposed`,
`low_iqa`) and by decision state (all / undecided / decided). Decisions are
written through to the catalog immediately.

| Key | Action |
|-----|--------|
| `j` / `→` | Next photo |
| `k` / `←` | Previous photo |
| `Space` / `Enter` | Mark **keep** (green) |
| `x` | Mark **reject** (red) |
| `u` | Mark **undecide** |
| `K` | Pick as the **keeper** of its duplicate group |
| `f` | Open / close the detail view |
| `Esc` | Close the detail view |

The footer shows live keep / reject / undecided counts. The server binds
`127.0.0.1` only — it is never exposed on the network.

### Review tree (file-manager browsing)

```bash
photopipe review-tree ~/Photos/2024 ~/Photos/_review --include rejected,duplicates,uncertain
```

Builds `rejected/<reason>/`, `uncertain/`, and `duplicates/group_NNNNN/` folders of
**copied** files for browsing in any OS file manager (Linux, macOS, or Windows).
The CLI prints how much data will be copied before starting. `--regenerate`
deletes and rebuilds the tree from scratch.

---

## Exporting keepers

After reviewing, export the kept set — via the **Export keepers** button in the UI
or the CLI:

```bash
photopipe export-keepers ~/Photos/2024 ~/Photos/_keepers
```

This builds a `<output>/YYYY-MM/` tree of **copies** of everything with a **keep**
decision (for duplicate groups, only your chosen keeper). Originals are never
moved or modified. The CLI prints the estimated copy size before starting; the
web UI shows the estimate and asks you to confirm before copying.
`--regenerate` deletes and rebuilds the tree.

---

## Inspecting the catalog

```bash
# Summary: file/flag counts, duplicate groups, per-camera & per-lens breakdown,
# and catalog/cache disk usage.
photopipe stats ~/Photos/2024

# Everything the catalog knows about one file, as JSON.
photopipe info ~/Photos/2024/DSC01234.arw

# Health check: DB schema, model presence/loadability, selected ORT provider,
# cache writability, free disk. Exits non-zero on a critical problem.
photopipe doctor
```

---

## Libraries

Each analyzed folder gets its own per-folder library stored in OS app-data
(catalog in the data dir, previews in the cache dir), keyed by the folder
path. Nothing is written into the photo folder itself.

```bash
photopipe libraries           # list all known libraries (folder, photo count, last analyzed)
```

Pre-existing single catalogs from older builds (e.g. `…/photopipe/catalog.duckdb`)
are no longer used and can be deleted.

---

## Command reference

| Command | Purpose |
|---------|---------|
| `scan <PATH>...` | Ingest + analyse library roots. `--no-models`, `--reprocess`. |
| `calibrate <folder>` | Rebuild per-lens sharpness baselines for a folder's library. |
| `dedupe <folder>` | Rebuild duplicate groups from current embeddings. |
| `serve <folder>` | Launch the local review web UI. `--port` (default 8787). |
| `review-tree <folder> <output>` | Generate/update the review tree (copies). `--include`, `--regenerate`. |
| `export-keepers <folder> <output>` | Materialize the keepers export tree (copies). `--regenerate`. |
| `info <FILE>` | Print all catalog data for one file as JSON (walks up to find the library). |
| `stats <folder>` | Print catalog summary statistics for a folder's library. |
| `libraries` | List all analyzed libraries (folder, photo count, last analyzed). |
| `doctor` | Diagnose config, models, DB, and system health. |

All commands accept `--config <path>`, `--log-level <level>`, and
`--log-format <pretty|json>`.

---

## Guarantees

- **Non-destructive:** originals are only read; outputs are a separate DuckDB file
  and trees of real copied files — nothing is moved, modified, or deleted.
- **Idempotent:** re-running `scan` on unchanged inputs does no new work;
  re-running `export-keepers` reconciles the tree without touching originals.
- **Decisions persist:** your keep/reject choices live in the catalog and survive
  re-scans and re-dedupes.
- **Safe tree management:** every review and keepers tree is marked with a
  `.photopipe-tree` sentinel file when created. The tool refuses to remove a
  directory that lacks this marker, preventing accidental deletion of unrelated
  folders.
