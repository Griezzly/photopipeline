// Screen 1h — the photo detail overlay (docs/design/mockups/Photopipe.dc.html:510-604).
// A full-surface layer mounted in #modal-host so review.js's grid keydown
// handler stands down (it checks #modal-host.children.length) while this is
// open. Everything about the *current photo* (path, decision, flags, score)
// comes from the live ReviewListItem the grid already holds — reviewApply
// patches that same object, so the decision bar here and the tile behind it
// never disagree. Everything that row does not carry (EXIF, file size/format,
// per-flag confidence, duplicate-group membership) comes from a dedicated
// GET /api/photos/:id fetch ("the dump") issued each time the photo changes.
import { api, humanBytes, state } from '/app.js';
import { icon } from '/icons.js';

// Mirrors review.js's FLAG_META exactly (not exported from there, so this is
// a deliberate, small duplication rather than a new cross-module dependency).
const FLAG_META = [
  { key: 'blur', code: 'BLR', long: 'Blur' },
  { key: 'back_focus', code: 'BF', long: 'Back focus' },
  { key: 'overexposed', code: 'OE', long: 'Overexposed' },
  { key: 'underexposed', code: 'UE', long: 'Underexposed' },
  { key: 'low_iqa', code: 'IQA', long: 'Low quality score' },
];

let root = null; // the mounted .detail element, or null when closed
let idx = 0;
let dump = null; // the /api/photos/:id response for the current photo
let shownFileId = null; // the file `dump` and the stage belong to
let dumpState = 'loading'; // 'loading' | 'ready' | 'error'
let zoom = 'fit'; // 'fit' | 1 | 2
let deciding = false; // one decision in flight at a time, as in review.js

// /api/photos/:id has no notion of "how many frames are in this group" (it
// only lists the group ids a file belongs to). That count only exists on
// GET /api/photos (ReviewListItem.group_id, one row per file) or
// GET /api/clusters. Rather than add either call to every detail open, the
// full list is fetched lazily, the first time a grouped photo is shown, and
// cached for the rest of the time the user stays on that library.
//
// The cache MUST be scoped to the active library: group_id comes from a
// per-catalog SEQUENCE (schema.rs), so every library numbers its groups from
// 1 — library A's group 1 and library B's group 1 are unrelated. Switching
// libraries is a pure SPA transition (no location.reload anywhere), so this
// module's state survives it, and a cache keyed only by group_id would print
// a real-looking but fabricated frame count for the new library. `state`
// (imported from app.js) is updated synchronously by openReview() before any
// photo in the new library can be opened, so comparing against
// `state.activeFolder` is sufficient to detect the switch.
let groupSizeCache = null;
let groupSizePromise = null;
let groupSizeFolder = null; // the folder groupSizeCache/groupSizePromise were built for

/** Synchronously drop the cache the instant it's known to belong to a
 *  different library than the one now active. Called both before reading the
 *  cache in render() and before deciding whether to fetch, so there is no
 *  window where a stale cross-library count can reach the screen. */
function invalidateGroupSizesIfStale() {
  if (state.activeFolder !== groupSizeFolder) {
    groupSizeCache = null;
    groupSizePromise = null;
    groupSizeFolder = state.activeFolder;
  }
}

const el = (id) => document.getElementById(id);
const esc = (s) => String(s == null ? '' : s)
  .replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;');
// Group counts are thousands-separated here exactly as in review.js and
// duplicates.js — a 1234-frame group must not read "1234 frames" on this
// screen and "1,234 frames" on the next one.
const plural = (n, w) => `${n.toLocaleString()} ${w}${n === 1 ? '' : 's'}`;

function basename(path) {
  const s = String(path || '');
  const i = Math.max(s.lastIndexOf('/'), s.lastIndexOf('\\'));
  return i === -1 ? s : s.slice(i + 1);
}

function current() {
  const list = window.pp.reviewPhotos();
  return list[idx];
}

