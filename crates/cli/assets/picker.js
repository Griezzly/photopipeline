import { api } from '/app.js';
import { icon } from '/icons.js';

// Folder paths of every analyzed library, normalised for comparison. Fetched
// once per picker session so each row can carry a truthful "Analyzed" badge.
let analyzed = new Set();

const norm = (p) => p.replace(/[\\/]+$/, '').toLowerCase();

export async function openPicker(startPath) {
  try {
    const libs = await api('GET', '/api/libraries');
    analyzed = new Set(libs.map(l => norm(l.folder)));
  } catch (e) {
    analyzed = new Set();   // badge is a nicety; a failure must not block browsing
  }

  const m = window.pp.modal({
    title: 'Choose a folder to analyze',
    subtitle: 'photopipe reads RAW files in place. Nothing is moved or written.',
    width: 720,
    body: `
      <div class="picker-nav">
        <button class="btn btn-icon sm" id="pk-up" title="Up one level">${icon('arrow-up', 15, 1.9)}</button>
        <div class="picker-crumbs" id="pk-crumbs"></div>
      </div>
      <div class="picker-list" id="pk-list"></div>`,
    footer: `
      <div class="picker-foot">
        <div class="picker-cur">
          <div class="section-label">Current folder</div>
          <div class="picker-cur-path" id="pk-cur">—</div>
        </div>
        <button class="btn" id="pk-cancel">Cancel</button>
        <button class="btn btn-primary" id="pk-go" disabled>Analyze</button>
      </div>`,
  });

  let cur = null;

  m.el.querySelector('#pk-cancel').onclick = () => m.close();

  const load = async (path) => {
    const list = m.el.querySelector('#pk-list');
    list.innerHTML = '<div class="picker-loading">Reading…</div>';
    let listing;
    try {
      const q = path ? `?path=${encodeURIComponent(path)}` : '';
      listing = await api('GET', `/api/fs${q}`);
    } catch (e) {
      list.innerHTML = '<div class="picker-loading">Cannot read that folder.</div>';
      window.pp.toast({ kind: 'error', title: 'Cannot read folder', body: e.message });
      return;
    }

    cur = listing.path || null;
    m.el.querySelector('#pk-cur').textContent = cur || 'Pick a drive or location';

    // Breadcrumbs from the path itself; the last segment is the current folder.
    const crumbs = m.el.querySelector('#pk-crumbs');
    if (cur) {
      const parts = cur.split(/[\\/]/).filter(Boolean);
      crumbs.innerHTML = parts.map((p, i) =>
        `<span class="crumb-seg${i === parts.length - 1 ? ' on' : ''}">${p}</span>`
      ).join('<span class="crumb-sep">/</span>');
    } else {
      crumbs.innerHTML = '<span class="crumb-seg">Locations</span>';
    }

    const up = m.el.querySelector('#pk-up');
    up.disabled = !listing.parent;
    up.onclick = () => load(listing.parent);

    const go = m.el.querySelector('#pk-go');
    const total = listing.entries.reduce((a, e) => a + e.photo_count, 0);
    go.disabled = !cur;
    go.textContent = cur ? 'Analyze this folder' : 'Analyze';
    go.onclick = () => { m.close(); window.pp.startAnalyze(cur); };

    list.innerHTML = '';
    if (!listing.entries.length) {
      list.innerHTML = '<div class="picker-loading">No sub-folders here.</div>';
      return;
    }
    for (const e of listing.entries) {
      const done = analyzed.has(norm(e.path));
      const row = document.createElement('button');
      row.className = 'picker-row' + (e.photo_count === 0 ? ' quiet' : '');
      row.innerHTML = `
        <span class="picker-ico">${icon('folder', 17, 1.7)}</span>
        <span class="picker-name">${e.name}</span>
        ${done ? '<span class="pill pill-done">Analyzed</span>' : ''}
        <span class="picker-count">${e.photo_count ? `${e.photo_count.toLocaleString()} photos` : 'no photos'}</span>
        <span class="picker-chev">${icon('chevron-right', 15, 1.8)}</span>`;
      row.onclick = () => load(e.path);
      list.appendChild(row);
    }
  };

  await load(startPath || null);
}

Object.assign(window.pp, { openPicker });
