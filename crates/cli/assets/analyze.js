import { api, show } from '/app.js';

export async function startAnalyze(folder) {
  show('analyze');
  const el = document.getElementById('view-analyze');
  el.innerHTML = `<div class="browse">
    <h2>Analyzing</h2>
    <div class="crumb">${folder}</div>
    <div id="an-stage">starting…</div>
    <div class="bar"><div id="an-fill" class="bar-fill"></div></div>
    <div id="an-detail"></div>
  </div>`;

  try { await api('POST', '/api/analyze', { folder }); }
  catch (e) {
    if (String(e.message).includes('409')) { document.getElementById('an-stage').textContent = 'An analysis is already running.'; return; }
    document.getElementById('an-stage').textContent = `Failed to start: ${e.message}`; return;
  }

  const poll = async () => {
    let s;
    try { s = await api('GET', '/api/analyze/status'); } catch (_) { setTimeout(poll, 1000); return; }
    document.getElementById('an-stage').textContent = s.message || s.stage;
    const fill = document.getElementById('an-fill');
    if (s.files_total > 0) {
      // Countable phase (scan, defects, quality): show a real 0→100% bar.
      const pct = Math.round((s.files_done / s.files_total) * 100);
      fill.classList.remove('indeterminate');
      fill.style.width = `${pct}%`;
      document.getElementById('an-detail').textContent = `${s.files_done} / ${s.files_total}`;
    } else {
      // Phase with no per-item count (calibrating, grouping): show activity, not 100%.
      fill.classList.add('indeterminate');
      fill.style.width = '100%';
      document.getElementById('an-detail').textContent = '';
    }
    if (s.stage === 'done') { window.pp.openReview(s.folder, { ml_ran: s.ml_ran }); return; }
    if (s.stage === 'failed') { document.getElementById('an-stage').textContent = `Failed: ${s.error || 'unknown error'}`; return; }
    setTimeout(poll, 1000);
  };
  poll();
}
