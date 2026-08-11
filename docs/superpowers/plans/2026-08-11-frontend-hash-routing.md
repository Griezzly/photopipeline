# Frontend Hash Routing Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give the browser's Back button a meaning inside the review UI — stepping back through screens and closing overlays instead of leaving the page.

**Architecture:** A new `crates/cli/assets/router.js` owns `location.hash` and is the single entry point for every screen and overlay transition. All navigation call sites change from `window.pp.openReview(folder)` style direct calls to `window.pp.go('/review')`; the router resolves the path and calls the same `open*` functions as the route's *applier*. Back, forward, reload and a hand-typed URL therefore all take one code path.

**Tech Stack:** Vanilla ES modules, no build step, no framework. `history.pushState`/`replaceState` writing hash URLs, with `popstate` + `hashchange` listeners. Server side is axum + rust-embed; no server changes beyond one test.

**Spec:** `docs/superpowers/specs/2026-08-10-frontend-hash-routing-design.md`

## Global Constraints

- Assets are plain ES modules served by rust-embed from `crates/cli/assets/`. No bundler, no npm, no new dependency of any kind.
- `router.js` **imports nothing.** It reaches everything through `window.pp` inside functions. This matches the existing comment in `app.js:1-3` ("Screens reach each other through `window.pp` rather than importing one another, to keep the module graph acyclic") and avoids a circular import with `app.js`.
- The hash is never sent to the server. `crates/cli/src/serve/mod.rs` gains no new routes.
- The active library is **not** in the URL. Routes that need one resolve it via `state.activeFolder`, falling back to `GET /api/active`.
- Review filters, sort mode, and grid cursor stay **out** of the URL.
- `cargo fmt`, `cargo clippy --all-targets --all-features -- -D warnings`, and `cargo test --all` must all pass before the branch is done (project CLAUDE.md).
- Commit messages are conventional-commit style. Do not sign commits as Claude.

## A note on testing

This project has **no JavaScript test harness** — no npm, no headless browser — and the approved spec explicitly declines to add one ("If automated coverage of the above is wanted later, that is a separate decision"). So the TDD cycle for the JS tasks is not `write failing test → make it pass`; it is a scripted **manual verification** against a real library, with the exact steps and expected results written out per task. Task 2 carries the one automated test that is possible: a Rust assertion that `/router.js` is served and referenced.

Every JS task's verification step assumes a running server:

```bash
cargo run --release -- serve --folder <a-folder-with-an-analyzed-library>
# then open the printed http://127.0.0.1:<port>/ in a browser
```

If you do not have an analyzed library to hand, run `cargo run --release -- analyze <folder>` first, or ask the user which folder to use. **Do not fabricate fixtures.**

---

## Deviations from the spec

Two things surfaced while reading the call sites that the spec's route table did not cover. Task 1 records both as spec amendments before anything is implemented.

**A1 — Compare has three parents, not one.** The spec listed only `#/duplicates/compare/:groupId`. But `openCompare` is reached from three places: the duplicates list (`duplicates.js:277`, `duplicates.js:544`), the review grid via `c` (`review.js:778`), and the photo detail overlay via `c` (`detail.js:224`). `compare.js:266-268` already branches on which one it was, returning to duplicates only when that is where it came from. Routing all three under `/duplicates` would change that behaviour. So compare gets three routes:

| Hash | Layer underneath |
|---|---|
| `#/duplicates/compare/:groupId` | duplicates list |
| `#/review/compare/:groupId` | review grid |
| `#/review/photo/:photoId/compare/:groupId` | photo detail overlay |

**A2 — Analyze: push in, replace out.** The spec's prose says `#/analyze` is "`replace`d rather than pushed", but its worked example (`#/libraries` → `#/analyze` → `#/review` leaves two entries, Back from review lands on libraries) only works if *entering* analyze pushes and *leaving* it replaces. The example is what gets implemented; the prose is corrected.

---

## File Structure

| File | Responsibility |
|---|---|
| `crates/cli/assets/router.js` | **New.** Path parsing, history mechanics (`go`/`replace`/`back`/`setPath`), the route→applier table, the library guard, overlay teardown. |
| `crates/cli/assets/app.js` | Imports `router.js`; `boot()` shrinks to starting the router. |
| `crates/cli/assets/rail.js` | Rail buttons call `go()`. |
| `crates/cli/assets/libraries.js` | Opening a library navigates instead of calling `openReview` directly. |
| `crates/cli/assets/picker.js` | "Analyze this folder" navigates. |
| `crates/cli/assets/analyze.js` | Entering pushes, leaving replaces. |
| `crates/cli/assets/review.js` | Tile click, `f`, `c`, export buttons, analyze button navigate. |
| `crates/cli/assets/duplicates.js` | Compare buttons and `c` navigate. |
| `crates/cli/assets/detail.js` | `openDetail` becomes a pure applier; `dismissDetail()` goes through history; stepping pushes. |
| `crates/cli/assets/compare.js` | `openCompare` returns a success boolean; `dismissCompare()` goes through history. |
| `crates/cli/assets/export.js` | `openExport` returns a success boolean; modal dismissal goes through history. |
| `crates/cli/tests/serve.rs` | Asserts `/router.js` is served as JavaScript and `app.js` references it. |

---

### Task 1: Record the two spec amendments

Documentation only. Both amendments change the route table that Tasks 4–5 implement, so they land first.

**Files:**
- Modify: `docs/superpowers/specs/2026-08-10-frontend-hash-routing-design.md`

**Interfaces:**
- Consumes: nothing.
- Produces: the authoritative route table that Tasks 2–6 implement.

- [ ] **Step 1: Replace the compare row in the spec's route table**

Find this line in the `### Route table` section:

```markdown
| `#/duplicates/compare/:groupId` | `openDuplicates` + `openCompare(groupId)` |
```

Replace it with three rows:

```markdown
| `#/duplicates/compare/:groupId` | `openDuplicates` + `openCompare(groupId)` |
| `#/review/compare/:groupId` | `openReview` + `openCompare(groupId)` |
| `#/review/photo/:photoId/compare/:groupId` | `openReview` + `openDetail` + `openCompare(groupId)` |
```

- [ ] **Step 2: Add the amendments section**

Append to the end of the spec file:

```markdown
## Amendments

