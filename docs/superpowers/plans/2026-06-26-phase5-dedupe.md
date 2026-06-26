# Phase 5 — Duplicate Detection Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build `photopipe dedupe` — cluster near-duplicate photos using time-window + embedding cosine-similarity edges, group them via connected components, select a suggested keeper per group, and persist the result idempotently into `duplicate_groups` / `duplicate_members`.

**Architecture:** A `dedupe` module loads all embeddings (DuckDB `FLOAT[]`) into memory as `Vec<(file_id, Vec<f32>)>`, L2-normalizes them, builds an undirected graph whose edges come from two sources — pairs captured within `time_window_seconds` with cosine ≥ `cosine_threshold_within_window`, and global top-`knn_k` neighbors with cosine ≥ `cosine_threshold_global`. Neighbor search sits behind a `KnnIndex` trait with a `BruteForceKnn` impl (rayon-parallel normalized dot products). Connected components (via `petgraph`) of size ≥ `min_group_size` become groups; each member's `quality_score` is computed from its IQA score and defect flags, and the highest scorer is flagged `is_suggested_keeper`. The command clears both tables before rebuilding, so re-running yields identical, deterministic output (file_ids are sorted before graph construction).

**Tech Stack:** Rust 2021, DuckDB (`duckdb` crate), `petgraph` (connected components), `rayon` (parallel cosine), `anyhow`/`thiserror`, `tracing`.

## Global Constraints

- Edition 2021, stable Rust. `anyhow::Result` at CLI boundaries; `thiserror`-derived types (`CatalogError`) inside `pipeline`.
- **DuckDB ONLY** (no SQLite). Bulk writes go through ONE transaction per batch; use the proven **prepared `INSERT … ON CONFLICT` inside `Connection::transaction()`** pattern (mirror `flush_ml_batch` / `flush_defect_batch`). Do not fight the Appender API for tables with sequence/identity ids or UNIQUE constraints.
- DuckDB has **no `ON DELETE CASCADE`** — cascade deletes in application code (delete `duplicate_members` before `duplicate_groups`).
- **Work in f32 throughout.** Embeddings persist as DuckDB `FLOAT[]`; the `Embedder` trait returns `Vec<f32>`. Do NOT add `half`/`f16`.
- No AGPL deps. No Python at runtime. Non-destructive — symlinks/reads only, never mutate/move/delete an original photo.
- **Idempotency is a correctness requirement:** `photopipe dedupe` is a rebuild-style command — it clears `duplicate_groups` + `duplicate_members` then rebuilds, producing identical output on a second run. petgraph node iteration order must be deterministic, so **sort `file_id`s before building the graph**.
- `tracing` for logs (`info!`/`warn!`/`debug!`); no `println!` except intentional CLI user output (the `cmd_dedupe` report print).
- Run before declaring any task done: `cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D warnings`, `cargo test --all` (in WSL: `source ~/.cargo/env` first).
- Surface (don't silently add) any new dependency. `petgraph` is the only new dependency; it is already named in IMPLEMENTATION_PLAN §3.2 (Connected components for dedupe groups) and is MIT/Apache-2.0 — therefore **pre-approved**, not a silent addition. We pin `petgraph = "0.6"`. The spec's `DuckDbVssKnn` (vss/HNSW) backend is **deliberately omitted** this phase — brute force only; the omission is logged at runtime (no silent cap).
- Commit message trailer (last line of every commit body): `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`

---

## File Structure

| File | Create/Modify | Responsibility |
|------|---------------|----------------|
| `Cargo.toml` | Modify (`[workspace.dependencies]`) | Add `petgraph = "0.6"` to the workspace dependency table. |
| `crates/pipeline/Cargo.toml` | Modify (`[dependencies]`) | Pull `petgraph` into the library crate via `workspace = true`. |
| `crates/pipeline/src/catalog/mod.rs` | Modify | New read/write methods: `load_all_embeddings`, `captured_at_map`, `iqa_scores_map`, `quality_inputs_map`, `clear_duplicate_groups`, `insert_duplicate_group`, `insert_duplicate_members`, `duplicate_group_count`, `duplicate_member_count`. Plus a `DuplicateMember` struct and a `QualityInputs` struct. |
| `crates/pipeline/src/dedupe/knn.rs` | Create | `KnnIndex` trait + `BruteForceKnn` impl; `l2_normalize`, `cosine` helpers; unit tests. |
| `crates/pipeline/src/dedupe/cluster.rs` | Create | Edge building + connected components + `quality_score` + keeper selection; unit tests. |
| `crates/pipeline/src/dedupe/mod.rs` | Modify (replace stub) | `DedupeReport`, `run_dedupe(catalog, cfg)` orchestrator; declares `mod knn; mod cluster;` and re-exports. |
| `crates/pipeline/src/lib.rs` | Modify | Add `pub use dedupe::{run_dedupe, DedupeReport};`. |
| `crates/cli/src/main.rs` | Modify (`cmd_dedupe`, lines 183-187) | Open catalog, call `run_dedupe(&catalog, &cfg.dedupe)`, print the report. |
| `crates/pipeline/tests/dedupe.rs` | Create | Integration tests: `FLOAT[]` round-trip de-risk, end-to-end synthetic dedupe, idempotency; `#[ignore]` real-photo acceptance tests. |

**Type vocabulary used across tasks (defined in Task 2 / Task 5):**

```rust
// catalog/mod.rs (Task 2)
pub struct DuplicateMember {
    pub file_id: i64,
    pub is_suggested_keeper: bool,
    pub quality_score: f32,
}
pub struct QualityInputs {
    pub iqa_score: Option<f32>,
    pub has_blur: bool,
    pub has_back_focus: bool,
    pub clipped_highlights: f32,
    pub clipped_shadows: f32,
}

// dedupe/knn.rs (Task 3)
pub trait KnnIndex {
    fn neighbors(&self, query_idx: usize, k: usize) -> Vec<(usize, f32)>;
}
pub struct BruteForceKnn { /* holds normalized vectors */ }

// dedupe/mod.rs (Task 6)
pub struct DedupeReport { pub groups: u64, pub members: u64, pub keepers: u64 }
```

---

## Task 1: Add the `petgraph` dependency

**Files:**
- Modify: `Cargo.toml` (the `[workspace.dependencies]` table, after line 22 `ndarray = "0.17"`)
- Modify: `crates/pipeline/Cargo.toml` (the `[dependencies]` table, after line 28 `ndarray = { workspace = true }`)

**Interfaces:**
- Consumes: nothing.
- Produces: `petgraph` available to `crates/pipeline` (used by Task 5).

- [ ] **Step 1: Add `petgraph` to the workspace dependency table**

In `Cargo.toml`, add a line to `[workspace.dependencies]` immediately after `ndarray = "0.17"`:

```toml
ndarray     = "0.17"
petgraph    = "0.6"
```

- [ ] **Step 2: Add `petgraph` to the pipeline crate dependencies**

In `crates/pipeline/Cargo.toml`, add a line to `[dependencies]` immediately after `ndarray = { workspace = true }`:

```toml
ndarray      = { workspace = true }
petgraph     = { workspace = true }
```

- [ ] **Step 3: Verify it resolves and compiles**

Run: `source ~/.cargo/env && cargo build -p pipeline`
Expected: build succeeds; `petgraph v0.6.x` appears in `cargo tree -p pipeline -i petgraph` output.

Run: `source ~/.cargo/env && cargo tree -p pipeline -i petgraph`
Expected: shows `petgraph v0.6.x` with `pipeline v0.1.0` depending on it.

- [ ] **Step 4: Commit**

```bash
git add Cargo.toml crates/pipeline/Cargo.toml Cargo.lock
git commit -m "chore(deps): add petgraph 0.6 for dedupe connected components

Pre-approved in IMPLEMENTATION_PLAN §3.2 (MIT/Apache-2.0).

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 2: Catalog — embedding round-trip (de-risk `FLOAT[]` read-back)

This task locks down the single riskiest unknown for the whole phase: **how duckdb-rs returns a `FLOAT[]` column**. We write the test FIRST, against the existing JSON `CAST(? AS FLOAT[])` write path, and confirm the read approach before any clustering code depends on it.

**Files:**
- Modify: `crates/pipeline/src/catalog/mod.rs` (add `load_all_embeddings` method to `impl Catalog`, after `iqa_count` ending line 760)
- Test: `crates/pipeline/src/catalog/mod.rs` (add to the existing `#[cfg(test)] mod tests` block, after `files_needing_defect_analysis_filters_correctly` ending line 889)

**Interfaces:**
- Consumes: existing `Catalog::open`, `Catalog::flush_ml_batch(&[MlRow])`, `MlRow { file_id, embedding: Option<(String, Vec<f32>)>, iqa_score }`.
- Produces:
  ```rust
  pub fn load_all_embeddings(&self) -> Result<Vec<(i64, Vec<f32>)>, CatalogError>;
  ```
  Returns one entry per row in `embeddings`, ordered by `file_id ASC` (deterministic).

- [ ] **Step 1: Write the failing round-trip test**

Add to the `mod tests` block in `crates/pipeline/src/catalog/mod.rs`:

```rust
#[test]
fn load_all_embeddings_round_trip() {
    use crate::ingest::{ExifData, FileFormat, IngestedFile};

    let (catalog, _dir) = make_catalog();

    // Insert two files.
    let mut ids = Vec::new();
    for i in 0..2i64 {
        let file = IngestedFile {
            path: PathBuf::from(format!("/emb/file{i}.jpg")),
            content_hash: 100 + i as u128,
            size: 10 + i as u64,
            mtime_ns: i,
            format: FileFormat::Jpg,
            has_sidecar_jpg: false,
        };
        let batch_ids = catalog.flush_batch(&[(file, None::<ExifData>)]).unwrap();
        ids.push(batch_ids[0]);
    }

    // Write embeddings via the existing JSON CAST(? AS FLOAT[]) path.
    let v0 = vec![1.0f32, 2.0, 3.0, 4.0];
    let v1 = vec![-0.5f32, 0.25, 0.125, 8.0];
    catalog
        .flush_ml_batch(&[
            MlRow {
                file_id: ids[0],
                embedding: Some(("test-model".to_string(), v0.clone())),
                iqa_score: None,
            },
            MlRow {
                file_id: ids[1],
                embedding: Some(("test-model".to_string(), v1.clone())),
                iqa_score: None,
            },
        ])
        .unwrap();

    // Read them back.
    let loaded = catalog.load_all_embeddings().unwrap();
    assert_eq!(loaded.len(), 2, "expected two embedding rows");

    // Ordered by file_id ASC.
    assert_eq!(loaded[0].0, ids[0]);
    assert_eq!(loaded[1].0, ids[1]);

    // Vectors round-trip exactly (f32 written, f32 read).
    assert_eq!(loaded[0].1, v0, "first vector mismatch");
    assert_eq!(loaded[1].1, v1, "second vector mismatch");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `source ~/.cargo/env && cargo test -p pipeline --lib catalog::tests::load_all_embeddings_round_trip`
Expected: FAIL — `no method named load_all_embeddings found`.

- [ ] **Step 3: Implement `load_all_embeddings` — primary approach (`Vec<f32>` extraction)**

Add this method to `impl Catalog` in `crates/pipeline/src/catalog/mod.rs`, immediately after the `iqa_count` method (after line 760):

```rust
/// Load every embedding vector from the catalog, ordered by `file_id`.
///
/// Vectors are stored as DuckDB `FLOAT[]`.  duckdb-rs maps a `FLOAT[]`
/// column to a Rust `Vec<f32>` via `row.get::<_, Vec<f32>>(idx)`.  Ordering
/// by `file_id ASC` makes graph construction deterministic downstream.
pub fn load_all_embeddings(&self) -> Result<Vec<(i64, Vec<f32>)>, CatalogError> {
    let conn = self
        .conn
        .lock()
        .map_err(|_| CatalogError::Db("mutex poisoned".into()))?;
    let mut stmt = conn
        .prepare("SELECT file_id, vector FROM embeddings ORDER BY file_id ASC")
        .map_err(|e| CatalogError::Db(e.to_string()))?;
    let rows = stmt
        .query_map([], |row| {
            let id: i64 = row.get(0)?;
            let vector: Vec<f32> = row.get(1)?;
            Ok((id, vector))
        })
        .map_err(|e| CatalogError::Db(e.to_string()))?;

    let mut result = Vec::new();
    for row in rows {
        result.push(row.map_err(|e| CatalogError::Db(e.to_string()))?);
    }
    Ok(result)
}
```

- [ ] **Step 4: Run the test against the primary approach**

Run: `source ~/.cargo/env && cargo test -p pipeline --lib catalog::tests::load_all_embeddings_round_trip`
Expected: PASS.

**If it FAILS with a type-conversion / `FromSql` error** (duckdb-rs cannot map `FLOAT[]` → `Vec<f32>` directly in this version), use the fallback below instead. **Do not silently leave it failing.** Replace the `query_map` body so the array comes back as `f64` or as a string:

Fallback A — list comes back as `Vec<f64>`:

```rust
.query_map([], |row| {
    let id: i64 = row.get(0)?;
    let vector: Vec<f64> = row.get(1)?;
    Ok((id, vector.into_iter().map(|v| v as f32).collect::<Vec<f32>>()))
})
```

Fallback B — coerce to a JSON string in SQL and parse in Rust (most portable). Change the SQL to `SELECT file_id, CAST(vector AS VARCHAR) AS vstr FROM embeddings ORDER BY file_id ASC` and parse:

```rust
.query_map([], |row| {
    let id: i64 = row.get(0)?;
    let vstr: String = row.get(1)?;
    // DuckDB renders a FLOAT[] as "[1.0, 2.0, 3.0]".
    let vector: Vec<f32> = vstr
        .trim_matches(|c| c == '[' || c == ']')
        .split(',')
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.trim().parse::<f32>().unwrap_or(0.0))
        .collect();
    Ok((id, vector))
})
```

Re-run Step 4 after applying a fallback; iterate to PASS. Record in the commit body which approach worked.

- [ ] **Step 5: Commit**

```bash
git add crates/pipeline/src/catalog/mod.rs
git commit -m "feat(dedupe): catalog load_all_embeddings reads FLOAT[] back as Vec<f32>

