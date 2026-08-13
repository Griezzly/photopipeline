# Automatic RAW Development (`photopipe finish`) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Turn the photos a user kept into finished JPEGs automatically, with no per-photo human input, via a new `photopipe finish` command.

**Architecture:** Three separable layers. A **measure** stage reads raw-linear sensor statistics with `rawler`. A **decide** stage is a pure function from those statistics plus EXIF to a typed `EditRecipe` — no image data, no I/O, so all tuning logic is unit-testable over numbers. A **render** stage emits the recipe as a RawTherapee `.pp3`, drives `rawtherapee-cli` as a subprocess to a 16-bit TIFF, and (in Phase 2) applies an image-specific 3D LUT in Rust before encoding the JPEG.

**Tech Stack:** Rust 2021, `rawler` 0.7 (raw decode), `duckdb` 1 (catalog), `image` 0.25 (TIFF read / JPEG write), `ort` 2.0.0-rc.12 (ONNX predictor), `rawtherapee-cli` (external subprocess), Python 3.12 in `tools/.venv` (one-time ONNX export only).

**Spec:** `docs/superpowers/specs/2026-07-29-auto-develop-design.md` (revised 2026-08-09). Read §0 first — it lists the eight amendments this plan implements.

## Global Constraints

Copied from `CLAUDE.md` and the spec. Every task's requirements implicitly include this section.

- **No SQLite.** DuckDB only.
- **No AGPL dependencies.** RawTherapee is GPL-3 but is invoked as a subprocess and never linked, so no obligation propagates. Do not link or vendor any RawTherapee code.
- **No Python at runtime.** Python lives only in `tools/` for one-time ONNX exports. The shipped binary has zero Python dependency.
- **No mutation of original photo files.** Reads only, even in error paths. `.pp3` files go next to the output JPEG; `.cube` files go in the cache dir. **Never** beside the source raw.
- **Idempotency is a correctness requirement, not a perf goal.** A second `finish` run over an unchanged library must perform zero renders.
- **One corrupt file must never abort a run.** Wrap per-file work so an `Err` logs `tracing::warn!(path = %p.display(), error = %e)` and continues, leaving no `edits` row.
- **Bulk inserts use the DuckDB Appender API.** Row-at-a-time `INSERT` is a perf bug. Batch size lives in `CatalogConfig::write_batch_size` (default 64). **Scope (ruled 2026-08-09):** this binds bulk ingest paths. `upsert_raw_stats` and `upsert_edit` use `INSERT … ON CONFLICT` instead, because the Appender cannot express `ON CONFLICT` — the same reason already documented for `flush_blur_flag_batch` in `catalog/mod.rs` — and `finish` writes one row per photo between multi-second renders, so there is no batch to accelerate.
- **Migrations are atomic** — each wrapped in `BEGIN TRANSACTION; … COMMIT;`.
- **`ON DELETE CASCADE` is unsupported** in this DuckDB version. Manage cascades in application code.
- **No `println!` outside user-facing CLI output.** Use `tracing`: `info!` for phase events, `warn!` for per-file failures, `debug!` for detail.
- **`anyhow::Result` at command-handler boundaries; `thiserror` types inside `pipeline`** (`crates/pipeline/src/error.rs`).
- **Windows must keep working.** No Unix-only APIs. Subprocess paths and arguments must be built with `PathBuf`/`OsStr`, never string concatenation with `/`.
- **ONNX weights are gitignored** and produced on the user's machine by a `tools/` script. The repo never contains or redistributes them.
- **Every task ends green:** `cargo fmt`, `cargo clippy --all-targets --all-features -- -D warnings`, `cargo test --all`.
- **Commit messages are conventional-commit style** (`feat(develop): …`, `fix(develop): …`, `docs(spec): …`). **Do not sign commits with Claude.**

## File Structure

**New — `crates/pipeline/src/develop/`** (the whole feature; one concern per file):

| File | Responsibility |
|---|---|
| `mod.rs` | `finish_folder()` orchestration, `FinishReport`, idempotency check, failure isolation. Nothing else. |
| `measure.rs` | `RawStats` + `measure_raw()`. Raw-linear percentiles, clipping fractions, as-shot WB. |
| `illuminant.rs` | Cheng-2014 PCA illuminant estimate. Pure; returns `Option`. |
| `decide.rs` | `EditRecipe` + `decide()`. Pure function. The tuning-sensitive logic. |
| `pp3.rs` | `EditRecipe` → `.pp3` text. Pure; golden-file tested. |
| `render.rs` | `Pp3Renderer` — the `rawtherapee-cli` subprocess and its probe. |
| `lut.rs` | *(Phase 2)* `Lut33` type, `.cube` read/write, basis-LUT fuse. |
| `lut_apply.rs` | *(Phase 2)* Trilinear application of a `Lut33` to an image. |

**New — elsewhere:**

| File | Responsibility |
|---|---|
| `crates/pipeline/src/catalog/develop.rs` | `impl Catalog` block for `raw_stats` + `edits`. A child module of `catalog`, so it can reach `Catalog`'s private `conn`. Keeps the already-3537-line `catalog/mod.rs` from growing further. |
| `crates/pipeline/assets/base.pp3` | The version-controlled neutral baseline profile, embedded with `include_str!`. |
| `crates/pipeline/src/models/lut_predictor.rs` | *(Phase 2)* The ONNX predictor CNN + basis LUTs. |
| `tools/export_lut3d.py` | *(Phase 2)* One-time ONNX export. |
| `docs/design/pp3-keys.md` | *(Phase 0)* The verified `.pp3` key table. The authority Task 8 codes against. |

**Modified:**

| File | Change |
|---|---|
| `crates/pipeline/src/catalog/schema.rs` | Append migration version 4. |
| `crates/pipeline/src/catalog/mod.rs` | Add `mod develop;` and the `KeeperToDevelop` struct + `keepers_to_develop()`. |
| `crates/pipeline/src/config.rs` | Add `DevelopConfig` + `LookConfig`, wire into `Config`. |
| `crates/pipeline/src/lib.rs` | Add `pub mod develop;` and re-exports. |
| `crates/cli/src/main.rs` | Add the `Finish` subcommand, `cmd_finish`, and the RawTherapee doctor check. |
| `README.md` | Document `finish` in the pipeline diagram and quick start. |

## Phase Structure and the Checkpoint

- **Phase 0 (Tasks 1–2)** — environment and research. Installs `rawtherapee-cli`, lands the doctor check, and resolves the `.pp3` key names. Blocks everything else.
- **Phase 1 (Tasks 3–12)** — baseline develop. Produces finished JPEGs with technical corrections and no look.
- **CHECKPOINT** — mandatory. Phase 1 output is reviewed on real photos and signed off before Task 13 starts. Do not skip. See the CHECKPOINT section between Task 12 and Task 13.
- **Phase 2 (Tasks 13–18)** — the look. LUT model, application, and the IQA guard.

---

## Phase 0 — Environment and research

### Task 1: RawTherapee dependency and the `doctor` check

**Files:**
- Modify: `crates/pipeline/src/config.rs` (add `DevelopConfig`, wire into `Config`)
- Modify: `crates/cli/src/main.rs` (add `doctor_check_rawtherapee`, call it from `cmd_doctor`)
- Test: inline `#[cfg(test)] mod tests` in `crates/pipeline/src/config.rs`

**Interfaces:**
- Consumes: `Config` (`crates/pipeline/src/config.rs:9`), `DoctorCheck` (`crates/cli/src/main.rs:32`)
- Produces:
  - `pipeline::config::DevelopConfig { renderer: String, rawtherapee_path: String, finished_dir: String, jpeg_quality: u8, output_subdirs: OutputSubdirs, look: LookConfig }`
  - `pipeline::config::LookConfig { enable: bool, model: String, guard_iqa: bool, guard_margin: f32 }`
  - `pipeline::config::OutputSubdirs` enum — `Month` | `Flat`
  - `Config.develop: DevelopConfig`
  - `photopipe doctor` prints a `RawTherapee` line.

- [ ] **Step 1: Install `rawtherapee-cli` and verify it by hand**

This is manual and must happen before any code. On macOS:

```bash
brew install --cask rawtherapee
# The CLI lives inside the app bundle and is NOT on PATH by default:
/Applications/RawTherapee.app/Contents/MacOS/rawtherapee-cli --version
```

Record the exact path and version. If `brew` is unavailable, download from
https://rawtherapee.com/downloads and locate `rawtherapee-cli` in the bundle.

Now prove the invocation the renderer will use actually works, against one of
your own RAW files:

```bash
mkdir -p /tmp/rt-smoke
/Applications/RawTherapee.app/Contents/MacOS/rawtherapee-cli \
  -Y -t -b16 -o /tmp/rt-smoke -c ~/Photos/<some>.ARW
ls -la /tmp/rt-smoke
```

Expected: a `.tif` appears, tens to hundreds of MB, and exit code 0. If the
flags are rejected, run `rawtherapee-cli --help` and record the correct spelling —
every later task depends on this exact command line.

- [ ] **Step 2: Write the failing config test**

Add to the `mod tests` block at the bottom of `crates/pipeline/src/config.rs`:

```rust
    #[test]
    fn develop_defaults_and_override() {
        let cfg = Config::default();
        assert_eq!(cfg.develop.renderer, "rawtherapee");
        assert_eq!(cfg.develop.finished_dir, "<library>/_finished");
        assert_eq!(cfg.develop.jpeg_quality, 92);
        assert_eq!(cfg.develop.output_subdirs, OutputSubdirs::Month);
        assert!(cfg.develop.look.enable);
        assert!(cfg.develop.look.guard_iqa);

        let toml_str = r#"
            [develop]
            jpeg_quality = 85
            output_subdirs = "flat"

            [develop.look]
            enable = false
        "#;
        let parsed: Config = toml::from_str(toml_str).expect("parse");
        assert_eq!(parsed.develop.jpeg_quality, 85);
        assert_eq!(parsed.develop.output_subdirs, OutputSubdirs::Flat);
        assert!(!parsed.develop.look.enable);
        // untouched fields keep their defaults
        assert_eq!(parsed.develop.renderer, "rawtherapee");
        assert_eq!(parsed.develop.look.guard_margin, 0.02);
    }
```

- [ ] **Step 3: Run it to confirm it fails**

Run: `cargo test -p pipeline --lib config::tests::develop_defaults_and_override`
Expected: FAIL — compile error, `no field 'develop' on type 'Config'`.

- [ ] **Step 4: Add the config types**

In `crates/pipeline/src/config.rs`, add `develop` to `Config`:

```rust
pub struct Config {
    pub catalog: CatalogConfig,
    pub ingest: IngestConfig,
    pub models: ModelsConfig,
    pub defect: DefectConfig,
    pub dedupe: DedupeConfig,
    pub output: OutputConfig,
    pub develop: DevelopConfig,
}
```

Then add a new section after `// ── output ──`:

```rust
// ── develop ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DevelopConfig {
    /// Only "rawtherapee" is implemented in v1.
    pub renderer: String,
    /// Absolute path to `rawtherapee-cli`. Empty = search PATH.
    pub rawtherapee_path: String,
    /// Literal `<library>` is substituted with the scan root, matching
    /// `[output].review_tree`.
    pub finished_dir: String,
    pub jpeg_quality: u8,
    pub output_subdirs: OutputSubdirs,
    pub look: LookConfig,
}

impl Default for DevelopConfig {
    fn default() -> Self {
        Self {
            renderer: "rawtherapee".into(),
            rawtherapee_path: String::new(),
            finished_dir: "<library>/_finished".into(),
            jpeg_quality: 92,
            output_subdirs: OutputSubdirs::Month,
            look: LookConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OutputSubdirs {
    Month,
    Flat,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LookConfig {
    pub enable: bool,
    pub model: String,
    /// Fall back to baseline-only if the look lowers the IQA score.
    pub guard_iqa: bool,
    /// Allowed IQA drop before the look is rejected.
    pub guard_margin: f32,
}

impl Default for LookConfig {
    fn default() -> Self {
        Self {
            enable: true,
            model: "lut3d-fivek".into(),
            guard_iqa: true,
            guard_margin: 0.02,
        }
    }
}
```

- [ ] **Step 5: Run the test to confirm it passes**

Run: `cargo test -p pipeline --lib config::tests`
Expected: PASS, including the pre-existing `defaults_round_trip`.

- [ ] **Step 6: Add the doctor check**

In `crates/cli/src/main.rs`, add after `doctor_check_disk_free`:

```rust
/// Locate `rawtherapee-cli` and confirm it runs. Non-critical: `finish` is the
/// only command that needs it, so a missing binary must not fail `doctor` for a
/// user who only scans and reviews.
fn doctor_check_rawtherapee(cfg: &config::DevelopConfig) -> DoctorCheck {
    let exe = resolve_rawtherapee(cfg);
    let out = std::process::Command::new(&exe).arg("--version").output();
    match out {
        Ok(o) if o.status.success() => {
            let text = String::from_utf8_lossy(&o.stdout);
            let version = text.lines().next().unwrap_or("unknown").trim().to_string();
            DoctorCheck::ok("RawTherapee", format!("{version} ({})", exe.display()))
        }
        Ok(o) => DoctorCheck::warn(
            "RawTherapee",
            format!("{} exited {} — `photopipe finish` will not work", exe.display(), o.status),
        ),
        Err(e) => DoctorCheck::warn(
            "RawTherapee",
            format!(
                "not found ({e}) — install it and set [develop] rawtherapee_path, \
                 or `photopipe finish` will not work"
            ),
        ),
    }
}

/// The configured path if set, otherwise the bare name so the OS searches PATH.
fn resolve_rawtherapee(cfg: &config::DevelopConfig) -> PathBuf {
    if cfg.rawtherapee_path.is_empty() {
        PathBuf::from("rawtherapee-cli")
    } else {
        config::expand_tilde(std::path::Path::new(&cfg.rawtherapee_path))
    }
}
```

In `cmd_doctor`, add the check next to the existing two:

```rust
    checks.push(doctor_check_cache_writable(&roots.cache));
    checks.push(doctor_check_disk_free(&roots.data));
    checks.push(doctor_check_rawtherapee(&cfg.develop));
```

- [ ] **Step 7: Run doctor and confirm the line appears**

```bash
cargo build --release
./target/release/photopipe doctor
```

Expected: a `[ ok ] RawTherapee` line showing the version, once
`[develop] rawtherapee_path` points at the binary you verified in Step 1.
Write that path into your config file now:

```toml
[develop]
rawtherapee_path = "/Applications/RawTherapee.app/Contents/MacOS/rawtherapee-cli"
```

- [ ] **Step 8: Verify green and commit**

```bash
cargo fmt
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all
git add crates/pipeline/src/config.rs crates/cli/src/main.rs
git commit -m "feat(develop): [develop] config section and the RawTherapee doctor check"
```

---

### Task 2: Verify the `.pp3` key names and author `base.pp3`

This task is **research plus two committed artifacts**. It resolves spec open
item 1, the single largest unknown in the design: RawPedia does not document the
`.pp3` key names, so they must be established empirically before Task 8 can emit
them. Getting this wrong is silent — RawTherapee ignores unknown keys without
warning, so a typo produces a plausible-looking render with a setting that never
applied.

**Files:**
- Create: `docs/design/pp3-keys.md`
- Create: `crates/pipeline/assets/base.pp3`

**Interfaces:**
- Consumes: a working `rawtherapee-cli` and the RawTherapee GUI from Task 1.
- Produces: `docs/design/pp3-keys.md` — the authoritative key table Task 8 codes
  against; `crates/pipeline/assets/base.pp3` — embedded by Task 8 via
  `include_str!`.

- [ ] **Step 1: Capture a GUI-default reference profile**

Open one of your RAW files in the RawTherapee **GUI**. Without touching any
control, save the processing profile: `Save current profile` → `/tmp/pp3/00-default.pp3`.

This is the reference every later diff is taken against.

- [ ] **Step 2: Diff one tool at a time**

For each recipe field below, change **only** that control in the GUI, save to its
own file, and diff against the reference. Working one tool at a time is what makes
the diff unambiguous — changing two controls leaves you guessing which keys belong
to which.

```bash
mkdir -p /tmp/pp3
# after saving each variant:
diff /tmp/pp3/00-default.pp3 /tmp/pp3/01-exposure.pp3
```

Capture one variant per row:

| Variant file | GUI control to change |
|---|---|
| `01-exposure.pp3` | Exposure → Exposure compensation, set to `+1.0` |
| `02-highlights.pp3` | Exposure → Highlight reconstruction, enable, method `Blend` |
| `03-highlights-color.pp3` | same, method `Color propagation` |
| `04-shadows.pp3` | Shadows/Highlights, enable, Shadows to `40` |
| `05-wb.pp3` | White Balance → Custom, Temperature `5500`, Tint/Green `1.05` |
| `06-denoise.pp3` | Noise Reduction, enable, Luminance `20`, Chrominance `30` |
| `07-sharpen.pp3` | Capture Sharpening, enable, and note its radius/iteration keys |
| `08-lens.pp3` | Lens/Geometry → Profiled Lens Correction, `Auto-matched` |

- [ ] **Step 3: Write the key table**

Create `docs/design/pp3-keys.md` with the **verified** keys. The table below is a
starting hypothesis from the RawTherapee 5.x profile format — **replace each cell
with what your diff actually showed**, and mark any that differ:

```markdown
# RawTherapee `.pp3` keys used by `photopipe finish`

Verified against RawTherapee <VERSION> on <DATE> by the GUI-diff method
(spec open item 1). Section headers are literal INI sections; keys are
case-sensitive and RawTherapee silently ignores anything it does not recognise.

| Recipe field | Section | Key | Type / range | Verified |
|---|---|---|---|---|
| `exposure_ev` | `[Exposure]` | `Compensation` | float, EV | ☐ |
| — | `[Exposure]` | `Auto` | bool — must be `false` | ☐ |
| `highlight_recovery` (on/off) | `[HLRecovery]` | `Enabled` | bool | ☐ |
| `highlight_recovery` (method) | `[HLRecovery]` | `Method` | `Blend` \| `Color` \| `Coloropp` | ☐ |
| `shadow_lift` | `[Shadows & Highlights]` | `Enabled`, `Shadows` | bool, 0–100 | ☐ |
| white balance | `[White Balance]` | `Setting`=`Camera` | — (uses the camera's own coefficients) | ☐ |
| `denoise_luma` | `[Directional Pyramid Denoising]` | `Enabled`, `Luma` | bool, 0–100 | ☐ |
| `denoise_chroma` | same | `Chroma` | 0–100 | ☐ |
| `sharpen_amount` | `[PostDemosaicSharpening]` | `Enabled`, `Contrast`, `DeconvRadius` | bool, 0–100, float | ☐ |
| `lens_correct` | `[LensProfile]` | `LcMode`=`lfauto`, `UseDistortion`, `UseVignette` | — | ☐ |

## Ranges and units

Record here anything the diff revealed that the recipe's own 0..1 scale must map
onto — e.g. if `Chroma` runs 0–100, `denoise_chroma = 0.3` emits `Chroma=30`.

## Keys deliberately NOT written

`base.pp3` owns these; the per-photo profile must never set them, or it would
override the baseline: film simulation, tone curves, saturation, and anything
under `[Film Simulation]`.
```

Tick each ☐ only after you have seen the key in a real diff.

- [ ] **Step 4: Author `base.pp3`**

Create `crates/pipeline/assets/base.pp3`. Start from `/tmp/pp3/00-default.pp3`,
then **strip everything that is not a technical correction**. Per spec §4, the
baseline must be as close to a default raw conversion as possible, because the
Phase 2 look model's input distribution depends on it.

Remove or neutralise:
- any `[Film Simulation]` section (`Enabled=false`)
- any non-linear tone curve (`[ToneCurve] Curve=` should be the identity)
- saturation, vibrance, and `[ColorToning]`

Keep enabled: demosaic, white-level/black-level handling, and the colour
management block that produces **sRGB** output — the look model is trained in
sRGB, so the output profile must be sRGB, not AdobeRGB or ProPhoto.

Add a header comment RawTherapee will ignore:

```
# photopipe base.pp3 — version-controlled neutral baseline.
# Applied with `-p base.pp3 -p <photo>.pp3`, in that order, so per-photo keys win.
# NEVER use rawtherapee-cli -d: that reads the user's GUI default profile and
# makes output depend on invisible machine-local state (spec §4).
```

- [ ] **Step 5: Prove the two-profile stack works**

```bash
/Applications/RawTherapee.app/Contents/MacOS/rawtherapee-cli \
  -Y -t -b16 \
  -p crates/pipeline/assets/base.pp3 \
  -p /tmp/pp3/01-exposure.pp3 \
  -o /tmp/rt-smoke -c ~/Photos/<some>.ARW
```

Expected: exit 0, and the resulting TIFF is visibly one stop brighter than the
Step-1 smoke render. That brightness difference is the proof that the second `-p`
actually stacked on the first — if the images match, the stacking order or a key
name is wrong and Task 8 will silently produce no-op profiles.

- [ ] **Step 6: Commit**

```bash
git add docs/design/pp3-keys.md crates/pipeline/assets/base.pp3
git commit -m "docs(develop): verified .pp3 key table and the neutral base profile"
```

---

## Phase 1 — Baseline develop

### Task 3: Schema migration version 4

**Files:**
- Modify: `crates/pipeline/src/catalog/schema.rs`
- Test: `crates/pipeline/tests/develop.rs` (create)

**Interfaces:**
- Consumes: `MIGRATIONS` (`crates/pipeline/src/catalog/schema.rs:1`), `Catalog::open`, `Catalog::schema_version`
- Produces: tables `raw_stats` and `edits`; `Catalog::schema_version()` returns `4`.

- [ ] **Step 1: Write the failing test**

Create `crates/pipeline/tests/develop.rs`:

```rust
use pipeline::catalog::Catalog;

fn temp_catalog() -> (tempfile::TempDir, Catalog) {
    let dir = tempfile::TempDir::new().unwrap();
    let cat = Catalog::open(&dir.path().join("catalog.duckdb")).unwrap();
    (dir, cat)
}

#[test]
fn migration_v4_creates_develop_tables() {
    let (_dir, cat) = temp_catalog();
    assert_eq!(cat.schema_version().unwrap(), 4);
}
```

- [ ] **Step 2: Run it to confirm it fails**

Run: `cargo test -p pipeline --test develop migration_v4_creates_develop_tables`
Expected: FAIL — `assertion `left == right` failed: left: 3, right: 4`.

- [ ] **Step 3: Append the migration**

In `crates/pipeline/src/catalog/schema.rs`, append to the `MIGRATIONS` slice after
the version-3 entry. Copy the DDL verbatim from spec §5 — the column set is part
of the approved design, not a suggestion:

```rust
    // version 4 — automatic develop: raw-linear stats and edit recipes
    "BEGIN TRANSACTION;
     INSERT INTO schema_version VALUES (4);
     CREATE TABLE raw_stats (
         file_id           BIGINT PRIMARY KEY REFERENCES files(id),
         p1                REAL NOT NULL,
         p50               REAL NOT NULL,
         p99               REAL NOT NULL,
         p999              REAL NOT NULL,
         clipped_frac      REAL NOT NULL,
         black_frac        REAL NOT NULL,
         wb_r              REAL NOT NULL,
         wb_g              REAL NOT NULL,
         wb_b              REAL NOT NULL,
         illum_r           REAL,
         illum_g           REAL,
         illum_b           REAL
     );
     CREATE TABLE edits (
         file_id            BIGINT PRIMARY KEY REFERENCES files(id),
         content_hash       VARCHAR NOT NULL,
         exposure_ev        REAL NOT NULL,
         highlight_recovery REAL NOT NULL,
         shadow_lift        REAL NOT NULL,
         denoise_luma       REAL NOT NULL,
         denoise_chroma     REAL NOT NULL,
         sharpen_amount     REAL NOT NULL,
         lens_correct       BOOLEAN NOT NULL,
         recipe_hash        VARCHAR NOT NULL,
         decider_version    VARCHAR NOT NULL,
         renderer           VARCHAR NOT NULL,
         look_model         VARCHAR,
         look_version       VARCHAR,
         lut_hash           VARCHAR,
         look_applied       BOOLEAN NOT NULL,
         iqa_before         REAL,
         iqa_after          REAL,
         output_path        VARCHAR,
         output_size_bytes  BIGINT,
         rendered_at        BIGINT
     );
     CREATE INDEX idx_edits_hash ON edits(content_hash);
     COMMIT;",
```

