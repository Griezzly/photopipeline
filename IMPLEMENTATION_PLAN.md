# PhotoPipe — Step 1 (Filter Stage) Implementation Plan

> Audience: Claude Code (or any engineer) implementing the project end-to-end.
> Scope: Step 1 of a larger pipeline — defect detection and near-duplicate grouping over a library of RAW photos. Steps 2 (selection) and 3 (editing) are out of scope and will be planned separately.

## 1. Project Overview

PhotoPipe is a local-first command-line tool that ingests a directory of RAW (and optionally JPG) photographs and produces:

1. A DuckDB catalog containing per-file metadata, defect flags, and duplicate-group assignments.
2. A symlink-based "review tree" the user navigates with the OS file browser (Finder / nautilus / etc.) and the default photo viewer.

It is **non-destructive**: originals are never modified or moved. It is **idempotent**: re-running on the same library is safe and skips already-processed files. It is **hardware-portable**: the same binary runs optimally on an Apple Silicon Mac and on a CUDA box, with a CPU fallback.

### 1.1 Goals (Step 1 only)

- Identify blurry photos and back-focus failures, distinguishing them from intentional shallow depth-of-field (bokeh).
- Identify badly exposed photos (over- or underexposed beyond recovery).
- Group near-duplicate photos (burst shots, multiple takes of the same scene) and suggest a "keeper" per group.
- Produce a reviewer-friendly output tree so the user can validate flags using only their OS's default tools.
- Make every model choice runtime-configurable so the pipeline runs on either a 24 GB M4 Pro Mac or an RTX 5090 with the same codebase.

### 1.2 Non-Goals