De-risks the FLOAT[] read-back: round-trips f32 vectors through the
existing JSON CAST(? AS FLOAT[]) write path. Primary extraction uses
row.get::<_, Vec<f32>>; fallbacks documented in code if duckdb-rs's
FromSql mapping differs.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 3: Catalog — quality-input read methods

These methods feed keeper selection. They load per-file `captured_at`, IQA scores, and the inputs to `quality_score` (defect flags + exposure clip fractions).

**Files:**
- Modify: `crates/pipeline/src/catalog/mod.rs` (add `QualityInputs` struct near the top with the other structs after `MlRow` ending line 17; add three methods to `impl Catalog` after `load_all_embeddings`)
- Test: `crates/pipeline/src/catalog/mod.rs` (`mod tests` block)

**Interfaces:**
- Consumes: existing `Catalog::flush_batch`, `flush_ml_batch`, `flush_defect_batch`, `upsert_exposure`, `upsert_exif`.
- Produces:
  ```rust
  pub struct QualityInputs {
      pub iqa_score: Option<f32>,
      pub has_blur: bool,
      pub has_back_focus: bool,
      pub clipped_highlights: f32,
      pub clipped_shadows: f32,
  }
  impl Catalog {
      pub fn captured_at_map(&self) -> Result<std::collections::HashMap<i64, Option<i64>>, CatalogError>;
      pub fn iqa_scores_map(&self) -> Result<std::collections::HashMap<i64, f32>, CatalogError>;
      pub fn quality_inputs_map(&self) -> Result<std::collections::HashMap<i64, QualityInputs>, CatalogError>;
  }
  ```

- [ ] **Step 1: Write the failing test**

Add to the `mod tests` block in `crates/pipeline/src/catalog/mod.rs`:

```rust
#[test]
fn quality_inputs_and_maps_round_trip() {
    use crate::defect::{DefectFlag, DefectRow, ExposureResult, SharpnessResult};
    use crate::ingest::{ExifData, FileFormat, IngestedFile};

    let (catalog, _dir) = make_catalog();

    // Two files.
    let mut ids = Vec::new();
    for i in 0..2i64 {
        let file = IngestedFile {
            path: PathBuf::from(format!("/q/file{i}.jpg")),
            content_hash: 200 + i as u128,
            size: 10 + i as u64,
            mtime_ns: i,
            format: FileFormat::Jpg,
            has_sidecar_jpg: false,
        };
        // File 0 gets EXIF with captured_at; file 1 gets none.
        let exif = if i == 0 {
            Some(ExifData {
                captured_at: Some(1_700_000_000),
                camera_make: None,
                camera_model: None,
                lens_model: None,
                focal_length_mm: None,
                aperture: None,
                iso: None,
                shutter_seconds: None,
                width: None,
                height: None,
                orientation: None,
            })
        } else {
            None
        };
        let batch_ids = catalog.flush_batch(&[(file, exif)]).unwrap();
        ids.push(batch_ids[0]);
    }

    // File 0: IQA 0.8, a blur flag, exposure clips.
    catalog
        .flush_ml_batch(&[MlRow {
            file_id: ids[0],
            embedding: None,
            iqa_score: Some(("clip-iqa".to_string(), 0.8)),
        }])
        .unwrap();
    catalog
        .flush_defect_batch(&[DefectRow {
            file_id: ids[0],
            sharpness: SharpnessResult {
                s_global: 1.0,
                s_subject: None,
                s_background: None,
                subject_ratio: None,
                detector_used: "x".into(),
            },
            exposure: ExposureResult {
                clipped_highlights: 0.1,
                clipped_shadows: 0.4,
                mean_luma: 0.5,
                histogram_skew: 0.0,
            },
            flags: vec![DefectFlag {
                flag_type: "blur".into(),
                confidence: 0.9,
                reason: "test".into(),
            }],
        }])
        .unwrap();

    // captured_at_map: file 0 has Some, file 1 absent or None.
    let cap = catalog.captured_at_map().unwrap();
    assert_eq!(cap.get(&ids[0]).copied().flatten(), Some(1_700_000_000));
    assert!(cap.get(&ids[1]).copied().flatten().is_none());

    // iqa_scores_map: only file 0 present.
    let iqa = catalog.iqa_scores_map().unwrap();
    assert!((iqa.get(&ids[0]).copied().unwrap() - 0.8).abs() < 1e-6);
    assert!(iqa.get(&ids[1]).is_none());

    // quality_inputs_map: file 0 has blur + exposure, no back_focus.
    let q = catalog.quality_inputs_map().unwrap();
    let q0 = q.get(&ids[0]).unwrap();
    assert!((q0.iqa_score.unwrap() - 0.8).abs() < 1e-6);
    assert!(q0.has_blur);
    assert!(!q0.has_back_focus);
    assert!((q0.clipped_highlights - 0.1).abs() < 1e-6);
    assert!((q0.clipped_shadows - 0.4).abs() < 1e-6);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `source ~/.cargo/env && cargo test -p pipeline --lib catalog::tests::quality_inputs_and_maps_round_trip`
Expected: FAIL — `cannot find type QualityInputs` / `no method named captured_at_map`.

- [ ] **Step 3: Add the `QualityInputs` struct**

In `crates/pipeline/src/catalog/mod.rs`, immediately after the `MlRow` struct (after line 17, before `pub struct Catalog`):

```rust
/// Per-file inputs to the dedupe `quality_score` formula.
pub struct QualityInputs {
    /// IQA score (`iqa.score`), or `None` when no IQA row exists.
    pub iqa_score: Option<f32>,
    /// True when a `blur` defect flag exists for this file.
    pub has_blur: bool,
    /// True when a `back_focus` defect flag exists for this file.
    pub has_back_focus: bool,
    /// Fraction of clipped highlights (0.0 when no exposure row).
    pub clipped_highlights: f32,
    /// Fraction of clipped shadows (0.0 when no exposure row).
    pub clipped_shadows: f32,
}
```

- [ ] **Step 4: Implement the three map methods**

In `crates/pipeline/src/catalog/mod.rs`, add to `impl Catalog` immediately after `load_all_embeddings`:

```rust
/// Map every file's `captured_at` (unix epoch seconds), `None` when absent.
///
/// Only files that have a row in `files` appear; the value is `None` when
/// there is no `exif` row or `captured_at` is NULL.
pub fn captured_at_map(
    &self,
) -> Result<std::collections::HashMap<i64, Option<i64>>, CatalogError> {
    let conn = self
        .conn
        .lock()
        .map_err(|_| CatalogError::Db("mutex poisoned".into()))?;
    let mut stmt = conn
        .prepare(
            "SELECT f.id, e.captured_at
             FROM files f
             LEFT JOIN exif e ON e.file_id = f.id",
        )
        .map_err(|e| CatalogError::Db(e.to_string()))?;
    let rows = stmt
        .query_map([], |row| {
            let id: i64 = row.get(0)?;
            let captured_at: Option<i64> = row.get(1)?;
            Ok((id, captured_at))
        })
        .map_err(|e| CatalogError::Db(e.to_string()))?;

    let mut map = std::collections::HashMap::new();
    for row in rows {
        let (id, captured_at) = row.map_err(|e| CatalogError::Db(e.to_string()))?;
        map.insert(id, captured_at);
    }
    Ok(map)
}