- [ ] **Step 4: Add a test that the tables are actually usable**

Append to `crates/pipeline/tests/develop.rs`:

```rust
#[test]
fn migration_v4_tables_accept_rows() {
    let (_dir, cat) = temp_catalog();
    // A raw_stats row needs a real files row to satisfy the FK.
    let conn = cat.raw_conn_for_test();
    conn.execute_batch(
        "INSERT INTO files (path, content_hash, size_bytes, mtime_ns, file_format, last_processed)
         VALUES ('/tmp/a.arw', 'deadbeef', 100, 0, 'arw', 0);
         INSERT INTO raw_stats VALUES
             ((SELECT id FROM files WHERE path='/tmp/a.arw'),
              0.01, 0.18, 0.95, 0.001, 0.002, 2.0, 1.0, 1.5, NULL, NULL, NULL);",
    )
    .unwrap();
}
```

This needs a test-only accessor. Add to `crates/pipeline/src/catalog/mod.rs` in the
`impl Catalog` block, next to the existing `simulate_flush_error` test helper:

```rust
    /// Test-only raw connection access, for asserting schema shape directly.
    #[doc(hidden)]
    pub fn raw_conn_for_test(&self) -> std::sync::MutexGuard<'_, duckdb::Connection> {
        self.conn.lock().expect("mutex poisoned")
    }
```

- [ ] **Step 5: Run both tests**

Run: `cargo test -p pipeline --test develop`
Expected: PASS, 2 tests.

- [ ] **Step 6: Confirm no existing test regressed**

Run: `cargo test --all`
Expected: PASS. Any test asserting `schema_version() == 3` must be updated to `4`;
search for it with `grep -rn "schema_version" crates/*/tests crates/*/src`.

- [ ] **Step 7: Verify green and commit**

```bash
cargo fmt && cargo clippy --all-targets --all-features -- -D warnings && cargo test --all
git add crates/pipeline/src/catalog/schema.rs crates/pipeline/src/catalog/mod.rs crates/pipeline/tests/develop.rs
git commit -m "feat(catalog): schema v4 — raw_stats and edits tables"
```

---

### Task 4: Measure raw-linear statistics

**Files:**
- Create: `crates/pipeline/src/develop/mod.rs`
- Create: `crates/pipeline/src/develop/measure.rs`
- Modify: `crates/pipeline/src/lib.rs`
- Test: inline `#[cfg(test)] mod tests` in `measure.rs`

**Interfaces:**
- Consumes: `rawler::get_decoder`, `rawler::rawsource::RawSource`, `rawler::decoders::RawDecodeParams`, `rawler::rawimage::{RawImage, RawImageData}`
- Produces:
  - `pipeline::develop::measure::RawStats { p1: f32, p50: f32, p99: f32, p999: f32, clipped_frac: f32, black_frac: f32, wb_r: f32, wb_g: f32, wb_b: f32, illum_r: Option<f32>, illum_g: Option<f32>, illum_b: Option<f32> }`
  - `pipeline::develop::measure::measure_raw(path: &Path) -> Result<RawStats, DevelopError>`
  - `pipeline::develop::measure::stats_from_samples(samples: &[f32], black: f32, white: f32) -> RawStats` — the pure inner function, so percentile logic is testable without a RAW file
  - `pipeline::develop::DevelopError` (thiserror)

- [ ] **Step 1: Write the failing test for the pure statistics function**

Create `crates/pipeline/src/develop/measure.rs` with only a test module for now:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// A ramp from black to white: percentiles land at known positions and
    /// nothing is clipped except the single top sample.
    #[test]
    fn ramp_percentiles_are_positionally_correct() {
        let samples: Vec<f32> = (0..=1000).map(|i| i as f32).collect();
        let s = stats_from_samples(&samples, 0.0, 1000.0);
        assert!((s.p50 - 0.5).abs() < 0.01, "p50 was {}", s.p50);
        assert!((s.p1 - 0.01).abs() < 0.01, "p1 was {}", s.p1);
        assert!((s.p999 - 0.999).abs() < 0.01, "p999 was {}", s.p999);
    }

    /// Black subtraction and white normalisation: a sensor with black=512,
    /// white=16383 must map its own black to 0.0 and its own white to 1.0.
    #[test]
    fn black_is_subtracted_and_white_normalised() {
        let samples = vec![512.0, 512.0, 8447.5, 16383.0, 16383.0];
        let s = stats_from_samples(&samples, 512.0, 16383.0);
        assert!((s.p50 - 0.5).abs() < 0.01, "p50 was {}", s.p50);
        // two of five samples sit at white, two at black
        assert!((s.clipped_frac - 0.4).abs() < 0.001, "clipped {}", s.clipped_frac);
        assert!((s.black_frac - 0.4).abs() < 0.001, "black {}", s.black_frac);
    }

    /// Values below black or above white must clamp, never produce negatives
    /// or >1 — the decide() formulas take log2 of these and would produce NaN.
    #[test]
    fn out_of_range_samples_clamp_to_unit_interval() {
        let samples = vec![0.0, 100.0, 20000.0];
        let s = stats_from_samples(&samples, 512.0, 16383.0);
        for v in [s.p1, s.p50, s.p999] {
            assert!((0.0..=1.0).contains(&v), "value {v} escaped the unit interval");
        }
    }

    /// An empty sample set must not panic or divide by zero.
    #[test]
    fn empty_samples_yield_neutral_stats() {
        let s = stats_from_samples(&[], 0.0, 16383.0);
        assert_eq!(s.clipped_frac, 0.0);
        assert_eq!(s.black_frac, 0.0);
        assert_eq!(s.p50, 0.0);
    }
}
```

- [ ] **Step 2: Run it to confirm it fails**

Run: `cargo test -p pipeline --lib develop::measure`
Expected: FAIL — the module is not declared yet, so nothing compiles.

- [ ] **Step 3: Create the module skeleton and error type**

Create `crates/pipeline/src/develop/mod.rs`:

```rust
//! Automatic RAW development: measure → decide → render → look.
//!
//! Stage boundaries are deliberate. `measure` touches pixels but makes no
//! decisions; `decide` makes every decision but never touches a pixel; `render`
//! and `pp3` translate a decision into RawTherapee's vocabulary. Keeping those
//! separate is what makes the tuning logic testable over plain numbers.

pub mod measure;

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
```

Add to `crates/pipeline/src/lib.rs`, keeping the module list alphabetical:

```rust
pub mod develop;
```

- [ ] **Step 4: Implement the pure statistics function**

Prepend to `crates/pipeline/src/develop/measure.rs`:

```rust
//! Raw-linear sensor statistics. Distinct from the `exposure` table, which is
//! derived from the 8-bit preview: a preview reports 255 for anything the
//! camera's tone curve pushed to white, whereas the raw reveals whether the
//! photosite actually saturated. Highlight reconstruction needs the latter.

use std::path::Path;

use crate::develop::DevelopError;

/// Raw-linear statistics for one file. Percentiles are black-subtracted and
/// white-normalised into 0..1.
#[derive(Debug, Clone, PartialEq)]
pub struct RawStats {
    pub p1: f32,
    pub p50: f32,
    pub p99: f32,
    pub p999: f32,
    /// Fraction of samples at or above the white level.
    pub clipped_frac: f32,
    /// Fraction of samples at or below the black level.
    pub black_frac: f32,
    /// As-shot white balance coefficients as encoded in the file (unnormalised).
    pub wb_r: f32,
    pub wb_g: f32,
    pub wb_b: f32,
    /// PCA illuminant estimate; `None` when estimation fails.
    pub illum_r: Option<f32>,
    pub illum_g: Option<f32>,
    pub illum_b: Option<f32>,
}

/// Percentiles and clipping fractions from raw sample values.
///
/// Pure: no I/O, no decode. `black` and `white` are the sensor's own levels, so
/// the returned percentiles are comparable across cameras.
pub fn stats_from_samples(samples: &[f32], black: f32, white: f32) -> RawStats {
    if samples.is_empty() {
        return RawStats {
            p1: 0.0,
            p50: 0.0,
            p99: 0.0,
            p999: 0.0,
            clipped_frac: 0.0,
            black_frac: 0.0,
            wb_r: 1.0,
            wb_g: 1.0,
            wb_b: 1.0,
            illum_r: None,
            illum_g: None,
            illum_b: None,
        };
    }

    let n = samples.len();
    let clipped = samples.iter().filter(|v| **v >= white).count();
    let blacked = samples.iter().filter(|v| **v <= black).count();

    // Normalise before sorting so the percentile read-out is already in 0..1.
    let range = (white - black).max(1.0);
    let mut norm: Vec<f32> = samples
        .iter()
        .map(|v| ((v - black) / range).clamp(0.0, 1.0))
        .collect();
    norm.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    RawStats {
        p1: percentile(&norm, 0.01),
        p50: percentile(&norm, 0.50),
        p99: percentile(&norm, 0.99),
        p999: percentile(&norm, 0.999),
        clipped_frac: clipped as f32 / n as f32,
        black_frac: blacked as f32 / n as f32,
        wb_r: 1.0,
        wb_g: 1.0,
        wb_b: 1.0,
        illum_r: None,
        illum_g: None,
        illum_b: None,
    }
}

/// Nearest-rank percentile over an already-sorted slice.
fn percentile(sorted: &[f32], q: f32) -> f32 {
    if sorted.is_empty() {
        return 0.0;
    }
    let idx = ((sorted.len() - 1) as f32 * q).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}
```

- [ ] **Step 5: Run the tests to confirm they pass**

Run: `cargo test -p pipeline --lib develop::measure`
Expected: PASS, 4 tests.

- [ ] **Step 6: Add the RAW-reading wrapper**

Append to `measure.rs`. Note the subsampling — a 60MP sensor is 60 million
samples, and percentiles do not need every one of them:

```rust
/// Sample at most this many photosites. Percentile estimates converge long
/// before a full 60MP read, and the stride keeps the walk cache-friendly.
const MAX_SAMPLES: usize = 2_000_000;

/// Decode `path` and compute raw-linear statistics.
///
/// Reads the raw sensor plane, not the embedded preview. Restricted to the
/// active area when the decoder reports one, so masked black borders never
/// enter the percentiles.
pub fn measure_raw(path: &Path) -> Result<RawStats, DevelopError> {
    use rawler::{decoders::RawDecodeParams, rawsource::RawSource};

    let src = RawSource::new(path).map_err(|e| DevelopError::Decode {
        path: path.to_path_buf(),
        reason: e.to_string(),
    })?;
    let decoder = rawler::get_decoder(&src).map_err(|e| DevelopError::Decode {
        path: path.to_path_buf(),
        reason: e.to_string(),
    })?;
    let raw = decoder
        .raw_image(&src, &RawDecodeParams::default(), false)
        .map_err(|e| DevelopError::Decode {
            path: path.to_path_buf(),
            reason: e.to_string(),
        })?;

    let data = raw.data.as_f32();
    let white = raw
        .whitelevel
        .0
        .first()
        .copied()
        .unwrap_or(u16::MAX as u32) as f32;
    let black = raw
        .blacklevel
        .levels
        .first()
        .map(|r| r.as_f32())
        .unwrap_or(0.0);

    let stride = (data.len() / MAX_SAMPLES).max(1);
    let samples: Vec<f32> = data.iter().step_by(stride).copied().collect();

    let mut stats = stats_from_samples(&samples, black, white);
    // rawler stores coefficients in RGBE order.
    stats.wb_r = raw.wb_coeffs[0];
    stats.wb_g = raw.wb_coeffs[1];
    stats.wb_b = raw.wb_coeffs[2];
    if !stats.wb_r.is_finite() || !stats.wb_g.is_finite() || !stats.wb_b.is_finite() {
        tracing::warn!(path = %path.display(), "non-finite wb_coeffs; falling back to neutral");
        stats.wb_r = 1.0;
        stats.wb_g = 1.0;
        stats.wb_b = 1.0;
    }
    Ok(stats)
}
```

- [ ] **Step 7: Confirm it compiles and the whole suite is green**

Run: `cargo test --all`
Expected: PASS. `measure_raw` has no unit test — it needs a real RAW file, and
per CLAUDE.md we do not fabricate fixtures. It is exercised by the end-to-end
test in Task 12.

- [ ] **Step 8: Verify green and commit**

```bash
cargo fmt && cargo clippy --all-targets --all-features -- -D warnings && cargo test --all
git add crates/pipeline/src/develop crates/pipeline/src/lib.rs
git commit -m "feat(develop): raw-linear statistics from the sensor plane"
```

---

### Task 5: Catalog persistence for `raw_stats` and the develop work list

**Files:**
- Create: `crates/pipeline/src/catalog/develop.rs`
- Modify: `crates/pipeline/src/catalog/mod.rs` (add `mod develop;`, the two row structs)
- Test: `crates/pipeline/tests/develop.rs`

`catalog/mod.rs` is already 3537 lines. New develop queries go in a child module
rather than growing it further. Because `catalog::develop` is a descendant of
`catalog`, it can reach `Catalog`'s private `conn` field without any visibility
change.

**Interfaces:**
- Consumes: `Catalog` (`crates/pipeline/src/catalog/mod.rs:291`), `CatalogError` (`crates/pipeline/src/error.rs`), `RawStats` (Task 4)
- Produces:
  - `pipeline::catalog::KeeperToDevelop { file_id: i64, path: PathBuf, content_hash: String, year_month: String }`
  - `Catalog::keepers_to_develop(&self) -> Result<Vec<KeeperToDevelop>, CatalogError>`
  - `Catalog::upsert_raw_stats(&self, file_id: i64, s: &RawStats) -> Result<(), CatalogError>`
  - `Catalog::get_raw_stats(&self, file_id: i64) -> Result<Option<RawStats>, CatalogError>`

- [ ] **Step 1: Write the failing tests**

Append to `crates/pipeline/tests/develop.rs`:

```rust
use pipeline::develop::measure::RawStats;

fn sample_stats() -> RawStats {
    RawStats {
        p1: 0.01,
        p50: 0.18,
        p99: 0.90,
        p999: 0.95,
        clipped_frac: 0.002,
        black_frac: 0.004,
        wb_r: 2.1,
        wb_g: 1.0,
        wb_b: 1.6,
        illum_r: Some(0.4),
        illum_g: Some(0.5),
        illum_b: Some(0.3),
    }
}

/// Seed one file with a decision and return its id.
fn seed_file(cat: &Catalog, path: &str, verdict: &str) -> i64 {
    let conn = cat.raw_conn_for_test();
    conn.execute(
        "INSERT INTO files (path, content_hash, size_bytes, mtime_ns, file_format, last_processed)
         VALUES (?, 'hash-a', 100, 0, 'arw', 0)",
        duckdb::params![path],
    )
    .unwrap();
    let id: i64 = conn
        .query_row("SELECT id FROM files WHERE path = ?", duckdb::params![path], |r| r.get(0))
        .unwrap();
    conn.execute(
        "INSERT INTO decisions (file_id, verdict, is_keeper, note, decided_at)
         VALUES (?, ?, false, NULL, 0)",
        duckdb::params![id, verdict],
    )
    .unwrap();
    id
}

#[test]
fn raw_stats_round_trip() {
    let (_dir, cat) = temp_catalog();
    let id = seed_file(&cat, "/tmp/a.arw", "keep");
    cat.upsert_raw_stats(id, &sample_stats()).unwrap();
    let got = cat.get_raw_stats(id).unwrap().expect("row should exist");
    assert_eq!(got, sample_stats());
}

/// Upsert must overwrite, not duplicate — file_id is the primary key and a
/// re-measure after a config change has to replace the old row.
#[test]
fn raw_stats_upsert_overwrites() {
    let (_dir, cat) = temp_catalog();
    let id = seed_file(&cat, "/tmp/a.arw", "keep");
    cat.upsert_raw_stats(id, &sample_stats()).unwrap();
    let mut second = sample_stats();
    second.p50 = 0.42;
    cat.upsert_raw_stats(id, &second).unwrap();
    assert_eq!(cat.get_raw_stats(id).unwrap().unwrap().p50, 0.42);
}

/// NULL illuminant columns must survive the round trip as None, not 0.0 —
/// decide() branches on whether the estimate exists at all.
#[test]
fn raw_stats_null_illuminant_round_trips_as_none() {
    let (_dir, cat) = temp_catalog();
    let id = seed_file(&cat, "/tmp/a.arw", "keep");
    let mut s = sample_stats();
    s.illum_r = None;
    s.illum_g = None;
    s.illum_b = None;
    cat.upsert_raw_stats(id, &s).unwrap();
    let got = cat.get_raw_stats(id).unwrap().unwrap();
    assert_eq!(got.illum_r, None);
}

/// The work list is `verdict = 'keep'` (spec A8), NOT is_keeper. A plain keep
/// writes is_keeper = false, so keying on it would skip every photo outside a
/// duplicate group.
#[test]
fn work_list_selects_keeps_not_just_group_keepers() {
    let (_dir, cat) = temp_catalog();
    seed_file(&cat, "/tmp/keep.arw", "keep");
    seed_file(&cat, "/tmp/reject.arw", "reject");
    let work = cat.keepers_to_develop().unwrap();
    assert_eq!(work.len(), 1, "only the kept file belongs in the work list");
    assert_eq!(work[0].path, std::path::PathBuf::from("/tmp/keep.arw"));
    assert_eq!(work[0].content_hash, "hash-a");
    // No exif row was seeded, so the month falls back.
    assert_eq!(work[0].year_month, "unknown-date");
}
```

- [ ] **Step 2: Run them to confirm they fail**

Run: `cargo test -p pipeline --test develop`
Expected: FAIL — `no method named 'upsert_raw_stats' found for struct 'Catalog'`.

- [ ] **Step 3: Implement the catalog module**

Create `crates/pipeline/src/catalog/develop.rs`:

```rust
//! Catalog access for the develop stage: `raw_stats` persistence and the
//! keeper work list. Split out of `catalog/mod.rs` to keep that file from
//! growing further; as a child module it still reaches `Catalog`'s private
//! connection.

use std::path::PathBuf;

use super::{optional_row, Catalog};
use crate::develop::measure::RawStats;
use crate::error::CatalogError;

/// One photo the user kept, with everything `finish` needs to place its output.
#[derive(Debug, Clone)]
pub struct KeeperToDevelop {
    pub file_id: i64,
    pub path: PathBuf,
    pub content_hash: String,
    /// `YYYY-MM` from `captured_at`, or `"unknown-date"`.
    pub year_month: String,
}

impl Catalog {
    /// Every photo with `verdict = 'keep'`.
    ///
    /// Deliberately NOT `is_keeper`: that column is written only by
    /// `pick_keeper()` and means "best shot of a duplicate group", so a photo
    /// with no duplicates would never be developed (spec A8).
    pub fn keepers_to_develop(&self) -> Result<Vec<KeeperToDevelop>, CatalogError> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| CatalogError::Db("mutex poisoned".into()))?;
        let mut stmt = conn
            .prepare(
                "SELECT f.id, f.path, f.content_hash,
                        COALESCE(
                            strftime(CAST(to_timestamp(e.captured_at) AS TIMESTAMP), '%Y-%m'),
                            'unknown-date') AS ym
                 FROM decisions dec
                 JOIN files f ON f.id = dec.file_id
                 LEFT JOIN exif e ON e.file_id = f.id
                 WHERE dec.verdict = 'keep'
                 ORDER BY f.path",
            )
            .map_err(|e| CatalogError::Db(e.to_string()))?;
        let rows = stmt
            .query_map([], |r| {
                Ok(KeeperToDevelop {
                    file_id: r.get(0)?,
                    path: PathBuf::from(r.get::<_, String>(1)?),
                    content_hash: r.get(2)?,
                    year_month: r.get(3)?,
                })
            })
            .map_err(|e| CatalogError::Db(e.to_string()))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| CatalogError::Db(e.to_string()))?);
        }
        Ok(out)
    }

    /// Insert or replace the raw-linear statistics for one file.
    pub fn upsert_raw_stats(&self, file_id: i64, s: &RawStats) -> Result<(), CatalogError> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| CatalogError::Db("mutex poisoned".into()))?;
        conn.execute(
            "INSERT INTO raw_stats
                (file_id, p1, p50, p99, p999, clipped_frac, black_frac,
                 wb_r, wb_g, wb_b, illum_r, illum_g, illum_b)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT (file_id) DO UPDATE SET
                 p1 = excluded.p1, p50 = excluded.p50, p99 = excluded.p99,
                 p999 = excluded.p999,
                 clipped_frac = excluded.clipped_frac, black_frac = excluded.black_frac,
                 wb_r = excluded.wb_r, wb_g = excluded.wb_g, wb_b = excluded.wb_b,
                 illum_r = excluded.illum_r, illum_g = excluded.illum_g,
                 illum_b = excluded.illum_b",
            duckdb::params![
                file_id, s.p1, s.p50, s.p99, s.p999, s.clipped_frac, s.black_frac,
                s.wb_r, s.wb_g, s.wb_b, s.illum_r, s.illum_g, s.illum_b
            ],
        )
        .map_err(|e| CatalogError::Db(e.to_string()))?;
        Ok(())
    }

    pub fn get_raw_stats(&self, file_id: i64) -> Result<Option<RawStats>, CatalogError> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| CatalogError::Db("mutex poisoned".into()))?;
        let row = conn.query_row(
            "SELECT p1, p50, p99, p999, clipped_frac, black_frac, wb_r, wb_g, wb_b,
                    illum_r, illum_g, illum_b
             FROM raw_stats WHERE file_id = ?",
            duckdb::params![file_id],
            |r| {
                Ok(RawStats {
                    p1: r.get(0)?,
                    p50: r.get(1)?,
                    p99: r.get(2)?,
                    p999: r.get(3)?,
                    clipped_frac: r.get(4)?,
                    black_frac: r.get(5)?,
                    wb_r: r.get(6)?,
                    wb_g: r.get(7)?,
                    wb_b: r.get(8)?,
                    illum_r: r.get(9)?,
                    illum_g: r.get(10)?,
                    illum_b: r.get(11)?,
                })
            },
        );
        optional_row(row)
    }
}
```

- [ ] **Step 4: Declare the module and export the type**

In `crates/pipeline/src/catalog/mod.rs`, add near the other `mod` declarations at
the top of the file:

```rust
mod develop;

pub use develop::KeeperToDevelop;
```

Check that `optional_row` is reachable from the child module. If it is declared as
a private free function in `catalog/mod.rs`, no change is needed — a child module
can call it. If `cargo build` reports it as unresolved, adjust the `use super::`
line to match its real location.

- [ ] **Step 5: Run the tests**

Run: `cargo test -p pipeline --test develop`
Expected: PASS, 6 tests.

- [ ] **Step 6: Verify green and commit**

```bash
cargo fmt && cargo clippy --all-targets --all-features -- -D warnings && cargo test --all
git add crates/pipeline/src/catalog crates/pipeline/tests/develop.rs
git commit -m "feat(catalog): raw_stats persistence and the keeper work list"
```

---

### Task 6: The decision layer

This is the tuning-sensitive core, and it is a pure function precisely so it can
be exercised over plain numbers without a single fixture photo. Every formula
below is copied from spec §6.

**Files:**
- Create: `crates/pipeline/src/develop/decide.rs`
- Modify: `crates/pipeline/src/develop/mod.rs` (declare the module)
- Test: inline `#[cfg(test)] mod tests` in `decide.rs`