- No bespoke UI in this phase. Review happens through the OS file browser + symlink tree.
- No image editing or RAW development (that's Step 3).
- No closed-eye detection, smile detection, or aesthetic ranking. Explicitly out of scope per user request.
- No Windows support targeted; code should compile, but Windows-specific paths and CI are not maintained.
- No cloud features. Everything runs locally.

### 1.3 Out of Scope but Worth Knowing About

The longer-term pipeline has three stages — filter (this plan), select (curate keepers from the filter output), and edit (auto-process the final set). The catalog schema in this plan should accommodate the later stages without rewrites: don't paint yourself into a corner.

## 2. Architecture Principles

These are non-negotiable. If you find yourself violating one, stop and propose an alternative.

1. **The DuckDB catalog is the source of truth.** The symlink tree is a *view* of the catalog and can always be regenerated. Nothing about the catalog should depend on the tree existing.
2. **Idempotent processing.** A second run over the same library does no work unless a file's `(path, mtime, size)` has changed. Each phase is also independently re-runnable.
3. **Content-addressable cache.** Preview JPEGs, embeddings, and other derived data are keyed by a stable content hash (`xxh3-128`) of the source file. This makes the cache survive moves and renames.
4. **Plug-and-play models.** Every ML model is hidden behind a trait. ONNX Runtime selects the execution provider at startup (CoreML on Mac, CUDA/TensorRT on Linux+NVIDIA, CPU fallback). The same `.onnx` file works on all of them.
5. **Crash-safe.** Per-file processing happens inside a DuckDB transaction; the catalog never contains half-written rows. A single corrupt file logs and is skipped; it never aborts a full scan.
6. **Non-destructive.** No code path in this project modifies, moves, or deletes an original photo file. Ever.

## 3. Technology Stack

### 3.1 Language and Runtime

**Rust (stable, 1.79+).** Rationale: the pipeline is heavy I/O + classical CV + ONNX inference + parallel processing, which is squarely in Rust's strengths. The `ort` crate gives us a mature, hardware-portable inference path without any Python at runtime. Rust's type system also forces explicit handling of the many failure modes you encounter across a corpus of real-world photo files (corrupt EXIF, unreadable RAWs, missing previews, race conditions during scans).

### 3.2 Core Crates

| Crate | Purpose |
|-------|---------|
| `rawler` | RAW decoding, EXIF, embedded preview extraction |
| `kamadak-exif` | Fallback EXIF parser for JPGs / cases `rawler` doesn't cover |
| `image` | Image I/O (WebP, JPG, PNG) |
| `imageproc` | Convolutions (Laplacian/Sobel), color conversions |
| `ndarray` | N-dim arrays for ML pre/post-processing |
| `ort` (2.x) | ONNX Runtime bindings |
| `duckdb` (with `bundled` feature) | Catalog storage |
| `rayon` | Data-parallel iteration |
| `walkdir` | Directory traversal |
| `xxhash-rust` | Fast content hashing (xxh3-128) |
| `clap` (derive) | CLI parsing |
| `serde`, `toml` | Config |
| `tracing`, `tracing-subscriber` | Structured logging |
| `anyhow`, `thiserror` | Error handling |
| `petgraph` | Connected components for dedupe groups |
| `directories` | XDG / OS-appropriate cache and config dirs |
| `half` | f16 storage for embeddings |

### 3.3 Python (one-time only)

Python is used **exclusively** to export pre-trained models to ONNX. The Rust binary at runtime has no Python dependency. Scripts live in `tools/` and produce `.onnx` files committed to the repo (or downloaded via a script — see §10).

### 3.4 Database Choice: DuckDB via the `duckdb` crate

DuckDB is the catalog store. The win is on the analytical side: per-lens percentile baselines, dedupe similarity queries, and `stats` reporting are all idiomatic single-query SQL in DuckDB and would require either application-side aggregation or extension shenanigans in SQLite. DuckDB also has first-class typed array columns, which lets us store embeddings as `FLOAT[768]` instead of opaque BLOBs — they're introspectable from the CLI, you can write array arithmetic directly in SQL, and they plug straight into DuckDB's `vss` (HNSW) extension if we want to skip brute-force KNN later.

The trade-off is that DuckDB's per-row INSERT throughput is meaningfully worse than SQLite's because it's a columnar engine. **All ingestion writes must be batched.** Collect N processed files (default 64) in a Rust `Vec<IngestedFile>`, then flush them in a single transaction. Two batch-flush patterns are appropriate depending on the table:

- **Pure inserts (no conflict handling, no need for generated IDs in-line):** use the DuckDB `Appender` API (`Connection::appender("table_name")`). Fastest path. Use this for `embeddings`, `defect_flags`, `duplicate_members`, and similar tables where we only ever append.
- **Upserts (`ON CONFLICT ... DO UPDATE`) or `RETURNING` needed:** the Appender does not support `ON CONFLICT` or `RETURNING`. Use prepared `INSERT` statements inside a single `Connection::transaction()` instead. Use this for `files` and `exif`, where we need both upsert semantics and the inserted row's id.

Either way the goal is the same: one transaction per batch, not one statement per row. The pipeline is naturally batched because of `rayon`, so this isn't disruptive.

One connection per worker thread (DuckDB allows multiple in-process connections with MVCC). Use the `duckdb` crate with the `bundled` feature for zero-dep distribution; the binary is bigger than SQLite-bundled but still reasonable (~20 MB).

## 4. Project Structure

```
photopipe/
├── Cargo.toml                  # workspace manifest
├── README.md
├── photopipe.example.toml      # sample config
├── crates/
│   ├── pipeline/               # library crate — all the real work
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── catalog/        # DB schema, migrations, queries
│   │       │   ├── mod.rs
│   │       │   ├── schema.rs
│   │       │   └── queries.rs
│   │       ├── ingest/         # walk dir, hash, EXIF, preview extract
│   │       │   ├── mod.rs
│   │       │   ├── hash.rs
│   │       │   ├── exif.rs
│   │       │   └── preview.rs
│   │       ├── cache/          # content-addressable preview/embedding cache
│   │       │   └── mod.rs
│   │       ├── defect/         # blur, exposure detection
│   │       │   ├── mod.rs
│   │       │   ├── blur.rs
│   │       │   └── exposure.rs
│   │       ├── models/         # ONNX wrappers + traits
│   │       │   ├── mod.rs
│   │       │   ├── hub.rs
│   │       │   ├── embedder.rs    # trait + DinoV2 impl
│   │       │   ├── iqa.rs         # trait + Musiq impl
│   │       │   └── detector.rs    # trait + RtDetr impl
│   │       ├── dedupe/         # similarity + clustering
│   │       │   ├── mod.rs
│   │       │   ├── knn.rs
│   │       │   └── cluster.rs
│   │       ├── calibration/    # per-lens baseline computation
│   │       │   └── mod.rs
│   │       ├── output/         # symlink tree generator
│   │       │   └── mod.rs
│   │       ├── config.rs
│   │       └── error.rs
│   └── cli/                    # photopipe binary
│       ├── Cargo.toml
│       └── src/main.rs
├── models/                     # ONNX model files (or download script)
│   ├── README.md
│   └── download.sh
├── tools/                      # Python — one-off model exports
│   ├── export_dinov2.py
│   ├── export_rt_detr.py
│   ├── export_musiq.py
│   └── requirements.txt
└── tests/
    ├── fixtures/               # small downsampled test photos
    └── integration.rs
```

## 5. Configuration

A single TOML file, default location `~/.config/photopipe/photopipe.toml`, overridable via `--config <path>`. Sensible defaults baked in so a fresh user can run `photopipe scan <dir>` with no setup.

```toml
[catalog]
db_path = "~/.local/share/photopipe/catalog.duckdb"
cache_dir = "~/.cache/photopipe"
write_batch_size = 64                # files per transactional flush during ingest
enable_vss = false                   # load DuckDB's vss extension for HNSW dedupe (experimental)

[ingest]
extensions = ["arw", "cr3", "cr2", "nef", "raf", "rw2", "dng", "jpg", "jpeg"]
follow_symlinks = false
threads = 0                          # 0 = use all logical cores
sidecar_jpg = "prefer"               # "prefer" | "ignore" | "require"
preview_max_long_edge = 2048
preview_quality = 85                  # WebP quality

[models]
device = "auto"                      # "auto" | "coreml" | "cuda" | "tensorrt" | "cpu"
embedder = "dinov2-base"             # "dinov2-small" | "dinov2-base"
iqa = "musiq"                        # "musiq" | "clip-iqa"
detector = "rt-detr-l"               # "rt-detr-l" | "rt-detr-xl"
model_dir = "./models"

[defect.blur]
enable = true
subject_min_area_ratio = 0.02        # ignore detected subjects smaller than this
fallback_center_crop = 0.4           # if no subject found, use center NxN crop
iqa_second_opinion = true            # cross-check Laplacian flag with IQA score
percentile_threshold = 0.10          # bottom 10% within (body, lens, focal, aperture) bucket
min_samples_for_bucket = 30          # else fall back to global threshold

[defect.exposure]
enable = true
clipped_highlights_threshold = 0.05   # > 5% of pixels at ≥ 0.99 → flag
clipped_shadows_threshold = 0.10      # > 10% of pixels at ≤ 0.01 → flag

[dedupe]
enable = true
time_window_seconds = 60
cosine_threshold_within_window = 0.92
cosine_threshold_global = 0.97
knn_k = 10
min_group_size = 2

[output]
review_tree = "<library>/_review"    # literal "<library>" is substituted with scan root
link_type = "symlink"                # "symlink" | "hardlink"
keeper_strategy = "iqa"              # "iqa" | "sharpness" | "iqa_then_sharpness"
```

### 5.1 Hardware Auto-Detection

At startup, `ModelHub::from_config` does:

1. Probe `ort::ExecutionProvider` availability in this order: TensorRT → CUDA → CoreML → CPU. Stop at the first that initializes successfully.
2. Log the chosen provider and version.
3. If `device != "auto"`, force that provider and fail loudly if unavailable.
4. Detect system RAM via `sysinfo`; warn if total RAM < 8 GB; refuse to run if available RAM < 2 GB.

## 6. Catalog Schema (DuckDB)

Versioned via a `schema_version` table. Migrations live in `crates/pipeline/src/catalog/schema.rs` as ordered SQL strings, applied at `Catalog::open()`. DuckDB uses `BIGINT` with `GENERATED BY DEFAULT AS IDENTITY` for auto-increment primary keys, and supports first-class typed arrays.

```sql
CREATE TABLE schema_version (version INTEGER PRIMARY KEY);

CREATE TABLE files (
    id              BIGINT PRIMARY KEY GENERATED BY DEFAULT AS IDENTITY,
    path            VARCHAR NOT NULL UNIQUE,    -- canonical absolute path
    content_hash    VARCHAR NOT NULL,            -- xxh3-128, hex
    size_bytes      BIGINT NOT NULL,
    mtime_ns        BIGINT NOT NULL,
    file_format     VARCHAR NOT NULL,            -- "arw", "cr3", "jpg", ...
    has_sidecar_jpg BOOLEAN NOT NULL DEFAULT false,
    last_processed  BIGINT NOT NULL              -- unix epoch
);
CREATE INDEX idx_files_hash ON files(content_hash);

CREATE TABLE exif (
    file_id              BIGINT PRIMARY KEY REFERENCES files(id),
    captured_at          BIGINT,                 -- unix epoch
    camera_make          VARCHAR,
    camera_model         VARCHAR,
    lens_model           VARCHAR,
    focal_length_mm      REAL,
    aperture             REAL,
    iso                  INTEGER,
    shutter_seconds      REAL,
    width                INTEGER,
    height               INTEGER,
    orientation          SMALLINT
);
CREATE INDEX idx_exif_captured ON exif(captured_at);
CREATE INDEX idx_exif_lens ON exif(camera_model, lens_model);

CREATE TABLE sharpness (
    file_id          BIGINT PRIMARY KEY REFERENCES files(id),
    s_global         REAL NOT NULL,    -- Laplacian variance on whole image
    s_subject        REAL,             -- Laplacian variance on subject ROI (may be NULL)
    s_background     REAL,             -- Laplacian variance on background
    subject_ratio    REAL,             -- (subject_area / total_area), 0..1
    detector_used    VARCHAR           -- "rt-detr-l" | "center-crop-fallback"
);

CREATE TABLE exposure (
    file_id              BIGINT PRIMARY KEY REFERENCES files(id),
    clipped_highlights   REAL NOT NULL,    -- fraction
    clipped_shadows      REAL NOT NULL,
    mean_luma            REAL NOT NULL,
    histogram_skew       REAL NOT NULL
);

CREATE TABLE iqa (
    file_id     BIGINT PRIMARY KEY REFERENCES files(id),
    model       VARCHAR NOT NULL,            -- "musiq" | "clip-iqa"
    score       REAL NOT NULL                -- normalized 0..1
);

-- Native typed array. Dimension is fixed per model variant; if the configured
-- embedder changes between runs, the catalog must be migrated (truncate +
-- repopulate). Use `FLOAT[768]` for dinov2-base, `FLOAT[384]` for dinov2-small.
CREATE TABLE embeddings (
    file_id     BIGINT PRIMARY KEY REFERENCES files(id),
    model       VARCHAR NOT NULL,            -- "dinov2-base"
    vector      FLOAT[]  NOT NULL            -- variable-dim native list; access with vector[i]
);

CREATE TABLE defect_flags (
    id              BIGINT PRIMARY KEY GENERATED BY DEFAULT AS IDENTITY,
    file_id         BIGINT NOT NULL REFERENCES files(id),
    flag_type       VARCHAR NOT NULL,        -- "blur" | "back_focus" | "overexposed" | "underexposed" | "low_iqa"
    confidence      REAL NOT NULL,           -- 0..1
    reason          VARCHAR,                 -- human-readable explanation
    UNIQUE(file_id, flag_type)
);
CREATE INDEX idx_flags_type ON defect_flags(flag_type);

CREATE TABLE duplicate_groups (
    id              BIGINT PRIMARY KEY GENERATED BY DEFAULT AS IDENTITY,
    method          VARCHAR NOT NULL,        -- "time+embed"
    created_at      BIGINT NOT NULL
);

CREATE TABLE duplicate_members (
    group_id            BIGINT NOT NULL REFERENCES duplicate_groups(id),
    file_id             BIGINT NOT NULL REFERENCES files(id),
    is_suggested_keeper BOOLEAN NOT NULL DEFAULT false,
    quality_score       REAL,
    PRIMARY KEY (group_id, file_id)
);
CREATE INDEX idx_dup_members_file ON duplicate_members(file_id);

CREATE TABLE sharpness_baseline (
    camera_model     VARCHAR NOT NULL,
    lens_model       VARCHAR NOT NULL,
    focal_bucket     INTEGER NOT NULL,        -- mm, snapped to bucket
    aperture_bucket  REAL NOT NULL,           -- f-number, snapped to 1/3 stop
    s_subject_p10    REAL NOT NULL,
    s_subject_p50    REAL NOT NULL,
    s_subject_p90    REAL NOT NULL,
    n_samples        INTEGER NOT NULL,
    last_updated     BIGINT NOT NULL,
    PRIMARY KEY (camera_model, lens_model, focal_bucket, aperture_bucket)
);
```

**DuckDB-specific notes for Claude Code:**

- DuckDB does not support `ON DELETE CASCADE` on foreign keys (as of the current stable release). Cascade deletes happen in application code if needed. For Step 1 we never delete catalog rows, so this is fine.
- Identity columns work like Postgres — let DuckDB assign the id, then read it back via `RETURNING id` on the INSERT.
- `BLOB` is supported, but prefer `FLOAT[]` for embeddings — it's typed, indexable by element, and works with the `vss` extension natively.
- For bulk writes in the ingest hot path, batch into transactions (see §3.4). Pure inserts use the `Appender` API; tables that need `ON CONFLICT` or `RETURNING` (specifically `files` and `exif`) use prepared `INSERT` statements inside a single `Connection::transaction()`. Both are an order of magnitude faster than row-at-a-time `execute()` calls.
- Migrations: wrap each migration's SQL in `BEGIN TRANSACTION; ... COMMIT;`. DuckDB has full ACID transactions.

Bucketing helpers (lives in `calibration/mod.rs`):

- Focal-length buckets: snap to nearest of `[14, 18, 24, 28, 35, 50, 70, 85, 105, 135, 200, 300, 400, 600]`.
- Aperture buckets: snap to nearest 1/3 stop. Implementation: `round(log2(f) * 3) / 3`, then `2^x`.

## 7. CLI Surface

```
photopipe scan <PATH>...           Ingest + analyze one or more library roots.
                                   Flags: --config, --no-models (skip ML phases),
                                          --reprocess (force re-analysis of all files)

photopipe calibrate                Rebuild per-lens sharpness baselines from catalog.
                                   Should be run after scanning at least a few hundred
                                   photos per lens for meaningful percentiles.

photopipe dedupe                   Rebuild duplicate groups using current embeddings.

photopipe review-tree <OUTPUT>     Generate/update the symlink review tree.
                                   Flags: --include rejected,duplicates,uncertain
                                          --regenerate (delete + rebuild)

photopipe info <FILE>              Show all catalog data for a single file (JSON).

photopipe stats                    Summary: file count, flag counts, group counts,
                                   disk usage, per-camera/per-lens breakdowns.

photopipe doctor                   Diagnostic: config sanity, DB schema version,
                                   model files present and loadable, ORT EP detected,
                                   disk free space.
```

All commands accept `--config <path>` and `--log-level <level>`.

## 8. Implementation Phases

Phases are sequential. Each phase has explicit acceptance criteria; do not move on until they pass.

**Status snapshot (2026-06-26):**

| Phase | Status | Notes |
|-------|--------|-------|
| 0 — Scaffold | ✅ Done | workspace, CLI, config, `doctor` skeleton |
| 1 — Ingest + Catalog | ✅ Done | full DuckDB schema created up front; idempotent scan |
| 2 — Classical defects | ✅ Done | Laplacian sharpness + histogram exposure |
| 3 — ONNX models | 🟡 Partial | DinoV2 embedder + CLIP-IQA done & wired (`analyze_ml`). **RT-DETR detector deferred** (see §15.7); sharpness uses center-crop fallback until it lands. CoreML EP disabled (macOS runs CPU). |
| 4 — Calibration + refined blur | ❌ Not started | depends on RT-DETR for meaningful subject ROIs |
| 5 — Duplicate detection | ❌ Not started | embeddings already available; self-contained |
| 6 — Review tree output | ❌ Not started | |
| 7 — Polish (doctor/stats/info/docs) | 🟡 Partial | `doctor` partially implemented |

⚠️ **Test fixtures are not yet in the repo.** Acceptance criteria below that reference curated real photos (Phases 2, 3, 5) cannot be validated until the user supplies fixtures into `tests/fixtures/`.

### Phase 0 — Project Scaffold (~½ day)

**Deliverables:**
- Cargo workspace with `pipeline` library crate and `cli` binary crate.
- GitHub Actions CI: `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test`.
- `tracing` + `tracing-subscriber` initialized in `cli/main.rs`, with `--log-level` flag and `RUST_LOG` fallback.
- Config loading (`config.rs`): TOML parsing with serde, defaults from a `default_config()` function, path-expansion for `~`.
- `photopipe doctor` command — at this phase it only loads config and prints it plus detected OS.
- A working `clap` derive-based CLI with all command stubs.

**Acceptance:**
- `cargo build --release` succeeds.
- `cargo test` runs (even if empty).
- `photopipe doctor` prints the effective config and OS info.
- CI passes on a fresh push.

### Phase 1 — Ingestion and Catalog (1–2 days)

**Files:** `catalog/*`, `ingest/*`, `cache/mod.rs`.

**Public API sketch:**

```rust
pub struct Catalog { /* DuckDB connection pool */ }
impl Catalog {
    pub fn open(path: &Path) -> Result<Self>;
    pub fn upsert_file(&self, file: &IngestedFile) -> Result<i64>;
    pub fn upsert_exif(&self, file_id: i64, exif: &ExifData) -> Result<()>;
    pub fn needs_processing(&self, path: &Path, mtime_ns: i64, size: u64) -> Result<bool>;
}

pub struct IngestedFile {
    pub path: PathBuf,
    pub content_hash: u128,    // xxh3-128
    pub size: u64,
    pub mtime_ns: i64,
    pub format: FileFormat,
    pub has_sidecar_jpg: bool,
}

pub struct ExifData {
    pub captured_at: Option<i64>,
    pub camera_make: Option<String>,
    pub camera_model: Option<String>,
    pub lens_model: Option<String>,
    pub focal_length_mm: Option<f32>,
    pub aperture: Option<f32>,
    pub iso: Option<u32>,
    pub shutter_seconds: Option<f32>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub orientation: Option<u16>,
}

pub fn ingest_directory(
    roots: &[PathBuf],
    catalog: &Catalog,
    cache: &Cache,
    cfg: &IngestConfig,
) -> Result<IngestReport>;
```

**Behavior:**

1. Use `walkdir` to traverse each root in parallel via `rayon::par_bridge`.
2. For each file matching `cfg.extensions`:
   1. Stat to get `(size, mtime_ns)`.
   2. Query `catalog.needs_processing(path, mtime_ns, size)`. If `false`, skip.
   3. Compute `xxh3-128` over the file contents (stream in 1 MB chunks).
   4. Read EXIF: try `rawler::RawSource` for RAW formats, fall back to `kamadak-exif` for JPGs.
   5. Detect sidecar JPG: same basename, `.jpg`/`.jpeg`/`.JPG`/`.JPEG`, same directory.
   6. Extract preview into the cache:
      - If sidecar JPG exists and `sidecar_jpg = "prefer"`, use it directly.
      - Else, use `rawler` to pull the embedded preview (`get_jpeg_preview()` or `get_thumbnail()` as a fallback).
      - Decode, resize (long edge ≤ `preview_max_long_edge`), encode as WebP at `preview_quality`, write to `cache_dir/previews/<hex[0..2]>/<hex>.webp`.
   7. Push the parsed result into a per-worker `Vec<IngestedFile>`. When the vec reaches `write_batch_size`, the worker takes the catalog lock, opens an Appender (or wraps INSERTs in a single transaction), flushes the batch, and clears the vec.
3. At end of scan, flush all remaining partial batches.
4. Aggregate counts (processed, skipped, errored) into `IngestReport`.

**Error handling:**
- Any per-file error is logged with the path and reason, and counted; the scan continues.
- A failed cache write is non-fatal; the file is still cataloged, but its `preview_path` is null.

**Acceptance:**
- Running `photopipe scan ./test-fixtures` twice in a row: second run processes 0 files (idempotency).
- A test fixture directory of ~30 mixed files (`.arw`, `.cr3`, `.nef`, `.jpg`) all appear in the `files` table with correct hashes.
- EXIF round-trip: pick a file with known EXIF, scan, query catalog, verify all fields match.
- Cache directory contains preview WebPs at expected paths.
- Delete one of the originals; re-run scan; the catalog row is *not* deleted (we don't garbage-collect on scan). Add a separate `photopipe prune` later if needed (out of scope for this phase).

### Phase 2 — Classical Defect Detection (1–2 days)

**Files:** `defect/blur.rs`, `defect/exposure.rs`.

**Sharpness algorithm:**

```rust
pub fn compute_sharpness(
    preview: &DynamicImage,
    subject_rois: Option<&[BBox]>,    // None for now; filled in Phase 3
    cfg: &BlurConfig,
) -> SharpnessResult {
    // 1. Convert to grayscale.
    // 2. Compute Laplacian convolution (kernel: [[0,1,0],[1,-4,1],[0,1,0]]).
    // 3. s_global = variance of Laplacian over entire image.
    // 4. If subject_rois present and any meet `subject_min_area_ratio`:
    //      s_subject = variance over the union of subject ROIs
    //      s_background = variance over the complement
    //      detector_used = "rt-detr-l"
    //    Else:
    //      Take a center crop of size `fallback_center_crop` × image dims.
    //      s_subject = variance on the center crop
    //      s_background = variance on the surrounding region
    //      detector_used = "center-crop-fallback"
    // 5. Return SharpnessResult { s_global, s_subject, s_background, subject_ratio, detector_used }
}
```

Store the result in the `sharpness` table. *Don't flag yet* — flagging happens in Phase 4 after calibration.

**Exposure algorithm:**

```rust
pub fn compute_exposure(preview: &DynamicImage) -> ExposureResult {
    // 1. Convert to luminance (Rec. 709 weights).
    // 2. Compute 256-bin histogram.
    // 3. clipped_highlights = sum(hist[253..=255]) / total
    // 4. clipped_shadows = sum(hist[0..=2]) / total
    // 5. mean_luma = weighted mean
    // 6. histogram_skew = third standardized moment
    // 7. Return ExposureResult.
}
```

Flag immediately (no calibration needed):
- `overexposed` if `clipped_highlights > cfg.clipped_highlights_threshold`.
- `underexposed` if `clipped_shadows > cfg.clipped_shadows_threshold`.

Write to `defect_flags` with `confidence = min(1.0, clipped_fraction / threshold)`.

**Acceptance:**
- Curated test set (commit to `tests/fixtures/exposure/`): 10 normal, 5 overexposed, 5 underexposed. All exposure flags are correct.
- Curated sharpness fixtures (`tests/fixtures/sharpness/`): 10 sharp, 10 blurry, 5 shallow-DoF with sharp subject. Without subject detection (Phase 3), expect the sharp/blurry distinction to mostly work, but the shallow-DoF cases may be false-positives for now — that's fine, Phase 4 fixes them.

### Phase 3 — ONNX Model Integration (2–3 days)

**Files:** `models/*`.

**Trait definitions:**

```rust
use std::sync::Arc;
use anyhow::Result;
use image::DynamicImage;
use half::f16;

pub trait Embedder: Send + Sync {
    fn embed(&self, img: &DynamicImage) -> Result<Vec<f16>>;
    fn dim(&self) -> usize;
    fn name(&self) -> &str;
}

pub trait Iqa: Send + Sync {
    fn score(&self, img: &DynamicImage) -> Result<f32>;  // returns 0..1
    fn name(&self) -> &str;
}

#[derive(Debug, Clone)]
pub struct BBox { pub x: f32, pub y: f32, pub w: f32, pub h: f32 }  // normalized 0..1

#[derive(Debug, Clone)]
pub enum SubjectClass { Person, Animal, Vehicle, Object, Other }

#[derive(Debug, Clone)]
pub struct DetectedSubject {
    pub bbox: BBox,
    pub class: SubjectClass,
    pub confidence: f32,
}

pub trait SubjectDetector: Send + Sync {
    fn detect(&self, img: &DynamicImage) -> Result<Vec<DetectedSubject>>;
    fn name(&self) -> &str;
}

pub struct ModelHub {
    pub embedder: Arc<dyn Embedder>,
    pub iqa: Arc<dyn Iqa>,
    pub detector: Arc<dyn SubjectDetector>,
    pub provider: String,         // logged, e.g. "CoreMLExecutionProvider"
}

impl ModelHub {
    pub fn from_config(cfg: &ModelsConfig) -> Result<Self>;
}
```

**Implementation notes:**
- One `ort::Environment` per process.
- One `ort::Session` per model. Sessions are safe to share across threads for inference; wrap in `Arc`.
- Image preprocessing in pure Rust (resize via `image::imageops::resize` with `Lanczos3`, normalize, layout HWC→CHW into `ndarray::Array4<f32>`).
- Each concrete impl owns its preprocessing constants (mean, std, input size).

**Concrete implementations (initial set):**

| Trait | Variant | ONNX file | Input size | Notes |
|-------|---------|-----------|-----------|-------|
| `Embedder` | `DinoV2Embedder` | `dinov2_base.onnx` | 224×224 | ImageNet mean/std |
| `SubjectDetector` | `RtDetrDetector` | `rt-detr-l.onnx` | 640×640 | COCO classes; map to `SubjectClass` |
| `Iqa` | `MusiqIqa` | `musiq.onnx` | multi-scale, see export script | Output normalized to 0..1 |

**After this phase, wire the detector into `defect::blur::compute_sharpness`** (replace the `None` ROI argument with real detections).

**Acceptance:**
- `photopipe doctor` reports the loaded models and the chosen ORT execution provider.
- Same image embedded on Mac (CoreML) and Linux (CUDA) yields vectors with cosine similarity > 0.999.
- Subject detector correctly localizes a person in a test image with bbox IoU > 0.7 against a hand-labeled fixture.
- IQA model produces scores in 0..1 for a batch of test images, with a high-quality fixture scoring > 0.7 and a known-bad fixture scoring < 0.3.

### Phase 4 — Lens Calibration and Refined Blur Flagging (1 day)

**Files:** `calibration/mod.rs`, plus new logic in `defect/blur.rs`.

**Calibration command (`photopipe calibrate`):**

DuckDB's analytical SQL does most of the work. The whole calibration query is essentially one statement:

```sql
INSERT OR REPLACE INTO sharpness_baseline
SELECT
    e.camera_model,
    e.lens_model,
    focal_bucket(e.focal_length_mm) AS focal_bucket,
    aperture_bucket(e.aperture) AS aperture_bucket,
    quantile_cont(s.s_subject, 0.10) AS s_subject_p10,
    quantile_cont(s.s_subject, 0.50) AS s_subject_p50,
    quantile_cont(s.s_subject, 0.90) AS s_subject_p90,
    COUNT(*) AS n_samples,
    epoch(now()) AS last_updated
FROM sharpness s
JOIN exif e ON s.file_id = e.file_id
WHERE s.s_subject IS NOT NULL
  AND e.camera_model IS NOT NULL
  AND e.lens_model IS NOT NULL
  AND e.focal_length_mm IS NOT NULL
  AND e.aperture IS NOT NULL
GROUP BY e.camera_model, e.lens_model, focal_bucket, aperture_bucket
HAVING COUNT(*) >= $min_samples_for_bucket;
```

Register `focal_bucket` and `aperture_bucket` as DuckDB scalar UDFs (via the `duckdb` crate's `create_scalar_function`) or compute them in a CTE. Either is fine.

Also compute a global fallback (no GROUP BY) and write it under a sentinel key like `('*', '*', 0, 0.0)`.

**Re-flagging (after calibration):**

For each file with sharpness data:

```text
bucket = lookup(exif)
if bucket exists and bucket.n_samples >= min_samples_for_bucket:
    threshold = bucket.s_subject_p10
else:
    threshold = global.s_subject_p10

if file.s_subject < threshold:
    if file.s_background > file.s_subject * 2.0:
        flag: back_focus, confidence = clamp((threshold - s_subject) / threshold, 0..1)
    else:
        flag: blur, confidence = clamp((threshold - s_subject) / threshold, 0..1)

if cfg.iqa_second_opinion:
    if iqa.score is in bottom decile globally:
        flag: low_iqa (always, not just when also blur-flagged)
    if both blur AND low_iqa: bump blur confidence by +0.2 (capped at 1.0)
```

**Acceptance:**
- After calibration on the test fixtures, the shallow-DoF photos (sharp subject, blurry background) are NOT flagged as blur.
- Back-focus photos ARE flagged with `back_focus`.
- Genuinely blurry photos ARE flagged with `blur`.
- A photo with no calibration data falls back to global threshold cleanly (no crash).

### Phase 5 — Duplicate Detection (1–2 days)

**Files:** `dedupe/*`.

**Algorithm:**

1. Load all embeddings into memory as a `Vec<(file_id, f16-vector)>`. For 50k photos × 768 dims × 2 bytes ≈ 73 MB. Comfortable.
2. Convert to f32 for similarity math; L2-normalize each vector.
3. Build edges:
   - **Time-window edges:** sort by `captured_at`. For each pair `(i, j)` where `|captured_at_i − captured_at_j| ≤ cfg.time_window_seconds`, add edge if cosine ≥ `cfg.cosine_threshold_within_window`.
   - **Global KNN edges:** for each photo, find its top `cfg.knn_k` nearest neighbors by cosine. For neighbors with cosine ≥ `cfg.cosine_threshold_global`, add an edge.
   - **KNN backend.** Two implementations behind a `KnnIndex` trait:
     - `BruteForceKnn`: pull embeddings into Rust, L2-normalize, compute cosine via matrix multiply (use `ndarray` with BLAS, or just `rayon` chunks). Fine up to ~50k photos.
     - `DuckDbVssKnn`: if `catalog.enable_vss = true`, load the `vss` extension at startup, build an HNSW index on the `embeddings.vector` column once, then issue `array_cosine_similarity(vector, $query) > $threshold` queries directly in SQL. Recommended above ~50k photos.
   - Default to brute force; switching is a config flip.
4. Build graph via `petgraph::Graph<i64, ()>`, run `petgraph::algo::tarjan_scc` or `kosaraju_scc`.
5. For each connected component with `≥ cfg.min_group_size` nodes:
   - Create a `duplicate_groups` row (`method = "time+embed"`).
   - For each member, compute `quality_score`:
     ```
     quality_score = iqa.score
                   - 0.3 * (1 if has blur flag else 0)
                   - 0.2 * (1 if has back_focus flag else 0)
                   - 0.2 * max(clipped_highlights, clipped_shadows)
     ```
   - Mark the highest-scoring member with `is_suggested_keeper = 1`.

**Idempotency:** `photopipe dedupe` clears `duplicate_groups` + `duplicate_members` and rebuilds. This is safe because the tables don't hold user state.

**Acceptance:**
- A burst of 5 shots taken within 2 seconds clusters into one group.
- The same scene photographed twice 10 minutes apart with different framing clusters together (relies on embedding similarity).
- The sharpest, best-exposed photo of a burst is selected as keeper.
- Two unrelated photos with similar color palettes (e.g., two different sunsets) do *not* cluster.

### Phase 6 — Review Tree Output (½–1 day)

**Files:** `output/mod.rs`.

**Tree layout (relative to output root):**

```
<output>/
├── README.txt                       # autogenerated, explains how to review
├── rejected/
│   ├── blur/
│   │   └── 2024-08/                 # bucketed by year-month
│   │       ├── IMG_001.ARW          # symlink to original
│   │       └── ...
│   ├── back_focus/
│   ├── overexposed/
│   ├── underexposed/
│   └── low_quality/
├── duplicates/
│   ├── group_00042_2024-08-12/
│   │   ├── _keeper/
│   │   │   └── IMG_005.ARW          # symlink to suggested keeper
│   │   └── _others/
│   │       ├── IMG_003.ARW
│   │       ├── IMG_004.ARW
│   │       └── IMG_006.ARW
│   └── ...
└── uncertain/                       # flags with confidence < 0.6
    └── 2024-08/
        └── ...
```

**Behavior:**

- The literal `<library>` token in the configured `review_tree` path is replaced with the scan root at runtime. If multiple roots, use the first.
- Symlinks point to absolute paths so the tree is portable across moves.
- Filenames preserve the original basename (so Finder's "Show in Finder" works).
- If `link_type = "hardlink"`, create hardlinks (requires same filesystem; fail loudly otherwise).
- `--regenerate` deletes the tree first; default behavior is incremental update (add new symlinks, remove stale ones).

**README.txt content (autogenerated):**

```
PhotoPipe Review Tree
=====================

This directory was generated by photopipe at 2024-08-12 14:23:01.
DO NOT delete files in `<library-root>`; this tree only contains symlinks.

What to review:

  rejected/blur/         — photos flagged as out-of-focus or blurry.
                           Spot-check; if any are actually fine, see "Overriding".
  rejected/back_focus/   — photos where focus landed on background, not subject.
  rejected/overexposed/  — highlights blown beyond likely recovery.
  rejected/underexposed/ — shadows crushed.
  duplicates/group_NN/   — burst shots or near-identical scenes.
                           `_keeper/` contains photopipe's suggested best.
                           Open the whole group folder, pick what to keep.
  uncertain/             — low-confidence flags. Worth a second look.

Overriding:
  The catalog at <db-path> is the source of truth. Deleting a symlink here
  has NO effect on flags. To override:
    photopipe override <file> --remove-flag blur
  (forthcoming command)

To delete the originals you've decided to reject:
  photopipe commit-rejects --confirm   (forthcoming command)
```

**Acceptance:**
- Opening `<output>/duplicates/group_42_2024-08-12/` in Finder shows all members; arrow keys + spacebar work for review.
- Counting originals before and after `review-tree` shows the original count unchanged.
- Deleting a symlink does not delete the original (test explicitly).
- `--regenerate` after deleting half the symlinks rebuilds them all.

### Phase 7 — Polish, Doctor, Stats, Docs (½ day)

**Deliverables:**
- `photopipe doctor` full implementation: DB schema version match, model files exist and load successfully, ORT EP detected, cache dir writable, disk free > 5 GB.
- `photopipe stats`: counts per flag type, group counts, total catalog size, per-camera/per-lens breakdown.
- `photopipe info <file>` prints a JSON dump of all catalog rows for that file.
- `README.md` quickstart with: install, sample config, common workflows.
- `photopipe.example.toml` committed at repo root.

**Acceptance:**
- `photopipe doctor` passes on a freshly-cloned + built repo on Mac and Linux.
- `photopipe stats` produces sensible output on the test fixtures.

## 9. Model Preparation (Python ONNX Export)

Scripts in `tools/`. Each is standalone, ~50 lines, runnable from a fresh `pip install -r requirements.txt`. Each script:

1. Downloads the model from its canonical source (HuggingFace hub or model zoo).
2. Wraps it in a thin `nn.Module` that handles preprocessing if needed.
3. Traces / exports to ONNX (opset 17+).
4. Runs `onnxsim.simplify` to fold constants.
5. Validates output matches the PyTorch reference for a fixed input within tolerance (relative error < 1e-3).
6. Writes to `models/<name>.onnx`.

**`tools/requirements.txt`:**
```
torch>=2.2
torchvision>=0.17
transformers>=4.40
onnx>=1.16
onnxruntime>=1.18
onnxsim>=0.4
opencv-python-headless
huggingface-hub
```

**`tools/export_dinov2.py`:** Loads `facebook/dinov2-base` from HF, exports with dynamic batch axis, fixed 224×224 input.

**`tools/export_rt_detr.py`:** Loads `PekingU/rtdetr_r50vd` (Apache 2.0) from HF, exports with dynamic batch, fixed 640×640 input. The model outputs (logits, boxes); post-process to (bbox, class, confidence) in Rust.

**`tools/export_musiq.py`:** MUSIQ is harder — the official repo is TF. Two options: (a) use the PyTorch port at `github.com/anse3832/MUSIQ`; (b) substitute CLIP-IQA, which reuses the OpenCLIP backbone we'd already have. **Default decision: ship CLIP-IQA as the IQA model** (simpler, one fewer backbone), and document MUSIQ as an alternative for users who want it. Update the config: `iqa = "clip-iqa"` is default.

> Note to Claude Code: if MUSIQ ONNX export proves painful, drop it and use only CLIP-IQA. Don't burn a day fighting the TF→ONNX conversion.

`models/download.sh`: a shell script that downloads pre-exported ONNX files from a URL (TBD; for now, just have the script run the Python exporters). Committing the ONNX files directly is also acceptable if they're under 1 GB total.

## 10. Testing Strategy

**Unit tests** (alongside source files):
- EXIF parsing edge cases (missing fields, weird timezone strings, fractional ISO).
- Laplacian on synthetic images (uniform → variance ≈ 0; checkerboard → high variance).
- Histogram computation on solid-color images.
- Bucket-rounding logic (focal lengths, apertures).
- Union-find / connected components on hand-built graphs.
- Percentile computation on small samples.

**Integration tests** (`tests/integration.rs`):
- Curate ~30 small (≤ 500 KB each) downsampled real photos in `tests/fixtures/`, organized as `blurry/`, `sharp/`, `overexposed/`, `underexposed/`, `bokeh-sharp-subject/`, `burst/`. Commit them.
- Each integration test sets up a temp dir, runs the relevant pipeline phase, asserts catalog contents.

**Property tests** (`proptest`):
- Bucketing: `bucket(f) == bucket(f)` (idempotent).
- Embedding L2-normalization: norm within 1e-6 of 1.0.

**CLI tests** (`assert_cmd`):
- `photopipe scan ./fixtures` → exit 0, files in catalog match expected count.
- `photopipe doctor` → exit 0 in healthy state, exit non-zero with missing models.
- `photopipe info <file>` → valid JSON output.

**Snapshot tests** (`insta`):
- Catalog row dumps for fixture files (use a stable text format that excludes timestamps).

## 11. Error Handling Philosophy

- **Library code** uses `thiserror`-derived error types per module (`IngestError`, `CatalogError`, `ModelError`, etc.).
- **Binary code** uses `anyhow::Result` at top-level command handlers.
- **Per-file errors do not abort.** Wrap each file's processing in a `catch`; log the path and reason; increment the error counter; continue.
- **Transactions per file.** If a file's processing partially fails (e.g., EXIF read OK, preview extraction failed), the catalog gets a row with null `preview_path` and a logged warning. Half-written rows are not allowed.
- **Schema migrations are atomic.** Wrap migration SQL in a `BEGIN`/`COMMIT` block; on failure, roll back and refuse to start.

## 12. Logging and Observability

- `tracing` spans for: each scan invocation (`span!(Level::INFO, "scan", root = ?root)`), each file (`span!(Level::DEBUG, "file", path = ?path)`), each phase.
- Always log at INFO: phase start/end with file counts and durations.
- Log at WARN: per-file errors with file path and reason.
- Log at DEBUG: per-file processing details.
- Default level: INFO. `--log-level debug` or `RUST_LOG=photopipe=debug` for verbose.
- Optional `--log-format json` for machine-readable output.

## 13. Performance Targets

These are guidelines, not gates. Measure first, optimize if needed.

For 10,000 photos on M4 Pro 24GB:
- Phase 1 (ingest + EXIF + preview extract): < 30 minutes.
- Phase 2 + 3 (defects + ML inference): < 2 hours.
- Phase 5 (dedupe): < 5 minutes.
- Peak memory: < 8 GB.

For the same on RTX 5090: roughly 3–5× faster on the ML phases.

## 14. Sequencing and Pull Request Plan

Each phase → one PR (or one cohesive set of commits if working solo). Suggested PR titles:

1. `chore: cargo workspace scaffold + CI`
2. `feat(ingest): RAW walking, hashing, EXIF, preview cache`
3. `feat(defect): Laplacian sharpness + histogram exposure`
4. `feat(models): ONNX runtime with DINOv2 + RT-DETR + CLIP-IQA`
5. `feat(calibration): per-lens sharpness baselines + bokeh-aware blur flagging`
6. `feat(dedupe): time+embedding clustering with keeper selection`
7. `feat(output): symlink review tree`
8. `chore(polish): doctor, stats, docs`

## 15. Open Questions — Resolution Log

These were the questions to confirm before coding. Status as of 2026-06-26:

1. **Primary RAW formats to test against?** — ⏳ *Pending fixtures.* The user will supply curated real photos into `tests/fixtures/` (organized by category). Specific camera bodies TBD when those photos land; that determines which `rawler` decode paths get real coverage.
2. **Cache and DB location?** — ✅ XDG defaults: `~/.cache/photopipe/` and `~/.local/share/photopipe/catalog.duckdb`. Implemented in `config.rs`.
3. **MUSIQ vs CLIP-IQA?** — ✅ CLIP-IQA. MUSIQ dropped (TF→ONNX export not worth the time). `iqa = "clip-iqa"` is the default.
4. **Ship pre-exported ONNX files, or require export scripts?** — ✅ ONNX files are gitignored; produced by `tools/export_*.py`. **Note:** the `tools/` directory is not yet committed (export scripts referenced by `models/README.md` are missing) — recreate it when the RT-DETR work lands.
5. **License?** — ✅ Permissive only (Apache-2.0 / MIT / BSD). No AGPL deps.
6. **Output `link_type` default?** — ✅ `symlink` (absolute targets, portable across moves of the tree itself).
7. **RT-DETR subject detector** (added 2026-06-26) — 🔧 *Decision: unblock it properly* before Phase 4. The deferred stub stays only until the ONNX export blocker (int64 `Cos` in `rtdetr_r50vd` positional encodings) is fixed via graph surgery or an alternate checkpoint. Phase 4's bokeh-vs-blur logic depends on real subject ROIs.

---

## Appendix A — Subject Class Mapping (RT-DETR / COCO)

RT-DETR is trained on COCO (80 classes). Map to internal `SubjectClass`:

- `Person`: COCO `person`
- `Animal`: COCO `bird`, `cat`, `dog`, `horse`, `sheep`, `cow`, `elephant`, `bear`, `zebra`, `giraffe`
- `Vehicle`: COCO `bicycle`, `car`, `motorcycle`, `airplane`, `bus`, `train`, `truck`, `boat`
- `Object`: everything else
- `Other`: unmapped / unknown class id

For sharpness ROI: prefer `Person` and `Animal` boxes; fall back to other classes; fall back to center crop.

## Appendix B — Focal-Length and Aperture Buckets

Focal length (mm):
```
[14, 18, 24, 28, 35, 50, 70, 85, 105, 135, 200, 300, 400, 600]
```
Snap to nearest. Photos outside this range (e.g., 800 mm) go to the nearest edge bucket.

Aperture (f-number), 1/3 stop steps starting at f/1.0:
```
1.0, 1.1, 1.2, 1.4, 1.6, 1.8, 2.0, 2.2, 2.5, 2.8, 3.2, 3.5,
4.0, 4.5, 5.0, 5.6, 6.3, 7.1, 8.0, 9.0, 10, 11, 13, 14, 16, 18, 20, 22
```
Snap via `2.0_f32.powf((log2(f) * 3.0).round() / 3.0)`.

## Appendix C — DuckDB Notes

| Aspect | DuckDB | SQLite (for reference) |
|--------|--------|------------------------|
| Workload fit | Analytical (calibration percentiles, dedupe, stats) ✓ | OLTP |
| Per-row INSERT throughput | Lower — **must batch with Appender** | Very high |
| Bulk INSERT (Appender) | Very high | N/A |
| Typed arrays (`FLOAT[768]`) | ✓ — embeddings stay typed | BLOB only |
| Vector similarity extension | `vss` (HNSW) built in | Requires external extension |
| Window / quantile functions | First-class | Limited |
| Foreign key cascade | Not supported (manage in app) | ✓ |
| Concurrent in-process connections | MVCC, multiple writers | One writer at a time |
| Multi-process write access | Not supported (single process owns DB) | Limited (locking) |
| Inspect from CLI | `duckdb catalog.duckdb` | `sqlite3 catalog.db` |
| Bundled-binary size | ~20 MB | ~5 MB |

**Things to watch out for during implementation:**

- The `duckdb` crate API mirrors `rusqlite` closely but differs around prepared statements with parameterized list literals. When inserting an embedding, bind the vector as `FLOAT[]` via the crate's `Value::List` (or use the Appender, which sidesteps this).
- DuckDB files are not portable across major versions for the storage format until v1.0+. Pin the `duckdb` crate to a specific minor version and document upgrades.
- DuckDB holds a file lock when open in read-write mode. The pipeline should `Catalog::open` once at startup and pass a `&Catalog` (with internal connection pool) into worker threads.
- The `vss` extension is marked experimental. Only enable it via `enable_vss = true` after you've validated dedupe quality with brute force.
