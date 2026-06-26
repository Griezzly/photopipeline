# Phase 7 — Polish: doctor, stats, info, docs Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Finish PhotoPipe's diagnostic and reporting surface — a full `doctor` health check, a `stats` summary, a per-file `info` JSON dump — and ship user docs (`README.md` + `photopipe.example.toml`).

**Architecture:** All real query logic lives in new `impl Catalog` methods in `crates/pipeline/src/catalog/mod.rs` (locking the private `Mutex<Connection>` exactly like the existing methods). The CLI handlers in `crates/cli/src/main.rs` stay thin: they call the catalog/model APIs and format output to stdout. `doctor` additionally probes the filesystem and (via the newly-added `sysinfo` crate) reports free disk space. `info` serialises a serde struct with `serde_json`. Tests target the library functions directly against a temp DuckDB catalog populated with directly-inserted synthetic rows; one CLI-level test spawns the built binary via `env!("CARGO_BIN_EXE_photopipe")` to confirm exit codes and JSON parseability.

**Tech Stack:** Rust (edition 2021, stable), DuckDB (`duckdb` crate, bundled), `clap`, `tracing`, `serde`, `toml`, plus two new permissive-licensed deps surfaced in Task 1 (`serde_json`, `sysinfo`).

## Global Constraints

- Edition 2021, stable Rust. `anyhow::Result` at CLI boundaries; `thiserror` types inside `pipeline`.
- DuckDB ONLY (no SQLite). Bulk writes go through ONE transaction per batch.
- No AGPL deps. No Python at runtime (Python only in `tools/` for one-time ONNX export).
- Non-destructive: never modify/move/delete an original photo. Symlinks/hardlinks/reads only. (Phase 7 reads only.)
- Idempotency is a correctness requirement: re-running a command on unchanged input does zero new work. (`doctor`/`stats`/`info` are pure reads — they are inherently idempotent.)
- `tracing` for logs (`info!`/`warn!`/`debug!`); no `println!` except intentional CLI user output — **`doctor`/`stats`/`info` are explicitly allowed to use `println!`** for their user-facing output.
- Run before declaring done: `cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D warnings`, `cargo test --all`.
- Surface (don't silently add) any new dependency or deviation from the spec.
- New `CatalogError` errors use `CatalogError::Db(e.to_string())`. `CatalogError` has variants `Db(String)` and `Migration { version: u32, reason: String }` only.
- New catalog methods lock the connection with `let conn = self.conn.lock().map_err(|_| CatalogError::Db("mutex poisoned".into()))?;`.
- Every commit message ends with: `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`
- In WSL, `source ~/.cargo/env` before any `cargo` command.

---

## File Structure

| File | Status | Responsibility |
|------|--------|----------------|
| `Cargo.toml` (workspace root) | Modify | Add `serde_json` and `sysinfo` to `[workspace.dependencies]`. |
| `crates/cli/Cargo.toml` | Modify | Depend on `serde_json` and `sysinfo` (workspace). |
| `crates/pipeline/src/catalog/mod.rs` | Modify | Add `schema_version()`, `CatalogStats` + `stats()` + `flag_counts()` + `per_camera_counts()` + `per_lens_counts()`, and `FileDump` + `dump_file()`. Add unit tests for each. |
| `crates/cli/src/main.rs` | Modify | Replace `cmd_info`/`cmd_stats` stubs with real impls; extend `cmd_doctor`; add helper `doctor_check_*` fns and a `Doctor` exit-code path. |
| `crates/cli/tests/cli.rs` | Create | Integration test: `photopipe info` on a known/unknown file (exit codes + JSON); `doctor` exit 0 healthy / non-zero with bad model dir. |
| `README.md` (repo root) | Modify (currently absent → create) | Quickstart: install, sample config, workflows scan→calibrate→dedupe→review-tree, command reference. |
| `photopipe.example.toml` (repo root) | Create | Full default config mirroring `config.rs` defaults exactly. |

**Task → deliverable map:**
- Task 1: dependencies surfaced + added; `Catalog::schema_version()`. (one commit)
- Task 2: `doctor` full implementation + exit codes + library check fns. (one commit)
- Task 3: `stats` catalog methods + `cmd_stats`. (one commit)
- Task 4: `info` `FileDump` + `dump_file()` + `cmd_info`. (one commit)
- Task 5: CLI integration test for `info` + `doctor` exit codes. (one commit)
- Task 6: `README.md` + `photopipe.example.toml`. (one commit)

---

## Task 1: Dependencies + `Catalog::schema_version()`

This task surfaces and adds the two new crates, then adds the smallest catalog method (`schema_version`) that the rest of the plan builds on.

**SURFACED DEVIATIONS — the user must approve before this task proceeds:**
- **`serde_json` (dual MIT/Apache-2.0)** — needed by Task 4 to serialise the `info` JSON dump. Ubiquitous, permissive, consistent with IMPLEMENTATION_PLAN §7 which specifies `info` emits JSON. Added to the **CLI crate only** (the pipeline crate exposes a plain serde-derive struct; serialisation happens in the CLI).
- **`sysinfo` (MIT)** — needed by Task 2's `doctor` to report free disk space (IMPLEMENTATION_PLAN §7/§8 "disk free space", §5.1 "Detect system RAM via `sysinfo`"). Cross-platform (Linux + macOS), no AGPL, no Python. Added to the **CLI crate only** (doctor lives in the CLI). Pinned to `0.33`.

If the user prefers a std-only disk-free approach instead of `sysinfo`, that is possible only via `libc::statvfs`/`GetDiskFreeSpaceEx` behind `#[cfg]` — more code, less portable, and still a new dep (`libc`/`windows-sys`). RECOMMENDATION: approve `sysinfo`.

**Files:**
- Modify: `Cargo.toml` (workspace root, `[workspace.dependencies]` block, after line 22)
- Modify: `crates/cli/Cargo.toml` (`[dependencies]` block)
- Modify: `crates/pipeline/src/catalog/mod.rs` (add method to `impl Catalog`, ~after `file_count` at line 364; add test in `mod tests`)

**Interfaces:**
- Consumes: existing `Catalog` (private `conn: Mutex<Connection>`), `CatalogError::Db(String)`.
- Produces:
  - `impl Catalog { pub fn schema_version(&self) -> Result<u32, CatalogError>; }`
  - Workspace deps `serde_json = "1"` and `sysinfo = "0.33"`.

- [ ] **Step 1: Add the two crates to the workspace dependency table**

In `Cargo.toml`, append two lines to the end of the `[workspace.dependencies]` block (after the `ndarray` line, line 22):

```toml
serde_json  = "1"
sysinfo     = "0.33"
```

- [ ] **Step 2: Wire both crates into the CLI crate**

In `crates/cli/Cargo.toml`, append to the `[dependencies]` block (after the `toml` line):

```toml
serde_json         = { workspace = true }
sysinfo            = { workspace = true }
```

- [ ] **Step 3: Verify the workspace still resolves and builds**

Run: `source ~/.cargo/env && cargo build -p photopipe`
Expected: builds successfully (the new deps download + compile; no code uses them yet).

- [ ] **Step 4: Write the failing test for `schema_version`**

Add this test inside the existing `mod tests` block in `crates/pipeline/src/catalog/mod.rs` (before the closing `}` of the module, after `files_needing_defect_analysis_filters_correctly`):

```rust
    #[test]
    fn schema_version_reports_current_migration() {
        let (catalog, _dir) = make_catalog();
        // The single migration (version 1) runs at open().
        assert_eq!(catalog.schema_version().unwrap(), 1);
    }
```

- [ ] **Step 5: Run the test to verify it fails**

Run: `source ~/.cargo/env && cargo test -p pipeline schema_version_reports_current_migration`
Expected: FAIL — compile error `no method named schema_version found for struct Catalog`.

- [ ] **Step 6: Implement `schema_version`**

Add this method to `impl Catalog` in `crates/pipeline/src/catalog/mod.rs`, immediately after the `file_count` method (after line 364):

```rust
    /// Return the highest applied schema migration version.
    ///
    /// `0` means no migrations have been recorded (a fresh, empty
    /// `schema_version` table); the current shipping schema is version `1`.
    pub fn schema_version(&self) -> Result<u32, CatalogError> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| CatalogError::Db("mutex poisoned".into()))?;
        let version: u32 = conn
            .query_row(
                "SELECT COALESCE(MAX(version), 0) FROM schema_version",
                [],
                |r| r.get(0),
            )
            .map_err(|e| CatalogError::Db(e.to_string()))?;
        Ok(version)
    }
```

- [ ] **Step 7: Run the test to verify it passes**

Run: `source ~/.cargo/env && cargo test -p pipeline schema_version_reports_current_migration`
Expected: PASS (1 passed).

- [ ] **Step 8: Format, lint, commit**

Run: `source ~/.cargo/env && cargo fmt && cargo clippy --all-targets --all-features -- -D warnings`
Expected: no warnings.

```bash
git add Cargo.toml Cargo.lock crates/cli/Cargo.toml crates/pipeline/src/catalog/mod.rs
git commit -m "$(cat <<'EOF'
chore(deps): add serde_json + sysinfo; feat(catalog): schema_version()

Surfaces serde_json (info JSON) and sysinfo (doctor disk-free) as new
permissive deps on the CLI crate. Adds Catalog::schema_version() for the
doctor schema-match check.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 2: `photopipe doctor` — full implementation

Extend the existing partial `cmd_doctor` to a real health check that returns a non-zero exit code on critical failures. Logic that is testable lives in small free functions returning a `DoctorCheck` so a future test (and human readers) can reason about pass/fail without spawning the binary.

**Files:**
- Modify: `crates/cli/src/main.rs` (rewrite `cmd_doctor` at lines 212-250; keep `doctor_model_file` lines 252-270 and `doctor_provider` lines 272-285; add new helper fns + a `DoctorCheck`/`CheckStatus` type; add `use` lines)

**Interfaces:**
- Consumes: `Catalog::open` + `Catalog::schema_version` (Task 1), `ModelHub::from_config(&cfg.models)`, `config::Config`, `sysinfo` (`Disks`), `cfg.catalog.{db_path,cache_dir}`, `cfg.models.{model_dir,embedder,iqa,detector}`.
- Produces: a `cmd_doctor(config_path, cfg) -> anyhow::Result<()>` that prints a check report and **returns `Err` (process exits non-zero) when any critical check fails**. No symbols consumed by later tasks except by the Task 5 test, which only relies on the binary's stdout + exit code.

The critical checks (failure ⇒ non-zero exit): (a) DB opens AND `schema_version()` == `EXPECTED_SCHEMA_VERSION` (1); (b) cache dir is writable; (c) every model that is *configured by name* in `cfg.models` has its `.onnx` file present in `model_dir`. Non-critical (warn only, exit 0): disk free < 5 GB, individual model fails to *load* (file present but ORT rejects it), provider not the one requested.

- [ ] **Step 1: Add the `CheckStatus` enum and `DoctorCheck` struct + constant**

In `crates/cli/src/main.rs`, add near the top (after the `use pipeline::config;` line at line 7):

```rust
use pipeline::catalog::Catalog;
use pipeline::models::ModelHub;

/// Schema version the binary expects the catalog to be at.
const EXPECTED_SCHEMA_VERSION: u32 = 1;
const MIN_FREE_DISK_GB: u64 = 5;

#[derive(Clone, Copy, PartialEq, Eq)]
enum CheckStatus {
    Ok,
    Warn,
    Fail,
}

impl CheckStatus {
    fn glyph(self) -> &'static str {
        match self {
            CheckStatus::Ok => "[ ok ]",
            CheckStatus::Warn => "[warn]",
            CheckStatus::Fail => "[fail]",
        }
    }
}