**Interfaces:**
- Consumes: `RawStats` (Task 4), `ExifData` (`crates/pipeline/src/ingest/exif.rs:4`)
- Produces:
  - `pipeline::develop::decide::EditRecipe { exposure_ev: f32, highlight_recovery: f32, shadow_lift: f32, denoise_luma: f32, denoise_chroma: f32, sharpen_amount: f32, lens_correct: bool }`
  - `pipeline::develop::decide::Sharpness { s_global: f32 }`
  - `pipeline::develop::decide::decide(raw: &RawStats, exif: &ExifData, sharp: &Sharpness) -> EditRecipe`
  - `pipeline::develop::decide::DECIDER_VERSION: &str`
  - `EditRecipe::recipe_hash(&self) -> String`

- [ ] **Step 1: Write the failing tests**

Create `crates/pipeline/src/develop/decide.rs` with only a test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn neutral_stats() -> RawStats {
        RawStats {
            p1: 0.01,
            p50: 0.18,
            p99: 0.90,
            p999: 0.95,
            clipped_frac: 0.0,
            black_frac: 0.0,
            wb_r: 2.0,
            wb_g: 1.0,
            wb_b: 1.5,
            illum_r: None,
            illum_g: None,
            illum_b: None,
        }
    }

    fn exif_at_iso(iso: u32) -> ExifData {
        ExifData {
            iso: Some(iso),
            lens_model: Some("FE 24-70mm F2.8 GM".into()),
            ..Default::default()
        }
    }

    fn sharp(s: f32) -> Sharpness {
        Sharpness { s_global: s }
    }

    /// A correctly exposed frame needs no correction: p50 already sits at
    /// middle grey and p99 leaves headroom.
    #[test]
    fn correct_exposure_is_left_alone() {
        let r = decide(&neutral_stats(), &exif_at_iso(100), &sharp(0.5));
        assert!(r.exposure_ev.abs() < 0.05, "ev was {}", r.exposure_ev);
    }

    /// An underexposed frame is lifted — but never past clipping. p50 at 0.045
    /// wants +2 EV; p99 at 0.5 only allows +0.93.
    #[test]
    fn lift_is_bounded_by_available_headroom() {
        let mut s = neutral_stats();
        s.p50 = 0.045;
        s.p99 = 0.5;
        let r = decide(&s, &exif_at_iso(100), &sharp(0.5));
        let headroom = (0.95f32 / 0.5).log2();
        assert!(
            (r.exposure_ev - headroom).abs() < 0.01,
            "expected the lift clamped to headroom {headroom}, got {}",
            r.exposure_ev
        );
    }

    /// An overexposed frame is pulled down: headroom goes negative.
    #[test]
    fn overexposure_produces_negative_ev() {
        let mut s = neutral_stats();
        s.p50 = 0.5;
        s.p99 = 1.0;
        let r = decide(&s, &exif_at_iso(100), &sharp(0.5));
        assert!(r.exposure_ev < 0.0, "ev should be negative, was {}", r.exposure_ev);
    }

    /// REGRESSION GUARD. A dark frame containing a few blown specular
    /// highlights must still be lifted. These are the real measured values from
    /// `example-pictures/DSC03073.ARW`: p999 is fully saturated at 1.0 because
    /// 2.1% of pixels clip, but p99 shows plenty of headroom remains.
    ///
    /// Deriving headroom from p999 — as the original spec §6 did — emitted
    /// -0.07 EV here and threw away a wanted +1.62 EV lift. Any change that
    /// reintroduces a p999-based headroom will fail this test.
    #[test]
    fn saturated_p999_does_not_suppress_the_lift() {
        let mut s = neutral_stats();
        s.p50 = 0.05866;
        s.p99 = 0.42;
        s.p999 = 1.0;
        s.clipped_frac = 0.0212;
        let r = decide(&s, &exif_at_iso(100), &sharp(0.5));
        assert!(
            r.exposure_ev > 1.0,
            "a dark frame with a few clipped highlights must still be lifted, got {}",
            r.exposure_ev
        );
    }

    /// The +3 EV clamp is reachable and must bind: a nearly black frame wants
    /// far more than three stops.
    #[test]
    fn extreme_underexposure_is_clamped_to_three_stops() {
        let mut s = neutral_stats();
        s.p50 = 0.0001;
        s.p99 = 0.001;
        let r = decide(&s, &exif_at_iso(100), &sharp(0.5));
        assert_eq!(r.exposure_ev, 3.0, "the upper clamp must bind exactly");
    }

    /// The -3 EV clamp is UNREACHABLE by construction, and this test records
    /// why so nobody mistakes it for tested behaviour. Percentiles are
    /// normalised to 0..=1, so the most negative `lift` obtainable is
    /// log2(0.18 / 1.0) = -2.474 EV, and `headroom` only ever raises the
    /// minimum further. The clamp stays as defence-in-depth against a future
    /// change to the percentile range; what is asserted here is the real
    /// reachable floor.
    #[test]
    fn maximum_pull_down_is_the_reachable_floor_not_the_clamp() {
        let mut s = neutral_stats();
        s.p50 = 1.0;
        s.p99 = 1.0;
        let r = decide(&s, &exif_at_iso(100), &sharp(0.5));
        let reachable_floor = (0.18f32 / 1.0).log2(); // -2.474
        assert!(
            (r.exposure_ev - reachable_floor).abs() < 0.01,
            "expected the reachable floor {reachable_floor}, got {}",
            r.exposure_ev
        );
        assert!(r.exposure_ev > -3.0, "the -3 clamp should never be what binds");
    }

    /// Degenerate percentiles must not produce NaN — log2(0) is -inf and would
    /// poison every downstream clamp and the .pp3 text.
    #[test]
    fn zero_percentiles_do_not_produce_nan() {
        let mut s = neutral_stats();
        s.p50 = 0.0;
        s.p99 = 0.0;
        s.p999 = 0.0;
        let r = decide(&s, &exif_at_iso(100), &sharp(0.5));
        assert!(r.exposure_ev.is_finite(), "ev was {}", r.exposure_ev);
        assert!(r.shadow_lift.is_finite());
    }

    /// NEGATIVE percentiles are the case the LOG_FLOOR guards actually carry:
    /// `log2` of a negative number is NaN, and unlike an infinity a NaN
    /// survives `clamp`. Zeroed percentiles alone do not prove the guards are
    /// load-bearing, because they produce an infinity that the trailing clamp
    /// would rescue on its own.
    #[test]
    fn negative_percentiles_do_not_produce_nan() {
        let mut s = neutral_stats();
        s.p1 = -0.5;
        s.p50 = -0.5;
        s.p99 = -0.5;
        s.p999 = -0.5;
        let r = decide(&s, &exif_at_iso(100), &sharp(0.5));
        for (name, v) in [
            ("exposure_ev", r.exposure_ev),
            ("highlight_recovery", r.highlight_recovery),
            ("shadow_lift", r.shadow_lift),
            ("denoise_luma", r.denoise_luma),
            ("sharpen_amount", r.sharpen_amount),
        ] {
            assert!(v.is_finite(), "{name} was {v}");
        }
    }

    /// Denoise rises monotonically with ISO across the spec's anchor points.
    #[test]
    fn denoise_is_monotone_in_iso() {
        let isos = [100u32, 400, 1600, 6400, 25600, 102400];
        let mut prev_l = -1.0f32;
        let mut prev_c = -1.0f32;
        for iso in isos {
            let r = decide(&neutral_stats(), &exif_at_iso(iso), &sharp(0.5));
            assert!(r.denoise_luma >= prev_l, "luma dropped at ISO {iso}");
            assert!(r.denoise_chroma >= prev_c, "chroma dropped at ISO {iso}");
            assert!((0.0..=1.0).contains(&r.denoise_luma));
            assert!((0.0..=1.0).contains(&r.denoise_chroma));
            prev_l = r.denoise_luma;
            prev_c = r.denoise_chroma;
        }
    }

    /// The spec's anchors, checked at the anchor points themselves.
    #[test]
    fn denoise_hits_the_specified_anchors() {
        for (iso, expected) in [(100u32, 0.0f32), (1600, 0.3), (6400, 0.6), (25600, 0.85)] {
            let r = decide(&neutral_stats(), &exif_at_iso(iso), &sharp(0.5));
            assert!(
                (r.denoise_luma - expected).abs() < 0.02,
                "ISO {iso}: expected ~{expected}, got {}",
                r.denoise_luma
            );
        }
    }

    /// Missing ISO must not panic; treat it as base ISO.
    #[test]
    fn missing_iso_falls_back_to_base() {
        let exif = ExifData::default();
        let r = decide(&neutral_stats(), &exif, &sharp(0.5));
        assert_eq!(r.denoise_luma, 0.0);
    }

    /// Shadow lift is throttled at high ISO: lifting shadows there only
    /// reveals noise, so the ceiling falls as ISO rises.
    #[test]
    fn shadow_lift_ceiling_falls_with_iso() {
        let mut s = neutral_stats();
        s.p1 = 0.0;
        s.black_frac = 0.05;
        let low = decide(&s, &exif_at_iso(100), &sharp(0.5)).shadow_lift;
        let high = decide(&s, &exif_at_iso(25600), &sharp(0.5)).shadow_lift;
        assert!(low > high, "low-ISO lift {low} should exceed high-ISO {high}");
        assert!((0.0..=1.0).contains(&high));
    }

    /// Highlight recovery scales with how much actually clipped.
    #[test]
    fn highlight_recovery_scales_with_clipping() {
        let mut none = neutral_stats();
        none.clipped_frac = 0.0;
        let mut heavy = neutral_stats();
        heavy.clipped_frac = 0.20;
        let r_none = decide(&none, &exif_at_iso(100), &sharp(0.5));
        let r_heavy = decide(&heavy, &exif_at_iso(100), &sharp(0.5));
        assert_eq!(r_none.highlight_recovery, 0.0);
        assert!(r_heavy.highlight_recovery > 0.5);
        assert!(r_heavy.highlight_recovery <= 1.0);

        // An interior point, so a wrong scale divisor cannot hide behind the
        // clamped extremes: clipped_frac 0.025 against the 0.05 scale is 0.5.
        let mut mid = neutral_stats();
        mid.clipped_frac = 0.025;
        let r_mid = decide(&mid, &exif_at_iso(100), &sharp(0.5));
        assert!(
            (r_mid.highlight_recovery - 0.5).abs() < 0.01,
            "expected 0.5 at the midpoint, got {}",
            r_mid.highlight_recovery
        );
    }

    /// A soft frame is never sharpened into crunch.
    #[test]
    fn soft_frames_are_not_over_sharpened() {
        let soft = decide(&neutral_stats(), &exif_at_iso(100), &sharp(0.05));
        let crisp = decide(&neutral_stats(), &exif_at_iso(100), &sharp(0.95));
        assert!(soft.sharpen_amount < crisp.sharpen_amount);
        assert!(crisp.sharpen_amount <= 0.8, "hard cap breached: {}", crisp.sharpen_amount);
    }

    /// Lens correction is on only when EXIF names a lens; RawTherapee no-ops
    /// if its own lensfun lookup fails.
    #[test]
    fn lens_correction_follows_exif_lens_presence() {
        let with = decide(&neutral_stats(), &exif_at_iso(100), &sharp(0.5));
        assert!(with.lens_correct);
        let without = decide(&neutral_stats(), &ExifData::default(), &sharp(0.5));
        assert!(!without.lens_correct);
    }

    /// v1 has no white-balance override: the recipe carries no WB field and the
    /// illuminant estimate must not change any decision. A frame with a wildly
    /// disagreeing illuminant must produce the same recipe as one with none.
    #[test]
    fn illuminant_estimate_does_not_affect_the_recipe() {
        let plain = decide(&neutral_stats(), &exif_at_iso(100), &sharp(0.5));
        let mut s = neutral_stats();
        s.illum_r = Some(1.0);
        s.illum_g = Some(1.0);
        s.illum_b = Some(0.1);
        let with_estimate = decide(&s, &exif_at_iso(100), &sharp(0.5));
        assert_eq!(
            plain, with_estimate,
            "v1 must not act on the illuminant estimate"
        );
    }

    /// The recipe hash is what idempotency keys on: identical recipes must
    /// hash identically, and any field change must move it.
    #[test]
    fn recipe_hash_is_stable_and_sensitive() {
        let a = decide(&neutral_stats(), &exif_at_iso(100), &sharp(0.5));
        let b = decide(&neutral_stats(), &exif_at_iso(100), &sharp(0.5));
        assert_eq!(a.recipe_hash(), b.recipe_hash());
        let mut c = a.clone();
        c.exposure_ev += 0.5;
        assert_ne!(a.recipe_hash(), c.recipe_hash());
    }
}
```

- [ ] **Step 2: Run them to confirm they fail**

Run: `cargo test -p pipeline --lib develop::decide`
Expected: FAIL — the module is not declared and `decide` does not exist.

- [ ] **Step 3: Declare the module**

In `crates/pipeline/src/develop/mod.rs`, add next to `pub mod measure;`:

```rust
pub mod decide;
```

- [ ] **Step 4: Implement the decision layer**

Prepend to `crates/pipeline/src/develop/decide.rs`:

```rust
//! The decision layer: a pure function from measurements to an `EditRecipe`.
//!
//! No image data, no I/O, no database. That is the single most important
//! testability property in this design — the tuning-sensitive logic is
//! exercised by table-driven unit tests over numbers, without fixtures.

use crate::develop::measure::RawStats;
use crate::ingest::exif::ExifData;

/// Bumped whenever any formula below changes. Stored in `edits.decider_version`
/// and part of the idempotency key, so a tuning change re-renders everything.
pub const DECIDER_VERSION: &str = "decide-2";

/// The sharpness measurement `decide` consumes. A struct rather than a bare f32
/// so adding subject/background terms later does not churn the signature.
#[derive(Debug, Clone, Copy)]
pub struct Sharpness {
    /// Global sharpness score from the `sharpness` table, roughly 0..1.
    pub s_global: f32,
}

/// A complete, renderer-agnostic development recipe. Every field is normalised
/// 0..1 except `exposure_ev`, which is in stops.
///
/// Carries NO white-balance fields. v1 emits RawTherapee's `Setting=Camera`,
/// which applies the camera's own as-shot coefficients exactly. See `decide()`.
#[derive(Debug, Clone, PartialEq)]
pub struct EditRecipe {
    pub exposure_ev: f32,
    pub highlight_recovery: f32,
    pub shadow_lift: f32,
    pub denoise_luma: f32,
    pub denoise_chroma: f32,
    pub sharpen_amount: f32,
    pub lens_correct: bool,
}

impl EditRecipe {
    /// Content hash of the recipe, for idempotency. Fields are quantised before
    /// hashing so a float rounding difference of 1e-9 does not force a re-render.
    pub fn recipe_hash(&self) -> String {
        use std::hash::Hasher;
        let mut h = xxhash_rust::xxh3::Xxh3::new();
        for v in [
            self.exposure_ev,
            self.highlight_recovery,
            self.shadow_lift,
            self.denoise_luma,
            self.denoise_chroma,
            self.sharpen_amount,
        ] {
            h.write_i64((v * 10_000.0).round() as i64);
        }
        h.write_u8(self.lens_correct as u8);
        format!("{:016x}", h.finish())
    }
}

/// Middle grey in a linear raw signal.
const MID_GREY: f32 = 0.18;
/// Target for the 99.9th percentile: just below clipping.
const HIGHLIGHT_TARGET: f32 = 0.95;
/// Floor for any percentile before a log is taken, so log2 never sees zero.
const LOG_FLOOR: f32 = 1e-6;

/// Decide how to develop one photo.
pub fn decide(raw: &RawStats, exif: &ExifData, sharp: &Sharpness) -> EditRecipe {
    let iso = exif.iso.unwrap_or(100) as f32;

    // ── exposure ──
    // Lift toward middle grey, but never past the point where the brightest
    // *recoverable* detail clips. An overexposed frame has negative headroom,
    // which pulls the exposure down.
    //
    // Headroom is measured from p99, NOT p999. p999 saturates to 1.0 as soon as
    // more than 0.1% of pixels clip — true of almost any frame containing sky, a
    // specular highlight, or a light source — after which it reports zero
    // headroom regardless of how dark the image actually is, and the lift is
    // silently thrown away. Measured on a real frame: p50 = 0.0587 (median 1.62
    // stops below middle grey), p999 = 1.0, clipped_frac = 2.1%; the p999 form
    // emitted -0.07 EV where +1.62 EV was wanted. Pixels that already clipped
    // are unrecoverable, so protecting them costs the rest of the image.
    let headroom = (HIGHLIGHT_TARGET / raw.p99.max(LOG_FLOOR)).log2();
    let lift = (MID_GREY / raw.p50.max(LOG_FLOOR)).log2();
    let wanted = lift.min(headroom).clamp(-3.0, 3.0);
    // Soft deadband. Modern camera metering is good, and the CHECKPOINT review
    // showed the unguarded correction actively made well-metered frames worse:
    // on three real frames the baseline render beat ours wherever a lift was
    // applied, because `MID_GREY` is a grey-card reflectance target and the
    // median of a scene full of dark conifers is legitimately far below it.
    // So correct outliers, not every frame — the same reasoning already applied
    // to white balance.
    //
    // Subtracting the deadband rather than thresholding on it keeps the response
    // continuous: a frame wanting 0.76 EV gets 0.01 rather than jumping to 0.76.
    // It is also inherently conservative, since the applied correction is always
    // smaller than the computed one.
    let exposure_ev = deadband(wanted, EXPOSURE_DEADBAND_EV);

    // ── white balance ──
    // Nothing to decide. v1 emits RawTherapee's `Setting=Camera`, so the camera's
    // own as-shot coefficients are applied exactly and no conversion error can
    // enter. `EditRecipe` therefore carries no white-balance field at all.
    //
    // An earlier revision converted the as-shot coefficients into RawTherapee's
    // Temperature/Green parameterisation and got it wrong twice: the temperature
    // relation was inverted (tungsten -> 8214 K, daylight -> 3713 K) and Green
    // landed near 0.5 against its 1.0 neutral, casting every frame magenta.
    //
    // The PCA illuminant estimate in `raw.illum_*` is measured and persisted for
    // the audit record but deliberately not acted on: overriding the camera needs
    // a conversion we can verify, which is deferred to its own spec.

    // ── highlight recovery ──
    // Scales with how much actually clipped. 5% clipped is already severe.
    let highlight_recovery = (raw.clipped_frac / 0.05).clamp(0.0, 1.0);

    // ── shadow lift ──
    // Driven only by genuinely crushed blacks, with its own deadband, and
    // throttled by a noise penalty since lifting shadows at high ISO only
    // reveals noise.
    //
    // The `p1` term this replaced was the same category of error as the exposure
    // target: a low 1st percentile means the scene *has* deep shadows, not that
    // it is broken. On a real frame p1 = 0.0018 drove shadow_lift to 0.46, which
    // flattened the image badly. `black_frac` already measures what matters —
    // how much actually hit the black level — so the deadband keys on that alone.
    let shadow_demand =
        ((raw.black_frac - SHADOW_DEADBAND_FRAC) / 0.045).clamp(0.0, 1.0);
    let noise_penalty = 1.0 - denoise_curve(iso);
    let shadow_lift = (shadow_demand * 0.5 * noise_penalty).clamp(0.0, 1.0);

    // ── denoise ──
    let denoise_luma = denoise_curve(iso);
    // Chroma noise is more objectionable and cheaper to remove than luma.
    let denoise_chroma = (denoise_luma * 1.2).clamp(0.0, 1.0);

    // ── sharpening ──
    // Modulated by measured sharpness and hard-capped, so a genuinely soft
    // frame is never sharpened into crunch. The cap is structural: the input is
    // clamped to 0..1 and then scaled, so the product cannot exceed SHARPEN_MAX.
    // An outer `.clamp(0.0, SHARPEN_MAX)` here would be a no-op and would make
    // the cap untestable, since removing it could not change any result.
    let sharpen_amount = sharp.s_global.clamp(0.0, 1.0) * SHARPEN_MAX;

    EditRecipe {
        exposure_ev,
        highlight_recovery,
        shadow_lift,
        denoise_luma,
        denoise_chroma,
        sharpen_amount,
        lens_correct: exif.lens_model.is_some(),
    }
}

/// Exposure corrections smaller than this are not applied at all; larger ones
/// have it subtracted. Tuned at the CHECKPOINT against real frames.
const EXPOSURE_DEADBAND_EV: f32 = 0.75;

/// Shadow lift stays at zero until at least this fraction of the frame has
/// actually hit the black level.
const SHADOW_DEADBAND_FRAC: f32 = 0.005;

/// Subtract `dz` from the magnitude of `v`, preserving sign, floored at zero.
/// Continuous, and always returns something no larger than `v`.
fn deadband(v: f32, dz: f32) -> f32 {
    let out = (v.abs() - dz).max(0.0);
    if v.is_sign_negative() { -out } else { out }
}

/// Ceiling on capture-sharpening strength. Applied as a scale factor rather
/// than an outer clamp so the bound is structural and a change to it is
/// observable in the tests.
const SHARPEN_MAX: f32 = 0.8;

/// Piecewise-linear denoise strength in ISO, through the spec's anchor points:
/// (100→0), (1600→0.3), (6400→0.6), (25600→0.85).
///
/// These anchors are a starting shape, not a validated claim — spec §13 open
/// item 2 calls for calibration against real high-ISO files before they can be
/// described as tuned.
fn denoise_curve(iso: f32) -> f32 {
    const ANCHORS: [(f32, f32); 4] = [(100.0, 0.0), (1600.0, 0.3), (6400.0, 0.6), (25600.0, 0.85)];
    if iso <= ANCHORS[0].0 {
        return ANCHORS[0].1;
    }
    for w in ANCHORS.windows(2) {
        let (x0, y0) = w[0];
        let (x1, y1) = w[1];
        if iso <= x1 {
            // Interpolate in log-ISO: a stop is a stop, whatever the absolute value.
            let t = (iso.log2() - x0.log2()) / (x1.log2() - x0.log2());
            return y0 + t * (y1 - y0);
        }
    }
    // Beyond the last anchor, approach but never reach 1.0.
    let last = ANCHORS[ANCHORS.len() - 1];
    (last.1 + (iso.log2() - last.0.log2()) * 0.05).clamp(0.0, 0.98)
}

```

- [ ] **Step 5: Add `xxhash-rust` to the pipeline crate if it is not already there**

It is already a workspace dependency and already listed in
`crates/pipeline/Cargo.toml`. Confirm with:

```bash
grep -n "xxhash-rust" crates/pipeline/Cargo.toml
```

Expected: a line is printed. If not, add `xxhash-rust = { workspace = true }`.

- [ ] **Step 6: Run the tests**

Run: `cargo test -p pipeline --lib develop::decide`
Expected: PASS, 16 tests.

If `denoise_hits_the_specified_anchors` fails at ISO 100, check that
`denoise_curve` returns the first anchor for `iso <= 100` rather than
interpolating from zero.

- [ ] **Step 7: Verify green and commit**

```bash
cargo fmt && cargo clippy --all-targets --all-features -- -D warnings && cargo test --all
git add crates/pipeline/src/develop
git commit -m "feat(develop): the pure decision layer, decide() and EditRecipe"
```

---

### Task 7: PCA illuminant estimator

Resolves spec open item 4. The estimator is a cross-check on the camera's own
white balance, so it must **fail soft**: when it cannot produce a trustworthy
answer it returns `None`, `decide()` keeps the as-shot coefficients, and nothing
propagates as an error.

**Files:**
- Create: `crates/pipeline/src/develop/illuminant.rs`
- Modify: `crates/pipeline/src/develop/mod.rs` (declare the module)
- Modify: `crates/pipeline/src/develop/measure.rs` (call it from `measure_raw`)
- Test: inline `#[cfg(test)] mod tests` in `illuminant.rs`

**Interfaces:**
- Consumes: nothing outside the crate.
- Produces: `pipeline::develop::illuminant::estimate_illuminant(pixels: &[[f32; 3]]) -> Option<[f32; 3]>`

