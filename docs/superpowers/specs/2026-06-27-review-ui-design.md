# Review UI — Design Spec

**Date:** 2026-06-27
**Status:** Approved (brainstorm) — ready for implementation planning
**Scope:** A local web application for reviewing the photo catalog, recording keep/reject decisions, and materializing a curated keepers export tree.

## 1. Motivation

Today the "review" step is a symlink/hardlink tree (`photopipe review-tree`) that the user
navigates in their OS file browser. It is read-only: there is no way to record a verdict,
override a pipeline flag, or pick a keeper within a duplicate group. The user judges photos
in the file manager and then acts entirely by hand.

This project replaces that workflow with a **local web app** that lets the user:

- Browse the whole catalog, with flagged/duplicate photos surfaced first.
- See each photo with its flag reasons, confidence, quality score, and EXIF side-by-side.
- Record decisions (keep / reject / pick keeper) that persist in the catalog.
- Materialize a `keepers/` export tree on demand for hand-off to other tools.

Originals are never modified or moved — the non-destructive contract is unchanged.

## 2. Decisions locked during brainstorming

| Decision | Choice |
|---|---|
| Primary job | Triage **and** act on disk |
| Form factor | Local web app (axum server + browser) |
| Frontend stack | Zero-build vanilla: HTML + ES-module JS + CSS, no npm/bundler |
| Review scope | Everything browsable, **flagged-first** surfacing |
| Source of truth | Decisions recorded in the DuckDB catalog |
| On-disk output | **Keepers export tree**, materialized on demand |
| Decision persistence | **Write-through** — every decision commits to DuckDB immediately |

## 3. Architecture

```
photopipe serve  ──>  axum server (in the existing cli crate)
                        │
                        ├── GET  /api/photos?filter=...   → JSON: paged photo list + flags + scores
                        ├── GET  /api/photos/:id          → JSON: full metadata for one photo
                        ├── GET  /api/groups              → JSON: duplicate groups + members + suggested keeper
                        ├── GET  /thumb/:hash             → WebP bytes (from existing content-hash cache)
                        ├── GET  /preview/:hash           → larger WebP (decoded on demand, cached)
                        ├── POST /api/decisions           → record keep/reject/keeper-pick (write-through)
                        └── POST /api/export              → materialize keepers tree
                        │
                        └── static: index.html + app.js + style.css (rust-embed)
```

### Crate boundaries

- **`crates/pipeline`** (library) — owns all data and disk logic:
  - new `decisions` table + migration (schema version 2)
  - query methods for the review list (files ⨝ flags ⨝ scores), decision upsert, keeper-pick
  - keepers-export link planning + materialization (reuses `output/`)
- **`crates/cli`** (binary) — owns the HTTP layer:
  - `serve` subcommand → boots axum bound to `127.0.0.1:PORT`
  - JSON + thumbnail + preview + decision + export handlers
  - embeds the frontend assets (rust-embed) and serves them statically

### Reuse of existing code

- **Thumbnails:** the content-hash-addressed WebP cache (`crates/pipeline/src/cache/mod.rs`)
  already holds previews produced during `scan`. `/thumb/:hash` is mostly a cache lookup.
- **Larger previews:** `extract_preview_raw` / `extract_preview_jpg` / `encode_webp`
  (`crates/pipeline/src/ingest/preview.rs`) decode and encode on demand; results cached.
- **Export tree:** the link planner and guarded, idempotent I/O in `crates/pipeline/src/output/`
  are reused — same symlink/hardlink logic and `OutputConfig.link_type`, driven by decisions
  instead of raw flags.
- **Catalog queries:** existing query methods feed the JSON endpoints where they fit; new
  joins are added where needed.

## 4. Data model

A new migration adds schema version 2 with one table:

```sql
CREATE TABLE decisions (
    file_id      BIGINT PRIMARY KEY REFERENCES files(id),
    verdict      VARCHAR NOT NULL,                 -- 'keep' | 'reject'
    is_keeper    BOOLEAN NOT NULL DEFAULT false,   -- chosen keeper within its duplicate group
    note         VARCHAR,                          -- optional free-text
    decided_at   BIGINT NOT NULL
);
```

Rules:

- **Undecided = absence of a row.** No third enum state. A fresh scan starts with everything
  undecided.
- **`is_keeper`** is per-file but only meaningful inside a duplicate group. Picking a keeper
  sets `is_keeper=true` on that file and `false` on its group siblings, and implies
  `verdict='keep'` for the keeper.
- **Distinct from the pipeline's suggestion.** `duplicate_members.is_suggested_keeper` remains
  the tool's *suggestion*; `decisions.is_keeper` is the *user's* choice. They never overwrite
  each other.
