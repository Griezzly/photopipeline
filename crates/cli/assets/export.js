import { api, humanBytes } from '/app.js';
import { icon } from '/icons.js';

export async function openExport() {
  let est;
  try {
    est = await api('GET', '/api/export/estimate');
  } catch (e) {
    window.pp.toast({ kind: 'error', title: 'Could not size the export', body: e.message });
    return;
  }

  if (!est.files) {
    window.pp.toast({
      kind: 'info',
      title: 'Nothing new to export',
      body: 'No kept or keeper photos are waiting — any you already exported are in _keepers.',
    });
    return;
  }

  const m = window.pp.modal({
    title: `Export ${est.files.toLocaleString()} photo${est.files === 1 ? '' : 's'}`,
    subtitle: 'RAW files are copied. Originals stay where they are.',
    width: 520,
    body: `
      <div class="exp-body">
        <div>
          <div class="section-label">Destination</div>
          <div class="exp-dest">
            <span class="exp-dest-ico">${icon('folder', 16, 1.7)}</span>
            <span class="exp-dest-path">_keepers</span>
          </div>
          <div class="exp-dest-note">Relative to the folder <code>photopipe serve</code> was
            started in. Fixed for now.</div>
        </div>
        <div class="exp-stats">
          <div class="stat"><div class="stat-n">${est.files.toLocaleString()}</div>
            <div class="stat-label">photos</div></div>
          <div class="stat"><div class="stat-n">${humanBytes(est.bytes)}</div>
            <div class="stat-label">to copy</div></div>
        </div>
        <div class="exp-note">Developing keepers into JPEGs will happen here in a later
          version. For now photopipe hands you the RAW files.</div>
      </div>`,
    footer: `
      <div class="modal-foot-row">
        <span class="exp-foot-gap"></span>
        <button class="btn" id="exp-cancel">Cancel</button>
        <button class="btn btn-primary" id="exp-go">Copy ${est.files.toLocaleString()} photos
          <span class="kbd">↵</span></button>
      </div>`,
  });

  m.el.querySelector('#exp-cancel').onclick = () => m.close();

  const go = m.el.querySelector('#exp-go');
  go.focus();
  const run = async () => {
    go.disabled = true;
    go.textContent = 'Copying…';
    let r;
    try {
      r = await api('POST', '/api/export', { regenerate: false });
    } catch (e) {
      m.close();
      window.pp.toast({
        kind: 'error',
        title: 'Export failed',
        body: `${e.message}. Nothing was overwritten.`,
        actions: [{ label: 'Retry', onClick: () => { openExport(); } }],
      });
      return;
    }
    m.close();
    const body = `${humanBytes(r.bytes_copied)} copied` +
      (r.errors ? ` · ${r.errors} file${r.errors === 1 ? '' : 's'} failed` : '');
    window.pp.toast({
      kind: r.errors ? 'warn' : 'success',
      title: `${r.files_copied.toLocaleString()} photo${r.files_copied === 1 ? '' : 's'} copied to _keepers`,
      body,
    });
  };
  go.onclick = run;
  m.el.addEventListener('keydown', (e) => {
    if (e.key === 'Enter' && !go.disabled) { e.preventDefault(); run(); }
  });
}

Object.assign(window.pp, { openExport });