/** Same precedence as review.js's tile decisionOf: keeper beats plain keep. */
function decisionOf(p) {
  if (p.is_keeper) return 'keeper';
  if (p.verdict === 'keep') return 'keep';
  if (p.verdict === 'reject') return 'reject';
  return 'undecided';
}

async function ensureGroupSizes() {
  invalidateGroupSizesIfStale();
  if (groupSizeCache) return groupSizeCache;
  if (!groupSizePromise) {
    // The library the fetch is *for*, fixed at the moment it starts. If the
    // user switches libraries while this is in flight, invalidateGroupSizesIfStale()
    // will already have moved groupSizeFolder on — but this specific promise's
    // result still belongs to `forFolder`, and must never be written into the
    // (by-then-different) current cache. Without this guard, a late-arriving
    // response from the *previous* library can silently overwrite the cache
    // the *new* library just correctly populated, reopening the same
    // fabricated-frame-count bug through a narrower, timing-dependent door.
    const forFolder = state.activeFolder;
    groupSizePromise = api('GET', '/api/photos?limit=100000')
      .then((rows) => {
        const m = new Map();
        for (const r of rows) {
          if (r.group_id != null) m.set(r.group_id, (m.get(r.group_id) || 0) + 1);
        }
        if (forFolder !== state.activeFolder) return null; // stale: discard, don't cache
        groupSizeCache = m;
        return m;
      })
      .catch(() => {
        // Same guard as the .then above, applied to the shared `groupSizePromise`
        // slot rather than the cache: if a *newer* fetch (for the folder that's
        // current now) is already sitting in that slot, a failing stale fetch
        // must not clear it out from under it — that would just force a
        // redundant refetch, not a wrong number, but the discipline of "only
        // touch shared state if you're still the relevant fetch" should be
        // applied uniformly rather than only where a wrong number was proven.
        if (forFolder === state.activeFolder) groupSizePromise = null; // allow a retry
        return null;
      });
  }
  return groupSizePromise;
}

/** Fetch the dump for the photo now at `idx`. Never blanks the image: the
 *  stage is painted from the ReviewListItem before this resolves, and stays
 *  painted if it rejects. */
async function loadDump() {
  const p = current();
  if (!p) return;
  shownFileId = p.file_id;
  dumpState = 'loading';
  dump = null;
  render();
  try {
    dump = await api('GET', `/api/photos/${p.file_id}`);
    dumpState = 'ready';
  } catch (e) {
    dumpState = 'error';
    dump = null;
    window.pp.toast({ kind: 'error', title: 'Could not load photo details', body: e.message });
  }
  render();
  // Always route through ensureGroupSizes (not a `!groupSizeCache` shortcut):
  // that shortcut previously skipped the fetch — and the staleness check
  // inside it — whenever a cache from a *different* library was still
  // sitting there from an earlier visit, which is exactly how the fabricated
  // cross-library frame count happened.
  if (dumpState === 'ready' && dump.duplicate_groups.length) {
    ensureGroupSizes().then(() => render());
  }
}

function mount() {
  root = document.createElement('div');
  root.className = 'detail';
  document.getElementById('modal-host').appendChild(root);
  document.addEventListener('keydown', onKey, true);
}

export function openDetail(index) {
  const list = window.pp.reviewPhotos();
  if (!list.length) return;
  idx = Math.max(0, Math.min(list.length - 1, index));
  zoom = 'fit';
  if (!root) mount();
  window.pp.reviewSetIndex(idx);
  loadDump();
}

export function closeDetail() {
  if (!root) return;
  document.removeEventListener('keydown', onKey, true);
  if (document.fullscreenElement && root.contains(document.fullscreenElement)) {
    document.exitFullscreen().catch(() => {});
  }
  root.remove();
  root = null;
  dump = null;
  shownFileId = null;
  dumpState = 'loading';
}

