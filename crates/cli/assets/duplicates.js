// Screen 1i — duplicates review. One card per cluster: the algorithm's
// suggested keeper is a dashed *suggestion*, never a decision. Nothing in a
// cluster is rejected until the user explicitly sets a keeper (confirmed
// first, named siblings and all), and every set is one undo away.
//
// Two mockup numbers are deliberately absent everywhere in this module: the
// per-cluster capture time range ("20:14:29 → 20:14:33") and the "96%
// similar" figure. `ReviewCluster` exposes neither `captured_at` range nor a
// similarity score, and inventing either would be exactly the kind of
// fabricated data the project's non-negotiable rules forbid.
import { api, show, state } from '/app.js';
import { icon } from '/icons.js';

// The folder this module last painted for. `group_id` restarts at 1 in every
// library's own catalog, and switching libraries is a pure SPA transition
// with no reload, so every async action captures `folder` before its first
// `await` and re-checks it after — a stale response from a library the user
// has since left must never touch this screen's DOM.
let folder = null;
let lastFolder = null;

let clusters = [];         // ReviewCluster[] — the last successful /api/clusters fetch.
let undecidedOnly = true;  // Mockup's default "Undecided clusters" filter.
let confirming = null;     // { groupId, fileId } of the open confirm popover, or null.
let skipped = new Set();   // group_id set dismissed from the undecided-only view this session.
let lastDecidedGroupId = null; // for the `u` shortcut's "most recently decided cluster".
let loading = true;
let loadError = null;
let emptyNoScores = false; // true when the library has zero clusters AND zero IQA scores.
let keysWired = false;

const el = (id) => document.getElementById(id);
const esc = (s) => String(s == null ? '' : s)
  .replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;');
const plural = (n, w) => `${n.toLocaleString()} ${w}${n === 1 ? '' : 's'}`;

/** Last path segment, for either separator. */
function basename(path) {
  const s = String(path || '');
  const i = Math.max(s.lastIndexOf('/'), s.lastIndexOf('\\'));
  return i === -1 ? s : s.slice(i + 1);
}

/** Filename without its extension — matches the mockup's "DSC04182" labels. */
function basenameNoExt(path) {
  const b = basename(path);
  const i = b.lastIndexOf('.');
  return i > 0 ? b.slice(0, i) : b;
}

/** 'keeper-set' when any member has been made the keeper, else 'undecided'.
 *  There is no third state: a cluster with only plain keep/reject verdicts
 *  and no keeper is still 'undecided' for this screen's purposes — the
 *  duplicates workflow is specifically about picking *the* keeper. */
function clusterState(c) {
  return c.members.some((m) => m.is_keeper) ? 'keeper-set' : 'undecided';
}

function decisionOf(m) {
  if (m.is_keeper) return 'keeper';
  if (m.verdict === 'keep') return 'keep';
  if (m.verdict === 'reject') return 'reject';
  return 'undecided';
}

// ── Top bar ──────────────────────────────────────────────────────────────