/// One diagnostic line. `critical` checks that `Fail` make `doctor` exit non-zero.
struct DoctorCheck {
    label: String,
    status: CheckStatus,
    detail: String,
    critical: bool,
}

impl DoctorCheck {
    fn ok(label: &str, detail: impl Into<String>) -> Self {
        Self { label: label.into(), status: CheckStatus::Ok, detail: detail.into(), critical: false }
    }
    fn warn(label: &str, detail: impl Into<String>) -> Self {
        Self { label: label.into(), status: CheckStatus::Warn, detail: detail.into(), critical: false }
    }
    fn fail_critical(label: &str, detail: impl Into<String>) -> Self {
        Self { label: label.into(), status: CheckStatus::Fail, detail: detail.into(), critical: true }
    }
    fn print(&self) {
        println!("{} {:<22} {}", self.status.glyph(), self.label, self.detail);
    }
}
```

- [ ] **Step 2: Add the schema check helper**

Add this free function to `crates/cli/src/main.rs` (place all the new `doctor_check_*` helpers just above the existing `fn doctor_model_file` at line 252):

```rust
/// Open the catalog and verify its schema version matches what we expect.
fn doctor_check_schema(db_path: &std::path::Path) -> DoctorCheck {
    if !db_path.exists() {
        // Not yet created is fine — `scan` creates it. Report, don't fail.
        return DoctorCheck::warn(
            "DB schema",
            format!("no catalog yet at {} (run `scan` to create)", db_path.display()),
        );
    }
    match Catalog::open(db_path) {
        Ok(catalog) => match catalog.schema_version() {
            Ok(v) if v == EXPECTED_SCHEMA_VERSION => {
                DoctorCheck::ok("DB schema", format!("version {v}"))
            }
            Ok(v) => DoctorCheck::fail_critical(
                "DB schema",
                format!("version {v}, expected {EXPECTED_SCHEMA_VERSION} — DB is from a different photopipe build"),
            ),
            Err(e) => DoctorCheck::fail_critical("DB schema", format!("cannot read schema version: {e}")),
        },
        Err(e) => DoctorCheck::fail_critical("DB schema", format!("cannot open catalog: {e}")),
    }
}
```

- [ ] **Step 3: Add the cache-writable check helper**

Add to `crates/cli/src/main.rs` (next to the other `doctor_check_*` fns):

```rust
/// Verify the cache directory exists (creating it) and is writable by
/// creating then removing a probe file.
fn doctor_check_cache_writable(cache_dir: &std::path::Path) -> DoctorCheck {
    if let Err(e) = std::fs::create_dir_all(cache_dir) {
        return DoctorCheck::fail_critical(
            "Cache writable",
            format!("cannot create {}: {e}", cache_dir.display()),
        );
    }
    let probe = cache_dir.join(".photopipe-doctor-probe");
    match std::fs::write(&probe, b"ok") {
        Ok(()) => {
            let _ = std::fs::remove_file(&probe);
            DoctorCheck::ok("Cache writable", cache_dir.display().to_string())
        }
        Err(e) => DoctorCheck::fail_critical(
            "Cache writable",
            format!("cannot write under {}: {e}", cache_dir.display()),
        ),
    }
}
```

- [ ] **Step 4: Add the disk-free check helper (uses `sysinfo`)**

Add to `crates/cli/src/main.rs`:

```rust
/// Report free space on the filesystem that holds `path`. Non-critical:
/// warns when below MIN_FREE_DISK_GB but never fails the run.
fn doctor_check_disk_free(path: &std::path::Path) -> DoctorCheck {
    use sysinfo::Disks;

    let disks = Disks::new_with_refreshed_list();
    // Pick the disk whose mount point is the longest prefix of `path`
    // (the most specific mount). Fall back to the max available if none match.
    let target = path.to_path_buf();
    let best = disks
        .list()
        .iter()
        .filter(|d| target.starts_with(d.mount_point()))
        .max_by_key(|d| d.mount_point().as_os_str().len())
        .or_else(|| disks.list().iter().max_by_key(|d| d.available_space()));

    match best {
        Some(d) => {
            let free_gb = d.available_space() / 1_073_741_824;
            let detail = format!("{free_gb} GB free on {}", d.mount_point().display());
            if free_gb >= MIN_FREE_DISK_GB {
                DoctorCheck::ok("Disk free", detail)
            } else {
                DoctorCheck::warn("Disk free", format!("{detail} (< {MIN_FREE_DISK_GB} GB)"))
            }
        }
        None => DoctorCheck::warn("Disk free", "could not determine free space".to_string()),
    }
}
```

- [ ] **Step 5: Add the model presence + load check helper**

Add to `crates/cli/src/main.rs`. This maps each configured model *name* to its expected ONNX filename, reports presence (critical) and — when present — whether the hub actually loaded it (non-critical):

```rust
/// For each model configured by name, check the ONNX file is present
/// (critical) and whether the loaded hub populated the slot (non-critical).
fn doctor_check_models(cfg: &config::ModelsConfig, hub: &ModelHub) -> Vec<DoctorCheck> {
    // (config name, expected filename, slot-loaded predicate, role label)
    let specs: [(&str, &str, bool); 3] = [
        (cfg.embedder.as_str(), "dinov2_base.onnx", hub.embedder.is_some()),
        (cfg.iqa.as_str(), "clip_iqa.onnx", hub.iqa.is_some()),
        (cfg.detector.as_str(), "rt_detr_l.onnx", hub.detector.is_some()),
    ];
    let roles = ["embedder", "iqa", "detector"];

    let mut checks = Vec::new();
    for ((name, filename, loaded), role) in specs.into_iter().zip(roles) {
        let label = format!("Model {role}");
        let path = cfg.model_dir.join(filename);
        if !path.exists() {
            checks.push(DoctorCheck::fail_critical(
                &label,
                format!("'{name}' configured but {} missing", path.display()),
            ));
        } else if loaded {
            checks.push(DoctorCheck::ok(&label, format!("'{name}' loaded ({filename})")));
        } else {
            checks.push(DoctorCheck::warn(
                &label,
                format!("'{name}' file present but failed to load ({filename})"),
            ));
        }
    }
    checks
}
```

- [ ] **Step 6: Rewrite `cmd_doctor` to run all checks and set the exit code**

Replace the entire body of `cmd_doctor` (lines 212-250) in `crates/cli/src/main.rs` with:

```rust
fn cmd_doctor(config_path: &std::path::Path, cfg: &config::Config) -> Result<()> {
    println!("PhotoPipe Doctor");
    println!("================");
    println!();
    println!(
        "OS:           {} ({})",
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    println!("Family:       {}", std::env::consts::FAMILY);
    println!("Config file:  {}", config_path.display());
    println!("Exists:       {}", config_path.exists());
    println!("Model dir:    {}", cfg.models.model_dir.display());
    println!("Provider:     {}", doctor_provider(cfg.models.device));

    #[cfg(target_os = "macos")]
    println!(
        "  [macOS] CoreML EP disabled (ort rc.12 incompatibility with external-data models); \
         using CPU — revisit when ort ≥ 2.0.0 stable"
    );
    println!();

    println!("Health checks");
    println!("-------------");

    let mut checks: Vec<DoctorCheck> = Vec::new();
    checks.push(doctor_check_schema(&cfg.catalog.db_path));
    checks.push(doctor_check_cache_writable(&cfg.catalog.cache_dir));
    checks.push(doctor_check_disk_free(&cfg.catalog.db_path));

    // Actually attempt to load models so we report which slots came up.
    match ModelHub::from_config(&cfg.models) {
        Ok(hub) => {
            println!("(ORT execution provider in use: {})", hub.provider);
            checks.extend(doctor_check_models(&cfg.models, &hub));
        }
        Err(e) => {
            checks.push(DoctorCheck::fail_critical(
                "Models",
                format!("ModelHub::from_config failed: {e}"),
            ));
        }
    }

    for c in &checks {
        c.print();
    }
    println!();

    let failed = checks.iter().any(|c| c.critical && c.status == CheckStatus::Fail);
    if failed {
        println!("Result: UNHEALTHY — fix the [fail] items above.");
        anyhow::bail!("doctor: one or more critical checks failed");
    }
    println!("Result: healthy.");
    Ok(())
}
```

- [ ] **Step 7: Build and run doctor manually to confirm exit code 0 on a healthy setup**

Run:
```
source ~/.cargo/env && cargo build -p photopipe && ./target/debug/photopipe doctor; echo "exit=$?"
```
Expected: prints the check table; `exit=0` (on the dev box the model files exist; if a configured model file is absent locally it will correctly print `exit=1` — that is the intended behaviour and is exercised by Task 5).

- [ ] **Step 8: Format, lint**

Run: `source ~/.cargo/env && cargo fmt && cargo clippy --all-targets --all-features -- -D warnings`
Expected: no warnings. (If clippy flags the unused `doctor_model_file` now that `cmd_doctor` no longer calls it, delete `doctor_model_file` lines 252-270 — it has been superseded by `doctor_check_models` — and re-run.)

- [ ] **Step 9: Commit**

```bash
git add crates/cli/src/main.rs
git commit -m "$(cat <<'EOF'
feat(cli): full doctor health check with exit codes

Doctor now verifies DB schema version, cache-dir writability, free disk
space (via sysinfo), and that every configured model is present and
loadable (attempts ModelHub::from_config). Exits non-zero on critical
failures (schema mismatch, cache not writable, missing configured model).

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 3: `photopipe stats` — catalog stats methods + CLI

Add structured stats to the catalog, then a readable table to the CLI.

**Files:**
- Modify: `crates/pipeline/src/catalog/mod.rs` (add `CatalogStats` struct + `stats()`, `flag_counts()`, `per_camera_counts()`, `per_lens_counts()` to `impl Catalog`; add tests)
- Modify: `crates/cli/src/main.rs` (replace `cmd_stats` stub at lines 206-210)

**Interfaces:**
- Consumes: existing `Catalog` connection; `cfg.catalog.{db_path,cache_dir}` for disk-usage; the directory-size helper defined below.
- Produces:
  ```rust
  pub struct CatalogStats {
      pub total_files: i64,
      pub duplicate_group_count: i64,
      pub grouped_file_count: i64,
      pub embedding_count: i64,
      pub iqa_count: i64,
  }
  impl Catalog {
      pub fn stats(&self) -> Result<CatalogStats, CatalogError>;
      pub fn flag_counts(&self) -> Result<Vec<(String, i64)>, CatalogError>;
      pub fn per_camera_counts(&self) -> Result<Vec<(String, i64)>, CatalogError>;
      pub fn per_lens_counts(&self) -> Result<Vec<(String, String, i64)>, CatalogError>;
  }
  ```
  `flag_counts` returns `(flag_type, count)` over all `defect_flags`. `per_camera_counts` returns `(camera_model_or_unknown, count)`. `per_lens_counts` returns `(camera_model_or_unknown, lens_model_or_unknown, count)`.

- [ ] **Step 1: Write failing tests for the four stats methods**

Add to the `mod tests` block in `crates/pipeline/src/catalog/mod.rs` (after the `schema_version_reports_current_migration` test from Task 1). These helpers insert synthetic rows directly via the locked connection — no real photos:

```rust
    /// Insert a file with the given path/hash and return its id.
    fn insert_file(catalog: &Catalog, path: &str, hash: u128) -> i64 {
        use crate::ingest::{ExifData, FileFormat, IngestedFile};
        let file = IngestedFile {
            path: PathBuf::from(path),
            content_hash: hash,
            size: 1234,
            mtime_ns: 1,
            format: FileFormat::Jpg,
            has_sidecar_jpg: false,
        };
        catalog.flush_batch(&[(file, None::<ExifData>)]).unwrap()[0]
    }

    #[test]
    fn stats_counts_files_and_groups() {
        let (catalog, _dir) = make_catalog();
        let a = insert_file(&catalog, "/p/a.jpg", 1);
        let b = insert_file(&catalog, "/p/b.jpg", 2);
        let _c = insert_file(&catalog, "/p/c.jpg", 3);

        // One duplicate group with two members (a, b).
        {
            let conn = catalog.conn.lock().unwrap();
            conn.execute_batch(
                "INSERT INTO duplicate_groups (method, created_at) VALUES ('test', 0);",
            )
            .unwrap();
            let gid: i64 = conn
                .query_row("SELECT MAX(id) FROM duplicate_groups", [], |r| r.get(0))
                .unwrap();
            conn.execute(
                "INSERT INTO duplicate_members (group_id, file_id, is_suggested_keeper, quality_score)
                 VALUES (?, ?, true, 1.0)",
                duckdb::params![gid, a],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO duplicate_members (group_id, file_id, is_suggested_keeper, quality_score)
                 VALUES (?, ?, false, 0.5)",
                duckdb::params![gid, b],
            )
            .unwrap();
        }

        let s = catalog.stats().unwrap();
        assert_eq!(s.total_files, 3);
        assert_eq!(s.duplicate_group_count, 1);
        assert_eq!(s.grouped_file_count, 2);
        assert_eq!(s.embedding_count, 0);
        assert_eq!(s.iqa_count, 0);
    }

    #[test]
    fn flag_counts_groups_by_type() {
        let (catalog, _dir) = make_catalog();
        let a = insert_file(&catalog, "/p/a.jpg", 1);
        let b = insert_file(&catalog, "/p/b.jpg", 2);
        {
            let conn = catalog.conn.lock().unwrap();
            for (fid, ft) in [(a, "blur"), (b, "blur"), (a, "overexposed")] {
                conn.execute(
                    "INSERT INTO defect_flags (file_id, flag_type, confidence, reason)
                     VALUES (?, ?, 0.9, 'r')",
                    duckdb::params![fid, ft],
                )
                .unwrap();
            }
        }
        let mut counts = catalog.flag_counts().unwrap();
        counts.sort();
        assert_eq!(counts, vec![("blur".to_string(), 2), ("overexposed".to_string(), 1)]);
    }

    #[test]
    fn per_camera_and_per_lens_counts() {
        use crate::ingest::ExifData;
        let (catalog, _dir) = make_catalog();
        let a = insert_file(&catalog, "/p/a.jpg", 1);
        let b = insert_file(&catalog, "/p/b.jpg", 2);
        let exif = ExifData {
            captured_at: Some(1),
            camera_make: Some("TestMake".into()),
            camera_model: Some("CamX".into()),
            lens_model: Some("Lens50".into()),
            focal_length_mm: Some(50.0),
            aperture: Some(2.8),
            iso: Some(200),
            shutter_seconds: Some(0.01),
            width: Some(100),
            height: Some(100),
            orientation: Some(1),
        };
        catalog.upsert_exif(a, &exif).unwrap();
        catalog.upsert_exif(b, &exif).unwrap();

        let cams = catalog.per_camera_counts().unwrap();
        assert_eq!(cams, vec![("CamX".to_string(), 2)]);

        let lenses = catalog.per_lens_counts().unwrap();
        assert_eq!(lenses, vec![("CamX".to_string(), "Lens50".to_string(), 2)]);
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `source ~/.cargo/env && cargo test -p pipeline stats_counts_files_and_groups flag_counts_groups_by_type per_camera_and_per_lens_counts`
Expected: FAIL — compile errors (`CatalogStats`, `stats`, `flag_counts`, `per_camera_counts`, `per_lens_counts` not found).

- [ ] **Step 3: Add the `CatalogStats` struct**

In `crates/pipeline/src/catalog/mod.rs`, add this struct just below the existing `MlRow` struct (after line 17, before `pub struct Catalog`):

```rust
/// Aggregate catalog counts for `photopipe stats`.
pub struct CatalogStats {
    pub total_files: i64,
    pub duplicate_group_count: i64,
    /// Distinct files that belong to at least one duplicate group.
    pub grouped_file_count: i64,
    pub embedding_count: i64,
    pub iqa_count: i64,
}
```

- [ ] **Step 4: Implement the four methods**

Add to `impl Catalog` in `crates/pipeline/src/catalog/mod.rs`, after the `iqa_count` method (after line 760, before the closing `}` of the impl block):

```rust
    /// Aggregate counts for the `stats` command.
    pub fn stats(&self) -> Result<CatalogStats, CatalogError> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| CatalogError::Db("mutex poisoned".into()))?;
        let total_files: i64 = conn
            .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))
            .map_err(|e| CatalogError::Db(e.to_string()))?;
        let duplicate_group_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM duplicate_groups", [], |r| r.get(0))
            .map_err(|e| CatalogError::Db(e.to_string()))?;
        let grouped_file_count: i64 = conn
            .query_row(
                "SELECT COUNT(DISTINCT file_id) FROM duplicate_members",
                [],
                |r| r.get(0),
            )
            .map_err(|e| CatalogError::Db(e.to_string()))?;
        let embedding_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM embeddings", [], |r| r.get(0))
            .map_err(|e| CatalogError::Db(e.to_string()))?;
        let iqa_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM iqa", [], |r| r.get(0))
            .map_err(|e| CatalogError::Db(e.to_string()))?;
        Ok(CatalogStats {
            total_files,
            duplicate_group_count,
            grouped_file_count,
            embedding_count,
            iqa_count,
        })
    }

    /// Count of each defect flag type, e.g. `("blur", 12)`.
    pub fn flag_counts(&self) -> Result<Vec<(String, i64)>, CatalogError> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| CatalogError::Db("mutex poisoned".into()))?;
        let mut stmt = conn
            .prepare(
                "SELECT flag_type, COUNT(*) FROM defect_flags
                 GROUP BY flag_type ORDER BY flag_type",
            )
            .map_err(|e| CatalogError::Db(e.to_string()))?;
        let rows = stmt
            .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)))
            .map_err(|e| CatalogError::Db(e.to_string()))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| CatalogError::Db(e.to_string()))?);
        }
        Ok(out)
    }

    /// File count per camera model (NULL model reported as "(unknown)").
    pub fn per_camera_counts(&self) -> Result<Vec<(String, i64)>, CatalogError> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| CatalogError::Db("mutex poisoned".into()))?;
        let mut stmt = conn
            .prepare(
                "SELECT COALESCE(camera_model, '(unknown)'), COUNT(*)
                 FROM exif GROUP BY camera_model ORDER BY COUNT(*) DESC, 1",
            )
            .map_err(|e| CatalogError::Db(e.to_string()))?;
        let rows = stmt
            .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)))
            .map_err(|e| CatalogError::Db(e.to_string()))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| CatalogError::Db(e.to_string()))?);
        }
        Ok(out)
    }

    /// File count per (camera model, lens model) pair (NULLs → "(unknown)").
    pub fn per_lens_counts(&self) -> Result<Vec<(String, String, i64)>, CatalogError> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| CatalogError::Db("mutex poisoned".into()))?;
        let mut stmt = conn
            .prepare(
                "SELECT COALESCE(camera_model, '(unknown)'),
                        COALESCE(lens_model, '(unknown)'), COUNT(*)
                 FROM exif GROUP BY camera_model, lens_model
                 ORDER BY COUNT(*) DESC, 1, 2",
            )
            .map_err(|e| CatalogError::Db(e.to_string()))?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })
            .map_err(|e| CatalogError::Db(e.to_string()))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| CatalogError::Db(e.to_string()))?);
        }
        Ok(out)
    }
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `source ~/.cargo/env && cargo test -p pipeline stats_counts_files_and_groups flag_counts_groups_by_type per_camera_and_per_lens_counts`
Expected: PASS (3 passed).

