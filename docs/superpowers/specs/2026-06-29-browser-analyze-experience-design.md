# Browser analyze experience — Design Spec (Spec 2 of 2)

**Date:** 2026-06-29
**Status:** Approved (brainstorm) — ready for implementation planning
**Scope:** Turn `photopipe serve` into an out-of-the-box web app: open the browser, pick a folder, watch it analyze to its fullest (scan → calibrate → dedupe), then review — plus a home screen of previously-analyzed folders. Builds on the per-folder library foundation (spec 1, shipped).

> **Decomposition note:** This is **spec 2 of 2**. Spec 1 (per-folder library model + CLI migration) is merged. This spec adds the browser-driven analyze flow on top. The CLI is unchanged.

## 1. Motivation

The per-folder library model exists, but you still have to drive it from the CLI (`scan`, then `calibrate`, then `dedupe`, then `serve <folder>`). The goal: `photopipe serve` opens a **Home** screen; you click "Analyze a folder", pick one in a server-side folder browser, and a one-click background job runs the full pipeline with live progress, then drops you into the existing review UI. Previously-analyzed folders are listed for instant re-open. The CLI stays a first-class power interface over the same libraries.

## 2. Decisions locked during brainstorming

| Decision | Choice |
|---|---|
| Folder selection | Server-side folder browser (`GET /api/fs`) — local-first, server reads by path |
| Long-running analysis | In-process background job + **polling** progress (`POST /api/analyze` + `GET /api/analyze/status`); single job at a time |
| Analyze scope | Auto-chain **scan → calibrate → dedupe** ("fullest") |
| Models missing | **Analyze anyway, ML skipped**, surface a banner (`ml_ran=false`) |
| Re-open behavior | Open is instant; detect new/changed files and offer **Re-analyze** (incremental) |
| Cancel | **No cancel in v1** (partial catalog stays valid; re-analyze resumes) |
| Active library | Server tracks one active library; review endpoints scope to it; set on analyze-complete or open |
| Frontend | Zero-build vanilla SPA (Home → Browse → Analyze → Review), reusing the existing review UI |

## 3. Architecture & flow

```
photopipe serve            (one binary; CLI unchanged)
  │  Home (recent libraries + "Analyze a folder")
  │     │ "Analyze a folder"            │ click a recent library
  │     ▼                               ▼
  │  Browse (GET /api/fs)          POST /api/open {folder}  → set active, return pending-new count
  │     │ "Analyze this folder"         │
  │     ▼                               ▼
  │  POST /api/analyze {folder}    Review (existing grid)
  │     → background job                 ▲  ("N new — Re-analyze" banner if pending>0;
  │  Analyze (poll GET /api/analyze/status)   ML-skipped banner if ml_ran=false)
  │     → on done: set active ───────────┘
```