function renderTopBar() {
  const host = el('dup-topbar');
  if (!host) return;
  const name = basename(folder) || folder || '';

  // While a fetch for the *current* folder is outstanding (including the
  // instant after a folder switch, before `load()`'s first await), `clusters`
  // is not yet known to describe this folder — it is either empty (folder
  // just changed) or a leftover from before this load started. Render only
  // the static crumb/title and omit every data-driven control, most
  // importantly "Accept all suggestions": that button posts decisions using
  // `clusters`' file_id/group_id values, so it must not exist in the DOM
  // until a confirmed-fresh fetch has landed. This mirrors review.js's
  // paintChrome(), which never paints a data-driven control before `load()`
  // resolves either.
  if (loading) {
    host.innerHTML = `
      <span class="topbar-crumb">${esc(name)}</span>
      <span class="topbar-sep">/</span>
      <span class="topbar-title">Duplicates</span>
      <span class="topbar-count" id="dup-summary">Loading…</span>`;
    return;
  }

  const decided = clusters.filter((c) => clusterState(c) === 'keeper-set').length;
  const frames = clusters.reduce((n, c) => n + c.members.length, 0);
  const eligible = clusters.filter((c) => clusterState(c) === 'undecided' && c.suggested_keeper_id != null).length;

  host.innerHTML = `
    <span class="topbar-crumb">${esc(name)}</span>
    <span class="topbar-sep">/</span>
    <span class="topbar-title">Duplicates</span>
    <span class="topbar-count" id="dup-summary">${plural(clusters.length, 'cluster')} ·
      ${decided.toLocaleString()} decided · ${plural(frames, 'frame')}</span>
    <div class="topbar-gap"></div>
    <button class="btn sm ${undecidedOnly ? 'on' : ''}" id="dup-toggle">
      Undecided clusters${icon('chevron-down', 13, 1.9)}</button>
    <button class="btn sm" id="dup-accept-all" ${eligible ? '' : 'disabled'}>Accept all suggestions</button>`;

  el('dup-toggle').onclick = () => { undecidedOnly = !undecidedOnly; renderTopBar(); render(); };
  el('dup-accept-all').onclick = () => acceptAll();
}

// ── Cluster cards (1i:606-686, clusterFrames() 1022-1036) ───────────────

function frameHtml(m, c, csState) {
  const dec = decisionOf(m);
  let tag = null;
  if (m.is_keeper) tag = { cls: 'keeper', text: '★ Keeper' };
  else if (m.verdict === 'reject') tag = { cls: 'rejected', text: 'Rejected' };
  else if (csState === 'undecided' && c.suggested_keeper_id === m.file_id) tag = { cls: 'suggested', text: 'Suggested best' };

  const hasScore = m.iqa_score != null;
  const name = esc(basenameNoExt(m.path));
  // Every member but the current keeper can be nominated — including a
  // rejected sibling in an already-decided cluster, which just re-runs
  // pick_keeper with a different target rather than requiring Undo first.
  const showPick = !m.is_keeper;

  return `<div class="tile dup-frame ${dec}" data-fid="${m.file_id}" title="${name}">
    <img class="tile-img" loading="lazy" src="/thumb/${m.file_id}" alt="">
    ${tag ? `<span class="tile-tag ${tag.cls}">${esc(tag.text)}</span>` : ''}
    ${showPick ? `<button class="dup-frame-pick" data-fid="${m.file_id}"
        title="Set ${name} as keeper" aria-label="Set ${name} as keeper">★</button>` : ''}
    <div class="tile-foot">
      <span class="tile-file">${name}</span>
      <span class="tile-foot-gap"></span>
      <span class="tile-score">${hasScore ? m.iqa_score.toFixed(2) : '—'}</span>
    </div>
  </div>`;
}

/** The 1i confirm popover, anchored bottom-right of the cluster body. Only
 *  rendered while `confirming` names this cluster. */
function popoverHtml(c) {
  if (!confirming || confirming.groupId !== c.group_id) return '';
  const target = c.members.find((m) => m.file_id === confirming.fileId);
  if (!target) return '';
  const siblings = c.members.filter((m) => m.file_id !== confirming.fileId);
  const shown = siblings.slice(0, 4).map((m) => basenameNoExt(m.path));
  const names = shown.join(', ') + (siblings.length > shown.length ? `, +${siblings.length - shown.length} more` : '');
  return `
    <div class="dup-pop">
      <div class="dup-pop-title">Keep ${esc(basenameNoExt(target.path))} and reject
        ${plural(siblings.length, 'sibling')}?</div>
      <div class="dup-pop-body">${esc(names)} become <span class="dup-pop-reject">reject</span>.
        One undo restores all ${c.members.length}.</div>
      <div class="dup-pop-acts">
        <button class="btn btn-primary sm" data-confirm-yes>★ Set keeper<span class="kbd">↵</span></button>
        <button class="btn sm" data-confirm-no>Cancel<span class="kbd">Esc</span></button>
      </div>
    </div>`;
}

