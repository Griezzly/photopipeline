# Known issues and follow-ups

Findings that are real, reproduced, and deliberately **not** fixed in the change
that surfaced them. Each says why it was left, and what fixing it involves.

Last updated: 2026-08-13. The **Open** section below was filled by the automatic
RAW development work (`docs/superpowers/specs/2026-07-29-auto-develop-design.md`,
Phase 1). Everything the review-UI redesign left behind was fixed on
`fix/known-issues` and is recorded further down.

---

## Open

From Phase 1 of automatic RAW development (`feat/auto-develop`). All were
reproduced, none is a regression of existing behaviour, and each was left because
it sits outside that phase's scope. KI-11 and KI-12 were found later, during the
CHECKPOINT review on 2026-08-13.

### Affects photos, not just internals

**KI-11 — `photopipe scan --reprocess` is accepted and ignored.** The flag is
declared in the CLI, documented as "force re-analysis of already-processed
files", bound to `_reprocess` in `cmd_scan`, and then never read; nothing named
`reprocess` exists anywhere in the pipeline crate. A user asking for a forced
re-analysis silently gets an ordinary incremental scan. This is how the CHECKPOINT
review nearly drew the wrong conclusion: the sample library only picked up fresh
`sharpness` rows because a differently-spelled path created new `files` rows (see
KI-12), not because `--reprocess` did anything.

Fixing it is design work, not plumbing: "re-analysis" has to define what it
invalidates — preview cache, `sharpness`, `iqa`, `embeddings`, defect flags, or
all of them — and each choice has a different cost. Until then the flag should
arguably fail loudly rather than lie.

**KI-12 — the same photo is catalogued twice when the path is spelled
differently.** `library_key` is an xxh3 of the *canonicalised* folder path, so
`./example-pictures` and `/Users/…/example-pictures` open the same catalog, but
`files.path` stores the path as given. Scanning a library once by relative path
and once by absolute path therefore inserts every photo a second time, with an
identical `content_hash`, and the second copy carries none of the first's
decisions. Observed live: 6 rows for 3 files, keeper verdicts on 3 of them.

This breaks the "re-running `scan` does zero work" constraint whenever the
spelling changes, and it feeds dedupe a set of guaranteed-identical pairs.
Canonicalising the path at ingest is the obvious fix; a migration would need to
merge the duplicate rows rather than just delete them, since decisions can sit on
either copy.

**KI-3 — the next `scan` re-ingests finished JPEGs as new photos.** `_finished`
defaults to living inside the library and `jpg` is an ingest extension, and
`ingest_directory` has no exclusion for managed trees. `_review` has had this
problem all along, so it is not new, but `finish` makes it far easier to hit. The
`.photopipe-tree` marker that `output/mod.rs` already writes would give the
walker something cheap to skip on.

**KI-4 — output JPEGs carry no metadata.** `encode_jpeg` uses `JpegEncoder`
without `set_exif`/`set_icc_profile`, so finished files have no capture date,
camera, lens or ICC tag, and viewers sort them by mtime. The spec never promised
metadata, so this is a gap rather than a regression — but it is the kind of thing
noticed on the first real run.

### Internals

**KI-6 — `[develop] renderer` and the whole `[develop.look]` section are read by
nobody.** Only `finished_dir`, `jpeg_quality`, `output_subdirs` and
`rawtherapee_path` are consumed. `renderer = "vkdt"` silently renders through
RawTherapee; `look.enable = true` silently does nothing, because the look is
Phase 2. One validation line rejecting an unknown renderer, plus a `debug!` noting
the look is unimplemented, removes the trap.

**KI-7 — a long run is nearly silent, and the failure hint is wrong.**
`CliProgress` makes `set_total` and `inc` no-ops and nothing logs per rendered
photo, so a multi-hour serial run prints two lines. (`cmd_scan` passes `None`, so
this is convention rather than regression.) Separately, per-file failures are
logged at `warn!` while the default log level is `info`, so the summary's "re-run
with --log-level debug to see why individual files failed" is misleading — the
reasons already printed. `stage("rendering")` is also never emitted, so a future
UI wired to `ProgressSink` would show "measuring" for the whole render.

