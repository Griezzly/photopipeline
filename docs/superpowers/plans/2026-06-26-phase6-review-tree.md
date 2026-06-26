# Phase 6 — Symlink Review Tree Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement `photopipe review-tree <OUTPUT>` — materialize a directory of symlinks (or hardlinks) pointing at original photos, organized into `rejected/`, `duplicates/`, and `uncertain/` categories, so the user can review flagged photos in their OS file browser. Strictly non-destructive.

**Architecture:** A new `pipeline::output` module reads three small query APIs from the catalog (defect flags with confidence + capture year-month, and duplicate groups with keeper/others), then writes a deterministic tree of links under the output root. Date bucketing is done in SQL via DuckDB `strftime(CAST(to_timestamp(captured_at) AS TIMESTAMP), …)` so no date crate is needed. Two build modes: `--regenerate` (delete-then-rebuild) and incremental (ensure expected links exist, prune stale ones), both guarded so only links the tool manages are ever removed. The CLI handler `cmd_review_tree` wires it up.

**Tech Stack:** Rust (edition 2021, stable), `duckdb` (bundled), `std::os::unix::fs::symlink`, `std::fs::hard_link`, `anyhow` at the CLI boundary, `thiserror` (`CatalogError`) inside the library, `tracing` for logs, `tempfile` for tests.

## Global Constraints

Every task's requirements implicitly include this section. Copy the exact values.

- Edition 2021, stable Rust. `anyhow::Result` at CLI boundaries; `thiserror` types (`CatalogError`) inside `pipeline`.
- **DuckDB ONLY** (no SQLite). Reads use `conn.prepare(...)` + `query_map(...)` / `query_row(...)`, locking the private `Mutex<Connection>` exactly as existing catalog methods do: `let conn = self.conn.lock().map_err(|_| CatalogError::Db("mutex poisoned".into()))?;`. Map DuckDB errors with `CatalogError::Db(e.to_string())`. Handle `Err(duckdb::Error::QueryReturnedNoRows) => Ok(None)` where a single row may be absent.
- **No new dependencies.** Do NOT add `chrono` or any date crate — do date formatting in SQL. If you believe a date crate is genuinely required, STOP and surface it as a deviation instead of adding it.
- **Non-destructive — hard constraint:** never modify, move, or delete an original photo. The tool only ever creates/removes symlinks, hardlinks, and directories *inside the output root*. The `--regenerate` delete and incremental prune MUST be guarded so they only touch entries the tool manages (symlinks, and directories that contain only managed entries). If the output root contains a non-symlink **regular file** that the tool did not create, refuse to delete it and return an error.
- **Idempotency / determinism:** re-running `review-tree` on an unchanged catalog produces an identical tree. Incremental mode does no destructive churn when nothing changed.
- `tracing` for logs (`info!` for phase events, `warn!` for per-item failures, `debug!` for detail). The CLI may use `println!` for user-facing output (allowed in `crates/cli`).
- No AGPL deps. No Python at runtime.
- Run before declaring done: `cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D warnings`, `cargo test --all`. In WSL run `source ~/.cargo/env` first.
- Conventional commits. Commit prefixes for this phase: `feat(output):`, `test(output):`. Every commit body ends with the trailer:
  `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`

### Verified facts this plan relies on (do not re-derive)

- **DuckDB date formatting (empirically confirmed against the bundled DuckDB on 2026-06-26):**
  `to_timestamp(<bigint>)` returns `TIMESTAMP WITH TIME ZONE`, which `strftime` rejects. The working form is to CAST first:
  - Year-month: `strftime(CAST(to_timestamp(captured_at) AS TIMESTAMP), '%Y-%m')` → e.g. `2023-06`
  - Year-month-day: `strftime(CAST(to_timestamp(captured_at) AS TIMESTAMP), '%Y-%m-%d')` → e.g. `2023-06-15`
  - NULL `captured_at` (or NULL whole exif row): wrap in `COALESCE(<expr>, 'unknown-date')`.
- **Schema (no migration needed for Phase 6):**
  - `files(id BIGINT pk, path VARCHAR unique, content_hash VARCHAR, …)`
  - `exif(file_id pk, captured_at BIGINT /*unix epoch, nullable*/, …)`
  - `defect_flags(id BIGINT pk, file_id, flag_type VARCHAR, confidence REAL, reason VARCHAR, UNIQUE(file_id, flag_type))`. Flag types: `overexposed`, `underexposed`, `blur`, `back_focus`, `low_iqa`.
  - `duplicate_groups(id BIGINT pk, method VARCHAR, created_at BIGINT)`
  - `duplicate_members(group_id, file_id, is_suggested_keeper BOOLEAN, quality_score REAL, PRIMARY KEY(group_id, file_id))`
- **`OutputConfig { review_tree: String, link_type: LinkType, keeper_strategy: KeeperStrategy }`**, `enum LinkType { Symlink, Hardlink }` (in `crates/pipeline/src/config.rs`). `review_tree` default `"<library>/_review"`, may contain literal `<library>`.
- **`Catalog`** has a private `conn: Mutex<duckdb::Connection>`; new read methods live in `impl Catalog` in `crates/pipeline/src/catalog/mod.rs`. `Catalog::open(path: &Path) -> Result<Self, CatalogError>`.
- **`crates/cli/src/main.rs`** already defines `Command::ReviewTree { output: PathBuf, include: Vec<String>, regenerate: bool }` and a stub `cmd_review_tree(output, include, regenerate, cfg)`.
- **`crates/pipeline/src/output/mod.rs`** is a one-line placeholder. **`lib.rs`** already declares `pub mod output;` (no re-export of its contents yet).

### Category mapping (from IMPLEMENTATION_PLAN §8 Phase 6)

A defect flag maps to a top-level category by its `flag_type` and `confidence`:

| flag_type | confidence ≥ 0.6 | confidence < 0.6 |
|---|---|---|
| `blur` | `rejected/blur/<YYYY-MM>/` | `uncertain/<YYYY-MM>/` |
| `back_focus` | `rejected/back_focus/<YYYY-MM>/` | `uncertain/<YYYY-MM>/` |
| `overexposed` | `rejected/overexposed/<YYYY-MM>/` | `uncertain/<YYYY-MM>/` |
| `underexposed` | `rejected/underexposed/<YYYY-MM>/` | `uncertain/<YYYY-MM>/` |
| `low_iqa` | `rejected/low_quality/<YYYY-MM>/` | `uncertain/<YYYY-MM>/` |

- `low_iqa` is the **only** flag type that maps to the `low_quality/` subfolder when rejected; every other flag type keeps its own name as the subfolder under `rejected/`.
- `uncertain/` is bucketed by `<YYYY-MM>` only (no per-flag-type subfolder), as drawn in the spec tree.
- Duplicate groups go to `duplicates/group_<NNNNN>_<YYYY-MM-DD>/{_keeper,_others}/`, where `<NNNNN>` is the group id zero-padded to 5 digits and `<YYYY-MM-DD>` derives from the keeper's `captured_at` (or `unknown-date`).
- `<YYYY-MM>` / `<YYYY-MM-DD>` come from `exif.captured_at`; NULL → `unknown-date`.

### Top-level category names (for `--include` filtering)

`rejected`, `duplicates`, `uncertain`. Empty `--include` means "all three".

---

## File Structure

- **Modify** `crates/pipeline/src/catalog/mod.rs` — add three read methods + the public row structs (`ReviewEntry`, `ReviewGroup`). One responsibility: catalog read access for the review tree.
- **Create** `crates/pipeline/src/output/mod.rs` (currently a placeholder) — the review-tree builder: tree layout, link creation (symlink/hardlink), collision-safe naming, regenerate vs incremental reconcile, README.txt, the guarded delete. One responsibility: materialize the review tree.
- **Modify** `crates/pipeline/src/lib.rs` — add `pub use output::{build_review_tree, ReviewTreeReport};`.
- **Modify** `crates/cli/src/main.rs:189-198` — replace the `cmd_review_tree` stub body with a real implementation that opens the catalog, resolves the output root, calls `build_review_tree`, and prints the report.
- **Create** `crates/pipeline/tests/review_tree.rs` — integration tests (synthetic JPEGs + directly-inserted catalog rows). Defines its own helpers (test files in this repo do not share helpers).
- Unit tests for collision-naming and `<library>` substitution live in `#[cfg(test)] mod tests` inside `crates/pipeline/src/output/mod.rs`.