function clusterHeaderHtml(c, csState) {
  const decided = csState === 'keeper-set';
  const pillCls = decided ? 'dup-pill-keeper-set' : 'dup-pill-undecided';
  const pillText = decided ? 'Keeper set' : 'Undecided';
  const rightBtn = decided
    ? `<button class="btn sm" data-undo="${c.group_id}">${icon('undo', 13, 1.9)}Undo<span class="kbd">U</span></button>`
    : `<button class="btn btn-ghost sm" data-skip="${c.group_id}">Skip cluster</button>`;
  return `
    <div class="dup-head">
      <span class="dup-title">Cluster ${String(c.group_id).padStart(2, '0')}</span>
      <span class="dup-meta">${plural(c.members.length, 'frame')} · ${esc(c.date)}</span>
      <span class="dup-pill ${pillCls}">${pillText}</span>
      <div class="topbar-gap"></div>
      <button class="btn sm" data-compare="${c.group_id}">Compare<span class="kbd">C</span></button>
      ${rightBtn}
    </div>`;
}

function clusterHtml(c) {
  const csState = clusterState(c);
  return `
    <div class="dup-cluster ${csState === 'keeper-set' ? 'decided' : ''}" data-group="${c.group_id}">
      ${clusterHeaderHtml(c, csState)}
      <div class="dup-body">
        ${c.members.map((m) => frameHtml(m, c, csState)).join('')}
        ${popoverHtml(c)}
      </div>
    </div>`;
}

/** The screen's governing rule, verbatim from the mockup's own footnote. */
function noteHtml() {
  return `<div class="dup-note">
    <span class="notice-ico">${icon('info', 14, 1.9)}</span>
    <span>A suggestion is never a decision. The dashed frame is the algorithm's pick —
      nothing is rejected until you set a keeper, and every set is one undo away.</span>
  </div>`;
}

// ── List body ─────────────────────────────────────────────────────────────

function render() {
  const host = el('dup-list');
  if (!host) return;

  if (loading) {
    host.innerHTML = `
      ${Array.from({ length: 3 }, () => '<div class="skeleton" style="height:223px"></div>').join('')}
      <div class="grid-loading-note">Loading duplicate clusters…</div>`;
    return;
  }

  if (loadError) {
    host.innerHTML = `
      <div class="empty">
        <span class="empty-icon">${icon('warn', 22, 1.7)}</span>
        <div>
          <div class="empty-title">Could not load duplicate clusters</div>
          <div class="empty-body">${esc(loadError)}</div>
        </div>
        <button class="btn btn-primary" id="dup-retry">${icon('refresh', 14, 2)}Try again</button>
      </div>`;
    el('dup-retry').onclick = () => load();
    return;
  }

  if (!clusters.length) {
    const body = emptyNoScores
      ? "Duplicate grouping runs on the IQA model's quality score, and this library has none yet. "
        + 'Analyze it again once the models are installed to find clusters.'
      : 'Nothing in this library looked similar enough to group.';
    host.innerHTML = `
      <div class="empty">
        <span class="empty-icon">${icon('layers', 22, 1.6)}</span>
        <div>
          <div class="empty-title">No duplicate groups in this library</div>
          <div class="empty-body">${esc(body)}</div>
        </div>
      </div>
      ${noteHtml()}`;
    return;
  }

  const visible = clusters.filter((c) => {
    if (!undecidedOnly) return true;
    return clusterState(c) === 'undecided' && !skipped.has(c.group_id);
  });

  if (!visible.length) {
    host.innerHTML = `
      <div class="empty">
        <span class="empty-icon">${icon('check', 22, 2)}</span>
        <div>
          <div class="empty-title">No undecided clusters</div>
          <div class="empty-body">${skipped.size
            ? 'Everything left is either decided or skipped.'
            : 'Every cluster in this library already has a keeper.'}</div>
        </div>
        <button class="btn" id="dup-show-all">Show all clusters</button>
      </div>
      ${noteHtml()}`;
    el('dup-show-all').onclick = () => { undecidedOnly = false; renderTopBar(); render(); };
    return;
  }

  host.innerHTML = visible.map((c) => clusterHtml(c)).join('') + noteHtml();
  wireListHandlers(host);
}

