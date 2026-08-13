import { api, show, state } from '/app.js';
import { icon } from '/icons.js';

// Each divisor is paired with the unit it converts INTO, not the unit it
// converts from — dividing seconds by 60 yields minutes, so that row is
// labelled 'minute'. Pairing it with 'second' makes every label one
// conversion step stale ("1 hour ago" for a day-old library).
function relTime(unixSecs) {
  if (!unixSecs) return 'never';
  const s = Math.max(0, Math.floor(Date.now() / 1000 - unixSecs));
  const steps = [[60, 'minute'], [60, 'hour'], [24, 'day'], [7, 'week'], [4.35, 'month'], [12, 'year']];
  let v = s, unit = 'second';
  for (const [span, name] of steps) {
    if (v < span) break;
    v = v / span; unit = name;
  }
  if (unit === 'second' && v < 45) return 'moments ago';
  const n = Math.round(v);
  return `${n} ${unit}${n === 1 ? '' : 's'} ago`;
}

export async function openLibraries() {
  state.activeFolder = null;
  show('libraries');
  const el = document.getElementById('view-libraries');
  el.innerHTML = `
    <div class="topbar">
      <span class="topbar-crumb">photopipe</span>
      <span class="topbar-sep">/</span>
      <span class="topbar-title">Libraries</span>
      <div class="topbar-gap"></div>
      <button class="btn btn-icon" id="lib-theme" aria-label="Toggle theme"></button>
      <button class="btn btn-primary" id="lib-analyze">${icon('plus', 14, 2)}Analyze folder</button>
    </div>
    <div class="pad-page">
      <div id="lib-banners"></div>
      <div class="page-head">
        <div class="page-title">Libraries</div>
        <div class="page-sub" id="lib-sub"></div>
      </div>
      <div class="panel" id="lib-panel">
        <div class="lib-row lib-head">
          <div>Folder</div><div class="r">Photos</div><div class="r">Analyzed</div><div></div>
        </div>
        <div id="lib-rows"><div class="lib-empty">Loading…</div></div>
        <button class="lib-add" id="lib-add">
          <span class="lib-add-ico">${icon('plus', 15, 2)}</span>
          <span>Analyze a new folder…</span>
        </button>
      </div>
    </div>`;

  const themeBtn = document.getElementById('lib-theme');
  const paintTheme = () => {
    themeBtn.innerHTML = icon(window.pp.theme.get() === 'dark' ? 'moon' : 'sun', 15, 1.7);
  };
  paintTheme();
  themeBtn.onclick = () => { window.pp.theme.toggle(); paintTheme(); };

  const openPicker = () => window.pp.openPicker(null);
  document.getElementById('lib-analyze').onclick = openPicker;
  document.getElementById('lib-add').onclick = openPicker;

  await loadLibraries();
}

async function loadLibraries() {
  const rows = document.getElementById('lib-rows');
  const sub = document.getElementById('lib-sub');
  let libs;
  try {
    libs = await api('GET', '/api/libraries');
  } catch (e) {
    rows.innerHTML = '<div class="lib-empty">Could not read your libraries.</div>';
    window.pp.toast({ kind: 'error', title: 'Failed to load libraries', body: e.message });
    return;
  }

  if (!libs.length) {
    sub.textContent = 'No analyzed folders yet';
    rows.innerHTML = `<div class="lib-empty">Point photopipe at a folder of RAW files and it
      will flag the obvious failures before you look at a single frame.</div>`;
    return;
  }

  const total = libs.reduce((a, l) => a + l.photo_count, 0);
  const newest = libs.reduce((a, l) => Math.max(a, l.last_analyzed || 0), 0);
  sub.textContent = `${libs.length} analyzed folder${libs.length === 1 ? '' : 's'} · ` +
    `${total.toLocaleString()} photos · last session ${relTime(newest)}`;

  rows.innerHTML = '';
  for (const l of libs) {
    const name = l.folder.replace(/[\\/]+$/, '').split(/[\\/]/).pop() || l.folder;
    const row = document.createElement('button');
    row.className = 'lib-row';
    row.innerHTML = `
      <div class="lib-name-cell">
        <span class="lib-ico">${icon('folder', 16, 1.7)}</span>
        <div class="lib-name-text">
          <div class="lib-name">${name}</div>
          <div class="lib-path">${l.folder}</div>
        </div>
      </div>
      <div class="r lib-count">${l.photo_count.toLocaleString()}</div>
      <div class="r lib-when">${relTime(l.last_analyzed)}</div>
      <div class="lib-chev">${icon('chevron-right', 16, 1.8)}</div>`;
    row.onclick = () => openLibrary(l.folder);
    rows.appendChild(row);
  }
}

async function openLibrary(folder) {
  let res;
  try {
    res = await api('POST', '/api/open', { folder });
  } catch (e) {
    if (e.status === 409) { window.pp.go('/analyze', { folder, resume: true }); return; }
    window.pp.toast({ kind: 'error', title: 'Could not open that library', body: e.message });
    return;
  }
  state.activeFolder = res.folder;
  window.pp.go('/review', { pendingNew: res.pending_new });
}

Object.assign(window.pp, { openLibraries });
