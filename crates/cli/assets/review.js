// Screens 1e / 1f / 1g — the review grid. The app's primary working surface:
// stats header, filter bar, tile grid, and the floating shortcut sheet.
//
// Everything the header shows is derived from the two payloads this screen
// already loads (`/api/photos` and `/api/counts`) — there is no per-flag or
// per-group count endpoint, and inventing one would mean guessing. The photo
// list is therefore fetched with a deliberately high limit so the flag counts
// describe the whole library rather than the first page.
import { api, show, state } from '/app.js';
import { icon } from '/icons.js';

const FLAG_META = [
  { key: 'blur', code: 'BLR', label: 'blur', long: 'Blur' },
  { key: 'back_focus', code: 'BF', label: 'back focus', long: 'Back focus' },
  { key: 'overexposed', code: 'OE', label: 'over', long: 'Overexposed' },
  { key: 'underexposed', code: 'UE', label: 'under', long: 'Underexposed' },
  { key: 'low_iqa', code: 'IQA', label: 'low IQA', long: 'Low quality score' },
];
const CODE = Object.fromEntries(FLAG_META.map(f => [f.key, f.code]));

const SORTS = [
  { key: 'score', label: 'quality score' },
  { key: 'filename', label: 'filename' },
  { key: 'flagged', label: 'flagged first' },
];
const DENSITY = [
  { key: 'roomy', cols: 5, ico: 'rows' },
  { key: 'normal', cols: 8, ico: 'cells' },
  { key: 'dense', cols: 12, ico: 'dense' },
];

// The 1f menu offers "Any defect flag" and "No flags at all" alongside the five
// flags, but neither is a flag_type — they are modes over the whole set. They
// live in ui.flags under sentinels no flag_type can collide with, and are
// mutually exclusive with each other and with the real keys.
const ANY = '*';
const NONE = '!';

const MARK = { keep: '✓', reject: '✕', keeper: '★' };

const SHORTCUTS = [
  { label: 'Move between photos', keys: ['←', '→', 'J', 'K'] },
  { label: 'Move a row up or down', keys: ['↑', '↓'] },
  { label: 'Keep', keys: ['Space'] },
  { label: 'Reject', keys: ['X'] },
  // `u` clears the decision on the photo under the cursor. The mockup calls
  // this "undo the last decision"; there is no decision history to undo, so the
  // sheet says what the key actually does.
  { label: 'Clear the decision', keys: ['U'] },
  { label: 'Mark as keeper of its group', keys: ['⇧K'] },
  { label: 'Fullscreen', keys: ['F'] },
  { label: 'Compare the duplicate group', keys: ['C'] },
  { label: 'Toggle this sheet', keys: ['?'] },
  { label: 'Exit / close', keys: ['Esc'] },
];

// Every photo in the library, unfiltered — the basis for honest flag counts.
let all = [];
// The filtered + sorted view the grid renders and the cursor indexes into.
// Its entries are the *same objects* as in `all`, so patching a row in `all`
// patches its twin here as well.
let photos = [];
let cursor = 0;
let counts = { kept: 0, rejected: 0, undecided: 0 };
let loading = true;
let loadError = null;
// file_id -> "2/4", the frame's position within its duplicate group.
let dupPos = new Map();
let keysWired = false;
let scoreBannerShown = false;
// Whether the last `renderGrid()` painted the 1g completion card. A decision
// that flips this has to repaint the grid, not just the tile it touched.
let lastComplete = false;
// One decision in flight at a time. Held `Space` repeats far faster than the
// round trip, and `onKey` reads `photos[cursor]` synchronously, so without this
// several repeats would all decide the *same* photo while the cursor advanced
// once per resolution — leaving the photos in between silently undecided.
let deciding = false;
// The folder the loaded rows belong to. Re-opening the same library (coming
// back from the detail or duplicates screens) keeps the user's filters;
// switching to a different one drops them, since "keepers only" or a flag
// filter carried across libraries just looks like an empty grid.
let lastFolder = null;

const ui = {
  flags: new Set(),      // selected flag keys; empty means no flag filter
  undecidedOnly: false,
  dupOnly: false,
  keepersOnly: false,    // set by the 1g completion card's "Review keepers"
  sort: 'score',
  density: 'normal',
  sheet: false,          // shortcut sheet open
  menu: false,           // defect filter menu open
  sortMenu: false,       // sort menu open
};

const el = (id) => document.getElementById(id);
const cols = () => (DENSITY.find(d => d.key === ui.density) || DENSITY[1]).cols;
const esc = (s) => String(s == null ? '' : s)
  .replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;');
const plural = (n, w) => `${n.toLocaleString()} ${w}${n === 1 ? '' : 's'}`;

/** Last path segment, for either separator. */
function basename(path) {
  const s = String(path || '');
  const i = Math.max(s.lastIndexOf('/'), s.lastIndexOf('\\'));
  return i === -1 ? s : s.slice(i + 1);
}

