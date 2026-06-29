import { api, show } from '/app.js';

export async function renderHome() {
  show('home');
  const el = document.getElementById('view-home');
  el.innerHTML = `<div class="home">
    <h1>photopipe</h1>
    <button id="analyze-new" class="primary">Analyze a folder…</button>
    <h2>Recent libraries</h2>
    <div id="lib-list" class="lib-list">Loading…</div>
  </div>`;
  document.getElementById('analyze-new').onclick = () => window.pp.renderBrowse(null);

  const list = document.getElementById('lib-list');
  try {
    const libs = await api('GET', '/api/libraries');
    if (!libs.length) { list.textContent = 'None yet — analyze a folder to get started.'; return; }
    list.innerHTML = '';
    for (const l of libs) {
      const when = l.last_analyzed ? new Date(l.last_analyzed * 1000).toLocaleString() : 'never';
      const card = document.createElement('button');
      card.className = 'lib-card';
      card.innerHTML = `<div class="lib-folder">${l.folder}</div>
        <div class="lib-meta">${l.photo_count} photos · analyzed ${when}</div>`;
      card.onclick = async () => { await api('POST', '/api/open', { folder: l.folder }); window.pp.openReview(l.folder); };
      list.appendChild(card);
    }
  } catch (e) { list.textContent = `Failed to load libraries: ${e.message}`; }
}