- [ ] **Step 1: Write the failing tests**

Create `crates/pipeline/src/develop/illuminant.rs` with only a test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// A grey scene under a neutral light: the estimate is neutral.
    #[test]
    fn neutral_scene_gives_neutral_illuminant() {
        let px: Vec<[f32; 3]> = (1..200)
            .map(|i| {
                let v = i as f32 / 200.0;
                [v, v, v]
            })
            .collect();
        let e = estimate_illuminant(&px).expect("neutral scene should estimate");
        assert!((e[0] - e[1]).abs() < 0.05, "not neutral: {e:?}");
        assert!((e[1] - e[2]).abs() < 0.05, "not neutral: {e:?}");
    }

    /// The same scene under a warm light: the estimate leans red.
    #[test]
    fn warm_cast_is_detected() {
        let px: Vec<[f32; 3]> = (1..200)
            .map(|i| {
                let v = i as f32 / 200.0;
                [v * 1.6, v, v * 0.6]
            })
            .collect();
        let e = estimate_illuminant(&px).expect("cast scene should estimate");
        assert!(e[0] > e[1], "red should dominate: {e:?}");
        assert!(e[1] > e[2], "blue should be weakest: {e:?}");
    }

    /// The result is a unit vector — only direction carries colour.
    #[test]
    fn estimate_is_normalised() {
        let px: Vec<[f32; 3]> = (1..200).map(|i| { let v = i as f32 / 200.0; [v * 1.6, v, v * 0.6] }).collect();
        let e = estimate_illuminant(&px).unwrap();
        let norm = (e[0] * e[0] + e[1] * e[1] + e[2] * e[2]).sqrt();
        assert!((norm - 1.0).abs() < 1e-4, "norm was {norm}");
    }

    /// Too few usable pixels: fail soft rather than guess.
    #[test]
    fn insufficient_pixels_return_none() {
        assert!(estimate_illuminant(&[]).is_none());
        assert!(estimate_illuminant(&[[0.5, 0.5, 0.5]]).is_none());
    }

    /// An entirely clipped or entirely black frame carries no colour
    /// information; both must return None rather than a degenerate direction.
    #[test]
    fn degenerate_frames_return_none() {
        let white = vec![[1.0f32, 1.0, 1.0]; 500];
        assert!(estimate_illuminant(&white).is_none());
        let black = vec![[0.0f32, 0.0, 0.0]; 500];
        assert!(estimate_illuminant(&black).is_none());
    }

    /// Non-finite input must never escape into the result — decide() takes
    /// logs and reciprocals of these values.
    #[test]
    fn non_finite_pixels_are_rejected() {
        let mut px: Vec<[f32; 3]> = (1..200).map(|i| { let v = i as f32 / 200.0; [v, v, v] }).collect();
        px.push([f32::NAN, 1.0, 1.0]);
        px.push([f32::INFINITY, 1.0, 1.0]);
        let e = estimate_illuminant(&px).expect("the valid majority should still estimate");
        assert!(e.iter().all(|v| v.is_finite()), "non-finite escaped: {e:?}");
    }
}
```

- [ ] **Step 2: Run them to confirm they fail**

Run: `cargo test -p pipeline --lib develop::illuminant`
Expected: FAIL — module not declared.

- [ ] **Step 3: Declare the module**

In `crates/pipeline/src/develop/mod.rs`:

```rust
pub mod illuminant;
```

- [ ] **Step 4: Implement the estimator**

Prepend to `crates/pipeline/src/develop/illuminant.rs`:

```rust
//! Illuminant estimation by principal component analysis of bright pixels
//! (Cheng et al., 2014, "Illuminant Estimation for Color Constancy: Why
//! spatial-domain methods work and the role of the color distribution").
//!
//! The insight: in a linear RGB scene the brightest chromatic pixels line up
//! along the illuminant direction, so the first principal component of that
//! subset *is* the illuminant. Cheap, no training, no model file.
//!
//! Fails soft by design. This is a cross-check on the camera's own white
//! balance, and the camera is usually right — returning `None` costs nothing
//! because `decide()` then keeps the as-shot coefficients.

/// Fraction of the brightest pixels to run the PCA over.
const BRIGHT_FRACTION: f32 = 0.035;
/// Below this many usable pixels the principal component is noise.
const MIN_PIXELS: usize = 32;
/// Reject pixels at or above this: clipped channels have lost their ratios.
const CLIP_CEILING: f32 = 0.99;
/// Reject pixels below this: read noise dominates and the direction is random.
const BLACK_FLOOR: f32 = 0.01;

/// Estimate the scene illuminant as a unit RGB direction.
///
/// `pixels` must be linear RGB in 0..1. Returns `None` when the frame carries
/// no usable colour information.
pub fn estimate_illuminant(pixels: &[[f32; 3]]) -> Option<[f32; 3]> {
    // Keep only well-exposed, finite pixels.
    let mut usable: Vec<[f32; 3]> = pixels
        .iter()
        .copied()
        .filter(|p| {
            p.iter().all(|v| v.is_finite())
                && p.iter().all(|v| *v < CLIP_CEILING)
                && p.iter().any(|v| *v > BLACK_FLOOR)
        })
        .collect();
    if usable.len() < MIN_PIXELS {
        return None;
    }

    // Brightest first; the illuminant direction is clearest in the highlights.
    usable.sort_by(|a, b| {
        let sa: f32 = a.iter().sum();
        let sb: f32 = b.iter().sum();
        sb.partial_cmp(&sa).unwrap_or(std::cmp::Ordering::Equal)
    });
    let take = ((usable.len() as f32 * BRIGHT_FRACTION) as usize).max(MIN_PIXELS);
    let bright = &usable[..take.min(usable.len())];

    // First principal component by power iteration on the 3×3 scatter matrix.
    // Uncentred on purpose: we want the direction from the origin, which is
    // what the illuminant is, not the direction of maximum variance.
    let mut scatter = [[0.0f64; 3]; 3];
    for p in bright {
        for i in 0..3 {
            for j in 0..3 {
                scatter[i][j] += (p[i] as f64) * (p[j] as f64);
            }
        }
    }

    let mut v = [1.0f64, 1.0, 1.0];
    for _ in 0..64 {
        let mut next = [0.0f64; 3];
        for i in 0..3 {
            for j in 0..3 {
                next[i] += scatter[i][j] * v[j];
            }
        }
        let norm = (next[0] * next[0] + next[1] * next[1] + next[2] * next[2]).sqrt();
        if !norm.is_finite() || norm < 1e-12 {
            return None;
        }
        for i in 0..3 {
            next[i] /= norm;
        }
        v = next;
    }

    // The component may come out negated; an illuminant is positive.
    if v.iter().sum::<f64>() < 0.0 {
        for x in v.iter_mut() {
            *x = -*x;
        }
    }
    let out = [v[0] as f32, v[1] as f32, v[2] as f32];
    if out.iter().any(|x| !x.is_finite() || *x <= 0.0) {
        return None;
    }
    Some(out)
}
```

- [ ] **Step 5: Run the tests**

Run: `cargo test -p pipeline --lib develop::illuminant`
Expected: PASS, 6 tests.

- [ ] **Step 6: Wire it into `measure_raw`**

The estimator needs RGB triples, but a Bayer sensor delivers one channel per
photosite. Demosaicing for a white-balance estimate would be wasteful, so build
coarse RGB triples by averaging each 2×2 CFA cell.

Add to `crates/pipeline/src/develop/measure.rs`, and call it from `measure_raw`
just before the `Ok(stats)`:

```rust
/// Build coarse linear-RGB triples by averaging 2×2 CFA cells.
///
/// Not a demosaic — each output pixel is a whole cell, so the result is
/// quarter resolution and has no interpolation artefacts. That is exactly
/// right for a white-balance estimate and far cheaper than demosaicing.
fn cfa_cells_to_rgb(raw: &rawler::rawimage::RawImage, black: f32, white: f32) -> Vec<[f32; 3]> {
    use rawler::rawimage::RawPhotometricInterpretation;

    let RawPhotometricInterpretation::Cfa(ref cfg) = raw.photometric else {
        // Already RGB (some DNGs, LinearRaw): take pixels directly.
        let data = raw.data.as_f32();
        if raw.cpp != 3 {
            return Vec::new();
        }
        let range = (white - black).max(1.0);
        return data
            .chunks_exact(3)
            .map(|c| {
                [
                    ((c[0] - black) / range).clamp(0.0, 1.0),
                    ((c[1] - black) / range).clamp(0.0, 1.0),
                    ((c[2] - black) / range).clamp(0.0, 1.0),
                ]
            })
            .collect();
    };

    let data = raw.data.as_f32();
    let (w, h) = (raw.width, raw.height);
    let range = (white - black).max(1.0);
    // Cap the walk: a full 60MP pass is pointless for a 3-vector estimate.
    let cell_stride = (((w * h) / (4 * 250_000)) as usize).max(1);

    let mut out = Vec::new();
    let mut cell_index = 0usize;
    for y in (0..h.saturating_sub(1)).step_by(2) {
        for x in (0..w.saturating_sub(1)).step_by(2) {
            cell_index += 1;
            if cell_index % cell_stride != 0 {
                continue;
            }
            let mut sum = [0.0f32; 3];
            let mut count = [0u32; 3];
            for (dy, dx) in [(0, 0), (0, 1), (1, 0), (1, 1)] {
                let color = cfg.cfa.color_at(y + dy, x + dx);
                if color > 2 {
                    continue; // the E channel of an RGBE sensor
                }
                let v = data[(y + dy) * w + (x + dx)];
                sum[color] += ((v - black) / range).clamp(0.0, 1.0);
                count[color] += 1;
            }
            if count.iter().any(|c| *c == 0) {
                continue;
            }
            out.push([
                sum[0] / count[0] as f32,
                sum[1] / count[1] as f32,
                sum[2] / count[2] as f32,
            ]);
        }
    }
    out
}
```

Then, in `measure_raw`, after the `wb_coeffs` block and before `Ok(stats)`:

```rust
    let cells = cfa_cells_to_rgb(&raw, black, white);
    match crate::develop::illuminant::estimate_illuminant(&cells) {
        Some(e) => {
            stats.illum_r = Some(e[0]);
            stats.illum_g = Some(e[1]);
            stats.illum_b = Some(e[2]);
        }
        None => {
            tracing::debug!(
                path = %path.display(),
                "illuminant estimation declined; keeping as-shot white balance"
            );
        }
    }
```

- [ ] **Step 7: Verify green and commit**

```bash
cargo fmt && cargo clippy --all-targets --all-features -- -D warnings && cargo test --all
git add crates/pipeline/src/develop
git commit -m "feat(develop): PCA illuminant estimator as a white-balance cross-check"
```

---

### Task 8: `.pp3` emission

**Depends on Task 2.** Every key name written here must match
`docs/design/pp3-keys.md`. RawTherapee silently ignores keys it does not
recognise, so a typo produces a plausible render with a setting that never
applied — which is why the golden test below exists.

**Files:**
- Create: `crates/pipeline/src/develop/pp3.rs`
- Create: `crates/pipeline/tests/fixtures/golden.pp3`
- Modify: `crates/pipeline/src/develop/mod.rs` (declare the module)
- Test: inline `#[cfg(test)] mod tests` in `pp3.rs`

**Interfaces:**
- Consumes: `EditRecipe` (Task 6)
- Produces:
  - `pipeline::develop::pp3::BASE_PP3: &str` — `include_str!` of the Task 2 asset
  - `pipeline::develop::pp3::emit_pp3(recipe: &EditRecipe) -> String`

- [ ] **Step 1: Write the failing tests**

Create `crates/pipeline/src/develop/pp3.rs` with only a test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::develop::decide::EditRecipe;

    fn fixed_recipe() -> EditRecipe {
        EditRecipe {
            exposure_ev: 0.75,
            highlight_recovery: 0.6,
            shadow_lift: 0.4,
            denoise_luma: 0.3,
            denoise_chroma: 0.36,
            sharpen_amount: 0.5,
            lens_correct: true,
        }
    }

    /// Golden-file test: a fixed recipe produces byte-exact .pp3 text.
    /// This is what catches accidental key drift.
    #[test]
    fn fixed_recipe_matches_golden_file() {
        let got = emit_pp3(&fixed_recipe());
        let want = include_str!("../../tests/fixtures/golden.pp3");
        assert_eq!(got, want, "\n--- emitted ---\n{got}\n--- expected ---\n{want}");
    }

    /// Percent-scaled fields map from 0..1 onto RawTherapee's 0..100.
    #[test]
    fn unit_fields_scale_to_percent() {
        let out = emit_pp3(&fixed_recipe());
        assert!(out.contains("Luma=30"), "denoise_luma 0.3 should emit 30:\n{out}");
        assert!(out.contains("Chroma=36"), "denoise_chroma 0.36 should emit 36:\n{out}");
    }

    /// The three silent no-op traps from docs/design/pp3-keys.md. Each of
    /// these keys defaults to a value that makes a neighbouring key we DO set
    /// be ignored, with no warning from RawTherapee.
    #[test]
    fn silent_no_op_traps_are_defused() {
        let out = emit_pp3(&fixed_recipe());
        assert!(out.contains("Setting=Camera"), "WB must use the camera's own coefficients:\n{out}");
        assert!(out.contains("AutoContrast=false"), "Contrast would be ignored:\n{out}");
        assert!(out.contains("AutoRadius=false"), "DeconvRadius would be ignored:\n{out}");
        assert!(out.contains("CMethod=MAN"), "Chroma would be ignored:\n{out}");
    }

    /// Zeroed corrections emit disabled tools, not enabled ones set to zero —
    /// an enabled no-op tool still costs render time.
    #[test]
    fn zero_strength_disables_the_tool() {
        let mut r = fixed_recipe();
        r.highlight_recovery = 0.0;
        r.denoise_luma = 0.0;
        r.denoise_chroma = 0.0;
        r.shadow_lift = 0.0;
        let out = emit_pp3(&r);
        assert!(out.contains("[HLRecovery]\nEnabled=false"), "{out}");
        assert!(
            out.contains("[Directional Pyramid Denoising]\nEnabled=false"),
            "{out}"
        );
    }

    /// Lens correction off must not emit a LensProfile section at all, so the
    /// base profile's own setting is left untouched.
    #[test]
    fn lens_correction_off_omits_the_section() {
        let mut r = fixed_recipe();
        r.lens_correct = false;
        let out = emit_pp3(&r);
        assert!(!out.contains("[LensProfile]"), "{out}");
    }

    /// Floats must render with a fixed decimal count and a `.` separator —
    /// never locale-dependent, and never scientific notation, which
    /// RawTherapee cannot parse.
    #[test]
    fn floats_are_formatted_deterministically() {
        let mut r = fixed_recipe();
        r.exposure_ev = 0.000_012_5;
        let out = emit_pp3(&r);
        assert!(!out.contains('e'), "scientific notation leaked:\n{out}");
        assert!(out.contains("Compensation=0.000"), "{out}");
    }

    /// Every emitted line is `key=value` under a section header. A stray line
    /// makes RawTherapee reject the whole profile.
    #[test]
    fn output_is_well_formed_ini() {
        let out = emit_pp3(&fixed_recipe());
        let mut seen_section = false;
        for line in out.lines() {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if line.starts_with('[') {
                assert!(line.ends_with(']'), "malformed section: {line}");
                seen_section = true;
                continue;
            }
            assert!(seen_section, "key before any section: {line}");
            assert!(line.contains('='), "not a key=value line: {line}");
        }
    }
}
```

- [ ] **Step 2: Run them to confirm they fail**

Run: `cargo test -p pipeline --lib develop::pp3`
Expected: FAIL — module not declared, and `tests/fixtures/golden.pp3` missing.

- [ ] **Step 3: Declare the module**

In `crates/pipeline/src/develop/mod.rs`:

```rust
pub mod pp3;
```

- [ ] **Step 4: Implement emission**

Prepend to `crates/pipeline/src/develop/pp3.rs`. **Reconcile every section and
key below against `docs/design/pp3-keys.md` before running the tests** — that
file, not this plan, is the authority:

```rust
//! `EditRecipe` → RawTherapee processing profile.
//!
//! Emits ONLY technical corrections. The look is applied afterwards, in Rust,
//! to the 16-bit TIFF (spec §4). Nothing here may touch film simulation, tone
//! curves, or saturation — `base.pp3` owns those, and overriding them would
//! change the input distribution the look model was trained on.
//!
//! Key names come from `docs/design/pp3-keys.md`, established by GUI diffing.
//! RawTherapee silently ignores unknown keys, so drift here is invisible at
//! runtime and only the golden test catches it.

use crate::develop::decide::EditRecipe;

/// The version-controlled neutral baseline, applied before the per-photo
/// profile. Embedded so the binary carries no external file dependency.
pub const BASE_PP3: &str = include_str!("../../assets/base.pp3");

/// Render `recipe` as a `.pp3` to stack on top of `BASE_PP3`.
pub fn emit_pp3(recipe: &EditRecipe) -> String {
    let mut s = String::with_capacity(1024);

    s.push_str("# Generated by photopipe. Stacked on base.pp3; do not edit by hand.\n");

    // ── exposure ──
    s.push_str("\n[Exposure]\n");
    // Auto must be false or RawTherapee overrides our compensation.
    s.push_str("Auto=false\n");
    s.push_str(&format!("Compensation={}\n", f3(recipe.exposure_ev)));

    // ── highlight reconstruction ──
    s.push_str("\n[HLRecovery]\n");
    if recipe.highlight_recovery > 0.0 {
        s.push_str("Enabled=true\n");
        // The method escalates with severity: blending is gentle and safe for
        // mild clipping; colour propagation reconstructs hue from neighbouring
        // unclipped channels and is what heavy clipping needs.
        let method = if recipe.highlight_recovery < 0.5 {
            "Blend"
        } else {
            "Coloropp"
        };
        s.push_str(&format!("Method={method}\n"));
    } else {
        s.push_str("Enabled=false\n");
    }

    // ── shadows ──
    s.push_str("\n[Shadows & Highlights]\n");
    if recipe.shadow_lift > 0.0 {
        s.push_str("Enabled=true\n");
        s.push_str(&format!("Shadows={}\n", pct(recipe.shadow_lift)));
        s.push_str("Highlights=0\n");
    } else {
        s.push_str("Enabled=false\n");
    }

    // ── white balance ──
    // ── white balance ──
    // Setting=Camera applies the camera's own as-shot coefficients exactly.
    //
    // We deliberately do NOT convert those coefficients into RawTherapee's
    // Temperature/Green parameterisation. An earlier revision did, and the
    // conversion was wrong twice over: the temperature relation was inverted
    // (tungsten light produced 8214 K, daylight 3713 K) and Green landed
    // systematically near 0.5 against its 1.0 neutral, casting every frame
    // magenta. RawTherapee derives its own multipliers from the camera profile,
    // so asking it to use the camera WB is both exact and simpler than
    // restating that WB in a foreign parameterisation.
    //
    // The PCA illuminant estimate is still measured and stored in `raw_stats`;
    // acting on it needs a verifiable conversion and is deferred.
    s.push_str("\n[White Balance]\n");
    s.push_str("Enabled=true\n");
    s.push_str("Setting=Camera\n");

    // ── denoise ──
    s.push_str("\n[Directional Pyramid Denoising]\n");
    if recipe.denoise_luma > 0.0 || recipe.denoise_chroma > 0.0 {
        s.push_str("Enabled=true\n");
        // TRAP: a single Chroma slider requires Method=Lab AND CMethod=MAN.
        // Under CMethod=AUT — which several bundled profiles use — Chroma is
        // ignored. See docs/design/pp3-keys.md.
        s.push_str("Method=Lab\n");
        s.push_str("CMethod=MAN\n");
        s.push_str(&format!("Luma={}\n", pct(recipe.denoise_luma)));
        s.push_str(&format!("Chroma={}\n", pct(recipe.denoise_chroma)));
    } else {
        s.push_str("Enabled=false\n");
    }

    // ── capture sharpening ──
    s.push_str("\n[PostDemosaicSharpening]\n");
    if recipe.sharpen_amount > 0.0 {
        s.push_str("Enabled=true\n");
        // TRAP: AutoContrast and AutoRadius both default to true, and each
        // overrides the manual value below it. See docs/design/pp3-keys.md.
        s.push_str("AutoContrast=false\n");
        s.push_str("AutoRadius=false\n");
        s.push_str(&format!("Contrast={}\n", pct(recipe.sharpen_amount)));
        s.push_str("DeconvRadius=0.750\n");
    } else {
        s.push_str("Enabled=false\n");
    }

    // ── lens correction ──
    // Omitted entirely when off, so base.pp3's own setting stands.
    if recipe.lens_correct {
        s.push_str("\n[LensProfile]\n");
        s.push_str("LcMode=lfauto\n");
        s.push_str("UseDistortion=true\n");
        s.push_str("UseVignette=true\n");
        s.push_str("UseCA=false\n");
    }

    s
}

/// Three decimal places, always `.`, never scientific notation. RawTherapee's
/// parser accepts neither locale commas nor exponents.
fn f3(v: f32) -> String {
    format!("{:.3}", if v.is_finite() { v } else { 0.0 })
}

/// A 0..1 recipe field as RawTherapee's 0..100 integer scale.
fn pct(v: f32) -> i32 {
    (v.clamp(0.0, 1.0) * 100.0).round() as i32
}
```

- [ ] **Step 5: Generate the golden file from the implementation, then read it**

```bash
mkdir -p crates/pipeline/tests/fixtures
cargo test -p pipeline --lib develop::pp3::tests::fixed_recipe_matches_golden_file -- --nocapture 2>&1 | head -60
```

The test fails and prints the emitted text. Copy it verbatim into
`crates/pipeline/tests/fixtures/golden.pp3`.

**Then read that file line by line against `docs/design/pp3-keys.md`.** This
is the point of the whole task — the golden file is only a regression guard, and
guarding the wrong keys guards nothing. Fix `emit_pp3` and regenerate if any
section header or key name disagrees with what your GUI diff showed.

- [ ] **Step 6: Run the tests**

Run: `cargo test -p pipeline --lib develop::pp3`
Expected: PASS, 7 tests.

- [ ] **Step 7: Prove the emitted profile actually changes the render**

A passing golden test proves the text is stable, not that RawTherapee honours it.
Render one file twice — once with a +2 EV profile, once with −2 EV:

```bash
cat > /tmp/plus.pp3 <<'EOF'
[Exposure]
Auto=false
Compensation=2.000
EOF
sed 's/2.000/-2.000/' /tmp/plus.pp3 > /tmp/minus.pp3

RT=/Applications/RawTherapee.app/Contents/MacOS/rawtherapee-cli
$RT -Y -t -b16 -p crates/pipeline/assets/base.pp3 -p /tmp/plus.pp3  -o /tmp/rt-plus  -c ~/Photos/<some>.ARW
$RT -Y -t -b16 -p crates/pipeline/assets/base.pp3 -p /tmp/minus.pp3 -o /tmp/rt-minus -c ~/Photos/<some>.ARW
```

Expected: two visibly different TIFFs, four stops apart. If they are identical,
the `[Exposure] Compensation` key is wrong — go back to Task 2.

- [ ] **Step 8: Verify green and commit**

```bash
cargo fmt && cargo clippy --all-targets --all-features -- -D warnings && cargo test --all
git add crates/pipeline/src/develop crates/pipeline/tests/fixtures/golden.pp3
git commit -m "feat(develop): emit RawTherapee processing profiles from an EditRecipe"
```

---

### Task 9: The RawTherapee renderer

**Files:**
- Create: `crates/pipeline/src/develop/render.rs`
- Modify: `crates/pipeline/src/develop/mod.rs` (declare the module)
- Test: inline `#[cfg(test)] mod tests` in `render.rs`

