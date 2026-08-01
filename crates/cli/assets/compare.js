// Screen 1j — compare mode (docs/design/mockups/Photopipe.dc.html:688-733).
// Two frames from one duplicate cluster, side by side, sharing one zoom level
// and mirroring each other's pan. Synced zoom/pan is the whole point of this
// screen — if it doesn't work, the overlay has no reason to exist.
//
// group_id and file_id both restart at 1 in every library's own catalog
// (per-catalog SEQUENCEs), and switching libraries is a pure SPA transition
// with no page reload, so every module-level variable here survives a switch.
// `openedFolder` is captured from state.activeFolder the instant openCompare
// is called, and every continuation after an `await` re-checks it before
// touching the DOM or writing a decision — the same pattern duplicates.js
// uses for its own `folder`/`openedFolder` guard.
import { api, state } from '/app.js';
import { icon } from '/icons.js';

// Mirrors review.js's / detail.js's FLAG_META exactly (not exported from
// either, so this is a deliberate small duplication rather than a new
// cross-module dependency).
const FLAG_META = [
  { key: 'blur', code: 'BLR' },
  { key: 'back_focus', code: 'BF' },
  { key: 'overexposed', code: 'OE' },
  { key: 'underexposed', code: 'UE' },
  { key: 'low_iqa', code: 'IQA' },
];

let root = null; // the mounted .cmp element, or null when closed
let openedFolder = null; // state.activeFolder at the moment this instance opened
let groupId = null;
let totalMembers = 0; // cluster.members.length, for "N of M frames"
let frames = []; // [{ item: ReviewListItem, exif: string|null, exifLoaded: bool }], length 2
let zoom = 'fit'; // 'fit' | 1
let syncing = false; // guards the scroll-mirror feedback loop
let deciding = false; // one keeper write in flight at a time

const el = (id) => document.getElementById(id);
const esc = (s) => String(s == null ? '' : s)
  .replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;');

function basename(path) {
  const s = String(path || '');
  const i = Math.max(s.lastIndexOf('/'), s.lastIndexOf('\\'));
  return i === -1 ? s : s.slice(i + 1);
}

function basenameNoExt(path) {
  const b = basename(path);
  const i = b.lastIndexOf('.');
  return i > 0 ? b.slice(0, i) : b;
}

/** "1/640 s · f/2.0 · ISO 800" from a /api/photos/:id dump, or null if the
 *  dump carries no EXIF at all. Never fabricates a missing field. */
function exifLine(dump) {
  if (!dump || !dump.exif) return null;
  const e = dump.exif;
  const parts = [];
  if (e.shutter_seconds != null && e.shutter_seconds > 0) {
    parts.push(e.shutter_seconds < 1 ? `1/${Math.round(1 / e.shutter_seconds)} s` : `${e.shutter_seconds} s`);
  }
  if (e.aperture != null) parts.push(`f/${e.aperture}`);
  if (e.iso != null) parts.push(`ISO ${e.iso}`);
  return parts.length ? parts.join(' · ') : null;
}

function flagChipsHtml(item) {
  return FLAG_META.filter((m) => (item.flags || []).includes(m.key))
    .map((m) => `<span class="chip-code">${esc(m.code)}</span>`).join('');
}

/** The pane whose member has the strictly higher iqa_score, or null if the
 *  scores tie or either is missing. Never a guess — rule 2's "unambiguous or
 *  absent" is implemented as exactly this: a strict `>` comparison with both
 *  operands required non-null, nothing else considered "sharper". */
function betterIndex() {
  if (frames.length < 2) return null;
  const a = frames[0].item.iqa_score;
  const b = frames[1].item.iqa_score;
  if (a == null || b == null || a === b) return null;
  return a > b ? 0 : 1;
}

// ── Zoom — relative to the served /preview/:id (capped at 2048px on the long
// edge), never the sensor-resolution original. Both zoom levels compute an
// explicit pixel box in JS (from the loaded <img>'s naturalWidth/Height and
// the stage's own available area) rather than relying on CSS percentage
// sizing through an auto-sized wrapper, so the badge/Sharper pill — which are
// positioned against that same wrapper — always hug the actual visible photo
// edges, at both zoom levels, whatever the photo's aspect ratio. ────────────

