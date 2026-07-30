import { api, show, state } from '/app.js';

// The server reports one stage string at a time. The checklist's done/active/
// pending state is derived from position in this ordered list — these strings
// must match JobState.stage in crates/cli/src/serve/mod.rs.
const STAGES = [
  { key: 'scanning',           label: 'Scanning folder' },
  { key: 'detecting defects',  label: 'Detecting defects' },
  { key: 'scoring quality',    label: 'Scoring quality' },
  { key: 'calibrating',        label: 'Calibrating thresholds' },
  { key: 'grouping duplicates', label: 'Grouping duplicates' },
];

let timer = null;

// Remembers how far the run actually got, so a terminal or unrecognised stage
// string does not regress the checklist. `failed` is set server-side without
// resetting files_done/files_total (see handlers.rs), so falling back to index 0
// would redraw completed stages as "queued" underneath a failure toast.
let lastStageIdx = 0;

export async function startAnalyze(folder, opts = {}) {
  state.activeFolder = folder;
  show('analyze');
  render(folder, null);

  if (!opts.resume) {
    try {
      await api('POST', '/api/analyze', { folder });
    } catch (e) {
      if (e.status !== 409) {
        window.pp.toast({ kind: 'error', title: 'Could not start the analysis', body: e.message });
        window.pp.openLibraries();
        return;
      }
      // 409 means a job is already in flight — fall through and attach to it.
    }
  }
  poll(folder);
}

function stageIndex(stage) {
  if (stage === 'done') return STAGES.length;
  const i = STAGES.findIndex(s => s.key === stage);
  if (i !== -1) {
    lastStageIdx = i;
    return i;
  }
  return lastStageIdx;
}

function render(folder, s) {
  const el = document.getElementById('view-analyze');
  const cur = s ? stageIndex(s.stage) : 0;
  const counted = !!s && s.files_total > 0;
  const pct = counted ? Math.round((s.files_done / s.files_total) * 100) : null;

  el.innerHTML = `
    <div class="topbar">
      <span class="topbar-crumb">Libraries</span>
      <span class="topbar-sep">/</span>
      <span class="topbar-title">${folder.split(/[\\/]/).filter(Boolean).pop() || folder}</span>
      <span class="pill pill-run">Analyzing</span>
      <div class="topbar-gap"></div>
    </div>
    <div class="center-stage">
      <div class="card an-card">
        <div class="an-head">
          <div class="an-head-text">
            <div class="an-title">${counted
              ? `Analyzing ${s.files_total.toLocaleString()} photos`
              : (s ? STAGES[Math.min(cur, STAGES.length - 1)].label : 'Starting…')}</div>
            <div class="an-path">${folder}</div>
          </div>
          ${counted
            ? `<div class="an-pct-wrap">
                 <div class="an-pct">${pct}%</div>
                 <div class="an-eta">${s.files_done.toLocaleString()} of ${s.files_total.toLocaleString()}</div>
               </div>`
            : '<div class="spinner" role="progressbar" aria-label="Working"></div>'}
        </div>
        <div class="an-rule"></div>
        <div class="an-stages">${STAGES.map((st, i) => {
          const stateName = !s ? (i === 0 ? 'active' : 'pending')
            : i < cur ? 'done' : i === cur ? 'active' : 'pending';
          const meta = stateName === 'done' ? 'done'
            : stateName === 'pending' ? 'queued'
            : counted ? `${s.files_done.toLocaleString()} / ${s.files_total.toLocaleString()}`
            : 'running';
          const glyph = stateName === 'done' ? '✓' : stateName === 'active' ? '·' : '';
          const showBar = stateName === 'active';
          return `<div class="stage-row">
              <span class="stage-dot ${stateName}">${glyph}</span>
              <span class="stage-title ${stateName}">${st.label}</span>
              <span class="stage-meta ${stateName}">${meta}</span>
            </div>
            ${showBar ? `<div class="stage-bar-wrap"><div class="bar">
              <div class="bar-fill${counted ? '' : ' indet'}"
                   style="${counted ? `width:${pct}%` : ''}"></div>
            </div></div>` : ''}`;
        }).join('')}</div>
        ${s && !counted && s.message
          ? `<div class="an-live"><span class="an-live-dot"></span><span>${s.message}</span></div>`
          : ''}
        <div class="an-foot">
          <span class="an-note">Decisions stay editable while later stages run.</span>
          <button class="btn" id="an-back">Back to libraries</button>
          ${s && s.files_done > 0
            ? `<button class="btn btn-primary" id="an-review">Review ${s.files_done.toLocaleString()} so far</button>`
            : ''}
        </div>
      </div>
    </div>`;

  el.querySelector('#an-back').onclick = () => { stopPolling(); window.pp.openLibraries(); };
  const rv = el.querySelector('#an-review');
  if (rv) rv.onclick = () => { stopPolling(); window.pp.openReview(folder); };
}

function stopPolling() { if (timer) { clearTimeout(timer); timer = null; } }

function poll(folder) {
  stopPolling();
  const tick = async () => {
    let s;
    try {
      s = await api('GET', '/api/analyze/status');
    } catch (e) {
      timer = setTimeout(tick, 1500);
      return;
    }
    if (state.view !== 'analyze') { stopPolling(); return; }

    if (s.stage === 'done') {
      stopPolling();
      if (s.ml_ran === false) {
        window.pp.toast({
          kind: 'warn',
          title: 'Quality scores and duplicate groups were skipped',
          body: "The ML models aren't installed, so this library has defect flags only. " +
                'Tiles show a dash instead of a score.',
        });
      }
      window.pp.openReview(s.folder || folder);
      return;
    }
    if (s.stage === 'failed') {
      stopPolling();
      render(folder, s);
      window.pp.toast({
        kind: 'error',
        title: 'Analysis failed',
        body: s.error || 'No reason was reported.',
      });
      return;
    }
    render(folder, s);
    timer = setTimeout(tick, 1000);
  };
  tick();
}

Object.assign(window.pp, { startAnalyze });