/** One delegated listener for the whole list — cheap to rebind on every full
 *  render, and unaffected by the targeted per-cluster DOM replacement that
 *  `renderClusterCard` does after a decision. */
function wireListHandlers(host) {
  host.onclick = (e) => {
    const compareBtn = e.target.closest('[data-compare]');
    if (compareBtn) { window.pp.openCompare(Number(compareBtn.dataset.compare)); return; }

    const skipBtn = e.target.closest('[data-skip]');
    if (skipBtn) { skipped.add(Number(skipBtn.dataset.skip)); render(); return; }

    const undoBtn = e.target.closest('[data-undo]');
    if (undoBtn) { undoCluster(Number(undoBtn.dataset.undo)); return; }

    const pickBtn = e.target.closest('.dup-frame-pick');
    if (pickBtn) {
      const card = pickBtn.closest('.dup-cluster');
      if (!card) return;
      confirming = { groupId: Number(card.dataset.group), fileId: Number(pickBtn.dataset.fid) };
      renderClusterCard(confirming.groupId);
      return;
    }

    if (e.target.closest('[data-confirm-yes]')) {
      if (confirming) pickKeeper(confirming.groupId, confirming.fileId);
      return;
    }
    if (e.target.closest('[data-confirm-no]')) {
      if (confirming) { const g = confirming.groupId; confirming = null; renderClusterCard(g); }
    }
  };
}

/** Repaint one cluster in place; falls back to a full `render()` if the
 *  cluster's new state would change whether it's visible under the current
 *  filter (e.g. it just became decided while "Undecided clusters" is on). */
function renderClusterCard(groupId) {
  const c = clusters.find((x) => x.group_id === groupId);
  if (!c) { render(); return; }
  const stillVisible = !undecidedOnly || (clusterState(c) === 'undecided' && !skipped.has(groupId));
  const host = el('dup-list');
  const node = host && host.querySelector(`.dup-cluster[data-group="${groupId}"]`);
  if (!stillVisible || !node) { render(); return; }
  const tmp = document.createElement('div');
  tmp.innerHTML = clusterHtml(c);
  node.replaceWith(tmp.firstElementChild);
}

// ── Loading ──────────────────────────────────────────────────────────────

/** Only reachable when the cluster list is empty — checks whether the whole
 *  library has no IQA scores at all, which is why duplicate grouping (which
 *  runs on the quality score) found nothing to group. */
async function checkEmptyLibraryScores(openedFolder) {
  try {
    const rows = await api('GET', '/api/photos?limit=100000');
    if (folder !== openedFolder) return;
    emptyNoScores = rows.length > 0 && rows.every((r) => r.iqa_score == null);
  } catch (e) {
    emptyNoScores = false;
  }
}

async function load() {
  const openedFolder = folder;
  loading = true;
  loadError = null;
  render();

  let rows;
  try {
    rows = await api('GET', '/api/clusters');
  } catch (e) {
    if (folder !== openedFolder) return; // switched libraries mid-fetch
    clusters = [];
    loading = false;
    loadError = e.message;
    window.pp.toast({ kind: 'error', title: 'Could not load duplicate clusters', body: e.message });
    renderTopBar();
    render();
    return;
  }
  if (folder !== openedFolder) return; // switched libraries mid-fetch

  clusters = rows;
  loading = false;
  emptyNoScores = false;
  if (!clusters.length) await checkEmptyLibraryScores(openedFolder);
  if (folder !== openedFolder) return;

  renderTopBar();
  render();
}

// ── Decisions ────────────────────────────────────────────────────────────

/**
 * Route the write through `window.pp.reviewApply` — never POST `/api/decisions`
 * directly, or the review grid's own cache desyncs. `reviewApply` never
 * throws (an API failure raises its own toast and leaves its state alone), so
 * it gives this module no success/failure signal by itself. Rather than
 * assume success — which would risk this screen showing a decision the
 * catalog never recorded, exactly what the "a suggestion is never a
 * decision" rule forbids — this re-fetches `/api/clusters` afterward and
 * mirrors whatever the server actually holds. Returns the fresh cluster on
 * confirmed success, `null` otherwise (including on failure or a folder
 * switch mid-flight).
 */