function applyZoomStyle(i) {
  const img = el(`cmp-img-${i}`);
  const frame = el(`cmp-frame-${i}`);
  const stage = el(`cmp-stage-${i}`);
  if (!img || !frame || !stage || !img.naturalWidth) return;
  const availW = stage.clientWidth - 40; // stage padding is 20px each side
  const availH = stage.clientHeight - 40;
  let w;
  let h;
  if (zoom === 'fit' && availW > 0 && availH > 0) {
    const scale = Math.min(availW / img.naturalWidth, availH / img.naturalHeight);
    w = img.naturalWidth * scale;
    h = img.naturalHeight * scale;
  } else {
    w = img.naturalWidth;
    h = img.naturalHeight;
  }
  frame.style.width = `${w}px`;
  frame.style.height = `${h}px`;
  img.style.width = `${w}px`;
  img.style.height = `${h}px`;
}

function applyAllZoomStyles() {
  for (let i = 0; i < frames.length; i++) applyZoomStyle(i);
}

// ── Synced pan — both .cmp-stage elements mirror each other's scrollLeft/Top.
// `syncing` breaks the feedback loop: a real user scroll on one stage sets
// `syncing = true` and writes the other stage's scroll offset; the resulting
// (asynchronous) scroll event that write produces on the other stage sees
// `syncing` already true, consumes it (resets to false), and does not write
// back — one hop, then done. ────────────────────────────────────────────────

function wireStageSync() {
  const s0 = el('cmp-stage-0');
  const s1 = el('cmp-stage-1');
  if (!s0 || !s1) return;
  const mirror = (from, to) => {
    if (syncing) { syncing = false; return; }
    syncing = true;
    to.scrollLeft = from.scrollLeft;
    to.scrollTop = from.scrollTop;
  };
  s0.onscroll = () => mirror(s0, s1);
  s1.onscroll = () => mirror(s1, s0);
}

/** Click-and-drag panning. Ends up going through the same scroll listener
 *  (and therefore the same sync guard) as wheel/trackpad scrolling — dragging
 *  is just another way of changing scrollLeft/scrollTop. */
function wireDrag(stage) {
  if (!stage) return;
  let dragging = false;
  let startX = 0;
  let startY = 0;
  let startLeft = 0;
  let startTop = 0;
  stage.addEventListener('pointerdown', (e) => {
    if (e.target.closest('button')) return;
    dragging = true;
    startX = e.clientX;
    startY = e.clientY;
    startLeft = stage.scrollLeft;
    startTop = stage.scrollTop;
    stage.setPointerCapture(e.pointerId);
    stage.classList.add('dragging');
  });
  stage.addEventListener('pointermove', (e) => {
    if (!dragging) return;
    stage.scrollLeft = startLeft - (e.clientX - startX);
    stage.scrollTop = startTop - (e.clientY - startY);
  });
  const stop = () => { dragging = false; stage.classList.remove('dragging'); };
  stage.addEventListener('pointerup', stop);
  stage.addEventListener('pointercancel', stop);
}

// ── Rendering ────────────────────────────────────────────────────────────

function paneHtml(f, i, betterI) {
  const item = f.item;
  const better = betterI === i;
  const name = esc(basenameNoExt(item.path));
  const score = item.iqa_score != null ? item.iqa_score.toFixed(2) : '—';
  const chips = flagChipsHtml(item);
  const exifText = f.exifLoaded ? (f.exif ? esc(f.exif) : '') : 'Loading…';
  const keyLabel = i === 0 ? 'A' : 'D';
  return `
    <div class="cmp-pane" data-idx="${i}">
      <div class="cmp-stage" id="cmp-stage-${i}">
        <div class="cmp-frame" id="cmp-frame-${i}">
          <img class="cmp-img ${better ? 'better' : ''}" id="cmp-img-${i}"
            src="/preview/${item.file_id}" alt="${name}" draggable="false">
          <span class="cmp-badge">${name}</span>
          ${better ? '<span class="cmp-sharper">Sharper</span>' : ''}
        </div>
      </div>
      <div class="cmp-foot">
        <div style="flex:1;min-width:0">
          <div style="display:flex;align-items:baseline;gap:8px">
            <span class="cmp-score ${better ? 'better' : ''}">${score}</span>
            <span style="font:500 11.5px/1 var(--font);color:var(--text-dim)">quality</span>
          </div>
          <div style="display:flex;align-items:center;gap:5px;margin-top:8px;flex-wrap:wrap">
            ${chips}
            <span class="cmp-exif" id="cmp-exif-${i}">${exifText}</span>
          </div>
        </div>
        <button class="btn ${better ? 'btn-primary' : ''} sm" data-keeper="${item.file_id}">
          ★ Keeper<span class="kbd">${keyLabel}</span></button>
      </div>
    </div>`;
}