/// Map file_id → IQA score for every file that has an `iqa` row.
pub fn iqa_scores_map(
    &self,
) -> Result<std::collections::HashMap<i64, f32>, CatalogError> {
    let conn = self
        .conn
        .lock()
        .map_err(|_| CatalogError::Db("mutex poisoned".into()))?;
    let mut stmt = conn
        .prepare("SELECT file_id, score FROM iqa")
        .map_err(|e| CatalogError::Db(e.to_string()))?;
    let rows = stmt
        .query_map([], |row| {
            let id: i64 = row.get(0)?;
            let score: f32 = row.get(1)?;
            Ok((id, score))
        })
        .map_err(|e| CatalogError::Db(e.to_string()))?;

    let mut map = std::collections::HashMap::new();
    for row in rows {
        let (id, score) = row.map_err(|e| CatalogError::Db(e.to_string()))?;
        map.insert(id, score);
    }
    Ok(map)
}

/// Map file_id → `QualityInputs` for every file in `files`.
///
/// Joins `iqa`, `exposure`, and aggregates `defect_flags` so a single pass
/// yields everything the `quality_score` formula needs.  Missing IQA →
/// `iqa_score = None`; missing exposure → clip fractions 0.0.
pub fn quality_inputs_map(
    &self,
) -> Result<std::collections::HashMap<i64, QualityInputs>, CatalogError> {
    let conn = self
        .conn
        .lock()
        .map_err(|_| CatalogError::Db("mutex poisoned".into()))?;
    let mut stmt = conn
        .prepare(
            "SELECT
                 f.id,
                 i.score,
                 COALESCE(x.clipped_highlights, 0.0),
                 COALESCE(x.clipped_shadows, 0.0),
                 COALESCE(MAX(CASE WHEN d.flag_type = 'blur' THEN 1 ELSE 0 END), 0),
                 COALESCE(MAX(CASE WHEN d.flag_type = 'back_focus' THEN 1 ELSE 0 END), 0)
             FROM files f
             LEFT JOIN iqa i        ON i.file_id = f.id
             LEFT JOIN exposure x   ON x.file_id = f.id
             LEFT JOIN defect_flags d ON d.file_id = f.id
             GROUP BY f.id, i.score, x.clipped_highlights, x.clipped_shadows",
        )
        .map_err(|e| CatalogError::Db(e.to_string()))?;
    let rows = stmt
        .query_map([], |row| {
            let id: i64 = row.get(0)?;
            let iqa_score: Option<f32> = row.get(1)?;
            let clipped_highlights: f32 = row.get(2)?;
            let clipped_shadows: f32 = row.get(3)?;
            let has_blur: i64 = row.get(4)?;
            let has_back_focus: i64 = row.get(5)?;
            Ok((
                id,
                QualityInputs {
                    iqa_score,
                    has_blur: has_blur != 0,
                    has_back_focus: has_back_focus != 0,
                    clipped_highlights,
                    clipped_shadows,
                },
            ))
        })
        .map_err(|e| CatalogError::Db(e.to_string()))?;

    let mut map = std::collections::HashMap::new();
    for row in rows {
        let (id, qi) = row.map_err(|e| CatalogError::Db(e.to_string()))?;
        map.insert(id, qi);
    }
    Ok(map)
}
```

- [ ] **Step 5: Run test to verify it passes**

Run: `source ~/.cargo/env && cargo test -p pipeline --lib catalog::tests::quality_inputs_and_maps_round_trip`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/pipeline/src/catalog/mod.rs
git commit -m "feat(dedupe): catalog quality-input read maps for keeper scoring

captured_at_map, iqa_scores_map, and quality_inputs_map (joins iqa +
exposure + aggregated defect_flags) feed time-window edges and the
quality_score formula.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 4: Catalog — duplicate-group write methods

These persist results and enforce the clear-then-rebuild idempotency contract.

**Files:**
- Modify: `crates/pipeline/src/catalog/mod.rs` (add `DuplicateMember` struct after `QualityInputs`; add four methods to `impl Catalog`)
- Test: `crates/pipeline/src/catalog/mod.rs` (`mod tests` block)

**Interfaces:**
- Consumes: existing `Catalog::flush_batch`.
- Produces:
  ```rust
  pub struct DuplicateMember {
      pub file_id: i64,
      pub is_suggested_keeper: bool,
      pub quality_score: f32,
  }
  impl Catalog {
      pub fn clear_duplicate_groups(&self) -> Result<(), CatalogError>;
      pub fn insert_duplicate_group(&self, method: &str, created_at: i64) -> Result<i64, CatalogError>;
      pub fn insert_duplicate_members(&self, group_id: i64, members: &[DuplicateMember]) -> Result<(), CatalogError>;
      pub fn duplicate_group_count(&self) -> Result<i64, CatalogError>;
      pub fn duplicate_member_count(&self) -> Result<i64, CatalogError>;
  }
  ```

- [ ] **Step 1: Write the failing test**

Add to the `mod tests` block in `crates/pipeline/src/catalog/mod.rs`:

```rust
#[test]
fn duplicate_group_write_and_clear() {
    use crate::ingest::{ExifData, FileFormat, IngestedFile};

    let (catalog, _dir) = make_catalog();

    let mut ids = Vec::new();
    for i in 0..3i64 {
        let file = IngestedFile {
            path: PathBuf::from(format!("/dg/file{i}.jpg")),
            content_hash: 300 + i as u128,
            size: 10 + i as u64,
            mtime_ns: i,
            format: FileFormat::Jpg,
            has_sidecar_jpg: false,
        };
        let batch_ids = catalog.flush_batch(&[(file, None::<ExifData>)]).unwrap();
        ids.push(batch_ids[0]);
    }

    // Empty initially.
    assert_eq!(catalog.duplicate_group_count().unwrap(), 0);
    assert_eq!(catalog.duplicate_member_count().unwrap(), 0);

    // Insert one group with two members.
    let gid = catalog.insert_duplicate_group("time+embed", 12345).unwrap();
    catalog
        .insert_duplicate_members(
            gid,
            &[
                DuplicateMember { file_id: ids[0], is_suggested_keeper: true, quality_score: 0.9 },
                DuplicateMember { file_id: ids[1], is_suggested_keeper: false, quality_score: 0.4 },
            ],
        )
        .unwrap();

    assert_eq!(catalog.duplicate_group_count().unwrap(), 1);
    assert_eq!(catalog.duplicate_member_count().unwrap(), 2);

    // Distinct ids for a second group.
    let gid2 = catalog.insert_duplicate_group("time+embed", 12346).unwrap();
    assert_ne!(gid, gid2, "group ids must be distinct");

    // Clear wipes everything (members first, then groups — no CASCADE).
    catalog.clear_duplicate_groups().unwrap();
    assert_eq!(catalog.duplicate_group_count().unwrap(), 0);
    assert_eq!(catalog.duplicate_member_count().unwrap(), 0);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `source ~/.cargo/env && cargo test -p pipeline --lib catalog::tests::duplicate_group_write_and_clear`
Expected: FAIL — `cannot find type DuplicateMember` / `no method named insert_duplicate_group`.

- [ ] **Step 3: Add the `DuplicateMember` struct**

In `crates/pipeline/src/catalog/mod.rs`, immediately after the `QualityInputs` struct (added in Task 3):

```rust
/// One row destined for the `duplicate_members` table.
pub struct DuplicateMember {
    pub file_id: i64,
    pub is_suggested_keeper: bool,
    pub quality_score: f32,
}
```

- [ ] **Step 4: Implement the four methods**

Add to `impl Catalog` in `crates/pipeline/src/catalog/mod.rs`, after `quality_inputs_map`:

```rust
/// Delete all duplicate groups and members.  Members are deleted first
/// because DuckDB does not support `ON DELETE CASCADE`; both deletes run
/// in one transaction so the tables are never left half-cleared.
pub fn clear_duplicate_groups(&self) -> Result<(), CatalogError> {
    let mut conn = self
        .conn
        .lock()
        .map_err(|_| CatalogError::Db("mutex poisoned".into()))?;
    let tx = conn
        .transaction()
        .map_err(|e| CatalogError::Db(e.to_string()))?;
    tx.execute("DELETE FROM duplicate_members", [])
        .map_err(|e| CatalogError::Db(e.to_string()))?;
    tx.execute("DELETE FROM duplicate_groups", [])
        .map_err(|e| CatalogError::Db(e.to_string()))?;
    tx.commit().map_err(|e| CatalogError::Db(e.to_string()))?;
    Ok(())
}

/// Insert a `duplicate_groups` row and return its generated `id`.
pub fn insert_duplicate_group(
    &self,
    method: &str,
    created_at: i64,
) -> Result<i64, CatalogError> {
    let conn = self
        .conn
        .lock()
        .map_err(|_| CatalogError::Db("mutex poisoned".into()))?;
    let id: i64 = conn
        .query_row(
            "INSERT INTO duplicate_groups (method, created_at)
             VALUES (?, ?)
             RETURNING id",
            duckdb::params![method, created_at],
            |r| r.get(0),
        )
        .map_err(|e| CatalogError::Db(e.to_string()))?;
    Ok(id)
}

/// Insert all members of one group in a single transaction.
pub fn insert_duplicate_members(
    &self,
    group_id: i64,
    members: &[DuplicateMember],
) -> Result<(), CatalogError> {
    if members.is_empty() {
        return Ok(());
    }
    let mut conn = self
        .conn
        .lock()
        .map_err(|_| CatalogError::Db("mutex poisoned".into()))?;
    let tx = conn
        .transaction()
        .map_err(|e| CatalogError::Db(e.to_string()))?;
    {
        let mut stmt = tx
            .prepare(
                "INSERT INTO duplicate_members
                     (group_id, file_id, is_suggested_keeper, quality_score)
                 VALUES (?, ?, ?, ?)
                 ON CONFLICT (group_id, file_id) DO UPDATE SET
                     is_suggested_keeper = excluded.is_suggested_keeper,
                     quality_score       = excluded.quality_score",
            )
            .map_err(|e| CatalogError::Db(e.to_string()))?;
        for m in members {
            stmt.execute(duckdb::params![
                group_id,
                m.file_id,
                m.is_suggested_keeper,
                m.quality_score,
            ])
            .map_err(|e| CatalogError::Db(e.to_string()))?;
        }
    }
    tx.commit().map_err(|e| CatalogError::Db(e.to_string()))?;
    Ok(())
}

/// Count rows in `duplicate_groups`.
pub fn duplicate_group_count(&self) -> Result<i64, CatalogError> {
    let conn = self
        .conn
        .lock()
        .map_err(|_| CatalogError::Db("mutex poisoned".into()))?;
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM duplicate_groups", [], |r| r.get(0))
        .map_err(|e| CatalogError::Db(e.to_string()))?;
    Ok(count)
}

/// Count rows in `duplicate_members`.
pub fn duplicate_member_count(&self) -> Result<i64, CatalogError> {
    let conn = self
        .conn
        .lock()
        .map_err(|_| CatalogError::Db("mutex poisoned".into()))?;
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM duplicate_members", [], |r| r.get(0))
        .map_err(|e| CatalogError::Db(e.to_string()))?;
    Ok(count)
}
```

- [ ] **Step 5: Run test to verify it passes**

Run: `source ~/.cargo/env && cargo test -p pipeline --lib catalog::tests::duplicate_group_write_and_clear`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/pipeline/src/catalog/mod.rs
git commit -m "feat(dedupe): catalog duplicate-group write + clear methods

insert_duplicate_group (RETURNING id), batch insert_duplicate_members,
clear_duplicate_groups (members-then-groups in one tx, no CASCADE), and
count helpers for the report and tests.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 5: KNN — cosine, L2-normalize, brute-force neighbors

**Files:**
- Create: `crates/pipeline/src/dedupe/knn.rs`
- Test: `crates/pipeline/src/dedupe/knn.rs` (inline `#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: `rayon` (workspace dep).
- Produces:
  ```rust
  pub fn l2_normalize(v: &mut [f32]);
  pub fn cosine_normalized(a: &[f32], b: &[f32]) -> f32; // dot product of pre-normalized vectors
  pub trait KnnIndex {
      /// Top-`k` neighbors of `query_idx` (excluding itself), as (index, cosine), sorted desc.
      fn neighbors(&self, query_idx: usize, k: usize) -> Vec<(usize, f32)>;
  }
  pub struct BruteForceKnn { /* stores normalized vectors */ }
  impl BruteForceKnn {
      pub fn new(normalized: Vec<Vec<f32>>) -> Self;
      pub fn len(&self) -> usize;
      pub fn is_empty(&self) -> bool;
      pub fn cosine(&self, i: usize, j: usize) -> f32;
  }
  ```

- [ ] **Step 1: Write the failing tests**

Create `crates/pipeline/src/dedupe/knn.rs` with ONLY the test module first (implementation follows in Step 3):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn l2_normalize_yields_unit_norm() {
        let mut v = vec![3.0f32, 4.0];
        l2_normalize(&mut v);
        let norm = (v[0] * v[0] + v[1] * v[1]).sqrt();
        assert!((norm - 1.0).abs() < 1e-6, "norm was {norm}");
        assert!((v[0] - 0.6).abs() < 1e-6);
        assert!((v[1] - 0.8).abs() < 1e-6);
    }

    #[test]
    fn l2_normalize_zero_vector_is_safe() {
        let mut v = vec![0.0f32, 0.0, 0.0];
        l2_normalize(&mut v);
        // Stays all-zero, no NaN.
        assert!(v.iter().all(|x| *x == 0.0), "zero vector must not NaN");
    }

    #[test]
    fn cosine_of_identical_normalized_is_one() {
        let mut a = vec![1.0f32, 1.0, 0.0];
        l2_normalize(&mut a);
        let sim = cosine_normalized(&a, &a);
        assert!((sim - 1.0).abs() < 1e-6, "sim was {sim}");
    }

    #[test]
    fn cosine_of_orthogonal_is_zero() {
        let a = vec![1.0f32, 0.0];
        let b = vec![0.0f32, 1.0];
        let sim = cosine_normalized(&a, &b);
        assert!(sim.abs() < 1e-6, "sim was {sim}");
    }

    #[test]
    fn brute_force_neighbors_ranks_by_cosine() {
        // Index 0 close to 1, far from 2.
        let mut v0 = vec![1.0f32, 0.0];
        let mut v1 = vec![0.99f32, 0.14];
        let mut v2 = vec![0.0f32, 1.0];
        l2_normalize(&mut v0);
        l2_normalize(&mut v1);
        l2_normalize(&mut v2);
        let knn = BruteForceKnn::new(vec![v0, v1, v2]);
        assert_eq!(knn.len(), 3);

        let nbrs = knn.neighbors(0, 2);
        assert_eq!(nbrs.len(), 2, "should exclude self, return up to k");
        // Nearest neighbor of 0 is 1.
        assert_eq!(nbrs[0].0, 1, "closest neighbor of 0 should be index 1");
        assert!(nbrs[0].1 > nbrs[1].1, "neighbors must be sorted desc by cosine");
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `source ~/.cargo/env && cargo test -p pipeline --lib dedupe::knn`
Expected: FAIL — `cannot find function l2_normalize` (and the module isn't declared yet; this will be a compile error until Step 4 of Task 6 declares `mod knn;`). To compile-check this task in isolation, temporarily add `pub mod knn;` to `dedupe/mod.rs` — but Task 6 owns that line, so accept the failing/compile-error state here and confirm the named functions are the cause.

- [ ] **Step 3: Write the implementation**

Prepend the implementation to `crates/pipeline/src/dedupe/knn.rs` (above the `#[cfg(test)] mod tests` block):

```rust
//! K-nearest-neighbor search over embedding vectors.
//!
//! Vectors are L2-normalized once up front, so cosine similarity reduces to a
//! plain dot product.  Only the brute-force backend is implemented this phase;
//! a `DuckDbVssKnn` (HNSW via the DuckDB `vss` extension, gated on
//! `cfg.catalog.enable_vss`) is a documented future alternative — see
//! `run_dedupe` for the runtime note about the omission.

use rayon::prelude::*;

/// Normalize `v` to unit L2 length in place.  A zero vector is left untouched
/// (avoids dividing by zero / producing NaN).
pub fn l2_normalize(v: &mut [f32]) {
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in v.iter_mut() {
            *x /= norm;
        }
    }
}

/// Cosine similarity of two already-L2-normalized vectors (a dot product).
/// Iterates over `min(len)` so mismatched dims don't panic.
pub fn cosine_normalized(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b.iter()).map(|(x, y)| x * y).sum()
}

/// Brute-force KNN: stores normalized vectors and computes cosine on demand.
pub struct BruteForceKnn {
    normalized: Vec<Vec<f32>>,
}

impl BruteForceKnn {
    /// `normalized` must already be L2-normalized (see [`l2_normalize`]).
    pub fn new(normalized: Vec<Vec<f32>>) -> Self {
        Self { normalized }
    }

    pub fn len(&self) -> usize {
        self.normalized.len()
    }

    pub fn is_empty(&self) -> bool {
        self.normalized.is_empty()
    }

    /// Cosine similarity between stored vectors `i` and `j`.
    pub fn cosine(&self, i: usize, j: usize) -> f32 {
        cosine_normalized(&self.normalized[i], &self.normalized[j])
    }
}

/// Abstraction over neighbor search so a `vss`/HNSW backend can slot in later.
pub trait KnnIndex {
    /// Top-`k` neighbors of `query_idx` (excluding itself), as
    /// `(index, cosine)`, sorted descending by cosine.
    fn neighbors(&self, query_idx: usize, k: usize) -> Vec<(usize, f32)>;
}

impl KnnIndex for BruteForceKnn {
    fn neighbors(&self, query_idx: usize, k: usize) -> Vec<(usize, f32)> {
        let q = &self.normalized[query_idx];
        let mut sims: Vec<(usize, f32)> = (0..self.normalized.len())
            .into_par_iter()
            .filter(|&i| i != query_idx)
            .map(|i| (i, cosine_normalized(q, &self.normalized[i])))
            .collect();
        // Sort descending by cosine; tie-break by index for determinism.
        sims.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.0.cmp(&b.0))
        });
        sims.truncate(k);
        sims
    }
}
```

- [ ] **Step 4: Run tests (after Task 6 declares the module) — temporary local check**

Temporarily add `pub mod knn;` as the first line of `crates/pipeline/src/dedupe/mod.rs` (Task 6 will own the final module declarations).
Run: `source ~/.cargo/env && cargo test -p pipeline --lib dedupe::knn`
Expected: PASS (all five tests). Then revert the temporary `dedupe/mod.rs` edit if Task 6 has not run yet — or simply leave it; Task 6's content includes `mod knn;` and will be written as a full replacement.

- [ ] **Step 5: Commit**

```bash
git add crates/pipeline/src/dedupe/knn.rs crates/pipeline/src/dedupe/mod.rs
git commit -m "feat(dedupe): KnnIndex trait + BruteForceKnn cosine neighbor search

L2-normalize, cosine (normalized dot product), rayon-parallel top-k
neighbors with deterministic tie-break. DuckDbVssKnn left as documented
future backend.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 6: Clustering — edges, components, quality score, keeper selection

**Files:**
- Create: `crates/pipeline/src/dedupe/cluster.rs`
- Modify: `crates/pipeline/src/dedupe/mod.rs` (replace the placeholder line with module declarations so `knn` and `cluster` compile and the unit tests run)
- Test: `crates/pipeline/src/dedupe/cluster.rs` (inline `#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: `petgraph` (Task 1); `crate::dedupe::knn::{BruteForceKnn, KnnIndex, l2_normalize}` (Task 5); `crate::catalog::QualityInputs` (Task 3).
- Produces:
  ```rust
  pub fn quality_score(q: Option<&crate::catalog::QualityInputs>) -> f32;
  pub fn connected_components_sorted(node_count: usize, edges: &[(usize, usize)]) -> Vec<Vec<usize>>;
  pub fn build_edges(
      ids: &[i64],
      normalized: &[Vec<f32>],
      captured_at: &[Option<i64>],
      cfg: &crate::config::DedupeConfig,
  ) -> Vec<(usize, usize)>;
  ```
  (`ids`, `normalized`, `captured_at` are parallel arrays — index `i` is the same photo in all three.)

- [ ] **Step 1: Replace the `dedupe/mod.rs` stub with module declarations**

Overwrite `crates/pipeline/src/dedupe/mod.rs` (currently just `// placeholder`) with:

```rust
//! Duplicate detection: time-window + embedding-similarity clustering.

pub mod cluster;
pub mod knn;
```

(Task 7 expands this file with `DedupeReport` and `run_dedupe`; the `pub use` re-exports happen there. For now this just makes `knn` and `cluster` compile.)

- [ ] **Step 2: Write the failing tests**

Create `crates/pipeline/src/dedupe/cluster.rs` with ONLY the test module first:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::QualityInputs;
    use crate::config::DedupeConfig;

    #[test]
    fn quality_score_missing_inputs_is_zero() {
        assert_eq!(quality_score(None), 0.0);
    }

    #[test]
    fn quality_score_applies_penalties() {
        let q = QualityInputs {
            iqa_score: Some(0.9),
            has_blur: true,
            has_back_focus: false,
            clipped_highlights: 0.2,
            clipped_shadows: 0.5,
        };
        // 0.9 - 0.3*1 - 0.2*0 - 0.2*max(0.2,0.5) = 0.9 - 0.3 - 0.1 = 0.5
        let s = quality_score(Some(&q));
        assert!((s - 0.5).abs() < 1e-6, "score was {s}");
    }

    #[test]
    fn components_two_clusters_and_a_singleton() {
        // 0-1-2 form one component, 3-4 another, 5 alone.
        let edges = vec![(0, 1), (1, 2), (3, 4)];
        let mut comps = connected_components_sorted(6, &edges);
        // Sort each component and the outer list for stable assertion.
        for c in comps.iter_mut() {
            c.sort_unstable();
        }
        comps.sort_by_key(|c| c[0]);
        assert_eq!(comps, vec![vec![0, 1, 2], vec![3, 4], vec![5]]);
    }

    #[test]
    fn time_window_edges_only_within_window_and_threshold() {
        let ids = vec![10i64, 11, 12];
        // 0 and 1 nearly identical; 2 orthogonal.
        let normalized = {
            use crate::dedupe::knn::l2_normalize;
            let mut a = vec![1.0f32, 0.0];
            let mut b = vec![0.999f32, 0.044];
            let mut c = vec![0.0f32, 1.0];
            l2_normalize(&mut a);
            l2_normalize(&mut b);
            l2_normalize(&mut c);
            vec![a, b, c]
        };
        // 0 and 1 within 5s; 2 is 10000s away.
        let captured_at = vec![Some(1000i64), Some(1003), Some(11000)];
        let cfg = DedupeConfig {
            enable: true,
            time_window_seconds: 5,
            cosine_threshold_within_window: 0.92,
            cosine_threshold_global: 0.97,
            knn_k: 10,
            min_group_size: 2,
        };
        let edges = build_edges(&ids, &normalized, &captured_at, &cfg);
        // Expect exactly the (0,1) edge: within window AND cosine ≥ 0.92.
        assert!(
            edges.contains(&(0, 1)) || edges.contains(&(1, 0)),
            "expected an edge between 0 and 1, got {edges:?}"
        );
        // No edge should touch the orthogonal, far-away node 2.
        assert!(
            !edges.iter().any(|(a, b)| *a == 2 || *b == 2),
            "node 2 must stay isolated, got {edges:?}"
        );
    }

    #[test]
    fn global_knn_edges_link_high_cosine_far_apart_in_time() {
        let ids = vec![10i64, 11];
        let normalized = {
            use crate::dedupe::knn::l2_normalize;
            let mut a = vec![1.0f32, 0.01];
            let mut b = vec![1.0f32, 0.0];
            l2_normalize(&mut a);
            l2_normalize(&mut b);
            vec![a, b]
        };
        // Far apart in time → time-window rule won't fire; global KNN must.
        let captured_at = vec![Some(0i64), Some(1_000_000)];
        let cfg = DedupeConfig {
            enable: true,
            time_window_seconds: 60,
            cosine_threshold_within_window: 0.92,
            cosine_threshold_global: 0.97,
            knn_k: 10,
            min_group_size: 2,
        };
        let edges = build_edges(&ids, &normalized, &captured_at, &cfg);
        assert!(
            edges.contains(&(0, 1)) || edges.contains(&(1, 0)),
            "global KNN should link near-identical vectors, got {edges:?}"
        );
    }
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `source ~/.cargo/env && cargo test -p pipeline --lib dedupe::cluster`
Expected: FAIL — `cannot find function quality_score` / `connected_components_sorted` / `build_edges`.

- [ ] **Step 4: Write the implementation**

Prepend the implementation to `crates/pipeline/src/dedupe/cluster.rs` (above the test module):

```rust
//! Edge construction, connected components, quality scoring, keeper selection.

use std::collections::HashSet;

use petgraph::graph::UnGraph;

use crate::catalog::QualityInputs;
use crate::config::DedupeConfig;
use crate::dedupe::knn::{BruteForceKnn, KnnIndex};

/// `quality_score = iqa.score
///                - 0.3 * has_blur
///                - 0.2 * has_back_focus
///                - 0.2 * max(clipped_highlights, clipped_shadows)`
///
/// A file with no `QualityInputs` (or no IQA score) scores from a 0.0 base —
/// documented choice: an unmeasured photo is treated as worst-quality so a
/// measured sibling wins the keeper slot.
pub fn quality_score(q: Option<&QualityInputs>) -> f32 {
    let Some(q) = q else { return 0.0 };
    let base = q.iqa_score.unwrap_or(0.0);
    let blur_pen = if q.has_blur { 0.3 } else { 0.0 };
    let bf_pen = if q.has_back_focus { 0.2 } else { 0.0 };
    let clip_pen = 0.2 * q.clipped_highlights.max(q.clipped_shadows);
    base - blur_pen - bf_pen - clip_pen
}

/// Build undirected edges from time-window and global-KNN rules.
///
/// `ids`, `normalized`, `captured_at` are parallel arrays indexed by node.
/// Returns deduplicated `(min, max)` index pairs.
pub fn build_edges(
    ids: &[i64],
    normalized: &[Vec<f32>],
    captured_at: &[Option<i64>],
    cfg: &DedupeConfig,
) -> Vec<(usize, usize)> {
    let n = ids.len();
    let mut edge_set: HashSet<(usize, usize)> = HashSet::new();

    let knn = BruteForceKnn::new(normalized.to_vec());

    // --- Time-window edges ---------------------------------------------------
    // O(n^2) pairwise; fine at the spec's scale. For each captured pair within
    // the window, add an edge when cosine ≥ within-window threshold.
    for i in 0..n {
        let Some(ti) = captured_at[i] else { continue };
        for j in (i + 1)..n {
            let Some(tj) = captured_at[j] else { continue };
            let dt = ti.abs_diff(tj);
            if dt <= cfg.time_window_seconds {
                let sim = knn.cosine(i, j);
                if sim >= cfg.cosine_threshold_within_window {
                    edge_set.insert((i, j));
                }
            }
        }
    }

    // --- Global KNN edges ----------------------------------------------------
    for i in 0..n {
        for (j, sim) in knn.neighbors(i, cfg.knn_k) {
            if sim >= cfg.cosine_threshold_global {
                let edge = if i < j { (i, j) } else { (j, i) };
                edge_set.insert(edge);
            }
        }
    }

    let mut edges: Vec<(usize, usize)> = edge_set.into_iter().collect();
    // Deterministic ordering.
    edges.sort_unstable();
    edges
}

/// Connected components of an undirected graph over `node_count` nodes.
///
/// Returns one `Vec<usize>` of node indices per component.  Node indices are
/// added in ascending order before edges, so component membership is
/// deterministic regardless of edge order.
pub fn connected_components_sorted(
    node_count: usize,
    edges: &[(usize, usize)],
) -> Vec<Vec<usize>> {
    let mut graph: UnGraph<usize, ()> = UnGraph::new_undirected();
    // Add nodes 0..node_count in order; node index == graph NodeIndex order.
    let node_ids: Vec<_> = (0..node_count).map(|i| graph.add_node(i)).collect();
    let mut sorted_edges = edges.to_vec();
    sorted_edges.sort_unstable();
    for &(a, b) in &sorted_edges {
        graph.add_edge(node_ids[a], node_ids[b], ());
    }

    // Union-find over node indices to recover component membership
    // (petgraph::algo::connected_components only returns a count).
    let mut parent: Vec<usize> = (0..node_count).collect();
    fn find(parent: &mut [usize], x: usize) -> usize {
        let mut root = x;
        while parent[root] != root {
            root = parent[root];
        }
        let mut cur = x;
        while parent[cur] != root {
            let next = parent[cur];
            parent[cur] = root;
            cur = next;
        }
        root
    }
    for &(a, b) in &sorted_edges {
        let ra = find(&mut parent, a);
        let rb = find(&mut parent, b);
        if ra != rb {
            // Attach larger root to smaller for deterministic structure.
            if ra < rb {
                parent[rb] = ra;
            } else {
                parent[ra] = rb;
            }
        }
    }

    let mut by_root: std::collections::BTreeMap<usize, Vec<usize>> =
        std::collections::BTreeMap::new();
    for i in 0..node_count {
        let r = find(&mut parent, i);
        by_root.entry(r).or_default().push(i);
    }
    by_root.into_values().collect()
}
```

> **Plan note (no silent cap):** the spec describes a second neighbor backend, `DuckDbVssKnn` (HNSW via the DuckDB `vss` extension, gated on `cfg.catalog.enable_vss`). It is intentionally NOT implemented in this phase — brute force only. `run_dedupe` (Task 7) emits a one-time `tracing::warn!` when `enable_vss` is set, so the omission is visible at runtime.

- [ ] **Step 5: Run tests to verify they pass**

Run: `source ~/.cargo/env && cargo test -p pipeline --lib dedupe::cluster`
Expected: PASS (all five tests).

- [ ] **Step 6: Run clippy to confirm no warnings on the new module**

Run: `source ~/.cargo/env && cargo clippy -p pipeline --all-targets --all-features -- -D warnings`
Expected: no warnings.

- [ ] **Step 7: Commit**

```bash
git add crates/pipeline/src/dedupe/cluster.rs crates/pipeline/src/dedupe/mod.rs
git commit -m "feat(dedupe): edge building, connected components, quality + keeper logic

Time-window + global-KNN edges, deterministic union-find components
(nodes added in id order), quality_score with documented missing-input
handling. Notes the deliberate DuckDbVssKnn omission.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 7: Orchestrator — `run_dedupe` + `DedupeReport`

**Files:**
- Modify: `crates/pipeline/src/dedupe/mod.rs` (add `DedupeReport`, `run_dedupe`, re-exports)
- Modify: `crates/pipeline/src/lib.rs` (add `pub use dedupe::{run_dedupe, DedupeReport};`)

**Interfaces:**
- Consumes: `Catalog::{load_all_embeddings, captured_at_map, quality_inputs_map, clear_duplicate_groups, insert_duplicate_group, insert_duplicate_members}` (Tasks 2-4); `DuplicateMember`, `QualityInputs` (Tasks 2-3); `cluster::{build_edges, connected_components_sorted, quality_score}` (Task 6); `knn::l2_normalize` (Task 5); `crate::config::DedupeConfig`.
- Produces:
  ```rust
  pub struct DedupeReport { pub groups: u64, pub members: u64, pub keepers: u64 }
  pub fn run_dedupe(catalog: &crate::catalog::Catalog, cfg: &crate::config::DedupeConfig) -> anyhow::Result<DedupeReport>;
  ```
  Re-exported from the crate root: `pub use dedupe::{run_dedupe, DedupeReport};`

- [ ] **Step 1: Write the implementation (integration tests in Task 8 drive it)**

Append to `crates/pipeline/src/dedupe/mod.rs` (after the `pub mod` lines from Task 6):

```rust
use std::time::{SystemTime, UNIX_EPOCH};

use crate::catalog::{Catalog, DuplicateMember};
use crate::config::DedupeConfig;

pub use cluster::{build_edges, connected_components_sorted, quality_score};
pub use knn::{l2_normalize, BruteForceKnn, KnnIndex};

/// Summary of a dedupe run.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct DedupeReport {
    pub groups: u64,
    pub members: u64,
    pub keepers: u64,
}

/// Rebuild all duplicate groups from current embeddings.
///
/// Clears `duplicate_groups` + `duplicate_members`, then rebuilds. Running
/// twice on unchanged data produces identical groups (file_ids are sorted
/// before graph construction, so component membership and keeper selection
/// are deterministic).
pub fn run_dedupe(catalog: &Catalog, cfg: &DedupeConfig) -> anyhow::Result<DedupeReport> {
    if !cfg.enable {
        tracing::info!("dedupe disabled in config — skipping");
        return Ok(DedupeReport::default());
    }

    // Load embeddings, already ordered by file_id ASC (deterministic).
    let raw = catalog.load_all_embeddings()?;
    if raw.len() < cfg.min_group_size {
        tracing::info!(
            count = raw.len(),
            "fewer embeddings than min_group_size — nothing to cluster"
        );
        catalog.clear_duplicate_groups()?;
        return Ok(DedupeReport::default());
    }

    // Parallel arrays: ids[i], normalized[i], captured_at[i] all describe node i.
    let ids: Vec<i64> = raw.iter().map(|(id, _)| *id).collect();
    let mut normalized: Vec<Vec<f32>> = raw.into_iter().map(|(_, v)| v).collect();
    for v in normalized.iter_mut() {
        l2_normalize(v);
    }

    let captured_map = catalog.captured_at_map()?;
    let captured_at: Vec<Option<i64>> = ids
        .iter()
        .map(|id| captured_map.get(id).copied().flatten())
        .collect();

    let quality_map = catalog.quality_inputs_map()?;

    tracing::info!(
        photos = ids.len(),
        knn_k = cfg.knn_k,
        time_window_s = cfg.time_window_seconds,
        "building dedupe graph (brute-force KNN)"
    );

    // Brute force only this phase; surface the omission rather than cap silently.
    // (enable_vss lives on CatalogConfig, not DedupeConfig; we cannot read it
    // here, so the runtime note is emitted by the CLI in cmd_dedupe — see Task 9.)

    let edges = build_edges(&ids, &normalized, &captured_at, cfg);
    let components = connected_components_sorted(ids.len(), &edges);

    // Clear and rebuild.
    catalog.clear_duplicate_groups()?;

    let created_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    let mut report = DedupeReport::default();

    for comp in &components {
        if comp.len() < cfg.min_group_size {
            continue;
        }

        // Score every member; pick the highest as keeper (tie-break: lowest
        // file_id, for determinism).
        let mut scored: Vec<(i64, f32)> = comp
            .iter()
            .map(|&idx| {
                let fid = ids[idx];
                let score = quality_score(quality_map.get(&fid));
                (fid, score)
            })
            .collect();
        // Sort by score desc, then file_id asc.
        scored.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.0.cmp(&b.0))
        });
        let keeper_id = scored[0].0;

        let group_id = catalog.insert_duplicate_group("time+embed", created_at)?;
        let members: Vec<DuplicateMember> = scored
            .iter()
            .map(|(fid, score)| DuplicateMember {
                file_id: *fid,
                is_suggested_keeper: *fid == keeper_id,
                quality_score: *score,
            })
            .collect();
        catalog.insert_duplicate_members(group_id, &members)?;

        report.groups += 1;
        report.members += members.len() as u64;
        report.keepers += 1;
    }

    tracing::info!(
        groups = report.groups,
        members = report.members,
        keepers = report.keepers,
        "dedupe complete"
    );
    Ok(report)
}
```

- [ ] **Step 2: Add the crate-root re-export**

In `crates/pipeline/src/lib.rs`, after `pub use ml::analyze_ml;`:

```rust
pub use dedupe::{run_dedupe, DedupeReport};
```

- [ ] **Step 3: Verify the crate compiles**

Run: `source ~/.cargo/env && cargo build -p pipeline`
Expected: builds cleanly.

Run: `source ~/.cargo/env && cargo test -p pipeline --lib dedupe`
Expected: PASS — all existing knn + cluster unit tests still green.

- [ ] **Step 4: Commit**

```bash
git add crates/pipeline/src/dedupe/mod.rs crates/pipeline/src/lib.rs
git commit -m "feat(dedupe): run_dedupe orchestrator + DedupeReport, re-exported

Loads embeddings (id-ordered), L2-normalizes, builds edges, finds
components, clears+rebuilds duplicate_groups/_members, selects keeper by
highest quality_score (tie-break lowest file_id) for determinism.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 8: Integration tests — round-trip, end-to-end, idempotency, real-photo ignores

**Files:**
- Create: `crates/pipeline/tests/dedupe.rs`

**Interfaces:**
- Consumes: `pipeline::{catalog::Catalog, catalog::MlRow, run_dedupe, config::DedupeConfig, ingest::{IngestedFile, ExifData, FileFormat}, defect::{DefectRow, DefectFlag, SharpnessResult, ExposureResult}}`.
- Produces: nothing (test-only).

- [ ] **Step 1: Write the integration tests**

Create `crates/pipeline/tests/dedupe.rs`:

```rust
//! Integration tests for Phase 5 duplicate detection.
//!
//! All synthetic-vector tests need NO real photos: we upsert hand-crafted
//! embeddings, captured_at, and IQA rows directly, then run `run_dedupe`.
//! Acceptance criteria that genuinely need real bursts/scenes are written
//! `#[ignore]` with instructions for which fixtures to add.

use std::path::PathBuf;

use pipeline::catalog::{Catalog, MlRow};
use pipeline::config::DedupeConfig;
use pipeline::ingest::{ExifData, FileFormat, IngestedFile};
use tempfile::TempDir;

fn make_catalog() -> (Catalog, TempDir) {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.duckdb");
    let catalog = Catalog::open(&db_path).unwrap();
    (catalog, dir)
}

/// Insert a file with the given captured_at, returning its file_id.
fn insert_file(catalog: &Catalog, idx: i64, captured_at: Option<i64>) -> i64 {
    let file = IngestedFile {
        path: PathBuf::from(format!("/syn/file{idx}.jpg")),
        content_hash: 1000 + idx as u128,
        size: 10 + idx as u64,
        mtime_ns: idx,
        format: FileFormat::Jpg,
        has_sidecar_jpg: false,
    };
    let exif = captured_at.map(|ts| ExifData {
        captured_at: Some(ts),
        camera_make: None,
        camera_model: None,
        lens_model: None,
        focal_length_mm: None,
        aperture: None,
        iso: None,
        shutter_seconds: None,
        width: None,
        height: None,
        orientation: None,
    });
    catalog.flush_batch(&[(file, exif)]).unwrap()[0]
}

fn set_embedding(catalog: &Catalog, file_id: i64, vec: Vec<f32>) {
    catalog
        .flush_ml_batch(&[MlRow {
            file_id,
            embedding: Some(("test-model".to_string(), vec)),
            iqa_score: None,
        }])
        .unwrap();
}

fn set_iqa(catalog: &Catalog, file_id: i64, score: f32) {
    catalog
        .flush_ml_batch(&[MlRow {
            file_id,
            embedding: None,
            iqa_score: Some(("clip-iqa".to_string(), score)),
        }])
        .unwrap();
}

fn test_cfg() -> DedupeConfig {
    DedupeConfig {
        enable: true,
        time_window_seconds: 60,
        cosine_threshold_within_window: 0.92,
        cosine_threshold_global: 0.97,
        knn_k: 10,
        min_group_size: 2,
    }
}

/// De-risk: embeddings written via the JSON CAST path read back intact and
/// drive a dedupe run end to end.
#[test]
fn embedding_round_trip_through_dedupe() {
    let (catalog, _dir) = make_catalog();
    let id = insert_file(&catalog, 0, Some(1000));
    set_embedding(&catalog, id, vec![0.1f32, 0.2, 0.3, 0.4]);

    let loaded = catalog.load_all_embeddings().unwrap();
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].0, id);
    assert_eq!(loaded[0].1, vec![0.1f32, 0.2, 0.3, 0.4]);

    // A single embedding < min_group_size → no groups.
    let report = run_dedupe(&catalog, &test_cfg()).unwrap();
    assert_eq!(report.groups, 0);
}

