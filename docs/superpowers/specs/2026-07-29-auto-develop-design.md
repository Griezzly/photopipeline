# Automatic RAW development (`photopipe finish`) — Design Spec

**Date:** 2026-07-29
**Revised:** 2026-08-09 — see §0
**Status:** Implemented (2026-08-13). Phase 1 baseline, the CHECKPOINT (A11),
and Phase 2's look all landed; see §13 for what each open item turned out to be.
**Scope:** Step 3 of the pipeline (*edit*). Turn curated keepers into finished
JPEGs automatically, with no per-photo human input. Adds a `finish` command, a
`raw_stats` + `edits` schema pair, an analytic decision layer, a RawTherapee
render backend, and an ONNX look model. Curation itself is unchanged.

## 0. Revision 2026-08-09

The original draft predates the review UI redesign. This revision reconciles the
spec with what shipped since and resolves two assumptions that turned out to
need a decision. Nothing in the technical core — the renderer choice, the schema,
the decision layer, the look model — changed.

| # | Amendment | Where |
|---|---|---|
| A1 | Both stages go in **one** implementation plan, with a mandatory sign-off checkpoint between them | §13 |
| A2 | v1 is **CLI-only**, but reports through the existing `ProgressSink` so a UI screen is later a wiring job | §8, §13 |
| A3 | `finish` and `export-keepers` are **independent** commands over independent trees | §10 |
| A4 | `rawtherapee-cli` is **not installed** — the plan opens with a Phase 0 that installs it and resolves the `.pp3` key research | §13 |
| A5 | The IQA guard and the ONNX exporter are **reuse**, not new work | §7 |
| A6 | Schema **v4 confirmed available** — the catalog is at v3 | §5 |
| A7 | The **FiveK look stands** for v1; no own-look corpus exists | §7, §12 |
| A8 | **Input set corrected** from `is_keeper = true` to `verdict = 'keep'` | §2, §4 |
| A9 | **Exposure headroom comes from `p99`, not `p999`** — a new `raw_stats` column | §5, §6 |
| A10 | **The white-balance override is removed.** v1 emits RawTherapee's `Setting=Camera`; `EditRecipe` and `edits` carry no WB fields | §5, §6 |

**A8 — the input set was wrong.** The original draft named
`decisions.is_keeper = true`. In the shipped schema `is_keeper` is written only by
`Catalog::pick_keeper()` and means *best shot of a duplicate group*; an ordinary
keep through `Catalog::set_decision()` writes `is_keeper = false` explicitly. Using
it would have developed only duplicate-group winners and silently skipped every
photo kept outside a group. The correct predicate is `verdict = 'keep'`, which is
what `Catalog::keeper_files()` and `export-keepers` already use, and what "the
photos I kept" means to the user.

**A9 — headroom must come from `p99`.** The formula below originally read
`headroom = log2(0.95 / p999)`. The 99.9th percentile saturates to 1.0 as soon as
more than 0.1% of pixels clip — true of almost any frame containing sky, a
specular highlight, or a light source — after which it reports zero headroom no
matter how dark the image actually is, and the exposure lift is silently
discarded. Measured on a real Sony ARW: `p50 = 0.0587` (median 1.62 stops below
middle grey), `p999 = 1.0`, `clipped_frac = 2.1%`; the `p999` form emitted
**−0.07 EV** where **+1.62 EV** was wanted. `p99` stays meaningful until 1% of
pixels clip, and pixels that already clipped are unrecoverable anyway, so
protecting them costs the rest of the image. A `p99` column was added to
`raw_stats` and there is a regression test, `saturated_p999_does_not_suppress_the_lift`,
built from those measured numbers.

**A10 — the white-balance override is removed.** The design below started from
the as-shot coefficients and converted them into RawTherapee's
`Temperature`/`Green` parameterisation. That conversion was wrong in two
independent ways. The temperature relation was **inverted**: `5000·(b/r)^0.85`
maps warm light to a high kelvin value, giving 8214 K for tungsten coefficients,
3713 K for daylight and 5838 K for shade. And `Green`, computed as
`g/√(r·b)`, always landed near 0.5 against RawTherapee's 1.0 neutral — because
camera coefficients normalise `g` to 1.0 while `r` and `b` both exceed it — which
is a magenta cast on every photo.

