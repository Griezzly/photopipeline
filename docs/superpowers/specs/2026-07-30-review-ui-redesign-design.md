# Review UI redesign — design

**Date:** 2026-07-30
**Source design:** `Photopipe.dc.html` + `Rail.dc.html` in the Claude Design project
"UI mockups for image review" (`75034e4f-bfb3-402c-a7a8-c8e03b56dace`).

## Goal

Replace the functional-but-plain `photopipe serve` web UI with the visual language
defined in the mockups: an Onboard-cyan chrome, neutral grey photo wells, monochrome
defect flags, and a 56px navigation rail. Scope is a **reskin against the existing
API surface** — no new endpoints, no new catalog queries.

## Non-goals

- Any new HTTP endpoint or catalog query.
- A JavaScript build step, bundler, or framework. The shipped binary must keep
  embedding plain ES modules via `rust-embed`.
- Responsive/mobile layout. The design targets a single desktop window at a
  1440×900 baseline.

## Design system

Taken verbatim from the mockups' notes section.

**Decision semantics** — the only place hue carries meaning:

| Decision | Colour (dark / light) | Key |
|---|---|---|
| Keep | `#4ADE80` / `#16A34A` | Space |
| Reject | `#FF6B66` / `#BA1A1A` | X |
| Keeper (best of a duplicate group) | `#09B2C7` / `#007A8A` | K |
| Undecided | 1px neutral border | U to clear |
| Cursor | white ring, never coloured | — |

**Defect flags are monochrome.** Flags are facts, not decisions, so they carry no
hue: white monospace glyphs on a `rgba(0,0,0,.82)` scrim. Codes stay two or three
characters so a tile never needs more than one chip — `BLR`, `BF`, `OE`, `UE`, `IQA`.

**Rejected tiles drop to 42% luminance** so a wall of rejects stays readable
without red flooding the grid.

**Accent.** Onboard cyan is the only accent, and only on chrome — never on a photo
surface. Photos always render on `#121414` / `#1A1C1C` wells.

## Theming

One global theme on `<html data-theme>`, defaulting to **dark**, persisted in
`localStorage`, toggled by the sun/moon button in the top bar. Every colour in
`style.css` resolves through a custom property, so both themes come from a single
`tokens.css` block. The light mockups (1a, 1b, 1c, 1f, 1k) and the dark mockups
(1e, 1g, 1h, 1i, 1j) are the same components under different token values.

This deviates from the mockups' note that photo screens are always dark and setup
screens always light: a single user-chosen theme was preferred.

## Architecture

Flat `crates/cli/assets/` — the `/:file` route serves top-level embedded files only,
so no subdirectories. One concern per module.

```
assets/
  index.html        shell: rail + view slots + toast host + modal host
  Manrope.ttf       embedded variable font (weights 200-800)
  tokens.css        @font-face, light/dark custom properties, keyframes
  style.css         components + view layout, all colours via tokens
  app.js            api(), router, theme store, boot
  icons.js          SVG path strings lifted from the design
  rail.js           nav rail
  toast.js          toast + banner host
  libraries.js      screen 1a
  picker.js         screen 1b (modal)
  analyze.js        screens 1c / 1d
  review.js         screens 1e / 1f / 1g
  detail.js         screen 1h
  duplicates.js     screen 1i
  compare.js        screen 1j
  export.js         screen 1k
```

`review.js` today mixes grid rendering, the detail lightbox, keyboard handling, and
export. Splitting `detail.js`, `compare.js`, and `export.js` out of it is part of
this work, not incidental refactoring — each screen in the design is large enough to
own a file.

The existing `home.js` and `browse.js` are replaced by `libraries.js` and
`picker.js`; `analyze.js`, `review.js`, and `duplicates.js` are rewritten in place.

### Navigation rail

56px, from `Rail.dc.html`: Libraries → Review → Duplicates → Export → Develop.
Develop is drawn but disabled with a dot marker — the slot for RAW→JPEG after
export. The bottom gear is inert; the design defines no settings screen.

Review, Duplicates, and Export are disabled until a library is active.

## Rust changes

One change only: `static_asset` in `crates/cli/src/serve/handlers.rs` matches on
file extension for `Content-Type` and has no font arm. Add:

- `ttf` → `font/ttf`
- `svg` → `image/svg+xml`

Without this the font is served as `application/octet-stream`.

## Screens and their data sources

All from endpoints that exist today.

| Screen | Endpoints |
|---|---|
| 1a Libraries | `GET /api/libraries`, `POST /api/open` (for `pending_new` → re-analyze banner) |
| 1b Folder picker | `GET /api/fs`, `GET /api/libraries` |
| 1c/1d Analyzing | `POST /api/analyze`, `GET /api/analyze/status` |
| 1e/1f/1g Review grid | `GET /api/photos`, `GET /api/counts`, `POST /api/decisions`, `GET /thumb/:id` |
| 1h Photo detail | `GET /api/photos/:id`, `GET /preview/:id`, `POST /api/decisions` |
| 1i Duplicates | `GET /api/clusters`, `POST /api/decisions` |
| 1j Compare | `GET /preview/:id` |
| 1k Export | `GET /api/export/estimate`, `POST /api/export` |

