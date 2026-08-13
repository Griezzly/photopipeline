import { api, show, state } from '/app.js';
import { icon } from '/icons.js';

// The server reports one step at a time within the `developing` phase. These
// strings must match ProgressSink::step's callers in
// crates/pipeline/src/develop/mod.rs, which documents the order — and the
// `step` field on JobState in crates/cli/src/serve/mod.rs.
//
// Unlike the analyze checklist, this list is not the shape of the whole run:
// it is one photo's four phases, cycling once per photo. The run's own
// progress is the "N of M photos" counter above it, which `step` deliberately
// does not disturb.
const STEPS = [
  { key: 'measuring',     label: 'Measuring the raw' },
  { key: 'rendering',     label: 'Rendering' },
  { key: 'applying look', label: 'Applying the look' },
  { key: 'encoding',      label: 'Encoding the JPEG' },
];

const esc = (s) => String(s == null ? '' : s)
  .replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;');

let timer = null;

// Remembers how far the current photo got, so a phase with no step of its own
// (`pruning`) or a terminal one (`done`, `failed`) does not redraw finished
// steps as queued. Same regression the analyze screen guards against.
let lastStepIdx = 0;

// What the estimate said when this run was started: the resolved output path
// and whether a look is active. Kept so the running screen and the summary can
// state both without re-asking the server mid-run. Refilled on a cold restore
// of `/develop/run`, where there was no modal to populate it.
let runInfo = { out_dir: '', look_available: false };

// The live confirm modal's close function, or null — the router needs to tear
// this down when it applies a different route. Same shape as export.js.
let closeFn = null;

/** Pure unmount, for the router's overlay teardown. Never navigates: the
 *  onClose hook below checks the current route precisely so a teardown the
 *  router initiated does not bounce it somewhere else. */
export function closeDevelop() {
  const c = closeFn;
  closeFn = null;
  if (c) c();
}

// An in-flight mount, so a second entry — a double-click, or the router
// draining a queued /develop navigation — joins it rather than stacking a
// second modal. The exposed window is the estimate fetch, during which nothing
// is on screen yet and every entry point is still clickable.
let pending = null;

/** Route `/develop`: the pre-flight confirm modal, over whichever screen is up. */
export async function openDevelop() {
  if (closeFn) return true; // already mounted
  if (pending) return pending; // mount in flight — both callers get one answer
  pending = openDevelopModal();
  try {
    return await pending;
  } finally {
    pending = null;
  }
}

async function openDevelopModal() {
  let est;
  try {
    est = await api('GET', '/api/finish/estimate');
  } catch (e) {
    window.pp.toast({
      kind: 'error',
      title: 'Could not size the develop run',
      body: e.status === 409 ? 'No library is open.' : e.message,
    });
    return false;
  }

  if (!est.renderer_available) {
    window.pp.toast({
      kind: 'error',
      title: 'RawTherapee is not installed',
      body: 'photopipe develops RAW files through rawtherapee-cli. Install RawTherapee, ' +
            'set [develop] rawtherapee_path in your config, then run `photopipe doctor`.',
    });
    return false;
  }

  if (!est.raw_keepers) {
    window.pp.toast({
      kind: 'info',
      title: est.keepers ? 'No RAW photos to develop' : 'Nothing to develop yet',
      body: est.keepers
        ? `${est.keepers.toLocaleString()} kept photo${est.keepers === 1 ? ' is' : 's are'} ` +
          'not RAW files, and photopipe only develops RAWs.'
        : 'Keep some photos in Review first — developing works from your keepers.',
    });
    return false;
  }

  return confirmAndStart(est);
}

