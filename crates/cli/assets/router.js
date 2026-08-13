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
  // Dedupe only the history entry, not the apply. Clicking the rail cell for
  // the screen you are on used to re-render it, and an unrouted screen can be
  // showing while appliedPath still names the routed one underneath.
  if (path !== appliedPath) history.pushState({ ppDepth: depth() + 1 }, '', `#${path}`);
  apply(path, payload || null);
}

export function replace(path, payload) {
  history.replaceState({ ppDepth: depth() }, '', `#${path}`);
  apply(path, payload || null);
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
  // replace() rather than a bare apply(), so the first entry carries
  // ppDepth 0 and back() knows there is nothing of ours behind it.
  if (parsePath(path)) replace(path);
  else resolveDefault();
}

// ensureLibrary, closeOverlays, parentPath and ROUTES stay module-private —
// Tasks 3-6 add their appliers inside this file, not from the outside.
Object.assign(window.pp, { go, replace, back, setPath, routerPath, startRouter });
