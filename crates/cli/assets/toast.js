import { icon } from '/icons.js';

const HOST = () => document.getElementById('toast-host');
const esc = (s) => String(s == null ? '' : s)
  .replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;');
const KIND_ICON = { success: 'check', error: 'error', warn: 'warn', info: 'refresh' };
const AUTO_MS = { success: 6000, info: 6000 };

function actionsHtml(actions) {
  return (actions || [])
    .map((a, i) => `<button class="btn btn-ghost sm" data-act="${i}">${esc(a.label)}</button>`)
    .join('');
}

function wireActions(el, actions, close) {
  for (const b of el.querySelectorAll('[data-act]')) {
    const a = actions[Number(b.dataset.act)];
    b.onclick = () => { const keep = a.onClick && a.onClick(); if (!keep) close(); };
  }
  const x = el.querySelector('.notice-x');
  if (x) x.onclick = close;
}

// `title` and `body` are escaped here, once, rather than at each of the ~30
// call sites. They are interpolated user data as often as not — duplicates.js
// puts a photo *filename* in the title, picker/analyze put folder names and
// server error strings in the body — and none of the call sites passes
// intentional markup (all of them build plain strings; only the icons and the
// action buttons this module renders itself are markup). Escaping at the sink
// means a future caller cannot reintroduce the hole by forgetting.
function noticeHtml(kind, title, body, actions, inline) {
  return `<span class="notice-ico">${icon(KIND_ICON[kind] || 'info', 15, 2.1)}</span>
    <div class="notice-text">
      <div class="notice-title">${esc(title)}</div>
      ${body ? `<div class="notice-body">${esc(body)}</div>` : ''}
      ${inline && actions && actions.length ? `<div class="notice-acts">${actionsHtml(actions)}</div>` : ''}
    </div>
    ${!inline ? actionsHtml(actions) : ''}
    <button class="notice-x" aria-label="Dismiss">${icon('close', 12, 2.2)}</button>`;
}

/** Floating toast. Returns a dismiss function. */
export function toast({ kind = 'info', title, body, actions }) {
  const el = document.createElement('div');
  el.className = `toast ${kind}`;
  el.innerHTML = noticeHtml(kind, title, body, actions, false);
  HOST().appendChild(el);
  let done = false;
  const close = () => { if (done) return; done = true; el.remove(); };
  wireActions(el, actions || [], close);
  const ms = AUTO_MS[kind];
  if (ms) setTimeout(close, ms);
  return close;
}

/** Inline banner (mockup 1a / 1k). Appended to hostEl. */
export function banner(hostEl, { kind = 'info', title, body, actions, onDismiss }) {
  const el = document.createElement('div');
  el.className = `notice ${kind}`;
  el.innerHTML = noticeHtml(kind, title, body, actions, true);
  hostEl.appendChild(el);
  const close = () => { el.remove(); if (onDismiss) onDismiss(); };
  wireActions(el, actions || [], close);
  return close;
}

/** Scrim + panel in #modal-host. Closes on Esc and scrim click. */
export function modal({ title, subtitle, body, footer, width = 520, onClose }) {
  const scrim = document.createElement('div');
  scrim.className = 'modal-scrim';
  scrim.innerHTML = `<div class="modal" style="width:${width}px" role="dialog" aria-modal="true">
      ${title ? `<div class="modal-head">
        <div class="modal-title">${title}</div>
        ${subtitle ? `<div class="modal-sub">${subtitle}</div>` : ''}
      </div>` : ''}
      <div class="modal-body"></div>
      ${footer ? '<div class="modal-foot"></div>' : ''}
    </div>`;

  const panel = scrim.querySelector('.modal');
  const bodyEl = scrim.querySelector('.modal-body');
  if (typeof body === 'string') bodyEl.innerHTML = body;
  else if (body) bodyEl.appendChild(body);
  if (footer) {
    const f = scrim.querySelector('.modal-foot');
    if (typeof footer === 'string') f.innerHTML = footer;
    else f.appendChild(footer);
  }

  let done = false;
  const close = () => {
    if (done) return;
    done = true;
    document.removeEventListener('keydown', onKey, true);
    scrim.remove();
    if (onClose) onClose();
  };
  const onKey = (e) => { if (e.key === 'Escape') { e.stopPropagation(); close(); } };
  document.addEventListener('keydown', onKey, true);
  scrim.onclick = (e) => { if (e.target === scrim) close(); };
  panel.onclick = (e) => e.stopPropagation();

  document.getElementById('modal-host').appendChild(scrim);
  return { el: panel, body: bodyEl, close };
}

/** confirm() replacement. Resolves true on confirm, false on cancel/Esc. */
export function confirmDialog({ title, body, confirmLabel = 'Continue', danger = false }) {
  return new Promise((resolve) => {
    let settled = false;
    const finish = (v) => { if (settled) return; settled = true; resolve(v); m.close(); };
    const m = modal({
      title,
      body: `<p class="modal-copy">${body}</p>`,
      footer: `<div class="modal-foot-row">
          <button class="btn" data-no>Cancel</button>
          <button class="btn ${danger ? 'btn-danger' : 'btn-primary'}" data-yes>${confirmLabel}
            <span class="kbd">↵</span></button>
        </div>`,
      width: 460,
      onClose: () => { if (!settled) { settled = true; resolve(false); } },
    });
    m.el.querySelector('[data-no]').onclick = () => finish(false);
    m.el.querySelector('[data-yes]').onclick = () => finish(true);
    m.el.querySelector('[data-yes]').focus();
  });
}

Object.assign(window.pp, { toast, banner, modal, confirmDialog });