The fix was not to repair the conversion but to stop converting. RawTherapee's
`Setting=Camera` applies the camera's own coefficients exactly, which is what
"cameras are usually right" wanted in the first place. `EditRecipe` and the
`edits` table therefore carry **no** white-balance fields, and `decide()` has no
white-balance logic. The Cheng-2014 PCA illuminant estimate is still computed and
persisted in `raw_stats` for the audit record — validated on real frames, where
it lands 1.3°–5.4° from the direction implied by the reciprocal as-shot gains —
but nothing acts on it. Overriding the camera needs a conversion that can be
checked against reference values, and is deferred to its own spec.

## 1. Motivation

Steps 1 and 2 (*filter*, *select*) are done: `scan` flags defects and groups
duplicates, the browser UI lets the user curate, and `decisions.is_keeper`
records what survived. What's missing is the last step — the keepers still need
developing, which today means opening them in Lightroom one at a time.

The goal is that the user's only job is curating. Everything after that is
automatic: run `finish`, get a directory of finished JPEGs.

This is the `edit` stage that `IMPLEMENTATION_PLAN.md` §1.3 deliberately left
out of scope, and the catalog schema was designed to accommodate it without
rewrites.

## 2. Decisions locked during brainstorming

| Decision | Choice |
|---|---|
| Output | Finished JPEGs. Fully automatic, no per-photo interaction. |
| Input set | `decisions.verdict = 'keep'` for the folder's library. **Corrected 2026-08-09 — see A8.** |
| Render engine | **`rawtherapee-cli`**, invoked as a subprocess. External dependency, detected by `doctor`. |
| Baseline profile | **Our own version-controlled `base.pp3`**, stacked via `-p`. Never RT's `-d` GUI default. |
| Look | Image-adaptive 3D LUT model trained on MIT-Adobe FiveK (generic "pro" look). |
| LUT application | **In Rust**, on the 16-bit TIFF RT emits. Not via RT's Film Simulation tool. |
| Recipe representation | Typed `EditRecipe` struct → real DuckDB columns (not JSON). |
| Decision layer | Pure function of measurements. No image data, no I/O. |
| Crop / rotation / local edits | **Out of scope for v1.** |
| darktable backend | **Rejected.** XMP history params are encoded blobs; not authorable. |
| vkdt backend | Deferred, not rejected. No Windows support — see §9. |
| Interface (v1) | **CLI only** — `photopipe finish`. No HTTP endpoint, no nav-rail screen. A Develop screen is a later spec. **Shipped 2026-08-13** — see A2. |
| Relation to `export-keepers` | **Independent.** Two commands, two trees, no ordering dependency. |

## 3. Why RawTherapee, and why not the alternatives

The decision layer is ours; the pixel work should not be. Highlight
reconstruction, profiled denoise, lens correction and capture sharpening are
genuinely hard, RawTherapee does them well, and rebuilding them in Rust is a
multi-year detour for strictly worse output.

Among engines that can be driven headlessly:

- **RawTherapee** — `.pp3` is plain-text INI, `-p` stacks profiles, `-t -b16`
  gives 16-bit TIFF, and Windows builds are published. Chosen.
- **darktable** — arguably the best modern modules, but XMP stores per-module
  params as version-tagged encoded blobs. Third parties cannot author them.
  Driving it would mean manipulating `darktablerc`/Lua presets. Rejected.
- **ART** — same CLI shape, `.arp` params, more modern automatic tools. Smaller
  community and thinner packaging, which matters when reverse-engineering an
  undocumented parameter format. Not chosen.
- **LibRaw `dcraw_emu`** — tiny, bundleable, deterministic, ~15 flags. But no
  real denoise, no lens correction, no capture sharpening, and only
  clip/blend/rebuild highlight heuristics. A credible lower-ceiling fallback if
  the RawTherapee dependency ever becomes untenable.
- **vkdt** — see §9.

RawTherapee is GPL-3, but photopipe invokes it as a subprocess and does not link
against it, so no licence obligation propagates.

## 4. Pipeline