---

## Task 1: Catalog read API for the review tree

**Files:**
- Modify: `crates/pipeline/src/catalog/mod.rs` (add public structs near the top, add three methods to `impl Catalog`; add a `#[cfg(test)] mod` test or extend the existing test module at the bottom)

**Interfaces:**
- Consumes: `Catalog` (`self.conn: Mutex<Connection>`, private), `CatalogError::Db(String)`, the existing `flush_batch` / `upsert_exif` / `upsert_defect_flag` write methods (used by tests to set up rows).
- Produces (relied on by Task 2 and Task 4):
  ```rust
  pub struct ReviewEntry {
      pub file_id: i64,
      pub path: std::path::PathBuf,
      pub flag_type: String,
      pub confidence: f32,
      pub year_month: String, // "YYYY-MM" or "unknown-date"
  }
  pub struct ReviewGroup {
      pub group_id: i64,
      pub date: String,                       // "YYYY-MM-DD" or "unknown-date"
      pub keeper: Option<std::path::PathBuf>,  // is_suggested_keeper = TRUE member
      pub others: Vec<std::path::PathBuf>,     // remaining members
  }
  impl Catalog {
      pub fn review_entries(&self) -> Result<Vec<ReviewEntry>, CatalogError>;
      pub fn duplicate_groups_for_review(&self) -> Result<Vec<ReviewGroup>, CatalogError>;
      // Test-only helper (used by Task 5's integration tests) — inserts a
      // duplicate group with one keeper + zero or more others on the single
      // shared connection and returns the new group id.
      #[doc(hidden)]
      pub fn test_insert_duplicate_group(&self, keeper_id: i64, others: &[i64]) -> Result<i64, CatalogError>;
  }
  ```
  `review_entries` returns **all** defect-flag rows joined to their file path and capture year-month; Task 2 buckets them into rejected/uncertain by `confidence`. This keeps the SQL simple and avoids two near-identical queries. `test_insert_duplicate_group` exists so Task 5's tests can populate `duplicate_groups`/`duplicate_members` without opening a second `duckdb::Connection` to the same file (which can deadlock under DuckDB's single-writer model).

- [ ] **Step 1: Write the failing test**

Add this to the existing `#[cfg(test)] mod tests` block at the bottom of `crates/pipeline/src/catalog/mod.rs` (after the last existing test, before the closing `}` of the module). It exercises both new methods.

```rust
    #[test]
    fn review_entries_and_groups_round_trip() {
        use crate::defect::DefectFlag;
        use crate::ingest::{ExifData, FileFormat, IngestedFile};

        let (catalog, _dir) = make_catalog();

        // captured_at 1686830400 = 2023-06-15.
        let mk = |p: &str, hash: u128| IngestedFile {
            path: std::path::PathBuf::from(p),
            content_hash: hash,
            size: 100,
            mtime_ns: 1,
            format: FileFormat::Jpg,
            has_sidecar_jpg: false,
        };
        let exif_dated = ExifData {
            captured_at: Some(1686830400),
            ..Default::default()
        };

        let ids = catalog
            .flush_batch(&[
                (mk("/lib/a.jpg", 1), Some(exif_dated.clone())),
                (mk("/lib/b.jpg", 2), Some(exif_dated.clone())),
                (mk("/lib/c.jpg", 3), None::<ExifData>), // NULL captured_at -> unknown-date
            ])
            .unwrap();

        catalog
            .upsert_defect_flag(
                ids[0],
                &DefectFlag { flag_type: "blur".into(), confidence: 0.8, reason: "x".into() },
            )
            .unwrap();
        catalog
            .upsert_defect_flag(
                ids[2],
                &DefectFlag { flag_type: "low_iqa".into(), confidence: 0.3, reason: "y".into() },
            )
            .unwrap();

        let entries = catalog.review_entries().unwrap();
        assert_eq!(entries.len(), 2);
        let blur = entries.iter().find(|e| e.flag_type == "blur").unwrap();
        assert_eq!(blur.path, std::path::PathBuf::from("/lib/a.jpg"));
        assert!((blur.confidence - 0.8).abs() < 1e-6);
        assert_eq!(blur.year_month, "2023-06");
        let lowq = entries.iter().find(|e| e.flag_type == "low_iqa").unwrap();
        assert_eq!(lowq.year_month, "unknown-date");

        // Duplicate group: a is keeper, b is other; dated 2023-06-15.
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
                 VALUES (?, ?, TRUE, 1.0)",
                duckdb::params![gid, ids[0]],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO duplicate_members (group_id, file_id, is_suggested_keeper, quality_score)
                 VALUES (?, ?, FALSE, 0.5)",
                duckdb::params![gid, ids[1]],
            )
            .unwrap();
        }

        let groups = catalog.duplicate_groups_for_review().unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].date, "2023-06-15");
        assert_eq!(groups[0].keeper, Some(std::path::PathBuf::from("/lib/a.jpg")));
        assert_eq!(groups[0].others, vec![std::path::PathBuf::from("/lib/b.jpg")]);
    }
```

This relies on the existing test helper `make_catalog()` already used in that module's tests, and on `DefectFlag` being `Clone`-free (it is constructed inline). Confirm `make_catalog` exists in the test module (it is used by `files_needing_defect_analysis_filters_correctly`).

- [ ] **Step 2: Run test to verify it fails**

Run: `source ~/.cargo/env && cargo test -p pipeline --lib review_entries_and_groups_round_trip 2>&1 | tail -20`
Expected: FAIL — compile error `no method named review_entries`/`duplicate_groups_for_review`, and `cannot find type ReviewEntry`.

- [ ] **Step 3: Add the public row structs**

Add near the top of `crates/pipeline/src/catalog/mod.rs`, just after the existing `pub struct MlRow { … }` block:

```rust
/// One defect-flagged file as needed by the review tree.
#[derive(Debug, Clone)]
pub struct ReviewEntry {
    pub file_id: i64,
    pub path: std::path::PathBuf,
    pub flag_type: String,
    pub confidence: f32,
    /// "YYYY-MM" from `exif.captured_at`, or "unknown-date" when NULL.
    pub year_month: String,
}

/// One duplicate group as needed by the review tree.
#[derive(Debug, Clone)]
pub struct ReviewGroup {
    pub group_id: i64,
    /// "YYYY-MM-DD" from the keeper's `captured_at`, or "unknown-date".
    pub date: String,
    pub keeper: Option<std::path::PathBuf>,
    pub others: Vec<std::path::PathBuf>,
}
```

- [ ] **Step 4: Implement `review_entries`**

Add this method inside `impl Catalog` (place it after `count_defect_flags`). The year-month is computed in SQL (verified working form: CAST `to_timestamp` to `TIMESTAMP` before `strftime`, COALESCE to `'unknown-date'`).

```rust
    /// Every defect-flagged file with its path, confidence, and capture
    /// year-month ("YYYY-MM" or "unknown-date"). Used to build the review tree.
    pub fn review_entries(&self) -> Result<Vec<ReviewEntry>, CatalogError> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| CatalogError::Db("mutex poisoned".into()))?;
        let mut stmt = conn
            .prepare(
                "SELECT f.id,
                        f.path,
                        d.flag_type,
                        d.confidence,
                        COALESCE(
                            strftime(CAST(to_timestamp(e.captured_at) AS TIMESTAMP), '%Y-%m'),
                            'unknown-date'
                        ) AS year_month
                 FROM defect_flags d
                 JOIN files f ON f.id = d.file_id
                 LEFT JOIN exif e ON e.file_id = f.id
                 ORDER BY f.id, d.flag_type",
            )
            .map_err(|e| CatalogError::Db(e.to_string()))?;

        let rows = stmt
            .query_map([], |row| {
                let file_id: i64 = row.get(0)?;
                let path_str: String = row.get(1)?;
                let flag_type: String = row.get(2)?;
                let confidence: f32 = row.get(3)?;
                let year_month: String = row.get(4)?;
                Ok(ReviewEntry {
                    file_id,
                    path: std::path::PathBuf::from(path_str),
                    flag_type,
                    confidence,
                    year_month,
                })
            })
            .map_err(|e| CatalogError::Db(e.to_string()))?;

        let mut result = Vec::new();
        for row in rows {
            result.push(row.map_err(|e| CatalogError::Db(e.to_string()))?);
        }
        Ok(result)
    }
```