/** What every user-facing close path calls: Escape, the ✕ button, `f`, and
 *  the defensive "the list emptied under us" branches. The unmount itself
 *  happens in closeDetail(), which the router calls when it applies the
 *  parent route. Stepping pushes one history entry per frame, so leaving
 *  goes through exitOverlay() — one hop across the whole run, not one step
 *  back to whatever frame was visited last — which is what makes Escape and
 *  Back interchangeable regardless of how far the run has gone.
 *  closeDetail() must stay a pure unmount or this recurses. */
function dismissDetail() {
  window.pp.exitOverlay('/review');
}

/** Stepping pushes one history entry per photo, so Back retraces the frames
 *  you looked at. The router's photo applier is what actually calls
 *  openDetail() — this only names the destination. An auto-repeating arrow
 *  (`coalesce`) is one held gesture, not N deliberate visits, and is routed
 *  through replace() instead of go() — see the call site in onKey(). */
async function move(d, coalesce) {
  const list = window.pp.reviewPhotos();
  if (!list.length) return;
  const n = Math.max(0, Math.min(list.length - 1, idx + d));
  if (n === idx) return;
  const path = `/review/photo/${list[n].file_id}`;
  // An auto-repeating arrow is one held gesture, not N deliberate visits.
  // Collapsing it into one entry keeps Back meaningful and keeps us under
  // the browser's pushState rate limit, which a held key crosses in seconds.
  if (coalesce) window.pp.replace(path);
  else window.pp.go(path);
}

/** Apply the current photo, then advance to the next frame unless `stay`. */
async function decide(action, stay, coalesce) {
  const p = current();
  if (!p || deciding) return;
  deciding = true;
  try {
    await window.pp.reviewApply(p.file_id, action);
  } finally {
    deciding = false;
  }
  if (stay) render();
  else await move(1, coalesce);
}

function compare() {
  const p = current();
  const gid = dumpState === 'ready' && dump.duplicate_groups.length
    ? dump.duplicate_groups[0]
    : (p ? p.group_id : null);
  if (gid == null) {
    window.pp.toast({
      kind: 'info',
      title: 'Nothing to compare',
      body: 'This photo is not in a duplicate group.',
    });
    return;
  }
  const p2 = current();
  if (!p2) return;
  window.pp.go(`/review/photo/${p2.file_id}/compare/${gid}`);
}

function toggleFullscreen() {
  if (document.fullscreenElement) { document.exitFullscreen().catch(() => {}); return; }
  if (root.requestFullscreen) root.requestFullscreen().catch(() => {});
}

/** Zoom is relative to the /preview/:id asset (webp, capped at 2048px on the
 *  long edge) — never the sensor-resolution original, which is why 100%/200%
 *  are computed from the loaded <img>'s naturalWidth rather than any EXIF
 *  dimension. 'fit' needs no measurement at all. */
function applyZoomStyle() {
  const img = el('dt-img');
  if (!img) return;
  if (zoom === 'fit') {
    img.style.width = '';
    img.style.maxWidth = '100%';
    img.style.maxHeight = '100%';
    img.style.objectFit = 'contain';
  } else {
    img.style.maxWidth = 'none';
    img.style.maxHeight = 'none';
    img.style.objectFit = '';
    if (img.naturalWidth) img.style.width = `${img.naturalWidth * zoom}px`;
  }
}

// ── Side-panel sections ─────────────────────────────────────────────────────

function renderDecisionSection(p) {
  const dec = decisionOf(p);
  const pills = [];
  if (dec === 'keep' || dec === 'keeper') pills.push('<span class="pill-keep">Keep</span>');
  else if (dec === 'reject') pills.push('<span class="pill-reject">Reject</span>');
  else pills.push('<span class="pill-undecided">Undecided</span>');
  if (p.is_keeper) {
    // `pick_keeper` sets is_keeper unconditionally, including for a file in no
    // duplicate group (Shift+K on an ungrouped photo). esc(null) renders as ''
    // so no literal "null" reaches the DOM, but "Keeper of group " truncates
    // mid-sentence — and it is permanent, because keeper outranks everything in
    // decisionOf(). Name the group only when there is one.
    const label = p.group_id != null ? `Keeper of group ${esc(p.group_id)}` : 'Keeper';
    pills.push(`<span class="pill-keeper">${icon('spark', 12, 2.2)}${label}</span>`);
  }
  return `<div class="side-sec">
    <div class="section-label">Decision</div>
    <div style="display:flex;align-items:center;gap:10px;margin-top:10px;flex-wrap:wrap">${pills.join('')}</div>
  </div>`;
}

