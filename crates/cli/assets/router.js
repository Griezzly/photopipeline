// Hash router. Owns location.hash and is the single entry point for every
// screen and overlay transition. See
// docs/superpowers/specs/2026-08-10-frontend-hash-routing-design.md.
//
// This module imports nothing. app.js pulls it in through the same
// Promise.all as every other screen, so everything it needs is read off
// window.pp inside functions — the same way the screens reach each other.

let appliedPath = null; // the path currently on screen
let queued = null;      // the newest navigation that arrived mid-apply
let applying = false;   // re-entrancy guard

// Routes that layer over a screen rather than replacing it. Dismissing one
// must leave the whole run it belongs to — stepping pushes an entry per
// photo, so a single back() would only reach the previous frame.
const OVERLAY_ROUTES = new Set(['photo', 'compare', 'export']);

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

/** The parent of an arbitrary path string, for callers outside this module
 *  that know their own route only as a URL. Null for a path we do not serve. */
export function parentOf(path) {
  const r = parsePath(path);
  return r ? parentPath(r) : null;
}

/** The screen `appliedPath` sits over: itself for a screen route, its parent
 *  for an overlay route. Two overlay entries belong to the same run when this
 *  agrees, which is what lets arrowing inherit the run's base. */
function appliedParent() {
  const r = appliedPath ? parsePath(appliedPath) : null;
  if (!r) return appliedPath;
  return OVERLAY_ROUTES.has(r.name) ? parentPath(r) : appliedPath;
}

/** ppBase for an entry about to be created: the depth of the screen entry
 *  this overlay run started from. Continuing an existing run inherits it;
 *  entering a new overlay records the entry we are leaving. Null for screen
 *  routes, which have no run to leave. */
function baseFor(r) {
  if (!r || !OVERLAY_ROUTES.has(r.name)) return null;
  const cur = history.state;
  if (cur && cur.ppBase != null && appliedParent() === parentPath(r)) return cur.ppBase;
  return depth();
}

// ── History mechanics ────────────────────────────────────────────────────

// ppDepth counts in-app entries behind the current one. It is stamped into
// history.state, so it survives a reload of a deep-linked URL — which is
// exactly right: after a reload the entries behind us are still there.
function depth() { return (history.state && history.state.ppDepth) || 0; }

/** The history.state for an entry naming `path`. ppDepth counts in-app
 *  entries behind it; ppBase and ppFolder are only stamped on overlay
 *  entries, which are the ones that need to know where to return to and
 *  which library their id belongs to. */
function entryState(path, ppDepth) {
  const r = parsePath(path);
  const st = { ppDepth };
  const base = baseFor(r);
  if (base != null) {
    st.ppBase = base;
    st.ppFolder = window.pp.state.activeFolder || null;
  }
  return st;
}

/** Browsers rate-limit history writes — Safari throws SecurityError past
 *  roughly 100 pushState calls per 30 seconds, and a held key can reach that.
 *  Losing the navigation entirely is worse than losing the history entry, so
 *  degrade to rewriting the current entry — the cost is a missing history
 *  entry; Back will skip the frame that was never recorded. */
function writeEntry(push, path, st) {
  try {
    if (push) history.pushState(st, '', `#${path}`);
    else history.replaceState(st, '', `#${path}`);
    return;
  } catch (e) {
    if (!push) return;
  }
  // The push was refused, so no entry was added — rewrite the current one
  // instead. It must NOT inherit the push's ppDepth: that would inflate
  // depth() by one per refusal, and exitOverlay's jump (ppBase - depth())
  // would then overshoot the run's base, potentially right out of the app.
  try {
    history.replaceState({ ...st, ppDepth: depth() }, '', `#${path}`);
  } catch (e) { /* refused too — apply() below still renders the route */ }
}

export function go(path, payload) {
  // Dedupe only the history entry, not the apply. Clicking the rail cell for
  // the screen you are on used to re-render it, and an unrouted screen can be
  // showing while appliedPath still names the routed one underneath.
  if (path !== appliedPath) writeEntry(true, path, entryState(path, depth() + 1));
  apply(path, payload || null);
}

export function replace(path, payload) {
  writeEntry(false, path, entryState(path, depth()));
  apply(path, payload || null);
}

/** Rewrite the URL without re-applying the route. For when the screen
 *  already shows the target state and re-running the applier would only
 *  refetch what is on it (detail.js's detailRefresh). */