**Interfaces:**
- Consumes: `EditRecipe` (Task 6), `emit_pp3` / `BASE_PP3` (Task 8), `DevelopConfig` (Task 1), `DevelopError` (Task 4)
- Produces:
  - `pipeline::develop::render::Pp3Renderer::new(cfg: &DevelopConfig) -> Pp3Renderer`
  - `Pp3Renderer::probe(&self) -> Result<String, DevelopError>` — returns the version string
  - `Pp3Renderer::render(&self, raw: &Path, recipe: &EditRecipe, tmp_dir: &Path) -> Result<RenderedTiff, DevelopError>`
  - `pipeline::develop::render::RenderedTiff { tiff: PathBuf, pp3: PathBuf }`
  - `pipeline::develop::render::RENDERER_NAME: &str` = `"rawtherapee"`

- [ ] **Step 1: Write the failing tests**

Create `crates/pipeline/src/develop/render.rs` with only a test module. These
tests cover everything that does **not** need RawTherapee installed — argument
construction and failure handling. The real render is covered by the gated
end-to-end test in Task 12.

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DevelopConfig;

    fn cfg_with(path: &str) -> DevelopConfig {
        DevelopConfig {
            rawtherapee_path: path.into(),
            ..Default::default()
        }
    }

    /// An empty configured path means "search PATH" — not "run the empty string".
    #[test]
    fn empty_path_falls_back_to_bare_name() {
        let r = Pp3Renderer::new(&cfg_with(""));
        assert_eq!(r.exe, std::path::PathBuf::from("rawtherapee-cli"));
    }

    #[test]
    fn configured_path_is_used_verbatim() {
        let r = Pp3Renderer::new(&cfg_with("/opt/rt/rawtherapee-cli"));
        assert_eq!(r.exe, std::path::PathBuf::from("/opt/rt/rawtherapee-cli"));
    }

    /// The argument vector is the contract with RawTherapee. Order matters:
    /// `-c <input>` must come last, and the two `-p` flags must be base-then-photo
    /// so the per-photo profile wins.
    #[test]
    fn arguments_are_ordered_base_then_photo_then_input() {
        let args = build_args(
            std::path::Path::new("/tmp/base.pp3"),
            std::path::Path::new("/tmp/photo.pp3"),
            std::path::Path::new("/tmp/out"),
            std::path::Path::new("/photos/a.arw"),
        );
        let strs: Vec<String> = args.iter().map(|a| a.to_string_lossy().into_owned()).collect();
        assert_eq!(strs.last().unwrap(), "/photos/a.arw");
        assert_eq!(strs[strs.len() - 2], "-c");

        let base_at = strs.iter().position(|s| s == "/tmp/base.pp3").unwrap();
        let photo_at = strs.iter().position(|s| s == "/tmp/photo.pp3").unwrap();
        assert!(base_at < photo_at, "base profile must be applied first");

        // -Y overwrites without prompting; without it the CLI blocks on stdin.
        assert!(strs.contains(&"-Y".to_string()));
        // -t -b16 is the 16-bit TIFF the look stage needs.
        assert!(strs.contains(&"-t".to_string()));
        assert!(strs.contains(&"-b16".to_string()));
        // -d would read the user's GUI default profile (spec §4). Never.
        assert!(!strs.contains(&"-d".to_string()));
    }

    /// A missing binary must surface as a typed error naming the file, not a
    /// panic and not a silent skip.
    #[test]
    fn missing_binary_is_a_typed_error() {
        let r = Pp3Renderer::new(&cfg_with("/nonexistent/rawtherapee-cli"));
        let err = r.probe().expect_err("probe should fail");
        assert!(matches!(err, DevelopError::Render { .. }), "got {err:?}");
    }

    /// A binary that runs but is not RawTherapee must be rejected. Guards the
    /// exit-status trap from the other side: since probe() cannot gate on the
    /// status (RawTherapee 5.13 exits 2 on --version), the version banner is
    /// the only success signal, so a successful non-RawTherapee binary must
    /// still fail.
    #[test]
    fn a_non_rawtherapee_binary_is_rejected() {
        let exe = if cfg!(windows) { "cmd" } else { "true" };
        let r = Pp3Renderer::new(&cfg_with(exe));
        assert!(r.probe().is_err(), "`{exe}` must not pass as RawTherapee");
    }
}
```

- [ ] **Step 2: Run them to confirm they fail**

Run: `cargo test -p pipeline --lib develop::render`
Expected: FAIL — module not declared.

- [ ] **Step 3: Declare the module**

In `crates/pipeline/src/develop/mod.rs`:

```rust
pub mod render;
```

- [ ] **Step 4: Implement the renderer**

Prepend to `crates/pipeline/src/develop/render.rs`:

```rust
//! The RawTherapee backend: writes a `.pp3` pair and drives `rawtherapee-cli`
//! as a subprocess to a 16-bit sRGB TIFF.
//!
//! photopipe never links against RawTherapee, only executes it, so its GPL-3
//! licence does not propagate (spec §3).

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::config::DevelopConfig;
use crate::develop::decide::EditRecipe;
use crate::develop::pp3::{emit_pp3, BASE_PP3};
use crate::develop::DevelopError;

/// Stored in `edits.renderer`.
pub const RENDERER_NAME: &str = "rawtherapee";

/// A completed baseline render.
///
/// Owns a private scratch directory. Every render gets its own, so two photos
/// that happen to share a filename stem cannot overwrite each other's
/// intermediates — a real hazard because the orchestrator hands the same parent
/// temp directory to every call. The directory is removed when this struct is
/// dropped, which is also what deletes the very large TIFF.
pub struct RenderedTiff {
    /// 16-bit sRGB TIFF in the temp directory. Large — delete it as soon as the
    /// JPEG is encoded (a 60MP raw is roughly 350 MB here).
    pub tiff: PathBuf,
    /// The per-photo profile, kept so it can be copied next to the output JPEG
    /// as an escape hatch for reopening the photo in RawTherapee.
    pub pp3: PathBuf,
    /// Held for its `Drop`: removes the scratch directory and everything in it.
    /// Never read directly.
    _scratch: tempfile::TempDir,
}

pub struct Pp3Renderer {
    pub(crate) exe: PathBuf,
}

impl Pp3Renderer {
    pub fn new(cfg: &DevelopConfig) -> Self {
        let exe = if cfg.rawtherapee_path.is_empty() {
            // Bare name: let the OS search PATH.
            PathBuf::from("rawtherapee-cli")
        } else {
            crate::config::expand_tilde(Path::new(&cfg.rawtherapee_path))
        };
        Self { exe }
    }

    /// Confirm the binary exists and runs. Called once before a run rather than
    /// per photo, so a missing dependency fails immediately instead of
    /// producing hundreds of identical per-file warnings.
    ///
    /// **Do not gate on the exit status here.** Verified against RawTherapee
    /// 5.13: `--version` exits 2 and `-h` exits 255, while a real render exits
    /// 0. Treating a non-zero status as failure would make `probe()` fail on
    /// every machine and abort `finish` unconditionally. The presence of a
    /// parseable version banner is the actual success signal, and it arrives on
    /// stdout or stderr depending on build.
    pub fn probe(&self) -> Result<String, DevelopError> {
        let out = Command::new(&self.exe)
            .arg("--version")
            .output()
            .map_err(|e| DevelopError::Render {
                path: self.exe.clone(),
                reason: format!("cannot execute: {e}"),
            })?;
        let banner = [&out.stdout, &out.stderr]
            .into_iter()
            .filter_map(|buf| {
                String::from_utf8_lossy(buf)
                    .lines()
                    .find(|l| l.contains("RawTherapee"))
                    .map(|l| l.trim().to_string())
            })
            .next();
        banner.ok_or_else(|| DevelopError::Render {
            path: self.exe.clone(),
            reason: "ran, but printed no RawTherapee version banner".into(),
        })
    }

    /// Render `raw` through `recipe`, using a fresh scratch directory created
    /// inside `tmp_dir`.
    ///
    /// Writes both profiles into that scratch directory — never beside the
    /// original, which would violate the non-destructive contract. RawTherapee's
    /// own convention is to write `photo.raw.pp3` next to the source; we
    /// deliberately do not.
    ///
    /// The per-call scratch directory matters: output names are derived from the
    /// input's filename stem, and the orchestrator passes one shared `tmp_dir`
    /// for the whole run, so two photos with the same stem — different folders,
    /// or two unnamed inputs both falling back to `"photo"` — would otherwise
    /// silently overwrite each other's TIFF and profile. The second would then
    /// look like it had rendered successfully against the wrong recipe.
    pub fn render(
        &self,
        raw: &Path,
        recipe: &EditRecipe,
        tmp_dir: &Path,
    ) -> Result<RenderedTiff, DevelopError> {
        let stem = raw
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("photo")
            .to_string();

        // A fresh directory per render, so stem collisions are impossible by
        // construction rather than by convention.
        let scratch = tempfile::TempDir::new_in(tmp_dir).map_err(|source| DevelopError::Io {
            path: tmp_dir.to_path_buf(),
            source,
        })?;
        let scratch_dir = scratch.path().to_path_buf();

        let base_path = scratch_dir.join("base.pp3");
        let photo_path = scratch_dir.join(format!("{stem}.pp3"));
        write_file(&base_path, BASE_PP3)?;
        write_file(&photo_path, &emit_pp3(recipe))?;

        let args = build_args(&base_path, &photo_path, &scratch_dir, raw);
        let out = Command::new(&self.exe)
            .args(&args)
            .output()
            .map_err(|e| DevelopError::Render {
                path: raw.to_path_buf(),
                reason: format!("cannot execute {}: {e}", self.exe.display()),
            })?;

        if !out.status.success() {
            return Err(DevelopError::Render {
                path: raw.to_path_buf(),
                reason: format!(
                    "exited {}: {}",
                    out.status,
                    String::from_utf8_lossy(&out.stderr).trim()
                ),
            });
        }

        // RawTherapee derives the output name from the input stem.
        let tiff = scratch_dir.join(format!("{stem}.tif"));
        if !tiff.exists() {
            return Err(DevelopError::Render {
                path: raw.to_path_buf(),
                reason: format!("expected output {} was not created", tiff.display()),
            });
        }
        Ok(RenderedTiff {
            tiff,
            pp3: photo_path,
            _scratch: scratch,
        })
    }
}

/// Build the argument vector.
///
/// Split out as a free function so the ordering contract is unit-testable
/// without a RawTherapee installation. `OsString` throughout, because a photo
/// path is not guaranteed to be valid UTF-8 on any platform.
fn build_args(base_pp3: &Path, photo_pp3: &Path, out_dir: &Path, input: &Path) -> Vec<OsString> {
    vec![
        // Overwrite without prompting; otherwise the CLI blocks on stdin.
        OsString::from("-Y"),
        // 16-bit TIFF: the domain the look stage operates in.
        OsString::from("-t"),
        OsString::from("-b16"),
        // Profiles stack in order, so the per-photo one wins.
        OsString::from("-p"),
        base_pp3.as_os_str().to_owned(),
        OsString::from("-p"),
        photo_pp3.as_os_str().to_owned(),
        OsString::from("-o"),
        out_dir.as_os_str().to_owned(),
        // -c must be last: everything after it is treated as input.
        OsString::from("-c"),
        input.as_os_str().to_owned(),
    ]
}

fn write_file(path: &Path, contents: &str) -> Result<(), DevelopError> {
    std::fs::write(path, contents).map_err(|source| DevelopError::Io {
        path: path.to_path_buf(),
        source,
    })
}
```

- [ ] **Step 5: Run the tests**

Run: `cargo test -p pipeline --lib develop::render`
Expected: PASS, 4 tests.

- [ ] **Step 6: Verify green and commit**

```bash
cargo fmt && cargo clippy --all-targets --all-features -- -D warnings && cargo test --all
git add crates/pipeline/src/develop
git commit -m "feat(develop): rawtherapee-cli renderer behind the EditRecipe contract"
```

---

### Task 10: `edits` persistence and the idempotency check

Idempotency is a correctness requirement in this project, not a performance
goal. This task is where it lives.

**Files:**
- Modify: `crates/pipeline/src/catalog/develop.rs`
- Test: `crates/pipeline/tests/develop.rs`

**Interfaces:**
- Consumes: `Catalog`, `EditRecipe` (Task 6), `KeeperToDevelop` (Task 5)
- Produces:
  - `pipeline::catalog::EditRow { file_id, content_hash, recipe, recipe_hash, decider_version, renderer, look_model: Option<String>, look_version: Option<String>, lut_hash: Option<String>, look_applied: bool, iqa_before: Option<f32>, iqa_after: Option<f32>, output_path: Option<String>, output_size_bytes: Option<i64>, rendered_at: i64 }`
  - `pipeline::catalog::EditIdentity { content_hash, recipe_hash, decider_version, renderer, look_model, look_version }`
  - `Catalog::upsert_edit(&self, row: &EditRow) -> Result<(), CatalogError>`
  - `Catalog::edit_identity(&self, file_id: i64) -> Result<Option<(EditIdentity, Option<String>, Option<i64>)>, CatalogError>`
  - `pipeline::develop::is_up_to_date(existing, wanted, ...) -> bool`

- [ ] **Step 1: Write the failing tests**

Append to `crates/pipeline/tests/develop.rs`:

```rust
use pipeline::catalog::{EditIdentity, EditRow};
use pipeline::develop::decide::{EditRecipe, DECIDER_VERSION};
use pipeline::develop::is_up_to_date;

fn sample_recipe() -> EditRecipe {
    EditRecipe {
        exposure_ev: 0.5,
        highlight_recovery: 0.2,
        shadow_lift: 0.1,
        denoise_luma: 0.0,
        denoise_chroma: 0.0,
        sharpen_amount: 0.4,
        lens_correct: true,
    }
}

fn sample_edit(file_id: i64, out: &std::path::Path, size: i64) -> EditRow {
    let recipe = sample_recipe();
    EditRow {
        file_id,
        content_hash: "hash-a".into(),
        recipe_hash: recipe.recipe_hash(),
        recipe,
        decider_version: DECIDER_VERSION.into(),
        renderer: "rawtherapee".into(),
        look_model: None,
        look_version: None,
        lut_hash: None,
        look_applied: false,
        iqa_before: None,
        iqa_after: None,
        output_path: Some(out.display().to_string()),
        output_size_bytes: Some(size),
        rendered_at: 1_700_000_000,
    }
}

#[test]
fn edit_round_trips() {
    let (_dir, cat) = temp_catalog();
    let id = seed_file(&cat, "/tmp/a.arw", "keep");
    let out = std::path::PathBuf::from("/tmp/out/a.jpg");
    cat.upsert_edit(&sample_edit(id, &out, 4096)).unwrap();
    let (identity, path, size) = cat.edit_identity(id).unwrap().expect("row should exist");
    assert_eq!(identity.content_hash, "hash-a");
    assert_eq!(identity.decider_version, DECIDER_VERSION);
    assert_eq!(identity.renderer, "rawtherapee");
    assert_eq!(identity.look_model, None);
    assert_eq!(path.as_deref(), Some("/tmp/out/a.jpg"));
    assert_eq!(size, Some(4096));
}

#[test]
fn edit_upsert_overwrites() {
    let (_dir, cat) = temp_catalog();
    let id = seed_file(&cat, "/tmp/a.arw", "keep");
    let out = std::path::PathBuf::from("/tmp/out/a.jpg");
    cat.upsert_edit(&sample_edit(id, &out, 4096)).unwrap();
    let mut second = sample_edit(id, &out, 8192);
    second.look_applied = true;
    cat.upsert_edit(&second).unwrap();
    let (_, _, size) = cat.edit_identity(id).unwrap().unwrap();
    assert_eq!(size, Some(8192));
}

#[test]
fn no_edit_row_means_not_up_to_date() {
    let (_dir, cat) = temp_catalog();
    let id = seed_file(&cat, "/tmp/a.arw", "keep");
    assert!(cat.edit_identity(id).unwrap().is_none());
}
```

And a set of pure tests for the predicate itself — no database needed:

```rust
fn identity(recipe_hash: &str) -> EditIdentity {
    EditIdentity {
        content_hash: "hash-a".into(),
        recipe_hash: recipe_hash.into(),
        decider_version: DECIDER_VERSION.into(),
        renderer: "rawtherapee".into(),
        look_model: None,
        look_version: None,
    }
}

#[test]
fn identical_identity_with_present_output_is_up_to_date() {
    let dir = tempfile::TempDir::new().unwrap();
    let out = dir.path().join("a.jpg");
    std::fs::write(&out, vec![0u8; 100]).unwrap();
    assert!(is_up_to_date(&identity("r1"), &identity("r1"), Some(&out), Some(100)));
}

/// Any identity component changing forces a re-render. A tuning change must
/// not leave stale JPEGs behind.
#[test]
fn each_identity_component_forces_a_rerender() {
    let dir = tempfile::TempDir::new().unwrap();
    let out = dir.path().join("a.jpg");
    std::fs::write(&out, vec![0u8; 100]).unwrap();
    let base = identity("r1");

    let mut recipe_changed = base.clone();
    recipe_changed.recipe_hash = "r2".into();
    assert!(!is_up_to_date(&base, &recipe_changed, Some(&out), Some(100)));

    let mut decider_changed = base.clone();
    decider_changed.decider_version = "some-other-decider".into();
    assert!(!is_up_to_date(&base, &decider_changed, Some(&out), Some(100)));

    let mut content_changed = base.clone();
    content_changed.content_hash = "hash-b".into();
    assert!(!is_up_to_date(&base, &content_changed, Some(&out), Some(100)));

    let mut look_changed = base.clone();
    look_changed.look_model = Some("lut3d-fivek".into());
    assert!(!is_up_to_date(&base, &look_changed, Some(&out), Some(100)));
}

/// A deleted or truncated output must be rebuilt even when the identity matches.
#[test]
fn missing_or_resized_output_forces_a_rerender() {
    let dir = tempfile::TempDir::new().unwrap();
    let out = dir.path().join("a.jpg");
    assert!(!is_up_to_date(&identity("r1"), &identity("r1"), Some(&out), Some(100)));

    std::fs::write(&out, vec![0u8; 50]).unwrap();
    assert!(!is_up_to_date(&identity("r1"), &identity("r1"), Some(&out), Some(100)));

    // Never rendered before: no recorded path at all.
    assert!(!is_up_to_date(&identity("r1"), &identity("r1"), None, None));
}
```

- [ ] **Step 2: Run them to confirm they fail**

Run: `cargo test -p pipeline --test develop`
Expected: FAIL — `EditRow` and `is_up_to_date` do not exist.

- [ ] **Step 3: Add the row types and persistence**

Append to `crates/pipeline/src/catalog/develop.rs`:

```rust
use crate::develop::decide::EditRecipe;

/// A complete `edits` row. Doubles as the audit record: for any finished JPEG
/// it answers which recipe and model version produced it and whether the look
/// survived the quality guard.
#[derive(Debug, Clone)]
pub struct EditRow {
    pub file_id: i64,
    /// Denormalised from `files` so the row survives moves and renames.
    pub content_hash: String,
    pub recipe: EditRecipe,
    pub recipe_hash: String,
    pub decider_version: String,
    pub renderer: String,
    pub look_model: Option<String>,
    pub look_version: Option<String>,
    /// Retained even when `look_applied` is false, so a rejected look is still
    /// reproducible for inspection.
    pub lut_hash: Option<String>,
    pub look_applied: bool,
    pub iqa_before: Option<f32>,
    pub iqa_after: Option<f32>,
    pub output_path: Option<String>,
    pub output_size_bytes: Option<i64>,
    pub rendered_at: i64,
}

/// The subset of an `edits` row that decides whether a re-render is needed.
#[derive(Debug, Clone, PartialEq)]
pub struct EditIdentity {
    pub content_hash: String,
    pub recipe_hash: String,
    pub decider_version: String,
    pub renderer: String,
    pub look_model: Option<String>,
    pub look_version: Option<String>,
}

impl Catalog {
    pub fn upsert_edit(&self, row: &EditRow) -> Result<(), CatalogError> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| CatalogError::Db("mutex poisoned".into()))?;
        let r = &row.recipe;
        conn.execute(
            "INSERT INTO edits
                (file_id, content_hash, exposure_ev,
                 highlight_recovery, shadow_lift, denoise_luma, denoise_chroma,
                 sharpen_amount, lens_correct, recipe_hash, decider_version,
                 renderer, look_model, look_version, lut_hash, look_applied,
                 iqa_before, iqa_after, output_path, output_size_bytes, rendered_at)
             VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)
             ON CONFLICT (file_id) DO UPDATE SET
                 content_hash = excluded.content_hash,
                 exposure_ev = excluded.exposure_ev,
                 highlight_recovery = excluded.highlight_recovery,
                 shadow_lift = excluded.shadow_lift,
                 denoise_luma = excluded.denoise_luma,
                 denoise_chroma = excluded.denoise_chroma,
                 sharpen_amount = excluded.sharpen_amount,
                 lens_correct = excluded.lens_correct,
                 recipe_hash = excluded.recipe_hash,
                 decider_version = excluded.decider_version,
                 renderer = excluded.renderer,
                 look_model = excluded.look_model,
                 look_version = excluded.look_version,
                 lut_hash = excluded.lut_hash,
                 look_applied = excluded.look_applied,
                 iqa_before = excluded.iqa_before,
                 iqa_after = excluded.iqa_after,
                 output_path = excluded.output_path,
                 output_size_bytes = excluded.output_size_bytes,
                 rendered_at = excluded.rendered_at",
            duckdb::params![
                row.file_id, row.content_hash, r.exposure_ev,
                r.highlight_recovery, r.shadow_lift, r.denoise_luma, r.denoise_chroma,
                r.sharpen_amount, r.lens_correct, row.recipe_hash, row.decider_version,
                row.renderer, row.look_model, row.look_version, row.lut_hash,
                row.look_applied, row.iqa_before, row.iqa_after, row.output_path,
                row.output_size_bytes, row.rendered_at
            ],
        )
        .map_err(|e| CatalogError::Db(e.to_string()))?;
        Ok(())
    }

    /// The identity of the last render plus where it landed and how big it was.
    #[allow(clippy::type_complexity)]
    pub fn edit_identity(
        &self,
        file_id: i64,
    ) -> Result<Option<(EditIdentity, Option<String>, Option<i64>)>, CatalogError> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| CatalogError::Db("mutex poisoned".into()))?;
        let row = conn.query_row(
            "SELECT content_hash, recipe_hash, decider_version, renderer,
                    look_model, look_version, output_path, output_size_bytes
             FROM edits WHERE file_id = ?",
            duckdb::params![file_id],
            |r| {
                Ok((
                    EditIdentity {
                        content_hash: r.get(0)?,
                        recipe_hash: r.get(1)?,
                        decider_version: r.get(2)?,
                        renderer: r.get(3)?,
                        look_model: r.get(4)?,
                        look_version: r.get(5)?,
                    },
                    r.get(6)?,
                    r.get(7)?,
                ))
            },
        );
        optional_row(row)
    }
}
```

Export the new types from `crates/pipeline/src/catalog/mod.rs`:

```rust
pub use develop::{EditIdentity, EditRow, KeeperToDevelop};
```

- [ ] **Step 4: Implement the idempotency predicate**

Append to `crates/pipeline/src/develop/mod.rs`:

```rust
use std::path::Path;