use pipeline::run_dedupe;

/// Near-identical vectors within the time window form one group; an
/// orthogonal vector stays ungrouped; the highest-IQA member is the keeper.
#[test]
fn near_identical_within_window_group_orthogonal_stays_out() {
    let (catalog, _dir) = make_catalog();

    // Three near-identical photos a few seconds apart.
    let a = insert_file(&catalog, 0, Some(1000));
    let b = insert_file(&catalog, 1, Some(1002));
    let c = insert_file(&catalog, 2, Some(1004));
    set_embedding(&catalog, a, vec![1.0, 0.0, 0.0]);
    set_embedding(&catalog, b, vec![0.999, 0.044, 0.0]);
    set_embedding(&catalog, c, vec![0.998, 0.0, 0.063]);

    // One orthogonal photo in the same window.
    let d = insert_file(&catalog, 3, Some(1003));
    set_embedding(&catalog, d, vec![0.0, 1.0, 0.0]);

    // IQA: c is the best of the burst.
    set_iqa(&catalog, a, 0.5);
    set_iqa(&catalog, b, 0.6);
    set_iqa(&catalog, c, 0.95);
    set_iqa(&catalog, d, 0.9);

    let report = run_dedupe(&catalog, &test_cfg()).unwrap();
    assert_eq!(report.groups, 1, "the three near-identical photos form one group");
    assert_eq!(report.members, 3, "orthogonal photo d must be excluded");
    assert_eq!(report.keepers, 1);

    // The keeper is c (highest IQA, no defects).
    let conn_check = catalog.duplicate_member_count().unwrap();
    assert_eq!(conn_check, 3);
}