- [ ] **Step 5: Implement `duplicate_groups_for_review`**

Add this method directly after `review_entries`. It pulls every member row (with keeper flag and the group's date derived from the keeper's `captured_at`), then folds rows into groups in Rust.

```rust
    /// All duplicate groups with their suggested keeper and other members,
    /// dated "YYYY-MM-DD" (or "unknown-date") from the keeper's capture time.
    pub fn duplicate_groups_for_review(&self) -> Result<Vec<ReviewGroup>, CatalogError> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| CatalogError::Db("mutex poisoned".into()))?;
        let mut stmt = conn
            .prepare(
                "SELECT m.group_id,
                        f.path,
                        m.is_suggested_keeper,
                        COALESCE(
                            strftime(CAST(to_timestamp(ke.captured_at) AS TIMESTAMP), '%Y-%m-%d'),
                            'unknown-date'
                        ) AS group_date
                 FROM duplicate_members m
                 JOIN files f ON f.id = m.file_id
                 LEFT JOIN duplicate_members km
                        ON km.group_id = m.group_id AND km.is_suggested_keeper = TRUE
                 LEFT JOIN exif ke ON ke.file_id = km.file_id
                 ORDER BY m.group_id, m.is_suggested_keeper DESC, f.path",
            )
            .map_err(|e| CatalogError::Db(e.to_string()))?;

        let rows = stmt
            .query_map([], |row| {
                let group_id: i64 = row.get(0)?;
                let path_str: String = row.get(1)?;
                let is_keeper: bool = row.get(2)?;
                let date: String = row.get(3)?;
                Ok((group_id, std::path::PathBuf::from(path_str), is_keeper, date))
            })
            .map_err(|e| CatalogError::Db(e.to_string()))?;

        let mut groups: Vec<ReviewGroup> = Vec::new();
        for row in rows {
            let (group_id, path, is_keeper, date) = row.map_err(|e| CatalogError::Db(e.to_string()))?;
            let g = match groups.last_mut() {
                Some(g) if g.group_id == group_id => g,
                _ => {
                    groups.push(ReviewGroup {
                        group_id,
                        date,
                        keeper: None,
                        others: Vec::new(),
                    });
                    groups.last_mut().unwrap()
                }
            };
            if is_keeper && g.keeper.is_none() {
                g.keeper = Some(path);
            } else {
                g.others.push(path);
            }
        }
        Ok(groups)
    }
```

- [ ] **Step 6: Add the test-only duplicate-group insert helper**

Add this method directly after `duplicate_groups_for_review` inside `impl Catalog`. Task 5's integration tests use it to populate `duplicate_groups`/`duplicate_members` on the single shared connection (opening a second `duckdb::Connection` to the same file can deadlock under DuckDB's single-writer model). It uses one transaction and returns the new group id.

```rust
    /// Test-only: insert a duplicate group with `keeper_id` as the suggested
    /// keeper and `others` as the remaining members. Returns the new group id.
    /// Uses the shared connection + one transaction so tests never open a
    /// second connection to the same DB file.
    #[doc(hidden)]
    pub fn test_insert_duplicate_group(
        &self,
        keeper_id: i64,
        others: &[i64],
    ) -> Result<i64, CatalogError> {
        let mut conn = self
            .conn
            .lock()
            .map_err(|_| CatalogError::Db("mutex poisoned".into()))?;
        let tx = conn
            .transaction()
            .map_err(|e| CatalogError::Db(e.to_string()))?;
        tx.execute(
            "INSERT INTO duplicate_groups (method, created_at) VALUES ('test', 0)",
            [],
        )
        .map_err(|e| CatalogError::Db(e.to_string()))?;
        let gid: i64 = tx
            .query_row("SELECT MAX(id) FROM duplicate_groups", [], |r| r.get(0))
            .map_err(|e| CatalogError::Db(e.to_string()))?;
        tx.execute(
            "INSERT INTO duplicate_members (group_id, file_id, is_suggested_keeper, quality_score)
             VALUES (?, ?, TRUE, 1.0)",
            duckdb::params![gid, keeper_id],
        )
        .map_err(|e| CatalogError::Db(e.to_string()))?;
        for o in others {
            tx.execute(
                "INSERT INTO duplicate_members (group_id, file_id, is_suggested_keeper, quality_score)
                 VALUES (?, ?, FALSE, 0.5)",
                duckdb::params![gid, o],
            )
            .map_err(|e| CatalogError::Db(e.to_string()))?;
        }
        tx.commit().map_err(|e| CatalogError::Db(e.to_string()))?;
        Ok(gid)
    }
```

- [ ] **Step 7: Run the test to verify it passes**

Run: `source ~/.cargo/env && cargo test -p pipeline --lib review_entries_and_groups_round_trip 2>&1 | tail -20`
Expected: PASS — `test result: ok. 1 passed`.

- [ ] **Step 8: Lint**

Run: `source ~/.cargo/env && cargo fmt && cargo clippy -p pipeline --all-targets --all-features -- -D warnings 2>&1 | tail -15`
Expected: no warnings/errors. (`test_insert_duplicate_group` is `pub`, so no dead-code warning even though it is consumed only by tests in another crate.)

- [ ] **Step 9: Commit**

