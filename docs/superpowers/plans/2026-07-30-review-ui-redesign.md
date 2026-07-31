# Review UI Redesign Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the plain `photopipe serve` web UI with the visual language of the imported design mockups — Onboard-cyan chrome, neutral photo wells, monochrome defect flags, 56px nav rail — using only the HTTP endpoints that already exist.

**Architecture:** Flat `crates/cli/assets/` of plain ES modules embedded by `rust-embed`, with all colour resolved through CSS custom properties in `tokens.css` so one `data-theme` attribute on `<html>` restyles every screen. One module per screen; `app.js` owns the fetch helper, the router, and the theme store. Exactly one Rust change: two extra `Content-Type` arms in `static_asset`.

**Tech Stack:** Vanilla ES modules (no bundler, no framework, no build step), CSS custom properties, `rust-embed`, axum. Verification via the Playwright MCP browser against a live `photopipe serve`.

## Global Constraints

Copied from `docs/superpowers/specs/2026-07-30-review-ui-redesign-design.md` and `CLAUDE.md`. Every task's requirements implicitly include this section.

- **No new HTTP endpoint and no new catalog query.** If a screen needs data that no endpoint returns, cut the element — see "Cut elements" below. Do not add a route to `crates/cli/src/serve/mod.rs`.
- **No JavaScript build step, bundler, framework, or npm dependency.** Plain ES modules served as-is.
- **Flat asset directory.** The `/:file` route (`crates/cli/src/serve/mod.rs:135`) matches a single path segment, so `crates/cli/assets/` must have no subdirectories.
- **No `alert()`, `confirm()`, or `prompt()`.** The design replaces all three with the modal and toast components.
- **Every colour goes through a token.** No literal hex in `style.css` or in any `*.js`; only `tokens.css` holds hex values. This is what makes the theme toggle work. Two narrow exceptions, both theme-independent by intent and both audited in Task 11: the drop-shadow `rgba()` values inside the `--shadow-*` tokens in `tokens.css`, and the drop-shadow under a photo (`.detail-img`, `.cmp-img`) — a shadow cast by an image is not chrome and does not re-tint per theme. Anything else with a literal colour is a defect.
- **`font-variant-numeric: tabular-nums` globally** — counts and scores must not jitter as they update.
- **Photos never carry the accent.** Onboard cyan is chrome-only. Photo wells are `--well` / `--well-2`.
- **Defect flags are monochrome** — white monospace on a black scrim, never hued. Codes are two or three characters: `BLR`, `BF`, `OE`, `UE`, `IQA`.
- **Rejected tiles render at 42% luminance** (`opacity:.42` on the image layer only, not the chrome).
- Rust style: `anyhow::Result` at command-handler boundaries, `tracing` not `println!`, no AGPL dependencies, DuckDB only, no mutation of original photo files.
- Before declaring any task done: `cargo fmt`, `cargo clippy --all-targets --all-features -- -D warnings`, `cargo test --all` must all pass.
- Source `~/.cargo/env` before any cargo command — this shell does not auto-source it.
- Each task is one commit, conventional-commit style (`feat(ui): …`, `fix(serve): …`, `docs(…): …`).

### Design reference

`docs/design/mockups/` is a read-only snapshot of the design. **Read the relevant line range before implementing each screen** — it is the authority on exact spacing, weights, and colours, and citing it beats transcribing it.

`docs/design/mockups/README.md` has the full screen → line-range index. The `<script type="text/x-dc">` block at `Photopipe.dc.html:855-1096` holds `tileStyles()` and `stage()`, which define tile outlines, decision marks, the reject dim, and stage-dot styling.

### Cut elements — do not implement, do not stub with fake values

| Element | Screen |
|---|---|
| Per-library cull-progress bars and percentage column | 1a |
| "2,183 pairs compared · 14 clusters so far" secondary counter | 1d |
| "Skip stage" button | 1d |
| Cluster time range and "96% similar" | 1i |
| "Sort: capture time" option | 1e |
| Export destination picker ("Change"), free-space stat tile, `rejected.txt` checkbox | 1k |

If a task's mockup line range shows one of these, leave it out.

---

## File Structure

| File | Responsibility | Task |
|---|---|---|
| `crates/cli/assets/Manrope.ttf` | Embedded variable font, copied from `docs/design/Manrope.ttf` | 1 |
| `crates/cli/assets/tokens.css` | `@font-face`, light + dark custom properties, keyframes. **The only file with hex colours.** | 1 |
| `crates/cli/assets/index.html` | Shell: rail mount, view slots, modal host, toast host | 1 |
| `crates/cli/assets/app.js` | `api()`, `humanBytes()`, router, theme store, boot | 1 |
| `crates/cli/src/serve/handlers.rs` | Two extra mime arms in `static_asset` | 1 |
| `crates/cli/tests/serve.rs` | Font mime test + asset-manifest test | 1 |
| `crates/cli/assets/icons.js` | SVG path strings lifted from the mockups | 2 |
| `crates/cli/assets/style.css` | Components + view layout, all colour via tokens | 2 |
| `crates/cli/assets/rail.js` | 56px nav rail | 2 |
| `crates/cli/assets/toast.js` | Toast + banner + modal host helpers | 2 |
| `crates/cli/assets/libraries.js` | Screen 1a | 3 |
| `crates/cli/assets/picker.js` | Screen 1b | 4 |
| `crates/cli/assets/analyze.js` | Screens 1c / 1d | 5 |
| `crates/cli/assets/review.js` | Screens 1e / 1f / 1g | 6 |
| `crates/cli/assets/detail.js` | Screen 1h | 7 |
| `crates/cli/assets/duplicates.js` | Screen 1i | 8 |
| `crates/cli/assets/compare.js` | Screen 1j | 9 |
| `crates/cli/assets/export.js` | Screen 1k | 10 |

Deleted along the way: `home.js` (→ `libraries.js`, Task 3), `browse.js` (→ `picker.js`, Task 4). `review.js`, `analyze.js`, `duplicates.js`, `style.css`, `index.html`, `app.js` are rewritten in place.

### Why the split

The current `review.js` is one 5.8 KB file holding grid rendering, the detail lightbox, keyboard handling, and export. Each of those is a separate screen in the design and each grows substantially. `detail.js`, `compare.js`, and `export.js` come out of it as part of this work.

---

## Task 1: Foundation — font, tokens, shell, mime arms

Everything downstream depends on the token names defined here. No screen work in this task.

**Files:**
- Create: `crates/cli/assets/Manrope.ttf` (copy of `docs/design/Manrope.ttf`)
- Create: `crates/cli/assets/tokens.css`
- Rewrite: `crates/cli/assets/index.html`
- Rewrite: `crates/cli/assets/app.js`
- Modify: `crates/cli/src/serve/handlers.rs:40-53` (the `static_asset` mime match)
- Test: `crates/cli/tests/serve.rs` (append two tests)

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `app.js` exports `api(method, url, body) -> Promise<any>`, `humanBytes(n) -> string`, `show(view)`, `theme.get() -> 'dark'|'light'`, `theme.toggle()`, `theme.apply()`, and `state` (a mutable object with `activeFolder: string|null`).
  - `window.pp` carries every navigation entry point so screens avoid circular imports — the existing pattern in `app.js`. Keys added by later tasks: `openLibraries`, `openPicker`, `startAnalyze`, `openReview`, `openDetail`, `openDuplicates`, `openCompare`, `openExport`.
  - The full token vocabulary in `tokens.css` (listed in Step 3).
  - View ids: `view-libraries`, `view-analyze`, `view-review`, `view-duplicates`. (`picker`, `detail`, `compare`, `export` are overlays on the modal host, not views.)

- [ ] **Step 1: Write the failing tests**

Append to `crates/cli/tests/serve.rs`:

```rust
/// The embedded font must be served as a font, not application/octet-stream —
/// `static_asset` matches on extension and had no arm for `ttf`.
#[tokio::test]
async fn font_is_served_with_font_content_type() {
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    let dir = tempfile::TempDir::new().unwrap();
    let catalog = pipeline::catalog::Catalog::open(&dir.path().join("c.duckdb")).unwrap();
    let cache = pipeline::cache::Cache::open(dir.path().join("cache")).unwrap();
    let app = photopipe::serve::router(app_state_active(catalog, cache));

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/Manrope.ttf")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers()
            .get(axum::http::header::CONTENT_TYPE)
            .unwrap(),
        "font/ttf"
    );
}

/// Every asset `index.html` references must resolve through the `/:file` route.
/// Catches a renamed or forgotten module before it ships as a blank screen.
#[tokio::test]
async fn every_asset_referenced_by_index_resolves() {
    use axum::body::to_bytes;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    let dir = tempfile::TempDir::new().unwrap();
    let catalog = pipeline::catalog::Catalog::open(&dir.path().join("c.duckdb")).unwrap();
    let cache = pipeline::cache::Cache::open(dir.path().join("cache")).unwrap();
    let state = app_state_active(catalog, cache);

    let index = {
        let app = photopipe::serve::router(state.clone());
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        String::from_utf8(to_bytes(resp.into_body(), 1 << 20).await.unwrap().to_vec()).unwrap()
    };

    // Pull every root-relative href/src out of index.html.
    let mut refs: Vec<String> = Vec::new();
    for attr in ["href=\"/", "src=\"/"] {
        let mut rest = index.as_str();
        while let Some(i) = rest.find(attr) {
            rest = &rest[i + attr.len()..];
            let end = rest.find('"').expect("unterminated attribute in index.html");
            let path = &rest[..end];
            if !path.is_empty() && !path.starts_with("api/") {
                refs.push(path.to_string());
            }
            rest = &rest[end..];
        }
    }
    assert!(
        refs.len() >= 3,
        "expected index.html to reference the stylesheets and app.js, found {refs:?}"
    );

    for path in refs {
        let app = photopipe::serve::router(state.clone());
        let resp = app
            .oneshot(
                Request::builder()
                    .uri(format!("/{path}"))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "/{path} did not resolve");
    }
}
```

Note: `app_state_active` (`crates/cli/tests/serve.rs:4`) already exists. `AppState` derives `Clone`, so `state.clone()` per request is fine — `router()` consumes its state.

- [ ] **Step 2: Run the tests to verify they fail**

```bash
source ~/.cargo/env
cargo test -p photopipe --test serve font_is_served_with_font_content_type -- --nocapture
```

Expected: FAIL. `/Manrope.ttf` returns 404 (file not embedded yet). The manifest test also fails — the current `index.html` references `/style.css` and `/app.js`, which exist, but `/tokens.css` does not yet.

- [ ] **Step 3: Copy the font and write `tokens.css`**

```bash
cp docs/design/Manrope.ttf crates/cli/assets/Manrope.ttf
```

Create `crates/cli/assets/tokens.css`. This is the single source of colour for the whole app — every other file references `var(--…)`.

```css
/* Manrope, SIL Open Font License 1.1, (c) 2019 The Manrope Project Authors.
   Full licence: docs/design/Manrope-OFL.txt */
@font-face {
  font-family: "Manrope";
  font-style: normal;
  font-weight: 200 800;
  font-display: swap;
  src: url("/Manrope.ttf") format("truetype-variations"),
       url("/Manrope.ttf") format("truetype");
}

:root {
  --font: "Manrope", system-ui, sans-serif;
  --mono: ui-monospace, Menlo, Consolas, monospace;

  --radius-tile: 8px;
  --radius-ctl: 8px;
  --radius-card: 12px;
  --radius-panel: 16px;
  --radius-pill: 999px;

  --rail-w: 56px;

  --shadow-pop: 0 1px 1px rgba(3,7,18,.01), 0 5px 5px rgba(3,7,18,.02),
                0 12px 12px rgba(3,7,18,.03), 0 20px 20px rgba(3,7,18,.04),
                0 32px 32px rgba(3,7,18,.05);
  --shadow-float: 0 24px 48px rgba(0,0,0,.55);
  --shadow-toast: 0 4px 12px rgba(0,0,0,.06), 0 2px 4px rgba(0,0,0,.04);
}

/* ── Light ─────────────────────────────────────────────────────────────── */
:root[data-theme="light"] {
  --bg:            #EFEEED;
  --surface:       #FFFFFF;
  --surface-2:     #FAF9F9;
  --inset:         #F1F0F0;
  --well:          #F7F7F7;   /* grid background */
  --well-2:        #FFFFFF;   /* photo stage */

  --border:        rgba(0,0,0,.08);
  --border-strong: #D8D8D8;
  --border-faint:  rgba(0,0,0,.05);

  --text:          #0A0A0A;
  --text-muted:    #525253;
  --text-dim:      #6B6C6C;
  --text-faint:    #909191;
  --text-ghost:    #B4B4B4;

  --accent:        #007A8A;
  --accent-hover:  #005F6B;
  --accent-on:     #FFFFFF;   /* text on an accent fill */
  --accent-soft:   rgba(9,178,199,.12);
  --accent-edge:   rgba(0,122,138,.40);
  --accent-tint:   #D2F7FF;
  --accent-tint-fg:#003138;
  --accent-wash:   #F2FCFE;
  --accent-glow:   #007A8A;

  --keep:          #16A34A;
  --keep-soft:     #DCFCE7;
  --keep-fg:       #14532D;
  --reject:        #BA1A1A;
  --reject-soft:   #FFDAD6;
  --reject-fg:     #410002;
  --keeper:        #007A8A;

  --warn-edge:     rgba(182,137,0,.35);
  --warn-bg:       #FFFBEB;
  --warn-soft:     #FFF1B8;
  --warn-fg:       #2A1E00;

  --scrim:         rgba(0,0,0,.34);
  --flag-scrim:    rgba(0,0,0,.82);
  --cursor-ring:   #0A0A0A;
  --cursor-gap:    #FFFFFF;
  --hatch:         rgba(0,0,0,.07);
  --kbd-bg:        rgba(0,0,0,.06);
  --kbd-edge:      rgba(0,0,0,.14);
}

/* ── Dark (default) ────────────────────────────────────────────────────── */
:root[data-theme="dark"] {
  --bg:            #121414;
  --surface:       #1A1C1C;
  --surface-2:     #161818;
  --inset:         #0E0E0E;
  --well:          #121414;
  --well-2:        #1A1C1C;

  --border:        rgba(255,255,255,.07);
  --border-strong: rgba(255,255,255,.12);
  --border-faint:  rgba(255,255,255,.05);

  --text:          #E3E2E2;
  --text-muted:    #ABABAB;
  --text-dim:      #6E7E8B;
  --text-faint:    #4F5C68;
  --text-ghost:    #4F5C68;

  --accent:        #09B2C7;
  --accent-hover:  #51D7ED;
  --accent-on:     #00262B;
  --accent-soft:   rgba(9,178,199,.14);
  --accent-edge:   rgba(9,178,199,.45);
  --accent-tint:   rgba(9,178,199,.18);
  --accent-tint-fg:#51D7ED;
  --accent-wash:   #161818;
  --accent-glow:   #7BF0E3;

  --keep:          #4ADE80;
  --keep-soft:     rgba(74,222,128,.18);
  --keep-fg:       #B7F1C7;
  --reject:        #FF6B66;
  --reject-soft:   rgba(255,107,102,.10);
  --reject-fg:     #FF9990;
  --keeper:        #09B2C7;

  --warn-edge:     rgba(182,137,0,.45);
  --warn-bg:       rgba(182,137,0,.10);
  --warn-soft:     rgba(255,241,184,.16);
  --warn-fg:       #FFE9A3;

  --scrim:         rgba(0,0,0,.55);
  --flag-scrim:    rgba(0,0,0,.72);
  --cursor-ring:   #FFFFFF;
  --cursor-gap:    #121414;
  --hatch:         rgba(255,255,255,.07);
  --kbd-bg:        rgba(255,255,255,.07);
  --kbd-edge:      rgba(255,255,255,.14);
}

html, body {
  margin: 0;
  height: 100%;
  background: var(--bg);
  color: var(--text);
  font-family: var(--font);
  font-variant-numeric: tabular-nums;
  -webkit-font-smoothing: antialiased;
}
* { box-sizing: border-box; }
a { color: var(--accent); text-decoration: none; }
a:hover { color: var(--accent-hover); text-decoration: underline; }

@keyframes pp-indet { 0% { transform: translateX(-70%) } 100% { transform: translateX(170%) } }
@keyframes pp-shim  { 0% { opacity: .5 } 50% { opacity: 1 } 100% { opacity: .5 } }
@keyframes pp-spin  { to { transform: rotate(360deg) } }
```

