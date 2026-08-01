# UI race checklist — run once before merging a review-UI branch

Three windows that per-task, per-module review structurally cannot cover: they
only exist between a synchronous repaint and an awaited fetch, and a serial
test (load library A fully, then load library B fully, then interact) never
enters them. Every cross-library state defect on the redesign branch — five of
them — lived in one of these three windows.

Use two libraries whose `group_id` and `file_id` sequences collide, which they
always do: every catalog numbers both from 1. Two libraries of clearly
different subject matter make a leak obvious at a glance rather than requiring
you to check ids.

**Revert every decision afterwards.** Both libraries must end at `kept: 0,
rejected: 0`, zero keepers.

## Forcing the window

The window is a network round trip, so widen it. In DevTools, before the click:

```js
// Hold /api/photos for 8s so the skeleton stays up long enough to type into.
const real = window.fetch;
window.fetch = (u, o) => (String(u).includes('/api/photos?')
  ? new Promise(r => setTimeout(() => r(real(u, o)), 8000))
  : real(u, o));
```

Throttling the whole profile to "Slow 3G" also works but is less precise.
Restore with `window.fetch = real` when done. Keep the Network panel open and
filtered to `decisions` for the whole run.

## (a) Review grid — library switch during the skeleton

1. Open library A. Let it settle.
2. Install the fetch delay above.
3. Rail → **Libraries** → click library B.
4. While the tile skeleton is on screen, press `Space`, then `x`, then `f`.

**Expect:** no `POST /api/decisions` in the Network panel, and no detail
overlay from `f`. When the grid paints, the header counts and the grid are
B's.

**Failure looks like:** a 200 `POST /api/decisions` whose `file_id` is one of
A's, applied to whichever B photo happens to hold that id — and for `f`, a
detail overlay showing A's filename and metadata beside a `/preview/<id>` that
renders B's photo.

Afterwards, check `GET /api/counts` on both libraries: both must still be
`kept: 0, rejected: 0`.

## (b) Duplicates — the same switch

1. Open library A → rail → **Duplicates**. Let it settle.
2. Install the fetch delay (widen it to `/api/clusters` as well).
3. Switch to library B and go to Duplicates.
4. While the cluster skeleton is up, press `c`, then `u`, and click
   **Accept all suggestions** if it is enabled.

**Expect:** no decision POSTs, and **Accept all suggestions** either absent or
inert while loading. A live, clickable Accept-all built from A's clusters is
the exact failure this checks for.

## (c) Compare opened from detail — the panel underneath

No delay needed; this one is about ordering, not timing.

1. Open any library, put the cursor on a photo that is in a duplicate group.
2. Press `f` to open the detail panel, then `c` to open compare on top of it.
3. Press `a` (or click a **★ Keeper** button) to set a keeper.

**Expect:** compare closes and the detail panel underneath now reads
**Keeper** (or **Reject**, if the frame you were on was a sibling) — not
"Undecided". Arrowing left/right from there must move to the neighbouring
photo, not jump.

**Failure looks like:** the panel still saying "Undecided" for a frame the
grid behind it already shows as a keeper.

## Cleanup

- Undo every decision: `u` on each affected photo, or **Undo** on the cluster.
- Confirm `GET /api/counts` is `kept: 0, rejected: 0` for every library used.
- If you exercised export, run the server from a scratch directory so
  `_keepers/` cannot land in the repo, and delete it.
- Kill the server.