### Derived, not faked

Several design elements have no dedicated endpoint but are honestly derivable from
data already on the client:

- **"Analyzed" badge in the folder picker (1b)** — cross-reference each `/api/fs`
  entry path against the folder list from `/api/libraries`.
- **Stage checklist, done/active/pending (1c/1d)** — `/api/analyze/status` returns a
  single stage string. Derive state from its position in the known ordered list:
  `scanning`, `detecting defects`, `scoring quality`, `calibrating`,
  `grouping duplicates`. `files_total > 0` renders a determinate bar with an
  `n / total` figure; `files_total == 0` renders the sweeping indeterminate band.
  Never a fake percentage.
- **Flag chip counts and filter-menu counts (1e/1f)** — count `flags` across the
  loaded photo list. "Any defect flag" and "No flags at all" fall out of the same
  pass. Request a high `limit` so the counts cover the library.
- **"In a duplicate group" filter (1e)** — `group_id != null`, client-side.
- **"39% culled" (1e)** — `/api/counts`.
- **"top 12% of this library" (1h)** — rank the photo's `iqa_score` against the
  distribution in the loaded list.
- **Cluster undo (1i)** — `POST /api/decisions` with `undecide` for every member of
  the cluster. Matches the design's "one undo restores all four".
- **"Accept all suggestions" (1i)** — loop `keeper` over every undecided cluster's
  suggested keeper, behind a confirm.

### Cut, because the data does not exist

Six mockup elements are dropped rather than faked. Each would require the backend
work this spec excludes.

| Element | Why | Replacement |
|---|---|---|
| Per-library cull-progress bars (1a) | `/api/libraries` returns no verdict counts; rendering the table would mean opening every catalog | Drop the column. Folder / Photos / Analyzed remain. |
| "2,183 pairs compared · 14 clusters so far" (1d) | No live secondary counter in `JobState` | Show `status.message` |
| "Skip stage" button (1d) | No API | Omitted |
| Cluster "20:14:29 → 20:14:33 · 96% similar" (1i) | `ReviewListItem` has no `captured_at`; similarity is never exposed | "4 frames · 2026-07-18" from `ReviewCluster::date` |
| "Sort: capture time" (1e) | No per-item capture timestamp in the list payload | Sort by quality score / filename / flagged-first |
| Export destination picker, free-space tile, `rejected.txt` checkbox (1k) | Destination is hardcoded server-side; no free-space or sidecar API | Read-only destination path, two stat tiles instead of three |

`Fit / 100% / 200%` in 1h and 1j is kept but is **relative to the 2048px preview**,
not sensor pixels — `/preview/:id` is a downscaled webp, so true 1:1 is impossible.
The control is labelled accordingly.

## Keyboard model

From the shortcut sheet in 1e, unchanged from the current app where they overlap:

| Action | Keys |
|---|---|
| Move between photos | ← → j k |
| Keep | Space |
| Reject | x |
| Undo the last decision | u |
| Mark as keeper of its group | **Shift+K** |
| Fullscreen | f |
| Compare selected frames | c |
| Exit / close | Esc |
| Toggle the shortcut sheet | ? |

The mockup's shortcut sheet lists `K` for both "move between photos" and "mark as
keeper", which cannot both hold. Resolved the way the current app already does it:
lowercase `k` moves, `Shift+K` sets the keeper. The sheet renders `⇧K` for the
keeper row.

Deciding advances to the next frame; holding Shift stays put.

## Error handling

- Every `api()` failure surfaces as an error toast (1k, red variant) with the
  failing operation named. No `alert()` or `confirm()` anywhere — the design
  replaces both with the modal and toast components.
- A 409 from `/api/analyze` means a job is already running: show the analyzing
  screen for the running job instead of an error.
- `/api/open` returning 409 means the folder is busy being analyzed; show the
  analyzing screen.
- Missing thumbnails already fall back to a server-side placeholder SVG; the tile
  renders it without special-casing.
- The "models not installed" warning banner (1k, amber variant) fires when
  `status.ml_ran === false`, telling the user this library has defect flags only and
  tiles show a dash instead of a score.

## Testing

- `crates/cli/tests/serve.rs` covers the API surface and must keep passing
  unchanged — no endpoint behaviour changes.
- Add a test asserting `/Manrope.ttf` is served with `Content-Type: font/ttf`,
  covering the one Rust change.
- Add a test asserting every asset referenced by `index.html` resolves 200 through
  the `/:file` route, so a renamed module cannot ship broken.
- Manual verification against a real library on the WSL box: every screen, both
  themes, and the keyboard model.

## Acceptance

1. All eleven screens render in the new visual language at 1440×900.
2. Theme toggle switches every screen and survives a reload.
3. `cargo fmt`, `cargo clippy --all-targets --all-features -- -D warnings`, and
   `cargo test --all` pass.
4. No new endpoint, no new catalog query, no build step.
5. The six cut elements are absent, not stubbed with fake values.