- **Idempotency preserved.** The `decisions` table is independent of `files`/flags, so
  re-running `scan` never touches it. Decisions survive re-scans as long as the `file_id`
  persists. The migration is atomic (`BEGIN TRANSACTION; ... COMMIT;`). Decisions also
  intentionally persist across `dedupe` regrouping (keyed by `file_id`); a re-grouped
  duplicate may therefore retain a prior reject decision — this keeps export correct (a
  rejected file is never exported) but the decision is surfaced in the UI without its
  original group context.

## 5. Review UI & workflow

Three views, one keyboard-driven loop. Decisions write through instantly; tiles reflect the
persisted verdict (e.g. colored border: green=keep, red=reject, none=undecided).

### 5.1 Queue view (default)

- Thumbnail grid of everything **needing a decision**, flagged-first: rejects, then uncertain,
  then duplicate groups. Clean keepers reachable via filter.
- Each tile shows the photo, flag badge(s), confidence, IQA score.
- Keyboard: `J/K` or arrows to move; `X` reject; `Space`/`Enter` keep; `U` undecide.

### 5.2 Detail view

- Large preview + full metadata panel (EXIF, sharpness, exposure, IQA) + flag reasons.
- Same keyboard verdicts; `←/→` walks the queue. `F` toggles from a tile.

### 5.3 Duplicate-compare view

- Members of a duplicate group side-by-side, suggested keeper highlighted, quality scores
  visible.
- `1–9` picks the keeper; remaining members implicitly marked reject; any can be overridden
  individually.

### 5.4 Filters / sort

Top bar: by flag type, camera, lens, date range, decided/undecided, quality score. This is how
the user reaches the full "everything" scope and hunts false negatives among clean keepers.

### 5.5 Footer

Live counts (kept / rejected / undecided) and an **Export keepers** button → `POST /api/export`.

## 6. Export

Exposed two ways: the `POST /api/export` endpoint (UI button) and a `photopipe export-keepers`
CLI command for headless use.

- Materializes a `keepers/<YYYY-MM>/` tree of links to every photo with `verdict='keep'`.
  For duplicate groups, only the chosen `is_keeper` is linked.
- **Default: keep means keep.** Undecided photos are excluded so export is deliberate.
  (A config/flag may later allow including undecided-but-unflagged clean photos; not in v1.)
- Reuses `output/`'s link planner + guarded idempotent I/O and `OutputConfig.link_type`
  (symlink default, hardlink option). Re-running export reconciles the tree (adds/removes
  links) without touching originals — same idempotency contract as `review-tree`.
- Writes a `README.txt` at the tree root explaining it is regenerated and safe to delete.

## 7. Error handling

Per project conventions (`CLAUDE.md`):

- Per-file failures (missing original, un-decodable preview) log `tracing::warn!` and continue —
  never abort a render pass or an export.
- A thumbnail absent from cache that cannot be extracted serves a placeholder image, not a 500.
- All decision writes and the export run inside DuckDB transactions; never leave half-written
  rows.
- `anyhow::Result` at axum handler boundaries; `thiserror`-derived errors inside the pipeline
  crate.
- The server binds `127.0.0.1` only — local-first, not exposed on the network.

## 8. Testing

- **Pipeline crate:** unit/integration tests for the `decisions` table (upsert; keeper-pick
  flips siblings; undecided = no row), the review-list join query, and export link-planning
  driven by decisions — reusing the synthetic-catalog fixtures the dedupe/review-tree tests
  already use.
- **CLI crate:** axum handlers tested with `tower::ServiceExt::oneshot` against a temp catalog —
  assert JSON shapes, decision round-trips, thumbnail content-type, and that export produces the
  expected link set.
- **No browser/E2E automation** for the zero-build vanilla UI in v1 — manual smoke-test instead.
- `cargo fmt`, `cargo clippy --all-targets --all-features -- -D warnings`, and
  `cargo test --all` must be green before the phase is declared done.

## 9. New dependencies

All MIT/Apache-2.0, no AGPL:

- `axum` — HTTP server
- `tower` / `tower-http` — middleware, static file serving
- `rust-embed` — embed frontend assets into the binary
- `tokio` — async runtime for axum

These are surfaced here per the "surface before implementing new dependencies" rule; they will
be added in this phase only.

## 10. Out of scope (v1)

- Authentication / network exposure (local-only).
- Sidecar/XMP or CSV export (considered, deferred).
- Editing photos or EXIF.
- Browser-based E2E test automation.
- Multi-user / concurrent-session coordination.