/** One walk over `all`: per-flag totals plus the any / none / duplicate totals. */
function flagCounts() {
  const perKey = {};
  for (const f of FLAG_META) perKey[f.key] = 0;
  let any = 0, none = 0, dup = 0;
  for (const p of all) {
    const fl = p.flags || [];
    if (fl.length) any++; else none++;
    for (const k of fl) perKey[k] = (perKey[k] || 0) + 1;
    if (p.group_id != null) dup++;
  }
  return { perKey, any, none, dup };
}

/** FLAG_META plus a synthesised entry for any flag_type the UI does not know,
 *  so a new server-side flag shows up as a chip instead of vanishing. */
function flagList(fc) {
  const extra = Object.keys(fc.perKey)
    .filter(k => !CODE[k])
    .sort()
    .map(k => ({ key: k, code: k.toUpperCase().slice(0, 3), label: k.replace(/_/g, ' '), long: k }));
  return FLAG_META.concat(extra);
}

const scoreDesc = (a, b) => {
  if (a.iqa_score == null && b.iqa_score == null) return 0;
  if (a.iqa_score == null) return 1;
  if (b.iqa_score == null) return -1;
  return b.iqa_score - a.iqa_score;
};
const scoreAsc = (a, b) => {
  if (a.iqa_score == null && b.iqa_score == null) return 0;
  if (a.iqa_score == null) return 1;
  if (b.iqa_score == null) return -1;
  return a.iqa_score - b.iqa_score;
};

/** Rebuild `photos` from `all` per `ui`, then sort and clamp the cursor. */
function applyFilters() {
  const sel = ui.flags;
  photos = all.filter((p) => {
    if (ui.undecidedOnly && p.verdict != null) return false;
    if (ui.dupOnly && p.group_id == null) return false;
    if (ui.keepersOnly && !p.is_keeper) return false;
    if (sel.size) {
      const n = (p.flags || []).length;
      if (sel.has(ANY)) return n > 0;
      if (sel.has(NONE)) return n === 0;
      return (p.flags || []).some(f => sel.has(f));
    }
    return true;
  });

  if (ui.sort === 'score') photos.sort(scoreDesc);
  else if (ui.sort === 'filename') {
    photos.sort((a, b) => basename(a.path).localeCompare(basename(b.path), undefined, { numeric: true }));
  } else {
    photos.sort((a, b) => ((b.flags || []).length - (a.flags || []).length) || scoreAsc(a, b));
  }

  cursor = photos.length ? Math.min(Math.max(0, cursor), photos.length - 1) : 0;
}

/** Position of each frame inside its duplicate group, by ascending file_id so
 *  the label is stable across reloads and sort changes. */
function indexGroups() {
  const groups = new Map();
  for (const p of all) {
    if (p.group_id == null) continue;
    if (!groups.has(p.group_id)) groups.set(p.group_id, []);
    groups.get(p.group_id).push(p);
  }
  dupPos = new Map();
  for (const members of groups.values()) {
    members.sort((a, b) => a.file_id - b.file_id);
    members.forEach((p, i) => dupPos.set(p.file_id, `${i + 1}/${members.length}`));
  }
}

/** Repaint everything that depends on the filtered list. */
function refresh() {
  applyFilters();
  renderStats();
  renderFilterBar();
  renderGrid();
}

// ── Stats header (1e:281-311) ───────────────────────────────────────────────

function renderStats() {
  const host = el('rv-stats');
  if (!host) return;
  const total = counts.kept + counts.rejected + counts.undecided;
  const decided = counts.kept + counts.rejected;
  const keepers = all.filter(p => p.is_keeper).length;
  const pctKeep = total ? (counts.kept / total) * 100 : 0;
  const pctReject = total ? (counts.rejected / total) * 100 : 0;
  const culled = total ? Math.round((decided / total) * 100) : 0;
  const fc = flagCounts();

  host.innerHTML = `
    <div class="stat-decided-wrap">
      <div class="stat-decided">
        <span class="stat-decided-n">${decided.toLocaleString()}</span>
        <span class="stat-decided-of">of ${total.toLocaleString()} decided</span>
      </div>
      <div class="legend">
        <span class="legend-item"><span class="legend-sw keep"></span>${counts.kept.toLocaleString()} keep</span>
        <span class="legend-item"><span class="legend-sw reject"></span>${counts.rejected.toLocaleString()} reject</span>
        <span class="legend-item"><span class="legend-sw keeper"></span>${keepers.toLocaleString()} keeper</span>
      </div>
    </div>
    <div class="decide-wrap">
      <div class="decide-bar">
        <div class="decide-bar-keep" style="width:${pctKeep.toFixed(1)}%"></div>
        <div class="decide-bar-reject" style="width:${pctReject.toFixed(1)}%"></div>
        <div class="decide-bar-rest"></div>
      </div>
      <div class="decide-legend">
        <span>${culled}% culled</span><span>${counts.undecided.toLocaleString()} undecided</span>
      </div>
    </div>
    <div class="flag-chips">
      ${flagList(fc).map(f => `
        <button class="chip ${ui.flags.has(f.key) ? 'on' : ''}" data-flag="${esc(f.key)}"
                title="Filter by ${esc(f.long)}">
          <span class="chip-code">${esc(f.code)}</span>${esc(f.label)}
          <span class="chip-n">${(fc.perKey[f.key] || 0).toLocaleString()}</span>
        </button>`).join('')}
    </div>`;

  for (const b of host.querySelectorAll('[data-flag]')) {
    b.onclick = () => toggleFlag(b.dataset.flag);
  }

  const count = el('rv-photos');
  if (count) count.textContent = plural(all.length, 'photo');
  const exp = el('rv-export');
  // "photos", not "keepers": export copies every `verdict='keep'` row, which is
  // a different population from the `is_keeper` count in the legend above.
  if (exp) exp.innerHTML = `${icon('download', 14, 2)}Export ${plural(counts.kept, 'photo')}`;
}

