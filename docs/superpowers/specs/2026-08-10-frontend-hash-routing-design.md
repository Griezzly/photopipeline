# Frontend hash routing — design

Date: 2026-08-10
Status: approved, ready for implementation planning

## Problem

The review UI (`crates/cli/assets/`) is a single-page app that never touches
`history`. `show(view)` toggles four `.view` divs; overlays (photo detail,
compare, export) mount into `#modal-host` and are dismissed with Escape or a
close button. The URL is always `http://127.0.0.1:<port>/`.

Consequences:

- The browser's Back button leaves the application entirely. There is no way
  back into a screen you just left.
- Escape is the only exit from the photo detail and compare overlays. Users who
  reach for Back get thrown out of the app instead.
- Reloading always lands on whatever `boot()` decides (review if a library is
  active, otherwise libraries), regardless of where you were.
- No screen can be linked to or bookmarked.

## Goals

Give the browser's Back button a meaning inside the app: it should step back
through screens and close overlays, never leave the page. Reload should return
you to the screen you were on.

## Non-goals

- Review filters, sort mode, and grid cursor stay out of the URL.
- No clean-path (`pushState`) URLs, and therefore no server changes.
- The active library is not encoded in the URL.

## Decisions

Four decisions were settled during brainstorming:

1. **Scope: screens plus overlays.** The four screens (libraries, analyze,
   review, duplicates) and the three overlay layers (photo detail, compare,
   export) get routes. Filters, sort, and cursor do not.
2. **Hash routing**, not `pushState` paths. The server's `/:file` static-asset
   route would otherwise swallow `/review`, and a hash can never 404 on reload.
   This is a localhost single-user tool; pretty URLs buy nothing here.
3. **The active library is not in the URL.** `#/review` means "review the
   server's active library". The router resolves it via `GET /api/active`.
4. **Detail stepping pushes.** Arrowing photo-to-photo inside the detail
   overlay pushes a history entry per photo, so Back retraces the frames you
   looked at. Escape remains the one-press exit from a long cull run.

## Architecture

A new module `crates/cli/assets/router.js`, imported first in `app.js`'s
`Promise.all` block. It owns the hash and installs a single `hashchange`
listener.

**The URL is the source of truth.** All navigation goes through
`window.pp.go(path, opts)` / `window.pp.replace(path, opts)`, which set
`location.hash`. The `hashchange` handler resolves the path and calls the
existing `open*` functions. Back, forward, reload, and a hand-typed URL
therefore all take the same code path — there is one behaviour to get right
rather than two that can drift.

The rejected alternative was leaving call sites alone and having each `open*`
function report its route to the router after the fact. That needs a second,
separate path→function mapping for restores; two mappings will drift.

### Route table

| Hash | Applies |
|---|---|
| `#/libraries` | `openLibraries()` |
| `#/analyze` | `startAnalyze()` — pushed on entry, replaced on exit; see A2 |
| `#/review` | `openReview(state.activeFolder)` |
| `#/review/photo/:id` | `openReview` (if not already there) + `openDetail(indexOf(id))` |
| `#/duplicates` | `openDuplicates(state.activeFolder)` |
| `#/duplicates/compare/:groupId` | `openDuplicates` + `openCompare(groupId)` |
| `#/review/compare/:groupId` | `openReview` + `openCompare(groupId)` |
| `#/review/photo/:photoId/compare/:groupId` | `openReview` + `openDetail` + `openCompare(groupId)` |
| `#/export` | parent screen + `openExport()` modal |
| empty or unknown | `replace()` to `#/review` if a library is active, else `#/libraries` |

`:id` is a `file_id`; `:groupId` is a duplicate cluster's `group_id`. Both are
integers scoped to the active library's catalog.

"Parent screen" for `#/export` means: the screen the router was on when
`#/export` was entered, if that was `review` or `duplicates`; otherwise
`review`. Export is reachable from the review topbar and from the rail, so both
cases occur.

### Library guard