function render() {
  if (!root) return;
  const betterI = betterIndex();
  root.innerHTML = `
    <div class="cmp-bar">
      <span style="font:700 13px/1 var(--font);color:var(--text)">Compare · Cluster ${String(groupId).padStart(2, '0')}</span>
      <span style="font:500 12px/1 var(--font);color:var(--text-dim)">${frames.length} of ${totalMembers} frames · zoom synced</span>
      <div class="topbar-gap"></div>
      <div class="seg">
        <button class="seg-btn ${zoom === 'fit' ? 'on' : ''}" data-zoom="fit">Fit</button>
        <button class="seg-btn ${zoom === 1 ? 'on' : ''}" data-zoom="1"
          title="100% of the 2048px preview, not the original">100%</button>
      </div>
      <button class="btn sm" id="cmp-close">Close<span class="kbd">Esc</span></button>
    </div>
    <div class="cmp-panes">
      ${frames.map((f, i) => paneHtml(f, i, betterI)).join('')}
    </div>`;
  wire();
  applyAllZoomStyles();
  wireStageSync();
}

function wire() {
  el('cmp-close').onclick = closeCompare;
  for (const b of root.querySelectorAll('[data-zoom]')) {
    b.onclick = () => { zoom = b.dataset.zoom === 'fit' ? 'fit' : Number(b.dataset.zoom); render(); };
  }
  for (const btn of root.querySelectorAll('[data-keeper]')) {
    btn.onclick = () => setKeeper(Number(btn.dataset.keeper));
  }
  for (let i = 0; i < frames.length; i++) {
    const img = el(`cmp-img-${i}`);
    if (!img) continue;
    img.addEventListener('load', () => applyZoomStyle(i));
    if (img.complete && img.naturalWidth) applyZoomStyle(i);
    wireDrag(el(`cmp-stage-${i}`));
  }
}

function onResize() { applyAllZoomStyles(); }

// ── Decisions ────────────────────────────────────────────────────────────

/** Route the write through window.pp.reviewApply — never POST /api/decisions
 *  directly. Re-checks state.activeFolder both before and after the write:
 *  file_id is only meaningful against the catalog it was fetched from, and a
 *  library switch mid-write must not lead to a stale refresh of "whichever
 *  screen is behind" using the old folder's identity. */
async function setKeeper(fileId) {
  if (deciding) return;
  if (state.activeFolder !== openedFolder) { closeCompare(); return; }
  deciding = true;
  try {
    await window.pp.reviewApply(fileId, 'keeper');
  } finally {
    deciding = false;
  }
  const stillSameFolder = state.activeFolder === openedFolder;
  const wasOnDuplicates = state.view === 'duplicates';
  closeCompare();
  if (!stillSameFolder) return; // switched libraries mid-write; nothing safe to refresh
  if (wasOnDuplicates) { window.pp.openDuplicates(state.activeFolder); return; }
  // Await the reload, then repaint the detail overlay if one is still mounted
  // underneath (compare can be opened from detail with `c`). Without this the
  // panel behind keeps reading "Undecided" for a frame that is now keeper or
  // reject, and its `idx` indexes the array reviewReload() just replaced.
  // detailRefresh is a no-op when detail is closed; `?.` covers the load order
  // where detail.js has not registered it yet.
  await window.pp.reviewReload();
  window.pp.detailRefresh?.();
}

/** Writes the two panes' EXIF text nodes directly, without going through
 *  render(). The EXIF fetch resolves ~1s after open — a full re-render at
 *  that point would blow away any pan/scroll the user has already set (both
 *  .cmp-stage elements get a fresh innerHTML subtree, so their scrollLeft/Top
 *  reset to 0) and would re-run wireStageSync(), needlessly re-attaching
 *  listeners onto (new) DOM nodes. EXIF never affects score, Sharper marking,
 *  or zoom sizing, so nothing else on the pane needs to change when it
 *  arrives — patching the two text nodes is sufficient and keeps the scroll
 *  listeners attached to the exact elements the user has been scrolling. */
function patchExifText() {
  for (let i = 0; i < frames.length; i++) {
    const span = el(`cmp-exif-${i}`);
    const f = frames[i];
    if (!span || !f) continue;
    // textContent, not innerHTML: no esc() needed, and it can't reintroduce
    // markup even if a future EXIF field ever contained "<"/"&".
    span.textContent = f.exifLoaded ? (f.exif || '') : 'Loading…';
  }
}

