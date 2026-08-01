# Known issues and follow-ups

Findings that are real, reproduced, and deliberately **not** fixed in the change
that surfaced them. Each says why it was left, and what fixing it involves.

Last updated: 2026-07-31, after the review-UI redesign
(`docs/superpowers/specs/2026-07-30-review-ui-redesign-design.md`).

---

## Backend bugs

These were found while building the new web UI. All three were verified
directly, not inferred. None were fixed there, because that work was scoped to
the front end — its only production Rust change was two `Content-Type` mime arms.

### BE-1 — `count_pending` counts excluded sidecar JPGs, so the re-analyze banner never clears

`crates/pipeline/src/analyze.rs`, `count_pending()`.

It walks the folder, filters by extension, and asks `needs_processing()` — but
never applies `exclude_sidecar_jpgs()`, which is private to
`crates/pipeline/src/ingest/mod.rs` and only called from the ingest walk. Every
sidecar JPG that ingest deliberately excluded therefore reports as pending
forever.

Evidence: Kijkduin has 150 files on disk, exactly 75 of them JPGs, and 75
cataloged photos. `POST /api/open` returns `pending_new: 75` — precisely the
excluded sidecars. Reproduced on all five test libraries.

Effect: the UI's "N new photos in this folder" banner fires on every library,
every time. The banner reports the API faithfully; the API is wrong.

Fix: make the sidecar-exclusion rule reachable from `analyze.rs` (export it, or
lift it into a shared helper) and apply it in `count_pending`. Add a test with a
RAW+JPG pair asserting `count_pending == 0` after a scan.

### BE-2 — `set_decision` never resets `is_keeper`, so a rejected photo can render as a keeper

`crates/pipeline/src/catalog/mod.rs`, `Catalog::set_decision()`.

```sql
INSERT INTO decisions (file_id, verdict, is_keeper, note, decided_at)
VALUES (?, ?, false, ?, ?)
ON CONFLICT (file_id) DO UPDATE SET
    verdict = excluded.verdict, note = excluded.note, decided_at = excluded.decided_at
```

The `false` applies only on a fresh insert. The `DO UPDATE SET` list omits
`is_keeper`, so marking a photo keeper and then keeping or rejecting it leaves
`is_keeper = true` in the database.

Effect: after any reload, `review_list` returns `is_keeper: true` and the UI —
where keeper outranks every other state — paints a **rejected** photo as a
keeper, permanently. The client already does the right thing locally; the
divergence only appears after a refetch.

No test covers keeper-then-decide; `crates/pipeline/tests/decisions.rs` stops at
`pick_keeper`.

Fix: add `is_keeper = false` to that `SET` list, plus a regression test that
sets a keeper, then rejects, then re-reads.

### BE-3 — one library's catalog has a corrupted DuckDB ART index

Every decision write to the Grindelwald library returns 500:

```
FATAL Error: Corrupted ART index - likely the same row id was inserted twice into the same ART
```

Reproduced on two different `file_id`s after a clean server restart. Reads
(counts, photos, thumbnails) are unaffected and no partial write lands.

**Localized, not systemic** — the identical write to another library succeeds and
reverts cleanly, so this is damage to one derived-data file rather than a code
path. Origin unknown; the likeliest candidate is an interrupted write during
early testing.

Fix: delete that library's directory under
`~/.local/share/photopipe/libraries/<key>/` and re-scan. Safe by this project's
own constraint — catalogs are derived data and originals are never modified. Not
done automatically because it is a deletion.

---

## UI follow-ups

Non-blocking; triaged as ship-as-is during the redesign's final review.

| Item | Where | Note |
|---|---|---|
| Test-comment inaccuracy | `crates/cli/tests/serve.rs` | A doc comment claims the font is covered by the index-manifest test. It is not — `Manrope.ttf` is referenced from `tokens.css`, not `index.html`. Coverage is intact via `font_is_served_with_font_content_type`; only the comment is wrong. |
| Guard test can go vacuous on a rename | `crates/cli/tests/serve.rs` | The cross-library guard assertion's strongest inner check is gated on `src.contains("let loading")`. Renaming that variable silently disables the check while the test still passes. One-line fix: also assert the set of modules matching `let loading`. |
| `norm()` lowercases whole paths | `crates/cli/assets/picker.js` | Can show "Analyzed" on `/photos/Shoot` when the library is `/photos/shoot` — a false positive on a badge whose whole point is truthfulness. Only affects case-sensitive filesystems. |
| `--shadow-float` is not themed | `crates/cli/assets/tokens.css` | Defined once in the shared `:root` as a dark-tuned drop shadow, then used by `.sheet` and `.dup-pop`, so it reads heavy over the light theme's pale wells. Move it into the two theme blocks. |
| Determinate progress bar lacks ARIA | `crates/cli/assets/analyze.js` | The indeterminate branch has a proper `role="progressbar"` on its spinner; the determinate bar does not. |
| Mockup-fidelity nits | `crates/cli/assets/style.css` | `.stat` radial wash strength, `.btn.on` text colour, `.decide-bar` track shade all differ slightly from the mockups. Sub-perceptual. |

### Accepted, not a defect

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
`crates/cli/tests/serve.rs` asserts the guards exist.