New HTTP endpoints (all `127.0.0.1`-bound, in the `cli` crate's `serve` module):
- `GET /api/fs?path=<abs>` — list subdirectories of `path` (+ its parent), each with a non-recursive photo count; with no `path`, list roots. Directories only.
- `GET /api/libraries` — recent libraries (wraps `pipeline::library::list_libraries`): `[{ folder, photo_count, last_analyzed }]`.
- `POST /api/analyze { folder }` — start the background analyze job; returns `{ job_id }` (or `409` if one is already running).
- `GET /api/analyze/status` — current job state (see §5).
- `POST /api/open { folder }` — open an existing library, set it active, return `{ pending_new: u64 }`.

Existing review endpoints (`/api/photos`, `/api/photos/:id`, `/api/groups`, `/thumb/:id`, `/preview/:id`, `/api/decisions`, `/api/counts`, `/api/export`, `/api/export/estimate`) are re-pointed from a fixed catalog to the **active library** (§6).

Pipeline (in `crates/pipeline`):
- A new orchestration entry point `analyze_folder(library, models, cfg, progress)` runs `ingest → defects → ml → calibrate → dedupe` against the library, invoking a progress callback at stage boundaries and per file during ingest. Returns an `AnalyzeReport { ml_ran, scanned, … }`.
- `ingest_directory` gains an **optional** per-file progress hook (default none → CLI behavior unchanged).
- The CLI's `scan`/`calibrate`/`dedupe` commands are unchanged (they call the same underlying functions, not `analyze_folder`).

## 4. Folder browser (`GET /api/fs`)

Request: `GET /api/fs?path=<abs>` (omit `path` for roots).
Response: `{ path: Option<String>, parent: Option<String>, entries: [{ name, path, photo_count }] }`.
- `entries` are **directories only**, sorted by name; `photo_count` is a non-recursive count of files whose extension is in the ingest extension set (cheap `read_dir`, no decode).
- No `path` → roots: on Windows, the available drive letters (`C:\`, `D:\`, …); on Unix, the home dir and `/`.
- Unreadable directory (permissions) → `403`/`500` with a message, never a panic. Listing only — never returns file contents.

## 5. Analyze job + progress

`POST /api/analyze { folder }`:
1. Resolve `open_or_create_library(roots, folder)`.
2. If a job is already running → `409 "analysis already running"`.
3. Detect model availability (`ModelHub::from_config`; on failure or missing files, use `ModelHub::empty()` and record `ml_ran=false`).
4. Spawn one background thread running `analyze_folder` with a progress sink writing into shared job state.
5. Return `{ job_id }`.

Job state (shared `Mutex`/atomics in `AppState`), exposed by `GET /api/analyze/status`:
```
{ "stage": "scanning" | "calibrating" | "deduping" | "done" | "failed",
  "files_done": u64, "files_total": u64,
  "ml_ran": bool, "folder": String, "message": String, "error": null | String }
```
- The **scanning** stage drives the progress bar from `files_done/files_total` (ingest + defects + ML over the folder's files). `calibrate`/`dedupe` report coarse start/finish.
- Per-file analyze failures `warn!` + count + continue (never abort the job).
- On success: `stage=done`, set the analyzed library **active**; the UI navigates to Review (showing the ML-skipped banner when `ml_ran=false`).
- On error: `stage=failed`, `error` set; the UI shows it.
- After completion, `last_analyzed` is stamped on `library_meta`.

No cancel endpoint in v1. A second `POST /api/analyze` while one runs is rejected (`409`).

## 6. Active library & open/staleness

`AppState` replaces the fixed `catalog: Arc<Catalog>` / `cache: Arc<Cache>` with:
```
active: Mutex<Option<ActiveLibrary>>   where ActiveLibrary { folder: PathBuf, catalog: Arc<Catalog>, cache: Arc<Cache> }
```
- A small accessor returns the active library or maps to `409 "no library open"`. Every review handler uses it instead of `state.catalog`/`state.cache`.
- The active library is set when (a) an analyze job completes, or (b) `POST /api/open` succeeds. It is **not** set during a fresh analyze (the progress screen issues no review calls), so the analyze thread is the sole accessor of that catalog while running.
- For an incremental **re-analyze** of the already-active library, the UI is on the progress screen during the run, so no concurrent review reads occur.

`POST /api/open { folder }`:
- `open_existing_library(roots, folder)` → `404` if none.
- Set active; compute `pending_new`: walk the folder for ingest-extension files and count those for which `catalog.needs_processing(path, mtime, size)` is true (new or changed). Cheap — directory walk + metadata, no decode.
- Return `{ pending_new }`. Review shows "*N new photos — Re-analyze*" when `pending_new > 0`; the button calls `POST /api/analyze` on the same folder.

`serve <folder>` (optional positional, retained from spec 1): open that library and land directly on Review. Plain `serve` lands on Home.

## 7. Frontend SPA

Zero-build vanilla (HTML + ES-module JS + CSS, embedded via rust-embed). One page, four views switched by a tiny client-side router (hash-route or state-driven show/hide). The existing review grid/detail/keyboard/export code becomes the **Review** view unchanged in behavior.

- **Home:** recent libraries (`GET /api/libraries`) as clickable cards (folder, photo count, last-analyzed) + an "Analyze a folder" button → Browse.
- **Browse:** breadcrumb + folder list (`GET /api/fs`) with photo counts; "Analyze this folder" → POST analyze → Analyze view.
- **Analyze:** stage label + progress bar polling `GET /api/analyze/status`; on `done` → Review (active library); on `failed` → error message + back to Home.
- **Review:** the existing grid, plus two banners — ML-skipped (when `ml_ran=false`) and "N new — Re-analyze" (when `pending_new>0`).

## 8. Error handling

Per project conventions (`CLAUDE.md`):
- `anyhow::Result` / `StatusCode` at HTTP boundaries; `tracing` not `println!`; DuckDB only; non-destructive (libraries in app-data; originals only read).
- Background job: per-file failures warn+continue; a fatal error sets `stage=failed` + `error`; the thread never poisons the server.
- `GET /api/fs` handles permission/IO errors cleanly; never serves file contents.
- Concurrency: single analyze job (409 on conflict); active library set only when no writer is active on it (analyze-complete / open), avoiding concurrent catalog access.
- Server binds `127.0.0.1` only.

## 9. Testing

- **Pipeline:** `analyze_folder` over a tiny synthetic folder (`ModelHub::empty()`) runs the full chain, fires the progress callback (stage transitions + per-file ticks), returns a report with `ml_ran=false`; idempotent re-run does no new ingest work.
- **Server (axum `tower::oneshot`, temp app-data via `XDG_*`):**
  - `GET /api/fs` lists subdirectories with correct photo counts; roots case returns drive/`/` roots; unreadable path errors cleanly.
  - `GET /api/libraries` returns scanned libraries.
  - `POST /api/analyze` then poll `GET /api/analyze/status` reaches `done` with `ml_ran=false` on a model-less run; a second concurrent `POST` gets `409`.
  - `POST /api/open` returns the correct `pending_new` (0 right after analyze; >0 after dropping a new file in).
  - Review endpoints return `409` when no library is open, and serve once a library is active.
- **Frontend:** build + `curl` the asset/API routes; a Playwright smoke at the end (Home → Browse → Analyze → Review, confirming progress reaches done and the grid renders).
- `cargo fmt`, `cargo clippy --all-targets --all-features -- -D warnings`, `cargo test --all` green. Include a per-task `cargo fmt`+commit step (avoids the fmt-drift that recurred in prior phases).

## 10. Out of scope (v1)

- Cancelling an in-progress analysis.
- Reviewing one library while another analyzes (single job; progress screen during a run).
- Cross-folder/global dedupe (per-folder model is intentional).
- Auth / network exposure (127.0.0.1 only).
- Server-Sent Events / websockets (polling is sufficient for localhost).
- A native OS folder dialog (server-side browser is the chosen mechanism).