Recorded 2026-08-11, while writing the implementation plan.

### A1 — Compare has three parents, not one

The original route table gave compare a single route under `/duplicates`.
`openCompare` is in fact reached from three places — the duplicates list
(`duplicates.js:277`, `duplicates.js:544`), the review grid via `c`
(`review.js:778`), and the photo detail overlay via `c` (`detail.js:224`) —
and `compare.js:266-268` already branches on which one it was, returning to
the duplicates list only when that is where it came from. A single
`/duplicates`-parented route would silently change that. Compare therefore
gets one route per parent, listed in the route table above. `parentPath()`
in `router.js` is the single place that mapping lives.

### A2 — Analyze pushes on entry, replaces on exit

The "Analyze" section said `#/analyze` is "`replace`d rather than pushed",
but the worked example in the same paragraph — `#/libraries` → `#/analyze` →
`#/review` leaving two entries, with Back from review landing on libraries —
requires the opposite split: **entering** analyze pushes a history entry,
and **leaving** it (job done, "Review N so far", or a start failure)
replaces that entry rather than stacking a third. The worked example is
correct and is what gets implemented; the sentence before it was wrong.
```

- [ ] **Step 3: Commit**

```bash
git add docs/superpowers/specs/2026-08-10-frontend-hash-routing-design.md
git commit -m "docs(spec): amend hash-routing spec with compare parents and analyze push/replace"
```

---

### Task 2: The router module and the three screen routes

Builds `router.js` and converts the plain-screen navigation (libraries, review, duplicates) plus boot. Overlays and analyze are left alone in this task — they keep working exactly as they do today, just without URLs, and get wired in Tasks 3–6.

**Files:**
- Create: `crates/cli/assets/router.js`
- Modify: `crates/cli/assets/app.js:53-83`
- Modify: `crates/cli/assets/rail.js:4-10`
- Modify: `crates/cli/assets/libraries.js:117,122`
- Test: `crates/cli/tests/serve.rs`

**Interfaces:**
- Consumes: `window.pp.api`, `window.pp.state`, `window.pp.toast`, and the existing `window.pp.openLibraries` / `openReview` / `openDuplicates` / `startAnalyze` / `openDetail` / `openCompare` / `openExport` entry points.
- Produces, on `window.pp`:
  - `go(path: string, payload?: object): void` — push a history entry and apply the route.
  - `replace(path: string, payload?: object): void` — replace the current entry and apply.
  - `back(fallback: string): void` — step back one in-app entry, or `replace(fallback)` when there is none.
  - `setPath(path: string): void` — rewrite the URL only; do **not** re-apply. For when the screen already shows that state.
  - `routerPath(): string|null` — the path currently applied.
  - `startRouter(): void` — install listeners and resolve the initial hash. Called once, from `boot()`.
- The `payload` object is one-shot: handed to the applier, then discarded. Never written to the URL. Recognised keys: `{ folder, resume }` for `/analyze`, `{ pendingNew }` for `/review`, `{ fileIds }` for compare routes.
- Route appliers return either nothing (success) or a **path string to redirect to**. Appliers must never call `go`/`replace` themselves — the re-entrancy guard would swallow it.

- [ ] **Step 1: Create `crates/cli/assets/router.js`**

```js
// Hash router. Owns location.hash and is the single entry point for every
// screen and overlay transition. See
// docs/superpowers/specs/2026-08-10-frontend-hash-routing-design.md.
//
// This module imports nothing. app.js pulls it in through the same
// Promise.all as every other screen, so everything it needs is read off
// window.pp inside functions — the same way the screens reach each other.

let appliedPath = null; // the path currently on screen
let pending = null;     // one-shot payload for the next apply
let applying = false;   // re-entrancy guard

// ── Path parsing ─────────────────────────────────────────────────────────

function int(s) { return /^\d+$/.test(s) ? Number(s) : null; }

/**
 * Split a route path into a descriptor, or null for anything this app does
 * not serve — including a well-formed shape carrying a non-integer id.
 *   /review                       → { name: 'review' }
 *   /review/photo/482             → { name: 'photo',   photoId: 482 }
 *   /review/compare/17            → { name: 'compare', over: 'review',     groupId: 17 }
 *   /review/photo/482/compare/17  → { name: 'compare', over: 'photo',      groupId: 17, photoId: 482 }
 *   /duplicates/compare/17        → { name: 'compare', over: 'duplicates', groupId: 17 }
 */