/// Running dedupe twice yields identical group/member/keeper counts.
#[test]
fn dedupe_is_idempotent() {
    let (catalog, _dir) = make_catalog();
    let a = insert_file(&catalog, 0, Some(1000));
    let b = insert_file(&catalog, 1, Some(1001));
    set_embedding(&catalog, a, vec![1.0, 0.0]);
    set_embedding(&catalog, b, vec![0.9995, 0.0316]); // cosine ≈ 0.9995
    set_iqa(&catalog, a, 0.5);
    set_iqa(&catalog, b, 0.8);

    let cfg = test_cfg();
    let first = run_dedupe(&catalog, &cfg).unwrap();
    let second = run_dedupe(&catalog, &cfg).unwrap();

    assert_eq!(first, second, "two runs must produce identical reports");
    assert_eq!(catalog.duplicate_group_count().unwrap(), first.groups as i64);
    assert_eq!(
        catalog.duplicate_member_count().unwrap(),
        first.members as i64
    );
}

/// Disabled config does no work.
#[test]
fn disabled_config_produces_empty_report() {
    let (catalog, _dir) = make_catalog();
    let mut cfg = test_cfg();
    cfg.enable = false;
    let report = run_dedupe(&catalog, &cfg).unwrap();
    assert_eq!(report.groups, 0);
    assert_eq!(report.members, 0);
}