function flagRow(meta) {
  const flags = dumpState === 'ready' ? dump.defect_flags : [];
  const match = flags.find((f) => f.flag_type === meta.key);
  if (match) {
    return `<div class="flag-row">
      <span class="chip-code">${esc(meta.code)}</span>
      <span style="flex:1;font:500 12.5px/1.2 var(--font)">${esc(meta.long)}</span>
      <span class="flag-conf">confidence ${match.confidence.toFixed(2)}</span>
    </div>`;
  }
  // The dump only carries flags that fired — there is no sub-threshold
  // confidence number for a flag that never matched, so none is shown here.
  return `<div class="flag-row off">
    <span class="chip-code">${esc(meta.code)}</span>
    <span style="flex:1;font:500 12.5px/1.2 var(--font)">${esc(meta.long)} — not flagged</span>
  </div>`;
}

function renderAnalysisSection() {
  let scoreBlock;
  let flagsBlock = '';
  let dupBlock = '';

  if (dumpState === 'ready' && dump.iqa) {
    const score = dump.iqa.score;
    const pct = Math.max(0, Math.min(100, Math.round(score * 100)));
    const rank = window.pp.reviewIqaRank(score);
    scoreBlock = `
      <div style="display:flex;align-items:baseline;gap:10px">
        <span class="iqa-n">${score.toFixed(2)}</span>
        <span class="iqa-note">quality score${rank ? `<br><span style="color:var(--text-dim)">${esc(rank)} of this library</span>` : ''}</span>
      </div>
      <div class="iqa-meter"><div style="width:${pct}%;height:100%;border-radius:var(--radius-pill);background:var(--accent-glow)"></div></div>`;
  } else {
    const note = dumpState === 'ready' ? 'models were not run'
      : dumpState === 'error' ? 'could not load analysis'
        : 'loading…';
    scoreBlock = `
      <div style="display:flex;align-items:baseline;gap:10px">
        <span class="iqa-n">—</span>
        <span class="iqa-note">quality score<br><span style="color:var(--text-dim)">${esc(note)}</span></span>
      </div>
      <div class="iqa-meter"></div>`;
  }

  if (dumpState === 'ready') {
    flagsBlock = `<div style="display:flex;flex-direction:column;gap:8px">
      ${FLAG_META.map(flagRow).join('')}
    </div>`;

    const gid = dump.duplicate_groups.length ? dump.duplicate_groups[0] : null;
    if (gid != null) {
      // Synchronous staleness check before the read: without this, a render()
      // that runs before the async ensureGroupSizes() from loadDump() settles
      // could still see a cache built for the *previous* library (group ids
      // collide across libraries — each catalog numbers its own groups from
      // 1 — so a stale hit here is a wrong-but-plausible number, not a miss).
      invalidateGroupSizesIfStale();
      const n = groupSizeCache ? groupSizeCache.get(gid) : null;
      dupBlock = `<div class="dup-card">
        <span style="color:var(--accent-tint-fg)">${icon('layers', 15, 1.9)}</span>
        <span style="flex:1;font:600 12.5px/1.2 var(--font)">Duplicate group ${esc(gid)}${n ? ` <span style="color:var(--text-dim);font-weight:500">· ${esc(plural(n, 'frame'))}</span>` : ''}</span>
        <span style="color:var(--accent-tint-fg);font:600 12px/1 var(--font);cursor:pointer" id="dt-compare">Compare</span>
      </div>`;
    }
  }

  return `<div class="side-sec">
    <div class="section-label" style="margin-bottom:12px">Analysis</div>
    ${scoreBlock}
    ${flagsBlock}
    ${dupBlock}
  </div>`;
}

