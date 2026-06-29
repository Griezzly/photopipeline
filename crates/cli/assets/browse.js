import { api, show } from '/app.js';

export async function renderBrowse(path) {
  show('browse');
  const el = document.getElementById('view-browse');
  el.innerHTML = `<div class="browse">
    <header><button id="browse-home">← Home</button><span id="crumb" class="crumb"></span></header>
    <div id="fs-list" class="fs-list">Loading…</div>
    <footer><button id="analyze-here" class="primary" disabled>Analyze this folder</button></footer>
  </div>`;
  document.getElementById('browse-home').onclick = () => window.pp.renderHome();

  const q = path ? `?path=${encodeURIComponent(path)}` : '';
  let listing;
  try { listing = await api('GET', `/api/fs${q}`); }
  catch (e) { document.getElementById('fs-list').textContent = `Cannot read folder: ${e.message}`; return; }

  document.getElementById('crumb').textContent = listing.path || 'Pick a drive / location';
  const analyzeBtn = document.getElementById('analyze-here');
  if (listing.path) {
    analyzeBtn.disabled = false;
    analyzeBtn.onclick = () => window.pp.startAnalyze(listing.path);
  }

  const list = document.getElementById('fs-list');
  list.innerHTML = '';
  if (listing.parent) {
    const up = document.createElement('button');
    up.className = 'fs-row';
    up.textContent = '⬆ ..';
    up.onclick = () => renderBrowse(listing.parent);
    list.appendChild(up);
  }
  for (const e of listing.entries) {
    const row = document.createElement('button');
    row.className = 'fs-row';
    row.innerHTML = `<span class="fs-name">📁 ${e.name}</span><span class="fs-count">${e.photo_count ? e.photo_count + ' photos' : ''}</span>`;
    row.onclick = () => renderBrowse(e.path);
    list.appendChild(row);
  }
}