use crate::catalog::EditIdentity;

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
```

- [ ] **Step 5: Run the tests**

Run: `cargo test -p pipeline --test develop`
Expected: PASS, 13 tests.

- [ ] **Step 6: Verify green and commit**

```bash
cargo fmt && cargo clippy --all-targets --all-features -- -D warnings && cargo test --all
git add crates/pipeline/src/catalog crates/pipeline/src/develop crates/pipeline/tests/develop.rs
git commit -m "feat(catalog): edits persistence and the render idempotency check"
```

---

### Task 11: `finish_folder()` orchestration

The one place the stages meet. Keep it thin — measurement, decision, rendering
and persistence all already exist; this task only sequences them, isolates
failures, and reports progress.

**Files:**
- Modify: `crates/pipeline/src/develop/mod.rs`
- Modify: `crates/pipeline/src/catalog/develop.rs` (add `develop_inputs`)
- Modify: `crates/pipeline/src/lib.rs` (re-exports)
- Test: `crates/pipeline/tests/develop.rs`

**Interfaces:**
- Consumes: everything from Tasks 4–10, `ProgressSink` (`crates/pipeline/src/analyze.rs:22`), `Catalog`, `DevelopConfig`
- Produces:
  - `Catalog::develop_inputs(&self, file_id: i64) -> Result<(ExifData, f32), CatalogError>`
  - `pipeline::develop::FinishReport { rendered: u64, skipped: u64, errored: u64 }`
  - `pipeline::develop::finish_folder(catalog: &Catalog, cfg: &DevelopConfig, out_dir: &Path, progress: &dyn ProgressSink) -> anyhow::Result<FinishReport>`
  - Re-exported from `lib.rs` as `pipeline::{finish_folder, FinishReport}`

- [ ] **Step 1: Write the failing tests**

Append to `crates/pipeline/tests/develop.rs`. These exercise the orchestration
paths that do **not** need RawTherapee: an empty work list, and failure
isolation when the renderer is missing.

```rust
use pipeline::config::DevelopConfig;
use pipeline::develop::{finish_folder, FinishReport};
use pipeline::ProgressSink;

/// A sink that records what it was told, so stage order is assertable.
#[derive(Default)]
struct RecordingSink {
    stages: std::sync::Mutex<Vec<String>>,
}

impl ProgressSink for RecordingSink {
    fn stage(&self, stage: &str) {
        self.stages.lock().unwrap().push(stage.to_string());
    }
    fn set_total(&self, _total: u64) {}
    fn inc(&self) {}
}

#[test]
fn empty_work_list_renders_nothing_and_still_reports_done() {
    let (_dir, cat) = temp_catalog();
    let out = tempfile::TempDir::new().unwrap();
    let sink = RecordingSink::default();
    let report = finish_folder(&cat, &DevelopConfig::default(), out.path(), &sink).unwrap();
    assert_eq!(report.rendered, 0);
    assert_eq!(report.errored, 0);
    let stages = sink.stages.lock().unwrap().clone();
    assert_eq!(stages.last().map(String::as_str), Some("done"));
}

/// A missing renderer must fail the run up front with one clear error, not
/// produce one warning per photo deep into a long run.
#[test]
fn missing_renderer_fails_before_any_work() {
    let (_dir, cat) = temp_catalog();
    seed_file(&cat, "/tmp/a.arw", "keep");
    let out = tempfile::TempDir::new().unwrap();
    let cfg = DevelopConfig {
        rawtherapee_path: "/nonexistent/rawtherapee-cli".into(),
        ..Default::default()
    };
    let err = finish_folder(&cat, &cfg, out.path(), &RecordingSink::default())
        .expect_err("a missing renderer should abort the run");
    assert!(
        err.to_string().contains("rawtherapee"),
        "error should name the missing dependency: {err}"
    );
}

/// One unreadable raw must not abort the run, and must leave no edits row —
/// a half-recorded render would make the next run believe it succeeded.
///
/// Needs a real RawTherapee: `probe()` authenticates the binary by its version
/// banner (see Task 9), so a stand-in like `true` cannot get us past it to the
/// per-file path this test is about. Gated, and skips cleanly when absent.
#[test]
fn unreadable_raw_is_skipped_without_an_edits_row() {
    let Some(rt) = std::env::var_os("PHOTOPIPE_TEST_RAWTHERAPEE") else {
        eprintln!("skipping: set PHOTOPIPE_TEST_RAWTHERAPEE to the rawtherapee-cli path");
        return;
    };
    let (_dir, cat) = temp_catalog();
    let id = seed_file(&cat, "/tmp/definitely-missing.arw", "keep");
    let out = tempfile::TempDir::new().unwrap();
    let cfg = DevelopConfig {
        rawtherapee_path: rt.to_string_lossy().into_owned(),
        ..Default::default()
    };
    let report = finish_folder(&cat, &cfg, out.path(), &RecordingSink::default()).unwrap();
    assert_eq!(report.errored, 1);
    assert_eq!(report.rendered, 0);
    assert!(cat.edit_identity(id).unwrap().is_none(), "no row on failure");
}
```

- [ ] **Step 2: Run them to confirm they fail**

Run: `cargo test -p pipeline --test develop`
Expected: FAIL — `finish_folder` does not exist.

- [ ] **Step 3: Add the decide-inputs query**

Append to `crates/pipeline/src/catalog/develop.rs`:

```rust
use crate::ingest::exif::ExifData;

impl Catalog {
    /// EXIF plus global sharpness for one file — the two non-raw inputs to
    /// `decide()`. Missing rows are not an error: `decide()` has a defined
    /// answer for absent ISO and absent sharpness.
    pub fn develop_inputs(&self, file_id: i64) -> Result<(ExifData, f32), CatalogError> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| CatalogError::Db("mutex poisoned".into()))?;
        let row = conn.query_row(
            "SELECT e.iso, e.lens_model, e.camera_model, s.s_global
             FROM files f
             LEFT JOIN exif e ON e.file_id = f.id
             LEFT JOIN sharpness s ON s.file_id = f.id
             WHERE f.id = ?",
            duckdb::params![file_id],
            |r| {
                let iso: Option<i32> = r.get(0)?;
                let lens_model: Option<String> = r.get(1)?;
                let camera_model: Option<String> = r.get(2)?;
                let s_global: Option<f32> = r.get(3)?;
                Ok((
                    ExifData {
                        iso: iso.map(|v| v as u32),
                        lens_model,
                        camera_model,
                        ..Default::default()
                    },
                    s_global.unwrap_or(0.5),
                ))
            },
        );
        Ok(optional_row(row)?.unwrap_or((ExifData::default(), 0.5)))
    }
}
```

- [ ] **Step 4: Implement the orchestrator**

Append to `crates/pipeline/src/develop/mod.rs`:

```rust
use anyhow::Context;

use crate::analyze::ProgressSink;
use crate::catalog::{Catalog, EditIdentity, EditRow};
use crate::config::{DevelopConfig, OutputSubdirs};
use crate::develop::decide::{decide, Sharpness, DECIDER_VERSION};
use crate::develop::render::{Pp3Renderer, RENDERER_NAME};

/// Summary of a `finish` run.
#[derive(Debug, Clone, Default)]
pub struct FinishReport {
    pub rendered: u64,
    pub skipped: u64,
    pub errored: u64,
}

/// Develop every kept photo into `out_dir`.
///
/// Deliberately serial. Unlike `scan`, this stage must not fan out with rayon:
/// `rawtherapee-cli` exposes no thread flag and saturates all cores internally,
/// and a 16-bit TIFF of a 60MP raw is roughly 350 MB, so several in flight at
/// once would thrash memory for no throughput gain (spec §8).
pub fn finish_folder(
    catalog: &Catalog,
    cfg: &DevelopConfig,
    out_dir: &Path,
    progress: &dyn ProgressSink,
) -> anyhow::Result<FinishReport> {
    let renderer = Pp3Renderer::new(cfg);
    // Probe once, before any work. A missing dependency should fail the run
    // immediately rather than produce one identical warning per photo.
    let version = renderer.probe().with_context(|| {
        format!(
            "rawtherapee-cli is required by `photopipe finish` but could not be run. \
             Install RawTherapee and set [develop] rawtherapee_path, then check \
             `photopipe doctor`"
        )
    })?;
    tracing::info!(renderer = %version, "renderer ready");

    let work = catalog.keepers_to_develop()?;
    tracing::info!(count = work.len(), "keepers to develop");

    let mut report = FinishReport::default();

    progress.stage("measuring");
    progress.set_total(work.len() as u64);

    // One temp dir for the whole run; each intermediate is deleted as soon as
    // its JPEG is encoded, so peak disk stays at roughly one TIFF.
    let tmp = tempfile::TempDir::new().context("cannot create temp render directory")?;

    for item in &work {
        match finish_one(catalog, cfg, &renderer, out_dir, tmp.path(), item) {
            Ok(Outcome::Rendered) => report.rendered += 1,
            Ok(Outcome::Skipped) => report.skipped += 1,
            Err(e) => {
                // One corrupt file must never abort a full run.
                tracing::warn!(path = %item.path.display(), error = %e, "develop failed; skipping");
                report.errored += 1;
            }
        }
        progress.inc();
    }

    progress.stage("done");
    tracing::info!(
        rendered = report.rendered,
        skipped = report.skipped,
        errored = report.errored,
        "finish complete"
    );
    Ok(report)
}

enum Outcome {
    Rendered,
    Skipped,
}

fn finish_one(
    catalog: &Catalog,
    cfg: &DevelopConfig,
    renderer: &Pp3Renderer,
    out_dir: &Path,
    tmp_dir: &Path,
    item: &crate::catalog::KeeperToDevelop,
) -> anyhow::Result<Outcome> {
    // ① measure
    let stats = crate::develop::measure::measure_raw(&item.path)?;
    catalog.upsert_raw_stats(item.file_id, &stats)?;

    // ② decide
    let (exif, s_global) = catalog.develop_inputs(item.file_id)?;
    let recipe = decide(&stats, &exif, &Sharpness { s_global });
    let recipe_hash = recipe.recipe_hash();

    let wanted = EditIdentity {
        content_hash: item.content_hash.clone(),
        recipe_hash: recipe_hash.clone(),
        decider_version: DECIDER_VERSION.into(),
        renderer: RENDERER_NAME.into(),
        // Phase 2 fills these in; baseline-only renders carry no look.
        look_model: None,
        look_version: None,
    };

    let dest = output_path_for(cfg, out_dir, item);

    // Idempotency: skip when the recorded render still satisfies what we want.
    if let Some((existing, path, size)) = catalog.edit_identity(item.file_id)? {
        if is_up_to_date(
            &existing,
            &wanted,
            path.as_deref().map(Path::new),
            size,
        ) {
            tracing::debug!(path = %item.path.display(), "already finished; skipping");
            return Ok(Outcome::Skipped);
        }
    }

    // ③ baseline render
    let rendered = renderer.render(&item.path, &recipe, tmp_dir)?;

    // ④ encode. Phase 2 inserts the look between these two steps.
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("cannot create {}", parent.display()))?;
    }
    let img = image::open(&rendered.tiff)
        .with_context(|| format!("cannot read rendered TIFF {}", rendered.tiff.display()))?;
    encode_jpeg(&img, &dest, cfg.jpeg_quality)?;

    // The .pp3 sits beside the JPEG as an escape hatch for reopening the photo
    // in RawTherapee. Never beside the original raw. Copy it before `rendered`
    // is dropped, since the drop removes the scratch directory.
    let pp3_dest = dest.with_extension("pp3");
    let _ = std::fs::copy(&rendered.pp3, &pp3_dest);

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
        look_model: None,
        look_version: None,
        lut_hash: None,
        look_applied: false,
        iqa_before: None,
        iqa_after: None,
        output_path: Some(dest.display().to_string()),
        output_size_bytes: size,
        rendered_at: now_secs(),
    })?;

    Ok(Outcome::Rendered)
}

/// Where one photo's JPEG lands.
fn output_path_for(
    cfg: &DevelopConfig,
    out_dir: &Path,
    item: &crate::catalog::KeeperToDevelop,
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
    dir.join(format!("{stem}.jpg"))
}