function toggleFlag(key) {
  const on = ui.flags.has(key);
  if (key === ANY || key === NONE) {
    ui.flags.clear();
    if (!on) ui.flags.add(key);
  } else {
    ui.flags.delete(ANY);
    ui.flags.delete(NONE);
    if (on) ui.flags.delete(key); else ui.flags.add(key);
  }
  refresh();
}

// ── Filter bar (1e:313-329, 1f:396-404) ─────────────────────────────────────

function flagButtonLabel() {
  if (!ui.flags.size) return 'Flags: any';
  if (ui.flags.has(ANY)) return 'Flags: any defect';
  if (ui.flags.has(NONE)) return 'Flags: none';
  if (ui.flags.size === 1) {
    const k = [...ui.flags][0];
    const m = FLAG_META.find(f => f.key === k);
    return `Flags: ${m ? m.long : k}`;
  }
  return `Flags: ${ui.flags.size} selected`;
}

function renderFilterBar() {
  const host = el('rv-filters');
  if (!host) return;
  const fc = flagCounts();
  const sort = SORTS.find(s => s.key === ui.sort) || SORTS[0];
  const filtered = ui.flags.size > 0 || ui.undecidedOnly || ui.dupOnly || ui.keepersOnly;

  host.innerHTML = `
    <button class="btn sm ${ui.undecidedOnly ? 'on' : ''}" id="rv-undecided">
      ${icon('filter', 13, 1.9)}Undecided only<span class="filter-n">${counts.undecided.toLocaleString()}</span>
    </button>
    <div class="filter-anchor" id="rv-flag-anchor">
      <button class="btn sm ${ui.flags.size ? 'on' : ''}" id="rv-flags">
        ${esc(flagButtonLabel())}${icon('chevron-down', 13, 1.9)}
      </button>
    </div>
    <button class="btn sm ${ui.dupOnly ? 'on' : ''}" id="rv-dup">
      ${icon('layers', 13, 1.9)}In a duplicate group<span class="filter-n">${fc.dup.toLocaleString()}</span>
    </button>
    <div class="filter-anchor" id="rv-sort-anchor">
      <button class="btn sm" id="rv-sort">Sort: ${esc(sort.label)}${icon('chevron-down', 13, 1.9)}</button>
    </div>
    ${ui.keepersOnly
      ? `<button class="btn sm on" id="rv-keepers-off">Keepers only${icon('close', 12, 2.2)}</button>`
      : ''}
    <button class="btn btn-ghost sm" id="rv-clear" ${filtered ? '' : 'disabled'}>Clear</button>
    <div class="topbar-gap"></div>
    <span class="filter-count">Showing ${photos.length.toLocaleString()} of ${all.length.toLocaleString()}</span>
    <span class="section-label">Density</span>
    <div class="seg">
      ${DENSITY.map(d => `<button class="seg-btn ${ui.density === d.key ? 'on' : ''}"
        data-density="${d.key}" title="${d.cols} across" aria-label="${d.cols} columns">
        ${icon(d.ico, 14, 1.7)}</button>`).join('')}
    </div>
    <button class="btn sm" id="rv-sheet">Shortcuts<span class="kbd">?</span></button>`;

  el('rv-undecided').onclick = () => { ui.undecidedOnly = !ui.undecidedOnly; refresh(); };
  el('rv-dup').onclick = () => { ui.dupOnly = !ui.dupOnly; refresh(); };
  el('rv-flags').onclick = () => {
    ui.menu = !ui.menu;
    ui.sortMenu = false;
    renderFilterBar();
  };
  el('rv-sort').onclick = () => {
    ui.sortMenu = !ui.sortMenu;
    ui.menu = false;
    renderFilterBar();
  };
  const off = el('rv-keepers-off');
  if (off) off.onclick = () => { ui.keepersOnly = false; refresh(); };
  el('rv-clear').onclick = clearFilters;
  el('rv-sheet').onclick = () => { ui.sheet = !ui.sheet; renderShortcutSheet(); };
  for (const b of host.querySelectorAll('[data-density]')) {
    b.onclick = () => { ui.density = b.dataset.density; renderFilterBar(); renderGrid(); };
  }

  renderFlagMenu(fc);
  renderSortMenu();
}