function renderFileSection(p) {
  const path = dumpState === 'ready' ? dump.file.path : p.path;
  let meta;
  if (dumpState === 'ready') {
    const f = dump.file;
    const e = dump.exif;
    const dims = e && e.width != null && e.height != null ? ` · ${e.width} × ${e.height}` : '';
    meta = `${esc(f.file_format)} · ${esc(humanBytes(f.size_bytes))}${dims}`;
  } else {
    meta = dumpState === 'error' ? 'Could not load file info' : 'Loading…';
  }
  return `<div class="side-sec">
    <div class="section-label" style="margin-bottom:10px">File</div>
    <div class="file-path">${esc(path)}</div>
    <div style="margin-top:8px;font:500 12px/1 var(--font);color:var(--text-dim)">${meta}</div>
  </div>`;
}

function renderCameraSection() {
  if (dumpState !== 'ready') {
    const msg = dumpState === 'error' ? 'Could not load camera info' : 'Loading…';
    return `<div class="side-sec">
      <div class="section-label" style="margin-bottom:12px">Camera</div>
      <div style="font:500 12px/1.4 var(--font);color:var(--text-dim)">${esc(msg)}</div>
    </div>`;
  }
  const e = dump.exif;
  const rows = [];
  if (e) {
    const body = [e.camera_make, e.camera_model].filter(Boolean).join(' ');
    if (body) rows.push(['Body', body]);
    if (e.lens_model) rows.push(['Lens', e.lens_model]);
    if (e.focal_length_mm != null) rows.push(['Focal length', `${e.focal_length_mm} mm`]);
    if (e.aperture != null) rows.push(['Aperture', `f/${e.aperture}`]);
    if (e.shutter_seconds != null && e.shutter_seconds > 0) {
      const s = e.shutter_seconds;
      rows.push(['Shutter', s < 1 ? `1/${Math.round(1 / s)} s` : `${s} s`]);
    }
    if (e.iso != null) rows.push(['ISO', String(e.iso)]);
    if (e.captured_at != null) rows.push(['Captured', new Date(e.captured_at * 1000).toLocaleString()]);
  }
  const body = rows.length
    ? `<div class="kv">${rows.map(([k, v]) => `<span class="kv-k">${esc(k)}</span><span class="kv-v">${esc(v)}</span>`).join('')}</div>`
    : '<div style="font:500 12px/1.4 var(--font);color:var(--text-dim)">No camera EXIF for this file</div>';
  return `<div class="side-sec">
    <div class="section-label" style="margin-bottom:12px">Camera</div>
    ${body}
  </div>`;
}

// ── Shell ────────────────────────────────────────────────────────────────

