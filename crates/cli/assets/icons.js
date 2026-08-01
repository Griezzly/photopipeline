// SVG paths lifted from docs/design/mockups/. Do not redraw these by hand —
// re-copy from the mockup if a glyph looks wrong.
export const ICONS = {
  // Rail (Rail.dc.html ITEMS)
  folder: 'M3 7.5A2 2 0 0 1 5 5.5h3.4l2 2H19a2 2 0 0 1 2 2v7a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z',
  grid: 'M4 4h7v7H4zM13 4h7v7h-7zM4 13h7v7H4zM13 13h7v7h-7z',
  layers: 'M12 3 3 8l9 5 9-5zM3 13l9 5 9-5',
  download: 'M12 4v11M7.5 11l4.5 4.5L16.5 11M4 20h16',
  develop: 'M12 3.5v3M12 17.5v3M3.5 12h3M17.5 12h3M6.4 6.4l2.1 2.1M15.5 15.5l2.1 2.1M17.6 6.4l-2.1 2.1M8.5 15.5l-2.1 2.1',
  settings: 'M6 4v5M6 13v7M18 4v7M18 15v5M3 11h6M15 13h6',
  // Chrome
  sun: 'M12 3v2M12 19v2M3 12h2M19 12h2M5.6 5.6l1.4 1.4M16.9 16.9l1.4 1.4M18.4 5.6L17 7M7 17l-1.4 1.4',  // 1a top bar
  moon: 'M20.5 14.6A8.5 8.5 0 0 1 9.4 3.5a8.5 8.5 0 1 0 11.1 11.1z',                                     // 1e top bar
  plus: 'M12 5v14M5 12h14',
  close: 'M6 6l12 12M18 6L6 18',
  check: 'M4 12.5l5 5 11-11',
  'chevron-right': 'M9 5l7 7-7 7',
  'chevron-down': 'M6 9l6 6 6-6',
  'chevron-left': 'M14 6l-6 6 6 6',
  'arrow-up': 'M12 19V6M6 12l6-6 6 6',
  refresh: 'M4 12a8 8 0 0 1 13.7-5.6M20 12a8 8 0 0 1-13.7 5.6M17 4v3h-3M7 20v-3h3',
  filter: 'M4 6h16l-6 7v6l-4-2v-4z',
  // Density (1e)
  rows: 'M4 5h16M4 12h16M4 19h16',
  cells: 'M4 4h7v7H4zM13 4h7v7h-7zM4 13h7v7H4zM13 13h7v7h-7z',
  dense: 'M4 4h4v4H4zM10 4h4v4h-4zM16 4h4v4h-4zM4 10h4v4H4zM10 10h4v4h-4zM16 10h4v4h-4zM4 16h4v4H4zM10 16h4v4h-4zM16 16h4v4h-4z',
  expand: 'M4 9V4h5M20 15v5h-5M15 4h5v5M9 20H4v-5',      // 1h fullscreen
  undo: 'M9 5L4 10l5 5M4 10h10a6 6 0 0 1 0 12h-3',        // 1i
  info: 'M12 8h.01M11 12h1v5h1',                          // 1i, pair with a circle
  warn: 'M12 9v4M12 16.5v.01M10.3 4l-7 12A2 2 0 0 0 5 19h14a2 2 0 0 0 1.7-3l-7-12a2 2 0 0 0-3.4 0z',
  error: 'M12 8v5M12 16.5v.01',                            // 1k, pair with a circle
  spark: 'M12 3 3 8l9 5 9-5zM3 13l9 5 9-5',
};

/** Icons that need a circle drawn alongside their path. */
const CIRCLED = new Set(['info', 'error']);

export function icon(name, size = 16, strokeWidth = 1.8) {
  const d = ICONS[name];
  if (!d) throw new Error(`icons.js: unknown icon "${name}"`);
  const circle = CIRCLED.has(name) ? '<circle cx="12" cy="12" r="9"></circle>' : '';
  return `<svg width="${size}" height="${size}" viewBox="0 0 24 24" fill="none"
    stroke="currentColor" stroke-width="${strokeWidth}" stroke-linecap="round"
    stroke-linejoin="round" aria-hidden="true">${circle}<path d="${d}"></path></svg>`;
}
