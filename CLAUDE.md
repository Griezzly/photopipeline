# CLAUDE.md — Project context

This file is loaded automatically by Claude Code at the start of every session in this repo. Read it first.

## What this project is

`photopipe` is a local-first command-line tool that ingests a directory of RAW (and JPG) photos and produces (a) a DuckDB catalog with per-file metadata, defect flags, and duplicate-group assignments, and (b) a symlink-based "review tree" the user navigates with their OS file browser. Strictly non-destructive — originals are never modified or moved.

The full design lives in `../IMPLEMENTATION_PLAN.md`. **That document is the authoritative spec.** This file just gives you the meta-rules for working effectively on it.

## Where to find things

- `../IMPLEMENTATION_PLAN.md` — full spec, organized by phase, with acceptance criteria
- `crates/pipeline/` — library crate; all real work lives here
- `crates/cli/` — `photopipe` binary, thin CLI layer over `pipeline`
- `models/` — ONNX files (populated in Phase 3)
- `tools/` — one-time Python ONNX export scripts (the only Python in the project)
- `tests/fixtures/` — small test photos (user-curated)

## How to work on this

- **Work phase by phase.** Read the relevant phase section of `IMPLEMENTATION_PLAN.md` before writing any code. Do not modify files outside the current phase's scope without surfacing why first.
- **Each sub-deliverable is one git commit** with a conventional-commit-style message (`feat(ingest): ...`, `chore(deps): ...`, etc.).
- **Run `cargo fmt`, `cargo clippy --all-targets --all-features -- -D warnings`, and `cargo test --all` before declaring a phase done.** If any fails, fix it before claiming completion.
- **Surface before implementing** any deviation from the plan, any new dependency not already listed in the relevant phase, or any architectural shortcut. Don't silently substitute.
- **Ask the user** if a phase's acceptance criteria require fixture files (real RAW photos) that aren't in the repo. Don't fabricate fixtures with fake EXIF.

## Project conventions

### Rust style
- Edition 2021, stable toolchain, MSRV not pinned yet.
- `anyhow::Result` at command-handler boundaries; `thiserror`-derived types inside library crate (see `crates/pipeline/src/error.rs`).
- `tracing` for logging — `info!` for phase-level events, `warn!` for per-file failures, `debug!` for verbose detail. No `println!` outside of CLI output that's meant for the user (e.g., `doctor`, `stats`).
- One concern per module file. Add a `mod.rs` only when the module needs to expose submodules.
- Don't over-abstract early. Concrete types > traits until a second implementation actually exists.

### Error handling
- A single corrupt file must never abort a full scan. Wrap per-file work in a closure that returns `Result<T, IngestError>`; on `Err`, log with `tracing::warn!(path = %p.display(), error = %e)` and continue.
- Database operations happen inside transactions. Never leave half-written rows.
- Cache corruption (hash mismatch) automatically triggers re-extraction; don't propagate as a hard error.

### Database
- **DuckDB only.** Do not introduce SQLite even if it would be easier for a particular query.
- **Bulk inserts use the Appender API.** Row-at-a-time `INSERT` statements are a perf bug in DuckDB; the plan calls them out explicitly. Batch size lives in `CatalogConfig::write_batch_size`.
- Schema migrations are atomic — wrap each migration's SQL in `BEGIN TRANSACTION; ... COMMIT;`.
- `ON DELETE CASCADE` is not supported in current DuckDB; manage cascading deletes in application code if needed.

### Naming
- Modules: snake_case, singular nouns (`ingest`, not `ingesters`).
- Functions: `verb_object` (`ingest_directory`, `extract_preview`).
- Types: descriptive, not abbreviated (`IngestedFile`, not `IFile`).

## Build / test / run

```bash
cargo build --release
cargo test --all
cargo fmt
cargo clippy --all-targets --all-features -- -D warnings
./target/release/photopipe doctor
```

## Hard constraints — do not violate

- **No AGPL dependencies.** Avoid Ultralytics YOLO, Qt-based crates, etc. Prefer Apache 2.0 / MIT / BSD.
- **No Python at runtime.** Python lives only in `tools/` for one-time ONNX exports. The shipped binary must have zero Python dependency.
- **No SQLite.** This project is DuckDB-only.
- **No mutation of original photo files.** Symlinks, hardlinks, and reads only. Even in error paths.
- **No skipping the idempotency check.** Re-running `photopipe scan` on the same directory must do zero work on the second invocation. This is a correctness requirement, not a perf goal.
- **No fabricating EXIF or photo data** to pass tests. If you need fixtures, ask.

## When you're unsure

Surface the question to the user before making the call. The plan is detailed precisely because the user wants to be in the loop on design decisions. It is much better to ask than to silently pick the wrong path and need a rewrite later.