async function loadExif() {
  const forFolder = openedFolder;
  const results = await Promise.all(
    frames.map((f) => api('GET', `/api/photos/${f.item.file_id}`).catch(() => null)),
  );
  if (!root || openedFolder !== forFolder || state.activeFolder !== forFolder) return;
  results.forEach((dump, i) => {
    if (frames[i]) {
      frames[i].exif = exifLine(dump);
      frames[i].exifLoaded = true;
    }
  });
  patchExifText();
}

// ── Keyboard ─────────────────────────────────────────────────────────────

function onKey(e) {
  // Stand down unless this overlay is the topmost layer in #modal-host, same
  // guard detail.js carries. `.cmp` is position:fixed;inset:0;z-index:130 and
  // today nothing can stack above it — but "nothing stacks above it" was the
  // argument for detail.js not needing the guard either, right up until compare
  // proved it wrong. `a`/`d` write a keeper; they do not get to run on
  // faith about the layer stack.
  const host = el('modal-host');
  if (host && root && host.lastElementChild !== root) return;
  if (e.target && e.target.closest
      && e.target.closest('input, textarea, select, [contenteditable="true"]')) return;
  // stopPropagation so one Escape closes one layer: detail.js sits underneath
  // when compare was opened with `c`, and would otherwise close too.
  if (e.key === 'Escape') { e.stopPropagation(); closeCompare(); return; }
  // Modifier chords are browser/OS shortcuts, never this screen's own.
  if (e.ctrlKey || e.metaKey || e.altKey) return;
  const k = e.key;
  if (k === 'a' || k === 'A') { if (frames[0]) setKeeper(frames[0].item.file_id); return; }
  if (k === 'd' || k === 'D') { if (frames[1]) setKeeper(frames[1].item.file_id); return; }
  if (k === '1') { zoom = 'fit'; render(); return; }
  if (k === '2') { zoom = 1; render(); }
}

// ── Mount / unmount ──────────────────────────────────────────────────────

function mount() {
  root = document.createElement('div');
  root.className = 'cmp';
  document.getElementById('modal-host').appendChild(root);
  document.addEventListener('keydown', onKey, true);
  window.addEventListener('resize', onResize);
}

function closeCompare() {
  if (!root) return;
  document.removeEventListener('keydown', onKey, true);
  window.removeEventListener('resize', onResize);
  root.remove();
  root = null;
  frames = [];
  groupId = null;
  totalMembers = 0;
  openedFolder = null;
}

// ── Entry point ──────────────────────────────────────────────────────────

/**
 * Open compare mode for `groupId`, showing `fileIds` (two members of that
 * group) or, if omitted, the group's first two members. Fewer than two
 * resolvable members (a genuinely single-member group, or a caller-supplied
 * `fileIds` naming fewer than two) produces an info toast and no overlay —
 * never a half-rendered screen.
 */
export async function openCompare(targetGroupId, fileIds) {
  const targetFolder = state.activeFolder;
  let clusters;
  try {
    clusters = await api('GET', '/api/clusters');
  } catch (e) {
    if (state.activeFolder !== targetFolder) return; // switched libraries mid-fetch
    window.pp.toast({ kind: 'error', title: 'Could not open compare', body: e.message });
    return;
  }
  if (state.activeFolder !== targetFolder) return; // switched libraries mid-fetch

  const c = clusters.find((x) => x.group_id === targetGroupId);
  if (!c) {
    window.pp.toast({
      kind: 'error',
      title: 'Could not open compare',
      body: `Cluster ${targetGroupId} was not found in this library.`,
    });
    return;
  }

  let chosen;
  if (Array.isArray(fileIds) && fileIds.length) {
    chosen = fileIds.map((id) => c.members.find((m) => m.file_id === id)).filter(Boolean);
  } else {
    chosen = c.members.slice(0, 2);
  }
  if (chosen.length < 2) {
    window.pp.toast({
      kind: 'info',
      title: 'Nothing to compare',
      body: 'This group has one frame.',
    });
    return;
  }
  chosen = chosen.slice(0, 2);

  if (root) closeCompare(); // defensive: guard against a double-invoke re-entering with stale state

  openedFolder = targetFolder;
  groupId = c.group_id;
  totalMembers = c.members.length;
  zoom = 'fit';
  syncing = false;
  frames = chosen.map((m) => ({ item: m, exif: null, exifLoaded: false }));

  mount();
  render();
  loadExif();
}

Object.assign(window.pp, { openCompare });