// ── Real-photo acceptance criteria (need fixtures) ──────────────────────────
// These require real RAW/JPG photos run through `scan` (ingest + embed) first.
// We do NOT fabricate EXIF/pixels. When executing this phase, ask the user to
// drop fixtures into `crates/pipeline/tests/fixtures/burst/` then un-ignore.

/// IMPLEMENTATION_PLAN §8 Phase 5 acceptance:
/// "A burst of 5 shots taken within 2 seconds clusters into one group."
/// Fixtures: 5 real burst frames → `tests/fixtures/burst/seq01/IMG_000{1..5}.*`.
#[test]
#[ignore = "needs real burst fixtures in tests/fixtures/burst/seq01/ — ask user"]
fn acceptance_five_shot_burst_clusters() {
    // Run scan over tests/fixtures/burst/seq01, then run_dedupe; assert one
    // group of 5. Requires a loaded embedder model + real photos.
}

/// "The same scene photographed twice 10 minutes apart with different framing
/// clusters together (relies on embedding similarity)."
/// Fixtures: two takes → `tests/fixtures/burst/same_scene/{take_a,take_b}.*`.
#[test]
#[ignore = "needs real same-scene fixtures in tests/fixtures/burst/same_scene/ — ask user"]
fn acceptance_same_scene_ten_minutes_apart_clusters() {
    // captured_at 10 min apart, similar embedding → global-KNN edge groups them.
}