```
photopipe finish <folder> --out <dir>

  decisions.verdict = 'keep'
        │
  ① MEASURE   (Rust, rawler)      raw-linear percentiles, per-CFA-channel
        │                          clipping, as-shot wb_coeffs, PCA illuminant
        │                          → raw_stats
        ▼
  ② DECIDE    (Rust, pure fn)     EditRecipe from raw_stats + exif + sharpness
        │                          → edits
        ▼
  ③ BASELINE  (rawtherapee-cli)   base.pp3 + <photo>.pp3
        │                          -Y -t -b16 -o <tmp> -c <raw>
        ▼                          → 16-bit sRGB TIFF
  ④ LOOK      (ort + Rust)        256px downsample → predictor CNN → blend
        │                          weights → fuse basis LUTs → 33³ LUT
        │                          → apply trilinear to full-res → encode JPEG
        ▼
  ⑤ GUARD     (CLIP-IQA)          if the look lowers the score past a
        │                          threshold, keep baseline-only, record why
        ▼
  <out>/<YYYY-MM>/<name>.jpg
  <out>/<YYYY-MM>/<name>.pp3       ← escape hatch: reopen in RawTherapee
  <cache>/luts/<lut_hash>.cube     ← content-addressed, shared between photos
```

### Why the look is a separate stage from the recipe

A FiveK LUT model restyles an already-developed sRGB image. It cannot recover a
clipped highlight or correct a colour cast at sensor level. So the two concerns
are split: **technical corrections** go in the `.pp3` and are decided
analytically; **the look** is a per-image LUT applied afterwards.

### Why the LUT is applied by us, not by RawTherapee

RawTherapee's Film Simulation tool locates HaldCLUTs through a GUI
preferences directory (`Preferences > Image Processing > Directories`), and
RawPedia documents no `.pp3` key for selecting one by path. Driving it headlessly
would mean writing into RT's own config. Applying the LUT ourselves to the
16-bit TIFF avoids that entirely and puts the LUT in exactly the domain it was
trained on — sRGB. The `image` crate (already a dependency) reads 16-bit TIFF and
writes JPEG, so this adds no new dependency.

### Why an explicit `base.pp3`

RT's `-d` flag reads whatever default profile the user configured in the GUI.
That would make photopipe's output depend on invisible machine-local state, and
would silently break the look stage's assumption that the model receives a
*default* raw conversion. `base.pp3` is committed to the repo, applies technical
corrections only — no film simulation, no strong tone curve — and makes the whole
render reproducible across machines.

## 5. Schema (migration version 4)

