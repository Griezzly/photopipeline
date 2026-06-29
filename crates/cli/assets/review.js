import { api, humanBytes, show } from '/app.js';

let photos = [], sel = 0, detailOpen = false, activeFolder = null;

const grid = () => document.getElementById('grid');
const countsEl = () => document.getElementById('counts');
const flagFilter = () => document.getElementById('flag-filter');
const decidedFilter = () => document.getElementById('decided-filter');

export async function openReview(folder, opts = {}) {
  show('review');
  activeFolder = folder;
  document.getElementById('review-title').textContent = folder;
  renderBanners(opts.ml_ran);
  wireChrome();
  await loadPhotos();
}

async function renderBanners(mlRan) {
  const b = document.getElementById('banners');
  b.innerHTML = '';
  if (mlRan === false) {
    const d = document.createElement('div');
    d.className = 'banner';
    d.textContent = 'Models not found — quality, subject-aware blur, and duplicate detection were skipped.';
    b.appendChild(d);
  }
  // Staleness: re-open to get pending_new.
  try {
    const o = await api('POST', '/api/open', { folder: activeFolder });
    if (o.pending_new > 0) {
      const d = document.createElement('div');
      d.className = 'banner';
      d.innerHTML = `<span>${o.pending_new} new photo(s) found.</span><button id="reanalyze">Re-analyze</button>`;
      d.querySelector('#reanalyze').onclick = () => window.pp.startAnalyze(activeFolder);
      b.appendChild(d);
    }
  } catch (_) {}
}

function wireChrome() {
  document.getElementById('home-btn').onclick = () => window.pp.renderHome();
  flagFilter().onchange = loadPhotos;
  decidedFilter().onchange = loadPhotos;
  document.getElementById('export-btn').onclick = onExport;
  document.onkeydown = onKey;
}

function qs() {
  const p = new URLSearchParams();
  if (flagFilter().value) p.set('flag_type', flagFilter().value);
  if (decidedFilter().value) p.set('decided', decidedFilter().value);
  const s = p.toString();
  return s ? `?${s}` : '';
}

async function loadPhotos() {
  photos = await api('GET', `/api/photos${qs()}`);
  if (sel >= photos.length) sel = Math.max(0, photos.length - 1);
  renderGrid();
  refreshCounts();
}

function tileClass(p) {
  let c = 'tile';
  if (p.verdict === 'keep') c += ' keep';
  else if (p.verdict === 'reject') c += ' reject';
  return c;
}

function renderGrid() {
  const g = grid();
  g.innerHTML = '';
  photos.forEach((p, i) => {
    const el = document.createElement('div');
    el.className = tileClass(p) + (i === sel ? ' sel' : '');
    const flags = p.flags.length ? p.flags.join(', ') : (p.group_id != null ? 'dup' : 'clean');
    el.innerHTML = `<img loading="lazy" src="/thumb/${p.file_id}" alt="">
      <span class="badge">${flags}${p.iqa_score != null ? ` · iqa ${p.iqa_score.toFixed(2)}` : ''}</span>`;
    el.addEventListener('click', () => { sel = i; openDetail(); });
    g.appendChild(el);
  });
  const selEl = g.querySelector('.tile.sel');
  if (selEl) selEl.scrollIntoView({ block: 'nearest' });
}

function showCounts(c) { countsEl().textContent = `keep ${c.kept} · reject ${c.rejected} · undecided ${c.undecided}`; }
async function refreshCounts() { try { showCounts(await api('GET', '/api/counts')); } catch { countsEl().textContent = ''; } }

async function setVerdict(action) {
  const p = photos[sel];
  if (!p) return;
  const c = await api('POST', '/api/decisions', { file_id: p.file_id, action });
  if (action === 'keep' || action === 'keeper') p.verdict = 'keep';
  else if (action === 'reject') p.verdict = 'reject';
  else if (action === 'undecide') { p.verdict = null; p.is_keeper = false; }
  showCounts(c);
  renderGrid();
  if (detailOpen) renderDetailMeta(p);
}

async function openDetail() {
  const p = photos[sel];
  if (!p) return;
  detailOpen = true;
  document.getElementById('detail').classList.remove('hidden');
  document.getElementById('detail-img').src = `/preview/${p.file_id}`;
  renderDetailMeta(p);
}
function closeDetail() { detailOpen = false; document.getElementById('detail').classList.add('hidden'); renderGrid(); }
function renderDetailMeta(p) {
  document.getElementById('detail-meta').innerHTML = `<dl>
    <dt>path</dt><dd>${p.path}</dd>
    <dt>flags</dt><dd>${p.flags.join(', ') || '—'}</dd>
    <dt>iqa</dt><dd>${p.iqa_score != null ? p.iqa_score.toFixed(3) : '—'}</dd>
    <dt>group</dt><dd>${p.group_id != null ? p.group_id : '—'}</dd>
    <dt>verdict</dt><dd>${p.verdict || 'undecided'}</dd>
  </dl><p>Space/Enter keep · X reject · U undecide · K keeper · F/Esc back</p>`;
}

function move(d) { if (!photos.length) return; sel = Math.min(photos.length - 1, Math.max(0, sel + d)); if (detailOpen) openDetail(); else renderGrid(); }

function onKey(e) {
  if (document.getElementById('view-review').classList.contains('hidden')) return;
  switch (e.key) {
    case 'j': case 'ArrowRight': move(1); break;
    case 'k': case 'ArrowLeft': move(-1); break;
    case ' ': case 'Enter': e.preventDefault(); setVerdict('keep'); break;
    case 'x': case 'X': setVerdict('reject'); break;
    case 'u': case 'U': setVerdict('undecide'); break;
    case 'K': setVerdict('keeper'); break;
    case 'f': case 'F': detailOpen ? closeDetail() : openDetail(); break;
    case 'Escape': if (detailOpen) closeDetail(); break;
  }
}

async function onExport() {
  try {
    const est = await api('GET', '/api/export/estimate');
    if (!confirm(`This will copy ${est.files} photo(s) (${humanBytes(est.bytes)}) to the "_keepers" folder (relative to where 'photopipe serve' was started). Continue?`)) return;
    const r = await api('POST', '/api/export', { regenerate: false });
    alert(`Copied ${r.files_copied} photo(s) (${humanBytes(r.bytes_copied)}), ${r.errors} error(s).`);
  } catch (err) { alert(`Export failed: ${err.message}`); }
}