Routes that need a library (`review`, `duplicates`, `export`, and their
children) run a guard before applying: if `state.activeFolder` is unset, `GET
/api/active`; if that is also empty, `replace('#/libraries')`. This is the logic
`boot()` performs today, moved into the router. `boot()` shrinks to "resolve the
initial hash".

### Transient arguments

Some `open*` functions take arguments that cannot be reconstructed from a URL:
`openReview`'s `opts.pendingNew` (drives a "new photos in this folder" banner)
and `openCompare`'s `fileIds` (which two of a cluster's members to show).

`go()` accepts an optional one-shot payload that the router hands to the target
and then discards. It is never written to the URL. On a Back/forward restore or
a reload the payload is absent and the target falls back to its documented
default: compare shows the group's first two members, review shows no banner.
The URL never promises to carry state it cannot reconstruct.

### Overlay close

`closeDetail()`, `closeCompare()`, and the export modal's dismiss currently
unmount the overlay directly. They become `history.back()` **when the current
hash is that overlay's own route**; the unmount itself moves into the
`hashchange` handler. Escape and Back then perform literally the same
operation.

Guard: if the overlay was entered by deep link, there is no parent entry in this
session's history and `back()` would leave the app. The router tracks whether it
has navigated at least once within this page load; if not, close `replace()`s to
the parent route instead of calling `back()`.

### Analyze

Entering `#/analyze` pushes a history entry; leaving it — job done, "Review N
so far", or a failed start — **replaces** that entry rather than stacking a
third. So `#/libraries` → `#/analyze` → `#/review` leaves two entries and Back
from review lands on libraries rather than re-entering a finished job screen.
A reload on `#/analyze` resolves via `GET /api/analyze/status`: a running job
shows progress; anything else redirects per the empty-route rule. See
amendment A2 — an earlier draft of this section claimed the opposite split.

## Error handling

- `openExport()` and `openCompare()` can bail early with a toast — nothing new
  to export, or a `group_id` not present in this library. They return a falsy
  result and the router `replace()`s back to the parent route, so the URL never
  sits on an overlay that is not on screen.
- `#/review/photo/:id` where the `file_id` is not in the loaded list (stale
  link, or the active filters exclude it): toast plus `replace('#/review')`.
- A malformed `:id` / `:groupId` (non-integer) is treated as an unknown route.
- Switching libraries from the libraries screen keeps its existing per-screen
  data reset; the router only adds the `#/review` push.
- An overlay entry's `ppFolder` naming a library other than the one now
  active (a stale link, or a Back/forward landing after a library switch):
  toast plus a redirect to the route's parent screen. See amendment A3 —
  `file_id`/`group_id` restart at 1 in every catalog, so a stale id is not
  merely missing, it can land on a real, unrelated row.

## Testing

The router is entirely client-side, so the Rust test suite can only assert
delivery: extend `crates/cli/tests/serve.rs` to check that `/router.js` is
served with a JavaScript content type and that `app.js` imports it.

Behavioural coverage is manual, against a real library:

1. Back and forward across all four screens.
2. Escape and Back are equivalent in the photo detail and compare overlays.
3. Arrowing through detail pushes one entry per photo; Back retraces them.
4. Reload on each route restores that route.
5. Deep link to `#/review` with no active library redirects to `#/libraries`.
6. Deep link to a stale `#/review/photo/:id` toasts and falls back to
   `#/review`.
7. Export with nothing to export leaves the URL on the parent route.

The project has no headless-browser dependency and this design does not add
one. If automated coverage of the above is wanted later, that is a separate
decision.

## Files touched

- `crates/cli/assets/router.js` — new
- `crates/cli/assets/app.js` — import the router, shrink `boot()`
- `crates/cli/assets/rail.js` — rail items call `go()`
- `crates/cli/assets/libraries.js`, `analyze.js`, `review.js`,
  `duplicates.js`, `detail.js`, `compare.js`, `export.js` — navigation call
  sites go through `go()`/`replace()`; overlay closers delegate to history
- `crates/cli/assets/picker.js` — its one `startAnalyze` call site becomes a
  `window.pp.go('/analyze', { folder })`