fn encode_jpeg(img: &image::DynamicImage, dest: &Path, quality: u8) -> anyhow::Result<()> {
    let file = std::fs::File::create(dest)
        .with_context(|| format!("cannot create {}", dest.display()))?;
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
```

- [ ] **Step 5: Confirm `tempfile` is a real dependency of `pipeline`**

`finish_folder` uses `tempfile::TempDir` at runtime, not just in tests. **Task 9
already moved it** from `[dev-dependencies]` to `[dependencies]`, because
`Pp3Renderer::render` needs it for its per-call scratch directory. Verify and
move on:

```bash
grep -n -A20 '^\[dependencies\]' crates/pipeline/Cargo.toml | grep tempfile
```

Expected: a line is printed. If not, add `tempfile = { workspace = true }` under
`[dependencies]`.

- [ ] **Step 6: Re-export from `lib.rs`**

In `crates/pipeline/src/lib.rs`, add alongside the other re-exports:

```rust
pub use develop::{finish_folder, is_up_to_date, FinishReport};
```

- [ ] **Step 7: Run the tests**

Run: `cargo test -p pipeline --test develop`
Expected: PASS, 16 tests.

- [ ] **Step 8: Verify green and commit**

```bash
cargo fmt && cargo clippy --all-targets --all-features -- -D warnings && cargo test --all
git add crates/pipeline
git commit -m "feat(develop): finish_folder orchestration with progress and failure isolation"
```

---

### Task 12: The `finish` command, docs, and the end-to-end test

Closes Phase 1. After this task the feature is usable.

**Files:**
- Modify: `crates/cli/src/main.rs`
- Modify: `README.md`
- Test: `crates/cli/tests/cli.rs`, `crates/pipeline/tests/develop.rs`

**Interfaces:**
- Consumes: `finish_folder`, `FinishReport` (Task 11), `require_library` (existing helper in `crates/cli/src/main.rs`)
- Produces: `photopipe finish <folder> [--out <dir>]`

- [ ] **Step 1: Write the failing CLI test**

Append to `crates/cli/tests/cli.rs`, following the shape of the tests already
there:

```rust
#[test]
fn finish_requires_an_analyzed_library() {
    let dir = tempfile::TempDir::new().unwrap();
    let out = assert_photopipe(&["finish", dir.path().to_str().unwrap()]);
    assert!(
        !out.status.success(),
        "finish on an unscanned folder should fail"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("scan") || stderr.contains("library"),
        "the error should tell the user to scan first: {stderr}"
    );
}
```

If `assert_photopipe` is not the existing helper's name, use whichever helper
`crates/cli/tests/cli.rs` already uses to invoke the binary — do not add a
second one.

- [ ] **Step 2: Run it to confirm it fails**

Run: `cargo test -p photopipe --test cli finish_requires_an_analyzed_library`
Expected: FAIL — `error: unrecognized subcommand 'finish'`.

- [ ] **Step 3: Add the subcommand**

In `crates/cli/src/main.rs`, add to `enum Command` after `ExportKeepers`:

```rust
    /// Develop every kept photo into finished JPEGs.
    ///
    /// Reads `decisions.verdict = 'keep'`, applies analytic technical
    /// corrections through RawTherapee, and writes a tree of JPEGs. Runs
    /// entirely without per-photo input. Requires `rawtherapee-cli` — check
    /// `photopipe doctor` first.
    Finish {
        /// Folder whose library to develop.
        folder: PathBuf,
        /// Destination directory. Overrides `[develop] finished_dir`.
        #[arg(long)]
        out: Option<PathBuf>,
    },
```

Add to the `match cli.command` block:

```rust
        Command::Finish { folder, out } => cmd_finish(&folder, out, &cfg, &roots),
```

Add the handler next to `cmd_export_keepers`:

```rust
fn cmd_finish(
    folder: &std::path::Path,
    out: Option<PathBuf>,
    cfg: &config::Config,
    roots: &LibraryRoots,
) -> Result<()> {
    let lib = require_library(roots, folder)?;

    // The CLI argument wins, so a one-off export never needs a config edit.
    let out_dir = match out {
        Some(p) => config::expand_tilde(&p),
        None => config::expand_tilde(&PathBuf::from(
            cfg.develop
                .finished_dir
                .replace("<library>", &lib.folder.to_string_lossy()),
        )),
    };

    println!("Developing keepers → {} …", out_dir.display());

    let report = pipeline::finish_folder(&lib.catalog, &cfg.develop, &out_dir, &CliProgress)?;

    println!(
        "Finished {} photos, {} already current, {} failed → {}",
        report.rendered,
        report.skipped,
        report.errored,
        out_dir.display()
    );
    if report.errored > 0 {
        println!("Re-run with --log-level debug to see why individual files failed.");
    }
    Ok(())
}

/// Terminal progress sink. A future Develop screen in `serve` passes the
/// server's job sink to the same `finish_folder` signature instead.
struct CliProgress;

impl pipeline::ProgressSink for CliProgress {
    fn stage(&self, stage: &str) {
        tracing::info!(stage, "finish");
    }
    fn set_total(&self, _total: u64) {}
    fn inc(&self) {}
}
```

Check whether `crates/cli/src/main.rs` already defines a progress sink for
another command. If it does, reuse it rather than adding `CliProgress`.

- [ ] **Step 4: Run the CLI test**

Run: `cargo test -p photopipe --test cli`
Expected: PASS.

- [ ] **Step 5: Write the gated end-to-end test**

Append to `crates/pipeline/tests/develop.rs`. It must skip cleanly when
RawTherapee is absent so `cargo test --all` still passes on a bare machine:

```rust
/// Full pipeline against a real RAW file. Gated twice: on RawTherapee being
/// installed, and on a fixture RAW existing. Skips with a message rather than
/// failing, so a bare checkout still goes green.
#[test]
fn end_to_end_finish_is_idempotent() {
    let Some(rt) = std::env::var_os("PHOTOPIPE_TEST_RAWTHERAPEE") else {
        eprintln!("skipping: set PHOTOPIPE_TEST_RAWTHERAPEE to the rawtherapee-cli path");
        return;
    };
    let Some(raw) = std::env::var_os("PHOTOPIPE_TEST_RAW") else {
        eprintln!("skipping: set PHOTOPIPE_TEST_RAW to a real RAW file");
        return;
    };
    let raw = std::path::PathBuf::from(raw);

    let (_dir, cat) = temp_catalog();
    let id = {
        let conn = cat.raw_conn_for_test();
        conn.execute(
            "INSERT INTO files (path, content_hash, size_bytes, mtime_ns, file_format, last_processed)
             VALUES (?, 'e2e-hash', 0, 0, 'arw', 0)",
            duckdb::params![raw.to_string_lossy()],
        )
        .unwrap();
        let id: i64 = conn
            .query_row("SELECT id FROM files WHERE content_hash = 'e2e-hash'", [], |r| r.get(0))
            .unwrap();
        conn.execute(
            "INSERT INTO decisions (file_id, verdict, is_keeper, note, decided_at)
             VALUES (?, 'keep', false, NULL, 0)",
            duckdb::params![id],
        )
        .unwrap();
        id
    };

    let out = tempfile::TempDir::new().unwrap();
    let cfg = DevelopConfig {
        rawtherapee_path: rt.to_string_lossy().into_owned(),
        ..Default::default()
    };

    let first = finish_folder(&cat, &cfg, out.path(), &RecordingSink::default()).unwrap();
    assert_eq!(first.rendered, 1, "first run should render");
    assert_eq!(first.errored, 0);

    let (_, path, _) = cat.edit_identity(id).unwrap().unwrap();
    let jpeg = std::path::PathBuf::from(path.unwrap());
    assert!(jpeg.exists(), "the JPEG should exist at {}", jpeg.display());
    assert!(jpeg.with_extension("pp3").exists(), "the .pp3 escape hatch should sit beside it");

    // The original must be untouched: no sidecar written next to it.
    assert!(
        !raw.with_extension("pp3").exists(),
        "nothing may be written beside the original"
    );

    // Idempotency is a correctness requirement: the second run does zero work.
    let second = finish_folder(&cat, &cfg, out.path(), &RecordingSink::default()).unwrap();
    assert_eq!(second.rendered, 0, "second run must render nothing");
    assert_eq!(second.skipped, 1);
}
```

- [ ] **Step 6: Run the end-to-end test for real**

```bash
PHOTOPIPE_TEST_RAWTHERAPEE=/Applications/RawTherapee.app/Contents/MacOS/rawtherapee-cli \
PHOTOPIPE_TEST_RAW=$HOME/Photos/<some>.ARW \
cargo test -p pipeline --test develop end_to_end_finish_is_idempotent -- --nocapture
```

Expected: PASS. Then confirm it also passes **without** those variables set —
that is the bare-machine path.

- [ ] **Step 7: Update the README**

In `README.md`, add `finish` to the stage diagram:

```
scan ──> calibrate ──> dedupe ──> serve / review-tree ──> export-keepers
                                        │                      finish
```

and add a bullet after the `export-keepers` one:

```markdown
- **`finish`** — develop everything you kept into finished JPEGs, automatically.
  Reads the raw sensor data to decide exposure, white balance, highlight
  recovery, shadow lift, denoise and sharpening per photo, renders through
  RawTherapee, and writes a `_finished/YYYY-MM/` tree. Requires
  `rawtherapee-cli` on your machine — run `photopipe doctor` to check. Each JPEG
  gets a `.pp3` beside it so you can reopen the photo in RawTherapee and take
  over by hand.

  `finish` and `export-keepers` are independent: one gives you finished JPEGs,
  the other hands the untouched RAWs to Lightroom or a backup. Neither requires
  the other.
```

And to the quick start, after the export-keepers line:

```bash
# develop the keepers into finished JPEGs
photopipe finish ~/Photos/2024 --out ~/Photos/_finished
```

- [ ] **Step 8: Verify green and commit**

```bash
cargo fmt && cargo clippy --all-targets --all-features -- -D warnings && cargo test --all
git add crates/cli README.md crates/pipeline/tests/develop.rs
git commit -m "feat(cli): photopipe finish command, docs, and the gated end-to-end test"
```

---

## CHECKPOINT — baseline sign-off

**Do not start Task 13 until this passes.** Spec §13 is explicit about why: once
a look sits on top of the baseline, a bad `exposure_ev` and a bad LUT are
indistinguishable in the output. The whole reason the analytic layer comes first
is that it can be judged alone.

- [x] **Render a real shoot** — 2026-08-13, on the ILCE-6300 sample set (three frames; the Grindelwald library referenced below is no longer registered).

```bash
cargo build --release
./target/release/photopipe finish ~/Photos/Grindelwald --out /tmp/finished-baseline
```

- [x] **Review the output by eye and answer each question**

Open `/tmp/finished-baseline` in a file browser and look at every image:

1. **Exposure** — is anything obviously too dark or blown out? A systematic bias
   in one direction means the `MID_GREY` / `HIGHLIGHT_TARGET` constants in
   `decide.rs` need adjusting, not individual photos.
2. **White balance** — v1 passes the camera's own WB through untouched
   (`Setting=Camera`), so any cast you see is the camera's, not ours. If casts
   are common enough to matter, that is the evidence for specifying a WB
   override — which needs a *verifiable* coefficient-to-Temperature/Green
   conversion, the thing that was got wrong and removed. The PCA illuminant
   estimate is already persisted in `raw_stats` and ready to drive it.
3. **Denoise at high ISO** — this is spec open item 2. Pick your highest-ISO
   frames and compare against the same file developed in Lightroom. Smeared
   detail means the anchors in `denoise_curve` are too aggressive; visible
   chroma blotches mean they are too weak. **Adjust the anchors now**, bump
   `DECIDER_VERSION`, and re-run.
4. **Sharpening** — is anything crunchy or haloed? Lower the `0.8` cap in
   `decide()`.
5. **Highlight recovery** — do blown skies come back with plausible colour, or
   do they go grey or magenta? Grey means the method escalation threshold in
   `emit_pp3` is too high.
6. **`base.pp3` neutrality** — does the output look like a *default* raw
   conversion plus corrections, or does it carry a look of its own? This is spec
   open item 5, and it matters more than it looks: Phase 2's model was trained on
   default-converted sRGB, so a baseline with its own contrast curve feeds it an
   input distribution it never saw.

- [x] **Record the outcome** — spec open item 5 closed, item 2 carried forward with reasons; see A11.

Write what you changed and why into the spec's open-items section, ticking off
items 2 and 5. If you changed any formula, bump `DECIDER_VERSION` in `decide.rs`
so existing renders are correctly invalidated.

```bash
git add docs/superpowers/specs/2026-07-29-auto-develop-design.md crates/pipeline/src/develop/decide.rs
git commit -m "docs(spec): close open items 2 and 5 after baseline review"
```

- [x] **Explicit go/no-go** — **GO, reviewed 2026-08-13.** Recorded as A11 in the
spec. Exposure, white balance, highlight recovery and `base.pp3` neutrality pass.
Sharpening failed and was fixed, not tuned: `decide()` clamped an unbounded
variance-of-Laplacian to 0..1, pinning every frame to `SHARPEN_MAX`; it now
normalises against the calibrated baseline (`DECIDER_VERSION` = `decide-3`).
Denoise is explicitly carried forward — no high-ISO material exists to judge it,
and the look does not depend on it.

Two limits on this sign-off, so Phase 2 does not inherit false confidence: the
corpus is three photographs, and the sharpness baseline they were normalised
against was built from those same three, so its p10/p90 are the set's own min and
max. The mechanism is verified end-to-end; its calibration is not. Re-run
`photopipe calibrate` and re-review once a few hundred frames per lens exist.

---

## Phase 2 — The look

> Start only after the CHECKPOINT above is signed off.

### Task 13: Export the LUT predictor to ONNX

Resolves spec open item 3. The reference implementation applies its LUT through
a custom CUDA trilinear-interpolation extension that will not trace to ONNX —
which is fine, because we want to own application anyway (spec §7). Only the
predictor CNN is exported; the basis LUTs come out as plain tensors.

**Files:**
- Create: `tools/export_lut3d.py`
- Modify: `tools/requirements.txt`, `models/download.sh`, `models/README.md`

**Interfaces:**
- Consumes: nothing in the Rust tree.
- Produces:
  - `models/lut3d_predictor.onnx` — input `image` `[1,3,256,256]` float32, output `weights` `[1,N]` float32
  - `models/lut3d_basis.npy` — float32 `[N,3,33,33,33]`, the basis LUTs
  - Both gitignored, following the existing `models/*.onnx` rule.

- [ ] **Step 1: Write the exporter**

Create `tools/export_lut3d.py`, following the structure of the three existing
exporters:

```python
#!/usr/bin/env python3
"""Export the Image-Adaptive-3DLUT predictor to ONNX (opset 18).

One-time tool. Produces:
    ../models/lut3d_predictor.onnx  — the weight-predictor CNN
    ../models/lut3d_basis.npy       — the basis LUTs as a plain [N,3,33,33,33] array

Only the predictor is exported. The reference implementation applies its fused
LUT through a custom CUDA trilinear-interpolation extension that cannot be
traced; photopipe performs the fuse and the apply in Rust at 16-bit precision
instead (spec section 7). Not used at runtime — the shipped photopipe binary
has zero Python dependency.

Licensing: the Image-Adaptive-3DLUT code is Apache-2.0, but its weights derive
from MIT-Adobe FiveK, whose Adobe licence grants use "solely for your own
research purposes". This script produces the weights on your machine; the
repository never contains or redistributes them.

Usage:
    python -m venv .venv && source .venv/bin/activate
    pip install -r requirements.txt
    python export_lut3d.py --checkpoint /path/to/LUTs.pth
"""
import argparse
import pathlib

import numpy as np
import torch
import torch.nn as nn

OPSET = 18
INPUT_SIZE = 256
LUT_DIM = 33


class Predictor(nn.Module):
    """The weight-predictor CNN from Zeng et al., under 600K parameters.

    Mirrors the reference `Classifier` module. Reproduced here rather than
    imported so the export does not depend on cloning the upstream repo.
    """

    def __init__(self, n_luts: int = 3):
        super().__init__()
        self.net = nn.Sequential(
            nn.Conv2d(3, 16, 3, stride=2, padding=1),
            nn.LeakyReLU(0.2),
            nn.InstanceNorm2d(16, affine=True),
            nn.Conv2d(16, 32, 3, stride=2, padding=1),
            nn.LeakyReLU(0.2),
            nn.InstanceNorm2d(32, affine=True),
            nn.Conv2d(32, 64, 3, stride=2, padding=1),
            nn.LeakyReLU(0.2),
            nn.InstanceNorm2d(64, affine=True),
            nn.Conv2d(64, 128, 3, stride=2, padding=1),
            nn.LeakyReLU(0.2),
            nn.InstanceNorm2d(128, affine=True),
            nn.Conv2d(128, 128, 3, stride=2, padding=1),
            nn.LeakyReLU(0.2),
            nn.Dropout(0.5),
            nn.AdaptiveAvgPool2d(1),
            nn.Flatten(),
            nn.Linear(128, n_luts),
        )

    def forward(self, image):
        return self.net(image)


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--checkpoint", required=True, help="path to the trained LUTs.pth")
    ap.add_argument("--n-luts", type=int, default=3)
    ap.add_argument("--out-dir", default=str(pathlib.Path(__file__).parent.parent / "models"))
    args = ap.parse_args()

    out_dir = pathlib.Path(args.out_dir)
    out_dir.mkdir(parents=True, exist_ok=True)

    state = torch.load(args.checkpoint, map_location="cpu")

    # ── the basis LUTs ──
    # Stored in the checkpoint as one tensor per basis LUT. Shapes vary by
    # release; normalise to [N, 3, D, D, D] before saving.
    basis = []
    for i in range(args.n_luts):
        for key in (f"LUT{i}.LUT", f"LUT_{i}.LUT", f"luts.{i}"):
            if key in state:
                basis.append(torch.as_tensor(state[key]).float())
                break
        else:
            raise SystemExit(
                f"cannot find basis LUT {i} in the checkpoint. Keys present: {sorted(state)[:20]}"
            )
    stacked = torch.stack(basis).reshape(args.n_luts, 3, LUT_DIM, LUT_DIM, LUT_DIM)
    np.save(out_dir / "lut3d_basis.npy", stacked.numpy().astype(np.float32))
    print(f"wrote {out_dir / 'lut3d_basis.npy'}  shape={tuple(stacked.shape)}")

    # ── the predictor ──
    model = Predictor(args.n_luts).eval()
    predictor_state = {
        k.split("classifier.", 1)[-1]: v for k, v in state.items() if "classifier" in k
    }
    missing = model.net.load_state_dict(predictor_state, strict=False)
    print(f"predictor load: {missing}")

    dummy = torch.zeros(1, 3, INPUT_SIZE, INPUT_SIZE)
    onnx_path = out_dir / "lut3d_predictor.onnx"
    torch.onnx.export(
        model,
        dummy,
        str(onnx_path),
        opset_version=OPSET,
        input_names=["image"],
        output_names=["weights"],
        dynamic_axes=None,  # fixed 1x3x256x256; the Rust side always downsamples to this
    )
    print(f"wrote {onnx_path}")

    # ── verify no custom op survived the trace ──
    # The reference trilinear-interpolation extension must NOT appear in the
    # graph; if it does, the export captured the apply step too and ORT will
    # fail to load the model at runtime.
    import onnx

    graph = onnx.load(str(onnx_path)).graph
    ops = {n.op_type for n in graph.node}
    custom = {o for o in ops if "trilinear" in o.lower() or "TrilinearInterpolation" in o}
    if custom:
        raise SystemExit(f"custom ops leaked into the graph: {custom}")
    print(f"graph ops: {sorted(ops)}")


if __name__ == "__main__":
    main()
```

- [ ] **Step 2: Run it and verify the outputs**

```bash
cd tools
source .venv/bin/activate
pip install -r requirements.txt
python export_lut3d.py --checkpoint /path/to/LUTs.pth
```

Expected: both files appear under `models/`, the printed graph ops contain no
custom operator, and the basis shape prints as `(3, 3, 33, 33, 33)`.

If the checkpoint key names do not match any of the three patterns tried, the
error prints the keys that *are* present — add the right pattern to the loop.

- [ ] **Step 3: Confirm the weights are gitignored**

```bash
git status --short models/
```

Expected: `lut3d_predictor.onnx` does not appear (covered by `models/*.onnx`).
`lut3d_basis.npy` is **not** covered by the existing rules — add to `.gitignore`:

```
models/*.npy
```

- [ ] **Step 4: Document the model**

Add a section to `models/README.md` matching the existing entries: what the file
is, which script produces it, and the FiveK licence constraint — that photopipe
is a personal non-commercial project, never redistributes the weights, and that
this would need revisiting before any commercial distribution.

Add the corresponding lines to `models/download.sh` following its existing
pattern, and `onnx` to `tools/requirements.txt` if it is not already listed.

- [ ] **Step 5: Commit**

```bash
git add tools/export_lut3d.py tools/requirements.txt models/README.md models/download.sh .gitignore
git commit -m "feat(tools): export the Image-Adaptive-3DLUT predictor and basis LUTs"
```

---

### Task 14: The `Lut33` type and `.cube` I/O

**Files:**
- Create: `crates/pipeline/src/develop/lut.rs`
- Modify: `crates/pipeline/src/develop/mod.rs`
- Test: inline `#[cfg(test)] mod tests` in `lut.rs`

**Interfaces:**
- Produces:
  - `pipeline::develop::lut::Lut33` — a 33³ RGB lookup table, `data: Vec<f32>` of length `33*33*33*3`
  - `Lut33::identity() -> Lut33`
  - `Lut33::fuse(basis: &[Lut33], weights: &[f32]) -> Lut33`
  - `Lut33::to_cube(&self) -> String`
  - `Lut33::from_cube(text: &str) -> Result<Lut33, DevelopError>`
  - `Lut33::content_hash(&self) -> String`
  - `pipeline::develop::lut::LUT_DIM: usize` = 33

- [ ] **Step 1: Write the failing tests**

Create `crates/pipeline/src/develop/lut.rs` with only a test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// The identity LUT maps every lattice point to itself.
    #[test]
    fn identity_maps_each_point_to_itself() {
        let lut = Lut33::identity();
        for (i, expected) in [(0usize, 0.0f32), (LUT_DIM - 1, 1.0)] {
            let idx = (i * LUT_DIM * LUT_DIM + i * LUT_DIM + i) * 3;
            assert!((lut.data[idx] - expected).abs() < 1e-6);
            assert!((lut.data[idx + 1] - expected).abs() < 1e-6);
            assert!((lut.data[idx + 2] - expected).abs() < 1e-6);
        }
    }

    /// A .cube round trip is lossless to within the written precision.
    #[test]
    fn cube_round_trips() {
        let lut = Lut33::identity();
        let text = lut.to_cube();
        let back = Lut33::from_cube(&text).expect("parse");
        for (a, b) in lut.data.iter().zip(back.data.iter()) {
            assert!((a - b).abs() < 1e-5, "{a} != {b}");
        }
    }

    /// The emitted .cube declares the size and holds exactly DIM^3 entries,
    /// so RawTherapee and darktable can load it.
    #[test]
    fn cube_header_and_entry_count_are_well_formed() {
        let text = Lut33::identity().to_cube();
        assert!(text.contains("LUT_3D_SIZE 33"), "{text}");
        let entries = text
            .lines()
            .filter(|l| !l.is_empty() && !l.starts_with('#') && !l.starts_with("LUT_") && !l.starts_with("TITLE") && !l.starts_with("DOMAIN"))
            .count();
        assert_eq!(entries, LUT_DIM * LUT_DIM * LUT_DIM);
    }

    /// Fusing weighted basis LUTs is a plain linear combination.
    #[test]
    fn fuse_is_a_weighted_sum() {
        let mut a = Lut33::identity();
        a.data.iter_mut().for_each(|v| *v = 0.0);
        let mut b = Lut33::identity();
        b.data.iter_mut().for_each(|v| *v = 1.0);
        let fused = Lut33::fuse(&[a, b], &[0.25, 0.75]);
        assert!((fused.data[0] - 0.75).abs() < 1e-6, "got {}", fused.data[0]);
    }

    /// Fused values must stay in range whatever the predictor emits — weights
    /// from a CNN are unconstrained and can be negative or sum above one.
    #[test]
    fn fuse_clamps_out_of_range_results() {
        let mut hot = Lut33::identity();
        hot.data.iter_mut().for_each(|v| *v = 1.0);
        let fused = Lut33::fuse(&[hot], &[5.0]);
        assert!(fused.data.iter().all(|v| (0.0..=1.0).contains(v)));
    }

    /// A weight/basis length mismatch returns the identity rather than
    /// panicking or producing a partly-fused table.
    #[test]
    fn mismatched_weights_fall_back_to_identity() {
        let fused = Lut33::fuse(&[Lut33::identity()], &[0.5, 0.5]);
        assert_eq!(fused.data, Lut33::identity().data);
    }

    /// Content hashing is what makes LUTs shareable between similar frames.
    #[test]
    fn content_hash_is_stable_and_sensitive() {
        let a = Lut33::identity();
        assert_eq!(a.content_hash(), Lut33::identity().content_hash());
        let mut b = Lut33::identity();
        b.data[0] += 0.5;
        assert_ne!(a.content_hash(), b.content_hash());
    }

    /// Malformed input is an error, not a panic.
    #[test]
    fn malformed_cube_is_an_error() {
        assert!(Lut33::from_cube("").is_err());
        assert!(Lut33::from_cube("LUT_3D_SIZE 33\n0.0 0.0").is_err());
        assert!(Lut33::from_cube("LUT_3D_SIZE 17\n").is_err(), "wrong size must be rejected");
    }
}
```

- [ ] **Step 2: Run them to confirm they fail**

Run: `cargo test -p pipeline --lib develop::lut`
Expected: FAIL — module not declared.

- [ ] **Step 3: Implement**

Prepend to `crates/pipeline/src/develop/lut.rs`:

```rust
//! 33³ RGB lookup tables and the `.cube` interchange format.
//!
//! LUTs are written to the cache as `.cube` rather than kept in memory only:
//! it keeps them inspectable, and a user can load one in RawTherapee or
//! darktable to reproduce a look by hand. Content-addressing means visually
//! similar frames in a burst share a single file.

use crate::develop::DevelopError;

/// Lattice points per axis. 33 is the de-facto standard and what the model
/// was trained at.
pub const LUT_DIM: usize = 33;

/// A 33³ RGB lookup table. Indexed `((b * DIM + g) * DIM + r) * 3`, which is
/// the ordering the `.cube` format iterates in (red fastest).
#[derive(Debug, Clone, PartialEq)]
pub struct Lut33 {
    pub data: Vec<f32>,
}

impl Lut33 {
    pub fn identity() -> Self {
        let mut data = Vec::with_capacity(LUT_DIM * LUT_DIM * LUT_DIM * 3);
        let last = (LUT_DIM - 1) as f32;
        for b in 0..LUT_DIM {
            for g in 0..LUT_DIM {
                for r in 0..LUT_DIM {
                    data.push(r as f32 / last);
                    data.push(g as f32 / last);
                    data.push(b as f32 / last);
                }
            }
        }
        Self { data }
    }

    /// Blend basis LUTs by predictor weights into one image-specific table.
    ///
    /// Weights come straight from a CNN and are unconstrained — they can be
    /// negative or sum above one — so the result is clamped. Returns the
    /// identity on a length mismatch rather than producing a partly-fused table.
    pub fn fuse(basis: &[Lut33], weights: &[f32]) -> Lut33 {
        if basis.is_empty() || basis.len() != weights.len() {
            tracing::warn!(
                basis = basis.len(),
                weights = weights.len(),
                "LUT fuse operand mismatch; falling back to identity"
            );
            return Lut33::identity();
        }
        let mut out = vec![0.0f32; LUT_DIM * LUT_DIM * LUT_DIM * 3];
        for (lut, w) in basis.iter().zip(weights.iter()) {
            if lut.data.len() != out.len() || !w.is_finite() {
                return Lut33::identity();
            }
            for (o, v) in out.iter_mut().zip(lut.data.iter()) {
                *o += w * v;
            }
        }
        for v in out.iter_mut() {
            *v = if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 };
        }
        Lut33 { data: out }
    }

    pub fn to_cube(&self) -> String {
        let mut s = String::with_capacity(self.data.len() * 8);
        s.push_str("# Generated by photopipe\n");
        s.push_str("TITLE \"photopipe adaptive look\"\n");
        s.push_str(&format!("LUT_3D_SIZE {LUT_DIM}\n"));
        s.push_str("DOMAIN_MIN 0.0 0.0 0.0\n");
        s.push_str("DOMAIN_MAX 1.0 1.0 1.0\n");
        for rgb in self.data.chunks_exact(3) {
            s.push_str(&format!("{:.6} {:.6} {:.6}\n", rgb[0], rgb[1], rgb[2]));
        }
        s
    }

    pub fn from_cube(text: &str) -> Result<Lut33, DevelopError> {
        let bad = |reason: &str| DevelopError::Render {
            path: std::path::PathBuf::from("<cube>"),
            reason: reason.to_string(),
        };

        let mut data = Vec::with_capacity(LUT_DIM * LUT_DIM * LUT_DIM * 3);
        let mut declared: Option<usize> = None;
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some(rest) = line.strip_prefix("LUT_3D_SIZE") {
                declared = rest.trim().parse::<usize>().ok();
                continue;
            }
            if line.starts_with("TITLE") || line.starts_with("DOMAIN") || line.starts_with("LUT_") {
                continue;
            }
            let mut parts = line.split_whitespace();
            let (Some(r), Some(g), Some(b)) = (parts.next(), parts.next(), parts.next()) else {
                return Err(bad("entry line does not hold three values"));
            };
            for v in [r, g, b] {
                data.push(v.parse::<f32>().map_err(|_| bad("non-numeric entry"))?);
            }
        }

        match declared {
            Some(d) if d == LUT_DIM => {}
            Some(d) => return Err(bad(&format!("LUT_3D_SIZE {d}, expected {LUT_DIM}"))),
            None => return Err(bad("missing LUT_3D_SIZE")),
        }
        if data.len() != LUT_DIM * LUT_DIM * LUT_DIM * 3 {
            return Err(bad(&format!(
                "expected {} values, found {}",
                LUT_DIM * LUT_DIM * LUT_DIM * 3,
                data.len()
            )));
        }
        Ok(Lut33 { data })
    }

    /// Content hash, used as the cache filename so visually similar frames
    /// share one `.cube`.
    pub fn content_hash(&self) -> String {
        use std::hash::Hasher;
        let mut h = xxhash_rust::xxh3::Xxh3::new();
        for v in &self.data {
            h.write_i32((v * 100_000.0).round() as i32);
        }
        format!("{:016x}", h.finish())
    }
}
```

Declare it in `crates/pipeline/src/develop/mod.rs`:

```rust
pub mod lut;
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p pipeline --lib develop::lut`
Expected: PASS, 8 tests.

- [ ] **Step 5: Verify green and commit**

```bash
cargo fmt && cargo clippy --all-targets --all-features -- -D warnings && cargo test --all
git add crates/pipeline/src/develop
git commit -m "feat(develop): Lut33 type, .cube I/O, and basis fusion"
```

---

### Task 15: Trilinear LUT application

**Files:**
- Create: `crates/pipeline/src/develop/lut_apply.rs`
- Modify: `crates/pipeline/src/develop/mod.rs`
- Test: inline `#[cfg(test)] mod tests` in `lut_apply.rs`

**Interfaces:**
- Consumes: `Lut33`, `LUT_DIM` (Task 14)
- Produces: `pipeline::develop::lut_apply::apply_lut(img: &image::DynamicImage, lut: &Lut33) -> image::DynamicImage`

- [ ] **Step 1: Write the failing tests**

Create `crates/pipeline/src/develop/lut_apply.rs` with only a test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::develop::lut::Lut33;
    use image::{DynamicImage, Rgb, RgbImage};

    fn test_image() -> DynamicImage {
        let mut img = RgbImage::new(4, 4);
        for (i, px) in img.pixels_mut().enumerate() {
            let v = (i * 16) as u8;
            *px = Rgb([v, 255 - v, 128]);
        }
        DynamicImage::ImageRgb8(img)
    }

    /// The identity LUT round-trips an image bit-exactly. This is the single
    /// most important property: any interpolation error shows up here.
    #[test]
    fn identity_lut_round_trips_bit_exactly() {
        let src = test_image();
        let out = apply_lut(&src, &Lut33::identity());
        assert_eq!(out.to_rgb8().into_raw(), src.to_rgb8().into_raw());
    }

    /// A LUT that maps everything to black produces black, proving the lookup
    /// is actually consulted rather than the source being passed through.
    #[test]
    fn constant_lut_maps_everything_to_its_constant() {
        let mut black = Lut33::identity();
        black.data.iter_mut().for_each(|v| *v = 0.0);
        let out = apply_lut(&test_image(), &black);
        assert!(out.to_rgb8().into_raw().iter().all(|v| *v == 0));
    }

    /// An inverting LUT matches an independently computed reference.
    #[test]
    fn inverting_lut_matches_a_reference_computation() {
        let mut inv = Lut33::identity();
        inv.data.iter_mut().for_each(|v| *v = 1.0 - *v);
        let src = test_image();
        let out = apply_lut(&src, &inv);
        let src_raw = src.to_rgb8().into_raw();
        for (i, got) in out.to_rgb8().into_raw().iter().enumerate() {
            let want = 255 - src_raw[i];
            assert!(
                (*got as i16 - want as i16).abs() <= 1,
                "index {i}: got {got}, want {want}"
            );
        }
    }

    /// Dimensions and channel count survive.
    #[test]
    fn geometry_is_preserved() {
        let out = apply_lut(&test_image(), &Lut33::identity());
        assert_eq!(out.width(), 4);
        assert_eq!(out.height(), 4);
    }

    /// A 16-bit source is accepted — the TIFF RawTherapee emits is 16-bit,
    /// and applying the look at 8-bit would throw away the headroom the
    /// baseline render exists to preserve.
    #[test]
    fn sixteen_bit_input_is_handled() {
        let mut img = image::ImageBuffer::<image::Rgb<u16>, Vec<u16>>::new(2, 2);
        for px in img.pixels_mut() {
            *px = image::Rgb([65535u16, 0, 32768]);
        }
        let out = apply_lut(&DynamicImage::ImageRgb16(img), &Lut33::identity());
        assert_eq!(out.width(), 2);
    }
}
```

- [ ] **Step 2: Run them to confirm they fail**

Run: `cargo test -p pipeline --lib develop::lut_apply`
Expected: FAIL — module not declared.

- [ ] **Step 3: Implement**

Prepend to `crates/pipeline/src/develop/lut_apply.rs`:

```rust
//! Trilinear application of a 3D LUT.
//!
//! Done in Rust rather than by RawTherapee's Film Simulation tool, which
//! locates HaldCLUTs through a GUI preferences directory and has no documented
//! `.pp3` key for selecting one by path. Driving it headlessly would mean
//! writing into RawTherapee's own config. Applying here also puts the LUT in
//! exactly the domain it was trained on — sRGB (spec section 4).

use image::DynamicImage;

use crate::develop::lut::{Lut33, LUT_DIM};

/// Apply `lut` to every pixel of `img`.
///
/// Input is read at 16-bit and output at 16-bit, so the headroom the baseline
/// render preserved survives into the JPEG encoder's input.
pub fn apply_lut(img: &DynamicImage, lut: &Lut33) -> DynamicImage {
    let src = img.to_rgb16();
    let (w, h) = (src.width(), src.height());
    let mut out = image::ImageBuffer::<image::Rgb<u16>, Vec<u16>>::new(w, h);

    for (dst_px, src_px) in out.pixels_mut().zip(src.pixels()) {
        let rgb = [
            src_px.0[0] as f32 / 65535.0,
            src_px.0[1] as f32 / 65535.0,
            src_px.0[2] as f32 / 65535.0,
        ];
        let looked = sample(lut, rgb);
        *dst_px = image::Rgb([
            (looked[0].clamp(0.0, 1.0) * 65535.0).round() as u16,
            (looked[1].clamp(0.0, 1.0) * 65535.0).round() as u16,
            (looked[2].clamp(0.0, 1.0) * 65535.0).round() as u16,
        ]);
    }
    DynamicImage::ImageRgb16(out)
}

/// Trilinear interpolation of one RGB triple through the lattice.
fn sample(lut: &Lut33, rgb: [f32; 3]) -> [f32; 3] {
    let last = (LUT_DIM - 1) as f32;
    // Position in lattice coordinates, split into cell index and fraction.
    let mut lo = [0usize; 3];
    let mut hi = [0usize; 3];
    let mut frac = [0f32; 3];
    for c in 0..3 {
        let pos = (rgb[c].clamp(0.0, 1.0) * last).clamp(0.0, last);
        let f = pos.floor();
        lo[c] = f as usize;
        hi[c] = (lo[c] + 1).min(LUT_DIM - 1);
        frac[c] = pos - f;
    }

    // Eight corners of the enclosing cell, weighted by the complement of each
    // axis fraction. Standard trilinear interpolation.
    let mut acc = [0f32; 3];
    for corner in 0..8 {
        let (ir, wr) = pick(corner, 0, &lo, &hi, &frac);
        let (ig, wg) = pick(corner, 1, &lo, &hi, &frac);
        let (ib, wb) = pick(corner, 2, &lo, &hi, &frac);
        let weight = wr * wg * wb;
        if weight == 0.0 {
            continue;
        }
        let base = ((ib * LUT_DIM + ig) * LUT_DIM + ir) * 3;
        for c in 0..3 {
            acc[c] += weight * lut.data[base + c];
        }
    }
    acc
}

/// For corner `corner`, axis `axis`: which lattice index and what weight.
fn pick(
    corner: usize,
    axis: usize,
    lo: &[usize; 3],
    hi: &[usize; 3],
    frac: &[f32; 3],
) -> (usize, f32) {
    if corner & (1 << axis) == 0 {
        (lo[axis], 1.0 - frac[axis])
    } else {
        (hi[axis], frac[axis])
    }
}
```

Declare it in `crates/pipeline/src/develop/mod.rs`:

```rust
pub mod lut_apply;
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p pipeline --lib develop::lut_apply`
Expected: PASS, 5 tests.

If `identity_lut_round_trips_bit_exactly` is off by one, the culprit is almost
always the 8↔16-bit conversion, not the interpolation: `to_rgb16` scales a `u8`
`v` to `v * 257`, so the reverse must divide by 257, not by 256.

- [ ] **Step 5: Verify green and commit**

```bash
cargo fmt && cargo clippy --all-targets --all-features -- -D warnings && cargo test --all
git add crates/pipeline/src/develop
git commit -m "feat(develop): trilinear 3D LUT application at 16-bit precision"
```

---

### Task 16: The LUT predictor model

**Files:**
- Create: `crates/pipeline/src/models/lut_predictor.rs`
- Modify: `crates/pipeline/src/models/mod.rs` (declare the module, add the trait and hub slot)
- Test: inline `#[cfg(test)] mod tests` in `lut_predictor.rs`

No config change: the predictor is found by fixed filename under
`cfg.models.model_dir`, exactly as the other three models are. Which look to use
is already `[develop.look] model` from Task 1.

**Interfaces:**
- Consumes: `build_session` (`crates/pipeline/src/models/mod.rs:214`), `Lut33` (Task 14), the artefacts from Task 13
- Produces:
  - `pipeline::models::LookPredictor` trait — `fn predict(&self, img: &DynamicImage) -> Result<Lut33>`, `fn name(&self) -> &str`, `fn version(&self) -> &str`
  - `pipeline::models::lut_predictor::Lut3dPredictor::load(onnx: &Path, basis: &Path) -> Result<Self>`
  - `ModelHub.look: Option<Arc<dyn LookPredictor>>`

- [ ] **Step 1: Write the failing tests**

Create `crates/pipeline/src/models/lut_predictor.rs` with only a test module.
These cover the parts that do not need the weights — basis parsing and the
missing-file path:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// A missing model file is a typed error naming the path, so the caller
    /// can degrade to baseline-only rather than aborting the run.
    #[test]
    fn missing_files_produce_a_not_found_error() {
        let err = Lut3dPredictor::load(
            std::path::Path::new("/nonexistent/lut3d_predictor.onnx"),
            std::path::Path::new("/nonexistent/lut3d_basis.npy"),
        )
        .expect_err("load should fail");
        assert!(err.to_string().contains("lut3d"), "error should name the file: {err}");
    }

    /// The .npy reader accepts the exact shape the exporter writes and
    /// rejects anything else — a silently mis-shaped basis would produce a
    /// plausible but wrong look.
    #[test]
    fn basis_shape_is_validated() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("basis.npy");
        // A well-formed header for the wrong shape.
        write_test_npy(&path, &[2, 3, 17, 17, 17]);
        let err = read_basis(&path).expect_err("wrong lattice size must be rejected");
        assert!(err.to_string().contains("33"), "{err}");
    }

    /// Weights that arrive as NaN or wildly out of range must not poison the
    /// fused LUT — Lut33::fuse clamps, and this confirms the path reaches it.
    #[test]
    fn non_finite_weights_yield_the_identity() {
        let basis = vec![crate::develop::lut::Lut33::identity()];
        let fused = crate::develop::lut::Lut33::fuse(&basis, &[f32::NAN]);
        assert_eq!(fused.data, crate::develop::lut::Lut33::identity().data);
    }
}
```

`write_test_npy` is a test helper you write alongside these — a minimal
`.npy` v1.0 header (`\x93NUMPY`, version bytes, a dict literal naming
`descr='<f4'`, `fortran_order: False`, and the shape tuple) followed by the
right number of zero bytes.

- [ ] **Step 2: Run them to confirm they fail**

Run: `cargo test -p pipeline --lib models::lut_predictor`
Expected: FAIL — module not declared.

- [ ] **Step 3: Implement**

Prepend to `crates/pipeline/src/models/lut_predictor.rs`:

```rust
//! Image-adaptive 3D LUT prediction (Zeng et al.).
//!
//! A predictor CNN under 600K parameters consumes a 256px downsample and emits
//! blend weights over N basis LUTs, which fuse into one image-specific 33³
//! table. Only the predictor is ONNX; the fuse and the apply are ours, because
//! the reference implementation's trilinear step is a custom CUDA extension
//! that will not trace (spec section 7).

use std::path::Path;
use std::sync::Mutex;

use anyhow::{Context, Result};
use image::DynamicImage;
use ndarray::Array4;

use crate::develop::lut::{Lut33, LUT_DIM};
use crate::models::LookPredictor;

/// The predictor's fixed input size.
const INPUT_SIZE: u32 = 256;

pub struct Lut3dPredictor {
    session: Mutex<ort::session::Session>,
    basis: Vec<Lut33>,
}

impl Lut3dPredictor {
    pub fn load(onnx: &Path, basis_npy: &Path) -> Result<Self> {
        let basis = read_basis(basis_npy)
            .with_context(|| format!("cannot read basis LUTs from {}", basis_npy.display()))?;
        let session = crate::models::build_session(onnx)
            .with_context(|| format!("cannot load lut3d predictor {}", onnx.display()))?;
        Ok(Self {
            session: Mutex::new(session),
            basis,
        })
    }

    fn preprocess(img: &DynamicImage) -> Array4<f32> {
        let rgb = img
            .resize_exact(INPUT_SIZE, INPUT_SIZE, image::imageops::FilterType::Triangle)
            .to_rgb8();
        let mut arr = Array4::<f32>::zeros((1, 3, INPUT_SIZE as usize, INPUT_SIZE as usize));
        for (x, y, px) in rgb.enumerate_pixels() {
            for c in 0..3 {
                arr[[0, c, y as usize, x as usize]] = px.0[c] as f32 / 255.0;
            }
        }
        arr
    }
}

impl LookPredictor for Lut3dPredictor {
    fn predict(&self, img: &DynamicImage) -> Result<Lut33> {
        let input = Self::preprocess(img);
        let tensor =
            ort::value::Tensor::<f32>::from_array(input).map_err(|e| anyhow::anyhow!("{e}"))?;
        let mut session = self
            .session
            .lock()
            .map_err(|_| anyhow::anyhow!("session mutex poisoned"))?;
        let outputs = session
            .run(ort::inputs!["image" => &tensor])
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        let (_, data) = outputs["weights"]
            .try_extract_tensor::<f32>()
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        let weights: Vec<f32> = data.iter().copied().take(self.basis.len()).collect();
        Ok(Lut33::fuse(&self.basis, &weights))
    }

    fn name(&self) -> &str {
        "lut3d-fivek"
    }

    fn version(&self) -> &str {
        "1"
    }
}

/// Read the exporter's `[N, 3, 33, 33, 33]` float32 array.
///
/// A hand-rolled reader rather than a new dependency: the file this consumes is
/// written by our own `tools/export_lut3d.py`, so exactly one `.npy` dialect
/// needs supporting — v1.0, little-endian float32, C order.
pub(crate) fn read_basis(path: &Path) -> Result<Vec<Lut33>> {
    let bytes = std::fs::read(path)
        .with_context(|| format!("cannot read {}", path.display()))?;
    if bytes.len() < 10 || &bytes[0..6] != b"\x93NUMPY" {
        anyhow::bail!("{} is not a .npy file", path.display());
    }
    let header_len = u16::from_le_bytes([bytes[8], bytes[9]]) as usize;
    let header = std::str::from_utf8(&bytes[10..10 + header_len])
        .context("npy header is not UTF-8")?;
    if !header.contains("'<f4'") && !header.contains("\"<f4\"") {
        anyhow::bail!("basis must be little-endian float32, header said: {header}");
    }
    if header.contains("'fortran_order': True") {
        anyhow::bail!("basis must be in C order");
    }

    let shape: Vec<usize> = header
        .split("'shape':")
        .nth(1)
        .and_then(|s| s.split('(').nth(1))
        .and_then(|s| s.split(')').next())
        .map(|s| {
            s.split(',')
                .filter_map(|t| t.trim().parse::<usize>().ok())
                .collect()
        })
        .unwrap_or_default();

    let expected_tail = [3usize, LUT_DIM, LUT_DIM, LUT_DIM];
    if shape.len() != 5 || shape[1..] != expected_tail {
        anyhow::bail!(
            "basis shape {shape:?} is not [N, 3, {LUT_DIM}, {LUT_DIM}, {LUT_DIM}]"
        );
    }

    let n = shape[0];
    let per_lut = 3 * LUT_DIM * LUT_DIM * LUT_DIM;
    let data_start = 10 + header_len;
    let floats: Vec<f32> = bytes[data_start..]
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    if floats.len() < n * per_lut {
        anyhow::bail!("basis payload is short: {} of {}", floats.len(), n * per_lut);
    }

    // The exporter writes [N, 3, B, G, R]; Lut33 wants [(B,G,R) -> RGB] interleaved.
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let base = i * per_lut;
        let plane = LUT_DIM * LUT_DIM * LUT_DIM;
        let mut data = Vec::with_capacity(plane * 3);
        for p in 0..plane {
            for c in 0..3 {
                data.push(floats[base + c * plane + p]);
            }
        }
        out.push(Lut33 { data });
    }
    Ok(out)
}
```

- [ ] **Step 4: Add the trait and the hub slot**

In `crates/pipeline/src/models/mod.rs`, add next to the other traits:

```rust
pub trait LookPredictor: Send + Sync {
    fn predict(&self, img: &DynamicImage) -> Result<crate::develop::lut::Lut33>;
    fn name(&self) -> &str;
    /// Stored in `edits.look_version`; part of the idempotency key.
    fn version(&self) -> &str;
}
```

Declare the module and add the slot:

```rust
pub mod lut_predictor;
```

```rust
pub struct ModelHub {
    pub embedder: Option<Arc<dyn Embedder>>,
    pub iqa: Option<Arc<dyn Iqa>>,
    pub detector: Option<Arc<dyn SubjectDetector>>,
    pub look: Option<Arc<dyn LookPredictor>>,
    pub provider: String,
}
```

Set `look: None` in `ModelHub::empty()`, and in `from_config` load it the same
way the other three slots are loaded — a missing file leaves the slot `None`
and logs a notice rather than failing, matching the existing contract:

```rust
        let look_onnx = cfg.model_dir.join("lut3d_predictor.onnx");
        let look_basis = cfg.model_dir.join("lut3d_basis.npy");
        let look = if look_onnx.exists() && look_basis.exists() {
            match lut_predictor::Lut3dPredictor::load(&look_onnx, &look_basis) {
                Ok(p) => Some(Arc::new(p) as Arc<dyn LookPredictor>),
                Err(e) => {
                    tracing::warn!(error = %e, "look predictor failed to load; baseline only");
                    None
                }
            }
        } else {
            tracing::info!("look predictor not present; `finish` will produce baseline JPEGs");
            None
        };
```

Leave `is_empty()` as it is — it gates the scan pipeline, which does not use the
look predictor.

- [ ] **Step 5: Run the tests**

Run: `cargo test -p pipeline --lib models::lut_predictor`
Expected: PASS, 3 tests.

- [ ] **Step 6: Verify green and commit**

```bash
cargo fmt && cargo clippy --all-targets --all-features -- -D warnings && cargo test --all
git add crates/pipeline/src/models
git commit -m "feat(models): image-adaptive 3D LUT predictor behind the LookPredictor trait"
```

---

### Task 17: Wire the look into `finish`, with the IQA guard

**Files:**
- Modify: `crates/pipeline/src/develop/mod.rs`
- Modify: `crates/cli/src/main.rs` (pass a `ModelHub` to `finish_folder`)
- Test: `crates/pipeline/tests/develop.rs`

**Interfaces:**
- Consumes: `ModelHub` (Task 16), `apply_lut` (Task 15), `Lut33` (Task 14), `Iqa` trait (existing, `crates/pipeline/src/models/mod.rs:47`)
- Produces:
  - `finish_folder(catalog: &Catalog, cfg: &DevelopConfig, hub: &ModelHub, cache_dir: &Path, out_dir: &Path, progress: &dyn ProgressSink) -> anyhow::Result<FinishReport>` — Task 11's signature gains `hub` and `cache_dir`
  - `pipeline::develop::guard_verdict(before: Option<f32>, after: Option<f32>, margin: f32) -> bool`

- [ ] **Step 1: Write the failing tests for the guard predicate**

Append to `crates/pipeline/tests/develop.rs`:

```rust
use pipeline::develop::guard_verdict;

/// The look is kept when it improves or holds quality.
#[test]
fn guard_keeps_an_improving_look() {
    assert!(guard_verdict(Some(0.60), Some(0.70), 0.02));
    assert!(guard_verdict(Some(0.60), Some(0.60), 0.02));
}

/// A drop inside the margin is tolerated — a look is a stylistic change and
/// a tiny IQA dip is not evidence it made the photo worse.
#[test]
fn guard_tolerates_a_drop_inside_the_margin() {
    assert!(guard_verdict(Some(0.60), Some(0.59), 0.02));
}

/// A drop past the margin rejects the look.
#[test]
fn guard_rejects_a_drop_past_the_margin() {
    assert!(!guard_verdict(Some(0.60), Some(0.50), 0.02));
}

/// With no scores available the guard cannot judge, so it must not reject —
/// silently dropping the look on every photo because the IQA model is absent
/// would be a confusing failure mode.
#[test]
fn guard_passes_when_scores_are_unavailable() {
    assert!(guard_verdict(None, None, 0.02));
    assert!(guard_verdict(Some(0.6), None, 0.02));
    assert!(guard_verdict(None, Some(0.6), 0.02));
}
```

- [ ] **Step 2: Run them to confirm they fail**

Run: `cargo test -p pipeline --test develop guard`
Expected: FAIL — `guard_verdict` does not exist.

- [ ] **Step 3: Implement the guard and wire the look in**

Append the predicate to `crates/pipeline/src/develop/mod.rs`:

```rust
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
```

Change the `finish_folder` signature to take the hub and the cache directory, and
thread both into `finish_one`:

```rust
pub fn finish_folder(
    catalog: &Catalog,
    cfg: &DevelopConfig,
    hub: &crate::models::ModelHub,
    /// Where content-addressed `.cube` files are written, under `luts/`.
    cache_dir: &Path,
    out_dir: &Path,
    progress: &dyn ProgressSink,
) -> anyhow::Result<FinishReport> {
```

`finish_one` gains matching `hub: &ModelHub` and `cache_dir: &Path` parameters.
Pass them down rather than reaching for a global — the cache root belongs to the
`Library` the caller already opened.

In `finish_one`, replace the "④ encode" block with the look stage. The `.cube`
goes into the cache directory, content-addressed, so a burst of similar frames
shares one file:

```rust
    // ④ look
    let baseline = image::open(&rendered.tiff)
        .with_context(|| format!("cannot read rendered TIFF {}", rendered.tiff.display()))?;

    let mut look_model: Option<String> = None;
    let mut look_version: Option<String> = None;
    let mut lut_hash: Option<String> = None;
    let mut look_applied = false;
    let mut iqa_before: Option<f32> = None;
    let mut iqa_after: Option<f32> = None;
    let mut final_image = baseline.clone();

    if cfg.look.enable {
        if let Some(predictor) = hub.look.as_ref() {
            match predictor.predict(&baseline) {
                Ok(lut) => {
                    let hash = lut.content_hash();
                    // Content-addressed in the cache: visually similar frames
                    // in a burst share one file rather than each writing its own.
                    let lut_dir = cache_dir.join("luts");
                    let _ = std::fs::create_dir_all(&lut_dir);
                    let cube = lut_dir.join(format!("{hash}.cube"));
                    if !cube.exists() {
                        let _ = std::fs::write(&cube, lut.to_cube());
                    }

                    let looked = crate::develop::lut_apply::apply_lut(&baseline, &lut);

                    // ⑤ guard
                    if cfg.look.guard_iqa {
                        if let Some(iqa) = hub.iqa.as_ref() {
                            iqa_before = iqa.score(&baseline).ok();
                            iqa_after = iqa.score(&looked).ok();
                        }
                    }

                    look_model = Some(predictor.name().to_string());
                    look_version = Some(predictor.version().to_string());
                    lut_hash = Some(hash);

                    if guard_verdict(iqa_before, iqa_after, cfg.look.guard_margin) {
                        final_image = looked;
                        look_applied = true;
                    } else {
                        tracing::info!(
                            path = %item.path.display(),
                            before = ?iqa_before,
                            after = ?iqa_after,
                            "look lowered quality past the margin; keeping baseline"
                        );
                    }
                }
                Err(e) => {
                    // A look failure is not a render failure. Ship the baseline.
                    tracing::warn!(path = %item.path.display(), error = %e, "look prediction failed; baseline only");
                }
            }
        }
    }

    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("cannot create {}", parent.display()))?;
    }
    encode_jpeg(&final_image, &dest, cfg.jpeg_quality)?;
```

Then set the corresponding `EditRow` fields from those locals instead of the
hard-coded `None`/`false`, and include the look in the idempotency identity:

```rust
    let wanted = EditIdentity {
        content_hash: item.content_hash.clone(),
        recipe_hash: recipe_hash.clone(),
        decider_version: DECIDER_VERSION.into(),
        renderer: RENDERER_NAME.into(),
        look_model: if cfg.look.enable {
            hub.look.as_ref().map(|p| p.name().to_string())
        } else {
            None
        },
        look_version: if cfg.look.enable {
            hub.look.as_ref().map(|p| p.version().to_string())
        } else {
            None
        },
    };
```

This must be computed **before** the render, from the hub rather than from the
prediction result — otherwise turning the look off would not invalidate the
existing looked JPEGs.

- [ ] **Step 4: Update the two callers**

`cmd_finish` in `crates/cli/src/main.rs` already opens a `Library`, which
carries `cache` — pass `lib.cache.root()` (or the equivalent accessor; check
`crates/pipeline/src/cache/mod.rs`) and a `ModelHub`:

```rust
    let hub = ModelHub::from_config(&cfg.models).map_err(|e| anyhow::anyhow!("models: {}", e))?;
    let report = pipeline::finish_folder(
        &lib.catalog,
        &cfg.develop,
        &hub,
        cache_root,
        &out_dir,
        &CliProgress,
    )?;
```

Update every `finish_folder` call in `crates/pipeline/tests/develop.rs` to pass
`&ModelHub::empty()` and a temp cache dir. An empty hub has no look predictor,
so those tests keep asserting baseline behaviour — which is what they were
written for.

- [ ] **Step 5: Run the tests**

Run: `cargo test -p pipeline --test develop`
Expected: PASS, 20 tests.

- [ ] **Step 6: Run the end-to-end test with the look enabled**

```bash
PHOTOPIPE_TEST_RAWTHERAPEE=/Applications/RawTherapee.app/Contents/MacOS/rawtherapee-cli \
PHOTOPIPE_TEST_RAW=$HOME/Photos/<some>.ARW \
cargo test -p pipeline --test develop end_to_end -- --nocapture
```

Expected: PASS, and idempotency still holds — the second run renders nothing
even with the look in the identity key.

- [ ] **Step 7: Verify green and commit**

```bash
cargo fmt && cargo clippy --all-targets --all-features -- -D warnings && cargo test --all
git add crates/pipeline crates/cli
git commit -m "feat(develop): apply the adaptive look with a CLIP-IQA quality guard"
```

---

### Task 18: Close out the feature

**Files:**
- Modify: `README.md`, `models/README.md`
- Modify: `docs/superpowers/specs/2026-07-29-auto-develop-design.md`

- [ ] **Step 1: Review the look on real photos**

```bash
cargo build --release
./target/release/photopipe finish ~/Photos/Grindelwald --out /tmp/finished-look
```

Compare against `/tmp/finished-baseline` from the CHECKPOINT, side by side.
Check specifically:

1. How many photos had the look rejected by the guard? Query it:
   ```bash
   ./target/release/photopipe stats ~/Photos/Grindelwald
   ```
   If nearly all were rejected, `guard_margin` is too tight. If none ever are,
   confirm the IQA model is actually loaded — a silently absent model makes the
   guard pass everything by design.
2. Do the looked images share a consistent character, or does the per-image
   adaptation swing wildly between neighbouring frames of the same scene?
   Wild swings mean the predictor is reacting to framing rather than to colour.

- [ ] **Step 2: Document the look in the README**

Extend the `finish` bullet added in Task 12:

```markdown
  With the look model installed (`models/lut3d_predictor.onnx`), `finish` also
  applies a per-image colour look — a 33³ LUT predicted from the photo itself
  and applied at 16-bit. Each LUT is written to the cache as a `.cube` you can
  load in RawTherapee or darktable. A CLIP-IQA guard falls back to the
  baseline render whenever the look would lower the measured quality; `edits`
  records which happened and why. Without the model, `finish` produces the
  baseline JPEGs and says so.
```

- [ ] **Step 3: Close the spec's open items**

In `docs/superpowers/specs/2026-07-29-auto-develop-design.md` §13, mark items 1
and 3 resolved with a line each on what was actually found — the verified `.pp3`
key table and whether the custom CUDA op needed excluding. Items 2, 4 and 5 were
closed at the CHECKPOINT.

Update the spec's **Status** line to `Implemented`.

- [ ] **Step 4: Final verification**

```bash
cargo fmt
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all
./target/release/photopipe doctor
```

Expected: all green, and `doctor` reports both RawTherapee and the look model.

- [ ] **Step 5: Commit**

```bash
git add README.md models/README.md docs/superpowers/specs/2026-07-29-auto-develop-design.md
git commit -m "docs: close out the automatic develop spec"
```

---

## Deferred to a later spec

Named here so they are not mistaken for gaps:

- **A Develop screen in `serve`.** `finish_folder` already reports through
  `ProgressSink`, so the remaining work is a `POST /api/finish` +
  `/api/finish/status` pair and a nav-rail screen reusing the analyze checklist.
- **Crop, rotation, straightening, and local adjustments.** Spec §12.
- **Learning the user's own look** from paired RAW/export files. Spec §12 and A7 —
  blocked on having such a corpus, not on the technique.
- **The vkdt renderer.** Spec §9. `EditRecipe` is already the stable contract it
  would sit behind.