**KI-8 — unchecked indexing in the CFA path, and a panic escapes failure
isolation.** `measure.rs`'s 2×2 CFA cell walk indexes `data[(y + dy) * w + (x +
dx)]` with no bounds guard, while both sibling paths check `row_end > data.len()`
and break. If `rawler` ever returns a buffer shorter than `width * height`, this
panics — and `finish_folder` matches on `Err`, not unwinds, so one malformed file
would abort the whole run rather than being skipped. Low likelihood, cheap to
close with the guard the neighbours already use.

**KI-9 — `cargo test` registers libraries in the real user data directory.**
There are currently 74 catalog directories under
`~/Library/Application Support/photopipe/libraries/` whose folders are long-gone
`/T/.tmpXXXX/trip` temp paths. The CLI tests isolate their app-data dir
correctly; the pipeline integration tests do not, so they leave a registration
behind on every run. Harmless but it makes `photopipe libraries` useless on a
development machine.

**KI-10 — `raw_conn_for_test` is a sharp escape hatch.** It returns a
`MutexGuard` over the raw DuckDB connection and is `pub` behind only
`#[doc(hidden)]`, so nothing at compile time stops production code from taking
it and holding the lock. Integration tests live in a separate crate, so
`#[cfg(test)]` cannot work; a `test-support` cargo feature would.

---

## Fixed on 2026-08-13 (second pass)

**KI-2 — the finished tree is now pruned.** Flipping a verdict from keep to
reject left the JPEG, its `.pp3` and the `edits` row in place with no way to
clean up, so `_finished/` only ever grew.

The row was the more dangerous half. With it still naming a path no longer
claimed by any keeper, `dedupe_name` — which only knows about names handed out
during the *current* run — could give that same path to a different photo, whose
render would overwrite the file while the old row went on describing it. Deleting
the file and the row together is what makes that unreachable, so the prune does
both in one pass rather than sweeping disk and tidying the catalog separately.

`finish_folder` now collects every path the run vouches for (including skipped
photos, or the first no-op run would delete the whole tree), removes orphaned
outputs and their rows, then sweeps anything else left behind. `finish
--regenerate` rebuilds from scratch, matching `review-tree`.

The safety rule is deliberately narrow: pruning only touches a directory
photopipe can *show* it wrote — one carrying the `.photopipe-tree` marker, an
empty one, or one holding outputs the catalog already claims (which adopts a tree
written before `finish` set the marker). Anything else is left alone and logged.
`--out` accepts any path, and deleting files photopipe did not create would be
unforgivable; `an_unmarked_directory_full_of_strangers_is_never_pruned` pins it.

**KI-5 — a "zero work" second run no longer decodes every RAW.** `finish_one`
measured and upserted `raw_stats` *before* consulting `edit_identity`, because
`recipe_hash` is needed to build the identity and the recipe needs the stats.

The persisted `raw_stats` row closes that circle. It carries no `content_hash` of
its own so it cannot be trusted alone, but an `edits` row whose `content_hash`
still matches the file proves those stats were measured from exactly these bytes,
because `finish_one` writes both in the same pass. Every other case — no `edits`
row, a file that changed, a missing `raw_stats` row — still measures.

Measured on the three sample ARWs: a no-op run went from 1.94s to 0.10s. The
test asserts the absence of a decode rather than a duration, by pointing the
catalog at a RAW that does not exist: reusing the stats skips cleanly, while any
decode attempt fails and is counted.

**A third bug, surfaced while verifying the other two: `finish --out
somewhere-new` wrote nothing and called it success.** It reported every photo as
"already current" and left the new directory empty, because `is_up_to_date`
checks the path the *previous* run recorded rather than the one being asked for.
An output that is not where we were told to write it is not current; the check
now compares the two.

## Fixed on 2026-08-13

**Sharpening was pinned to the cap for every photo.** Found during the CHECKPOINT
review, which is exactly the review question it was blocking.

`decide()` documented its input as "roughly 0..1" and clamped it there, but
`defect::blur` computes `s_global` as the variance of the Laplacian — unbounded,
and 128 / 357 / 1491 on the three sample ARWs. Every real frame saturated the
clamp to 1.0, so `sharpen_amount` was always exactly `SHARPEN_MAX`. The safety
property the comment claimed — "a genuinely soft frame is never sharpened into
crunch" — was not in force, and a visibly out-of-focus frame was getting maximum
sharpening. All 23 `decide` tests passed throughout, because every one of them
feeds `sharp(0.5)`, `sharp(0.05)` or `sharp(0.95)`: fabricated values in a range
no photograph produces.