function clearFilters() {
  ui.flags.clear();
  ui.undecidedOnly = false;
  ui.dupOnly = false;
  ui.keepersOnly = false;
  ui.menu = false;
  ui.sortMenu = false;
  refresh();
}

/** The 1f popover, anchored under the "Flags:" button. */
function renderFlagMenu(fc) {
  const anchor = el('rv-flag-anchor');
  if (!anchor || !ui.menu) return;
  const rows = [
    { key: ANY, label: 'Any defect flag', n: fc.any },
    ...flagList(fc).map(f => ({ key: f.key, label: f.long, n: fc.perKey[f.key] || 0 })),
    { key: NONE, label: 'No flags at all', n: fc.none },
  ];
  const pop = document.createElement('div');
  pop.className = 'filter-pop';
  pop.innerHTML = `
    <div class="menu">
      <div class="menu-head">Filter by defect</div>
      <div class="menu-list">
        ${rows.map(r => `
          <div class="menu-row ${ui.flags.has(r.key) ? 'on' : ''}" data-key="${esc(r.key)}" role="button">
            <span class="menu-box">${ui.flags.has(r.key) ? '✓' : ''}</span>
            <span class="menu-label">${esc(r.label)}</span>
            <span class="menu-n">${r.n.toLocaleString()}</span>
          </div>`).join('')}
      </div>
      <div class="menu-foot">
        <button class="btn sm" data-reset>Reset</button>
        <button class="btn btn-primary sm" data-apply>Apply</button>
      </div>
    </div>`;
  anchor.appendChild(pop);
  for (const r of pop.querySelectorAll('[data-key]')) r.onclick = () => toggleFlag(r.dataset.key);
  pop.querySelector('[data-reset]').onclick = () => { ui.flags.clear(); refresh(); };
  pop.querySelector('[data-apply]').onclick = () => { ui.menu = false; refresh(); };
}

function renderSortMenu() {
  const anchor = el('rv-sort-anchor');
  if (!anchor || !ui.sortMenu) return;
  const pop = document.createElement('div');
  pop.className = 'filter-pop';
  pop.innerHTML = `
    <div class="menu">
      <div class="menu-head">Sort by</div>
      <div class="menu-list">
        ${SORTS.map(s => `
          <div class="menu-row ${ui.sort === s.key ? 'on' : ''}" data-sort="${s.key}" role="button">
            <span class="menu-box">${ui.sort === s.key ? '✓' : ''}</span>
            <span class="menu-label">${esc(s.label)}</span>
          </div>`).join('')}
      </div>
    </div>`;
  anchor.appendChild(pop);
  for (const r of pop.querySelectorAll('[data-sort]')) {
    r.onclick = () => { ui.sort = r.dataset.sort; ui.sortMenu = false; refresh(); };
  }
}

// ── Tiles and grid (tileStyles() at 868-889, 1e:331-352, 1g:461-508) ────────

function decisionOf(p) {
  if (p.is_keeper) return 'keeper';          // keeper wins over plain keep
  if (p.verdict === 'keep') return 'keep';
  if (p.verdict === 'reject') return 'reject';
  return 'undecided';
}

function tile(p, i) {
  const dec = decisionOf(p);
  const cls = ['tile', dec, i === cursor ? 'cursor' : ''].filter(Boolean).join(' ');
  const flagText = (p.flags || []).map(f => CODE[f] ?? f.toUpperCase()).join(' ');
  // No score means the IQA model never ran for this file. Show a dash and an
  // empty meter — never 0.00, which would read as "scored, and terrible".
  // The meter track stays (so the tiles keep a uniform footer height) but the
  // fill element is omitted entirely rather than emitted at width:0%, which
  // renders identically to a genuine 0.00. detail.js does the same for the
  // same question: absent, not zero-width.
  const hasScore = p.iqa_score != null;
  const pct = hasScore ? Math.max(0, Math.min(100, Math.round(p.iqa_score * 100))) : 0;
  const dl = dupPos.get(p.file_id) || '';
  return `<div class="${cls}" data-i="${i}" data-fid="${p.file_id}" title="${esc(basename(p.path))}">
    <img class="tile-img" loading="lazy" src="/thumb/${p.file_id}" alt="">
    ${p.group_id != null ? `<span class="tile-dup">${icon('spark', 10, 2.2)}${esc(dl)}</span>` : ''}
    ${MARK[dec] ? `<span class="tile-mark ${dec}">${MARK[dec]}</span>` : ''}
    <div class="tile-foot">
      ${flagText ? `<span class="tile-flag">${esc(flagText)}</span>` : ''}
      <span class="tile-foot-gap"></span>
      <span class="tile-score">${hasScore ? p.iqa_score.toFixed(2) : '—'}</span>
    </div>
    <div class="tile-meter">${hasScore ? `<div class="tile-meter-fill" style="width:${pct}%"></div>` : ''}</div>
  </div>`;
}