- [ ] **Step 6: Implement `cmd_stats` in the CLI**

In `crates/cli/src/main.rs`, replace the `cmd_stats` stub (lines 206-210) with:

```rust
fn cmd_stats(cfg: &config::Config) -> Result<()> {
    let catalog =
        Catalog::open(&cfg.catalog.db_path).map_err(|e| anyhow::anyhow!("catalog: {}", e))?;

    let s = catalog.stats().map_err(|e| anyhow::anyhow!("stats: {}", e))?;
    let flags = catalog.flag_counts().map_err(|e| anyhow::anyhow!("flags: {}", e))?;
    let cameras = catalog
        .per_camera_counts()
        .map_err(|e| anyhow::anyhow!("cameras: {}", e))?;
    let lenses = catalog
        .per_lens_counts()
        .map_err(|e| anyhow::anyhow!("lenses: {}", e))?;

    let db_size = file_size(&cfg.catalog.db_path);
    let cache_size = dir_size(&cfg.catalog.cache_dir);

    println!("PhotoPipe Stats");
    println!("===============");
    println!("Total files          : {}", s.total_files);
    println!("Embeddings           : {}", s.embedding_count);
    println!("IQA scores           : {}", s.iqa_count);
    println!("Duplicate groups     : {}", s.duplicate_group_count);
    println!("Files in groups      : {}", s.grouped_file_count);
    println!();
    println!("Defect flags");
    println!("------------");
    if flags.is_empty() {
        println!("  (none)");
    } else {
        for (ft, n) in &flags {
            println!("  {ft:<14} {n}");
        }
    }
    println!();
    println!("Per camera");
    println!("----------");
    if cameras.is_empty() {
        println!("  (no EXIF)");
    } else {
        for (cam, n) in &cameras {
            println!("  {cam:<28} {n}");
        }
    }
    println!();
    println!("Per lens");
    println!("--------");
    if lenses.is_empty() {
        println!("  (no EXIF)");
    } else {
        for (cam, lens, n) in &lenses {
            println!("  {cam} / {lens:<28} {n}");
        }
    }
    println!();
    println!("Disk usage");
    println!("----------");
    println!("  Catalog : {:.1} MB ({})", db_size as f64 / 1_048_576.0, cfg.catalog.db_path.display());
    println!("  Cache   : {:.1} MB ({})", cache_size as f64 / 1_048_576.0, cfg.catalog.cache_dir.display());
    Ok(())
}

/// Size in bytes of a single file, or 0 if it can't be read.
fn file_size(path: &std::path::Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

/// Recursive byte size of a directory tree, ignoring entries it can't read.
fn dir_size(dir: &std::path::Path) -> u64 {
    let mut total = 0;
    let read = match std::fs::read_dir(dir) {
        Ok(r) => r,
        Err(_) => return 0,
    };
    for entry in read.flatten() {
        let Ok(meta) = entry.metadata() else { continue };
        if meta.is_dir() {
            total += dir_size(&entry.path());
        } else {
            total += meta.len();
        }
    }
    total
}
```