function confirmAndStart(est) {
  const n = est.raw_keepers;
  const notRaw = est.keepers - est.raw_keepers;

  const m = window.pp.modal({
    title: `Develop ${n.toLocaleString()} photo${n === 1 ? '' : 's'}`,
    subtitle: 'Originals are never touched. Finished JPEGs are written alongside them.',
    width: 540,
    onClose: () => {
      closeFn = null;
      // Only navigate when the user closed this. When the router tore the
      // modal down on its way elsewhere — including the confirm path below,
      // which navigates first and lets the router unmount — the current route
      // has already moved off /develop and this is a no-op.
      if (window.pp.routerPath() === '/develop') {
        window.pp.back(window.pp.parentOf('/develop') || '/review');
      }
    },
    body: `
      <div class="exp-body">
        <div>
          <div class="section-label">Finished photos go to</div>
          <div class="exp-dest">
            <span class="exp-dest-ico">${icon('folder', 16, 1.7)}</span>
            <span class="exp-dest-path" title="${esc(est.out_dir)}">${esc(est.out_dir)}</span>
          </div>
          <div class="exp-dest-note">Set by <code>[develop] finished_dir</code> in your
            config.</div>
        </div>
        <div class="exp-stats">
          <div class="stat"><div class="stat-n">${n.toLocaleString()}</div>
            <div class="stat-label">to develop</div></div>
          <div class="stat"><div class="stat-n">${notRaw.toLocaleString()}</div>
            <div class="stat-label">kept but not RAW</div></div>
        </div>
        <div class="exp-note">${est.look_available
          ? 'The adaptive look is active — each photo gets its own colour grade, kept only ' +
            'when it does not lower the photo’s quality score.'
          : 'No look model is installed, so this run produces baseline JPEGs: exposure, white ' +
            'balance and sharpening only.'}</div>
        <label class="dv-toggle">
          <input type="checkbox" id="dv-regen">
          <span>
            <span class="dv-toggle-label">Rebuild every photo from scratch</span>
            <span class="dv-toggle-note">Deletes the finished folder and develops all
              ${n.toLocaleString()} again. Without this, photos that are already up to date are
              left alone.</span>
          </span>
        </label>
        <div class="exp-note">A run is roughly seven seconds a photo, one photo at a time —
          around an hour for 500. You can leave this screen; the run keeps going.</div>
      </div>`,
    footer: `
      <div class="modal-foot-row">
        <span class="exp-foot-gap"></span>
        <button class="btn" id="dv-cancel">Cancel</button>
        <button class="btn btn-primary" id="dv-go">Develop ${n.toLocaleString()}
          <span class="kbd">↵</span></button>
      </div>`,
  });

  closeFn = m.close;

  m.el.querySelector('#dv-cancel').onclick = () => m.close();

  const go = m.el.querySelector('#dv-go');
  go.focus();
  const run = async () => {
    go.disabled = true;
    go.textContent = 'Starting…';
    const regenerate = m.el.querySelector('#dv-regen').checked;
    try {
      await api('POST', '/api/finish', { regenerate });
    } catch (e) {
      m.close();
      window.pp.toast(e.status === 409
        ? {
            kind: 'warn',
            title: 'Something else is running',
            body: 'photopipe runs one heavy job at a time — an analysis is in flight. ' +
                  'Wait for it to finish, then start the develop run.',
          }
        : {
            kind: 'error',
            title: 'Could not start developing',
            body: `${e.message}. Nothing was written.`,
          });
      return;
    }
    // Navigate first and let the router unmount the modal: closing it here
    // would fire onClose while the route is still /develop, which would send
    // us back to review instead of on to the run.
    //
    // `replace`, not `go` — the modal's entry is a step on the way to the run
    // screen, not a place to return to. Back from the run then lands on the
    // screen the rail was clicked from, and never re-opens a confirm dialog
    // for a run that is already going. Same push-on-entry / replace-on-exit
    // shape the analyze screen uses (spec A2).
    runInfo = { out_dir: est.out_dir, look_available: est.look_available };
    window.pp.replace('/develop/run');
  };
  go.onclick = run;
  m.el.addEventListener('keydown', (e) => {
    if (e.key === 'Enter' && !go.disabled) { e.preventDefault(); run(); }
  });
  return true;
}

/** Route `/develop/run`: the checklist screen, and the summary it ends on.
 *
 *  Reached by confirming the modal, and by a reload or a Back into a run that
 *  is still going — the router only applies this route once it has confirmed
 *  with the server that there is a run worth showing. */
export async function openDevelopRun() {
  // A cold restore has no modal behind it to have filled these in. Without
  // the estimate the screen would name the library instead of the output
  // folder, and the summary would claim baseline JPEGs whether or not a look
  // model is installed.
  if (!runInfo.out_dir) {
    try {
      const est = await api('GET', '/api/finish/estimate');
      runInfo = { out_dir: est.out_dir, look_available: est.look_available };
    } catch (e) { /* the summary still carries out_dir; the look line waits */ }
  }
  lastStepIdx = 0;
  show('develop');
  render(null);
  poll();
}

function stepIndex(s) {
  // Past the loop entirely: every step of the last photo did complete.
  if (s.stage === 'pruning' || s.stage === 'done') return STEPS.length;
  const i = STEPS.findIndex(st => st.key === s.step);
  if (i !== -1) {
    lastStepIdx = i;
    return i;
  }
  return lastStepIdx;
}