function activeFilterLabels() {
  const out = [];
  if (ui.undecidedOnly) out.push('undecided only');
  if (ui.flags.has(ANY)) out.push('any defect flag');
  else if (ui.flags.has(NONE)) out.push('no flags at all');
  else {
    for (const k of ui.flags) {
      const m = FLAG_META.find(f => f.key === k);
      out.push((m ? m.long : k).toLowerCase());
    }
  }
  if (ui.dupOnly) out.push('in a duplicate group');
  if (ui.keepersOnly) out.push('keepers only');
  return out;
}

/** Drop the last filter shown in the filtered-empty state's own list, so the
 *  button matches the sentence directly above it. */
function dropLastFilter() {
  if (ui.keepersOnly) { ui.keepersOnly = false; }
  else if (ui.dupOnly) { ui.dupOnly = false; }
  else if (ui.flags.size) {
    const last = [...ui.flags].pop();
    ui.flags.delete(last);
  } else if (ui.undecidedOnly) { ui.undecidedOnly = false; }
  refresh();
}

function renderGrid() {
  const host = el('rv-grid');
  if (!host) return;
  // Every early return below paints a state without the completion card; the
  // one path that can paint it sets this again just before it does.
  lastComplete = false;

  if (loading) {
    host.innerHTML = `
      <div class="grid" style="--cols:${cols()}">
        ${Array.from({ length: 12 }, () => '<div class="skeleton"></div>').join('')}
      </div>
      <div class="grid-loading-note">Loading thumbnails…</div>`;
    return;
  }

  if (loadError) {
    host.innerHTML = `
      <div class="empty grid-empty">
        <span class="empty-icon">${icon('warn', 22, 1.7)}</span>
        <div>
          <div class="empty-title">Could not load this library</div>
          <div class="empty-body">${esc(loadError)}</div>
        </div>
        <button class="btn btn-primary" id="rv-retry">${icon('refresh', 14, 2)}Try again</button>
      </div>`;
    el('rv-retry').onclick = () => load();
    return;
  }

  if (!all.length) {
    host.innerHTML = `
      <div class="empty grid-empty">
        <span class="empty-icon">${icon('folder', 22, 1.6)}</span>
        <div>
          <div class="empty-title">No photos here yet</div>
          <div class="empty-body">Point photopipe at a folder of RAW files and it will flag the
            obvious failures before you look at a single frame.</div>
        </div>
        <button class="btn btn-primary" id="rv-analyze">${icon('spark', 14, 2)}Analyze folder</button>
      </div>`;
    el('rv-analyze').onclick = () => {
      if (state.activeFolder) window.pp.go('/analyze', { folder: state.activeFolder });
      else window.pp.openPicker(null);
    };
    return;
  }

  if (!photos.length) {
    const labels = activeFilterLabels();
    host.innerHTML = `
      <div class="empty grid-empty">
        <span class="empty-icon">${icon('filter', 22, 1.6)}</span>
        <div>
          <div class="empty-title">Nothing matches these filters</div>
          <div class="empty-body">${labels.length ? `${esc(labels.join(' · '))}. ` : ''}${plural(all.length, 'photo')}
            ${all.length === 1 ? 'is' : 'are'} still in this library.</div>
        </div>
        <div class="complete-acts">
          ${labels.length ? '<button class="btn" id="rv-drop">Drop last filter</button>' : ''}
          <button class="btn btn-primary" id="rv-clear-all">Clear all</button>
        </div>
      </div>`;
    const drop = el('rv-drop');
    if (drop) drop.onclick = dropLastFilter;
    el('rv-clear-all').onclick = clearFilters;
    return;
  }

  const total = counts.kept + counts.rejected + counts.undecided;
  const keepers = all.filter(p => p.is_keeper).length;
  const complete = counts.undecided === 0 && all.length > 0;
  lastComplete = complete;
  host.innerHTML = `
    ${complete ? `
      <div class="complete-card">
        <span class="empty-icon complete-ico">${icon('check', 22, 2)}</span>
        <div class="complete-text">
          <div class="empty-title">All ${total.toLocaleString()} decided</div>
          <div class="empty-body">${keepers.toLocaleString()} marked keeper ·
            ${counts.kept.toLocaleString()} keep · ${counts.rejected.toLocaleString()} reject</div>
        </div>
        <div class="complete-acts">
          <button class="btn" id="rv-keepers">Review keepers</button>
          <button class="btn btn-primary" id="rv-export-2">${icon('download', 14, 2)}Export
            ${plural(counts.kept, 'photo')}</button>
        </div>
        <div class="complete-note">Develop to JPEG — coming later</div>
      </div>` : ''}
    <div class="grid" id="rv-tiles" style="--cols:${cols()}">
      ${photos.map((p, i) => tile(p, i)).join('')}
    </div>`;

  if (complete) {
    el('rv-keepers').onclick = () => { ui.keepersOnly = true; refresh(); };
    el('rv-export-2').onclick = () => window.pp.openExport();
  }
  el('rv-tiles').onclick = (e) => {
    const t = e.target.closest('.tile');
    if (!t) return;
    const i = Number(t.dataset.i);
    if (!Number.isNaN(i) && photos[i]) {
      cursor = i;
      window.pp.go(`/review/photo/${photos[i].file_id}`);
    }
  };
  scrollCursorIntoView();
}