async function decideKeeper(groupId, fileId) {
  const openedFolder = folder;
  await window.pp.reviewApply(fileId, 'keeper');
  if (folder !== openedFolder) return null;

  let fresh;
  try { fresh = await api('GET', '/api/clusters'); } catch (e) { return null; }
  if (folder !== openedFolder) return null;

  const c = fresh.find((x) => x.group_id === groupId);
  if (!c) return null;
  const idx = clusters.findIndex((x) => x.group_id === groupId);
  if (idx >= 0) clusters[idx] = c; else clusters.push(c);

  const target = c.members.find((m) => m.file_id === fileId);
  return target && target.is_keeper ? c : null;
}

async function pickKeeper(groupId, fileId) {
  const openedFolder = folder;
  const c = clusters.find((x) => x.group_id === groupId);
  if (!c) return;
  const target = c.members.find((m) => m.file_id === fileId);
  const label = target ? basenameNoExt(target.path) : `file ${fileId}`;
  const siblingCount = c.members.length - 1;

  confirming = null;
  renderClusterCard(groupId);

  const fresh = await decideKeeper(groupId, fileId);
  if (folder !== openedFolder) return; // this screen no longer applies

  renderTopBar();
  renderClusterCard(groupId);
  if (fresh) {
    lastDecidedGroupId = groupId;
    window.pp.toast({
      kind: 'success',
      title: `${label} set as keeper`,
      body: `${plural(siblingCount, 'sibling')} rejected.`,
      actions: [{ label: 'Undo', onClick: () => undoCluster(groupId) }],
    });
  }
  // else: reviewApply already raised its own error toast on a write failure,
  // or the confirming fetch failed — either way the re-render above shows
  // whatever the server actually holds, never a guess.
}

/** Undecide every member of a cluster, one at a time. Stops at the first
 *  write that doesn't verify against the server rather than plowing ahead
 *  and leaving the cluster half-applied. */
async function undoCluster(groupId) {
  const openedFolder = folder;
  const c = clusters.find((x) => x.group_id === groupId);
  if (!c) return;
  const total = c.members.length;
  let done = 0;

  for (const m of c.members.slice()) {
    if (m.verdict == null && !m.is_keeper) { done++; continue; }

    await window.pp.reviewApply(m.file_id, 'undecide');
    if (folder !== openedFolder) return;

    let fresh;
    try { fresh = await api('GET', '/api/clusters'); } catch (e) { fresh = null; }
    if (folder !== openedFolder) return;
    const freshC = fresh && fresh.find((x) => x.group_id === groupId);
    if (!freshC) break; // could not confirm anything further — stop, don't guess

    const idx = clusters.findIndex((x) => x.group_id === groupId);
    if (idx >= 0) clusters[idx] = freshC;
    const freshMember = freshC.members.find((x) => x.file_id === m.file_id);
    const ok = freshMember && freshMember.verdict == null && !freshMember.is_keeper;
    if (!ok) {
      renderTopBar();
      renderClusterCard(groupId);
      window.pp.toast({
        kind: 'error',
        title: 'Undo stopped partway',
        body: `${plural(done, 'frame')} of ${total} reset before a write failed. Run Undo again to finish.`,
      });
      return;
    }
    done++;
  }

  renderTopBar();
  renderClusterCard(groupId);
  window.pp.toast({
    kind: 'success',
    title: `Cluster ${String(groupId).padStart(2, '0')} reset`,
    body: `${plural(done, 'frame')} back to undecided.`,
  });
}