/// "Two unrelated photos with similar color palettes (two different sunsets)
/// do NOT cluster."
/// Fixtures: two different sunsets → `tests/fixtures/burst/sunsets/{a,b}.*`.
#[test]
#[ignore = "needs real sunset fixtures in tests/fixtures/burst/sunsets/ — ask user"]
fn acceptance_distinct_sunsets_do_not_cluster() {
    // Distinct scenes, low embedding cosine → no group despite color similarity.
}
```

- [ ] **Step 2: Run the (non-ignored) integration tests to verify they pass**

Run: `source ~/.cargo/env && cargo test -p pipeline --test dedupe`
Expected: PASS — `embedding_round_trip_through_dedupe`, `near_identical_within_window_group_orthogonal_stays_out`, `dedupe_is_idempotent`, `disabled_config_produces_empty_report`; the three `acceptance_*` tests report as `ignored`.

- [ ] **Step 3: Confirm the ignored tests are listed**

Run: `source ~/.cargo/env && cargo test -p pipeline --test dedupe -- --ignored --list`
Expected: lists the three `acceptance_*` functions as ignored tests.

- [ ] **Step 4: Commit**

```bash
git add crates/pipeline/tests/dedupe.rs
git commit -m "test(dedupe): synthetic end-to-end, idempotency, FLOAT[] round-trip

