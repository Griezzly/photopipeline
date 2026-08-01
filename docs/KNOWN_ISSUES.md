# Known issues and follow-ups

Findings that are real, reproduced, and deliberately **not** fixed in the change
that surfaced them. Each says why it was left, and what fixing it involves.

Last updated: 2026-08-01. **Nothing is currently open.** Everything the
review-UI redesign
(`docs/superpowers/specs/2026-07-30-review-ui-redesign-design.md`) left behind
was fixed on `fix/known-issues`; what remains below is the accepted risk and the
defect class worth not re-learning.

---

## Open

None.

---

## Fixed on 2026-08-01

Each was reproduced before the fix and re-verified after, against the real
`2024-05 Grindelwald` library rather than only in tests.

### Backend

**BE-1 — `count_pending` counted excluded sidecar JPGs** (`bf4438f`)

`analyze.rs::count_pending` walked by extension and asked `needs_processing()`,
but never applied `exclude_sidecar_jpgs()`, which was private to
`ingest/mod.rs`. Every sidecar JPG that ingest deliberately skips reported as
pending forever, so the UI's "N new photos in this folder" banner fired on every
library, every time.

Fixed by making the exclusion public and applying it in `count_pending`, so both
walks answer the same question; the logging moved to the ingest call site, where
"excluded from catalog" is actually true. Covered by
`count_pending_ignores_sidecar_jpgs` in `crates/pipeline/tests/analyze.rs`.
Verified live: Grindelwald has 94 RAWs and 94 sidecars, and `POST /api/open` now
returns `pending_new: 0` where it used to return 94.

**BE-2 — `set_decision` never reset `is_keeper`** (`e02a5df`)

The upsert wrote `is_keeper = false` only on a fresh insert; the
`ON CONFLICT DO UPDATE SET` list omitted the column. Picking a keeper and then
keeping or rejecting that photo left `is_keeper = true`, and since the UI ranks
keeper above every other state, the next refetch painted a **rejected** photo as
its group's keeper — permanently.

Fixed by adding `is_keeper = excluded.is_keeper` to the `SET` list; only
`pick_keeper` sets the flag now. Covered by
`set_decision_clears_a_previously_picked_keeper` in
`crates/pipeline/tests/decisions.rs`, which asserts through `review_list` as well
as `get_decision`, because `review_list` is what the client actually refetches.

**BE-3 — one library's catalog had a corrupted DuckDB ART index**

Every decision write to Grindelwald returned 500 with
`FATAL Error: Corrupted ART index - likely the same row id was inserted twice
into the same ART`. Confirmed still failing on 2026-08-01, then fixed by deleting
that library's `catalog.duckdb` and re-scanning: 94 processed, 0 errored, and a
second scan processed 0 — idempotent. Decision writes now return 200.

No code change, and none was warranted: the identical write always succeeded
against other libraries, so this was damage to one derived-data file. The library
held zero decisions, so nothing but derived data was lost. Origin remains
unknown; the likeliest candidate is an interrupted write during early testing.

### UI (`26773a5`)

| Item | Where | Fix |
|---|---|---|
| Test-comment inaccuracy | `crates/cli/tests/serve.rs` | The font is referenced from `tokens.css`, not `index.html`, so the index-manifest test never covered it. The comment now says so and names `font_is_served_with_font_content_type`, which does. |
| Guard test could go vacuous | `crates/cli/tests/serve.rs` | The cross-library guard's strongest inner check was gated on `src.contains("let loading")`; renaming that variable would have disabled it silently. The set of modules the gate fires on is now pinned — verified by renaming the variable and watching the test go red. |
| `norm()` lowercased whole paths | `crates/cli/assets/picker.js` | Library identity is `library_key`, an xxh3 of the canonical path with its case intact, so on a case-sensitive filesystem the two casings really are two libraries. `norm()` now strips trailing separators only, and fails toward a missing badge rather than a false one. |
| `--shadow-float` was not themed | `crates/cli/assets/tokens.css` | Moved from the shared `:root` into the two theme blocks. Light gets a soft `rgba(3,7,18,…)` lift instead of the dark-tuned `rgba(0,0,0,.55)` bruise. |
| Determinate progress bar lacked ARIA | `crates/cli/assets/analyze.js` | Both branches now expose `role="progressbar"`, with `aria-valuenow` where there is a value to report. |
| Mockup-fidelity nits | `crates/cli/assets/style.css` | Taken from the mockups' own values: `.stat` wash 9% → 3.5%, `.btn.on` text to a new `--accent-soft-fg` (`#007A8A` / `#51D7ED`), `.decide-bar` track to a new `--track` (`#F1F0F0` / `#1A1C1C`). |

---

## Accepted, not a defect

Folder names and filenames from the filesystem are interpolated into `innerHTML`
in several screens. Judged acceptable for a localhost-only tool that displays the
user their own directory contents: an attacker able to name local directories
already has code execution as that user. Toast titles and bodies *are* escaped at
the sink, because filenames are a wider surface than the user's own folder names.

---

## A recurring defect class worth remembering

The redesign shipped the same bug **five times**: module-level state surviving a
library switch.

Two facts make it easy to write and hard to see:

- `group_id` and `file_id` both restart at 1 in **every** library — each library
  has its own `catalog.duckdb` with its own sequences.
- Switching libraries is a pure SPA transition with **no page reload**, so every
  module-level variable persists across it.

The consequences were not cosmetic. One instance let "Accept all suggestions"
act on the previous library's IDs and wrote 25 decisions onto the wrong
catalog. Another let a keypress during a load window post the previous library's
`file_id` against the new catalog.

Every instance shared a shape: **serial testing cannot find them.** Load A fully,
load B fully, then interact, and everything passes. They only appear when you act
*during* the switch or while a fetch is in flight.

`docs/ui-race-checklist.md` is the standing countermeasure — run it before
merging UI changes. When adding a module that holds state or registers a keydown
handler, clear its data in the folder-change block, guard every post-`await`
continuation against the folder having changed, and stand down while loading.
`crates/cli/tests/serve.rs` asserts the guards exist, and — since 2026-08-01 —
asserts which modules they cover, so a rename cannot quietly retire one.
