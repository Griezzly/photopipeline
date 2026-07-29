import { api, show } from '/app.js';

// Dedicated duplicate-cluster review: one row per group, suggested keeper first,
// others best-IQA-first. Click a tile to inspect it big; "★ Keep this" makes it
// the keeper and rejects the rest of its group in one action (pick_keeper).

let clusters = [];
let folder = null;
let box = null; // { ci, mi } of the open lightbox, or null.

export async function openDuplicates(fromFolder) {
  folder = fromFolder;
  show('duplicates');
  const el = document.getElementById('view-duplicates');
  el.innerHTML = `
    <header>
      <button id="dup-back">← Review grid</button>
      <strong>Duplicate groups</strong>
      <span class="spacer"></span>
      <span id="dup-summary" class="lib-meta"></span>
    </header>
    <main id="dup-list" class="dup-list">Loading…</main>
    <div id="dup-box" class="detail hidden">
      <img id="dup-box-img" alt="">
      <aside id="dup-box-meta"></aside>
    </div>`;
  document.getElementById('dup-back').onclick = () => window.pp.openReview(folder);
  document.onkeydown = onKey;
  await load();
}

async function load() {
  const list = document.getElementById('dup-list');
  try { clusters = await api('GET', '/api/clusters'); }
  catch (e) { list.textContent = `Failed to load duplicate groups: ${e.message}`; return; }
  render();
}

function render() {
  const list = document.getElementById('dup-list');
  const summary = document.getElementById('dup-summary');
  list.innerHTML = '';
  if (!clusters.length) {
    summary.textContent = '';
    list.innerHTML = '<p class="lib-meta">No duplicate groups found in this library.</p>';
    return;
  }
  const undecided = clusters.filter(c => !c.members.some(m => m.verdict === 'keep')).length;
  summary.textContent = `${clusters.length} group(s) · ${undecided} awaiting a keeper`;
  clusters.forEach((c, ci) => list.appendChild(renderCluster(c, ci)));
}

function renderCluster(c, ci) {
  const wrap = document.createElement('section');
  wrap.className = 'cluster';
  wrap.id = `cluster-${ci}`;
  const chosen = c.members.some(m => m.verdict === 'keep');
  const head = document.createElement('div');
  head.className = 'cluster-head';
  head.innerHTML = `<span class="cluster-title">Group ${c.group_id}</span>
    <span class="lib-meta">${c.date} · ${c.members.length} shots</span>
    <span class="spacer"></span>
    <span class="cluster-status ${chosen ? 'done' : ''}">${chosen ? '✓ keeper chosen' : 'pick the best'}</span>`;
  wrap.appendChild(head);

  const row = document.createElement('div');
  row.className = 'cluster-row';
  c.members.forEach((m, mi) => row.appendChild(renderMember(m, c, ci, mi)));
  wrap.appendChild(row);
  return wrap;
}

function memberClass(m, c) {
  let cls = 'tile';
  if (m.verdict === 'keep') cls += ' keep';
  else if (m.verdict === 'reject') cls += ' reject';
  else if (m.file_id === c.suggested_keeper_id) cls += ' suggested';
  return cls;
}

function renderMember(m, c, ci, mi) {
  const el = document.createElement('div');
  el.className = memberClass(m, c);
  const chosen = m.verdict === 'keep';
  const suggested = m.file_id === c.suggested_keeper_id;
  const tag = chosen ? '★ KEEPER' : (suggested ? 'suggested' : '');
  const iqa = m.iqa_score != null ? `iqa ${m.iqa_score.toFixed(2)}` : 'no iqa';
  el.innerHTML = `<img loading="lazy" src="/thumb/${m.file_id}" alt="">
    <span class="badge">${tag ? tag + ' · ' : ''}${iqa}</span>
    <button class="pick-btn">${chosen ? '★ Keeper' : '★ Keep this'}</button>`;
  el.querySelector('img').onclick = () => openBox(ci, mi);
  el.querySelector('.pick-btn').onclick = (e) => { e.stopPropagation(); pickKeeper(ci, mi); };
  return el;
}

async function pickKeeper(ci, mi) {
  const c = clusters[ci];
  const chosenId = c.members[mi].file_id;
  try { await api('POST', '/api/decisions', { file_id: chosenId, action: 'keeper' }); }
  catch (e) { alert(`Failed to set keeper: ${e.message}`); return; }
  // pick_keeper keeps this file and rejects every sibling — mirror that locally.
  c.members.forEach(x => {
    const keep = x.file_id === chosenId;
    x.verdict = keep ? 'keep' : 'reject';
    x.is_keeper = keep;
  });
  document.getElementById(`cluster-${ci}`).replaceWith(renderCluster(c, ci));
  const summary = document.getElementById('dup-summary');
  const undecided = clusters.filter(cl => !cl.members.some(m => m.verdict === 'keep')).length;
  summary.textContent = `${clusters.length} group(s) · ${undecided} awaiting a keeper`;
  if (box) renderBox();
}

// ── Lightbox: inspect one shot at full size, arrow between the group's shots ──
function openBox(ci, mi) { box = { ci, mi }; document.getElementById('dup-box').classList.remove('hidden'); renderBox(); }
function closeBox() { box = null; document.getElementById('dup-box').classList.add('hidden'); }

function renderBox() {
  const c = clusters[box.ci];
  const m = c.members[box.mi];
  document.getElementById('dup-box-img').src = `/preview/${m.file_id}`;
  const suggested = m.file_id === c.suggested_keeper_id;
  document.getElementById('dup-box-meta').innerHTML = `<dl>
    <dt>group</dt><dd>${c.group_id} · ${c.date}</dd>
    <dt>shot</dt><dd>${box.mi + 1} of ${c.members.length}</dd>
    <dt>iqa</dt><dd>${m.iqa_score != null ? m.iqa_score.toFixed(3) : '—'}${suggested ? ' · suggested keeper' : ''}</dd>
    <dt>flags</dt><dd>${m.flags.join(', ') || '—'}</dd>
    <dt>verdict</dt><dd>${m.verdict || 'undecided'}</dd>
    <dt>path</dt><dd>${m.path}</dd>
  </dl>
  <button id="dup-box-pick" class="primary">★ Keep this one (reject the rest)</button>
  <p>← / → compare · Enter keep this · Esc close</p>`;
  document.getElementById('dup-box-pick').onclick = () => pickKeeper(box.ci, box.mi);
}

function moveBox(d) {
  const n = clusters[box.ci].members.length;
  box.mi = Math.min(n - 1, Math.max(0, box.mi + d));
  renderBox();
}

function onKey(e) {
  if (document.getElementById('view-duplicates').classList.contains('hidden')) return;
  if (!box) return;
  switch (e.key) {
    case 'ArrowRight': case 'j': moveBox(1); break;
    case 'ArrowLeft': case 'k': moveBox(-1); break;
    case 'Enter': case ' ': e.preventDefault(); pickKeeper(box.ci, box.mi); break;
    case 'Escape': closeBox(); break;
  }
}