- [ ] **Step 4: Write `index.html`**

The theme must be on `<html>` before first paint or the app flashes the wrong colours, so the resolving script is inline and blocking in `<head>`.

```html
<!doctype html>
<html lang="en" data-theme="dark">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>photopipe</title>
  <script>
    // Inline and blocking: set the theme before first paint to avoid a flash.
    try {
      var t = localStorage.getItem('pp-theme');
      if (t === 'light' || t === 'dark') document.documentElement.dataset.theme = t;
    } catch (e) { /* private mode — keep the dark default */ }
  </script>
  <link rel="stylesheet" href="/tokens.css">
  <link rel="stylesheet" href="/style.css">
</head>
<body>
  <div class="app">
    <nav id="rail" class="rail"></nav>
    <div class="stage">
      <div id="view-libraries"  class="view"></div>
      <div id="view-analyze"    class="view hidden"></div>
      <div id="view-review"     class="view hidden"></div>
      <div id="view-duplicates" class="view hidden"></div>
    </div>
  </div>
  <div id="modal-host"></div>
  <div id="toast-host" class="toast-host"></div>
  <script type="module" src="/app.js"></script>
</body>
</html>
```

- [ ] **Step 5: Write `app.js`**

```js
// Fetch helper, view router, theme store, and boot. Screens reach each other
// through window.pp rather than importing one another, to keep the module
// graph acyclic.

export async function api(method, url, body) {
  const opts = { method };
  if (body !== undefined) {
    opts.headers = { 'content-type': 'application/json' };
    opts.body = JSON.stringify(body);
  }
  const r = await fetch(url, opts);
  if (!r.ok) {
    const err = new Error(`${method} ${url} → ${r.status}`);
    err.status = r.status;
    throw err;
  }
  const ct = r.headers.get('content-type') || '';
  return ct.includes('application/json') ? r.json() : r.text();
}

export function humanBytes(n) {
  const u = ['B', 'KB', 'MB', 'GB', 'TB'];
  let v = n, i = 0;
  while (v >= 1024 && i < u.length - 1) { v /= 1024; i++; }
  return i ? `${v.toFixed(1)} ${u[i]}` : `${n} B`;
}

const VIEWS = ['libraries', 'analyze', 'review', 'duplicates'];

/** Mutable app state shared across screens. */
export const state = { activeFolder: null, view: 'libraries' };

export function show(view) {
  state.view = view;
  for (const v of VIEWS) {
    document.getElementById(`view-${v}`).classList.toggle('hidden', v !== view);
  }
  window.pp.renderRail();
}

export const theme = {
  get() { return document.documentElement.dataset.theme === 'light' ? 'light' : 'dark'; },
  apply(next) {
    document.documentElement.dataset.theme = next;
    try { localStorage.setItem('pp-theme', next); } catch (e) { /* private mode */ }
    window.pp.renderRail();
  },
  toggle() { this.apply(this.get() === 'dark' ? 'light' : 'dark'); },
};

window.pp = { api, humanBytes, show, state, theme, renderRail() {} };

async function boot() {
  // Later tasks register their entry points on window.pp before boot runs.
  try {
    const active = await api('GET', '/api/active');
    if (active && active.folder) {
      state.activeFolder = active.folder;
      if (window.pp.openReview) { await window.pp.openReview(active.folder); return; }
    }
  } catch (e) { /* fall through to the libraries screen */ }
  if (window.pp.openLibraries) await window.pp.openLibraries();
}

// Task 2 imports rail.js here; Tasks 3-10 add their screen imports. Keeping the
// import list in one place means index.html never changes again.
import('/rail.js').then(() => boot());
```

Note for later tasks: **append your screen's `import` to the bottom of `app.js`** and register the entry point on `window.pp` from inside your module. The final import chain is assembled in Task 11.

- [ ] **Step 6: Add the mime arms**

In `crates/cli/src/serve/handlers.rs`, extend the match in `static_asset`:

```rust
            let ct = match file.rsplit('.').next() {
                Some("js") => "text/javascript; charset=utf-8",
                Some("css") => "text/css; charset=utf-8",
                Some("html") => "text/html; charset=utf-8",
                Some("ttf") => "font/ttf",
                Some("svg") => "image/svg+xml; charset=utf-8",
                _ => "application/octet-stream",
            };
```

- [ ] **Step 7: Run the tests to verify they pass**

```bash
source ~/.cargo/env
cargo test -p photopipe --test serve font_is_served_with_font_content_type
cargo test -p photopipe --test serve every_asset_referenced_by_index_resolves
```

Expected: both PASS. The manifest test resolves `/tokens.css`, `/style.css`, `/app.js` — `style.css` still being the old file is fine, Task 2 replaces it.

- [ ] **Step 8: fmt, clippy, full test run**

```bash
source ~/.cargo/env
cargo fmt
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all
```

All must pass. Expect the existing serve tests to be untouched — no endpoint behaviour changed.

- [ ] **Step 9: Commit**

```bash
git add crates/cli/assets/Manrope.ttf crates/cli/assets/tokens.css \
        crates/cli/assets/index.html crates/cli/assets/app.js \
        crates/cli/src/serve/handlers.rs crates/cli/tests/serve.rs
git commit -m "feat(ui): design tokens, embedded Manrope, and app shell

Adds the two-theme token layer every screen resolves colour through, the
Manrope variable font the design specifies (SIL OFL 1.1), and a shell that
sets data-theme before first paint. static_asset gains ttf and svg mime
arms so the font is not served as octet-stream.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>"
```

---

## Task 2: Shared components — icons, rail, toasts, modal, component CSS

The vocabulary every screen composes from. Read `docs/design/mockups/Photopipe.dc.html:23-61` (the notes card) and `Rail.dc.html` in full before starting.

**Files:**
- Create: `crates/cli/assets/icons.js`
- Create: `crates/cli/assets/rail.js`
- Create: `crates/cli/assets/toast.js`
- Rewrite: `crates/cli/assets/style.css`
- Modify: `crates/cli/assets/app.js` (append the `toast.js` import)

**Interfaces:**
- Consumes: `app.js` — `api`, `show`, `state`, `theme`, `window.pp`.
- Produces:
  - `icons.js` exports `icon(name, size = 16) -> string` (an `<svg>` string) and `ICONS` (name → path `d`). Names: `folder`, `grid`, `layers`, `download`, `develop`, `settings`, `sun`, `moon`, `plus`, `close`, `check`, `chevron-right`, `chevron-down`, `chevron-left`, `arrow-up`, `refresh`, `filter`, `rows`, `cells`, `dense`, `expand`, `undo`, `info`, `warn`, `spark`.
  - `rail.js` registers `window.pp.renderRail()`. Reads `state.view` and `state.activeFolder`; disables Review/Duplicates/Export when no library is active; Develop is always dim with a dot.
  - `toast.js` exports and registers on `window.pp`:
    - `toast({ kind, title, body, actions }) -> () => void` — `kind` is `'success' | 'error' | 'warn' | 'info'`; `actions` is `[{ label, onClick }]`; returns a dismiss function. Auto-dismisses after 6 s for `success`/`info`, never for `error`/`warn`.
    - `banner(hostEl, { kind, title, body, actions, onDismiss })` — the inline variant (mockup 1a / 1k), appended to `hostEl`.
    - `modal({ title, subtitle, body, footer, width, onClose }) -> { el, close }` — mounts into `#modal-host` with a scrim, closes on Esc and scrim click.
    - `confirmDialog({ title, body, confirmLabel, danger }) -> Promise<boolean>` — the `confirm()` replacement.
  - CSS classes in `style.css` consumed by every later task:
    `.app .rail .stage .view .hidden`, `.btn .btn-primary .btn-ghost .btn-icon .btn-danger`,
    `.seg .seg-btn .seg-btn.on`, `.chip .chip.on`, `.kbd`, `.card .panel .panel-head .panel-body .panel-foot`,
    `.crumb`, `.stat .stat-n .stat-label`, `.bar .bar-fill .bar-fill.indet`, `.spinner`,
    `.tile .tile-img .tile-mark .tile-flag .tile-score .tile-meter .tile-dup`,
    `.tile.keep .tile.reject .tile.keeper .tile.undecided .tile.cursor`,
    `.empty .empty-icon .empty-title .empty-body`, `.skeleton`,
    `.toast-host .toast .toast.success .toast.error .toast.warn .toast.info`,
    `.modal-scrim .modal`, `.kv .kv-k .kv-v`, `.section-label`.

- [ ] **Step 1: Write `icons.js`**

Every path is lifted verbatim from the mockups. `folder`, `grid`, `layers`, `download`, `develop`, `settings` come from `Rail.dc.html`'s `ITEMS` array and its settings glyph; the rest from the screens noted in comments.

```js
// SVG paths lifted from docs/design/mockups/. Do not redraw these by hand —
// re-copy from the mockup if a glyph looks wrong.
export const ICONS = {
  // Rail (Rail.dc.html ITEMS)
  folder: 'M3 7.5A2 2 0 0 1 5 5.5h3.4l2 2H19a2 2 0 0 1 2 2v7a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z',
  grid: 'M4 4h7v7H4zM13 4h7v7h-7zM4 13h7v7H4zM13 13h7v7h-7z',
  layers: 'M12 3 3 8l9 5 9-5zM3 13l9 5 9-5',
  download: 'M12 4v11M7.5 11l4.5 4.5L16.5 11M4 20h16',
  develop: 'M12 3.5v3M12 17.5v3M3.5 12h3M17.5 12h3M6.4 6.4l2.1 2.1M15.5 15.5l2.1 2.1M17.6 6.4l-2.1 2.1M8.5 15.5l-2.1 2.1',
  settings: 'M6 4v5M6 13v7M18 4v7M18 15v5M3 11h6M15 13h6',
  // Chrome
  sun: 'M12 3v2M12 19v2M3 12h2M19 12h2M5.6 5.6l1.4 1.4M16.9 16.9l1.4 1.4M18.4 5.6L17 7M7 17l-1.4 1.4',  // 1a top bar
  moon: 'M20.5 14.6A8.5 8.5 0 0 1 9.4 3.5a8.5 8.5 0 1 0 11.1 11.1z',                                     // 1e top bar
  plus: 'M12 5v14M5 12h14',
  close: 'M6 6l12 12M18 6L6 18',
  check: 'M4 12.5l5 5 11-11',
  'chevron-right': 'M9 5l7 7-7 7',
  'chevron-down': 'M6 9l6 6 6-6',
  'chevron-left': 'M14 6l-6 6 6 6',
  'arrow-up': 'M12 19V6M6 12l6-6 6 6',
  refresh: 'M4 12a8 8 0 0 1 13.7-5.6M20 12a8 8 0 0 1-13.7 5.6M17 4v3h-3M7 20v-3h3',
  filter: 'M4 6h16l-6 7v6l-4-2v-4z',
  // Density (1e)
  rows: 'M4 5h16M4 12h16M4 19h16',
  cells: 'M4 4h7v7H4zM13 4h7v7h-7zM4 13h7v7H4zM13 13h7v7h-7z',
  dense: 'M4 4h4v4H4zM10 4h4v4h-4zM16 4h4v4h-4zM4 10h4v4H4zM10 10h4v4h-4zM16 10h4v4h-4zM4 16h4v4H4zM10 16h4v4h-4zM16 16h4v4h-4z',
  expand: 'M4 9V4h5M20 15v5h-5M15 4h5v5M9 20H4v-5',      // 1h fullscreen
  undo: 'M9 5L4 10l5 5M4 10h10a6 6 0 0 1 0 12h-3',        // 1i
  info: 'M12 8h.01M11 12h1v5h1',                          // 1i, pair with a circle
  warn: 'M12 9v4M12 16.5v.01M10.3 4l-7 12A2 2 0 0 0 5 19h14a2 2 0 0 0 1.7-3l-7-12a2 2 0 0 0-3.4 0z',
  error: 'M12 8v5M12 16.5v.01',                            // 1k, pair with a circle
  spark: 'M12 3 3 8l9 5 9-5zM3 13l9 5 9-5',
};

/** Icons that need a circle drawn alongside their path. */
const CIRCLED = new Set(['info', 'error']);

export function icon(name, size = 16, strokeWidth = 1.8) {
  const d = ICONS[name];
  if (!d) throw new Error(`icons.js: unknown icon "${name}"`);
  const circle = CIRCLED.has(name) ? '<circle cx="12" cy="12" r="9"></circle>' : '';
  return `<svg width="${size}" height="${size}" viewBox="0 0 24 24" fill="none"
    stroke="currentColor" stroke-width="${strokeWidth}" stroke-linecap="round"
    stroke-linejoin="round" aria-hidden="true">${circle}<path d="${d}"></path></svg>`;
}
```

- [ ] **Step 2: Write `rail.js`**