async function acceptAll() {
  const eligible = clusters.filter((c) => clusterState(c) === 'undecided' && c.suggested_keeper_id != null);
  if (!eligible.length) {
    window.pp.toast({
      kind: 'info',
      title: 'Nothing to accept',
      body: 'No undecided cluster has a suggested keeper.',
    });
    return;
  }
  const siblingTotal = eligible.reduce((n, c) => n + c.members.length - 1, 0);
  const ok = await window.pp.confirmDialog({
    title: `Set the suggested keeper in ${plural(eligible.length, 'cluster')}?`,
    body: `${plural(siblingTotal, 'sibling frame')} become reject. Each cluster stays individually undoable.`,
    confirmLabel: 'Accept all suggestions',
  });
  if (!ok) return;

  const openedFolder = folder;
  let succeeded = 0;
  let failed = 0;
  for (const c of eligible) {
    if (folder !== openedFolder) return;
    const fresh = await decideKeeper(c.group_id, c.suggested_keeper_id);
    if (fresh) { succeeded++; lastDecidedGroupId = c.group_id; } else { failed++; }
  }
  if (folder !== openedFolder) return;

  renderTopBar();
  render();
  window.pp.toast({
    kind: failed ? 'warn' : 'success',
    title: `${succeeded} succeeded${failed ? `, ${failed} failed` : ''}`,
  });
}

// ── Keyboard ─────────────────────────────────────────────────────────────

function onKey(e) {
  if (state.view !== 'duplicates') return;
  const host = el('modal-host');
  if (host && host.children.length) return; // a true modal (e.g. Accept-all's confirm) owns input
  if (e.target && e.target.closest
      && e.target.closest('input, textarea, select, [contenteditable="true"]')) return;
  if (e.ctrlKey || e.metaKey || e.altKey) return;

  if (confirming) {
    if (e.key === 'Enter') { e.preventDefault(); pickKeeper(confirming.groupId, confirming.fileId); }
    else if (e.key === 'Escape') {
      e.preventDefault();
      const g = confirming.groupId;
      confirming = null;
      renderClusterCard(g);
    }
    return; // no other shortcut fires while the popover is open
  }

  if (e.key === 'c' || e.key === 'C') {
    const first = clusters.find((c) => clusterState(c) === 'undecided');
    if (!first) {
      window.pp.toast({ kind: 'info', title: 'No undecided clusters', body: 'Every cluster already has a keeper.' });
      return;
    }
    window.pp.openCompare(first.group_id);
    return;
  }
  if (e.key === 'u' || e.key === 'U') {
    if (lastDecidedGroupId != null) undoCluster(lastDecidedGroupId);
  }
}

/** Closes the confirm popover on an outside click, same pattern as the
 *  review grid's filter-popover dismissal. */
function onDocDown(e) {
  if (state.view !== 'duplicates') return;
  if (!confirming) return;
  if (e.target && e.target.closest && (e.target.closest('.dup-pop') || e.target.closest('.dup-frame-pick'))) return;
  const g = confirming.groupId;
  confirming = null;
  renderClusterCard(g);
}

// ── Entry point ──────────────────────────────────────────────────────────

function paintShell() {
  const root = el('view-duplicates');
  root.innerHTML = `
    <div class="topbar" id="dup-topbar"></div>
    <div class="dup-list" id="dup-list"></div>`;
}

export async function openDuplicates(fromFolder) {
  folder = fromFolder;
  state.activeFolder = fromFolder;
  show('duplicates');

  if (folder !== lastFolder) {
    undecidedOnly = true;
    skipped = new Set();
    confirming = null;
    lastDecidedGroupId = null;
    // The critical fix: `clusters` is the previous library's data and its
    // file_id/group_id values mean nothing against the new folder's catalog
    // (each library has its own sequence). Without this, the synchronous
    // renderTopBar()/render() below would paint a genuinely clickable
    // "Accept all suggestions" built from the old library's clusters, and
    // nothing would repaint it until /api/clusters resolves — a live button
    // that, if clicked in that window, would decide keepers using the old
    // library's file_id/group_id values against the now-active new library.
    clusters = [];
    loading = true;
    loadError = null;
    lastFolder = folder;
  }

  paintShell();
  renderTopBar();
  render();

  if (!keysWired) {
    document.addEventListener('keydown', onKey);
    document.addEventListener('mousedown', onDocDown);
    keysWired = true;
  }

  await load();
}

Object.assign(window.pp, { openDuplicates });