export function parsePath(path) {
  const s = String(path || '').replace(/^#/, '').split('/').filter(Boolean);
  if (!s.length) return null;
  if (s.length === 1 && s[0] === 'libraries') return { name: 'libraries' };
  if (s.length === 1 && s[0] === 'analyze') return { name: 'analyze' };
  if (s.length === 1 && s[0] === 'export') return { name: 'export' };
  if (s[0] === 'review') {
    if (s.length === 1) return { name: 'review' };
    if (s[1] === 'photo' && int(s[2]) !== null) {
      if (s.length === 3) return { name: 'photo', photoId: int(s[2]) };
      if (s.length === 5 && s[3] === 'compare' && int(s[4]) !== null) {
        return { name: 'compare', over: 'photo', photoId: int(s[2]), groupId: int(s[4]) };
      }
      return null;
    }
    if (s.length === 3 && s[1] === 'compare' && int(s[2]) !== null) {
      return { name: 'compare', over: 'review', groupId: int(s[2]) };
    }
    return null;
  }
  if (s[0] === 'duplicates') {
    if (s.length === 1) return { name: 'duplicates' };
    if (s.length === 3 && s[1] === 'compare' && int(s[2]) !== null) {
      return { name: 'compare', over: 'duplicates', groupId: int(s[2]) };
    }
    return null;
  }
  return null;
}

/** The route one layer down — where Back from `r` lands, and where a failed
 *  applier falls back to. Amendment A1: compare's parent is whichever screen
 *  it was opened over, which is why `over` is part of the route. */
export function parentPath(r) {
  switch (r.name) {
    case 'photo': return '/review';
    case 'compare':
      if (r.over === 'photo') return `/review/photo/${r.photoId}`;
      if (r.over === 'review') return '/review';
      return '/duplicates';
    case 'export':
      return window.pp.state.view === 'duplicates' ? '/duplicates' : '/review';
    default: return '/libraries';
  }
}

// ── History mechanics ────────────────────────────────────────────────────

// ppDepth counts in-app entries behind the current one. It is stamped into
// history.state, so it survives a reload of a deep-linked URL — which is
// exactly right: after a reload the entries behind us are still there.
function depth() { return (history.state && history.state.ppDepth) || 0; }

export function go(path, payload) {
  if (path === appliedPath && !payload) return;
  history.pushState({ ppDepth: depth() + 1 }, '', `#${path}`);
  pending = payload || null;
  apply(path);
}

export function replace(path, payload) {
  history.replaceState({ ppDepth: depth() }, '', `#${path}`);
  pending = payload || null;
  apply(path);
}

/** Rewrite the URL without re-applying the route. For when the screen
 *  already shows the target state and re-running the applier would only
 *  refetch what is on it (detail.js's detailRefresh). */
export function setPath(path) {
  history.replaceState({ ppDepth: depth() }, '', `#${path}`);
  appliedPath = path;
}

/** Step back one in-app entry. When this page load entered the current route
 *  directly — a deep link or a hand-typed hash — there is nothing of ours
 *  behind it and history.back() would leave the app, so redirect instead. */
export function back(fallback) {
  if (depth() > 0) { history.back(); return; }
  replace(fallback);
}

export function routerPath() { return appliedPath; }

// ── Applying ─────────────────────────────────────────────────────────────

/** The active library folder, or null. Consolidates what boot() used to do:
 *  trust state.activeFolder, else ask the server once. */
async function ensureLibrary() {
  if (window.pp.state.activeFolder) return window.pp.state.activeFolder;
  try {
    const active = await window.pp.api('GET', '/api/active');
    if (active && active.folder) {
      window.pp.state.activeFolder = active.folder;
      return active.folder;
    }
  } catch (e) { /* no active library — fall through */ }
  return null;
}

/** Tear down every mounted overlay except `keep`. Top of the stack first:
 *  compare mounts above detail when opened from it with `c`. Each of these
 *  is a no-op when that overlay is closed, and `?.` covers the load order
 *  where a screen module has not registered yet. */
function closeOverlays(keep) {
  if (keep !== 'compare') window.pp.closeCompare?.();
  if (keep !== 'detail') window.pp.closeDetail?.();
  if (keep !== 'export') window.pp.closeExport?.();
}

const ROUTES = {
  async libraries() {
    closeOverlays(null);
    await window.pp.openLibraries();
  },

  async review(r, payload) {
    closeOverlays(null);
    const folder = await ensureLibrary();
    if (!folder) return '/libraries';
    await window.pp.openReview(folder, payload || {});
  },

  async duplicates() {
    closeOverlays(null);
    const folder = await ensureLibrary();
    if (!folder) return '/libraries';
    await window.pp.openDuplicates(folder);
  },
};

/**
 * Resolve `path` onto the screen. Unknown or empty paths redirect to the
 * default. Appliers signal failure by returning a fallback path; they must
 * not navigate themselves, because `applying` would swallow it — the
 * redirect happens here, after the guard is cleared.
 */
async function apply(path) {
  const r = parsePath(path);
  if (!r || !ROUTES[r.name]) { pending = null; await resolveDefault(); return; }
  if (applying) return;
  applying = true;
  appliedPath = path;
  const payload = pending;
  pending = null;
  let fallback = null;
  try {
    fallback = await ROUTES[r.name](r, payload);
  } finally {
    applying = false;
  }
  if (fallback) replace(fallback);
}

async function resolveDefault() {
  const folder = await ensureLibrary();
  replace(folder ? '/review' : '/libraries');
}

// Both listeners, deduped against appliedPath: Back/Forward fires popstate,
// while hand-editing the hash in the URL bar fires only hashchange.
function onPop() {
  const path = location.hash.replace(/^#/, '');
  if (path === appliedPath) return;
  pending = null;
  apply(path);
}

export function startRouter() {
  window.addEventListener('popstate', onPop);
  window.addEventListener('hashchange', onPop);
  const path = location.hash.replace(/^#/, '');
  // replace() rather than a bare apply(), so the first entry carries
  // ppDepth 0 and back() knows there is nothing of ours behind it.
  if (parsePath(path)) replace(path);
  else resolveDefault();
}

// ensureLibrary, closeOverlays, parentPath and ROUTES stay module-private —
// Tasks 3-6 add their appliers inside this file, not from the outside.
Object.assign(window.pp, { go, replace, back, setPath, routerPath, startRouter });
```

- [ ] **Step 2: Wire the router into `app.js`**

In `crates/cli/assets/app.js`, replace `boot()` (lines 53-63) with:

```js
async function boot() {
  // The router resolves location.hash — including the empty one, which it
  // turns into /review or /libraries depending on whether the server has an
  // active library. This is what the old GET /api/active dance did.
  window.pp.startRouter();
}
```

Then add `'/router.js'` as the **first** entry of the `Promise.all` array (line 68), so it registers before `boot()` runs:

```js
Promise.all([
  import('/router.js'),
  import('/rail.js'),
```

- [ ] **Step 3: Point the rail at the router**

In `crates/cli/assets/rail.js`, replace the `ITEMS` `go` callbacks (lines 5-8):

```js
const ITEMS = [
  { id: 'libraries',  label: 'Libraries',  ico: 'folder',   go: () => window.pp.go('/libraries') },
  { id: 'review',     label: 'Review',     ico: 'grid',     needsLib: true, go: () => window.pp.go('/review') },
  { id: 'duplicates', label: 'Duplicates', ico: 'layers',   needsLib: true, go: () => window.pp.go('/duplicates') },
  { id: 'export',     label: 'Export',     ico: 'download', needsLib: true, go: () => window.pp.go('/export') },
  { id: 'develop',    label: 'Develop — not yet available', ico: 'develop', soon: true },
];
```

Leave the `state` import in place — `renderRail` still reads `state.view` and `state.activeFolder`.

- [ ] **Step 4: Navigate when a library is opened**

In `crates/cli/assets/libraries.js`, replace the body of `openLibrary` after the fetch (lines 117 and 122):

```js
    if (e.status === 409) { window.pp.go('/analyze', { folder, resume: true }); return; }
    window.pp.toast({ kind: 'error', title: 'Could not open that library', body: e.message });
    return;
  }
  state.activeFolder = res.folder;
  window.pp.go('/review', { pendingNew: res.pending_new });
}
```

Note `openLibrary` no longer awaits the review load, so it can stop being `async` only if nothing else in it awaits — it still awaits `api('POST', '/api/open', …)`, so **keep `async`**.

- [ ] **Step 5: Add the Rust delivery test**

Append to `crates/cli/tests/serve.rs`:

```rust
/// The SPA is served from rust-embed, so a file that exists on disk but was
/// not embedded 404s at runtime with no build-time warning. Assert both that
/// router.js ships and that app.js actually pulls it in — a router nobody
/// imports is the failure mode this catches.
#[tokio::test]
async fn router_asset_is_served_and_imported() {
    use axum::body::to_bytes;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    let dir = tempfile::TempDir::new().unwrap();
    let catalog = pipeline::catalog::Catalog::open(&dir.path().join("c.duckdb")).unwrap();
    let cache = pipeline::cache::Cache::open(dir.path().join("cache")).unwrap();
    let state = app_state_active(catalog, cache);

    let resp = photopipe::serve::router(state.clone())
        .oneshot(
            Request::builder()
                .uri("/router.js")
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
        "text/javascript; charset=utf-8"
    );

    let resp = photopipe::serve::router(state)
        .oneshot(
            Request::builder()
                .uri("/app.js")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
    let app_js = String::from_utf8(body.to_vec()).unwrap();
    assert!(
        app_js.contains("/router.js"),
        "app.js does not import /router.js"
    );
    assert!(
        app_js.contains("startRouter"),
        "app.js never starts the router"
    );
}
```

- [ ] **Step 6: Run the Rust test**

```bash
cargo test --package photopipe --test serve router_asset_is_served_and_imported -- --nocapture
```

Expected: PASS. If `/router.js` 404s, the rust-embed folder glob is not picking it up — check the `#[derive(RustEmbed)]` `folder` attribute in `crates/cli/src/serve/handlers.rs` and that the file really is in `crates/cli/assets/`.

- [ ] **Step 7: Verify in the browser**

Start the server (`cargo run --release -- serve --folder <library>`), open the printed URL, and check each of these:

1. The URL becomes `#/review` on load (a library is active). No blank screen.
2. Rail → Duplicates → URL is `#/duplicates`, the duplicates list renders.
3. Browser **Back** → URL is `#/review`, the grid is back. **You do not leave the app.**
4. Browser **Forward** → `#/duplicates` again.
5. Rail → Libraries → `#/libraries`; open a library → `#/review` with that library's photos.
6. Reload on `#/duplicates` → the duplicates list, not the grid.
7. Type `#/nonsense` in the URL bar and press Enter → redirects to `#/review`.
8. Stop the server, restart it **without** `--folder`, reload → `#/libraries`.

Overlays (photo detail, compare, export) still open and close on Escape as before, just without URLs. That is expected at this task.

- [ ] **Step 8: Commit**

```bash
cargo fmt
cargo clippy --all-targets --all-features -- -D warnings
git add crates/cli/assets/router.js crates/cli/assets/app.js crates/cli/assets/rail.js \
        crates/cli/assets/libraries.js crates/cli/tests/serve.rs
git commit -m "feat(ui): hash router for the four top-level screens"
```

---

### Task 3: The analyze route

**Files:**
- Modify: `crates/cli/assets/router.js` (add the `analyze` applier)
- Modify: `crates/cli/assets/analyze.js:33,118,120,147`
- Modify: `crates/cli/assets/picker.js:87`
- Modify: `crates/cli/assets/review.js:509-512`

**Interfaces:**
- Consumes: `go`, `replace`, `back`, `routerEnsureLibrary` from Task 2.
- Produces: route `/analyze`, entered with payload `{ folder: string, resume?: boolean }`.
- Amendment A2: **entering** pushes, **leaving** replaces.

- [ ] **Step 1: Add the analyze applier**

In `crates/cli/assets/router.js`, add to the `ROUTES` object, after `libraries`:

```js
  async analyze(r, payload) {
    closeOverlays(null);
    if (payload && payload.folder) {
      await window.pp.startAnalyze(payload.folder, { resume: !!payload.resume });
      return;
    }
    // No payload means this route was restored — a reload, or Back into it.
    // Only meaningful while a job is genuinely in flight; a finished one is
    // not a screen worth returning to (spec amendment A2).
    let s = null;
    try { s = await window.pp.api('GET', '/api/analyze/status'); } catch (e) { /* below */ }
    const running = s && s.folder
      && s.stage !== 'idle' && s.stage !== 'done' && s.stage !== 'failed';
    if (running) {
      await window.pp.startAnalyze(s.folder, { resume: true });
      return;
    }
    return (await ensureLibrary()) ? '/review' : '/libraries';
  },
```

The stage strings match `JobState::running()` in `crates/cli/src/serve/mod.rs:56-58`.

- [ ] **Step 2: Make analyze leave by replacing**

In `crates/cli/assets/analyze.js`, four edits.

Line 33 — a failed start should not leave a dead analyze entry behind:

```js
      window.pp.replace('/libraries');
```

Line 118 — "Back to libraries" steps back if there is an entry, else redirects:

```js
  el.querySelector('#an-back').onclick = () => { stopPolling(); window.pp.back('/libraries'); };
```

Line 120 — "Review N so far" replaces the analyze entry:

```js
  if (rv) rv.onclick = () => { stopPolling(); window.pp.replace('/review'); };
```

Line 147 — job done. The folder the job finished on wins, and it has to reach `state.activeFolder` before the route is applied, because the `/review` applier reads it:

```js
      state.activeFolder = s.folder || folder;
      window.pp.replace('/review');
```

- [ ] **Step 3: Navigate from the picker and the empty-review prompt**

`crates/cli/assets/picker.js` line 87:

```js
    go.onclick = () => { m.close(); window.pp.go('/analyze', { folder: cur }); };
```

`crates/cli/assets/review.js` lines 509-512:

```js
    el('rv-analyze').onclick = () => {
      if (state.activeFolder) window.pp.go('/analyze', { folder: state.activeFolder });
      else window.pp.openPicker(null);
    };
```

And `review.js` line 915, the "new photos in this folder" banner action:

```js
      actions: [{ label: 'Re-analyze', onClick: () => window.pp.go('/analyze', { folder }) }],
```

- [ ] **Step 4: Verify in the browser**

You need a folder that is **not** yet analyzed, or one with new photos in it. Ask the user for one rather than inventing files.

1. Libraries → pick an un-analyzed folder → URL is `#/analyze`, the progress card renders.
2. While it runs, press **Back** → `#/libraries`. Press **Forward** → `#/analyze`, and the progress card re-attaches to the running job (it does not restart it — watch the percentage continue, not reset).
3. Reload while the job runs → `#/analyze`, progress still attached.
4. Let the job finish → URL becomes `#/review` on its own.
5. Press **Back** → `#/libraries`, **not** the finished analyze screen. This is amendment A2 working.
6. Navigate to `#/analyze` by hand with no job running → redirects to `#/review` (or `#/libraries` with no active library).

- [ ] **Step 5: Commit**

```bash
git add crates/cli/assets/router.js crates/cli/assets/analyze.js \
        crates/cli/assets/picker.js crates/cli/assets/review.js
git commit -m "feat(ui): route the analyze screen, pushing on entry and replacing on exit"
```

---

### Task 4: The photo detail route

**Files:**
- Modify: `crates/cli/assets/router.js` (add the `photo` applier)
- Modify: `crates/cli/assets/detail.js:164-195,405,452,482,504,523,536,542`
- Modify: `crates/cli/assets/review.js:563-573,830`

**Interfaces:**
- Consumes: `go`, `back`, `setPath`, `routerPath` from Task 2.
- Produces: route `/review/photo/:photoId` where `photoId` is a `file_id`.
- `closeDetail()` stays a **pure unmount** — the router calls it during teardown and it must not navigate. The new `dismissDetail()` is what user-facing close paths call.

- [ ] **Step 1: Add the photo applier**

In `crates/cli/assets/router.js`, add to `ROUTES` after `review`:

```js
  async photo(r) {
    closeOverlays('detail');
    const folder = await ensureLibrary();
    if (!folder) return '/libraries';
    // Only render the grid underneath when it is not already there —
    // openReview() re-runs load(), which would refetch on every arrow key.
    if (window.pp.state.view !== 'review') await window.pp.openReview(folder);
    const list = window.pp.reviewPhotos();
    const i = list.findIndex((p) => p.file_id === r.photoId);
    if (i < 0) {
      window.pp.toast({
        kind: 'info',
        title: 'That photo is not in the current view',
        body: 'It may be filtered out, or it belongs to a different library.',
      });
      return '/review';
    }
    window.pp.openDetail(i);
  },
```

- [ ] **Step 2: Split dismiss from close in `detail.js`**

`openDetail` (line 164) is now the applier and stays as it is. Add `dismissDetail` immediately after `closeDetail` (after line 182):

```js
/** What every user-facing close path calls: Escape, the ✕ button, `f`, and
 *  the defensive "the list emptied under us" branches. The unmount itself
 *  happens in closeDetail(), which the router calls when it applies the
 *  parent route — so Escape and Back are literally the same operation.
 *  closeDetail() must stay a pure unmount or this recurses. */
function dismissDetail() {
  window.pp.back('/review');
}
```

- [ ] **Step 3: Point every close path at `dismissDetail`**

Four call sites in `detail.js`:

Line 405, inside `render()`:

```js
  if (!p) { dismissDetail(); return; } // defensive: list emptied under us
```

Line 452, inside `wire()`:

```js
  el('dt-esc').onclick = dismissDetail;
```

Line 482, the Escape branch of `onKey`:

```js
  if (e.key === 'Escape') { e.stopPropagation(); dismissDetail(); return; }
```

Line 504, the `f` branch of `onKey` — the existing comment about `stopPropagation` still applies verbatim, keep it:

```js
  if (k === 'f' || k === 'F') { e.stopPropagation(); dismissDetail(); return; }
```

Line 523, inside `detailRefresh()`:

```js
  if (!list.length) { dismissDetail(); return; }
```

- [ ] **Step 4: Make stepping push a history entry**

Replace `move()` (lines 187-195) with:

```js
/** Stepping pushes one history entry per photo, so Back retraces the frames
 *  you looked at. The router's photo applier is what actually calls
 *  openDetail() — this only names the destination. */
async function move(d) {
  const list = window.pp.reviewPhotos();
  if (!list.length) return;
  const n = Math.max(0, Math.min(list.length - 1, idx + d));
  if (n === idx) return;
  window.pp.go(`/review/photo/${list[n].file_id}`);
}
```

- [ ] **Step 5: Keep the URL truthful when the list shifts under the overlay**

In `detailRefresh()` (lines 528-540), after each of the two `window.pp.reviewSetIndex(idx)` calls, the URL may now name a photo that is no longer at `idx`. Use `setPath`, not `replace` — the screen already shows the right thing and re-applying would only refetch the dump. Replace the tail of `detailRefresh` (from `const i = shownFileId == null …` to the end of the function) with:

```js
  const i = shownFileId == null ? -1 : list.findIndex((p) => p.file_id === shownFileId);
  if (i >= 0) {
    idx = i;
    window.pp.reviewSetIndex(idx);
    window.pp.setPath(`/review/photo/${list[idx].file_id}`);
    render(); // same photo, same dump — only the decision changed
    return;
  }
  // The photo left the filtered list. Stay open on whatever occupies that slot
  // now, but refetch the dump so the side panel is never another photo's.
  idx = Math.max(0, Math.min(list.length - 1, idx));
  window.pp.reviewSetIndex(idx);
  window.pp.setPath(`/review/photo/${list[idx].file_id}`);
  loadDump();
}
```

- [ ] **Step 6: Open detail through the router from review**

`crates/cli/assets/review.js`, the tile click handler (lines 564-573):

```js
  el('rv-tiles').onclick = (e) => {
    const t = e.target.closest('.tile');
    if (!t) return;
    const i = Number(t.dataset.i);
    if (!Number.isNaN(i) && photos[i]) {
      cursor = i;
      window.pp.go(`/review/photo/${photos[i].file_id}`);
    }
  };
```

And the `f` key (line 830):

```js
  if (k === 'f' || k === 'F') { if (p) window.pp.go(`/review/photo/${p.file_id}`); return; }
```

`p` is already in scope at that point in `onKey` — confirm it is the photo under the cursor before relying on it.

- [ ] **Step 7: Verify in the browser**

1. On `#/review`, click a tile → URL becomes `#/review/photo/<id>`, the overlay opens.
2. Press **Escape** → back on `#/review`, grid visible.
3. Open a photo again, press **Back** → same result. Escape and Back are interchangeable.
4. Open a photo, arrow **Right** three times → URL tracks each photo; press **Back** three times → you retrace 3 → 2 → 1, then a fourth Back closes the overlay onto the grid.
5. Copy a `#/review/photo/<id>` URL, reload → the grid loads *and* that photo's overlay opens on top.
6. Open that URL with a `<id>` that does not exist (e.g. 999999) → info toast, URL falls back to `#/review`, grid only.
7. Turn on "undecided only", open a photo, press Space to keep it (it leaves the filter) → the overlay stays open on the next frame and the URL updates to that frame's id without adding a history entry.
8. With the overlay open, press the rail's Duplicates → overlay tears down, `#/duplicates`. Back → the photo overlay is restored.

- [ ] **Step 8: Commit**

```bash
git add crates/cli/assets/router.js crates/cli/assets/detail.js crates/cli/assets/review.js
git commit -m "feat(ui): route the photo detail overlay, one history entry per frame"
```

---

### Task 5: The compare routes

Implements amendment A1 — three routes, one per parent.

**Files:**
- Modify: `crates/cli/assets/router.js` (add the `compare` applier)
- Modify: `crates/cli/assets/compare.js:230,257,266-268,329,349-360,370-422`
- Modify: `crates/cli/assets/review.js:778`
- Modify: `crates/cli/assets/duplicates.js:277,544`
- Modify: `crates/cli/assets/detail.js:224`

**Interfaces:**
- Consumes: `go`, `back`, `routerPath`, `parentPath` from Task 2.
- Produces: routes `/duplicates/compare/:groupId`, `/review/compare/:groupId`, `/review/photo/:photoId/compare/:groupId`.
- **`openCompare(groupId, fileIds)` must now return `true` on success and `false` on every bail-out path.** The router uses it to decide whether the URL may stay on the compare route.
- `closeCompare()` becomes exported and registered on `window.pp` (the router's teardown needs it). It stays a pure unmount.

- [ ] **Step 1: Add the compare applier**

In `crates/cli/assets/router.js`, add to `ROUTES` after `photo`:

```js
  async compare(r, payload) {
    closeOverlays('compare');
    const folder = await ensureLibrary();
    if (!folder) return '/libraries';
    // Restore the layer this route names as sitting underneath, so Back out
    // of compare lands where the user opened it from (amendment A1).
    if (r.over === 'duplicates') {
      if (window.pp.state.view !== 'duplicates') await window.pp.openDuplicates(folder);
    } else {
      if (window.pp.state.view !== 'review') await window.pp.openReview(folder);
      if (r.over === 'photo') {
        const list = window.pp.reviewPhotos();
        const i = list.findIndex((p) => p.file_id === r.photoId);
        if (i < 0) return '/review';
        window.pp.openDetail(i);
      }
    }
    // fileIds is one-shot: absent on a Back/forward restore or a reload, and
    // openCompare then falls back to the group's first two members.
    const ok = await window.pp.openCompare(r.groupId, payload && payload.fileIds);
    if (!ok) return parentPath(r);
  },
```

- [ ] **Step 2: Make `openCompare` report success**

In `crates/cli/assets/compare.js`, `openCompare` (line 370 onward) gains a return value on all four exits. The three early bail-outs each get `return false`:

```js
  } catch (e) {
    if (state.activeFolder !== targetFolder) return false; // switched libraries mid-fetch
    window.pp.toast({ kind: 'error', title: 'Could not open compare', body: e.message });
    return false;
  }
  if (state.activeFolder !== targetFolder) return false; // switched libraries mid-fetch
```

```js
  if (!c) {
    window.pp.toast({
      kind: 'error',
      title: 'Could not open compare',
      body: `Cluster ${targetGroupId} was not found in this library.`,
    });
    return false;
  }
```

```js
  if (chosen.length < 2) {
    window.pp.toast({
      kind: 'info',
      title: 'Nothing to compare',
      body: 'This group has one frame.',
    });
    return false;
  }
```

and the success path, at the very end of the function (after `loadExif();`):

```js
  mount();
  render();
  loadExif();
  return true;
}
```

Update the JSDoc above `openCompare` to say so — append to the existing block:

```js
 * Returns true when the overlay mounted, false on every bail-out. The router
 * needs that answer: a false leaves the URL sitting on a compare route with
 * nothing on screen, so it redirects to the parent instead.
```

- [ ] **Step 3: Export `closeCompare` and add `dismissCompare`**

`closeCompare` (line 349) becomes exported and stays a pure unmount:

```js
export function closeCompare() {
```

Add `dismissCompare` immediately after it:

```js
/** The user-facing close: Escape, the ✕ button, and the post-decision exit.
 *  The unmount happens in closeCompare() when the router applies the parent
 *  route, so Escape and Back do the same thing. */
function dismissCompare() {
  window.pp.back('/duplicates');
}
```

The `'/duplicates'` fallback only fires on a deep link straight into a compare route with no in-app entry behind it, and only for the duplicates-parented route in practice; the review-parented routes deep-link with `over: 'review'`/`'photo'`, and `back()` prefers a real history entry whenever one exists.

Register it on the module's exports line (line 422):

```js
Object.assign(window.pp, { openCompare, closeCompare });
```

- [ ] **Step 4: Point compare's close paths at `dismissCompare`**

Line 230, in the render wiring:

```js
  el('cmp-close').onclick = dismissCompare;
```

Line 329, the Escape branch of `onKey` — keep the existing `stopPropagation` comment:

```js
  if (e.key === 'Escape') { e.stopPropagation(); dismissCompare(); return; }
```

Line 408, the defensive double-invoke guard inside `openCompare`, stays a **pure** `closeCompare()` — it is re-entering with fresh state, not leaving the route:

```js
  if (root) closeCompare(); // defensive: guard against a double-invoke re-entering with stale state
```

- [ ] **Step 5: Rework `setKeeper`'s exit**

In `setKeeper` (lines 255-277), the manual "go back to duplicates" branch is now the router's job — leaving `#/duplicates/compare/17` re-applies `/duplicates`, which re-runs `openDuplicates` and its `load()`. Replace lines 257 and 264-270 so the function reads:

```js
async function setKeeper(fileId) {
  if (deciding) return;
  if (state.activeFolder !== openedFolder) { dismissCompare(); return; }
  deciding = true;
  try {
    await window.pp.reviewApply(fileId, 'keeper');
  } finally {
    deciding = false;
  }
  const stillSameFolder = state.activeFolder === openedFolder;
  const wasOnDuplicates = state.view === 'duplicates';
  dismissCompare();
  if (!stillSameFolder) return; // switched libraries mid-write; nothing safe to refresh
  // Leaving a /duplicates/compare route re-applies /duplicates, and
  // openDuplicates() reloads the clusters itself — nothing to do here.
  if (wasOnDuplicates) return;
  // Await the reload, then repaint the detail overlay if one is still mounted
  // underneath (compare can be opened from detail with `c`). Without this the
  // panel behind keeps reading "Undecided" for a frame that is now keeper or
  // reject, and its `idx` indexes the array reviewReload() just replaced.
  // detailRefresh is a no-op when detail is closed; `?.` covers the load order
  // where detail.js has not registered it yet.
  await window.pp.reviewReload();
  window.pp.detailRefresh?.();
}
```

- [ ] **Step 6: Navigate to compare from all three parents**

`crates/cli/assets/duplicates.js` line 277:

```js
    if (compareBtn) { window.pp.go(`/duplicates/compare/${Number(compareBtn.dataset.compare)}`); return; }
```

`crates/cli/assets/duplicates.js` line 544:

```js
    window.pp.go(`/duplicates/compare/${first.group_id}`);
```

`crates/cli/assets/review.js` line 778, at the end of `compareCursor()`:

```js
  window.pp.go(`/review/compare/${p.group_id}`);
```

`crates/cli/assets/detail.js` line 224, at the end of `compare()` — this one is nested under the photo route, which is how Back from compare returns to the still-open detail overlay:

```js
  const p2 = current();
  if (!p2) return;
  window.pp.go(`/review/photo/${p2.file_id}/compare/${gid}`);
```

(`current()` is re-read here rather than reusing `p` from the top of the function, because `dump` may have resolved in between; if you prefer, reuse the existing `p` — but then guard it with `if (!p) return;` before navigating.)

- [ ] **Step 7: Verify in the browser**

Needs a library with at least one duplicate group of two or more frames.

1. `#/duplicates` → click a cluster's Compare → URL `#/duplicates/compare/<gid>`, two panes.
2. **Escape** → `#/duplicates`. **Back** from compare does the same.
3. Press `a` in compare to set a keeper → compare closes, you land on `#/duplicates`, and the cluster shows its new keeper (the list reloaded).
4. `#/review` → put the cursor on a photo in a duplicate group, press `c` → URL `#/review/compare/<gid>`. Back → `#/review`, **not** duplicates.
5. Open a photo detail, press `c` → URL `#/review/photo/<id>/compare/<gid>`. Back → the detail overlay is still there, compare is gone.
6. From step 5, press `a` → compare closes onto the detail overlay, whose decision bar shows the new keeper state (not "Undecided").
7. Reload on a `#/review/photo/<id>/compare/<gid>` URL → grid, then detail, then compare, all three stacked.
8. Hand-type `#/duplicates/compare/999999` → error toast, URL falls back to `#/duplicates`.
9. Compare a group with only one resolvable frame (if you have one) → info toast, URL falls back to the parent, no half-rendered overlay.

- [ ] **Step 8: Commit**

```bash
git add crates/cli/assets/router.js crates/cli/assets/compare.js \
        crates/cli/assets/review.js crates/cli/assets/duplicates.js crates/cli/assets/detail.js
git commit -m "feat(ui): route compare under whichever screen opened it"
```

---

### Task 6: The export route

**Files:**
- Modify: `crates/cli/assets/router.js` (add the `export` applier)
- Modify: `crates/cli/assets/export.js:4-90`
- Modify: `crates/cli/assets/review.js:563,872`

**Interfaces:**
- Consumes: `go`, `back`, `routerPath`, `parentPath` from Task 2.
- Produces: route `/export`.
- **`openExport()` must return `true` when the modal mounted and `false` on both bail-outs** (estimate fetch failed; nothing to export).
- **`closeExport()`** — new, registered on `window.pp`. Pure unmount for the router's teardown.

- [ ] **Step 1: Add the export applier**

In `crates/cli/assets/router.js`, add to `ROUTES` after `duplicates`:

```js
  async export(r) {
    closeOverlays('export');
    const folder = await ensureLibrary();
    if (!folder) return '/libraries';
    // Export is reachable from the review topbar and from the rail, so the
    // screen underneath is review or duplicates — whichever is already up.
    // A cold reload of #/export has neither, and gets review.
    if (window.pp.state.view !== 'review' && window.pp.state.view !== 'duplicates') {
      await window.pp.openReview(folder);
    }
    const ok = await window.pp.openExport();
    // parentPath reads state.view, so it must run after the screen is up.
    if (!ok) return parentPath(r);
  },
```

- [ ] **Step 2: Give `export.js` a close handle and a return value**

In `crates/cli/assets/export.js`, add a module-level handle above `openExport`:

```js
// The live modal's close function, or null. The router needs to be able to
// tear this down when it applies a different route.
let closeFn = null;

/** Pure unmount, for the router's overlay teardown. Never navigates — the
 *  onClose hook below checks the current route precisely so that a teardown
 *  initiated by the router does not bounce it somewhere else. */
export function closeExport() {
  const c = closeFn;
  closeFn = null;
  if (c) c();
}
```

Both early bail-outs return `false`:

```js
  } catch (e) {
    window.pp.toast({ kind: 'error', title: 'Could not size the export', body: e.message });
    return false;
  }

  if (!est.files) {
    window.pp.toast({
      kind: 'info',
      title: 'Nothing new to export',
      body: 'No kept or keeper photos are waiting — any you already exported are in _keepers.',
    });
    return false;
  }
```

- [ ] **Step 3: Route the modal's dismissal through history**

Add an `onClose` to the `window.pp.modal({...})` call and capture its handle. The modal already closes itself on Escape and on a scrim click, so this one hook covers every dismissal path including the ✕:

```js
    width: 520,
    onClose: () => {
      closeFn = null;
      // Only navigate when this close came from the user. When the router
      // tore the modal down on its way somewhere else, it had already moved
      // the current route off /export, and this is a no-op.
      if (window.pp.routerPath() === '/export') window.pp.back('/review');
    },
    body: `
```

Immediately after the `const m = window.pp.modal({...});` call:

```js
  closeFn = m.close;
```

And at the very end of `openExport`, after the `m.el.addEventListener('keydown', …)` block:

```js
  return true;
}
```

- [ ] **Step 4: Fix the retry action**

The retry toast fires after `m.close()` has already navigated back, so it must re-enter through the router rather than calling `openExport` directly:

```js
        actions: [{ label: 'Retry', onClick: () => { window.pp.go('/export'); } }],
```

- [ ] **Step 5: Register `closeExport`**

Line 90:

```js
Object.assign(window.pp, { openExport, closeExport });
```

- [ ] **Step 6: Navigate to export from review**

`crates/cli/assets/review.js` line 563:

```js
    el('rv-export-2').onclick = () => window.pp.go('/export');
```

and line 872:

```js
  el('rv-export').onclick = () => window.pp.go('/export');
```

- [ ] **Step 7: Verify in the browser**

1. On `#/review` with keepers waiting, click Export → URL `#/export`, modal up.
2. **Escape** → `#/review`, modal gone. **Back** does the same. Clicking the scrim does the same.
3. On `#/duplicates`, rail → Export → `#/export`; Escape → back on `#/duplicates`, not review.
4. Reload on `#/export` → the review grid renders and the modal opens on top of it.
5. With **nothing** to export (decide nothing, or export everything first), click Export → info toast, and the URL stays on `#/review` rather than sitting on `#/export` with no modal.
6. Run an export to completion → the modal closes, the success toast shows, and the URL is back on `#/review`.

- [ ] **Step 8: Commit**

```bash
git add crates/cli/assets/router.js crates/cli/assets/export.js crates/cli/assets/review.js
git commit -m "feat(ui): route the export modal"
```

---

### Task 7: Full-suite verification and the README note

**Files:**
- Modify: `README.md` (the `photopipe serve` section)

**Interfaces:**
- Consumes: everything from Tasks 2–6.
- Produces: a verified branch.

- [ ] **Step 1: Run the whole gate**

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all
```

Expected: all three clean. Baseline on this branch before any of this work was **151 passed, 0 failed, 4 ignored**; Task 2 adds one, so expect **152 passed**. Any other delta is a regression — investigate before continuing.

- [ ] **Step 2: Run the cross-cutting browser pass**

The per-task checks covered each route in isolation. These are the interactions between them:

1. Walk `#/libraries` → open library → `#/review` → photo → compare → and press Back four times. You should retrace compare → photo → review → libraries and **never leave the app**.
2. From a deep-linked `#/review/photo/<id>` in a **fresh tab**, press Escape. There is no in-app entry behind it, so `back()` must redirect to `#/review` rather than closing the tab. This is the deep-link guard; it is the single most likely thing to be wrong.
3. Same as 2 but for `#/duplicates/compare/<gid>` and `#/export` in fresh tabs.
4. Switch libraries (Libraries → a different library) while on `#/review`, then press Back. You land on `#/libraries` — the spec accepts that Back across a library switch does not restore the previous library.
5. Hold the Back button down from deep inside a detail cull run — you should walk out photo by photo and stop at `#/review`, never past the app.
6. Toggle the theme on each screen and reload; the theme still applies before first paint (the inline script in `index.html` is untouched by this work, so this is a regression check only).

- [ ] **Step 3: Note the URLs in the README**

In the `photopipe serve` section of `README.md`, after the sentence describing the printed URL, add:

```markdown
Screens are addressable: `#/libraries`, `#/review`, `#/review/photo/<file_id>`,
`#/duplicates`, `#/duplicates/compare/<group_id>`, and `#/export`. The browser's
Back button steps back through screens and closes overlays rather than leaving
the app, and reloading returns you to the screen you were on.
```

Match the surrounding heading level and prose style; if the section reads differently from what is described here, adapt the wording rather than pasting it verbatim.

- [ ] **Step 4: Commit**

```bash
git add README.md
git commit -m "docs(readme): document the review UI's addressable screens"
```

---

## Self-review notes

Checked against the spec:

- Scope (four screens + three overlays), hash URLs, library-not-in-URL, and push-per-detail-step are each implemented in Tasks 2–6.
- The spec's error-handling list is covered: export/compare bail-outs (Tasks 5–6, appliers return a fallback path), the stale `photo/:id` (Task 4 step 1), malformed ids (`parsePath` returns null for a non-integer, which routes to the default), and the library-switch reset (unchanged existing behaviour).
- The spec's "Files touched" list matches the File Structure table above, plus `picker.js`, which the spec omitted — it holds one `startAnalyze` call site (`picker.js:87`) and is handled in Task 3.
- Naming is consistent across tasks: `go`/`replace`/`back`/`setPath`/`routerPath`/`startRouter` are defined in Task 2 and used under exactly those names afterwards; `closeDetail`/`closeCompare`/`closeExport` are pure unmounts throughout, with `dismissDetail`/`dismissCompare` and export's `onClose` as the user-facing counterparts.