```js
import { state, show } from '/app.js';
import { icon } from '/icons.js';

const ITEMS = [
  { id: 'libraries',  label: 'Libraries',  ico: 'folder',   go: () => window.pp.openLibraries() },
  { id: 'review',     label: 'Review',     ico: 'grid',     needsLib: true, go: () => window.pp.openReview(state.activeFolder) },
  { id: 'duplicates', label: 'Duplicates', ico: 'layers',   needsLib: true, go: () => window.pp.openDuplicates(state.activeFolder) },
  { id: 'export',     label: 'Export',     ico: 'download', needsLib: true, go: () => window.pp.openExport() },
  { id: 'develop',    label: 'Develop — not yet available', ico: 'develop', soon: true },
];

function renderRail() {
  const el = document.getElementById('rail');
  if (!el) return;
  const hasLib = !!state.activeFolder;
  el.innerHTML = `
    <div class="rail-mark" title="photopipe">
      <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor"
           stroke-width="1.8" stroke-linecap="round" aria-hidden="true">
        <circle cx="12" cy="12" r="9.2"></circle>
        <path d="M8.4 8.4l7.2 7.2M15.6 8.4l-7.2 7.2"></path>
      </svg>
    </div>
    ${ITEMS.map(it => {
      const disabled = it.soon || (it.needsLib && !hasLib);
      const cls = ['rail-cell',
        it.soon ? 'soon' : '',
        state.view === it.id ? 'on' : '',
        disabled ? 'disabled' : ''].filter(Boolean).join(' ');
      return `<button class="${cls}" data-id="${it.id}" title="${it.label}"
                ${disabled ? 'disabled' : ''} aria-label="${it.label}">
                ${icon(it.ico, 18, 1.7)}${it.soon ? '<span class="rail-dot"></span>' : ''}
              </button>`;
    }).join('')}
    <div class="rail-gap"></div>
    <div class="rail-cell inert" title="Settings">${icon('settings', 18, 1.7)}</div>`;

  for (const btn of el.querySelectorAll('.rail-cell[data-id]')) {
    const it = ITEMS.find(i => i.id === btn.dataset.id);
    if (it && it.go) btn.onclick = () => it.go();
  }
}

window.pp.renderRail = renderRail;
renderRail();
```

Note: the rail's `on` state keys off `state.view`, and the Export rail item opens the export modal without changing `state.view` — so Export never shows as the active cell. That matches the design, where Export is a dialog, not a screen.

- [ ] **Step 3: Write `toast.js`**

```js
import { icon } from '/icons.js';

const HOST = () => document.getElementById('toast-host');
const KIND_ICON = { success: 'check', error: 'error', warn: 'warn', info: 'refresh' };
const AUTO_MS = { success: 6000, info: 6000 };

function actionsHtml(actions) {
  return (actions || [])
    .map((a, i) => `<button class="btn btn-ghost sm" data-act="${i}">${a.label}</button>`)
    .join('');
}

function wireActions(el, actions, close) {
  for (const b of el.querySelectorAll('[data-act]')) {
    const a = actions[Number(b.dataset.act)];
    b.onclick = () => { const keep = a.onClick && a.onClick(); if (!keep) close(); };
  }
  const x = el.querySelector('.notice-x');
  if (x) x.onclick = close;
}

function noticeHtml(kind, title, body, actions, inline) {
  return `<span class="notice-ico">${icon(KIND_ICON[kind] || 'info', 15, 2.1)}</span>
    <div class="notice-text">
      <div class="notice-title">${title}</div>
      ${body ? `<div class="notice-body">${body}</div>` : ''}
      ${inline && actions && actions.length ? `<div class="notice-acts">${actionsHtml(actions)}</div>` : ''}
    </div>
    ${!inline ? actionsHtml(actions) : ''}
    <button class="notice-x" aria-label="Dismiss">${icon('close', 12, 2.2)}</button>`;
}

/** Floating toast. Returns a dismiss function. */
export function toast({ kind = 'info', title, body, actions }) {
  const el = document.createElement('div');
  el.className = `toast ${kind}`;
  el.innerHTML = noticeHtml(kind, title, body, actions, false);
  HOST().appendChild(el);
  let done = false;
  const close = () => { if (done) return; done = true; el.remove(); };
  wireActions(el, actions || [], close);
  const ms = AUTO_MS[kind];
  if (ms) setTimeout(close, ms);
  return close;
}

/** Inline banner (mockup 1a / 1k). Appended to hostEl. */
export function banner(hostEl, { kind = 'info', title, body, actions, onDismiss }) {
  const el = document.createElement('div');
  el.className = `notice ${kind}`;
  el.innerHTML = noticeHtml(kind, title, body, actions, true);
  hostEl.appendChild(el);
  const close = () => { el.remove(); if (onDismiss) onDismiss(); };
  wireActions(el, actions || [], close);
  return close;
}

/** Scrim + panel in #modal-host. Closes on Esc and scrim click. */
export function modal({ title, subtitle, body, footer, width = 520, onClose }) {
  const scrim = document.createElement('div');
  scrim.className = 'modal-scrim';
  scrim.innerHTML = `<div class="modal" style="width:${width}px" role="dialog" aria-modal="true">
      ${title ? `<div class="modal-head">
        <div class="modal-title">${title}</div>
        ${subtitle ? `<div class="modal-sub">${subtitle}</div>` : ''}
      </div>` : ''}
      <div class="modal-body"></div>
      ${footer ? '<div class="modal-foot"></div>' : ''}
    </div>`;

  const panel = scrim.querySelector('.modal');
  const bodyEl = scrim.querySelector('.modal-body');
  if (typeof body === 'string') bodyEl.innerHTML = body;
  else if (body) bodyEl.appendChild(body);
  if (footer) {
    const f = scrim.querySelector('.modal-foot');
    if (typeof footer === 'string') f.innerHTML = footer;
    else f.appendChild(footer);
  }

  let done = false;
  const close = () => {
    if (done) return;
    done = true;
    document.removeEventListener('keydown', onKey, true);
    scrim.remove();
    if (onClose) onClose();
  };
  const onKey = (e) => { if (e.key === 'Escape') { e.stopPropagation(); close(); } };
  document.addEventListener('keydown', onKey, true);
  scrim.onclick = (e) => { if (e.target === scrim) close(); };
  panel.onclick = (e) => e.stopPropagation();

  document.getElementById('modal-host').appendChild(scrim);
  return { el: panel, body: bodyEl, close };
}

/** confirm() replacement. Resolves true on confirm, false on cancel/Esc. */
export function confirmDialog({ title, body, confirmLabel = 'Continue', danger = false }) {
  return new Promise((resolve) => {
    let settled = false;
    const finish = (v) => { if (settled) return; settled = true; resolve(v); m.close(); };
    const m = modal({
      title,
      body: `<p class="modal-copy">${body}</p>`,
      footer: `<div class="modal-foot-row">
          <button class="btn" data-no>Cancel</button>
          <button class="btn ${danger ? 'btn-danger' : 'btn-primary'}" data-yes>${confirmLabel}
            <span class="kbd">↵</span></button>
        </div>`,
      width: 460,
      onClose: () => { if (!settled) { settled = true; resolve(false); } },
    });
    m.el.querySelector('[data-no]').onclick = () => finish(false);
    m.el.querySelector('[data-yes]').onclick = () => finish(true);
    m.el.querySelector('[data-yes]').focus();
  });
}

Object.assign(window.pp, { toast, banner, modal, confirmDialog });
```

- [ ] **Step 4: Write `style.css`**

Replace the file entirely. Read `docs/design/mockups/Photopipe.dc.html:855-1096` first — `tileStyles()` there is the authority for tile outlines, decision marks, and the reject dim.

Required rules, all colour via `var(--…)`:

```css
/* ── Shell ─────────────────────────────────────────────────────────────── */
.app { display: flex; height: 100vh; overflow: hidden; }
.stage { flex: 1; min-width: 0; display: flex; flex-direction: column; }
.view { flex: 1; min-height: 0; display: flex; flex-direction: column; }
.view.hidden { display: none; }

/* ── Rail (Rail.dc.html) ───────────────────────────────────────────────── */
.rail {
  width: var(--rail-w); flex: none; display: flex; flex-direction: column;
  align-items: center; gap: 4px; padding: 14px 0 12px;
  border-right: 1px solid var(--border); background: var(--surface);
}
:root[data-theme="dark"] .rail { background: var(--bg); }
.rail-mark {
  width: 38px; height: 38px; display: flex; align-items: center;
  justify-content: center; margin-bottom: 10px; color: var(--accent);
}
.rail-cell {
  position: relative; width: 38px; height: 36px; border: 0; border-radius: var(--radius-ctl);
  display: flex; align-items: center; justify-content: center;
  background: transparent; color: var(--text-muted); cursor: pointer;
}
.rail-cell:hover:not(.disabled):not(.inert) { background: var(--kbd-bg); }
.rail-cell.on { background: var(--accent-tint); color: var(--accent-tint-fg); }
.rail-cell.disabled, .rail-cell.soon { color: var(--text-ghost); cursor: default; }
.rail-cell.inert { cursor: default; }
.rail-dot { position: absolute; top: 5px; right: 5px; width: 5px; height: 5px;
  border-radius: var(--radius-pill); background: var(--text-ghost); }
.rail-gap { flex: 1; }
```

Then, following the mockups' literal values, write the remaining groups. Each must exist because later tasks reference the class names:

1. **Top bar** — `.topbar` (13px 20px, 1px bottom border), `.topbar-crumb`, `.topbar-sep`, `.topbar-title`, `.topbar-gap`. Mockup 1a:72-83 (light), 1e:267-278 (dark).
2. **Buttons** — `.btn` (9px 13px, `--radius-ctl`, 600 13px, `--border-strong`, `--surface`), `.btn-primary` (`--accent` fill, `--accent-on` text), `.btn-ghost` (no border, transparent), `.btn-danger` (`--reject` edge/text), `.btn-icon` (32×32 square), `.btn.sm` (7px 11px, 12.5px), `.btn.on` (accent-soft fill + accent-edge border + `box-shadow: 0 0 0 3px var(--accent-soft)`). Mockup 1a:77-82, 1e:314-320, 1f:410-412.
3. **Segmented control** — `.seg` (inline-flex, 1px border, overflow hidden, `--surface`), `.seg-btn` (32×30 or 7px 11px), `.seg-btn.on` (`--accent-tint` / `--accent-tint-fg`). Mockup 1e:323-327, 1h:519-523.
4. **Chips and kbd** — `.chip` (6px 8px, `--radius-ctl`), `.chip.on` (accent-soft + accent-edge), `.chip-code` (700 10px `--mono`, `--kbd-bg` fill), `.kbd` (min-width 20px, height 20px, `--kbd-bg`, `--kbd-edge`, 700 11px `--mono`). Mockup 1e:304-309, 1h:539-544.
5. **Cards and panels** — `.card` (`--surface`, 1px `--border-strong`, `--radius-panel`), `.panel`/`.panel-head`/`.panel-body`/`.panel-foot` (foot on `--surface-2`), `.section-label` (600 11px, `.04em`, uppercase, `--text-muted`), `.modal-copy`. Mockup 1c:191-222, 1k:738-777.
6. **Stats** — `.stat` (1px `--border-strong`, `--radius-card`, radial accent wash at 0% 0%), `.stat-n` (700 24px, -.02em), `.stat-label` (500 11.5px, `--text-muted`). Mockup 1k:752-765.
7. **Progress** — `.bar` (6px, `--radius-pill`, `--inset` track), `.bar-fill` (`--accent`), `.bar-fill.indet` (38% wide, accent gradient band, `pp-indet 1400ms cubic-bezier(.33,1,.68,1) infinite`), `.spinner` (26px, 2.5px ring, `pp-spin 900ms linear infinite`), `.decide-bar` (10px, three segments: keep / reject / hatched remainder using `repeating-linear-gradient(135deg, var(--hatch) 0 4px, transparent 4px 8px)`). Mockup 1c:211-214, 1d:246-250, 1e:292-301.
8. **Stage checklist** — `.stage-row`, `.stage-dot` (20px circle), `.stage-dot.done` (`--keep-soft`/`--keep-fg`), `.stage-dot.active` (`--accent-tint`/`--accent-tint-fg` plus `box-shadow: 0 0 0 4px var(--accent-soft)`), `.stage-dot.pending` (`--inset`/`--text-faint`), `.stage-title`, `.stage-title.pending`, `.stage-meta`. Mockup helper at 917-934.
9. **Tiles** — the core of the grid. `.tile` (position relative, `--radius-tile`, overflow hidden, `--well-2` background, `outline-offset: -1px`), `.tile-img` (absolute inset 0, `object-fit: cover`), `.tile.undecided` (`outline: 1px solid var(--border-strong)`), `.tile.keep` (`outline: 2px solid var(--keep)`), `.tile.reject` (`outline: 2px solid var(--reject)`), `.tile.keeper` (`outline: 2px solid var(--keeper)`), `.tile.reject .tile-img { opacity: .42 }`, `.tile.cursor` (`box-shadow: 0 0 0 2px var(--cursor-gap), 0 0 0 4px var(--cursor-ring)`), `.tile-mark` (19px rounded square, top-right; `.keep`/`.reject`/`.keeper` variants), `.tile-foot` (30px bottom gradient scrim), `.tile-flag` (700 9.5px `--mono` on `--flag-scrim`), `.tile-score` (700 10px `--mono`), `.tile-meter` (2.5px bottom bar), `.tile-dup` (top-left pill), `.tile-tag` (1i's `Suggested best` dashed pill / `★ Keeper` / `Rejected` variants). Mockup 868-889 and 1022-1036.
10. **Grid density** — `.grid` with `--cols` custom property: `grid-template-columns: repeat(var(--cols), 1fr); gap: 10px`. Density levels set `--cols` to 5 / 8 / 12.
11. **Empty states and skeletons** — `.empty` (centred column, gap 14px), `.empty-icon` (46px rounded square), `.empty-title` (700 17px), `.empty-body` (400 13px, `--text-muted`, max-width 300px), `.skeleton` (`--radius-tile`, `--kbd-bg`, `pp-shim 1400ms ease-in-out infinite`). Mockup 1g:464-506.
12. **Notices, toasts, modals** — `.toast-host` (fixed, bottom-right, column, gap 12px, `z-index: 200`, `pointer-events: none`; children `pointer-events: auto`), `.toast` (`--surface`, `--radius-card`, `--shadow-toast`), `.notice` (the inline variant), per-kind edges/washes for `.success` / `.error` / `.warn` / `.info` using the `--keep-*` / `--reject-*` / `--warn-*` / `--accent-*` tokens, `.notice-ico` (26px rounded square), `.notice-title` (600 13.5px), `.notice-body` (400 12.5px, `--text-muted`), `.notice-acts`, `.notice-x` (24px ghost). `.modal-scrim` (fixed inset 0, `--scrim`, `backdrop-filter: blur(3px)`, centred, `z-index: 150`), `.modal` (`--surface`, `--radius-panel`, `--shadow-pop`, overflow hidden), `.modal-head` / `.modal-body` / `.modal-foot` / `.modal-foot-row`. Mockup 1b:138-176, 1k:780-821.
13. **Key/value grid** — `.kv` (`grid-template-columns: auto 1fr`, gap 9px 16px), `.kv-k` (500 12px, `--text-dim`), `.kv-v` (600 12.5px, right-aligned). Mockup 1h:595-600.
14. **Filter bar** — `.filterbar` (12px 20px, 1px bottom border, `--surface-2`; dark uses `--surface-2` too). Mockup 1e:313-329.
15. **Menu popover** — `.menu` (266px, `--surface`, `--shadow-pop`, `--radius-card`), `.menu-head`, `.menu-row`, `.menu-row.on` (accent wash), `.menu-box` (16px checkbox), `.menu-foot`. Mockup 1f:442-457.

- [ ] **Step 5: Append the toast import to `app.js`**

Change the bottom of `app.js` from `import('/rail.js').then(() => boot());` to:

```js
Promise.all([import('/rail.js'), import('/toast.js')]).then(() => boot());
```

- [ ] **Step 6: Verify the assets resolve**

```bash
source ~/.cargo/env
cargo test -p photopipe --test serve every_asset_referenced_by_index_resolves
```

Expected: PASS. This test only checks `index.html` references; `icons.js`, `rail.js`, and `toast.js` are reached by dynamic import, so also confirm them directly:

```bash
cargo build --release
./target/release/photopipe serve --port 8899 &
sleep 2
for f in tokens.css style.css app.js icons.js rail.js toast.js Manrope.ttf; do
  printf '%s ' "$f"
  curl -s -o /dev/null -w '%{http_code} %{content_type}\n' "http://127.0.0.1:8899/$f"
done
kill %1
```

Expected: `200` for all seven, `font/ttf` for `Manrope.ttf`.

- [ ] **Step 7: Verify the rail renders in the browser**

Start the server, open `http://127.0.0.1:8899/` with the Playwright MCP browser, and confirm:
- The rail shows six cells: mark, Libraries, Review, Duplicates, Export, Develop, then the settings glyph pinned to the bottom.
- Review / Duplicates / Export are disabled (no library active on a fresh start).
- Develop is dim and carries its dot.
- `document.documentElement.dataset.theme` is `dark`; setting it to `light` restyles the rail.
- The browser console has **no errors**. Check with `browser_console_messages`.

- [ ] **Step 8: fmt, clippy, full test run**

```bash
source ~/.cargo/env
cargo fmt
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all
```

- [ ] **Step 9: Commit**

```bash
git add crates/cli/assets/icons.js crates/cli/assets/rail.js \
        crates/cli/assets/toast.js crates/cli/assets/style.css \
        crates/cli/assets/app.js
git commit -m "feat(ui): nav rail, toast/modal host, and the component CSS layer

Shared vocabulary the screens compose from: icon paths lifted from the
mockups, the 56px rail with Develop drawn-but-disabled, a toast/banner/modal
host that replaces alert() and confirm(), and the component styles. All
colour resolves through tokens so the theme toggle restyles everything.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>"
```

---

## Task 3: Libraries screen (1a)

**Files:**
- Create: `crates/cli/assets/libraries.js`
- Delete: `crates/cli/assets/home.js`
- Modify: `crates/cli/assets/app.js` (add the import)

Read `docs/design/mockups/Photopipe.dc.html:67-131`.

**Interfaces:**
- Consumes: `api`, `show`, `state` from `app.js`; `icon` from `icons.js`; `banner`, `toast` from `window.pp`.
- Produces: `window.pp.openLibraries()`.

**Cut from this screen:** the "Cull progress" column and its percentage. The table is `Folder | Photos | Analyzed | ›`.

- [ ] **Step 1: Write `libraries.js`**

```js
import { api, show, state } from '/app.js';
import { icon } from '/icons.js';

// Each divisor is paired with the unit it converts INTO, not the unit it
// converts from — dividing seconds by 60 yields minutes, so that row is
// labelled 'minute'. Pairing it with 'second' makes every label one
// conversion step stale ("1 hour ago" for a day-old library).
function relTime(unixSecs) {
  if (!unixSecs) return 'never';
  const s = Math.max(0, Math.floor(Date.now() / 1000 - unixSecs));
  const steps = [[60, 'minute'], [60, 'hour'], [24, 'day'], [7, 'week'], [4.35, 'month'], [12, 'year']];
  let v = s, unit = 'second';
  for (const [span, name] of steps) {
    if (v < span) break;
    v = v / span; unit = name;
  }
  if (unit === 'second' && v < 45) return 'moments ago';
  const n = Math.round(v);
  return `${n} ${unit}${n === 1 ? '' : 's'} ago`;
}

export async function openLibraries() {
  state.activeFolder = null;
  show('libraries');
  const el = document.getElementById('view-libraries');
  el.innerHTML = `
    <div class="topbar">
      <span class="topbar-crumb">photopipe</span>
      <span class="topbar-sep">/</span>
      <span class="topbar-title">Libraries</span>
      <div class="topbar-gap"></div>
      <button class="btn btn-icon" id="lib-theme" aria-label="Toggle theme"></button>
      <button class="btn btn-primary" id="lib-analyze">${icon('plus', 14, 2)}Analyze folder</button>
    </div>
    <div class="pad-page">
      <div id="lib-banners"></div>
      <div class="page-head">
        <div class="page-title">Libraries</div>
        <div class="page-sub" id="lib-sub"></div>
      </div>
      <div class="panel" id="lib-panel">
        <div class="lib-row lib-head">
          <div>Folder</div><div class="r">Photos</div><div class="r">Analyzed</div><div></div>
        </div>
        <div id="lib-rows"><div class="lib-empty">Loading…</div></div>
        <button class="lib-add" id="lib-add">
          <span class="lib-add-ico">${icon('plus', 15, 2)}</span>
          <span>Analyze a new folder…</span>
        </button>
      </div>
    </div>`;

  const themeBtn = document.getElementById('lib-theme');
  const paintTheme = () => {
    themeBtn.innerHTML = icon(window.pp.theme.get() === 'dark' ? 'moon' : 'sun', 15, 1.7);
  };
  paintTheme();
  themeBtn.onclick = () => { window.pp.theme.toggle(); paintTheme(); };

  const openPicker = () => window.pp.openPicker(null);
  document.getElementById('lib-analyze').onclick = openPicker;
  document.getElementById('lib-add').onclick = openPicker;

  await loadLibraries();
}

async function loadLibraries() {
  const rows = document.getElementById('lib-rows');
  const sub = document.getElementById('lib-sub');
  let libs;
  try {
    libs = await api('GET', '/api/libraries');
  } catch (e) {
    rows.innerHTML = '<div class="lib-empty">Could not read your libraries.</div>';
    window.pp.toast({ kind: 'error', title: 'Failed to load libraries', body: e.message });
    return;
  }

  if (!libs.length) {
    sub.textContent = 'No analyzed folders yet';
    rows.innerHTML = `<div class="lib-empty">Point photopipe at a folder of RAW files and it
      will flag the obvious failures before you look at a single frame.</div>`;
    return;
  }

  const total = libs.reduce((a, l) => a + l.photo_count, 0);
  const newest = libs.reduce((a, l) => Math.max(a, l.last_analyzed || 0), 0);
  sub.textContent = `${libs.length} analyzed folder${libs.length === 1 ? '' : 's'} · ` +
    `${total.toLocaleString()} photos · last session ${relTime(newest)}`;

  rows.innerHTML = '';
  for (const l of libs) {
    const name = l.folder.replace(/[\\/]+$/, '').split(/[\\/]/).pop() || l.folder;
    const row = document.createElement('button');
    row.className = 'lib-row';
    row.innerHTML = `
      <div class="lib-name-cell">
        <span class="lib-ico">${icon('folder', 16, 1.7)}</span>
        <div class="lib-name-text">
          <div class="lib-name">${name}</div>
          <div class="lib-path">${l.folder}</div>
        </div>
      </div>
      <div class="r lib-count">${l.photo_count.toLocaleString()}</div>
      <div class="r lib-when">${relTime(l.last_analyzed)}</div>
      <div class="lib-chev">${icon('chevron-right', 16, 1.8)}</div>`;
    row.onclick = () => openLibrary(l.folder);
    rows.appendChild(row);
  }
}

async function openLibrary(folder) {
  let res;
  try {
    res = await api('POST', '/api/open', { folder });
  } catch (e) {
    if (e.status === 409) { window.pp.startAnalyze(folder, { resume: true }); return; }
    window.pp.toast({ kind: 'error', title: 'Could not open that library', body: e.message });
    return;
  }
  state.activeFolder = res.folder;
  await window.pp.openReview(res.folder, { pendingNew: res.pending_new });
}

Object.assign(window.pp, { openLibraries });
```

`pending_new` is passed through to Review, which renders the re-analyze banner — the banner belongs on the screen the user lands on, as in mockup 1a where it sits above the table only because Libraries *is* the landing screen. Rendering it in both places would double it.

- [ ] **Step 2: Add the CSS for this screen**

Append to `style.css`, colours via tokens: `.pad-page` (22px 24px, column, gap 18px, `overflow: auto`), `.page-head`, `.page-title` (700 24px, -.02em), `.page-sub` (400 13px, `--text-muted`), `.lib-row` (`display: grid; grid-template-columns: 1fr 108px 150px 40px; align-items: center; padding: 14px 16px; border-bottom: 1px solid var(--border-faint)`; as a `<button>`: `width:100%; text-align:left; background:transparent; border:0; color:inherit; cursor:pointer; font:inherit`), `.lib-row:hover` (`--surface-2`), `.lib-head` (`--surface-2`, 600 11px uppercase `.04em`, `--text-muted`, `padding: 11px 16px`), `.r { text-align: right }`, `.lib-name-cell` (flex, gap 11px, `min-width: 0`), `.lib-ico` (30px rounded square, `--accent-soft`, `--accent`), `.lib-name` (600 13.5px, ellipsis), `.lib-path` (500 11.5px `--mono`, `--text-dim`, ellipsis), `.lib-count` (600 13px), `.lib-when` (500 12.5px, `--text-muted`), `.lib-chev` (`--text-dim`, right-aligned flex), `.lib-add` (flex row, 14px 16px, `--accent`, transparent border, cursor pointer), `.lib-add-ico` (30px, 1.5px dashed `--accent-edge`), `.lib-empty` (padding 24px 16px, `--text-muted`, 400 13px).

- [ ] **Step 3: Register the import**

In `app.js`, extend the bottom import list:

```js
Promise.all([
  import('/rail.js'), import('/toast.js'), import('/libraries.js'),
]).then(() => boot());
```

- [ ] **Step 4: Delete the old module**

```bash
git rm crates/cli/assets/home.js
```

- [ ] **Step 5: Verify in the browser**

```bash
source ~/.cargo/env && cargo build --release
./target/release/photopipe serve --port 8899
```

With the Playwright MCP browser at `http://127.0.0.1:8899/`:
- The Libraries table lists every analyzed folder with folder name, full path, photo count, and relative analyzed time.
- There is **no** cull-progress column.
- The theme button swaps between moon and sun glyphs and restyles the screen; reload keeps the choice.
- "Analyze folder" and "Analyze a new folder…" both attempt to open the picker. Until Task 4 lands, `window.pp.openPicker` is undefined — a console `TypeError` here is expected and acceptable for this task only.
- Take a screenshot in both themes.
- Console shows no errors other than the expected missing `openPicker`.

- [ ] **Step 6: fmt, clippy, full test run, commit**

```bash
source ~/.cargo/env
cargo fmt
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all
git add -A crates/cli/assets
git commit -m "feat(ui): Libraries screen in the new design language

Replaces home.js. Folder / Photos / Analyzed table with the theme toggle in
the top bar. The mockup's cull-progress column is omitted: /api/libraries
returns no verdict counts and rendering it would mean opening every catalog.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>"
```

---

## Task 4: Folder picker (1b)

**Files:**
- Create: `crates/cli/assets/picker.js`
- Delete: `crates/cli/assets/browse.js`
- Modify: `crates/cli/assets/app.js`, `crates/cli/assets/style.css`

Read `docs/design/mockups/Photopipe.dc.html:133-178`.

**Interfaces:**
- Consumes: `api` from `app.js`; `icon` from `icons.js`; `modal`, `toast` from `window.pp`.
- Produces: `window.pp.openPicker(path)` — `path` is `null` for the root listing.

The "Analyzed" badge is **truthful, not decorative**: `/api/libraries` returns the folder path of every analyzed library, so cross-referencing gives a real answer.

- [ ] **Step 1: Write `picker.js`**

```js
import { api } from '/app.js';
import { icon } from '/icons.js';

// Folder paths of every analyzed library, normalised for comparison. Fetched
// once per picker session so each row can carry a truthful "Analyzed" badge.
let analyzed = new Set();

const norm = (p) => p.replace(/[\\/]+$/, '').toLowerCase();

export async function openPicker(startPath) {
  try {
    const libs = await api('GET', '/api/libraries');
    analyzed = new Set(libs.map(l => norm(l.folder)));
  } catch (e) {
    analyzed = new Set();   // badge is a nicety; a failure must not block browsing
  }

  const m = window.pp.modal({
    title: 'Choose a folder to analyze',
    subtitle: 'photopipe reads RAW files in place. Nothing is moved or written.',
    width: 720,
    body: `
      <div class="picker-nav">
        <button class="btn btn-icon sm" id="pk-up" title="Up one level">${icon('arrow-up', 15, 1.9)}</button>
        <div class="picker-crumbs" id="pk-crumbs"></div>
      </div>
      <div class="picker-list" id="pk-list"></div>`,
    footer: `
      <div class="picker-foot">
        <div class="picker-cur">
          <div class="section-label">Current folder</div>
          <div class="picker-cur-path" id="pk-cur">—</div>
        </div>
        <button class="btn" id="pk-cancel">Cancel</button>
        <button class="btn btn-primary" id="pk-go" disabled>Analyze</button>
      </div>`,
  });

  let cur = null;

  m.el.querySelector('#pk-cancel').onclick = () => m.close();

  const load = async (path) => {
    const list = m.el.querySelector('#pk-list');
    list.innerHTML = '<div class="picker-loading">Reading…</div>';
    let listing;
    try {
      const q = path ? `?path=${encodeURIComponent(path)}` : '';
      listing = await api('GET', `/api/fs${q}`);
    } catch (e) {
      list.innerHTML = '<div class="picker-loading">Cannot read that folder.</div>';
      window.pp.toast({ kind: 'error', title: 'Cannot read folder', body: e.message });
      return;
    }

    cur = listing.path || null;
    m.el.querySelector('#pk-cur').textContent = cur || 'Pick a drive or location';

    // Breadcrumbs from the path itself; the last segment is the current folder.
    // A POSIX root ("/") has no non-empty segments, so it needs its own case —
    // otherwise filter(Boolean) yields [] and the strip renders blank.
    const crumbs = m.el.querySelector('#pk-crumbs');
    if (cur) {
      const parts = cur.split(/[\\/]/).filter(Boolean);
      crumbs.innerHTML = parts.length
        ? parts.map((p, i) =>
            `<span class="crumb-seg${i === parts.length - 1 ? ' on' : ''}">${p}</span>`
          ).join('<span class="crumb-sep">/</span>')
        : '<span class="crumb-seg on">/</span>';
    } else {
      crumbs.innerHTML = '<span class="crumb-seg">Locations</span>';
    }

    const up = m.el.querySelector('#pk-up');
    up.disabled = !listing.parent;
    up.onclick = () => load(listing.parent);

    const go = m.el.querySelector('#pk-go');
    const total = listing.entries.reduce((a, e) => a + e.photo_count, 0);
    go.disabled = !cur;
    go.textContent = cur ? 'Analyze this folder' : 'Analyze';
    go.onclick = () => { m.close(); window.pp.startAnalyze(cur); };

    list.innerHTML = '';
    if (!listing.entries.length) {
      list.innerHTML = '<div class="picker-loading">No sub-folders here.</div>';
      return;
    }
    for (const e of listing.entries) {
      const done = analyzed.has(norm(e.path));
      const row = document.createElement('button');
      row.className = 'picker-row' + (e.photo_count === 0 ? ' quiet' : '');
      row.innerHTML = `
        <span class="picker-ico">${icon('folder', 17, 1.7)}</span>
        <span class="picker-name">${e.name}</span>
        ${done ? '<span class="pill pill-done">Analyzed</span>' : ''}
        <span class="picker-count">${e.photo_count ? `${e.photo_count.toLocaleString()} photos` : 'no photos'}</span>
        <span class="picker-chev">${icon('chevron-right', 15, 1.8)}</span>`;
      row.onclick = () => load(e.path);
      list.appendChild(row);
    }
  };

  await load(startPath || null);
}

Object.assign(window.pp, { openPicker });
```

Note the deliberate difference from the mockup: the primary button reads "Analyze this folder" rather than "Analyze 800 photos", because `/api/fs` counts photos in the *listed children*, not in the current folder itself — a count there would be wrong.

- [ ] **Step 2: Add the CSS**

Append to `style.css`: `.picker-nav` (flex, gap 8px, 12px 20px, bottom border, `--surface-2`), `.picker-crumbs` (flex, gap 4px, wrap, 500 12.5px `--mono`), `.crumb-seg` (5px 7px, `--radius-tile`, `--text-dim`), `.crumb-seg.on` (`--accent-soft`, `--accent`, weight 700), `.crumb-sep` (`--text-ghost`), `.picker-list` (height 360px, `overflow: auto`, padding 6px 8px), `.picker-row` (flex, gap 11px, 11px 12px, `--radius-card`, full-width transparent button, `font: inherit`, `color: inherit`, cursor pointer), `.picker-row:hover` (`--accent-soft`), `.picker-row.quiet .picker-ico` (`--text-ghost`), `.picker-ico` (`--text-dim`), `.picker-name` (flex 1, 600 13.5px, ellipsis), `.picker-count` (500 12.5px, `--text-dim`, min-width 96px, right), `.picker-chev` (`--text-ghost`), `.pill` (700 10px, `.04em`, uppercase, 4px 7px, `--radius-pill`), `.pill-done` (`--keep-soft`, `--keep-fg`), `.picker-foot` (flex, gap 14px, align center), `.picker-cur` (flex 1, `min-width: 0`), `.picker-cur-path` (500 12.5px `--mono`, ellipsis, margin-top 5px), `.picker-loading` (padding 24px 12px, `--text-muted`, 400 13px).

- [ ] **Step 3: Register the import, delete the old module**

```js
Promise.all([
  import('/rail.js'), import('/toast.js'), import('/libraries.js'), import('/picker.js'),
]).then(() => boot());
```

```bash
git rm crates/cli/assets/browse.js
```

- [ ] **Step 4: Verify in the browser**

Build, serve, and with Playwright:
- "Analyze folder" opens the modal over a blurred scrim.
- The root listing shows locations; clicking a folder descends; the up button ascends and is disabled at the root.
- Breadcrumbs track the path with the last segment accented.
- A folder you have already analyzed shows the green "Analyzed" pill; one you have not does not. **Verify against a real analyzed library** — this is the claim most likely to be silently wrong.
- Esc closes the modal; clicking the scrim closes it; clicking inside does not.
- Screenshot in both themes. No console errors.

- [ ] **Step 5: fmt, clippy, test, commit**

```bash
source ~/.cargo/env
cargo fmt && cargo clippy --all-targets --all-features -- -D warnings && cargo test --all
git add -A crates/cli/assets
git commit -m "feat(ui): folder picker modal

Replaces browse.js with the modal from mockup 1b: breadcrumb path, photo
counts, and an Analyzed pill derived by cross-referencing /api/fs paths
against /api/libraries. The primary button says 'Analyze this folder' rather
than a photo count, because /api/fs counts children, not the folder itself.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>"
```

---

## Task 5: Analyzing screens (1c / 1d)

**Files:**
- Rewrite: `crates/cli/assets/analyze.js`
- Modify: `crates/cli/assets/app.js`, `crates/cli/assets/style.css`

Read `docs/design/mockups/Photopipe.dc.html:180-260` and the `stage()` helper at `917-934`.

**Interfaces:**
- Consumes: `api`, `show`, `state` from `app.js`; `icon`; `toast`, `confirmDialog` from `window.pp`.
- Produces: `window.pp.startAnalyze(folder, opts)` where `opts.resume === true` attaches to an already-running job instead of POSTing a new one.

**Cut from this screen:** the "2,183 pairs compared" secondary counter and the "Skip stage" button.

The design's central rule: **counted stages get a determinate bar and an `n / total` figure; uncountable stages get a sweeping band and never a fake percentage.**

- [ ] **Step 1: Write `analyze.js`**

```js
import { api, show, state } from '/app.js';

// The server reports one stage string at a time. The checklist's done/active/
// pending state is derived from position in this ordered list — these strings
// must match JobState.stage in crates/cli/src/serve/mod.rs.
const STAGES = [
  { key: 'scanning',           label: 'Scanning folder' },
  { key: 'detecting defects',  label: 'Detecting defects' },
  { key: 'scoring quality',    label: 'Scoring quality' },
  { key: 'calibrating',        label: 'Calibrating thresholds' },
  { key: 'grouping duplicates', label: 'Grouping duplicates' },
];

let timer = null;

export async function startAnalyze(folder, opts = {}) {
  state.activeFolder = folder;
  show('analyze');
  render(folder, null);

  if (!opts.resume) {
    try {
      await api('POST', '/api/analyze', { folder });
    } catch (e) {
      if (e.status !== 409) {
        window.pp.toast({ kind: 'error', title: 'Could not start the analysis', body: e.message });
        window.pp.openLibraries();
        return;
      }
      // 409 means a job is already in flight — fall through and attach to it.
    }
  }
  poll(folder);
}

// Remembers how far the run actually got, so a terminal or unrecognised stage
// string does not regress the checklist. `failed` is set server-side without
// resetting files_done/files_total (see handlers.rs), so falling back to index 0
// would redraw completed stages as "queued" underneath a failure toast.
let lastStageIdx = 0;

function stageIndex(stage) {
  if (stage === 'done') return STAGES.length;
  const i = STAGES.findIndex(s => s.key === stage);
  if (i !== -1) {
    lastStageIdx = i;
    return i;
  }
  return lastStageIdx;
}

function render(folder, s) {
  const el = document.getElementById('view-analyze');
  const cur = s ? stageIndex(s.stage) : 0;
  const counted = !!s && s.files_total > 0;
  const pct = counted ? Math.round((s.files_done / s.files_total) * 100) : null;

  el.innerHTML = `
    <div class="topbar">
      <span class="topbar-crumb">Libraries</span>
      <span class="topbar-sep">/</span>
      <span class="topbar-title">${folder.split(/[\\/]/).filter(Boolean).pop() || folder}</span>
      <span class="pill pill-run">Analyzing</span>
      <div class="topbar-gap"></div>
    </div>
    <div class="center-stage">
      <div class="card an-card">
        <div class="an-head">
          <div class="an-head-text">
            <div class="an-title">${counted
              ? `Analyzing ${s.files_total.toLocaleString()} photos`
              : (s ? STAGES[Math.min(cur, STAGES.length - 1)].label : 'Starting…')}</div>
            <div class="an-path">${folder}</div>
          </div>
          ${counted
            ? `<div class="an-pct-wrap">
                 <div class="an-pct">${pct}%</div>
                 <div class="an-eta">${s.files_done.toLocaleString()} of ${s.files_total.toLocaleString()}</div>
               </div>`
            : '<div class="spinner" role="progressbar" aria-label="Working"></div>'}
        </div>
        <div class="an-rule"></div>
        <div class="an-stages">${STAGES.map((st, i) => {
          const stateName = !s ? (i === 0 ? 'active' : 'pending')
            : i < cur ? 'done' : i === cur ? 'active' : 'pending';
          const meta = stateName === 'done' ? 'done'
            : stateName === 'pending' ? 'queued'
            : counted ? `${s.files_done.toLocaleString()} / ${s.files_total.toLocaleString()}`
            : 'running';
          const glyph = stateName === 'done' ? '✓' : stateName === 'active' ? '·' : '';
          const showBar = stateName === 'active';
          return `<div class="stage-row">
              <span class="stage-dot ${stateName}">${glyph}</span>
              <span class="stage-title ${stateName}">${st.label}</span>
              <span class="stage-meta ${stateName}">${meta}</span>
            </div>
            ${showBar ? `<div class="stage-bar-wrap"><div class="bar">
              <div class="bar-fill${counted ? '' : ' indet'}"
                   style="${counted ? `width:${pct}%` : ''}"></div>
            </div></div>` : ''}`;
        }).join('')}</div>
        ${s && !counted && s.message
          ? `<div class="an-live"><span class="an-live-dot"></span><span>${s.message}</span></div>`
          : ''}
        <div class="an-foot">
          <span class="an-note">Decisions stay editable while later stages run.</span>
          <button class="btn" id="an-back">Back to libraries</button>
          ${s && s.files_done > 0
            ? `<button class="btn btn-primary" id="an-review">Review ${s.files_done.toLocaleString()} so far</button>`
            : ''}
        </div>
      </div>
    </div>`;

  el.querySelector('#an-back').onclick = () => { stopPolling(); window.pp.openLibraries(); };
  const rv = el.querySelector('#an-review');
  if (rv) rv.onclick = () => { stopPolling(); window.pp.openReview(folder); };
}

function stopPolling() { if (timer) { clearTimeout(timer); timer = null; } }

function poll(folder) {
  stopPolling();
  const tick = async () => {
    let s;
    try {
      s = await api('GET', '/api/analyze/status');
    } catch (e) {
      timer = setTimeout(tick, 1500);
      return;
    }
    if (state.view !== 'analyze') { stopPolling(); return; }

    if (s.stage === 'done') {
      stopPolling();
      if (s.ml_ran === false) {
        window.pp.toast({
          kind: 'warn',
          title: 'Quality scores and duplicate groups were skipped',
          body: "The ML models aren't installed, so this library has defect flags only. " +
                'Tiles show a dash instead of a score.',
        });
      }
      window.pp.openReview(s.folder || folder);
      return;
    }
    if (s.stage === 'failed') {
      stopPolling();
      render(folder, s);
      window.pp.toast({
        kind: 'error',
        title: 'Analysis failed',
        body: s.error || 'No reason was reported.',
      });
      return;
    }
    render(folder, s);
    timer = setTimeout(tick, 1000);
  };
  tick();
}

Object.assign(window.pp, { startAnalyze });
```

- [ ] **Step 2: Add the CSS**

Append to `style.css`: `.center-stage` (flex 1, centred, padding 24px, `overflow: auto`), `.an-card` (width 620px, padding 26px 28px, radial accent wash at 0% 0%), `.an-head` (flex, gap 14px, align flex-start), `.an-title` (700 20px, -.02em), `.an-path` (500 12.5px `--mono`, `--text-dim`, margin-top 6px), `.an-pct-wrap` (right-aligned), `.an-pct` (700 32px, -.02em, `--accent`, `text-shadow: 0 0 10px var(--accent-soft)`), `.an-eta` (500 12px, `--text-muted`, margin-top 7px), `.an-rule` (1px `--border`, margin 22px 0), `.an-stages` (column, gap 4px), `.stage-bar-wrap` (margin 2px 0 8px 30px), `.an-live` (flex, gap 8px, 11px 12px, `--radius-card`, `--surface-2`, 500 12.5px, `--text-muted`, margin-top 20px), `.an-live-dot` (6px circle, `--accent`, `pp-shim 1200ms ease-in-out infinite`), `.an-foot` (flex, gap 12px, margin-top 22px, padding-top 18px, top border), `.an-note` (flex 1, 400 12.5px, `--text-muted`), `.pill-run` (`--accent-tint`, `--accent-tint-fg`).

- [ ] **Step 3: Register the import**

Add `import('/analyze.js')` to the `Promise.all` list in `app.js`.

- [ ] **Step 4: Verify in the browser**

Run a real analysis on a folder of RAW files and watch the whole run:
- The checklist shows five rows; completed rows carry `✓` on a green dot, the running row an accented dot with a glow ring, queued rows a grey dot and "queued".
- During `detecting defects` (countable) the bar is determinate and the figure reads `n / total` with a matching percentage.
- During `grouping duplicates` (uncountable) the bar becomes the sweeping band and **no percentage is displayed anywhere**. Confirm the `an-pct` element is absent from the DOM, not merely hidden.
- On completion the view switches to Review. If the models are missing, the amber warning toast appears.
- Screenshot the counted and uncountable states in both themes.

- [ ] **Step 5: fmt, clippy, test, commit**

```bash
source ~/.cargo/env
cargo fmt && cargo clippy --all-targets --all-features -- -D warnings && cargo test --all
git add -A crates/cli/assets
git commit -m "feat(ui): staged analyze progress with honest indeterminate phases

Derives the done/active/pending checklist from JobState.stage's position in
the known stage order. Countable phases get a determinate bar and an n/total
figure; uncountable phases get the sweeping band and no percentage at all.
The mockup's pairs-compared counter and Skip stage button are omitted -
neither has an API behind it.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>"
```

---

## Task 6: Review grid (1e / 1f / 1g)

The largest screen. Read `docs/design/mockups/Photopipe.dc.html:262-508` and the `tileStyles()` helper at `868-889`.

**Files:**
- Rewrite: `crates/cli/assets/review.js`
- Modify: `crates/cli/assets/app.js`, `crates/cli/assets/style.css`

**Interfaces:**
- Consumes: `api`, `show`, `state` from `app.js`; `icon`; `banner`, `toast` from `window.pp`.
- Produces:
  - `window.pp.openReview(folder, opts)` — `opts.pendingNew` renders the re-analyze banner.
  - `window.pp.reviewPhotos()` → the current filtered array of `ReviewListItem`, used by `detail.js`.
  - `window.pp.reviewIndex()` / `window.pp.reviewSetIndex(i)` — cursor accessors for `detail.js`.
  - `window.pp.reviewApply(fileId, action)` → `Promise<void>` — posts a decision, patches the local row, repaints the header and the affected tile. Shared with `detail.js` and `duplicates.js` so all three stay consistent.
  - `window.pp.reviewIqaRank(score)` → `string|null`, e.g. `'top 12%'`.

**Cut from this screen:** the "Sort: capture time" option. Offer quality score, filename, and flagged-first.

Load with a high limit so the derived flag counts describe the library, not a page: `GET /api/photos?limit=100000`.

- [ ] **Step 1: Write `review.js`**

Structure — write these in order:

```js
import { api, show, state } from '/app.js';
import { icon } from '/icons.js';

const FLAG_META = [
  { key: 'blur',         code: 'BLR', label: 'blur',        long: 'Blur' },
  { key: 'back_focus',   code: 'BF',  label: 'back focus',  long: 'Back focus' },
  { key: 'overexposed',  code: 'OE',  label: 'over',        long: 'Overexposed' },
  { key: 'underexposed', code: 'UE',  label: 'under',       long: 'Underexposed' },
  { key: 'low_iqa',      code: 'IQA', label: 'low IQA',     long: 'Low quality score' },
];
const CODE = Object.fromEntries(FLAG_META.map(f => [f.key, f.code]));

const SORTS = [
  { key: 'score',    label: 'quality score' },
  { key: 'filename', label: 'filename' },
  { key: 'flagged',  label: 'flagged first' },
];
const DENSITY = [
  { key: 'roomy', cols: 5,  ico: 'rows' },
  { key: 'normal', cols: 8, ico: 'cells' },
  { key: 'dense', cols: 12, ico: 'dense' },
];

// Every photo in the library, unfiltered — the basis for honest flag counts.
let all = [];
// The filtered + sorted view the grid renders and the cursor indexes into.
let photos = [];
let cursor = 0;
let counts = { kept: 0, rejected: 0, undecided: 0 };
let loading = true;

const ui = {
  flags: new Set(),      // selected flag keys; empty means no flag filter
  undecidedOnly: false,
  dupOnly: false,
  sort: 'score',
  density: 'normal',
  sheet: false,          // shortcut sheet open
  menu: false,           // defect filter menu open
};
```

Then these functions, in this order:

1. `basename(path)` — last path segment, handling both separators.
2. `flagCounts()` — walk `all` once, return `{ perKey: {blur: n, …}, any: n, none: n, dup: n }`.
3. `applyFilters()` — set `photos` from `all` using `ui`; then sort:
   - `score`: descending `iqa_score`, `null` last.
   - `filename`: ascending `basename(path)` with `localeCompare(…, undefined, { numeric: true })`.
   - `flagged`: descending flag count, tie-broken by ascending `iqa_score`.
   Clamp `cursor` into range afterwards.
4. `renderStats()` — the header block: decided count, keep/reject/keeper legend, the three-segment `.decide-bar`, "% culled" and "n undecided", and the flag chips. Keeper count is `all.filter(p => p.is_keeper).length`. Clicking a chip toggles that key in `ui.flags` and reloads the grid.
5. `renderFilterBar()` — "Undecided only" toggle, the "Flags: …" menu button, "In a duplicate group" toggle, the sort dropdown, "Clear", the density segmented control, and the "Shortcuts ?" button. The flag button's label is `Flags: any` when empty, the flag's `long` name when one is selected, or `Flags: n selected`.
6. `renderFlagMenu()` — the popover from 1f, anchored under its button: rows for "Any defect flag", each flag with its count, and "No flags at all", plus Reset / Apply. `Apply` closes and reloads; `Reset` clears `ui.flags`.
7. `tile(p, i)` — one grid tile. Classes: `tile` plus `keep` / `reject` / `keeper` / `undecided` from `p.verdict` and `p.is_keeper` (keeper wins over keep), plus `cursor` when `i === cursor`. Contents: `<img class="tile-img" loading="lazy" src="/thumb/${p.file_id}">`, the dup pill when `p.group_id != null`, the decision mark, the footer with the flag chip (`p.flags.map(f => CODE[f] ?? f.toUpperCase()).join(' ')`) and the score, and the meter. **Score renders `—` when `p.iqa_score == null`** — the models-missing case the design calls out. The meter is `width: 0` in that case.
8. `renderGrid()` — the four states from 1g:
   - `loading` → 12 `.skeleton` tiles plus a "Loading thumbnails" line.
   - `all.length === 0` → the "No photos here yet" empty state with an "Analyze folder" button.
   - `photos.length === 0` → "Nothing matches these filters", listing the active filters, with "Drop last filter" and "Clear all".
   - `counts.undecided === 0 && all.length > 0` → render the grid, and additionally show the "All n decided" completion card above it with "Review keepers" and "Export n photos", plus the muted "Develop to JPEG — coming later" line.
   Sets `--cols` on `.grid` from the active density. Scrolls the cursor tile into view with `block: 'nearest'`.
9. `renderShortcutSheet()` — the floating panel from 1e, bottom-right, listing the keyboard model from the spec. The keeper row renders `⇧K`.
10. `load()` — `GET /api/photos?limit=100000` into `all`, `GET /api/counts` into `counts`, clear `loading`, then `applyFilters()` and render everything.
11. `reviewApply(fileId, action)` — POST `/api/decisions`, then patch the row in `all` (and its twin in `photos`) exactly as the server would:
    - `keep` → `verdict = 'keep'`, `is_keeper = false`
    - `reject` → `verdict = 'reject'`, `is_keeper = false`
    - `keeper` → the target gets `verdict = 'keep'`, `is_keeper = true`; **every other member of its `group_id` gets `verdict = 'reject'`, `is_keeper = false`** — `pick_keeper` rejects the siblings server-side, and the UI must mirror it or the grid lies until reload
    - `undecide` → `verdict = null`, `is_keeper = false`
    Take `counts` from the POST response. Repaint stats and the affected tiles —
    and additionally re-render the grid and the filter bar whenever
    `counts.undecided` crosses into or out of zero, or the filter bar's own
    counts would go stale. Repainting only stats and one tile makes the 1g
    "All n decided" completion card unreachable through the normal cull path
    (it would appear only after a re-open or a filter toggle) and lets the
    `Undecided only <n>` pill freeze at its last full-render value.
    `renderGrid()` does not re-filter, so calling it does not make the grid
    jump under the cursor.
12. `onKey(e)` — active only when `state.view === 'review'`, no modal is mounted (`#modal-host` has no children), and the event target is not an input. **Return early on `e.ctrlKey || e.metaKey || e.altKey`** — without that guard the single-letter bindings hijack standard browser chords and perform silent, unlogged data writes: `Ctrl/Cmd+X` (cut) posts a reject, `Ctrl+U` (view source) posts an undecide, `Ctrl/Cmd+F` (find) opens the detail view. Bindings per the spec's keyboard table: `ArrowRight`/`j` and `ArrowLeft`/`k` move; `ArrowDown`/`ArrowUp` move by the column count; `Space` keeps; `x` rejects; `u` undecides; `Shift+K` sets the keeper; `f` opens the detail view; `c` opens compare; `?` toggles the sheet; `Esc` closes the sheet or the menu. Deciding advances the cursor **unless Shift is held**. `Space` must `preventDefault()` to stop the page scrolling.
13. `openReview(folder, opts)` — set `state.activeFolder`, `show('review')`, paint the chrome (top bar with library name, photo count, theme toggle, and "Export n photos"), render the `pendingNew` banner when `opts.pendingNew > 0` with a "Re-analyze" action calling `window.pp.startAnalyze(folder)`, wire the keydown listener, then `load()`.
14. `reviewIqaRank(score)` — percentile of `score` within `all`'s non-null `iqa_score` values, returned as `'top N%'`; `null` when there is no distribution.

Register at the bottom:

```js
Object.assign(window.pp, {
  openReview,
  reviewPhotos: () => photos,
  reviewIndex: () => cursor,
  reviewSetIndex: (i) => { cursor = Math.max(0, Math.min(photos.length - 1, i)); renderGrid(); },
  reviewApply,
  reviewIqaRank,
  reviewReload: load,
});
```

- [ ] **Step 2: Add the CSS**

Append to `style.css`: `.review-stats` (flex, gap 26px, 16px 20px 14px, bottom border), `.stat-decided` (flex, baseline, gap 8px), `.stat-decided-n` (700 32px, -.02em), `.stat-decided-of` (600 14px, `--text-dim`), `.legend` (flex, gap 14px, margin-top 9px), `.legend-item` (inline-flex, gap 6px, 600 12px, `--text-muted`), `.legend-sw` (8px square, 2px radius; `.keep`/`.reject`/`.keeper` variants), `.decide-bar` (flex, 10px, `--radius-pill`, overflow hidden, `--inset`, 1px `--border-faint`), `.decide-keep` (`--keep`), `.decide-reject` (`--reject`), `.decide-rest` (flex 1, hatch gradient), `.decide-legend` (flex, space-between, margin-top 8px, 500 11px, `--text-dim`), `.flag-chips` (flex, gap 8px, padding-left 24px, left border), `.chip-n` (700 12px), `.grid-wrap` (flex 1, `min-height: 0`, `overflow: auto`, padding 18px 20px, `--well`), `.grid` (grid, `repeat(var(--cols), 1fr)`, gap 10px), `.sheet` (absolute right 22px bottom 22px, width 336px, `--radius-card`, 1px `--border-strong`, `--surface`, `backdrop-filter: blur(20px)`, `--shadow-float`, `z-index: 40`), `.sheet-head`, `.sheet-row` (flex, gap 10px, 7px 6px), `.sheet-label` (flex 1, 500 12.5px), `.sheet-keys` (flex, gap 4px), `.complete-card` (the 1g completion block), `.grid-loading-note` (500 12px, `--text-dim`, margin-top 14px).

- [ ] **Step 3: Register the import**

Add `import('/review.js')` to the `Promise.all` list.

- [ ] **Step 4: Verify in the browser**

Against a real analyzed library:
- Header counts match `GET /api/counts` exactly. Compare the numbers on screen against `curl -s localhost:8899/api/counts`.
- Flag chip counts match a manual count. Verify with:
  `curl -s 'localhost:8899/api/photos?limit=100000' | python3 -c "import json,sys,collections; c=collections.Counter(f for p in json.load(sys.stdin) for f in p['flags']); print(c)"`
  The chips must agree with that Counter.
- Toggling a chip filters the grid and "Showing n of m" updates.
- The defect filter menu opens, its counts match the chips, Apply filters, Reset clears.
- "Undecided only" and "In a duplicate group" both filter correctly.
- All three sorts reorder the grid. There is **no** "capture time" option.
- Density buttons switch between 5, 8, and 12 columns.
- Tile states: a kept tile has a green outline and a `✓` mark; a rejected tile has a red outline, a `✕` mark, and is **visibly dimmed**; a keeper has a cyan outline and a `★`; the cursor tile has a white ring. Confirm the ring is white in both themes and never coloured.
- A photo with no IQA score shows `—`, not `0.00` or `null`.
- `?` toggles the shortcut sheet; every documented key works; `Space` does not scroll the page; Shift+decide does not advance.
- All four 1g states: filter to something impossible for the filtered-empty state; decide every photo in a small library for the completion state.
- Screenshot the grid in both themes. No console errors.

- [ ] **Step 5: fmt, clippy, test, commit**

```bash
source ~/.cargo/env
cargo fmt && cargo clippy --all-targets --all-features -- -D warnings && cargo test --all
git add -A crates/cli/assets
git commit -m "feat(ui): review grid with derived flag counts and all four grid states

Stats header, flag chips, defect filter menu, density control, and the
shortcut sheet from mockups 1e-1g. Flag counts and the duplicate-group
filter are computed from the loaded photo list rather than new endpoints.
Sort offers quality score / filename / flagged-first; capture time is
omitted because the list payload carries no per-item timestamp.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>"
```

---

## Task 7: Photo detail (1h)

**Files:**
- Create: `crates/cli/assets/detail.js`
- Modify: `crates/cli/assets/app.js`, `crates/cli/assets/style.css`

Read `docs/design/mockups/Photopipe.dc.html:510-604`.

**Interfaces:**
- Consumes: `api` from `app.js`; `icon`; `window.pp.reviewPhotos/reviewIndex/reviewSetIndex/reviewApply/reviewIqaRank`.
- Produces: `window.pp.openDetail(index)`, `window.pp.closeDetail()`.

Zoom is **relative to the served preview**, which `/preview/:id` caps at 2048px on the long edge — not sensor pixels. Label the control `Fit / 100% / 200%` and add `title="100% of the 2048px preview, not the original"` to the 100% button so the claim is honest on hover.

- [ ] **Step 1: Write `detail.js`**

A full-surface overlay (`z-index: 120`) inside `#modal-host` so `review.js`'s keydown handler stands down while it is open.

Structure:
1. Module state: `idx`, `dump` (the `/api/photos/:id` response), `zoom` (`'fit' | 1 | 2`).
2. `openDetail(index)` — set `idx`, mount the shell, fetch the dump, render. Fetch failures leave the image showing and render `—` in the metadata panel plus an error toast; a metadata failure must not blank the photo.
3. `render()` — three regions:
   - **Top bar:** an `Esc` button, the filename in mono, `"${idx + 1} / ${photos.length}"` plus the active filter description, the Fit/100%/200% segmented control, and the fullscreen button (`requestFullscreen()` on the overlay).
   - **Stage:** `--well-2` background, the `/preview/:id` image, and prev/next circular buttons. `fit` uses `max-width/max-height: 100%; object-fit: contain`; `100%`/`200%` set explicit `width` from the image's `naturalWidth × zoom` inside an `overflow: auto` container, so panning is native scrolling.
   - **Decision bar:** Reject / Keep / ★ Keeper / Undo buttons carrying their `.kbd` badges, and the "Deciding advances to the next frame. Hold Shift to stay put." note. Each calls `window.pp.reviewApply` then advances unless Shift is held.
   - **Side panel (344px):** four sections built from the dump.
     - *Decision* — pills for the current verdict and, when `is_keeper`, `★ Keeper of group N`.
     - *Analysis* — the IQA score (`dump.iqa.score`, an `f32`; `dump.iqa` is `null` when no IQA row exists) at 700 32px in `--accent-glow`, with `window.pp.reviewIqaRank()` beneath it and a meter at `score × 100` percent. Then one row per flag in `FLAG_META` order, matched against `dump.defect_flags[]` by `flag_type`: flagged rows show the code chip at full contrast and `confidence ${confidence.toFixed(2)}`; unflagged rows are dimmed and read `Blur — not flagged`. When `dump.iqa` is null the score reads `—` and the panel says the models were not run. Below that, the duplicate-group card with a "Compare" action calling `window.pp.openCompare(groupId)`.
     - *File* — the path in mono with `word-break: break-all`, then `${file_format} · ${humanBytes(size_bytes)} · ${exif.width} × ${exif.height}` (omit the dimensions when EXIF has none).
     - *Camera* — a `.kv` grid: Body (`camera_make` + `camera_model`), Lens, Focal length (`${n} mm`), Aperture (`f/${n}`), Shutter (as `1/${Math.round(1/s)} s` when `s < 1`, else `${s} s`), ISO, Captured (`new Date(captured_at * 1000).toLocaleString()`). Skip any row whose value is null rather than printing `null`.
4. `move(d)` — step `idx`, keep `review`'s cursor in sync via `reviewSetIndex`, refetch the dump.
5. `onKey(e)` — the same bindings as the grid plus `Esc`/`f` to close and `c` to open compare. Registered with `capture: true` while open, removed on close.

Register: `Object.assign(window.pp, { openDetail, closeDetail })`.

- [ ] **Step 2: Add the CSS**

`.detail` (fixed inset 0, `--bg`, flex, `z-index: 120`), `.detail-main` (flex 1, `min-width: 0`, column), `.detail-bar` (flex, gap 12px, 12px 16px, bottom border), `.detail-file` (600 13.5px `--mono`), `.detail-pos` (500 12px, `--text-dim`), `.detail-stage` (flex 1, `min-height: 0`, centred, `--well-2`, padding 26px, `overflow: auto`, position relative), `.detail-img` (block, `--radius-tile`, `box-shadow: 0 8px 40px rgba(0,0,0,.55)`), `.detail-nav` (absolute, 40px circle, 1px `--border-strong`, translucent `--bg`, `backdrop-filter: blur(20px)`; `.prev` left 18px, `.next` right 18px, both `top: 50%; transform: translateY(-50%)`), `.decide-row` (flex, gap 10px, 14px 16px, top border), `.btn-keep` (`--keep-soft` fill, `--keep` edge, `--keep-fg` text), `.btn-reject` (`--reject-soft`, `--reject` edge at 35%, `--reject-fg`), `.btn-keeper` (`--accent-soft`, `--accent-edge`, `--accent-tint-fg`), `.decide-note` (500 12px, `--text-dim`, right-aligned), `.detail-side` (344px, flex none, left border, column, `overflow: auto`), `.side-sec` (16px 18px, bottom border), `.pill-keep` / `.pill-keeper` (decision pills), `.iqa-n` (700 32px, -.02em, `--accent-glow`, `text-shadow: 0 0 24px var(--accent-soft)`), `.iqa-note` (500 12px, `--text-muted`), `.iqa-meter` (5px, `--radius-pill`, `--kbd-bg` track, `--accent-glow` fill, margin 14px 0 16px), `.flag-row` (flex, gap 9px, align center), `.flag-row.off` (`--text-dim`), `.flag-conf` (600 11.5px, `--text-dim`), `.dup-card` (flex, gap 10px, 11px 12px, `--radius-card`, 1px `--border`, `--surface`, margin-top 16px), `.file-path` (500 12px/1.5 `--mono`, `--text-muted`, `word-break: break-all`).

- [ ] **Step 3: Register the import**

Add `import('/detail.js')` to the `Promise.all` list.

- [ ] **Step 4: Verify in the browser**

- Clicking a tile, or pressing `f`, opens the detail overlay on that photo.
- The filename, position, path, format, size, and dimensions match `curl -s localhost:8899/api/photos/<id>` field for field. Spot-check three photos.
- Every EXIF row present in the JSON appears; absent ones are omitted, and **no row reads `null`, `undefined`, or `NaN`**.
- Fit scales the image to the stage; 100% and 200% enable scroll-panning; hovering 100% shows the honest tooltip.
- Flagged rows show the confidence from the JSON; unflagged rows are dimmed and read "not flagged".
- A photo with no IQA row shows `—`, not `0.00`.
- The decision bar works; deciding advances; Shift+deciding does not. The change is visible on the grid tile behind after closing.
- `Esc` closes; arrows move between photos; the grid cursor follows.
- Screenshot in both themes. No console errors.

- [ ] **Step 5: fmt, clippy, test, commit**

```bash
source ~/.cargo/env
cargo fmt && cargo clippy --all-targets --all-features -- -D warnings && cargo test --all
git add -A crates/cli/assets
git commit -m "feat(ui): photo detail overlay with the analysis side panel

Zoom, decision bar, and the decision/analysis/file/camera panel from mockup
1h, all fed by /api/photos/:id. Zoom is labelled honestly as relative to the
2048px preview rather than sensor pixels. Absent EXIF rows are omitted
instead of printing null.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>"
```

---

## Task 8: Duplicates review (1i)

**Files:**
- Rewrite: `crates/cli/assets/duplicates.js`
- Modify: `crates/cli/assets/app.js`, `crates/cli/assets/style.css`

Read `docs/design/mockups/Photopipe.dc.html:606-686` and the `clusterFrames()` helper at `1022-1036`.

**Interfaces:**
- Consumes: `api`, `show`, `state`; `icon`; `confirmDialog`, `toast`.
- Produces: `window.pp.openDuplicates(folder)`.

**Cut from this screen:** the cluster time range and the "96% similar" figure. The header reads `Cluster 07 · 4 frames · 2026-07-18`.

The design's governing rule, quoted in the screen's own footnote: **"A suggestion is never a decision."** The dashed "Suggested best" frame is the algorithm's pick; nothing is rejected until the user sets a keeper, and every set is one undo away. Render that footnote.

- [ ] **Step 1: Write `duplicates.js`**

Structure:
1. Module state: `clusters`, `undecidedOnly` (default `true`, matching the mockup's "Undecided clusters" filter), and `confirming` — the `{ groupId, fileId }` whose confirm popover is open, or `null`.
2. `clusterState(c)` — `'keeper-set'` when any member has `is_keeper`, else `'undecided'`.
3. `openDuplicates(folder)` — set `state.activeFolder`, `show('duplicates')`, paint the top bar (library name / "Duplicates" / `"n clusters · n decided · n frames"`, the undecided filter toggle, and "Accept all suggestions"), then `load()`.
4. `load()` — `GET /api/clusters` into `clusters`, then `render()`. On an empty list render the empty state: "No duplicate groups in this library", with the note that grouping needs the ML models when `iqa_score` is null across the library.
5. `renderCluster(c)` — a `.dup-cluster` card:
   - Header: `Cluster ${String(c.group_id).padStart(2, '0')}`, `${c.members.length} frames · ${c.date}`, the state pill (`Undecided` neutral / `Keeper set` accent), then `Compare` (with its `C` badge, calling `window.pp.openCompare(c.group_id)`), and either `Skip cluster` when undecided or `Undo` when decided.
   - Body: one `.dup-frame` per member at 262×175, carrying the tag from `clusterFrames()`: `Suggested best` (dashed accent pill) on `c.suggested_keeper_id` while the cluster is undecided, `★ Keeper` on the member with `is_keeper`, `Rejected` on members whose verdict is `reject`. Each frame shows the filename and score in its footer, uses the same outline/dim rules as grid tiles, and has a `★ Keeper` action.
   - When `confirming` matches this cluster, the confirm popover from the mockup, anchored bottom-right of the body: `Keep DSC04182 and reject 3 siblings?`, the sibling names, `★ Set keeper` (`↵`) and `Cancel` (`Esc`).
6. `pickKeeper(groupId, fileId)` — POST `keeper`, then mirror `pick_keeper` locally: the chosen member gets `verdict = 'keep'`, `is_keeper = true`; every sibling gets `verdict = 'reject'`, `is_keeper = false`. Re-render only that cluster. Toast the result with an "Undo" action calling `undoCluster`.
7. `undoCluster(groupId)` — POST `undecide` for every member, sequentially so a failure stops rather than half-applying; reset each member locally; re-render the cluster.
8. `acceptAll()` — for every undecided cluster with a `suggested_keeper_id`, behind a `confirmDialog` naming the count ("Set the suggested keeper in 8 clusters? 34 sibling frames become reject. Each cluster stays individually undoable."), call `pickKeeper` in sequence. Report `n succeeded, m failed` in a toast.
9. `onKey(e)` — active only when `state.view === 'duplicates'` and no overlay is mounted: `Enter` confirms the open popover, `Esc` cancels it, `c` opens compare for the first undecided cluster, `u` undoes the most recently decided cluster.

Register `Object.assign(window.pp, { openDuplicates })`.

- [ ] **Step 2: Add the CSS**

`.dup-list` (flex 1, `min-height: 0`, `overflow: auto`, padding 18px 20px, column, gap 14px), `.dup-cluster` (1px `--border-strong`, `--radius-card`, `--surface-2`, overflow hidden), `.dup-cluster.decided` (`opacity: .92`), `.dup-head` (flex, gap 10px, 12px 14px, bottom border), `.dup-title` (700 13px), `.dup-meta` (500 12px, `--text-dim`), `.pill-undecided` (`--kbd-bg`, `--text-muted`), `.pill-keeper-set` (`--accent-soft`, `--accent-glow`), `.dup-body` (position relative, flex, gap 12px, padding 14px, `--well-2`, `overflow-x: auto`), `.dup-frame` (262×175, tile rules), `.tile-tag` (absolute top 8px left 8px, 700 10px, `.04em`, uppercase, 5px 8px, `--radius-pill`), `.tag-suggested` (1px dashed `--accent-glow`, `--flag-scrim` background, `--accent-glow` text), `.tag-keeper` (`--accent` fill, `--accent-on` text), `.tag-rejected` (`--reject` fill at 90%, dark text), `.dup-pop` (absolute right 14px bottom 14px, width 330px, `--radius-card`, 1px `--border-strong`, `--surface`, `backdrop-filter: blur(20px)`, `--shadow-float`, padding 14px, `z-index: 20`), `.dup-note` (flex, gap 12px, 12px 14px, `--radius-card`, 1px `--border`, `--surface-2`, 400 12.5px, `--text-muted`).

- [ ] **Step 3: Register the import**

Add `import('/duplicates.js')` to the `Promise.all` list.

- [ ] **Step 4: Verify in the browser**

Against a library that has duplicate groups (requires the ML models — see the note at the end of this plan):
- Cluster count and frame count in the top bar match `curl -s localhost:8899/api/clusters | python3 -c "import json,sys; c=json.load(sys.stdin); print(len(c), sum(len(x['members']) for x in c))"`.
- An undecided cluster shows the dashed "Suggested best" pill on the member whose `file_id` equals `suggested_keeper_id`. Verify against the JSON.
- **Nothing is rejected before you act.** Confirm every member's verdict is null in a fresh undecided cluster.
- `★ Keeper` opens the confirm popover naming the correct siblings; `Enter` confirms, `Esc` cancels.
- After confirming, the keeper shows `★ Keeper` and every sibling shows `Rejected` and is dimmed. Cross-check with `curl -s localhost:8899/api/clusters` — the UI must match the server, not merely look plausible.
- `Undo` returns all members to undecided; re-check against the server.
- "Accept all suggestions" asks first, then decides every undecided cluster.
- The "A suggestion is never a decision" footnote is present.
- Screenshot in both themes. No console errors.

- [ ] **Step 5: fmt, clippy, test, commit**

```bash
source ~/.cargo/env
cargo fmt && cargo clippy --all-targets --all-features -- -D warnings && cargo test --all
git add -A crates/cli/assets
git commit -m "feat(ui): duplicates review with confirm-before-reject and cluster undo

Mockup 1i: suggested-best is a dashed suggestion, never a decision. Setting
a keeper is confirmed first, mirrors pick_keeper's sibling rejection locally,
and is undoable by undeciding every member. Cluster time range and the
similarity percentage are omitted - neither is exposed by /api/clusters.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>"
```

---

## Task 9: Compare mode (1j)

**Files:**
- Create: `crates/cli/assets/compare.js`
- Modify: `crates/cli/assets/app.js`, `crates/cli/assets/style.css`

Read `docs/design/mockups/Photopipe.dc.html:688-733`.

**Interfaces:**
- Consumes: `api`; `icon`; `window.pp.reviewApply`, `window.pp.openDuplicates`.
- Produces: `window.pp.openCompare(groupId, fileIds)` — with `fileIds` omitted, compares the first two members of `groupId`.

Same honesty note as the detail view: zoom is relative to the 2048px preview.

- [ ] **Step 1: Write `compare.js`**

A full-surface overlay in `#modal-host`, `z-index: 130`.

1. `openCompare(groupId, fileIds)` — fetch `/api/clusters`, find the group, take the two frames (the given ids, or the first two members). Fewer than two members → an info toast ("Nothing to compare — this group has one frame") and no overlay.
2. Layout: a top bar (`Compare · Cluster 07`, `2 of 4 frames · zoom synced`, the Fit/100% segmented control, `Close` with its `Esc` badge), then two equal panes separated by a 2px `--inset` gutter.
3. Each pane: the preview image with a filename badge top-left, and a footer with the quality score, the flag chips, the EXIF one-liner (`1/640 s · f/2.0 · ISO 800`, from `/api/photos/:id`), and a `★ Keeper` button. The pane whose member has the higher `iqa_score` gets `outline: 2px solid var(--accent-glow); outline-offset: 3px` and a `Sharper` pill top-right, exactly as the mockup marks the better frame. If the scores are equal or either is null, mark neither.
4. **Synced zoom and pan** is the point of the screen: both panes share one `zoom` value, and scrolling or dragging either pane writes its `scrollLeft`/`scrollTop` to the other. Guard against the feedback loop with a `syncing` flag.
5. `★ Keeper` calls `window.pp.reviewApply(fileId, 'keeper')`, closes the overlay, and refreshes whichever screen is behind (`openDuplicates` when `state.view === 'duplicates'`, otherwise `reviewReload`).
6. Keys: `Esc` closes, `a` picks the left frame, `d` picks the right (the mockup's badges), `1`/`2` set Fit/100%.

- [ ] **Step 2: Add the CSS**

`.cmp` (fixed inset 0, `--bg`, column, `z-index: 130`), `.cmp-bar` (flex, gap 12px, 12px 16px, bottom border), `.cmp-panes` (flex 1, `min-height: 0`, flex, gap 2px, `--inset`), `.cmp-pane` (flex 1, `min-width: 0`, column, `--well-2`), `.cmp-stage` (flex 1, `min-height: 0`, centred, padding 20px, `overflow: auto`), `.cmp-img` (`--radius-tile`, `box-shadow: 0 8px 30px rgba(0,0,0,.5)`), `.cmp-img.better` (`outline: 2px solid var(--accent-glow); outline-offset: 3px`), `.cmp-badge` (absolute top 8px left 8px, 700 10px `--mono`, `--flag-scrim`, white), `.cmp-sharper` (absolute top 8px right 8px, `--accent` fill, `--accent-on`, `--radius-pill`, 700 10px uppercase `.04em`), `.cmp-foot` (flex, gap 12px, 12px 16px, top border, `--surface-2`), `.cmp-score` (700 20px), `.cmp-score.better` (`--accent-glow`, `text-shadow: 0 0 24px var(--accent-soft)`), `.cmp-exif` (500 11.5px, `--text-dim`).

- [ ] **Step 3: Register the import**

Add `import('/compare.js')` to the `Promise.all` list.

- [ ] **Step 4: Verify in the browser**

- `C` from a duplicates cluster, and "Compare" from the detail view's duplicate card, both open the overlay on the right group.
- Both frames load. The higher-scoring one is outlined and carries the `Sharper` pill; when scores tie or are null, neither is marked.
- Setting zoom to 100% and scrolling one pane scrolls the other to the same offset. This is the screen's whole purpose — verify it in both directions and confirm no scroll jitter from the feedback guard.
- `a` and `d` set the keeper from the correct side; the screen behind reflects it.
- `Esc` closes.
- A single-member group produces the info toast, not a broken overlay.
- Screenshot in both themes. No console errors.

- [ ] **Step 5: fmt, clippy, test, commit**

```bash
source ~/.cargo/env
cargo fmt && cargo clippy --all-targets --all-features -- -D warnings && cargo test --all
git add -A crates/cli/assets
git commit -m "feat(ui): compare mode with synced zoom and pan

Mockup 1j: two frames from one cluster side by side, sharing a zoom level
and mirroring each other's scroll offset. The higher-scoring frame is
outlined and marked Sharper; ties and missing scores mark neither.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>"
```

---

## Task 10: Export dialog (1k)

**Files:**
- Create: `crates/cli/assets/export.js`
- Modify: `crates/cli/assets/app.js`, `crates/cli/assets/style.css`

Read `docs/design/mockups/Photopipe.dc.html:735-777` for the dialog and `779-822` for the toasts, which Task 2 already implemented.

**Interfaces:**
- Consumes: `api`, `humanBytes`; `icon`; `modal`, `toast`.
- Produces: `window.pp.openExport()`.

**Cut from this dialog:** the destination "Change" button, the free-space stat tile, and the `rejected.txt` sidecar checkbox. Two stat tiles remain: photos and bytes.

The destination is fixed server-side — `_keepers`, relative to where `photopipe serve` was started. Say exactly that rather than implying it is configurable.

- [ ] **Step 1: Write `export.js`**

```js
import { api, humanBytes } from '/app.js';
import { icon } from '/icons.js';

export async function openExport() {
  let est;
  try {
    est = await api('GET', '/api/export/estimate');
  } catch (e) {
    window.pp.toast({ kind: 'error', title: 'Could not size the export', body: e.message });
    return;
  }

  if (!est.files) {
    window.pp.toast({
      kind: 'info',
      title: 'Nothing to export yet',
      body: 'Keep or mark a few frames as keepers first.',
    });
    return;
  }

  const m = window.pp.modal({
    title: `Export ${est.files.toLocaleString()} keeper${est.files === 1 ? '' : 's'}`,
    subtitle: 'RAW files are copied. Originals stay where they are.',
    width: 520,
    body: `
      <div class="exp-body">
        <div>
          <div class="section-label">Destination</div>
          <div class="exp-dest">
            <span class="exp-dest-ico">${icon('folder', 16, 1.7)}</span>
            <span class="exp-dest-path">_keepers</span>
          </div>
          <div class="exp-dest-note">Relative to the folder <code>photopipe serve</code> was
            started in. Fixed for now.</div>
        </div>
        <div class="exp-stats">
          <div class="stat"><div class="stat-n">${est.files.toLocaleString()}</div>
            <div class="stat-label">photos</div></div>
          <div class="stat"><div class="stat-n">${humanBytes(est.bytes)}</div>
            <div class="stat-label">to copy</div></div>
        </div>
        <div class="exp-note">Developing keepers into JPEGs will happen here in a later
          version. For now photopipe hands you the RAW files.</div>
      </div>`,
    footer: `
      <div class="modal-foot-row">
        <span class="exp-foot-gap"></span>
        <button class="btn" id="exp-cancel">Cancel</button>
        <button class="btn btn-primary" id="exp-go">Copy ${est.files.toLocaleString()} photos
          <span class="kbd">↵</span></button>
      </div>`,
  });

  m.el.querySelector('#exp-cancel').onclick = () => m.close();

  const go = m.el.querySelector('#exp-go');
  go.focus();
  const run = async () => {
    go.disabled = true;
    go.textContent = 'Copying…';
    let r;
    try {
      r = await api('POST', '/api/export', { regenerate: false });
    } catch (e) {
      m.close();
      window.pp.toast({
        kind: 'error',
        title: 'Export failed',
        body: `${e.message}. Nothing was overwritten.`,
        actions: [{ label: 'Retry', onClick: () => { openExport(); } }],
      });
      return;
    }
    m.close();
    const body = `${humanBytes(r.bytes_copied)} copied` +
      (r.errors ? ` · ${r.errors} file${r.errors === 1 ? '' : 's'} failed` : '');
    window.pp.toast({
      kind: r.errors ? 'warn' : 'success',
      title: `${r.files_copied.toLocaleString()} photo${r.files_copied === 1 ? '' : 's'} copied to _keepers`,
      body,
    });
  };
  go.onclick = run;
  m.el.addEventListener('keydown', (e) => {
    if (e.key === 'Enter' && !go.disabled) { e.preventDefault(); run(); }
  });
}

Object.assign(window.pp, { openExport });
```

- [ ] **Step 2: Add the CSS**

`.exp-body` (column, gap 16px, padding 18px 20px), `.exp-dest` (flex, gap 10px, 10px 12px, 1px `--border-strong`, `--radius-card`, `--surface-2`), `.exp-dest-ico` (`--accent`), `.exp-dest-path` (flex 1, 500 12.5px `--mono`, ellipsis), `.exp-dest-note` (400 12px, `--text-dim`, margin-top 7px), `.exp-stats` (grid, `repeat(2, 1fr)`, gap 10px), `.exp-note` (400 12.5px, `--text-dim`), `.exp-foot-gap` (flex 1).

- [ ] **Step 3: Register the import**

Add `import('/export.js')` to the `Promise.all` list.

- [ ] **Step 4: Verify in the browser**

- The rail's Export cell and the grid's "Export n photos" button both open the dialog.
- The photo count and byte figure match `curl -s localhost:8899/api/export/estimate`.
- There are **two** stat tiles, no "Change" button, and no sidecar checkbox.
- With no keepers decided, the info toast appears instead of the dialog.
- A real export copies the files and raises the success toast with the right count; check `_keepers/` on disk.
- `Enter` triggers the copy; `Esc` and Cancel close.
- Screenshot in both themes. No console errors.

- [ ] **Step 5: fmt, clippy, test, commit**

```bash
source ~/.cargo/env
cargo fmt && cargo clippy --all-targets --all-features -- -D warnings && cargo test --all
git add -A crates/cli/assets
git commit -m "feat(ui): export dialog replacing the confirm/alert pair

Mockup 1k, with the destination shown read-only because it is fixed
server-side. The free-space tile, destination picker, and rejected.txt
checkbox are omitted - no API backs any of them. Results land as toasts.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>"
```

---

## Task 11: Integration pass and documentation

**Files:**
- Modify: `crates/cli/assets/app.js` (final import list), `crates/cli/assets/style.css` (cleanup)
- Modify: `README.md` (the review UI section)
- Test: `crates/cli/tests/serve.rs`

- [ ] **Step 1: Add a test pinning the asset set**

The manifest test from Task 1 only covers what `index.html` references directly. Every screen module is reached by dynamic import, so add a test that pins the full set:

```rust
/// Every module the app dynamically imports must be embedded. `index.html`
/// only references app.js, so a missing screen module would otherwise surface
/// as a blank view at runtime rather than a failing build.
#[tokio::test]
async fn every_screen_module_is_embedded() {
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    const MODULES: &[&str] = &[
        "tokens.css", "style.css", "index.html", "app.js", "icons.js", "rail.js",
        "toast.js", "libraries.js", "picker.js", "analyze.js", "review.js",
        "detail.js", "duplicates.js", "compare.js", "export.js", "Manrope.ttf",
    ];

    let dir = tempfile::TempDir::new().unwrap();
    let catalog = pipeline::catalog::Catalog::open(&dir.path().join("c.duckdb")).unwrap();
    let cache = pipeline::cache::Cache::open(dir.path().join("cache")).unwrap();
    let state = app_state_active(catalog, cache);

    for m in MODULES {
        let app = photopipe::serve::router(state.clone());
        let resp = app
            .oneshot(
                Request::builder()
                    .uri(format!("/{m}"))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "/{m} is not embedded");
    }

    // And nothing stale is left behind from the previous UI.
    for gone in ["home.js", "browse.js"] {
        let app = photopipe::serve::router(state.clone());
        let resp = app
            .oneshot(
                Request::builder()
                    .uri(format!("/{gone}"))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::NOT_FOUND,
            "/{gone} should have been deleted"
        );
    }
}
```

- [ ] **Step 2: Run it, confirm it passes**

```bash
source ~/.cargo/env
cargo test -p photopipe --test serve every_screen_module_is_embedded
```

If a module 404s, it was never created — go back to its task. If `home.js` or `browse.js` still resolve, they were not deleted.

- [ ] **Step 3: Finalise the import list in `app.js`**

```js
Promise.all([
  import('/rail.js'),
  import('/toast.js'),
  import('/libraries.js'),
  import('/picker.js'),
  import('/analyze.js'),
  import('/review.js'),
  import('/detail.js'),
  import('/duplicates.js'),
  import('/compare.js'),
  import('/export.js'),
]).then(() => boot()).catch((e) => {
  // A module that fails to parse would otherwise leave a blank window with the
  // error buried in the console.
  document.body.innerHTML =
    `<pre style="padding:24px;font:13px ui-monospace,monospace">photopipe UI failed to load:\n${e}</pre>`;
});
```

- [ ] **Step 4: Audit for literal colours**

The theme toggle only works if nothing bypasses the tokens.

```bash
cd crates/cli/assets
grep -nE '#[0-9a-fA-F]{3,8}\b' style.css *.js | grep -v '^tokens.css' || echo "clean"
grep -nE 'rgba?\([0-9]' style.css *.js || echo "clean"
```

Expected: `clean` for both, with these allowed exceptions, which are theme-independent by intent:
- the drop-shadow rgba values inside `--shadow-*` in `tokens.css`
- `box-shadow: 0 8px 40px rgba(0,0,0,.55)` on `.detail-img` and `.cmp-img` — a shadow under a photo, not chrome

Move anything else into `tokens.css` and reference it.

- [ ] **Step 5: Full-app walkthrough in both themes**

Start fresh (`rm -rf` nothing — use a real library) and drive the whole flow twice, once per theme:

Libraries → picker → analyze → review grid → filter and sort → detail → compare → duplicates → export.

Confirm on each screen:
- No literal white-on-white or black-on-black anywhere after toggling.
- No console errors or warnings at any point (`browser_console_messages`).
- Every number on screen agrees with the API it came from.
- No element from the "Cut elements" table is present.

Capture one screenshot per screen per theme (22 total) and keep them in the scratchpad for the review.

- [ ] **Step 6: Update the README**

Rewrite the review-UI section to describe the new screens: the rail, the Libraries table, the picker, staged analyze progress, the grid with its filters and density, the detail panel, duplicates with confirm-and-undo, compare, and export. Document the keyboard model as a table, matching the spec exactly — including `Shift+K` for keeper and the note that Shift+decide stays put. State that Develop is a placeholder.

- [ ] **Step 7: fmt, clippy, full test run**

```bash
source ~/.cargo/env
cargo fmt
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all
```

- [ ] **Step 8: Commit**

```bash
git add -A crates/cli README.md
git commit -m "feat(ui): finish the redesign — asset manifest test, README, colour audit

Pins the full embedded module set so a missing screen fails the build rather
than blanking a view, asserts the retired home.js/browse.js are gone, and
documents the new screens and keyboard model.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>"
```

---

## Verification prerequisite

Tasks 5 through 10 need a real analyzed library on the box, and Tasks 8 and 9 need one **with duplicate groups**, which requires the dinov2/clip ONNX exports to be present (`photopipe doctor` reports whether they are). There is currently no library at `~/.local/share/photopipe/libraries/` and `tests/fixtures/` is empty.

Per `CLAUDE.md`, fixtures are the user's call — do not fabricate photos or EXIF. Before starting Task 5, confirm with the user:

1. A folder of real RAW/JPG photos to analyze for verification.
2. Whether the ML models are installed on this box. If they are not, duplicate groups will be empty, and Tasks 8 and 9 can only be verified against their empty state — note that explicitly in the task's completion report rather than claiming the screens were verified.

Tasks 1 through 4 need no library: the Libraries screen renders its own empty state, and the picker walks the filesystem.

## Self-review notes

Checked against `docs/superpowers/specs/2026-07-30-review-ui-redesign-design.md`:

- Every spec section maps to a task: design system → Task 2, theming → Task 1, architecture → Tasks 1-2 plus the per-screen tasks, Rust changes → Task 1, the eleven screens → Tasks 3-10, keyboard model → Tasks 6-9 and documented in Task 11, error handling → Task 2's toast layer with per-screen use, testing → Tasks 1 and 11.
- All six cut elements appear in the Global Constraints table and again in the task that would otherwise implement them.
- `reviewApply` is defined once (Task 6) and consumed by Tasks 7, 8, and 9 under that exact name; `openCompare(groupId, fileIds)` is defined in Task 9 and called from Tasks 7 and 8 with that signature.
- The spec's `Fit / 100% / 200%` honesty caveat is carried into both Task 7 and Task 9.