function scrollCursorIntoView() {
  const t = el('rv-grid') && el('rv-grid').querySelector('.tile.cursor');
  if (t) t.scrollIntoView({ block: 'nearest' });
}

function repaintIndex(i) {
  const host = el('rv-grid');
  if (!host || !photos[i]) return;
  const node = host.querySelector(`.tile[data-i="${i}"]`);
  if (!node) return;
  const tmp = document.createElement('div');
  tmp.innerHTML = tile(photos[i], i);
  node.replaceWith(tmp.firstElementChild);
}

function repaintTile(fileId) {
  const i = photos.findIndex(p => p.file_id === fileId);
  if (i >= 0) repaintIndex(i);
}

function moveCursor(next) {
  if (!photos.length) return;
  const n = Math.min(Math.max(0, next), photos.length - 1);
  if (n === cursor) return;
  const prev = cursor;
  cursor = n;
  repaintIndex(prev);
  repaintIndex(cursor);
  scrollCursorIntoView();
}

// ── Shortcut sheet (1e:355-376) ─────────────────────────────────────────────

function renderShortcutSheet() {
  const root = el('rv-root');
  if (!root) return;
  const old = root.querySelector('.sheet');
  if (old) old.remove();
  if (!ui.sheet) return;
  const sheet = document.createElement('div');
  sheet.className = 'sheet';
  sheet.innerHTML = `
    <div class="sheet-head">
      <span class="sheet-title">Keyboard</span>
      <span class="sheet-hint">? toggles</span>
      <button class="notice-x" id="rv-sheet-x" aria-label="Close">${icon('close', 12, 2.2)}</button>
    </div>
    <div class="sheet-body">
      ${SHORTCUTS.map((s, i) => `
        <div class="sheet-row">
          <span class="sheet-label ${i < 6 ? '' : 'dim'}">${esc(s.label)}</span>
          <span class="sheet-keys">${s.keys.map(k => `<span class="kbd">${esc(k)}</span>`).join('')}</span>
        </div>`).join('')}
      <div class="sheet-note">Deciding moves to the next photo — hold Shift to stay put.</div>
    </div>`;
  root.appendChild(sheet);
  el('rv-sheet-x').onclick = () => { ui.sheet = false; renderShortcutSheet(); };
}

// ── Loading and decisions ───────────────────────────────────────────────────

async function load() {
  loading = true;
  loadError = null;
  renderGrid();
  try {
    const [rows, c] = await Promise.all([
      api('GET', '/api/photos?limit=100000'),
      api('GET', '/api/counts'),
    ]);
    all = rows;
    counts = c;
  } catch (e) {
    all = [];
    // `counts` has to go with `all`. renderStats() runs unconditionally below,
    // and on a failed load it would otherwise print the *previous* successful
    // load's "N of M decided", legend and "% culled" under the new library's
    // name — numbers that describe a library the user is not looking at.
    counts = { kept: 0, rejected: 0, undecided: 0 };
    loadError = e.message;
    window.pp.toast({ kind: 'error', title: 'Could not load this library', body: e.message });
  }
  loading = false;
  indexGroups();
  applyFilters();
  renderStats();
  renderFilterBar();
  renderGrid();
  renderShortcutSheet();
  renderScoreBanner();
}

/** Warn once when the library has no quality scores at all — the models were
 *  not installed when it was analysed, so every tile reads `—`. */
function renderScoreBanner() {
  if (scoreBannerShown || !all.length) return;
  if (all.some(p => p.iqa_score != null)) return;
  const host = el('rv-banners');
  if (!host) return;
  scoreBannerShown = true;
  window.pp.banner(host, {
    kind: 'warn',
    title: 'This library has no quality scores',
    body: 'The IQA model was not available when it was analysed, so every tile shows a dash '
      + 'instead of a score and low-quality flagging was skipped.',
  });
}

/**
 * Post a decision and mirror the server's write locally.
 *
 * `keeper` maps to the catalog's `pick_keeper`, which keeps the chosen file AND
 * rejects every other member of its duplicate group in the same transaction
 * (catalog/mod.rs:1632). The local patch has to do the same or the grid keeps
 * showing the siblings as undecided until the next reload.
 *
 * `photos` holds the same row objects as `all`, so patching a row here patches
 * its twin in the filtered view too. Counts come from the POST response rather
 * than being recomputed locally.
 */
