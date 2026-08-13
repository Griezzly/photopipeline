import { state, show } from '/app.js';
import { icon } from '/icons.js';

const ITEMS = [
  { id: 'libraries',  label: 'Libraries',  ico: 'folder',   go: () => window.pp.go('/libraries') },
  { id: 'review',     label: 'Review',     ico: 'grid',     needsLib: true, go: () => window.pp.go('/review') },
  { id: 'duplicates', label: 'Duplicates', ico: 'layers',   needsLib: true, go: () => window.pp.go('/duplicates') },
  { id: 'export',     label: 'Export',     ico: 'download', needsLib: true, go: () => window.pp.go('/export') },
  { id: 'develop',    label: 'Develop — not yet available', ico: 'develop', soon: true },
];

function renderRail() {
  const el = document.getElementById('rail');
  if (!el) return;
  const hasLib = !!state.activeFolder;
  el.innerHTML = `
    <div class="rail-mark" title="photopipe">
      <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor"
           stroke-width="1.8" stroke-linecap="round" aria-hidden="true">
        <circle cx="12" cy="12" r="9.2"></circle>
        <path d="M8.4 8.4l7.2 7.2M15.6 8.4l-7.2 7.2"></path>
      </svg>
    </div>
    ${ITEMS.map(it => {
      const disabled = it.soon || (it.needsLib && !hasLib);
      const cls = ['rail-cell',
        it.soon ? 'soon' : '',
        state.view === it.id ? 'on' : '',
        disabled ? 'disabled' : ''].filter(Boolean).join(' ');
      return `<button class="${cls}" data-id="${it.id}" title="${it.label}"
                ${disabled ? 'disabled' : ''} aria-label="${it.label}">
                ${icon(it.ico, 18, 1.7)}${it.soon ? '<span class="rail-dot"></span>' : ''}
              </button>`;
    }).join('')}
    <div class="rail-gap"></div>
    <div class="rail-cell inert" title="Settings">${icon('settings', 18, 1.7)}</div>`;

  for (const btn of el.querySelectorAll('.rail-cell[data-id]')) {
    const it = ITEMS.find(i => i.id === btn.dataset.id);
    if (it && it.go) btn.onclick = () => it.go();
  }
}

window.pp.renderRail = renderRail;
renderRail();