// A finished run has three outcomes, not two.
//
// `isPartial` — some developed, some did not. An ordinary result: one
// unreadable file among 500 is not a failed run, and the screen must not paint
// it red.
//
// `allFailed` — photos were attempted and *none* came through. `finish_folder`
// still returns Ok, because per-file failures are isolated by design, so the
// job never reaches stage "failed" and without this the screen announced
// "Developing complete" over a column of zeros. Nothing was produced; say so.
//
// Neither covers rendered === 0 with errored === 0, which is the everyday
// "everything was already up to date" run — a success.
function isPartial(sum) {
  return !!sum && sum.errored > 0 && sum.rendered > 0;
}

function allFailed(sum) {
  return !!sum && sum.errored > 0 && sum.rendered === 0;
}

function summaryHtml(sum) {
  const cells = [
    [sum.rendered, 'developed'],
    [sum.skipped, 'already current'],
    [sum.errored, 'failed'],
    [sum.skipped_unsupported, 'not RAW'],
    [sum.pruned, 'stale files removed'],
  ].filter(([n], i) => i < 2 || n > 0);
  return `
    <div class="dv-summary">
      <div class="dv-stats">${cells.map(([n, label]) => `
        <div class="stat"><div class="stat-n">${n.toLocaleString()}</div>
          <div class="stat-label">${label}</div></div>`).join('')}</div>
      ${allFailed(sum)
        ? `<div class="exp-note">All ${sum.errored.toLocaleString()}
             attempted photo${sum.errored === 1 ? '' : 's'} failed, so nothing was written.
             When every one fails the cause is usually shared — RawTherapee missing or
             misconfigured, or the originals no longer where the catalog expects them.
             The reason for each is in the terminal running
             <code>photopipe serve</code>.</div>`
        : ''}
      ${isPartial(sum)
        ? `<div class="exp-note">${sum.errored.toLocaleString()} photo${sum.errored === 1 ? '' : 's'}
             could not be developed and ${sum.errored === 1 ? 'was' : 'were'} skipped; the rest
             came through. The reason for each is in the terminal running
             <code>photopipe serve</code>.</div>`
        : ''}
      ${!runInfo.look_available && !allFailed(sum)
        ? '<div class="exp-note">No look model is installed, so these are baseline JPEGs: ' +
          'exposure, white balance and sharpening only.</div>'
        : ''}
    </div>`;
}