async function reviewApply(fileId, action) {
  const prev = counts;
  let c;
  try {
    c = await api('POST', '/api/decisions', { file_id: fileId, action });
  } catch (e) {
    window.pp.toast({ kind: 'error', title: 'Could not save that decision', body: e.message });
    return;
  }
  if (c && typeof c.kept === 'number') counts = c;

  const touched = [];
  const set = (p, verdict, keeper) => {
    p.verdict = verdict;
    p.is_keeper = keeper;
    touched.push(p.file_id);
  };
  const target = all.find(p => p.file_id === fileId);
  if (target) {
    if (action === 'keep') set(target, 'keep', false);
    else if (action === 'reject') set(target, 'reject', false);
    else if (action === 'undecide') set(target, null, false);
    else if (action === 'keeper') {
      set(target, 'keep', true);
      if (target.group_id != null) {
        for (const p of all) {
          if (p !== target && p.group_id === target.group_id) set(p, 'reject', false);
        }
      }
    }
  }

  renderStats();

  // The filter bar's "Undecided only n" pill and the 1g completion card read
  // from `counts`, not from the tiles, so repainting one tile is not enough:
  // without this, deciding the last undecided photo never raises the "All n
  // decided" card, and `u` while complete leaves the card claiming "0
  // undecided" beside a header reading "1 undecided".
  //
  // Neither call re-filters, so the tile under the cursor still cannot jump
  // away mid-cull.
  //
  // `complete || lastComplete` covers the case the undecided count misses:
  // flipping an already-decided photo while the card is up leaves `undecided`
  // at 0 but changes the keep/reject/keeper numbers the card prints.
  const complete = counts.undecided === 0 && all.length > 0;
  if (counts.undecided !== prev.undecided || complete || lastComplete) {
    renderFilterBar();
    renderGrid();
  } else {
    for (const id of touched) repaintTile(id);
  }
}

async function decide(p, action, stay) {
  // Drop the key rather than queueing it: with at most one POST outstanding the
  // server's counts can never arrive out of order, so the completion card is
  // always computed from the newest response. The cursor only advances once the
  // write has landed, so no photo is ever passed over undecided.
  if (deciding) return;
  deciding = true;
  try {
    await reviewApply(p.file_id, action);
  } finally {
    deciding = false;
  }
  if (!stay) moveCursor(cursor + 1);
}

function compareCursor() {
  const p = photos[cursor];
  if (!p) return;
  if (p.group_id == null) {
    window.pp.toast({
      kind: 'info',
      title: 'Nothing to compare',
      body: 'This photo is not in a duplicate group.',
    });
    return;
  }
  window.pp.go(`/review/compare/${p.group_id}`);
}

// ── Keyboard (spec's keyboard model) ────────────────────────────────────────

function onKey(e) {
  if (state.view !== 'review') return;
  const host = el('modal-host');
  if (host && host.children.length) return;
  // Stand down while the rows on screen are not (yet) this library's. `photos`
  // is only trustworthy between a settled load() and the next one: during the
  // /api/photos + /api/counts round trip it is either empty (a library switch,
  // which clears it in openReview) or the pre-reload snapshot of the *same*
  // library that load() is about to replace. Either way, deciding from it here
  // would post a file_id resolved against whatever catalog the server has open
  // now. The skeleton is on screen for this entire window, so there is nothing
  // for the user to aim at anyway.
  if (loading || loadError) return;
  if (e.target && e.target.closest
      && e.target.closest('input, textarea, select, [contenteditable="true"]')) return;

  const k = e.key;
  if (k === 'Escape') {
    if (ui.menu || ui.sortMenu) { ui.menu = false; ui.sortMenu = false; renderFilterBar(); }
    else if (ui.sheet) { ui.sheet = false; renderShortcutSheet(); }
    return;
  }
  if (k === '?') { ui.sheet = !ui.sheet; renderShortcutSheet(); return; }
  // Below this line every key writes to the catalog or navigates, and there is
  // no undo history on this screen. Browser chords must not be hijacked:
  // Ctrl/Cmd+X (cut) would post a *reject* and advance, Ctrl+U (view source) an
  // *undecide*, Ctrl/Cmd+F (find) would open the detail view. Placed after Esc
  // and `?` so both still work with a modifier held.
  if (e.ctrlKey || e.metaKey || e.altKey) return;
  if (!photos.length) return;

  switch (k) {
    case 'ArrowRight': case 'j': e.preventDefault(); moveCursor(cursor + 1); return;
    case 'ArrowLeft': case 'k': e.preventDefault(); moveCursor(cursor - 1); return;
    case 'ArrowDown': e.preventDefault(); moveCursor(cursor + cols()); return;
    case 'ArrowUp': e.preventDefault(); moveCursor(cursor - cols()); return;
    default: break;
  }

  const p = photos[cursor];
  if (!p) return;
  // Space would scroll the grid, so it always has to be swallowed.
  if (k === ' ') { e.preventDefault(); decide(p, 'keep', e.shiftKey); return; }
  if (k === 'x' || k === 'X') { decide(p, 'reject', e.shiftKey); return; }
  if (k === 'u' || k === 'U') { decide(p, 'undecide', e.shiftKey); return; }
  // Shift is inherently held for Shift+K, so the keeper never advances.
  if (k === 'K') { decide(p, 'keeper', true); return; }
  if (k === 'f' || k === 'F') { if (p) window.pp.go(`/review/photo/${p.file_id}`); return; }
  if (k === 'c' || k === 'C') { compareCursor(); }
}

