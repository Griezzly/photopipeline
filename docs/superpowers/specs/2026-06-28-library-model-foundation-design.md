# Library-model foundation + CLI migration — Design Spec (Spec 1 of 2)

**Date:** 2026-06-28
**Status:** Approved (brainstorm) — ready for implementation planning
**Scope:** Replace the single config-defined catalog with **per-folder libraries** resolved from the folder path, stored in OS app-data, and self-describing via a `library_meta` table. Migrate every CLI command onto this model and retire `[catalog] db_path`/`cache_dir`. This is the foundation both the CLI and the (separate) browser analyze flow build on.

> **Decomposition note:** This is **spec 1 of 2**. Spec 2 ("browser analyze experience" — server-side folder browser, background analyze job with progress, active-library switching, home → browse → analyze → review SPA) builds on this foundation and is brainstormed separately after this ships. Anything about the in-browser analyze flow is **out of scope here** (see §10).

## 1. Motivation

Today the CLI operates on one catalog whose path comes from `[catalog] db_path` in the config. We want an out-of-the-box browser UX where you pick a folder and it analyzes that folder — which implies **one library per folder**. To keep the CLI and the future web flow coherent (the user's explicit requirement), the per-folder library must become the *single* model for everything; the CLI cannot keep operating on a separate config catalog.

This spec introduces the per-folder library as that shared model and migrates the CLI to it. After this ships, `photopipe scan <folder>` / `calibrate <folder>` / `serve <folder>` all operate on the same per-folder library the web flow will later use — no divergent stores.

## 2. Decisions locked during brainstorming

| Decision | Choice |
|---|---|
| Folder → library mapping | One catalog per folder |
| Library storage | OS app-data, keyed by a hash of the folder path (photo folder stays pristine) |
| Library identity / listing | **Self-describing**: each catalog stores its own `folder_path` in a `library_meta` table; listing enumerates the app-data dir and reads each catalog. No JSON, no central registry. |
| Persistent state format | **DuckDB only** (no JSON index files) |
| CLI ↔ web coherence | **Fully unified** — per-folder libraries are the one model; the single config catalog is retired |
| Per-command library selection | Positional `<folder>` argument (not a `--library` flag); `info <file>` auto-resolves by walking up to the nearest ancestor library |

## 3. Library resolver (`pipeline::library`)

A new module `crates/pipeline/src/library.rs` is the single place that maps a folder to its storage and opens/lists libraries.

- **Path → hash.** `library_key(folder) = format!("{:032x}", xxh3_128(canonical_path_bytes))` where `canonical_path` is `std::fs::canonicalize(folder)` when the folder exists (its output is stable per path — on Windows a `\\?\` verbatim form, which is fine since we only hash it), falling back to a lexically-normalized absolute path otherwise. Canonicalization normalizes symlinks and (on Windows) case, so `C:\Photos`, `c:\photos`, and `C:\Photos\` resolve to one library. Reuse the existing `xxhash-rust` (xxh3) dependency — no new dep.
- **Storage layout** (derived from `dirs`, mirroring the existing data/cache split):
  - catalog: `dirs::data_dir()/photopipe/libraries/<key>/catalog.duckdb`
  - preview cache: `dirs::cache_dir()/photopipe/libraries/<key>/`
- **Public API:**
  - `pub struct Library { pub folder: PathBuf, pub catalog: Catalog, pub cache: Cache }`
  - `pub struct LibraryInfo { pub folder: PathBuf, pub key: String, pub created_at: i64, pub last_analyzed: Option<i64>, pub photo_count: i64 }`
  - `open_or_create_library(folder: &Path) -> anyhow::Result<Library>` — resolves paths, opens the catalog (runs migrations), creates dirs, and ensures the `library_meta` row exists with the real `folder_path`.
  - `open_existing_library(folder: &Path) -> anyhow::Result<Option<Library>>` — `None` if no catalog exists for that folder (used by read-only commands to error cleanly instead of creating an empty library).
  - `list_libraries() -> anyhow::Result<Vec<LibraryInfo>>` — enumerate `data_dir()/photopipe/libraries/*`, open each catalog, read `library_meta` + `COUNT(*) FROM files`. Skips dirs whose catalog won't open (logs `warn!`).
  - `find_library_for_file(file: &Path) -> anyhow::Result<Option<PathBuf>>` — from the file's parent, walk up ancestors; return the deepest ancestor whose library catalog exists. `None` if no analyzed ancestor.

## 4. `library_meta` table (schema v3)

Add migration v3 to `MIGRATIONS` in `crates/pipeline/src/catalog/schema.rs`:

```sql
BEGIN TRANSACTION;
INSERT INTO schema_version VALUES (3);
CREATE TABLE library_meta (
    folder_path   VARCHAR NOT NULL,
    created_at    BIGINT  NOT NULL,
    last_analyzed BIGINT
);
COMMIT;
```

Bump `EXPECTED_SCHEMA_VERSION` (in `crates/cli/src/main.rs`) from 2 to 3.

Catalog methods (in `catalog/mod.rs`):
- `set_library_meta(&self, folder_path: &str, created_at: i64) -> Result<(), CatalogError>` — inserts the single row if absent (idempotent; leaves an existing row untouched except it is the create path).
- `set_last_analyzed(&self, ts: i64) -> Result<(), CatalogError>` — updates `last_analyzed` on the row.
- `library_meta(&self) -> Result<Option<(String, i64, Option<i64>)>, CatalogError>` — reads `(folder_path, created_at, last_analyzed)`.

`last_analyzed` is set when `scan` completes (the primary analysis); `calibrate`/`dedupe` do not change it.

## 5. CLI migration

Every command resolves a per-folder library. Resolution uses `open_or_create_library` for write/analysis commands and `open_existing_library` for read-only ones (clean error if not yet scanned).

| Command | New invocation | Resolution |
|---|---|---|
| `scan <folder>...` | unchanged | each `<folder>` → `open_or_create_library`; sets `folder_path` on create, `last_analyzed` on completion |
| `calibrate <folder>` | + positional folder | `open_existing_library` |
| `dedupe <folder>` | + positional folder | `open_existing_library` |
| `stats <folder>` | + positional folder | `open_existing_library` |
| `export-keepers <folder> <output>` | + positional folder | `open_existing_library` |
| `review-tree <folder> <output>` | + positional folder | `open_existing_library` |
| `info <file>` | unchanged | `find_library_for_file`; error if no analyzed ancestor |
| `serve <folder>` | + positional folder | `open_or_create_library`; serves the existing review UI for that library |
| `doctor` | unchanged | no fixed-catalog schema check (none exists); checks toolchain/models/disk/ORT only |
| `libraries` *(new)* | — | `list_libraries()` → prints folder, last-analyzed, photo count |

Read-only commands on a folder with no library print: `no library for <folder> — run 'photopipe scan <folder>' first` and exit non-zero.

`serve <folder>` in this spec simply opens the resolved library and serves the current review UI (the web home/browse/analyze flow is spec 2). The active-library abstraction and folder browser are deferred to spec 2.

## 6. Config changes

In `crates/pipeline/src/config.rs`, `CatalogConfig` drops `db_path` and `cache_dir` (library roots derive from `dirs`). `write_batch_size` and `enable_vss` remain. Removing the two keys does not break existing configs — `#[serde(default)]` without `deny_unknown_fields` means old `db_path`/`cache_dir` keys are silently ignored. All other config sections (`[models]`, `[ingest]`, `[defect]`, `[dedupe]`, `[output]`) are unchanged. Update `photopipe.example.toml` accordingly.

## 7. Relationship to the prior Windows/dirs work

The Windows spec already routes defaults through `dirs` (`data_dir`/`cache_dir`). This spec reuses that: library roots are `dirs::data_dir()/photopipe/libraries/<key>/` and `dirs::cache_dir()/photopipe/libraries/<key>/`. The single `catalog.duckdb`/`cache` at the top of those dirs (the old default) is no longer used; pre-existing single catalogs from before this change are simply orphaned (the user can delete them; documented in the README).

## 8. Error handling

- Per project conventions: `anyhow::Result` at the CLI boundary; `thiserror`-based `CatalogError`/`IngestError` inside `pipeline`; `tracing` for logs, `println!` only for user-facing CLI output.
- DuckDB only; migrations atomic (`BEGIN TRANSACTION; … COMMIT;`).
- Non-destructive: libraries live entirely in app-data; nothing is ever written into the photo folder.
- `canonicalize` failure (folder missing) on a read-only command → clean "no such folder / no library" error, not a panic.
- `list_libraries` tolerates an unreadable/corrupt library dir by skipping it with a `warn!` (one bad library never breaks the listing).

## 9. Testing

- **Resolver (`library.rs` unit/integration):** `library_key` is stable and identical for `C:\X`, `c:\x`, `C:\X\` (canonicalized); two different folders get different keys; storage paths land under `data_dir`/`cache_dir` with the key.
- **`library_meta` migration:** fresh catalog is at schema v3 and has `library_meta`; `set_library_meta`/`set_last_analyzed`/`library_meta` round-trip; legacy v2 catalogs migrate to v3 on open.
- **`find_library_for_file`:** with a library created for `/a/b`, a file `/a/b/c/x.jpg` resolves to `/a/b`; a file under an un-analyzed folder resolves to `None`.
- **`list_libraries`:** two scanned temp folders both appear with correct `folder_path` and `photo_count`; a junk dir under the libraries root is skipped.
- **CLI (extend `crates/cli/tests/cli.rs`):** `scan <temp-folder>` creates a library; `stats <temp-folder>` reads it; `stats <un-scanned>` errors non-zero with the expected message; `info <file>` resolves via ancestor walk; `libraries` lists the scanned folder. All tests point `dirs` at a temp app-data via env (`XDG_DATA_HOME`/`XDG_CACHE_HOME` on Linux CI) so nothing touches the real app-data.
- `cargo fmt`, `cargo clippy --all-targets --all-features -- -D warnings`, `cargo test --all` green before done.

## 10. Out of scope (this spec)

Deferred to **spec 2 (browser analyze experience):**
- Server-side folder-browser API (`GET /api/fs`).
- Background analyze job + progress reporting (pipeline progress callbacks, `POST /api/analyze`, polling endpoint).
- Active-library switching in the server (`AppState` active slot, `POST /api/open`, `GET /api/libraries` HTTP endpoint) and scoping the review API to it.
- The home → browse → analyze → review SPA.

Also out of scope: migrating data from a pre-existing single config catalog into per-folder libraries (orphaned; user deletes it). Cross-folder/global dedupe (the per-folder model is intentional).