function render() {
  if (!root) return;
  const p = current();
  if (!p) { dismissDetail(); return; } // defensive: list emptied under us
  const list = window.pp.reviewPhotos();
  const name = basename(p.path);

  root.innerHTML = `
    <div class="detail-main">
      <div class="detail-bar">
        <button class="btn sm" id="dt-esc">${icon('close', 13, 2)}Esc</button>
        <span class="detail-file">${esc(name)}</span>
        <span class="detail-pos">${idx + 1} / ${list.length}</span>
        <div class="topbar-gap"></div>
        <div class="seg">
          <button class="seg-btn ${zoom === 'fit' ? 'on' : ''}" data-zoom="fit">Fit</button>
          <button class="seg-btn ${zoom === 1 ? 'on' : ''}" data-zoom="1"
            title="100% of the 2048px preview, not the original">100%</button>
          <button class="seg-btn ${zoom === 2 ? 'on' : ''}" data-zoom="2">200%</button>
        </div>
        <button class="btn btn-icon" id="dt-full" title="Fullscreen" aria-label="Fullscreen">${icon('expand', 14, 1.8)}</button>
      </div>
      <div class="detail-stage" id="dt-stage">
        <img class="detail-img" id="dt-img" src="/preview/${p.file_id}" alt="${esc(name)}">
        <button class="detail-nav prev" id="dt-prev" aria-label="Previous photo">${icon('chevron-left', 18, 1.9)}</button>
        <button class="detail-nav next" id="dt-next" aria-label="Next photo">${icon('chevron-right', 18, 1.9)}</button>
      </div>
      <div class="decide-row">
        <div style="display:flex;gap:8px">
          <button class="btn btn-reject" id="dt-reject">Reject<span class="kbd">X</span></button>
          <button class="btn btn-keep" id="dt-keep">${icon('check', 14, 2.4)}Keep<span class="kbd">Space</span></button>
          <button class="btn btn-keeper" id="dt-keeper">★ Keeper<span class="kbd">K</span></button>
          <button class="btn" id="dt-undo">Undo<span class="kbd">U</span></button>
        </div>
        <div class="topbar-gap"></div>
        <span class="decide-note">Deciding advances to the next frame.<br>Hold Shift to stay put.</span>
      </div>
    </div>
    <div class="detail-side">
      ${renderDecisionSection(p)}
      ${renderAnalysisSection()}
      ${renderFileSection(p)}
      ${renderCameraSection()}
    </div>`;

  wire();
  applyZoomStyle();
}

function wire() {
  el('dt-esc').onclick = dismissDetail;
  el('dt-full').onclick = toggleFullscreen;
  el('dt-prev').onclick = () => move(-1);
  el('dt-next').onclick = () => move(1);
  for (const b of root.querySelectorAll('[data-zoom]')) {
    b.onclick = () => { zoom = b.dataset.zoom === 'fit' ? 'fit' : Number(b.dataset.zoom); render(); };
  }
  el('dt-reject').onclick = (e) => decide('reject', e.shiftKey);
  el('dt-keep').onclick = (e) => decide('keep', e.shiftKey);
  el('dt-keeper').onclick = (e) => decide('keeper', e.shiftKey);
  el('dt-undo').onclick = (e) => decide('undecide', e.shiftKey);
  const img = el('dt-img');
  img.addEventListener('load', applyZoomStyle);
  if (img.complete) applyZoomStyle();
  const cmp = el('dt-compare');
  if (cmp) cmp.onclick = compare;
}

// ── Keyboard ─────────────────────────────────────────────────────────────

function onKey(e) {
  // Stand down while another layer (compare) is mounted above this one in
  // #modal-host. review.js and duplicates.js do the equivalent; without it,
  // keys typed over compare also reach the photo hidden underneath.
  const host = el('modal-host');
  if (host && root && host.lastElementChild !== root) return;
  // stopPropagation for the same reason as the `f` branch below: without it a
  // single Escape closes this overlay *and* reaches review.js's handler, which
  // then also shuts the shortcut sheet or a filter popover the user left open
  // behind the overlay. One Escape, one layer. This is load-bearing only on
  // dismissDetail()'s synchronous deep-link path — replace() there runs
  // closeOverlays() in the same call stack and empties #modal-host before the
  // event bubbles. On the common history.go() path the root is still mounted
  // when review.js's own handler runs, and its own #modal-host.children.length
  // guard (review.js:785-786) already stands down — this call just does not
  // rely on that.
  if (e.key === 'Escape') { e.stopPropagation(); dismissDetail(); return; }
  // Same guard as review.js: modifier chords are browser/OS shortcuts
  // (Ctrl+X cut, Ctrl+U view-source, Cmd+F find, …), never decisions.
  if (e.ctrlKey || e.metaKey || e.altKey) return;

  const k = e.key;
  switch (k) {
    // preventDefault as review.js does: at 100%/200% the stage is scrollable,
    // so an unswallowed ArrowLeft/Right both navigates to the next photo and
    // nudges the outgoing image sideways first.
    case 'ArrowRight': case 'j': e.preventDefault(); move(1, e.repeat); return;
    case 'ArrowLeft': case 'k': e.preventDefault(); move(-1, e.repeat); return;
    default: break;
  }
  if (k === ' ') { e.preventDefault(); decide('keep', e.shiftKey, e.repeat); return; }
  if (k === 'x' || k === 'X') { decide('reject', e.shiftKey, e.repeat); return; }
  if (k === 'u' || k === 'U') { decide('undecide', e.shiftKey, e.repeat); return; }
  if (k === 'K') { decide('keeper', true, e.repeat); return; } // Shift is inherent in 'K'
  // Stop the event here: review.js's bubble-phase handler re-checks
  // #modal-host after this capture-phase listener runs. Whether that check
  // still needs stopping depends on which of dismissDetail()'s two paths
  // fires: when a run is behind us, exitOverlay() calls history.go(), which
  // is async — #modal-host is still populated when the event bubbles, and
  // review.js's own "stand down while a layer is mounted" guard would have
  // handled it even without this. But when there is no run behind (a deep
  // link straight into a photo route), exitOverlay() falls back to
  // replace(), which runs closeOverlays() — and therefore empties the host —
  // synchronously, in this same call stack. Without stopPropagation there,
  // review.js would see an empty host on the same event and immediately
  // reopen detail, making 'f' a no-op on exactly the path that has no other
  // way out.
  if (k === 'f' || k === 'F') { e.stopPropagation(); dismissDetail(); return; }
  if (k === 'c' || k === 'C') { compare(); }
}

