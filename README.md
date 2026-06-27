# photopipe

Local-first command-line tool that ingests a directory of RAW (and JPG) photos and
produces:

- a **DuckDB catalog** of per-file metadata, defect flags (blur, back-focus, over/
  under-exposure, low quality), and duplicate-group assignments;
- a **local web review UI** (`photopipe serve`) to triage and cull, plus a
  symlink **review tree** you can browse in your file manager; and
- a **keepers export tree** of the photos you chose to keep.

Strictly **non-destructive** — your originals are only ever read. Outputs are a
separate DuckDB file and trees of symlinks; nothing is moved, modified, or
deleted.

> **Platform note:** photopipe builds and runs on **Linux and macOS**. On
> **Windows**, run it inside **WSL2** (see the Windows guide below) — this is
> also how the NVIDIA GPU path works on a Windows machine.

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
- **`export-keepers`** — build a `keepers/YYYY-MM/` tree of links to everything you
  kept, ready to hand to Lightroom / Capture One / a backup.

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
photopipe calibrate

# group near-duplicates
photopipe dedupe

# review in the browser
photopipe serve --port 8787      # open http://127.0.0.1:8787/

# export what you kept
photopipe export-keepers ~/Photos/_keepers
```

You can run `scan` without the ML models for a quick pass (classical defect
checks only): `photopipe scan ~/Photos/2024 --no-models`.

---

## Platform setup

photopipe needs a stable **Rust** toolchain (edition 2021). **Python is only used
once**, to export the ONNX model files — the shipped binary has no Python
dependency at runtime.

### Windows (PC with an NVIDIA GPU) — via WSL2

On Windows the GPU path and the symlink trees both work through **WSL2**. The
NVIDIA Windows driver exposes the GPU to WSL2 (CUDA-on-WSL) — you do **not**
install a Linux NVIDIA driver inside WSL.

1. **Install WSL2 + Ubuntu** (PowerShell, as admin), then reboot:
   ```powershell
   wsl --install -d Ubuntu
   ```
2. **NVIDIA driver:** install the latest **NVIDIA driver for Windows** (the
   standard Game-Ready/Studio driver includes WSL CUDA support). Nothing
   GPU-driver-related is installed inside Ubuntu.
3. **Inside the Ubuntu (WSL) shell**, install the build prerequisites:
   ```bash
   sudo apt update
   sudo apt install -y build-essential pkg-config git python3 python3-venv python3-pip
   # Rust
   curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
   source ~/.cargo/env
   # CUDA runtime + cuDNN that ONNX Runtime needs for the GPU provider:
   sudo apt install -y nvidia-cuda-toolkit       # or NVIDIA's CUDA-on-WSL packages
   ```
   ONNX Runtime's CUDA execution provider needs the CUDA runtime and cuDNN on
   the library path. If they're missing, photopipe still runs — it just falls
   back to CPU. `photopipe doctor` tells you which provider was selected.
4. **Build and run inside WSL:**
   ```bash
   git clone <repo-url> photopipe && cd photopipe
   cargo build --release
   ```
5. **Where to keep photos.** Reading from the Windows drive (`/mnt/c/...`) works
   but is slow; copying a library onto the WSL ext4 filesystem (e.g.
   `~/Photos`) is much faster for scanning. Keep the repo itself on ext4 too.
6. **Reviewing.** The web UI is the easiest review path on Windows: run
   `photopipe serve` in WSL and open `http://127.0.0.1:8787/` in your **Windows**
   browser (WSL forwards localhost automatically). The symlink review/keepers
   trees are best consumed by Linux tooling inside WSL.

> Native (non-WSL) Windows builds are **not** supported yet — the link trees use
> Unix symlinks.

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

The ONNX model files are **not** committed. Export them once with the Python
scripts in `tools/` (Python is needed *only* here, never at runtime):

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
- `[output] link_type` — `"symlink"` (default) or `"hardlink"` for the review and
  keepers trees.

---

## Reviewing your photos

### Web UI (recommended)

```bash
photopipe serve --port 8787      # then open http://127.0.0.1:8787/
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

### Symlink review tree (file-manager browsing)

```bash
photopipe review-tree ~/Photos/_review --include rejected,duplicates,uncertain
```

Builds `rejected/<reason>/`, `uncertain/`, and `duplicates/group_NNNNN/` folders of
symlinks for browsing in a Linux/macOS file manager. `--regenerate` rebuilds from
scratch.

---

## Exporting keepers

After reviewing, export the kept set — via the **Export keepers** button in the UI
or the CLI:

```bash
photopipe export-keepers ~/Photos/_keepers
```

This builds a links-only `<output>/YYYY-MM/` tree of everything with a **keep**
decision (for duplicate groups, only your chosen keeper). Originals are never
moved or modified. `--regenerate` deletes and rebuilds the tree.

---

## Inspecting the catalog

```bash
# Summary: file/flag counts, duplicate groups, per-camera & per-lens breakdown,
# and catalog/cache disk usage.
photopipe stats

# Everything the catalog knows about one file, as JSON.
photopipe info ~/Photos/2024/DSC01234.arw

# Health check: DB schema, model presence/loadability, selected ORT provider,
# cache writability, free disk. Exits non-zero on a critical problem.
photopipe doctor
```

---

## Command reference

| Command | Purpose |
|---------|---------|
| `scan <PATH>...` | Ingest + analyse library roots. `--no-models`, `--reprocess`. |
| `calibrate` | Rebuild per-lens sharpness baselines from the catalog. |
| `dedupe` | Rebuild duplicate groups from current embeddings. |
| `serve` | Launch the local review web UI. `--port` (default 8787). |
| `review-tree <OUTPUT>` | Generate/update the symlink review tree. `--include`, `--regenerate`. |
| `export-keepers <OUTPUT>` | Materialize the keepers export tree. `--regenerate`. |
| `info <FILE>` | Print all catalog data for one file as JSON. |
| `stats` | Print catalog summary statistics. |
| `doctor` | Diagnose config, models, DB, and system health. |

All commands accept `--config <path>`, `--log-level <level>`, and
`--log-format <pretty|json>`.

---

## Guarantees

- **Non-destructive:** originals are only read; outputs are a separate DuckDB file
  and trees of symlinks.
- **Idempotent:** re-running `scan` on unchanged inputs does no new work;
  re-running `export-keepers` reconciles the tree without touching originals.
- **Decisions persist:** your keep/reject choices live in the catalog and survive
  re-scans and re-dedupes.
