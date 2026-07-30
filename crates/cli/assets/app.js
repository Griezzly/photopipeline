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

// Task 2 imports rail.js and toast.js here; Tasks 3-10 add their screen
// imports. Keeping the import list in one place means index.html never
// changes again.
Promise.all([
  import('/rail.js'), import('/toast.js'), import('/libraries.js'), import('/picker.js'),
]).then(() => boot());