/** Close an open popover when the click lands outside its anchor. */
function onDocDown(e) {
  if (state.view !== 'review') return;
  if (!ui.menu && !ui.sortMenu) return;
  if (e.target && e.target.closest && e.target.closest('.filter-anchor')) return;
  ui.menu = false;
  ui.sortMenu = false;
  renderFilterBar();
}

// ── Entry point ─────────────────────────────────────────────────────────────

function paintChrome(folder) {
  const name = String(folder || '').replace(/[\\/]+$/, '').split(/[\\/]/).pop() || folder;
  const root = el('view-review');
  root.innerHTML = `
    <div class="review-root" id="rv-root">
      <div class="topbar">
        <span class="topbar-crumb">Libraries</span>
        <span class="topbar-sep">/</span>
        <span class="topbar-title">${esc(name)}</span>
        <span class="topbar-count" id="rv-photos"></span>
        <div class="topbar-gap"></div>
        <button class="btn btn-icon" id="rv-theme" aria-label="Toggle theme"></button>
        <button class="btn btn-primary" id="rv-export"></button>
      </div>
      <div class="review-banners" id="rv-banners"></div>
      <div class="review-stats" id="rv-stats"></div>
      <div class="filterbar" id="rv-filters"></div>
      <div class="grid-wrap" id="rv-grid"></div>
    </div>`;

  const themeBtn = el('rv-theme');
  const paintTheme = () => {
    themeBtn.innerHTML = icon(window.pp.theme.get() === 'dark' ? 'moon' : 'sun', 15, 1.7);
  };
  paintTheme();
  themeBtn.onclick = () => { window.pp.theme.toggle(); paintTheme(); };
  el('rv-export').onclick = () => window.pp.openExport();
}

export async function openReview(folder, opts = {}) {
  state.activeFolder = folder;
  show('review');
  if (folder !== lastFolder) {
    ui.flags.clear();
    ui.undecidedOnly = false;
    ui.dupOnly = false;
    ui.keepersOnly = false;
    ui.menu = false;
    ui.sortMenu = false;
    cursor = 0;
    // The critical half: the *data* has to go too, not just the filters.
    // `show('review')` above already set state.view = 'review' synchronously,
    // and libraries.js has already POSTed /api/open, so the server's active
    // library is the new one — but `load()` is only awaited at the very end of
    // this function. Through that whole round trip `all`/`photos` still hold
    // the *previous* library's rows, whose file_id values are meaningless
    // against the new catalog (each catalog's file_id sequence restarts at 1,
    // so a stale id lands on a real, unrelated photo). Without this reset a
    // keypress in that window posts a decision onto the wrong library's
    // catalog, with no undo path. duplicates.js:571-588 does the same for its
    // `clusters`. The `loading = true` matters just as much: renderGrid() and
    // the onKey guard below both stand down on it.
    all = [];
    photos = [];
    dupPos = new Map();
    counts = { kept: 0, rejected: 0, undecided: 0 };
    lastComplete = false;
    loading = true;
    loadError = null;
    lastFolder = folder;
  }
  scoreBannerShown = false;
  paintChrome(folder);

  if (opts.pendingNew > 0) {
    window.pp.banner(el('rv-banners'), {
      kind: 'warn',
      title: `${plural(opts.pendingNew, 'new photo')} in this folder`,
      body: 'They are not in the catalog yet, so they do not appear in the grid or the counts.',
      actions: [{ label: 'Re-analyze', onClick: () => window.pp.go('/analyze', { folder }) }],
    });
  }

  if (!keysWired) {
    document.addEventListener('keydown', onKey);
    document.addEventListener('mousedown', onDocDown);
    keysWired = true;
  }

  await load();
}

/** Where `score` sits in this library's distribution of non-null IQA scores. */
function reviewIqaRank(score) {
  if (score == null) return null;
  const scores = all.map(p => p.iqa_score).filter(s => s != null);
  if (!scores.length) return null;
  const better = scores.filter(s => s > score).length;
  const pct = Math.min(100, Math.max(1, Math.round(((better + 1) / scores.length) * 100)));
  return `top ${pct}%`;
}

Object.assign(window.pp, {
  openReview,
  reviewPhotos: () => photos,
  reviewIndex: () => cursor,
  reviewSetIndex: (i) => { cursor = Math.max(0, Math.min(photos.length - 1, i)); renderGrid(); },
  reviewApply,
  reviewIqaRank,
  reviewReload: load,
});