This is the second instance of the defect class recorded at the bottom of this
file, and the sharper lesson is the one about **units crossing a module
boundary**. Both sides were individually reasonable — an unbounded variance is
the right blur metric, and a 0..1 knob is the right recipe field — and the bug
lived entirely in the undocumented assumption between them, held in place by a
doc comment that asserted the contract instead of checking it.

Fixed by renaming the field to `s_relative` and computing it with
`relative_sharpness()` against the `sharpness_baseline` percentiles the blur
flagger already uses, falling back per-bucket → global sentinel → neutral. Note
the fix only bites once `photopipe calibrate` has run: `scan` deliberately does
not build baselines (spec §7 wants calibration run over a few hundred photos per
lens), so an uncalibrated library gets neutral sharpening by design. Verified on
the sample set: 0.800 → 0.000 for the out-of-focus frame, 0.407 for the mid one,
0.800 held for the sharpest, and `finish` still reports 0 rendered on a re-run.

## Fixed on 2026-08-11

**KI-1 — no preview could be extracted from Sony A6300 (ILCE-6300) ARW files.**

`extract_preview_raw` asked `rawler` for `preview_image()` then
`thumbnail_image()`; both answer `None` for these files, so ingest logged "no
preview or thumbnail available", wrote nothing to the preview cache, and still
reported the file as `Processed`, `Errored: 0`. Everything downstream silently
did nothing for this camera — review-UI thumbnails, blur and back-focus
detection, exposure flags, CLIP-IQA, DINOv2 embeddings and therefore duplicate
detection. EXIF worked, so the catalog looked populated at a glance while
`sharpness`, `exposure` and `iqa` were null. It also left `decide()` falling back
to `s_global = 0.5`, which is why the Phase 1 CHECKPOINT could not judge
sharpening.

The cause turned out to be narrower than the original note assumed, and no
byte-scanner was needed. `rawler` 0.7.2 wires each decoder's embedded JPEG to
whichever of three trait methods its author picked, inconsistently between
formats: the ARW decoder implements only `full_image`, whose own doc comment
reads "return the embedded JPEG preview". Despite the name, no decoder's
`full_image` develops the sensor plane — each reads an already-encoded image out
of a tag (embedded JPEG, or an uncompressed RGB strip for CR2), and the trait
default returns `None`. So `full_image` is now the third step of the fallback
chain, after `preview_image` and `thumbnail_image`; ordering it last keeps every
format that already had a preview on exactly the path it was on, and the sources
stay lazily evaluated so nothing decodes a second image it will not use. The
chain also no longer aborts on the first erroring step — a decoder that fails one
entry point can still return a usable image from another — and the final error
message names all three.

Covered by `arw_preview_falls_back_to_the_embedded_jpeg` in
`crates/pipeline/src/ingest/preview.rs`, which pins the aspect ratio so the
1616×1080 preview cannot be confused with the 160×120 thumbnail, and skips
cleanly where `example-pictures/` is absent. Verified live on the three sample
ARWs: `photopipe scan` cached 3 previews where it previously cached none, defect
analysis went from silently no-op to `Analyzed: 3` with one overexposure flag,
and `sharpness.s_global` holds three distinct real values (357, 128, 1491) where
the column was empty.

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

## Two defect classes from the auto-develop work

**A formula that passes every unit test can be systematically wrong on real
photos.** The decision layer was fully covered by table-driven tests over
synthetic numbers, all green, and it still made photos worse: `MID_GREY = 0.18`
is a grey-card reflectance target, and the median of a scene full of dark
conifers is legitimately far below it, so pulling the median to 0.18
over-brightened most outdoor frames. The same error appeared a second time in
`shadow_lift`, driven by a low 1st percentile that meant "this scene has deep
shadows", not "this scene is broken". Neither was visible in a test; both were
obvious the moment three real frames were rendered next to baseline-only renders.
Rendering real photos and comparing against a known-neutral baseline is the only
thing that found them, and it is worth doing before believing any tuning change.

**Amending a migration in place is unsafe as soon as any catalog has run it —
"unreleased" is not the relevant question.** Migration v4 was edited twice on the
reasoning that it had not shipped. Catalogs created at v4 *before* those edits
already existed on disk, and because `schema_version` already read `4`, no
migration re-ran: `finish` then failed on every photo with `Table "raw_stats"
does not have a column with name "p99"`, with a NOT NULL violation on `edits`
queued behind it. No unit test could catch this, because every test builds a
fresh catalog where the amended and the migrated schema are identical. The fix
was to restore v4 verbatim and add v5. Migrations are append-only from the moment
one has been executed anywhere.

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
