// Shared fetch helper + tiny view router.
export async function api(method, url, body) {
  const opts = { method };
  if (body !== undefined) {
    opts.headers = { 'content-type': 'application/json' };
    opts.body = JSON.stringify(body);
  }
  const r = await fetch(url, opts);
  if (!r.ok) throw new Error(`${method} ${url} → ${r.status}`);
  const ct = r.headers.get('content-type') || '';
  return ct.includes('application/json') ? r.json() : r.text();
}

export function humanBytes(n) {
  const u = ['B', 'KB', 'MB', 'GB', 'TB'];
  let v = n, i = 0;
  while (v >= 1024 && i < u.length - 1) { v /= 1024; i++; }
  return i ? `${v.toFixed(1)} ${u[i]}` : `${n} B`;
}

const VIEWS = ['home', 'browse', 'analyze', 'review'];
export function show(view) {
  for (const v of VIEWS) {
    document.getElementById(`view-${v}`).classList.toggle('hidden', v !== view);
  }
}

import { renderHome } from '/home.js';
import { renderBrowse } from '/browse.js';
import { startAnalyze } from '/analyze.js';
import { openReview } from '/review.js';

// Exposed so views can navigate without circular imports.
window.pp = { api, humanBytes, show, renderHome, renderBrowse, startAnalyze, openReview };

async function boot() {
  // If a library is already active (serve <folder>), go straight to Review.
  try {
    const active = await api('GET', '/api/active');
    if (active && active.folder) { await openReview(active.folder); return; }
  } catch (_) { /* fall through to home */ }
  await renderHome();
}
boot();