**A6 — confirmed available.** The catalog is at version 3
(`crates/pipeline/src/catalog/schema.rs`, "version 3 — per-folder library
identity"). Nothing has claimed 4 in the interim, so the migration below applies
verbatim with no renumbering.

The existing `exposure` table is derived from the 8-bit preview JPEG and feeds
defect flagging. It is left untouched: raw-linear statistics are a separate
concern with a separate consumer. Critically, a preview JPEG reports `255` for
anything the camera's tone curve pushed to white, whereas the raw reveals
whether the photosite actually saturated — and highlight reconstruction needs
the latter.

```sql
BEGIN TRANSACTION;
INSERT INTO schema_version VALUES (4);

CREATE TABLE raw_stats (
    file_id           BIGINT PRIMARY KEY REFERENCES files(id),
    p1                REAL NOT NULL,   -- raw-linear percentiles, 0..1,
    p50               REAL NOT NULL,   -- black-subtracted and white-normalised
    p99               REAL NOT NULL,   -- the highlight anchor; see A9
    p999              REAL NOT NULL,
    clipped_frac      REAL NOT NULL,   -- fraction at/above whitelevel
    black_frac        REAL NOT NULL,   -- fraction at/below blacklevel
    wb_r              REAL NOT NULL,   -- as-shot coefficients as encoded in the
    wb_g              REAL NOT NULL,   -- file (RGB, unnormalised)
    wb_b              REAL NOT NULL,
    illum_r           REAL,            -- PCA illuminant estimate;
    illum_g           REAL,            -- NULL when estimation fails
    illum_b           REAL
);

CREATE TABLE edits (
    file_id            BIGINT PRIMARY KEY REFERENCES files(id),
    content_hash       VARCHAR NOT NULL,   -- denormalised: survives moves/renames

    -- recipe
    exposure_ev        REAL NOT NULL,
    highlight_recovery REAL NOT NULL,      -- 0..1
    shadow_lift        REAL NOT NULL,      -- 0..1
    denoise_luma       REAL NOT NULL,      -- 0..1
    denoise_chroma     REAL NOT NULL,      -- 0..1
    sharpen_amount     REAL NOT NULL,      -- 0..1
    lens_correct       BOOLEAN NOT NULL,

    -- provenance / idempotency
    recipe_hash        VARCHAR NOT NULL,
    decider_version    VARCHAR NOT NULL,
    renderer           VARCHAR NOT NULL,   -- 'rawtherapee'
    look_model         VARCHAR,            -- NULL = baseline only
    look_version       VARCHAR,
    lut_hash           VARCHAR,            -- content hash of the .cube; retained
                                           -- even when look_applied is false
    look_applied       BOOLEAN NOT NULL,   -- false if the guard rejected it
    iqa_before         REAL,
    iqa_after          REAL,

    -- output
    output_path        VARCHAR,
    output_size_bytes  BIGINT,
    rendered_at        BIGINT
);
CREATE INDEX idx_edits_hash ON edits(content_hash);
COMMIT;
```

`edits` doubles as the audit record: for any finished JPEG it answers why that
photo looks the way it does, which recipe and model version produced it, and
whether the look survived the quality guard.

## 6. The decision layer

```rust
pub fn decide(raw: &RawStats, exif: &Exif, sharp: &Sharpness) -> EditRecipe
```

Pure — no image data, no I/O, no database. This is the single most important
testability property in the design: the tuning-sensitive logic is exercised by
table-driven unit tests over numbers, without fixtures.

- **`exposure_ev`** — `headroom = log2(0.95 / p99)` (**not `p999`** — see A9),
  `lift = log2(0.18 / p50)`, then `ev = clamp(min(lift, headroom), -3.0, 3.0)`.
  Underexposed frames are lifted but never past clipping; overexposed frames are
  pulled down because `headroom` goes negative.
- **white balance** — **nothing is decided (A10).** The `.pp3` emits
  `Setting=Camera`, so RawTherapee applies the camera's own as-shot coefficients
  exactly and no conversion error can enter. A Cheng-2014 PCA illuminant estimate
  is still computed and stored in `raw_stats` as a cross-check for a future
  override, but `decide()` does not read it and `EditRecipe` has no WB field.
  **Do not reintroduce a coefficient → `Temperature`/`Green` conversion without
  reference values to validate it against** — the previous attempt was inverted
  and cast every frame magenta.
- **`highlight_recovery`** — scales with `clipped_frac`; the reconstruction
  *method* escalates with severity (blend for mild, colour propagation for
  heavy).
- **`shadow_lift`** — from `p1` and `black_frac`, multiplied by a noise penalty
  derived from ISO. Lifting shadows at high ISO only reveals noise, so the
  ceiling falls as ISO rises.
- **`denoise_luma` / `denoise_chroma`** — monotone piecewise-linear in ISO,
  anchored approximately at (100→0), (1600→0.3), (6400→0.6), (25600→0.85).
  These anchors are a starting shape, not a validated claim; they require
  calibration against real high-ISO files (§13, open item 2).
- **`sharpen_amount`** — RT capture sharpening enabled, amount modulated by the
  existing `sharpness.s_global`, hard-capped so a genuinely soft frame is never
  sharpened into crunch.
- **`lens_correct`** — set when `exif.lens_model` is present; RawTherapee
  performs its own lensfun lookup and no-ops if the lens is unknown.

## 7. Look model

Image-adaptive 3D LUT (Zeng et al.): a predictor CNN under 600K parameters
consumes a downsampled image and emits blend weights over N basis 3D LUTs, which
fuse into one image-specific 33³ LUT.

**Only the predictor is exported to ONNX.** The reference implementation applies
its LUT through a custom CUDA trilinear-interpolation extension that will not
trace to ONNX. This is not a blocker, because we want to own application anyway:
the exporter emits the predictor CNN, dumps the basis LUTs as plain tensors, and
Rust performs the fuse and the trilinear apply at 16-bit precision.

Per-image LUTs are written as `<lut_hash>.cube` into the cache directory. This
keeps them inspectable, reusable, and loadable in RawTherapee or darktable if
the user wants to reproduce a look by hand.

Following the established convention, ONNX weights are gitignored and produced
on the user's machine by a `tools/export_lut3d.py` script; the repository never
contains or redistributes them.

### A5 — what already exists

Two parts of this stage that read as new work in the original draft are reuse:

- **The CLIP-IQA guard.** The public `Iqa` trait
  (`crates/pipeline/src/models/mod.rs`) already exposes
  `score(&self, img: &DynamicImage) -> Result<f32>`, and `ModelHub.iqa` is a
  public `Option<Arc<dyn Iqa>>` already populated by the scan pipeline. The guard
  is two calls on an existing handle — once on the baseline TIFF, once on the
  looked JPEG — with **no** change to the model layer at all.
  `[develop.look].guard_iqa` therefore costs almost nothing to ship.
- **`tools/export_lut3d.py`.** `tools/export_rt_detr.py` already performs ONNX
  graph surgery to fix up a traced graph, weights are gitignored, and
  `models/download.sh` is the established entry point. Excluding the reference
  implementation's custom CUDA trilinear op from the traced predictor is the same
  class of problem, already solved once in this repo. Open item 3 is routine, not
  research.

**A7 — the look model stands.** No corpus of paired RAW / Lightroom-export files
is available, so the own-look least-squares fit remains a §12 non-goal and the
generic FiveK model is the v1 look as originally designed.

**Licensing:** the Image-Adaptive-3DLUT *code* is Apache-2.0, but its weights
derive from MIT-Adobe FiveK, whose Adobe licence grants use "solely for your own
research purposes" and forbids exercising those rights "in any manner that is
intended for or directed toward commercial advantage." photopipe is a personal,
non-commercial project and never redistributes the weights, so this is recorded
as a constraint rather than a problem. It would need revisiting before any
commercial distribution.

## 8. Execution and resource behaviour

Unlike `scan`, this stage must **not** fan out widely with rayon:

- `rawtherapee-cli` exposes no thread flag and saturates all cores internally.
- A 16-bit TIFF of a 60MP raw is roughly 350 MB.

Stages ③–④ therefore process 1–2 files concurrently, streaming through a temp
directory and deleting each intermediate immediately after the JPEG is encoded.
Stages ① and ② are cheap and may use the existing parallelism.

**A2 — progress reporting.** v1 ships no HTTP endpoint and no UI screen, but the
library entry point takes the *existing* progress abstraction rather than printing
directly:

```rust
pub fn finish_folder(
    /* … catalog, cache, config, out_dir … */
    progress: &dyn ProgressSink,
) -> Result<FinishReport>
```

`ProgressSink` is the trait `analyze_folder()` already uses
(`crates/pipeline/src/analyze.rs`). `finish` calls `stage("measuring")`,
`stage("rendering")`, `stage("applying look")`, `stage("done")`. The CLI passes a
terminal sink. When a Develop screen is specified later, it passes the server's
existing job sink to a `POST /api/finish` + `/api/finish/status` pair and the
frontend reuses the analyze checklist component unchanged. This costs nothing now
and prevents the UI iteration from being a rewrite.

**Shipped 2026-08-13**, and the bet mostly paid: the endpoints, the job slot and
the checklist markup were all reused as predicted. One correction to the sketch
above. `stage()` alone could not carry a develop run's progress — it resets the
per-phase counter by contract, so a stage transition per photo would wipe the
run's own "N of M photos" four times per photo. The run is therefore *one*
counted phase (`developing`), with a new defaulted `ProgressSink::step(step,
item)` carrying the per-photo detail and the filename beside it. The screen also
needed a preflight the sketch did not anticipate — `GET /api/finish/estimate`,
so a missing `rawtherapee-cli` is refused as the setup problem it is rather than
surfacing as a job that dies three seconds in.

**Idempotency.** A file is skipped when an `edits` row matches on
`(content_hash, recipe_hash, decider_version, renderer, look_model,
look_version)` *and* `output_path` exists with `output_size_bytes` unchanged.
This mirrors the "missing or differs" semantics of the existing `copy_file`
helper, so a second `finish` run does zero work.

**Failure isolation.** Per the project error-handling rule, one unreadable raw
or one non-zero `rawtherapee-cli` exit must not abort the run: log with
`tracing::warn!(path, error)`, leave no `edits` row, and continue.

**Originals are never touched.** The `.pp3` is written next to the finished JPEG
in the output directory, and the `.cube` into the cache directory under
`luts/<lut_hash>.cube`. Neither goes beside the source raw. This is a deliberate
departure from RawTherapee's own convention, which writes `photo.raw.pp3`
alongside the original — that would violate the non-destructive contract by
adding files to the user's library. Content-addressing the LUTs also means
visually similar frames in a burst share one file rather than each writing their
own.

## 9. vkdt as a future backend

vkdt reached 1.0.0 in December 2024 and ships current nightlies; it is not
experimental. It is attractive here: the `.cfg` node graph is plain text,
`vkdt-cli --config` accepts additional cfg lines directly on the command line
(no sidecar files at all), processing is entirely GPU/Vulkan, and two of its
stated strengths — better highlight inpainting and neural denoising — are exactly
what this stage needs. On an RTX 5090 it would turn a large shoot from hours into
minutes.

It cannot be the v1 backend for one specific reason: **vkdt does not support
Windows.** Its readme lists NixOS, Arch, macOS and an unfinished Android port.
The 2026-06-27 windows-copy-export work made native Windows a delivered
requirement, and defaulting to vkdt would forfeit it.

The design keeps the door open: `EditRecipe` is the stable contract and renderers
sit behind it.

```
EditRecipe ──┬─→ Pp3Renderer    (rawtherapee-cli)   ← v1, Windows-safe
             ├─→ VkdtRenderer   (vkdt-cli --config)  ← future, Linux/macOS, GPU
             └─→ LutPostProcess (shared: TIFF → .cube → JPEG)
```

Only `Pp3Renderer` is built in v1. Building both would double the
reverse-engineering and validation work on the riskiest part of the design for no
new capability. `vkdt`'s `src/fit` parameter-fitting component should be read
before stage ② is written, in case it already solves part of the decision problem.

## 10. Configuration

```toml
[develop]
renderer          = "rawtherapee"      # only backend in v1
rawtherapee_path  = ""                 # empty = search PATH
finished_dir      = "<library>/_finished"  # `<library>` substituted with the
                                       # scan root, matching [output].review_tree
jpeg_quality      = 92
output_subdirs    = "month"            # "month" | "flat"

[develop.look]
enable            = true
model             = "lut3d-fivek"
guard_iqa         = true           # fall back to baseline if the look hurts IQA
guard_margin      = 0.02           # allowed IQA drop before rejecting the look
```

`--out <dir>` overrides `finished_dir`; the CLI argument wins so a one-off export
never needs a config edit.

`photopipe doctor` gains a check for `rawtherapee-cli` — presence, version, and a
one-frame smoke render — and fails loudly with an install hint when it is
missing, rather than failing per-photo deep into a run.

### A3 — relationship to `export-keepers`

`export-keepers` builds a `keepers/YYYY-MM/` tree of copied RAWs; `finish` builds
a `_finished/YYYY-MM/` tree of JPEGs. Both read `decisions.is_keeper` from the
library directly, and neither depends on the other having run.

They are kept independent deliberately. `finish` reading an already-exported
keepers tree would introduce an ordering requirement and a second source of truth
for file paths, for no gain — the catalog already knows every keeper's original
path. `export-keepers` remains the "hand the RAWs to Lightroom or a backup" path;
`finish` is the "I'm done, give me JPEGs" path. Neither supersedes the other.

## 11. Testing strategy

- **`decide()` unit tests** — table-driven over synthetic `RawStats`/`Exif`
  inputs. Covers the clipping-bounded lift, the negative-headroom pull-down, the
  ISO noise penalty, the WB blend branch, and every clamp boundary. No fixtures.
- **LUT application** — a known 33³ identity LUT must round-trip a test image
  bit-exactly; a known non-identity LUT must match a reference computed
  independently.
- **pp3 emission** — golden-file tests: a fixed `EditRecipe` produces a byte-exact
  `.pp3`. Catches accidental key drift.
- **Idempotency** — run `finish` twice over a fixture library; assert the second
  run performs zero renders. This is a correctness requirement, per CLAUDE.md.
- **End-to-end** — gated on `rawtherapee-cli` being present; skipped with a clear
  message when absent so `cargo test --all` still passes on a bare machine.
- **Error isolation** — a deliberately truncated raw in the fixture set must be
  warned about and skipped without aborting the run or writing an `edits` row.

Real-photo validation (does the output actually look good?) is a manual review
pass over the Grindelwald set, not something the test suite can assert.

## 12. Non-goals for v1

- Crop, rotation, straightening. Cropping is an authorial decision; automating it
  would take a compositional choice away from the user, who has said curation is
  the part they want to keep.
- Local or masked adjustments (sky, subject, skin). Expressible in neither `.pp3`
  nor our post-process cleanly; would need its own design.
- Learning the user's personal look from their existing Lightroom exports. This
  is the strongest long-term differentiator and needs no neural network — a
  least-squares 33³ LUT fit over RAW/export pairs would do — but it needs a corpus
  of paired RAW and exported files, which is not available (A7), so the generic
  FiveK look is the v1 look.
- A `finish` screen in the review UI. v1 is CLI-only; §8 keeps the seam open.
- Multiple render backends (§9).
- ML denoise or restoration models. RawTherapee's profiled denoise is good, and
  NAFNet-class models are heavy, need tiling, and would add little here.

## 13. Sequencing and open items

**A1 — one plan, three phases, one hard checkpoint.** The work goes into a single
implementation plan rather than two. The staging argument still holds, so it is
enforced by an explicit gate inside the plan instead of by a document boundary.

- **Phase 0 — environment and research.** No Rust. Install `rawtherapee-cli` and
  confirm `-Y -t -b16 -o <tmp> -c <raw>` produces a 16-bit TIFF from one of the
  user's own files. Resolve open item 1 by the GUI-diff method. Land the `doctor`
  check as the first commit, so every later task runs against a gated
  environment. This phase exists because `rawtherapee-cli` is **not currently
  installed** — the original draft assumed availability. It also front-loads the
  single largest unknown in the spec: the `.pp3` key names are pure manual
  research, and discovering key drift mid-implementation is far more expensive
  than discovering it now.
- **Phase 1 — baseline develop (was 13a).** Schema v4, `raw_stats`, `decide()`,
  `base.pp3`, `Pp3Renderer`, the `finish` command over `ProgressSink`,
  idempotency. Produces finished JPEGs with technical corrections and no look.
  Independently useful.
- **CHECKPOINT — baseline sign-off.** Phase 1's output is reviewed on the
  Grindelwald set and explicitly signed off before any Phase 2 work starts. This
  gate is not optional. Tuning a look on top of an unvalidated baseline makes a
  bad `exposure_ev` and a bad LUT indistinguishable in the output, and the whole
  reason the analytic layer comes first is that it can be judged alone.
- **Phase 2 — the look (was 13b).** `tools/export_lut3d.py`, the predictor CNN in
  `ort`, LUT fuse and trilinear apply, `.cube` caching, and the CLIP-IQA guard.

Open items:

1. **Exact `.pp3` key names** for each recipe field. RawPedia does not document
   them. Reliable method: set each tool in the RT GUI, save, and diff the
   resulting `.pp3`. [AI-PP3](https://github.com/tychenjiajun/art) provides
   templates worth cross-checking (GPL-2.0; reference only, no code reuse).
   *Scheduled as Phase 0 work.* **Closed in Phase 0.** The keys were
   recovered by the GUI-diff method and are recorded with their verified ranges
   in `docs/design/pp3-keys.md`; `base.pp3` and the emitter are built from that
   table rather than from RawPedia.
2. **ISO→denoise anchors** in §6 need calibration against the user's real
   high-ISO files before they can be claimed as tuned. *Phase 1.* **Still open
   after the 2026-08-13 CHECKPOINT: no high-ISO material exists to calibrate
   against.** Every available frame is ISO 100, so `denoise_luma` and
   `denoise_chroma` came out 0.00 across the board and the anchors were never
   exercised, let alone validated. They remain the spec's starting shape and must
   not be described as tuned. Phase 2 does not depend on them — the look model
   consumes a developed sRGB image, not the denoise parameters — so this is
   carried forward rather than blocking. Closing it needs either an ISO ladder
   shot on the ILCE-6300 (same sensor, same lens, one static scene) or a
   licence-clean public set; a foreign camera's noise character would calibrate
   the anchors for the wrong sensor.
3. **`tools/export_lut3d.py`** — export the predictor CNN and dump basis LUTs,
   confirming the custom CUDA op is excluded from the traced graph. *Phase 2;
   downgraded from research risk to routine by A5.* **Closed 2026-08-13.** The custom
   op never reached the graph and needed no excluding: the reference applies its
   LUT outside the module that gets traced, so exporting the predictor alone is
   the natural result rather than a surgical one. The exporter asserts this
   anyway, since a future upstream change could fold the apply back in.

   What did need care was everything around it. Upstream ships the basis LUTs
   and the classifier as two checkpoints, not one; the predictor's head is a
   `Conv2d(128, 3, 8)` over an 8x8 feature map rather than global-pool +
   `Linear`, which is why the 256×256 input size is load-bearing; and the basis
   tensors sit at `state[str(i)]["LUT"]`. The exporter therefore loads with
   `strict=True` and checks its ONNX output against PyTorch numerically —
   `strict=False` plus a key pattern that matched nothing would have exported a
   graph of random weights that still passed every structural check.
4. **PCA illuminant estimator** — confirm the Cheng-2014 variant behaves on the
   fixture set; fall back to as-shot coefficients whenever it fails rather than
   propagating an error. *Phase 1.*
5. **`base.pp3` contents** — the neutral baseline must be validated as close to a
   default raw conversion, since the look model's input distribution depends on
   it. *Phase 1, and part of the checkpoint's sign-off criteria.* **Closed at the
   2026-08-13 CHECKPOINT.** Verified by reading the profile against RawTherapee
   5.13 and by eye on three rendered frames: `Brightness`, `Contrast` and
   `Saturation` are 0, `Curve`/`Curve2` are empty, `HistogramMatching` and
   `CurveFromHistogramMatching` are false, `[Film Simulation]` and
   `[ColorToning]` are disabled, and `OutputProfile=RTv4_sRGB` is pinned so a
   RawTherapee release cannot silently hand the model a Rec2020 image. The
   renders carry no look of their own. No change was needed.

**A11 — the CHECKPOINT is signed off with item 2 explicitly carried forward.**
Reviewed 2026-08-13 on the ILCE-6300 sample set. Exposure, white balance,
highlight recovery and `base.pp3` neutrality all pass. Sharpening failed the
review and was fixed rather than tuned: `decide()` was clamping an unbounded
variance-of-Laplacian to 0..1, so every frame got `SHARPEN_MAX` regardless of how
soft it was (see `docs/KNOWN_ISSUES.md`, fixed 2026-08-13). Denoise could not be
judged at all — see item 2.

Two caveats on the strength of this sign-off, recorded so Phase 2 does not
inherit false confidence. The corpus is **three photographs**, so "no systematic
exposure bias" means no bias visible in three frames. And the sharpness baseline
those three were normalised against was itself built from the same three, making
its p10/p90 the set's own min and max; §7 asks for a few hundred photos per lens
before the percentiles mean anything. The baseline mechanism is verified
end-to-end, its calibration is not.

## 14. References

- [rawtherapee-cli(1)](https://manpages.debian.org/testing/rawtherapee/rawtherapee-cli.1)
- [RawPedia — Sidecar Files / Processing Profiles](https://rawpedia.rawtherapee.com/Sidecar_Files_-_Processing_Profiles)
- [RawPedia — Film Simulation](https://rawpedia.rawtherapee.com/Film_Simulation)
- [Image-Adaptive-3DLUT](https://github.com/HuiZeng/Image-Adaptive-3DLUT)
- [AdaInt](https://arxiv.org/pdf/2204.13983)
- [MIT-Adobe FiveK](https://data.csail.mit.edu/graphics/fivek/)
- [vkdt](https://github.com/hanatos/vkdt) · [vkdt-cli](https://github.com/hanatos/vkdt/blob/master/src/cli/readme.md)
- [LibRaw dcraw_emu options](https://www.libraw.org/docs/Samples-LibRaw.html)
- [ART](https://artraweditor.github.io/)
