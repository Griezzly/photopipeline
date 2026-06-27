// photopipe review — zero-build vanilla frontend.
const grid = document.getElementById('grid');
const detail = document.getElementById('detail');
const detailImg = document.getElementById('detail-img');
const detailMeta = document.getElementById('detail-meta');
const countsEl = document.getElementById('counts');
const flagFilter = document.getElementById('flag-filter');
const decidedFilter = document.getElementById('decided-filter');

let photos = [];   // current list
let sel = 0;       // selected index
let detailOpen = false;

async function api(method, url, body) {
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

function queryString() {
  const p = new URLSearchParams();
  if (flagFilter.value) p.set('flag_type', flagFilter.value);
  if (decidedFilter.value) p.set('decided', decidedFilter.value);
  const s = p.toString();
  return s ? `?${s}` : '';
}

async function loadPhotos() {
  photos = await api('GET', `/api/photos${queryString()}`);
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
  grid.innerHTML = '';
  photos.forEach((p, i) => {
    const el = document.createElement('div');
    el.className = tileClass(p) + (i === sel ? ' sel' : '');
    el.dataset.idx = i;
    const flags = p.flags.length ? p.flags.join(', ') : (p.group_id != null ? 'dup' : 'clean');
    el.innerHTML = `<img loading="lazy" src="/thumb/${p.file_id}" alt="">
      <span class="badge">${flags}${p.iqa_score != null ? ` · iqa ${p.iqa_score.toFixed(2)}` : ''}</span>`;
    el.addEventListener('click', () => { sel = i; openDetail(); });
    grid.appendChild(el);
  });
  const selEl = grid.querySelector('.tile.sel');
  if (selEl) selEl.scrollIntoView({ block: 'nearest' });
}

function showCounts(c) {
  countsEl.textContent = `keep ${c.kept} · reject ${c.rejected} · undecided ${c.undecided}`;
}

async function refreshCounts() {
  try {
    showCounts(await api('GET', '/api/counts'));
  } catch {
    countsEl.textContent = '';
  }
}

async function setVerdict(action) {
  const p = photos[sel];
  if (!p) return;
  const c = await api('POST', '/api/decisions', { file_id: p.file_id, action });
  // Update local state without a full reload.
  if (action === 'keep') p.verdict = 'keep';
  else if (action === 'reject') p.verdict = 'reject';
  else if (action === 'undecide') { p.verdict = null; p.is_keeper = false; }
  else if (action === 'keeper') p.verdict = 'keep';
  showCounts(c);
  renderGrid();
  if (detailOpen) renderDetailMeta(p);
}

async function openDetail() {
  const p = photos[sel];
  if (!p) return;
  detailOpen = true;
  detail.classList.remove('hidden');
  detailImg.src = `/preview/${p.file_id}`;
  renderDetailMeta(p);
}

function closeDetail() {
  detailOpen = false;
  detail.classList.add('hidden');
  renderGrid();
}

function renderDetailMeta(p) {
  const v = p.verdict || 'undecided';
  detailMeta.innerHTML = `<dl>
    <dt>path</dt><dd>${p.path}</dd>
    <dt>flags</dt><dd>${p.flags.join(', ') || '—'}</dd>
    <dt>iqa</dt><dd>${p.iqa_score != null ? p.iqa_score.toFixed(3) : '—'}</dd>
    <dt>group</dt><dd>${p.group_id != null ? p.group_id : '—'}</dd>
    <dt>verdict</dt><dd>${v}</dd>
  </dl><p>Space/Enter keep · X reject · U undecide · K keeper · F/Esc back</p>`;
}

function move(delta) {
  if (!photos.length) return;
  sel = Math.min(photos.length - 1, Math.max(0, sel + delta));
  if (detailOpen) openDetail(); else renderGrid();
}

document.addEventListener('keydown', (e) => {
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
});

flagFilter.addEventListener('change', loadPhotos);
decidedFilter.addEventListener('change', loadPhotos);
function humanBytes(n) {
  const u = ['B', 'KB', 'MB', 'GB', 'TB'];
  let v = n, i = 0;
  while (v >= 1024 && i < u.length - 1) { v /= 1024; i++; }
  return i ? `${v.toFixed(1)} ${u[i]}` : `${n} B`;
}

document.getElementById('export-btn').addEventListener('click', async () => {
  try {
    const est = await api('GET', '/api/export/estimate');
    const ok = confirm(
      `This will copy ${est.files} photo(s) (${humanBytes(est.bytes)}) to the "_keepers" ` +
      `folder (relative to where 'photopipe serve' was started). Continue?`
    );
    if (!ok) return;
    const r = await api('POST', '/api/export', { regenerate: false });
    alert(`Copied ${r.files_copied} photo(s) (${humanBytes(r.bytes_copied)}), ${r.errors} error(s).`);
  } catch (err) {
    alert(`Export failed: ${err.message}`);
  }
});

loadPhotos();
