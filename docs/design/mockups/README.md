# UI mockups — read-only design reference

Source: Claude Design project **"UI mockups for image review"**
`https://claude.ai/design/p/75034e4f-bfb3-402c-a7a8-c8e03b56dace`

Imported 2026-07-30 via the `claude_design` MCP.

| File | What it is |
|---|---|
| `Photopipe.dc.html` | All eleven screens (1a–1k) plus the assumptions / design-system notes card |
| `Rail.dc.html` | The 56px navigation rail component |
| `support.js` | Generated `dc-runtime` that renders the two files above. Not used by `photopipe`. |

## Do not edit these files

They are a snapshot of the design, kept so implementation tasks can cite exact
style values and line ranges. The implementation lives in `crates/cli/assets/`.
If the design changes upstream, re-import rather than hand-editing.

## Screen index

`Photopipe.dc.html` line ranges, for citation from plans and reviews:

| Screen | Lines | What |
|---|---|---|
| notes | 23–61 | Assumptions, decision semantics, flag rules, nav-shell note |
| 1a | 67–131 | Libraries home + re-analyze banner |
| 1b | 133–178 | Folder picker modal |
| 1c | 180–226 | Analyzing, counted stage active |
| 1d | 228–260 | Analyzing, uncountable stage (indeterminate) |
| 1e | 262–380 | Review grid, dark, shortcut sheet open |
| 1f | 382–459 | Review grid, light, defect filter menu open |
| 1g | 461–508 | Grid states: empty, filtered-empty, complete, loading |
| 1h | 510–604 | Photo detail: zoom, decision bar, metadata panel |
| 1i | 606–686 | Duplicates review: confirming cluster + decided cluster |
| 1j | 688–733 | Compare mode, two frames, synced zoom |
| 1k | 735–850 | Export dialog, toasts, banners, tile anatomy |

The `<script type="text/x-dc">` block at lines 855–1096 holds the mock data and
the shared `tileStyles()` / `stage()` helpers — the authority for tile outline
colours, decision marks, the 42% reject dim, and the stage-dot styling.

## How to view them

The `.dc.html` files render standalone in a browser (they load `support.js`
relative), but they were authored for the Claude Design canvas. Reading the
source is usually more useful than rendering it.