- `crates/cli/tests/serve.rs` — asset delivery assertion

No Rust changes beyond the test. The hash never reaches the server.

## Amendments

Recorded 2026-08-11, while writing the implementation plan.

### A1 — Compare has three parents, not one

The original route table gave compare a single route under `/duplicates`.
`openCompare` is in fact reached from three places — the duplicates list
(`duplicates.js:277`, `duplicates.js:544`), the review grid via `c`
(`review.js:778`), and the photo detail overlay via `c` (`detail.js:224`) —
and `compare.js:266-268` already branches on which one it was, returning to
the duplicates list only when that is where it came from. A single
`/duplicates`-parented route would silently change that. Compare therefore
gets one route per parent, listed in the route table above. `parentPath()`
in `router.js` is the single place that mapping lives.

### A2 — Analyze pushes on entry, replaces on exit

The "Analyze" section said `#/analyze` is "`replace`d rather than pushed",
but the worked example in the same paragraph — `#/libraries` → `#/analyze` →
`#/review` leaving two entries, with Back from review landing on libraries —
requires the opposite split: **entering** analyze pushes a history entry,
and **leaving** it (job done, "Review N so far", or a start failure)
replaces that entry rather than stacking a third. The worked example is
correct and is what gets implemented; the sentence before it was wrong.

### A3 — Overlay close, cross-library ids, `setPath`, and history rate limits

The "Overlay close" section said `closeDetail()`, `closeCompare()`, and the
export modal's dismiss "become `history.back()`". What got built is
materially larger, for reasons that only showed up once stepping through a
real cull run was tried against the design.

**Whole-run overlay exit.** Detail stepping (decision 4) pushes one history
entry per photo. A single `history.back()` is an exit only for an overlay
that is exactly one entry deep — true for compare and export, false for
detail after any run longer than one frame, where it would take one Back
press per frame just to leave. Every overlay entry therefore stamps
`ppBase`: the depth of the screen entry the run started from. `exitOverlay()`
reads `ppBase` off the current entry and leaves the whole run in a single
`history.go(ppBase - depth())`, falling back to `replace()` when the entry
was reached by deep link and has no run behind it. `back()` is unchanged and
remains correct for compare and export. This was the human partner's ruling,
taken after a review showed the original design made Escape useless — worse
than doing nothing — after any cull run longer than one photo.

**`ppFolder`, a cross-library id guard.** `file_id` and `group_id` both come
from per-catalog `SEQUENCE`s (schema.rs), so every library numbers its files
and groups from 1. A stale id left over from a previous library — a
Back/forward restore, or a deep link — is not merely absent from the new
catalog; it is very likely to name a real, unrelated row in it, which is a
worse failure than a 404 would be. Every overlay entry therefore stamps
`ppFolder`, the library it was created under, and the photo and compare
appliers in `router.js` reject an entry stamped for a different library with
a toast and a `replace()` to the route's parent screen, before restoring
anything underneath. Added to `## Error handling` above.

**`setPath`.** A navigation verb, `window.pp.setPath(path)`, that rewrites
`location.hash` and its `history.state` without re-running the path's
applier. Needed because some screens can already be showing the exact state
a route names — detail.js's `detailRefresh()` re-anchors on the frame a
decision left on screen and only wants the URL to agree with it, not to
refetch or reopen anything `go()`/`replace()` would trigger.

**History-write rate limiting.** Browsers cap how many history entries a page
may write — Safari throws past roughly 100 `pushState` calls in 30 seconds —
and a held arrow key or Space in the detail overlay reaches that in seconds.
`router.js`'s `writeEntry()` catches the throw and degrades to rewriting the
current entry instead of dropping the navigation; the cost is a missing
history entry, not a stuck app. Separately, `move()` in detail.js coalesces
an auto-repeating key (`event.repeat`) into a single `replace()` rather than
one `go()` per frame, so a held key is treated as one gesture rather than as
many deliberate visits — which also keeps ordinary browsing well under the
limit that degradation exists to catch.