```bash
git add crates/pipeline/src/catalog/mod.rs
git commit -m "$(cat <<'EOF'
feat(output): add catalog read API for review tree

review_entries() returns every defect-flagged file with its path,
confidence, and SQL-derived capture year-month; duplicate_groups_for_review()
returns each group's keeper + others dated from the keeper's captured_at.
Date formatting uses strftime(CAST(to_timestamp(...) AS TIMESTAMP), ...)
so no date crate is needed. Adds a #[doc(hidden)] test_insert_duplicate_group
helper so integration tests populate duplicate groups on the shared
connection without a second DuckDB handle.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 2: Output module — path planning (pure, no filesystem)

**Files:**
- Modify: `crates/pipeline/src/output/mod.rs` (replace the placeholder line)

**Interfaces:**
- Consumes: `ReviewEntry`, `ReviewGroup` from `crate::catalog` (Task 1).
- Produces (relied on by Tasks 3 & 4):
  ```rust
  /// A single link the tree should contain: relative directory under the
  /// output root + the original it points at.
  pub(crate) struct PlannedLink {
      pub rel_dir: std::path::PathBuf, // e.g. "rejected/blur/2023-06"
      pub original: std::path::PathBuf, // absolute path to the original photo
  }
  pub(crate) fn substitute_library(template: &str, scan_root: &std::path::Path) -> std::path::PathBuf;
  pub(crate) fn category_of(flag_type: &str, confidence: f32) -> Option<(&'static str, String)>;
  pub(crate) fn plan_links(
      entries: &[crate::catalog::ReviewEntry],
      groups: &[crate::catalog::ReviewGroup],
      include: &[String],
  ) -> Vec<PlannedLink>;
  pub(crate) fn dedupe_name(taken: &mut std::collections::HashSet<String>, basename: &str) -> String;
  ```
  `category_of` returns `(top_level, rel_dir_under_root)` — e.g. `("rejected", "rejected/blur/2023-06")` requires the caller to pass year-month, so instead it returns the **subpath fragment relative to the top-level dir**. To keep it simple and testable, `category_of(flag_type, confidence)` returns `Some((top_level, subfolder))` where `subfolder` is `""` for uncertain and the flag's rejected-subfolder name otherwise; `plan_links` joins the year-month. See Step 3 for the exact contract.

- [ ] **Step 1: Write the failing unit tests**

Replace the entire contents of `crates/pipeline/src/output/mod.rs` placeholder is fine to overwrite later; for now ADD a `#[cfg(test)] mod tests` at the end. But since the file is a 1-line placeholder, write the test module first as the failing target. Put this at the bottom of the file (the items it calls are added in Step 3):

```rust
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
        assert_eq!(category_of("low_iqa", 0.9), Some(("rejected", "low_quality")));
        assert_eq!(category_of("overexposed", 0.7), Some(("rejected", "overexposed")));
        assert_eq!(category_of("blur", 0.4), Some(("uncertain", "")));
        assert_eq!(category_of("low_iqa", 0.59), Some(("uncertain", "")));
        assert_eq!(category_of("nonsense", 0.9), None);
    }

    #[test]
    fn plan_links_buckets_correctly() {
        let entries = vec![
            ReviewEntry { file_id: 1, path: PathBuf::from("/lib/a.jpg"), flag_type: "blur".into(), confidence: 0.8, year_month: "2023-06".into() },
            ReviewEntry { file_id: 2, path: PathBuf::from("/lib/b.jpg"), flag_type: "low_iqa".into(), confidence: 0.9, year_month: "2023-06".into() },
            ReviewEntry { file_id: 3, path: PathBuf::from("/lib/c.jpg"), flag_type: "blur".into(), confidence: 0.4, year_month: "unknown-date".into() },
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
        assert!(dirs.iter().filter(|d| **d == "duplicates/group_00042_2023-06-15/_others").count() == 2);
    }

    #[test]
    fn plan_links_include_filter() {
        let entries = vec![ReviewEntry {
            file_id: 1, path: PathBuf::from("/lib/a.jpg"), flag_type: "blur".into(),
            confidence: 0.8, year_month: "2023-06".into(),
        }];
        let groups = vec![ReviewGroup {
            group_id: 1, date: "2023-06-15".into(),
            keeper: Some(PathBuf::from("/lib/k.jpg")), others: vec![],
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
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `source ~/.cargo/env && cargo test -p pipeline --lib output::tests 2>&1 | tail -20`
Expected: FAIL — compile errors `cannot find function substitute_library` etc.

- [ ] **Step 3: Implement the pure planning functions**

Put this ABOVE the `#[cfg(test)] mod tests` block, replacing the placeholder comment at the top of `crates/pipeline/src/output/mod.rs`:

```rust
//! Phase 6 — build the symlink/hardlink review tree from catalog data.

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
pub(crate) fn category_of(flag_type: &str, confidence: f32) -> Option<(&'static str, &'static str)> {
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
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `source ~/.cargo/env && cargo test -p pipeline --lib output::tests 2>&1 | tail -20`
Expected: PASS — `5 passed`.

- [ ] **Step 5: Lint**

Run: `source ~/.cargo/env && cargo fmt && cargo clippy -p pipeline --all-targets --all-features -- -D warnings 2>&1 | tail -15`
Expected: no warnings. (`PlannedLink`, `plan_links`, etc. are `pub(crate)` and used by tests, so no dead-code warning.)

- [ ] **Step 6: Commit**

```bash
git add crates/pipeline/src/output/mod.rs
git commit -m "$(cat <<'EOF'
feat(output): pure path-planning for the review tree

substitute_library() resolves the <library> token; category_of() maps a
flag_type+confidence to rejected/<sub> or uncertain; plan_links() expands
entries+groups into relative link dirs honoring the --include filter;
dedupe_name() makes basenames collision-free with " (N)" suffixes.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 3: Output module — filesystem materialization + README + guarded delete

**Files:**
- Modify: `crates/pipeline/src/output/mod.rs` (add the public report struct, the link primitives, README writer, guarded delete, and the public `build_review_tree` entry point)
- Modify: `crates/pipeline/src/lib.rs` (add the re-export)

**Interfaces:**
- Consumes: `PlannedLink`, `plan_links`, `dedupe_name` (Task 2); `Catalog`, `ReviewEntry`, `ReviewGroup` (Task 1); `crate::config::{OutputConfig, LinkType}`.
- Produces (relied on by Task 4 — the CLI):
  ```rust
  pub struct ReviewTreeReport {
      pub links_created: u64,
      pub links_removed: u64,
      pub groups: u64,
      pub errors: u64,
  }
  pub fn build_review_tree(
      catalog: &crate::catalog::Catalog,
      output_root: &std::path::Path,
      cfg: &crate::config::OutputConfig,
      include: &[String],
      regenerate: bool,
  ) -> anyhow::Result<ReviewTreeReport>;
  ```
  And in `lib.rs`: `pub use output::{build_review_tree, ReviewTreeReport};`

This task adds real I/O, so its tests live in the integration test file (Task 5). Here we implement and rely on Task 5's tests to validate; to keep this task independently testable we add one focused unit test for the guarded-delete safety check (which needs no catalog).

- [ ] **Step 1: Write the failing safety unit test**

Add inside the existing `#[cfg(test)] mod tests` block in `crates/pipeline/src/output/mod.rs` (it already imports `std::path` items; add `use tempfile` is not available in unit tests of the lib — `tempfile` is a dev-dependency and IS available under `#[cfg(test)]`). Add:

```rust
    #[test]
    fn remove_managed_tree_refuses_foreign_regular_file() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path().join("_review");
        std::fs::create_dir_all(root.join("rejected/blur/2023-06")).unwrap();
        // A foreign regular file the tool did not create.
        std::fs::write(root.join("rejected/blur/2023-06/notes.txt"), b"hi").unwrap();

        let err = remove_managed_tree(&root).unwrap_err();
        assert!(
            err.to_string().contains("non-symlink regular file"),
            "unexpected error: {err}"
        );
        // The foreign file must still exist.
        assert!(root.join("rejected/blur/2023-06/notes.txt").exists());
    }

    #[test]
    fn remove_managed_tree_deletes_symlinks_and_dirs() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path().join("_review");
        let target = dir.path().join("orig.jpg");
        std::fs::write(&target, b"jpeg").unwrap();
        std::fs::create_dir_all(root.join("rejected/blur/2023-06")).unwrap();
        std::os::unix::fs::symlink(&target, root.join("rejected/blur/2023-06/orig.jpg")).unwrap();
        std::fs::write(root.join("README.txt"), b"readme").unwrap();

        remove_managed_tree(&root).unwrap();
        assert!(!root.exists(), "tree root should be gone");
        assert!(target.exists(), "original must be untouched");
    }
```

Note: `README.txt` is a regular file the tool itself creates; `remove_managed_tree` must treat a top-level `README.txt` as managed (allowed to delete) but any *other* regular file as foreign. See Step 3.

- [ ] **Step 2: Run to verify it fails**

Run: `source ~/.cargo/env && cargo test -p pipeline --lib output::tests::remove_managed 2>&1 | tail -20`
Expected: FAIL — `cannot find function remove_managed_tree`.

- [ ] **Step 3: Implement the report struct, link primitives, README, and guarded delete**

Add these items ABOVE the `#[cfg(test)] mod tests` block (after the Task 2 functions) in `crates/pipeline/src/output/mod.rs`:

```rust
use crate::config::{LinkType, OutputConfig};

/// Summary of a review-tree build.
#[derive(Debug, Default, Clone)]
pub struct ReviewTreeReport {
    pub links_created: u64,
    pub links_removed: u64,
    pub groups: u64,
    pub errors: u64,
}

/// The single regular file the tool itself writes at the tree root.
const README_NAME: &str = "README.txt";

/// Create one link (symlink or hardlink) at `link_path` pointing to the
/// absolute `original`. Idempotent: if a correct symlink already exists, it
/// is left as-is. Hardlink across filesystems fails loudly (EXDEV).
/// Returns `true` if a new link was created.
fn create_link(link_path: &Path, original: &Path, link_type: LinkType) -> anyhow::Result<bool> {
    use anyhow::Context;

    let abs_original = original
        .canonicalize()
        .with_context(|| format!("original not found: {}", original.display()))?;

    match link_type {
        LinkType::Symlink => {
            if let Ok(existing) = std::fs::read_link(link_path) {
                if existing == abs_original {
                    return Ok(false); // already correct
                }
                std::fs::remove_file(link_path).with_context(|| {
                    format!("replacing stale symlink {}", link_path.display())
                })?;
            }
            std::os::unix::fs::symlink(&abs_original, link_path).with_context(|| {
                format!(
                    "symlink {} -> {}",
                    link_path.display(),
                    abs_original.display()
                )
            })?;
            Ok(true)
        }
        LinkType::Hardlink => {
            if link_path.exists() {
                // A regular file already present; assume it's our hardlink.
                return Ok(false);
            }
            std::fs::hard_link(&abs_original, link_path).with_context(|| {
                format!(
                    "hardlink {} -> {} (cross-filesystem hardlinks are not supported)",
                    link_path.display(),
                    abs_original.display()
                )
            })?;
            Ok(true)
        }
    }
}

/// Write the autogenerated README.txt at the tree root.
fn write_readme(output_root: &Path, db_path_hint: &str) -> anyhow::Result<()> {
    let body = format!(
        "PhotoPipe Review Tree\n\
=====================\n\
\n\
This directory was generated by photopipe.\n\
DO NOT delete files in your photo library; this tree only contains links.\n\
\n\
What to review:\n\
\n\
  rejected/blur/         — photos flagged as out-of-focus or blurry.\n\
                           Spot-check; if any are actually fine, see \"Overriding\".\n\
  rejected/back_focus/   — photos where focus landed on background, not subject.\n\
  rejected/overexposed/  — highlights blown beyond likely recovery.\n\
  rejected/underexposed/ — shadows crushed.\n\
  rejected/low_quality/  — low overall image-quality score.\n\
  duplicates/group_NN/   — burst shots or near-identical scenes.\n\
                           `_keeper/` contains photopipe's suggested best.\n\
                           Open the whole group folder, pick what to keep.\n\
  uncertain/             — low-confidence flags. Worth a second look.\n\
\n\
Overriding:\n\
  The catalog at {db} is the source of truth. Deleting a link here\n\
  has NO effect on flags. To override:\n\
    photopipe override <file> --remove-flag blur\n\
  (forthcoming command)\n\
\n\
To delete the originals you've decided to reject:\n\
  photopipe commit-rejects --confirm   (forthcoming command)\n",
        db = db_path_hint,
    );
    std::fs::write(output_root.join(README_NAME), body)
        .map_err(|e| anyhow::anyhow!("write README.txt: {e}"))
}

/// Recursively delete a tree the tool manages. Refuses (errors, deletes
/// nothing) if it encounters any regular file that is NOT a managed
/// `README.txt` at the root — protecting against pointing the output at a
/// real directory. Symlinks and directories are removed.
fn remove_managed_tree(output_root: &Path) -> anyhow::Result<()> {
    if !output_root.exists() {
        return Ok(());
    }
    // First pass: verify there are no foreign regular files anywhere.
    check_no_foreign_files(output_root, output_root)?;
    // Safe to delete the whole subtree.
    std::fs::remove_dir_all(output_root)
        .map_err(|e| anyhow::anyhow!("remove tree {}: {e}", output_root.display()))
}

/// Walk `dir`; error if any entry is a regular (non-symlink) file other than
/// the root-level README.txt.
fn check_no_foreign_files(root: &Path, dir: &Path) -> anyhow::Result<()> {
    for entry in std::fs::read_dir(dir)
        .map_err(|e| anyhow::anyhow!("read_dir {}: {e}", dir.display()))?
    {
        let entry = entry.map_err(|e| anyhow::anyhow!("dir entry: {e}"))?;
        let path = entry.path();
        // symlink_metadata does NOT follow symlinks.
        let meta = std::fs::symlink_metadata(&path)
            .map_err(|e| anyhow::anyhow!("stat {}: {e}", path.display()))?;
        let ft = meta.file_type();
        if ft.is_symlink() {
            continue; // managed link
        }
        if ft.is_dir() {
            check_no_foreign_files(root, &path)?;
            continue;
        }
        // Regular file: only a root-level README.txt is allowed.
        let is_root_readme = path.parent() == Some(root)
            && path.file_name().and_then(|n| n.to_str()) == Some(README_NAME);
        if !is_root_readme {
            anyhow::bail!(
                "refusing to delete {}: contains a non-symlink regular file not created by photopipe",
                root.display()
            );
        }
    }
    Ok(())
}
```

- [ ] **Step 4: Implement `build_review_tree`**

Add this `pub fn` after the helpers above (still before the test module). It is the only `pub` entry point besides the report struct.

```rust
/// Build (or incrementally update) the review tree at `output_root`.
///
/// `regenerate = true` deletes the managed tree first, then rebuilds.
/// `regenerate = false` ensures every expected link exists and prunes any
/// managed symlink whose target is no longer expected (incremental).
/// Non-destructive: only links/dirs inside `output_root` are ever touched.
pub fn build_review_tree(
    catalog: &crate::catalog::Catalog,
    output_root: &Path,
    cfg: &OutputConfig,
    include: &[String],
    regenerate: bool,
) -> anyhow::Result<ReviewTreeReport> {
    use std::collections::{HashMap, HashSet};

    let entries = catalog.review_entries()?;
    let groups = catalog.duplicate_groups_for_review()?;
    let planned = plan_links(&entries, &groups, include);

    let group_count = groups
        .iter()
        .filter(|_| include.is_empty() || include.iter().any(|c| c == "duplicates"))
        .count() as u64;

    let mut report = ReviewTreeReport {
        groups: group_count,
        ..Default::default()
    };

    if regenerate {
        remove_managed_tree(output_root)?;
        tracing::info!(root = %output_root.display(), "regenerate: removed existing tree");
    }

    std::fs::create_dir_all(output_root)
        .map_err(|e| anyhow::anyhow!("create output root {}: {e}", output_root.display()))?;

    // Resolve final link paths, dedeuping basenames per directory.
    // expected: set of absolute link paths we intend to have on disk.
    let mut expected: HashSet<PathBuf> = HashSet::new();
    let mut taken_per_dir: HashMap<PathBuf, HashSet<String>> = HashMap::new();

    for link in &planned {
        let abs_dir = output_root.join(&link.rel_dir);
        if let Err(e) = std::fs::create_dir_all(&abs_dir) {
            tracing::warn!(dir = %abs_dir.display(), error = %e, "create dir failed");
            report.errors += 1;
            continue;
        }
        let basename = link
            .original
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unnamed")
            .to_string();
        let taken = taken_per_dir.entry(abs_dir.clone()).or_default();
        let name = dedupe_name(taken, &basename);
        let link_path = abs_dir.join(&name);

        match create_link(&link_path, &link.original, cfg.link_type) {
            Ok(true) => report.links_created += 1,
            Ok(false) => {}
            Err(e) => {
                tracing::warn!(link = %link_path.display(), error = %e, "create link failed");
                report.errors += 1;
                continue;
            }
        }
        expected.insert(link_path);
    }

    // Incremental prune: remove managed symlinks no longer expected.
    if !regenerate {
        prune_stale_links(output_root, &expected, &mut report)?;
    }

    // Always (re)write the README at the root.
    let db_hint = "the photopipe catalog".to_string();
    write_readme(output_root, &db_hint)?;

    tracing::info!(
        created = report.links_created,
        removed = report.links_removed,
        groups = report.groups,
        errors = report.errors,
        "review tree built"
    );
    Ok(report)
}

/// Walk the tree; remove any symlink whose absolute path is not in `expected`.
/// Then remove now-empty directories. Never removes regular files.
fn prune_stale_links(
    output_root: &Path,
    expected: &std::collections::HashSet<PathBuf>,
    report: &mut ReviewTreeReport,
) -> anyhow::Result<()> {
    prune_dir(output_root, output_root, expected, report)
}

fn prune_dir(
    root: &Path,
    dir: &Path,
    expected: &std::collections::HashSet<PathBuf>,
    report: &mut ReviewTreeReport,
) -> anyhow::Result<()> {
    let read = match std::fs::read_dir(dir) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(dir = %dir.display(), error = %e, "prune read_dir failed");
            report.errors += 1;
            return Ok(());
        }
    };
    for entry in read {
        let entry = entry.map_err(|e| anyhow::anyhow!("dir entry: {e}"))?;
        let path = entry.path();
        let meta = std::fs::symlink_metadata(&path)
            .map_err(|e| anyhow::anyhow!("stat {}: {e}", path.display()))?;
        let ft = meta.file_type();
        if ft.is_symlink() {
            if !expected.contains(&path) {
                if let Err(e) = std::fs::remove_file(&path) {
                    tracing::warn!(link = %path.display(), error = %e, "prune remove failed");
                    report.errors += 1;
                } else {
                    report.links_removed += 1;
                }
            }
        } else if ft.is_dir() {
            prune_dir(root, &path, expected, report)?;
            // Remove the directory if it is now empty.
            if let Ok(mut it) = std::fs::read_dir(&path) {
                if it.next().is_none() {
                    let _ = std::fs::remove_dir(&path);
                }
            }
        }
        // Regular files (e.g. root README.txt) are left untouched here.
    }
    Ok(())
}
```

- [ ] **Step 5: Re-export from lib.rs**

In `crates/pipeline/src/lib.rs`, add to the existing `pub use` lines (next to `pub use defect::analyze_defects;`):

```rust
pub use output::{build_review_tree, ReviewTreeReport};
```

- [ ] **Step 6: Run the safety unit tests**

Run: `source ~/.cargo/env && cargo test -p pipeline --lib output::tests 2>&1 | tail -20`
Expected: PASS — all `output::tests` (7 total now) pass.

- [ ] **Step 7: Lint**

Run: `source ~/.cargo/env && cargo fmt && cargo clippy -p pipeline --all-targets --all-features -- -D warnings 2>&1 | tail -15`
Expected: no warnings.

- [ ] **Step 8: Commit**

```bash
git add crates/pipeline/src/output/mod.rs crates/pipeline/src/lib.rs
git commit -m "$(cat <<'EOF'
feat(output): materialize review tree with guarded, idempotent I/O

build_review_tree() creates symlinks/hardlinks under the output root,
dedupes basenames per folder, writes README.txt, and either regenerates
(guarded delete) or incrementally prunes stale managed links. The delete
guard refuses to touch any foreign non-symlink regular file. Re-export
build_review_tree + ReviewTreeReport from lib.rs.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 4: Wire the CLI handler

**Files:**
- Modify: `crates/cli/src/main.rs:189-198` (replace the `cmd_review_tree` stub body)

**Interfaces:**
- Consumes: `pipeline::build_review_tree`, `pipeline::catalog::Catalog`, `pipeline::config::Config` (already imported as `config`), the existing `Command::ReviewTree { output, include, regenerate }`.
- Produces: user-facing CLI behavior — no downstream Rust consumers.

Resolution of the output root: the **positional `<OUTPUT>` arg always wins** as the destination root. `cfg.output.review_tree` (with its `<library>` token) is only a fallback default for callers/config, not consulted here because the CLI requires the arg. We still pass `cfg.output` into `build_review_tree` because it carries `link_type`. (The `<library>` substitution helper from Task 2 is exercised by unit tests and remains available for a future config-driven default; document this so the reviewer knows it is intentional that the CLI does not substitute.)

- [ ] **Step 1: Replace the stub**

Replace the entire `cmd_review_tree` function (lines 189-198) in `crates/cli/src/main.rs` with:

```rust
fn cmd_review_tree(
    output: PathBuf,
    include: Vec<String>,
    regenerate: bool,
    cfg: &config::Config,
) -> Result<()> {
    use pipeline::{build_review_tree, catalog::Catalog};

    let catalog =
        Catalog::open(&cfg.catalog.db_path).map_err(|e| anyhow::anyhow!("catalog: {}", e))?;

    tracing::info!(output = %output.display(), regenerate, "building review tree");
    let report = build_review_tree(&catalog, &output, &cfg.output, &include, regenerate)?;

    println!("Review tree: {}", output.display());
    println!("  Links created : {}", report.links_created);
    println!("  Links removed : {}", report.links_removed);
    println!("  Groups        : {}", report.groups);
    println!("  Errors        : {}", report.errors);
    Ok(())
}
```

- [ ] **Step 2: Build the CLI**

Run: `source ~/.cargo/env && cargo build -p cli 2>&1 | tail -15`
Expected: compiles cleanly. (`output`, `include`, `regenerate` were previously underscore-prefixed; the new signature uses them, so no unused-variable warnings.)

- [ ] **Step 3: Smoke-run against an empty catalog**

Run:
```bash
source ~/.cargo/env
TMP=$(mktemp -d)
./target/debug/photopipe --config "$TMP/none.toml" review-tree "$TMP/_review" 2>&1 | tail -8
ls "$TMP/_review"
```
Expected: prints `Review tree: …`, all counters `0`, and `ls` shows `README.txt`. (The default config points the catalog at the XDG data dir; if that DB has no rows, counters are 0. If it errors because no catalog exists, that is acceptable for the smoke test — the integration tests in Task 5 are the real verification.)

- [ ] **Step 4: Lint**

Run: `source ~/.cargo/env && cargo fmt && cargo clippy -p cli --all-targets --all-features -- -D warnings 2>&1 | tail -15`
Expected: no warnings.

- [ ] **Step 5: Commit**

```bash
git add crates/cli/src/main.rs
git commit -m "$(cat <<'EOF'
feat(output): wire photopipe review-tree CLI handler

cmd_review_tree opens the catalog, calls build_review_tree with the
positional OUTPUT root (which wins over the config default), and prints
the link/group/error counts.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 5: Integration tests (synthetic library, no real photos)

**Files:**
- Create: `crates/pipeline/tests/review_tree.rs`

**Interfaces:**
- Consumes: `pipeline::{build_review_tree, ReviewTreeReport}`, `pipeline::catalog::Catalog`, `pipeline::config::{OutputConfig, LinkType, KeeperStrategy}`, `pipeline::ingest::{IngestedFile, ExifData, FileFormat}`, `pipeline::defect::DefectFlag`. Each test sets up catalog rows directly (no model inference needed). This file defines its own helpers (test files in this repo do not share helpers).
- Produces: nothing consumed downstream.

- [ ] **Step 1: Write the integration test file**

Create `crates/pipeline/tests/review_tree.rs` with the content below (the first test + shared helpers). Every test creates real synthetic JPEGs on disk (so `canonicalize` works), inserts `files`/`exif`/`defect_flags` rows directly, populates duplicate groups via `catalog.test_insert_duplicate_group` (added in Task 1, Step 6), builds the tree, and asserts on the filesystem. Group-folder assertions compute the name from the returned `gid` rather than hardcoding it.

```rust
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use image::{ImageBuffer, Rgb};
use pipeline::{
    build_review_tree,
    catalog::Catalog,
    config::{KeeperStrategy, LinkType, OutputConfig},
    defect::DefectFlag,
    ingest::{ExifData, FileFormat, IngestedFile},
};
use tempfile::TempDir;

// ── helpers (this test file is self-contained) ──────────────────────────────

fn make_synthetic_jpg(path: &Path, r: u8, g: u8, b: u8) {
    let img: ImageBuffer<Rgb<u8>, Vec<u8>> = ImageBuffer::from_fn(32, 32, |_, _| Rgb([r, g, b]));
    img.save(path).expect("save test jpg");
}

fn out_cfg(link_type: LinkType) -> OutputConfig {
    OutputConfig {
        review_tree: "<library>/_review".into(),
        link_type,
        keeper_strategy: KeeperStrategy::Iqa,
    }
}

/// Insert a file with a synthetic JPEG on disk; return its file_id.
/// `captured_at` of None -> NULL exif (unknown-date).
fn add_file(
    catalog: &Catalog,
    lib: &Path,
    name: &str,
    hash: u128,
    captured_at: Option<i64>,
) -> (i64, PathBuf) {
    let path = lib.join(name);
    make_synthetic_jpg(&path, (hash & 0xff) as u8, 0, 0);
    let file = IngestedFile {
        path: path.clone(),
        content_hash: hash,
        size: 100,
        mtime_ns: 1,
        format: FileFormat::Jpg,
        has_sidecar_jpg: false,
    };
    let exif = captured_at.map(|c| ExifData {
        captured_at: Some(c),
        ..Default::default()
    });
    let ids = catalog.flush_batch(&[(file, exif)]).unwrap();
    (ids[0], path)
}

fn count_originals(lib: &Path) -> usize {
    fs::read_dir(lib)
        .unwrap()
        .filter(|e| {
            let p = e.as_ref().unwrap().path();
            p.is_file() && p.extension().map(|x| x == "jpg").unwrap_or(false)
        })
        .count()
}

const JUNE_2023: i64 = 1_686_830_400; // 2023-06-15

// ── tests ───────────────────────────────────────────────────────────────────

#[test]
fn rejected_and_uncertain_and_duplicates_are_built() {
    let lib = TempDir::new().unwrap();
    let db = TempDir::new().unwrap();
    let catalog = Catalog::open(&db.path().join("c.duckdb")).unwrap();

    let (blur_id, blur_path) = add_file(&catalog, lib.path(), "blur.jpg", 1, Some(JUNE_2023));
    let (lowq_id, _) = add_file(&catalog, lib.path(), "lowq.jpg", 2, Some(JUNE_2023));
    let (unc_id, _) = add_file(&catalog, lib.path(), "uncertain.jpg", 3, Some(JUNE_2023));

    catalog
        .upsert_defect_flag(
            blur_id,
            &DefectFlag { flag_type: "blur".into(), confidence: 0.8, reason: "r".into() },
        )
        .unwrap();
    catalog
        .upsert_defect_flag(
            lowq_id,
            &DefectFlag { flag_type: "low_iqa".into(), confidence: 0.9, reason: "r".into() },
        )
        .unwrap();
    catalog
        .upsert_defect_flag(
            unc_id,
            &DefectFlag { flag_type: "blur".into(), confidence: 0.4, reason: "r".into() },
        )
        .unwrap();

    // Duplicate group: keeper + one other.
    let (keeper_id, keeper_path) = add_file(&catalog, lib.path(), "keeper.jpg", 4, Some(JUNE_2023));
    let (other_id, _) = add_file(&catalog, lib.path(), "other.jpg", 5, Some(JUNE_2023));
    let gid = catalog
        .test_insert_duplicate_group(keeper_id, &[other_id])
        .unwrap();
    let group_dir = format!("duplicates/group_{gid:05}_2023-06-15");

    let out = lib.path().join("_review");
    let report =
        build_review_tree(&catalog, &out, &out_cfg(LinkType::Symlink), &[], false).unwrap();
    assert!(report.links_created >= 4, "created {}", report.links_created);
    assert_eq!(report.groups, 1);

    // rejected/blur/2023-06/blur.jpg is a symlink resolving to the original.
    let blur_link = out.join("rejected/blur/2023-06/blur.jpg");
    assert!(fs::symlink_metadata(&blur_link).unwrap().file_type().is_symlink());
    assert_eq!(fs::canonicalize(&blur_link).unwrap(), fs::canonicalize(&blur_path).unwrap());

    // low_iqa -> low_quality.
    assert!(out.join("rejected/low_quality/2023-06/lowq.jpg").exists());

    // low-confidence blur -> uncertain.
    assert!(out.join("uncertain/2023-06/uncertain.jpg").exists());

    // duplicates keeper + others.
    let kdir = out.join(&group_dir).join("_keeper");
    assert!(kdir.join("keeper.jpg").exists());
    assert_eq!(
        fs::canonicalize(kdir.join("keeper.jpg")).unwrap(),
        fs::canonicalize(&keeper_path).unwrap()
    );
    assert!(out.join(&group_dir).join("_others/other.jpg").exists());

    // README present.
    assert!(out.join("README.txt").exists());
}
```

- [ ] **Step 2: Run the first test to verify it passes**

Run: `source ~/.cargo/env && cargo test -p pipeline --test review_tree rejected_and_uncertain 2>&1 | tail -20`
Expected: PASS. (This depends on `build_review_tree`, the catalog read API, and `test_insert_duplicate_group` from earlier tasks all being in place.)

- [ ] **Step 3: Append the remaining integration tests**

Add these tests to `crates/pipeline/tests/review_tree.rs`:

```rust
#[test]
fn non_destructive_originals_unchanged() {
    let lib = TempDir::new().unwrap();
    let db = TempDir::new().unwrap();
    let catalog = Catalog::open(&db.path().join("c.duckdb")).unwrap();

    let (id, orig) = add_file(&catalog, lib.path(), "a.jpg", 1, Some(JUNE_2023));
    catalog
        .upsert_defect_flag(
            id,
            &DefectFlag { flag_type: "blur".into(), confidence: 0.8, reason: "r".into() },
        )
        .unwrap();

    let before = count_originals(lib.path());
    let out = lib.path().join("_review");
    build_review_tree(&catalog, &out, &out_cfg(LinkType::Symlink), &[], false).unwrap();
    // _review is a subdir of lib; count only the top-level .jpg originals (none moved).
    assert_eq!(count_originals(lib.path()), before);

    // Delete a symlink -> original survives.
    let link = out.join("rejected/blur/2023-06/a.jpg");
    fs::remove_file(&link).unwrap();
    assert!(orig.exists(), "deleting a symlink must not delete the original");
}

#[test]
fn regenerate_rebuilds_after_manual_deletion() {
    let lib = TempDir::new().unwrap();
    let db = TempDir::new().unwrap();
    let catalog = Catalog::open(&db.path().join("c.duckdb")).unwrap();

    for i in 0..4i64 {
        let (id, _) = add_file(&catalog, lib.path(), &format!("f{i}.jpg"), i as u128 + 1, Some(JUNE_2023));
        catalog
            .upsert_defect_flag(
                id,
                &DefectFlag { flag_type: "blur".into(), confidence: 0.8, reason: "r".into() },
            )
            .unwrap();
    }
    let out = lib.path().join("_review");
    let r1 = build_review_tree(&catalog, &out, &out_cfg(LinkType::Symlink), &[], false).unwrap();
    assert_eq!(r1.links_created, 4);

    // Delete half the links by removing the whole month dir.
    fs::remove_dir_all(out.join("rejected/blur/2023-06")).unwrap();

    // --regenerate rebuilds all.
    let r2 = build_review_tree(&catalog, &out, &out_cfg(LinkType::Symlink), &[], true).unwrap();
    assert_eq!(r2.links_created, 4, "regenerate should recreate all 4 links");
    let n = fs::read_dir(out.join("rejected/blur/2023-06")).unwrap().count();
    assert_eq!(n, 4);
}

#[test]
fn incremental_prunes_stale_links() {
    let lib = TempDir::new().unwrap();
    let db = TempDir::new().unwrap();
    let catalog = Catalog::open(&db.path().join("c.duckdb")).unwrap();

    let (id, _) = add_file(&catalog, lib.path(), "a.jpg", 1, Some(JUNE_2023));
    catalog
        .upsert_defect_flag(
            id,
            &DefectFlag { flag_type: "blur".into(), confidence: 0.8, reason: "r".into() },
        )
        .unwrap();
    let out = lib.path().join("_review");
    build_review_tree(&catalog, &out, &out_cfg(LinkType::Symlink), &[], false).unwrap();

    // Plant a stale symlink the planner would never produce.
    let stale_dir = out.join("rejected/blur/2099-01");
    fs::create_dir_all(&stale_dir).unwrap();
    std::os::unix::fs::symlink(lib.path().join("a.jpg"), stale_dir.join("ghost.jpg")).unwrap();

    let r = build_review_tree(&catalog, &out, &out_cfg(LinkType::Symlink), &[], false).unwrap();
    assert_eq!(r.links_removed, 1, "stale link should be pruned");
    assert!(!stale_dir.join("ghost.jpg").exists());
}

#[test]
fn basename_collisions_get_distinct_links() {
    let lib = TempDir::new().unwrap();
    let sub = lib.path().join("subdir");
    fs::create_dir_all(&sub).unwrap();
    let db = TempDir::new().unwrap();
    let catalog = Catalog::open(&db.path().join("c.duckdb")).unwrap();

    // Two different originals with the SAME basename, same month/category.
    let p1 = lib.path().join("IMG_1.jpg");
    make_synthetic_jpg(&p1, 10, 0, 0);
    let p2 = sub.join("IMG_1.jpg");
    make_synthetic_jpg(&p2, 20, 0, 0);
    let id1 = catalog
        .flush_batch(&[(
            IngestedFile { path: p1.clone(), content_hash: 1, size: 1, mtime_ns: 1, format: FileFormat::Jpg, has_sidecar_jpg: false },
            Some(ExifData { captured_at: Some(JUNE_2023), ..Default::default() }),
        )])
        .unwrap()[0];
    let id2 = catalog
        .flush_batch(&[(
            IngestedFile { path: p2.clone(), content_hash: 2, size: 1, mtime_ns: 1, format: FileFormat::Jpg, has_sidecar_jpg: false },
            Some(ExifData { captured_at: Some(JUNE_2023), ..Default::default() }),
        )])
        .unwrap()[0];
    for id in [id1, id2] {
        catalog
            .upsert_defect_flag(
                id,
                &DefectFlag { flag_type: "blur".into(), confidence: 0.8, reason: "r".into() },
            )
            .unwrap();
    }

    let out = lib.path().join("_review");
    build_review_tree(&catalog, &out, &out_cfg(LinkType::Symlink), &[], false).unwrap();

    let dir = out.join("rejected/blur/2023-06");
    let names: HashSet<String> = fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(names.contains("IMG_1.jpg"));
    assert!(names.contains("IMG_1 (2).jpg"), "got {names:?}");
}

#[test]
fn include_filter_limits_categories() {
    let lib = TempDir::new().unwrap();
    let db = TempDir::new().unwrap();
    let catalog = Catalog::open(&db.path().join("c.duckdb")).unwrap();

    let (id, _) = add_file(&catalog, lib.path(), "a.jpg", 1, Some(JUNE_2023));
    catalog
        .upsert_defect_flag(
            id,
            &DefectFlag { flag_type: "blur".into(), confidence: 0.8, reason: "r".into() },
        )
        .unwrap();
    let (kid, _) = add_file(&catalog, lib.path(), "k.jpg", 2, Some(JUNE_2023));
    let (oid, _) = add_file(&catalog, lib.path(), "o.jpg", 3, Some(JUNE_2023));
    let gid = catalog.test_insert_duplicate_group(kid, &[oid]).unwrap();

    let out = lib.path().join("_review");
    build_review_tree(
        &catalog,
        &out,
        &out_cfg(LinkType::Symlink),
        &["duplicates".to_string()],
        false,
    )
    .unwrap();

    assert!(!out.join("rejected").exists(), "rejected excluded by filter");
    assert!(out
        .join(format!("duplicates/group_{gid:05}_2023-06-15/_keeper/k.jpg"))
        .exists());
}

#[test]
fn unknown_date_file_goes_to_unknown_date_folder() {
    let lib = TempDir::new().unwrap();
    let db = TempDir::new().unwrap();
    let catalog = Catalog::open(&db.path().join("c.duckdb")).unwrap();

    let (id, _) = add_file(&catalog, lib.path(), "nodate.jpg", 1, None);
    catalog
        .upsert_defect_flag(
            id,
            &DefectFlag { flag_type: "blur".into(), confidence: 0.8, reason: "r".into() },
        )
        .unwrap();
    let out = lib.path().join("_review");
    build_review_tree(&catalog, &out, &out_cfg(LinkType::Symlink), &[], false).unwrap();
    assert!(out.join("rejected/blur/unknown-date/nodate.jpg").exists());
}
```

- [ ] **Step 4: Run the full integration suite to verify it passes**

Run: `source ~/.cargo/env && cargo test -p pipeline --test review_tree 2>&1 | tail -30`
Expected: all tests PASS (`8 passed` — the 8 `#[test]` functions in this file).

- [ ] **Step 5: Full workspace verification**

Run:
```bash
source ~/.cargo/env
cargo fmt --check && \
cargo clippy --all-targets --all-features -- -D warnings && \
cargo test --all 2>&1 | tail -30
```
Expected: fmt clean, no clippy warnings, all tests pass (existing + new).

- [ ] **Step 6: Commit**

```bash
git add crates/pipeline/tests/review_tree.rs crates/pipeline/src/catalog/mod.rs
git commit -m "$(cat <<'EOF'
test(output): integration tests for the review tree

Covers rejected/uncertain/duplicates layout, symlinks resolving to
originals, non-destructiveness (deleting a link leaves the original),
--regenerate rebuild, incremental stale-link prune, basename-collision
dedupe, --include filtering, and unknown-date bucketing. Adds a
#[doc(hidden)] Catalog::test_insert_duplicate_group helper to keep tests
on the single shared connection.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Self-Review (run after writing all tasks)

**1. Spec coverage (IMPLEMENTATION_PLAN §8 Phase 6):**
- Tree layout `rejected/{blur,back_focus,overexposed,underexposed,low_quality}/<YYYY-MM>/` → Task 2 `category_of` + `plan_links`; Task 5 asserts.
- `duplicates/group_<NNNNN>_<YYYY-MM-DD>/{_keeper,_others}/` → Task 2 `plan_links`; Task 5 asserts.
- `uncertain/<YYYY-MM>/` for confidence < 0.6 → Task 2 threshold 0.6; Task 5 asserts.
- README.txt autogenerated → Task 3 `write_readme`; Task 5 asserts existence.
- `<library>` substitution → Task 2 `substitute_library` (unit-tested); CLI uses positional arg by design (documented in Task 4).
- Absolute-target symlinks → Task 3 `create_link` canonicalizes original; Task 5 asserts `canonicalize` equality.
- Hardlink mode + cross-fs failure → Task 3 `create_link` `LinkType::Hardlink` with EXDEV context message. (Note: no automated cross-fs test — surfaced as a reviewer check below.)
- Preserve basename + collision handling → Task 2 `dedupe_name`; Task 5 collision test.
- `--regenerate` delete-then-rebuild + incremental prune → Task 3 `remove_managed_tree`/`prune_stale_links`; Task 5 both tests.
- `--include` filter → Task 2 `plan_links` `wants`; Task 5 filter test.
- Non-destructive guard → Task 3 `check_no_foreign_files`; Task 3 unit test + Task 5 non-destructive test.
- Acceptance "count originals unchanged" / "delete symlink ≠ delete original" → Task 5 `non_destructive_originals_unchanged`.
- Acceptance "regenerate after deleting half rebuilds all" → Task 5 `regenerate_rebuilds_after_manual_deletion`.

**2. Placeholder scan:** No "TBD"/"add error handling"/"similar to Task N", no planted-then-fixed scaffolds, no out-of-order back-references. All code blocks are complete and self-consistent. Task 5's tests call the real `Catalog::test_insert_duplicate_group` helper (defined and committed in Task 1, Step 6) directly, and compute every group-folder name from the returned `gid`.

**3. Type consistency:** `ReviewEntry`/`ReviewGroup` field names match between Task 1 (definition), Task 2 (`plan_links` usage), and Task 5 (construction). `build_review_tree` signature identical in Task 3 (def), `lib.rs` re-export, and Task 4 (CLI call). `category_of` returns `Option<(&'static str, &'static str)>` consistently in Task 2 impl and its unit test. `PlannedLink { rel_dir, original }` consistent. `ReviewTreeReport { links_created, links_removed, groups, errors }` consistent across Task 3 and Task 4.

---

## Notes for the reviewer (double-check these)

- **`group_{gid:05}` numbering in tests:** every test computes the group-folder name from the `gid` returned by `test_insert_duplicate_group`, so they do not assume the sequence starts at `1`. (On a fresh catalog the first group is `1` → `group_00001_…`, but nothing depends on that.)
- **Hardlink cross-filesystem path is not auto-tested** (CI temp dirs are usually one filesystem). The `create_link` EXDEV error message is asserted only by reading code. If you want coverage, add an `#[ignore]` test documenting a two-filesystem setup.
- **`<library>` substitution is intentionally not used by the CLI** (the positional `<OUTPUT>` arg wins). `substitute_library` exists and is unit-tested for a future config-driven default. Confirm this matches the intended UX before shipping; if the config default should drive the path when no arg is given, the `Command::ReviewTree.output` would need to become `Option<PathBuf>` (a CLI change beyond this plan's scope — surface it).
- **Second-connection hazard:** tests use `Catalog::test_insert_duplicate_group` (single shared connection) rather than opening a second `duckdb::Connection` to the same file. Keep it that way.
```