function render(s) {
  const el = document.getElementById('view-develop');
  const done = !!s && s.stage === 'done';
  const failed = !!s && s.stage === 'failed';
  const sum = (s && s.summary) || null;
  const cur = s ? stepIndex(s) : 0;
  const counted = !!s && s.files_total > 0;
  const pct = counted ? Math.round((s.files_done / s.files_total) * 100) : null;
  const folder = state.activeFolder || '';

  const title = failed ? 'Developing failed'
    : done ? (allFailed(sum) ? 'No photo could be developed'
      : isPartial(sum) ? 'Developed with some skipped'
      : 'Developing complete')
    : counted ? `Developing ${s.files_total.toLocaleString()} photo${s.files_total === 1 ? '' : 's'}`
    : 'Starting…';

  const pill = failed || allFailed(sum) ? '<span class="pill pill-reject">Failed</span>'
    : done ? `<span class="pill ${isPartial(sum) ? 'pill-warn' : 'pill-done'}">${
        isPartial(sum) ? 'Partly done' : 'Done'}</span>`
    : '<span class="pill pill-run">Developing</span>';

  el.innerHTML = `
    <div class="topbar">
      <span class="topbar-crumb">Review</span>
      <span class="topbar-sep">/</span>
      <span class="topbar-title">${esc(folder.split(/[\\/]/).filter(Boolean).pop() || folder)}</span>
      ${pill}
      <div class="topbar-gap"></div>
    </div>
    <div class="center-stage">
      <div class="card an-card">
        <div class="an-head">
          <div class="an-head-text">
            <div class="an-title">${title}</div>
            <div class="an-path">${esc((sum && sum.out_dir) || runInfo.out_dir || folder)}</div>
          </div>
          ${counted && !done && !failed
            ? `<div class="an-pct-wrap">
                 <div class="an-pct">${pct}%</div>
                 <div class="an-eta">${s.files_done.toLocaleString()} of
                   ${s.files_total.toLocaleString()} photos</div>
               </div>`
            : (done || failed ? '' : '<div class="spinner" role="progressbar" aria-label="Working"></div>')}
        </div>
        <div class="an-rule"></div>
        ${sum ? summaryHtml(sum) : `<div class="an-stages">${STEPS.map((st, i) => {
          const stateName = !s ? (i === 0 ? 'active' : 'pending')
            : i < cur ? 'done' : i === cur ? 'active' : 'pending';
          const meta = stateName === 'done' ? 'done'
            : stateName === 'pending' ? 'queued'
            : (s && s.item ? esc(s.item) : 'running');
          const glyph = stateName === 'done' ? '✓' : stateName === 'active' ? '·' : '';
          return `<div class="stage-row">
              <span class="stage-dot ${stateName}">${glyph}</span>
              <span class="stage-title ${stateName}">${st.label}</span>
              <span class="stage-meta ${stateName}">${meta}</span>
            </div>`;
        }).join('')}
        <div class="stage-bar-wrap"><div class="bar" role="progressbar"
             aria-label="Photos developed"${counted
               ? ` aria-valuemin="0" aria-valuemax="100" aria-valuenow="${pct}"`
               : ''}>
          <div class="bar-fill${counted ? '' : ' indet'}"
               style="${counted ? `width:${pct}%` : ''}"></div>
        </div></div>`}
        ${!done && !failed
          // A photo takes minutes. Without a line that keeps moving, the screen
          // reads as hung long before anything is actually wrong.
          ? `<div class="an-live"><span class="an-live-dot"></span><span>${
              s && s.item ? `${esc(s.step || 'working on')} — ${esc(s.item)}`
                          : (s && s.message ? esc(s.message) : 'starting…')}</span></div>`
          : ''}
        ${failed && s.error ? `<div class="exp-note">${esc(s.error)}</div>` : ''}
        <div class="an-foot">
          <span class="an-note">${done || failed
            ? 'Originals were never touched.'
            : 'One photo at a time, by design — the renderer already uses every core.'}</span>
          <button class="btn" id="dv-back">Back to review</button>
        </div>
      </div>
    </div>`;

  el.querySelector('#dv-back').onclick = () => {
    stopPolling();
    // `replace`, matching the analyze screen: a finished — or still running —
    // job screen is not somewhere Back should re-enter. The run itself is
    // unaffected either way; it lives on the server, not on this screen.
    window.pp.replace('/review');
  };
}

function stopPolling() { if (timer) { clearTimeout(timer); timer = null; } }

function poll() {
  stopPolling();
  const tick = async () => {
    let s;
    try {
      s = await api('GET', '/api/finish/status');
    } catch (e) {
      timer = setTimeout(tick, 2000);
      return;
    }
    if (state.view !== 'develop') { stopPolling(); return; }

    render(s);
    if (s.stage === 'done') {
      stopPolling();
      const sum = s.summary;
      if (sum) {
        // The output path is deliberately not in the body: it is an absolute
        // path, it is already on the card behind this toast, and a long
        // unbroken one overflows the toast rather than wrapping.
        window.pp.toast(allFailed(sum)
          ? {
              kind: 'error',
              title: `No photo could be developed`,
              body: 'All ' + sum.errored.toLocaleString() + ' failed. The reason for each is ' +
                    'in the terminal running photopipe serve.',
            }
          : isPartial(sum)
          ? {
              kind: 'warn',
              title: `${sum.rendered.toLocaleString()} developed, ${sum.errored.toLocaleString()} skipped`,
              body: 'The finished JPEGs are in the folder shown on the screen behind this.',
            }
          : {
              kind: 'success',
              title: sum.rendered
                ? `${sum.rendered.toLocaleString()} photo${sum.rendered === 1 ? '' : 's'} developed`
                : 'Everything was already up to date',
              body: 'The finished JPEGs are in the folder shown on the screen behind this.',
            });
      }
      return;
    }
    if (s.stage === 'failed') {
      stopPolling();
      window.pp.toast({
        kind: 'error',
        title: 'Developing failed',
        body: s.error || 'No reason was reported.',
      });
      return;
    }
    // Slower than analyze's 1s: a photo takes seconds to minutes, so a faster
    // poll would only redraw the same frame.
    timer = setTimeout(tick, 1500);
  };
  tick();
}

Object.assign(window.pp, { openDevelop, openDevelopRun, closeDevelop });