/**
 * Repaint the overlay against whatever `window.pp.reviewPhotos()` now returns.
 * A no-op when detail is closed, so callers never have to know.
 *
 * This exists for compare.js: a keeper set from compare closes compare and
 * reloads the review list, but when compare was opened from *this* screen with
 * `c`, the .detail underneath is still mounted and would otherwise keep showing
 * the pre-decision state — "Undecided" for a frame that is now keeper or
 * reject — while `idx` indexes into a freshly replaced array behind stale DOM.
 * The header comment on this module claims the decision bar here and the tile
 * behind it can never disagree; this is what keeps that true across compare.
 */
/** Keep the URL pointed at the frame actually on screen — but only when the
 *  router is on a bare photo route. detailRefresh can run while a dismissal
 *  is queued but not yet landed (compare.js's setKeeper does exactly that),
 *  and setPath would then replace whichever entry is current. If that is the
 *  *compare* entry, the pending history.back() travels one entry too far and
 *  onPop finds nothing to apply — compare stays mounted forever with the URL
 *  naming the photo underneath. The anchored match is the whole point: a
 *  prefix test would also accept /review/photo/482/compare/17.
 */
function syncDetailPath(fileId) {
  if (/^\/review\/photo\/\d+$/.test(window.pp.routerPath() || '')) {
    window.pp.setPath(`/review/photo/${fileId}`);
  }
}

export function detailRefresh() {
  if (!root) return;
  const list = window.pp.reviewPhotos();
  if (!list.length) { dismissDetail(); return; }
  // Re-anchor on the file, not the index. reviewReload() rebuilds and re-filters
  // the list, so with "undecided only" active the frames compare just decided
  // drop out of it and every later index shifts — leaving `idx` pointing at a
  // different photo than the `dump` (EXIF, flags, group) already on screen.
  const i = shownFileId == null ? -1 : list.findIndex((p) => p.file_id === shownFileId);
  if (i >= 0) {
    idx = i;
    window.pp.reviewSetIndex(idx);
    syncDetailPath(list[idx].file_id);
    render(); // same photo, same dump — only the decision changed
    return;
  }
  // The photo left the filtered list. Stay open on whatever occupies that slot
  // now, but refetch the dump so the side panel is never another photo's.
  idx = Math.max(0, Math.min(list.length - 1, idx));
  window.pp.reviewSetIndex(idx);
  syncDetailPath(list[idx].file_id);
  loadDump();
}

Object.assign(window.pp, { openDetail, closeDetail, detailRefresh });