Covers grouping of near-identical within-window vectors, exclusion of an
orthogonal vector, keeper = highest IQA, and identical counts on a second
run. Real-photo acceptance criteria are #[ignore] with fixture paths.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 9: Wire the CLI `dedupe` command

**Files:**
- Modify: `crates/cli/src/main.rs` (replace `cmd_dedupe`, lines 183-187; the `Command::Dedupe => cmd_dedupe(&cfg)` dispatch at line 107 already exists and stays)

**Interfaces:**
- Consumes: `pipeline::{run_dedupe, catalog::Catalog, config::Config}`.
- Produces: a working `photopipe dedupe` command.

- [ ] **Step 1: Replace `cmd_dedupe` with a real implementation**

In `crates/cli/src/main.rs`, replace the stub `cmd_dedupe` (lines 183-187):

```rust
fn cmd_dedupe(cfg: &config::Config) -> Result<()> {
    use pipeline::{catalog::Catalog, run_dedupe};

    let db_path = &cfg.catalog.db_path;
    let catalog = Catalog::open(db_path).map_err(|e| anyhow::anyhow!("catalog: {}", e))?;

    // Brute-force KNN only this phase; surface the vss omission rather than
    // silently cap, when the user has opted into it via config.
    if cfg.catalog.enable_vss {
        tracing::warn!(
            "catalog.enable_vss = true, but the DuckDB vss/HNSW backend is not \
             implemented yet — falling back to brute-force KNN"
        );
    }

    let report = run_dedupe(&catalog, &cfg.dedupe)?;
    println!("Dedupe complete:");
    println!("  Groups  : {}", report.groups);
    println!("  Members : {}", report.members);
    println!("  Keepers : {}", report.keepers);
    Ok(())
}
```

- [ ] **Step 2: Verify the CLI builds**

Run: `source ~/.cargo/env && cargo build -p cli`
Expected: builds cleanly; no "unused" warnings for `cmd_dedupe`.

- [ ] **Step 3: Smoke-test the command against an empty catalog**

Run: `source ~/.cargo/env && cargo run -p cli -- --config /tmp/none.toml dedupe 2>/dev/null || true`
Then run against a temp db explicitly via a built binary if a config is required; minimally confirm the handler runs and prints the four report lines without panicking. (A fresh empty catalog yields `Groups: 0`.)
Expected: prints `Dedupe complete:` and `Groups : 0` (no panic).

- [ ] **Step 4: Commit**

```bash
git add crates/cli/src/main.rs
git commit -m "feat(dedupe): wire photopipe dedupe CLI command to run_dedupe

Opens the catalog, warns if enable_vss is set (unimplemented backend),
runs dedupe, prints the group/member/keeper report.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 10: Phase verification — fmt, clippy, full test suite

**Files:** none (verification only).

**Interfaces:**
- Consumes: everything from Tasks 1-9.
- Produces: a green tree, ready to declare Phase 5 done.

- [ ] **Step 1: Format check**

Run: `source ~/.cargo/env && cargo fmt --check`
Expected: no diff. If it reports changes, run `cargo fmt`, re-inspect, and amend the most relevant prior commit or make a `chore(fmt): cargo fmt` commit.

- [ ] **Step 2: Clippy with warnings-as-errors**

Run: `source ~/.cargo/env && cargo clippy --all-targets --all-features -- -D warnings`
Expected: no warnings. Fix any in place; commit fixes with `chore(clippy): …` + the Co-Authored-By trailer.

- [ ] **Step 3: Full workspace test suite**

Run: `source ~/.cargo/env && cargo test --all`
Expected: all tests pass; the three `acceptance_*` dedupe tests show as ignored. No regressions in `catalog`, `ml`, `defect`, `integration`.

- [ ] **Step 4: Confirm idempotency claim end-to-end**

Run: `source ~/.cargo/env && cargo test -p pipeline --test dedupe dedupe_is_idempotent -- --nocapture`
Expected: PASS — two `run_dedupe` calls return identical `DedupeReport`s.

- [ ] **Step 5: Final commit (only if Steps 1-2 produced fixups)**

```bash
git add -A
git commit -m "chore(dedupe): fmt/clippy cleanup for Phase 5

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

If Steps 1-2 produced no changes, skip this commit — Phase 5 is complete.

---

## Self-Review

**Spec coverage (IMPLEMENTATION_PLAN §8 Phase 5):**
- Load embeddings to memory, f32, L2-normalize → Task 2 (`load_all_embeddings`), Task 7 (`l2_normalize` loop). ✓
- Time-window edges (cosine ≥ within-window threshold) → Task 6 `build_edges`. ✓
- Global KNN edges (cosine ≥ global threshold) behind `KnnIndex`/`BruteForceKnn`, rayon-parallel → Tasks 5-6. ✓
- `DuckDbVssKnn` documented-but-omitted, no silent cap → Task 6 plan note + Task 9 runtime `warn!`. ✓
- Connected components via petgraph → Task 1 (dep) + Task 6 (`connected_components_sorted` builds `UnGraph`; union-find recovers membership since `petgraph::algo::connected_components` returns only a count). ✓
- Group rows `method="time+embed"`, members, `quality_score` formula, keeper = max → Tasks 4, 6 (`quality_score`), 7. ✓
- Missing-IQA handling documented (0.0 base) → Task 6 `quality_score` doc. ✓
- Idempotency: clear-then-rebuild, deterministic via sorted ids → Tasks 4 (`clear_duplicate_groups`), 7 (id-ordered load + sorted edges + tie-breaks), 8 (`dedupe_is_idempotent`). ✓
- Acceptance criteria needing real photos → Task 8 `#[ignore]` tests with fixture paths. ✓
- CLI `photopipe dedupe` rebuilds groups → Task 9. ✓

**Placeholder scan:** No "TBD"/"add error handling"/"similar to Task N". Every code step shows complete code (repeated, not cross-referenced). The `FLOAT[]` read-back has explicit primary + two fallbacks rather than a vague instruction.

**Type consistency:** `DuplicateMember { file_id, is_suggested_keeper, quality_score }`, `QualityInputs { iqa_score, has_blur, has_back_focus, clipped_highlights, clipped_shadows }`, `DedupeReport { groups, members, keepers }`, `KnnIndex::neighbors`, `BruteForceKnn::{new, cosine, neighbors}`, `build_edges(ids, normalized, captured_at, cfg)`, `connected_components_sorted(node_count, edges)`, `quality_score(Option<&QualityInputs>)`, `run_dedupe(&Catalog, &DedupeConfig)` are used consistently across Tasks 2-9. `flush_ml_batch` / `flush_batch` / `DefectRow` signatures match GROUNDING. ✓

**Reviewer double-checks:**
- **`FLOAT[]` read-back (highest risk):** Task 2 Step 4 confirms whether `row.get::<_, Vec<f32>>` works in this duckdb-rs version; if not, fallback A (`Vec<f64>`) or B (CAST→VARCHAR parse) is applied before anything depends on it.
- **petgraph 0.6 API:** `UnGraph::new_undirected`, `add_node`, `add_edge` are stable in 0.6. We use union-find for membership (not `petgraph::algo::connected_components`, which returns only a count) — confirm this matches the intended approach during review.