- [ ] **Step 7: Build and lint**

Run: `source ~/.cargo/env && cargo build -p photopipe && cargo fmt && cargo clippy --all-targets --all-features -- -D warnings`
Expected: builds; no warnings.

- [ ] **Step 8: Commit**

```bash
git add crates/pipeline/src/catalog/mod.rs crates/cli/src/main.rs
git commit -m "$(cat <<'EOF'
feat(catalog): CatalogStats + flag/camera/lens counts; feat(cli): stats

Adds Catalog::stats/flag_counts/per_camera_counts/per_lens_counts and a
readable `photopipe stats` table including catalog/cache disk usage.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 4: `photopipe info <FILE>` — `FileDump` + `dump_file()` + CLI

Add a serde-serialisable dump of every catalog row for one file; serialise it to JSON in the CLI.

**Files:**
- Modify: `crates/pipeline/src/catalog/mod.rs` (add `FileDump` + sub-structs, derive `serde::Serialize`; add `dump_file()`; add tests). Note: `serde` is already a pipeline dependency.
- Modify: `crates/cli/src/main.rs` (replace `cmd_info` stub at lines 200-204)

**Interfaces:**
- Consumes: existing `Catalog` connection.
- Produces:
  ```rust
  #[derive(serde::Serialize)]
  pub struct FileDump {
      pub file: FileRowDump,
      pub exif: Option<ExifRowDump>,
      pub sharpness: Option<SharpnessRowDump>,
      pub exposure: Option<ExposureRowDump>,
      pub iqa: Option<IqaRowDump>,
      /// Embedding model name + vector dimension (vector itself omitted — too large for `info`).
      pub embedding: Option<EmbeddingDump>,
      pub defect_flags: Vec<DefectFlagDump>,
      pub duplicate_groups: Vec<i64>,
  }
  impl Catalog { pub fn dump_file(&self, path: &Path) -> Result<Option<FileDump>, CatalogError>; }
  ```
  `dump_file` returns `Ok(None)` when no `files` row matches `path`.

- [ ] **Step 1: Write the failing test for `dump_file`**

Add to the `mod tests` block in `crates/pipeline/src/catalog/mod.rs`:

```rust
    #[test]
    fn dump_file_present_and_absent() {
        use crate::defect::{DefectFlag, ExposureResult, SharpnessResult};
        use crate::ingest::ExifData;

        let (catalog, _dir) = make_catalog();
        let id = insert_file(&catalog, "/p/known.jpg", 7);

        let exif = ExifData {
            captured_at: Some(1686830400),
            camera_make: Some("TestMake".into()),
            camera_model: Some("CamX".into()),
            lens_model: Some("Lens50".into()),
            focal_length_mm: Some(50.0),
            aperture: Some(2.8),
            iso: Some(200),
            shutter_seconds: Some(0.01),
            width: Some(100),
            height: Some(100),
            orientation: Some(1),
        };
        catalog.upsert_exif(id, &exif).unwrap();
        catalog
            .upsert_sharpness(
                id,
                &SharpnessResult {
                    s_global: 12.5,
                    s_subject: Some(20.0),
                    s_background: Some(5.0),
                    subject_ratio: Some(0.3),
                    detector_used: "rt-detr-l".into(),
                },
            )
            .unwrap();
        catalog
            .upsert_exposure(
                id,
                &ExposureResult {
                    clipped_highlights: 0.01,
                    clipped_shadows: 0.02,
                    mean_luma: 0.5,
                    histogram_skew: 0.0,
                },
            )
            .unwrap();
        catalog
            .upsert_defect_flag(
                id,
                &DefectFlag { flag_type: "blur".into(), confidence: 0.9, reason: "low".into() },
            )
            .unwrap();
        catalog
            .flush_ml_batch(&[MlRow {
                file_id: id,
                embedding: Some(("dinov2-base".into(), vec![0.1, 0.2, 0.3])),
                iqa_score: Some(("clip-iqa".into(), 0.75)),
            }])
            .unwrap();

        let dump = catalog
            .dump_file(&PathBuf::from("/p/known.jpg"))
            .unwrap()
            .expect("file should be present");
        assert_eq!(dump.file.path, "/p/known.jpg");
        assert_eq!(dump.exif.as_ref().unwrap().camera_model.as_deref(), Some("CamX"));
        assert!((dump.sharpness.as_ref().unwrap().s_global - 12.5).abs() < 1e-4);
        assert!(dump.exposure.is_some());
        assert_eq!(dump.iqa.as_ref().unwrap().model, "clip-iqa");
        assert_eq!(dump.embedding.as_ref().unwrap().dim, 3);
        assert_eq!(dump.defect_flags.len(), 1);
        assert_eq!(dump.defect_flags[0].flag_type, "blur");

        // Unknown file → None.
        assert!(catalog
            .dump_file(&PathBuf::from("/p/missing.jpg"))
            .unwrap()
            .is_none());
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `source ~/.cargo/env && cargo test -p pipeline dump_file_present_and_absent`
Expected: FAIL — `FileDump`/`dump_file` not found.

- [ ] **Step 3: Add the `FileDump` structs**

In `crates/pipeline/src/catalog/mod.rs`, add the import and structs near the top, right after `use crate::error::CatalogError;` (line 5). `serde::Serialize` is available since `serde` (with `derive`) is a workspace dep already used in `config.rs`:

```rust
/// Full catalog dump of one file for `photopipe info`. JSON-serialisable.
#[derive(serde::Serialize)]
pub struct FileDump {
    pub file: FileRowDump,
    pub exif: Option<ExifRowDump>,
    pub sharpness: Option<SharpnessRowDump>,
    pub exposure: Option<ExposureRowDump>,
    pub iqa: Option<IqaRowDump>,
    pub embedding: Option<EmbeddingDump>,
    pub defect_flags: Vec<DefectFlagDump>,
    /// Ids of duplicate groups this file belongs to.
    pub duplicate_groups: Vec<i64>,
}

#[derive(serde::Serialize)]
pub struct FileRowDump {
    pub id: i64,
    pub path: String,
    pub content_hash: String,
    pub size_bytes: i64,
    pub mtime_ns: i64,
    pub file_format: String,
    pub has_sidecar_jpg: bool,
    pub last_processed: i64,
}

#[derive(serde::Serialize)]
pub struct ExifRowDump {
    pub captured_at: Option<i64>,
    pub camera_make: Option<String>,
    pub camera_model: Option<String>,
    pub lens_model: Option<String>,
    pub focal_length_mm: Option<f32>,
    pub aperture: Option<f32>,
    pub iso: Option<i32>,
    pub shutter_seconds: Option<f32>,
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub orientation: Option<i16>,
}

#[derive(serde::Serialize)]
pub struct SharpnessRowDump {
    pub s_global: f32,
    pub s_subject: Option<f32>,
    pub s_background: Option<f32>,
    pub subject_ratio: Option<f32>,
    pub detector_used: Option<String>,
}

#[derive(serde::Serialize)]
pub struct ExposureRowDump {
    pub clipped_highlights: f32,
    pub clipped_shadows: f32,
    pub mean_luma: f32,
    pub histogram_skew: f32,
}

#[derive(serde::Serialize)]
pub struct IqaRowDump {
    pub model: String,
    pub score: f32,
}

#[derive(serde::Serialize)]
pub struct EmbeddingDump {
    pub model: String,
    pub dim: usize,
}

#[derive(serde::Serialize)]
pub struct DefectFlagDump {
    pub flag_type: String,
    pub confidence: f32,
    pub reason: Option<String>,
}
```

- [ ] **Step 4: Implement `dump_file`**

Add to `impl Catalog` in `crates/pipeline/src/catalog/mod.rs`, after the `per_lens_counts` method from Task 3 (before the closing `}` of the impl). The embedding row is read back as a `Vec<f32>` via `r.get::<_, Vec<f32>>` — duckdb-rs maps `FLOAT[]` to `Vec<f32>`. If that extraction errors at runtime, the fallback noted in the next step applies.

```rust
    /// Dump every catalog row associated with `path`. Returns `None` when no
    /// `files` row matches.
    pub fn dump_file(&self, path: &Path) -> Result<Option<FileDump>, CatalogError> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| CatalogError::Db("mutex poisoned".into()))?;
        let path_str = path.to_string_lossy();

        // files row (also gives us the id for the remaining lookups).
        let file = conn.query_row(
            "SELECT id, path, content_hash, size_bytes, mtime_ns, file_format,
                    has_sidecar_jpg, last_processed
             FROM files WHERE path = ?",
            duckdb::params![path_str.as_ref()],
            |r| {
                Ok(FileRowDump {
                    id: r.get(0)?,
                    path: r.get(1)?,
                    content_hash: r.get(2)?,
                    size_bytes: r.get(3)?,
                    mtime_ns: r.get(4)?,
                    file_format: r.get(5)?,
                    has_sidecar_jpg: r.get(6)?,
                    last_processed: r.get(7)?,
                })
            },
        );
        let file = match file {
            Ok(f) => f,
            Err(duckdb::Error::QueryReturnedNoRows) => return Ok(None),
            Err(e) => return Err(CatalogError::Db(e.to_string())),
        };
        let file_id = file.id;

        let exif = optional_row(conn.query_row(
            "SELECT captured_at, camera_make, camera_model, lens_model, focal_length_mm,
                    aperture, iso, shutter_seconds, width, height, orientation
             FROM exif WHERE file_id = ?",
            duckdb::params![file_id],
            |r| {
                Ok(ExifRowDump {
                    captured_at: r.get(0)?,
                    camera_make: r.get(1)?,
                    camera_model: r.get(2)?,
                    lens_model: r.get(3)?,
                    focal_length_mm: r.get(4)?,
                    aperture: r.get(5)?,
                    iso: r.get(6)?,
                    shutter_seconds: r.get(7)?,
                    width: r.get(8)?,
                    height: r.get(9)?,
                    orientation: r.get(10)?,
                })
            },
        ))?;

        let sharpness = optional_row(conn.query_row(
            "SELECT s_global, s_subject, s_background, subject_ratio, detector_used
             FROM sharpness WHERE file_id = ?",
            duckdb::params![file_id],
            |r| {
                Ok(SharpnessRowDump {
                    s_global: r.get(0)?,
                    s_subject: r.get(1)?,
                    s_background: r.get(2)?,
                    subject_ratio: r.get(3)?,
                    detector_used: r.get(4)?,
                })
            },
        ))?;

        let exposure = optional_row(conn.query_row(
            "SELECT clipped_highlights, clipped_shadows, mean_luma, histogram_skew
             FROM exposure WHERE file_id = ?",
            duckdb::params![file_id],
            |r| {
                Ok(ExposureRowDump {
                    clipped_highlights: r.get(0)?,
                    clipped_shadows: r.get(1)?,
                    mean_luma: r.get(2)?,
                    histogram_skew: r.get(3)?,
                })
            },
        ))?;

        let iqa = optional_row(conn.query_row(
            "SELECT model, score FROM iqa WHERE file_id = ?",
            duckdb::params![file_id],
            |r| Ok(IqaRowDump { model: r.get(0)?, score: r.get(1)? }),
        ))?;

        let embedding = optional_row(conn.query_row(
            "SELECT model, vector FROM embeddings WHERE file_id = ?",
            duckdb::params![file_id],
            |r| {
                let model: String = r.get(0)?;
                let vec: Vec<f32> = r.get(1)?;
                Ok(EmbeddingDump { model, dim: vec.len() })
            },
        ))?;

        let mut flag_stmt = conn
            .prepare(
                "SELECT flag_type, confidence, reason FROM defect_flags
                 WHERE file_id = ? ORDER BY flag_type",
            )
            .map_err(|e| CatalogError::Db(e.to_string()))?;
        let flag_rows = flag_stmt
            .query_map(duckdb::params![file_id], |r| {
                Ok(DefectFlagDump {
                    flag_type: r.get(0)?,
                    confidence: r.get(1)?,
                    reason: r.get(2)?,
                })
            })
            .map_err(|e| CatalogError::Db(e.to_string()))?;
        let mut defect_flags = Vec::new();
        for r in flag_rows {
            defect_flags.push(r.map_err(|e| CatalogError::Db(e.to_string()))?);
        }

        let mut grp_stmt = conn
            .prepare("SELECT group_id FROM duplicate_members WHERE file_id = ? ORDER BY group_id")
            .map_err(|e| CatalogError::Db(e.to_string()))?;
        let grp_rows = grp_stmt
            .query_map(duckdb::params![file_id], |r| r.get::<_, i64>(0))
            .map_err(|e| CatalogError::Db(e.to_string()))?;
        let mut duplicate_groups = Vec::new();
        for r in grp_rows {
            duplicate_groups.push(r.map_err(|e| CatalogError::Db(e.to_string()))?);
        }

        Ok(Some(FileDump {
            file,
            exif,
            sharpness,
            exposure,
            iqa,
            embedding,
            defect_flags,
            duplicate_groups,
        }))
    }
```

- [ ] **Step 5: Add the `optional_row` free helper**

Add this `pub(crate)`-free helper at module scope in `crates/pipeline/src/catalog/mod.rs` (after the struct definitions, before `impl Catalog`):

```rust
/// Map a single-row query result into `Option`, treating "no rows" as `None`.
fn optional_row<T>(result: Result<T, duckdb::Error>) -> Result<Option<T>, CatalogError> {
    match result {
        Ok(v) => Ok(Some(v)),
        Err(duckdb::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(CatalogError::Db(e.to_string())),
    }
}
```

- [ ] **Step 6: Run the test to verify it passes**

Run: `source ~/.cargo/env && cargo test -p pipeline dump_file_present_and_absent`
Expected: PASS.

If it FAILS on the embedding read with a DuckDB type error (`vector: Vec<f32>`), the `FLOAT[]` round-trip needs the explicit-cast read instead. Change ONLY the embedding query to:
```rust
        let embedding = optional_row(conn.query_row(
            "SELECT model, array_to_string(vector, ',') FROM embeddings WHERE file_id = ?",
            duckdb::params![file_id],
            |r| {
                let model: String = r.get(0)?;
                let joined: String = r.get(1)?;
                let dim = if joined.is_empty() { 0 } else { joined.split(',').count() };
                Ok(EmbeddingDump { model, dim })
            },
        ))?;
```
Re-run the test; it must pass with one of the two forms. Keep whichever compiles and passes; delete the other.

- [ ] **Step 7: Implement `cmd_info` in the CLI**

In `crates/cli/src/main.rs`, replace the `cmd_info` stub (lines 200-204) with:

```rust
fn cmd_info(file: PathBuf, cfg: &config::Config) -> Result<()> {
    let catalog =
        Catalog::open(&cfg.catalog.db_path).map_err(|e| anyhow::anyhow!("catalog: {}", e))?;
    match catalog
        .dump_file(&file)
        .map_err(|e| anyhow::anyhow!("info: {}", e))?
    {
        Some(dump) => {
            let json = serde_json::to_string_pretty(&dump)?;
            println!("{json}");
            Ok(())
        }
        None => {
            anyhow::bail!("no catalog entry for {}", file.display());
        }
    }
}
```

- [ ] **Step 8: Build, format, lint**

Run: `source ~/.cargo/env && cargo build -p photopipe && cargo fmt && cargo clippy --all-targets --all-features -- -D warnings`
Expected: builds; no warnings.

- [ ] **Step 9: Commit**

```bash
git add crates/pipeline/src/catalog/mod.rs crates/cli/src/main.rs
git commit -m "$(cat <<'EOF'
feat(catalog): FileDump + dump_file(); feat(cli): info JSON output

`photopipe info <FILE>` prints a pretty JSON dump of all catalog rows for
one file (files/exif/sharpness/exposure/iqa/embedding-dim/flags/dup-groups)
and exits non-zero when the file isn't catalogued.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 5: CLI integration test — `info` + `doctor` exit codes

Spawn the built binary to confirm end-to-end behaviour: exit codes and JSON parseability. The binary path comes from `env!("CARGO_BIN_EXE_photopipe")`, which Cargo sets for integration tests of the crate that defines the binary. We point the binary at a throwaway config so it uses a temp catalog/cache and never touches the user's real data.

**Files:**
- Create: `crates/cli/tests/cli.rs`

**Interfaces:**
- Consumes: the built `photopipe` binary; `Catalog::open`/`flush_batch` from `pipeline` (a dev-dependency we add) to seed a known file; `tempfile` (dev-dep).
- Produces: nothing consumed by later tasks.

- [ ] **Step 1: Add dev-dependencies the test needs to the CLI crate**

In `crates/cli/Cargo.toml`, append a dev-deps section (the CLI crate has none yet):

```toml
[dev-dependencies]
pipeline = { path = "../pipeline" }
tempfile = { workspace = true }
serde_json = { workspace = true }
```

(`pipeline` is already a normal dep, but listing it under dev-deps is harmless and makes the test's intent explicit; Cargo dedupes.)

- [ ] **Step 2: Write the integration test**

Create `crates/cli/tests/cli.rs`:

```rust
use std::path::PathBuf;
use std::process::Command;

use pipeline::catalog::Catalog;
use pipeline::ingest::{ExifData, FileFormat, IngestedFile};

/// Write a minimal config TOML pointing catalog + cache into `dir`, returning
/// (config_path, db_path).
fn write_config(dir: &std::path::Path) -> (PathBuf, PathBuf) {
    let db_path = dir.join("catalog.duckdb");
    let cache_dir = dir.join("cache");
    let cfg_path = dir.join("photopipe.toml");
    let toml = format!(
        "[catalog]\ndb_path = \"{}\"\ncache_dir = \"{}\"\n",
        db_path.display(),
        cache_dir.display()
    );
    std::fs::write(&cfg_path, toml).unwrap();
    (cfg_path, db_path)
}

fn seed_known_file(db_path: &std::path::Path, path: &str) {
    let catalog = Catalog::open(db_path).unwrap();
    let file = IngestedFile {
        path: PathBuf::from(path),
        content_hash: 42,
        size: 1000,
        mtime_ns: 5,
        format: FileFormat::Jpg,
        has_sidecar_jpg: false,
    };
    catalog.flush_batch(&[(file, None::<ExifData>)]).unwrap();
}

#[test]
fn info_known_file_emits_json_and_exits_zero() {
    let dir = tempfile::TempDir::new().unwrap();
    let (cfg_path, db_path) = write_config(dir.path());
    seed_known_file(&db_path, "/lib/known.jpg");

    let out = Command::new(env!("CARGO_BIN_EXE_photopipe"))
        .args(["--config", cfg_path.to_str().unwrap(), "info", "/lib/known.jpg"])
        .output()
        .expect("spawn photopipe");

    assert!(out.status.success(), "expected exit 0, got {:?}\nstderr: {}",
        out.status, String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8(out.stdout).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("stdout must be JSON");
    assert_eq!(parsed["file"]["path"], "/lib/known.jpg");
}

#[test]
fn info_unknown_file_exits_nonzero() {
    let dir = tempfile::TempDir::new().unwrap();
    let (cfg_path, db_path) = write_config(dir.path());
    seed_known_file(&db_path, "/lib/known.jpg");

    let out = Command::new(env!("CARGO_BIN_EXE_photopipe"))
        .args(["--config", cfg_path.to_str().unwrap(), "info", "/lib/missing.jpg"])
        .output()
        .expect("spawn photopipe");

    assert!(!out.status.success(), "expected non-zero exit for unknown file");
}

#[test]
fn doctor_exits_nonzero_when_configured_model_missing() {
    let dir = tempfile::TempDir::new().unwrap();
    let model_dir = dir.path().join("empty-models");
    std::fs::create_dir_all(&model_dir).unwrap();
    let db_path = dir.path().join("catalog.duckdb");
    let cache_dir = dir.path().join("cache");
    let cfg_path = dir.path().join("photopipe.toml");
    // model_dir is empty → every configured model file is missing → critical fail.
    let toml = format!(
        "[catalog]\ndb_path = \"{}\"\ncache_dir = \"{}\"\n\n[models]\nmodel_dir = \"{}\"\n",
        db_path.display(),
        cache_dir.display(),
        model_dir.display()
    );
    std::fs::write(&cfg_path, toml).unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_photopipe"))
        .args(["--config", cfg_path.to_str().unwrap(), "doctor"])
        .output()
        .expect("spawn photopipe");

    assert!(!out.status.success(), "doctor must fail when configured models are absent");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(combined.contains("missing"), "doctor output should mention a missing model:\n{combined}");
}
```

- [ ] **Step 3: Run the integration test (debug build) to verify it passes**

Run: `source ~/.cargo/env && cargo test -p photopipe --test cli`
Expected: PASS (3 passed). Cargo builds the binary first and sets `CARGO_BIN_EXE_photopipe`.

- [ ] **Step 4: Run the entire suite + lint to confirm nothing regressed**

Run: `source ~/.cargo/env && cargo test --all && cargo fmt --check && cargo clippy --all-targets --all-features -- -D warnings`
Expected: all tests pass; fmt clean; no clippy warnings.

- [ ] **Step 5: Commit**

```bash
git add crates/cli/Cargo.toml crates/cli/tests/cli.rs Cargo.lock
git commit -m "$(cat <<'EOF'
test(cli): info JSON/exit-code + doctor failure exit-code tests

Spawns the built binary against a temp config to verify `info` emits
parseable JSON and exits 0 for a known file / non-zero for an unknown one,
and that `doctor` exits non-zero when configured models are absent.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 6: Docs — `README.md` + `photopipe.example.toml`

Ship the quickstart and an example config whose values **exactly match `config.rs` defaults** (verified against the real source: `iqa = "clip-iqa"`, `keeper_strategy = "iqa"`, `write_batch_size = 64`, etc. — do NOT copy stale values like `musiq` from IMPLEMENTATION_PLAN §5).

**Files:**
- Create/Modify: `README.md` (repo root)
- Create: `photopipe.example.toml` (repo root)

**Interfaces:** none (documentation only).

- [ ] **Step 1: Create `photopipe.example.toml`**

Create `photopipe.example.toml` at the repo root. Every value below is the real default from `crates/pipeline/src/config.rs`. `db_path`/`cache_dir` are shown as concrete example paths (the real defaults are XDG-derived at runtime); the comment makes that explicit.

```toml
# photopipe configuration — every value here is the built-in default.
# Copy to $XDG_CONFIG_HOME/photopipe/photopipe.toml (usually
# ~/.config/photopipe/photopipe.toml) and edit. Any field you omit falls
# back to the default shown here.

[catalog]
# Defaults are derived from XDG dirs at runtime; the paths below are examples.
db_path = "~/.local/share/photopipe/catalog.duckdb"
cache_dir = "~/.cache/photopipe"
write_batch_size = 64
enable_vss = false

[ingest]
extensions = ["arw", "cr3", "cr2", "nef", "raf", "rw2", "dng", "jpg", "jpeg"]
follow_symlinks = false
threads = 0            # 0 = use all logical cores
sidecar_jpg = "prefer" # prefer | ignore | require
preview_max_long_edge = 2048
preview_quality = 85

[models]
device = "auto"        # auto | coreml | cuda | tensorrt | cpu
embedder = "dinov2-base"
iqa = "clip-iqa"
detector = "rt-detr-l"
model_dir = "./models"

[defect.blur]
enable = true
subject_min_area_ratio = 0.02
fallback_center_crop = 0.4
iqa_second_opinion = true
percentile_threshold = 0.10
min_samples_for_bucket = 30

[defect.exposure]
enable = true
clipped_highlights_threshold = 0.05
clipped_shadows_threshold = 0.10

[dedupe]
enable = true
time_window_seconds = 60
cosine_threshold_within_window = 0.92
cosine_threshold_global = 0.97
knn_k = 10
min_group_size = 2

[output]
review_tree = "<library>/_review"  # <library> is replaced with the scan root
link_type = "symlink"              # symlink | hardlink
keeper_strategy = "iqa"            # iqa | sharpness | iqa_then_sharpness
```

- [ ] **Step 2: Verify the example config parses into `Config`**

The CLI parses with `config::load`, which round-trips through `toml`. Confirm by pointing `doctor` at it:

Run:
```
source ~/.cargo/env && cargo build -p photopipe && ./target/debug/photopipe --config photopipe.example.toml doctor >/dev/null; echo "exit=$?"
```
Expected: doctor runs and reflects the file's settings; `exit` is `0` on the dev box (models present) or `1` (a configured model missing locally). Either way it must NOT print a TOML parse error — a parse error would abort before the health checks. If you see `config parse error`, fix the offending key to match `config.rs`.

- [ ] **Step 3: Create `README.md`**

Create `README.md` at the repo root:

````markdown
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
| `info <FILE>` | Print all catalog data for one file as JSON. |
| `stats` | Print catalog summary statistics. |
| `doctor` | Diagnose config, models, DB, and system health. |

All commands accept `--config <path>`, `--log-level <level>`, and `--log-format <pretty\|json>`.

## Guarantees

- **Non-destructive:** originals are only read; outputs are a separate DuckDB file and a tree of symlinks.
- **Idempotent:** re-running `scan` on unchanged inputs does no new work.
````

- [ ] **Step 4: Sanity-check the docs render and reference real commands**

Run: `source ~/.cargo/env && ./target/debug/photopipe --help`
Expected: the listed subcommands (`scan`, `calibrate`, `dedupe`, `review-tree`, `info`, `stats`, `doctor`) match the README command-reference table. Fix any drift in the README.

- [ ] **Step 5: Commit**

```bash
git add README.md photopipe.example.toml
git commit -m "$(cat <<'EOF'
docs: add README quickstart and photopipe.example.toml

README covers install, config, and the scan -> calibrate -> dedupe ->
review-tree workflow plus stats/info/doctor. The example config mirrors the
real config.rs defaults (iqa = clip-iqa, keeper_strategy = iqa, etc.).

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Self-Review

**Spec coverage (IMPLEMENTATION_PLAN §8 Phase 7):**
- doctor: schema-version match → Task 1 (`schema_version`) + Task 2 (`doctor_check_schema`). Models exist AND load → Task 2 (`doctor_check_models` uses real `ModelHub::from_config`). ORT EP detected → Task 2 prints `hub.provider`. Cache dir writable → Task 2 (`doctor_check_cache_writable`). Disk free > 5 GB → Task 2 (`doctor_check_disk_free`, `sysinfo`). Exit non-zero on critical fail → Task 2 (`anyhow::bail!`). macOS CoreML note kept → Task 2 Step 6. ✓
- stats: per-flag counts, dup group count + grouped file count, total files, per-camera + per-lens, catalog/cache disk usage → Task 3. ✓
- info: JSON dump of all rows for one file; exit non-zero if absent → Task 4. ✓
- docs: README quickstart + `photopipe.example.toml` with real defaults → Task 6. ✓
- Tests: `schema_version`/`stats`/`flag_counts`/`dump_file` unit tests (Tasks 1,3,4); CLI `info` JSON + exit codes and `doctor` exit codes (Task 5). ✓
- New deps surfaced (not silent): `serde_json`, `sysinfo` → Task 1. ✓

**Placeholder scan:** every code step contains complete code; no "TBD"/"add error handling"/"similar to Task N". The one conditional branch (Task 4 Step 6 embedding read-back) gives both concrete alternatives and a deterministic decision rule. ✓

**Type consistency:** `CatalogStats`, `FileDump`+sub-structs, `schema_version`, `flag_counts`, `per_camera_counts`, `per_lens_counts`, `dump_file`, `optional_row` are each defined once and referenced with identical names/signatures across tasks and tests. `CheckStatus`/`DoctorCheck`/`EXPECTED_SCHEMA_VERSION`/`MIN_FREE_DISK_GB` are CLI-local and used consistently in Task 2. `insert_file` test helper defined in Task 3 Step 1 and reused in Task 4 Step 1 (Task 3 commits first, so it exists). ✓

## Execution Handoff

**Plan complete and saved to `docs/superpowers/plans/2026-06-26-phase7-polish.md`. Two execution options:**

**1. Subagent-Driven (recommended)** — I dispatch a fresh subagent per task, review between tasks, fast iteration.

**2. Inline Execution** — Execute tasks in this session using executing-plans, batch execution with checkpoints.

**Which approach?**