export function setPath(path) {
  const cur = history.state || {};
  const st = { ppDepth: depth() };
  if (cur.ppBase != null) st.ppBase = cur.ppBase;
  if (cur.ppFolder != null) st.ppFolder = cur.ppFolder;
  history.replaceState(st, '', `#${path}`);
  appliedPath = path;
}

/** Step back one in-app entry. When this page load entered the current route
 *  directly — a deep link or a hand-typed hash — there is nothing of ours
 *  behind it and history.back() would leave the app, so redirect instead. */
export function back(fallback) {
  if (depth() > 0) { history.back(); return; }
  replace(fallback);
}

/** Leave the overlay run this entry belongs to, in one hop. A plain back()
 *  would only reach the previous frame. Falls back to replace() when the
 *  overlay was entered by deep link and has no run behind it. */
export function exitOverlay(fallback) {
  const st = history.state;
  const d = depth();
  if (st && typeof st.ppBase === 'number' && d > st.ppBase) { history.go(st.ppBase - d); return; }
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

  async photo(r) {
    closeOverlays('detail');
    const folder = await ensureLibrary();
    if (!folder) return '/libraries';
    // file_id is only meaningful against the catalog it came from — every
    // library restarts the sequence at 1, so a stale id from another library
    // lands on a real, unrelated photo rather than missing.
    const stamped = history.state && history.state.ppFolder;
    if (stamped && stamped !== folder) {
      window.pp.toast({
        kind: 'info',
        title: 'That photo belongs to a different library',
        body: 'Its id means nothing in the library you have open now.',
      });
      return '/review';
    }
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

  async compare(r, payload) {
    closeOverlays('compare');
    const folder = await ensureLibrary();
    if (!folder) return '/libraries';
    // group_id is only meaningful against the catalog it came from — every
    // library restarts the sequence at 1, so a stale id from another library
    // lands on a real, unrelated cluster rather than missing. Same guard as
    // ROUTES.photo, but falling back to this route's own parent rather than a
    // hardcoded path, since compare's parent varies with `over`. Checked
    // before restoring the layer underneath, so a rejected route never
    // flashes the wrong nested screen first.
    const stamped = history.state && history.state.ppFolder;
    if (stamped && stamped !== folder) {
      window.pp.toast({
        kind: 'info',
        title: 'That cluster belongs to a different library',
        body: 'Its id means nothing in the library you have open now.',
      });
      // Not parentPath(r): for over:'photo' that path carries the stale
      // file_id, which replace() would re-stamp with the current folder and
      // ROUTES.photo would then look up against the wrong catalog.
      return r.over === 'duplicates' ? '/duplicates' : '/review';
    }
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
};

/**
 * Resolve `path` onto the screen. Unknown or empty paths redirect to the
 * default. Appliers signal failure by returning a fallback path; they must
 * not navigate themselves, because `applying` would swallow it — the
 * redirect happens here, after the guard is cleared.
 */
async function apply(path, payload) {
  // A navigation that arrives mid-apply is queued, not dropped: dropping it
  // left location.hash naming a screen that never rendered, and a later Back
  // no-opped because onPop saw the hash already matching appliedPath. Only
  // the newest is kept — intermediate screens nobody sees are not worth
  // rendering.
  if (applying) { queued = { path, payload }; return; }
  const r = parsePath(path);
  if (!r || !ROUTES[r.name]) { await resolveDefault(); return; }
  applying = true;
  appliedPath = path;
  let fallback = null;
  try {
    fallback = await ROUTES[r.name](r, payload);
  } finally {
    applying = false;
  }
  const next = queued;
  queued = null;
  if (next) { apply(next.path, next.payload); return; }
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
  apply(path, null);
}

export function startRouter() {
  window.addEventListener('popstate', onPop);
  window.addEventListener('hashchange', onPop);
  const path = location.hash.replace(/^#/, '');
  if (!parsePath(path)) { resolveDefault(); return; }
  // A reload, or a Back into this page load, restores history.state along
  // with the URL. Rewriting it here would recompute ppBase/ppFolder against
  // an app that has not booted yet — appliedPath is null and activeFolder is
  // still unknown — and throw away the overlay run this entry belongs to.
  // Only stamp an entry that has none, which is the cold deep-link case.
  if (!history.state || history.state.ppDepth == null) {
    writeEntry(false, path, { ppDepth: 0 });
  }
  apply(path, null);
}

// ensureLibrary, closeOverlays, parentPath and ROUTES stay module-private —
// Tasks 3-6 add their appliers inside this file, not from the outside.
Object.assign(window.pp, {
  go, replace, back, exitOverlay, setPath, routerPath, parentOf, startRouter,
});
