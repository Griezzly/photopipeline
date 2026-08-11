# RawTherapee `.pp3` keys used by `photopipe finish`

Verified against **RawTherapee 5.13** on **2026-08-09**. Section headers are
literal INI sections; keys are case-sensitive and **RawTherapee silently ignores
anything it does not recognise**, so drift here is invisible at runtime.

## How these were verified

Not by GUI diffing. RawTherapee's CLI has a `-O <dir>` flag that copies the
resolved processing profile next to the output, and the CLI builds every profile
from *neutral* values unless `-d` is passed (confirmed by `rawtherapee-cli -h`:
"1- A new processing profile is created using neutral values"). So one render
emits a complete, key-exhaustive neutral profile:

```bash
rawtherapee-cli -Y -j90 -O <outdir> -c <some-image>
# → <outdir>/<name>.pp3, ~16 KB, every section and key at its default
```

This is stronger evidence than a GUI diff: it is the full key space, not the
subset one control happens to touch, and it is reproducible in one command.

To regenerate after a RawTherapee upgrade, re-run the above and diff against
this table.

## Verified key table

| Recipe field | Section | Key | Type / range | Verified |
|---|---|---|---|---|
| `exposure_ev` | `[Exposure]` | `Compensation` | float, EV, default `0` | ☑ |
| — | `[Exposure]` | `Auto` | bool — must be `false` | ☑ |
| `highlight_recovery` (on/off) | `[HLRecovery]` | `Enabled` | bool, default `false` | ☑ |
| `highlight_recovery` (method) | `[HLRecovery]` | `Method` | `Blend` \| `Coloropp` \| … , default `Coloropp` | ☑ |
| `shadow_lift` | `[Shadows & Highlights]` | `Enabled`, `Shadows` | bool, 0–100 | ☑ |
| — | `[Shadows & Highlights]` | `Highlights` | 0–100, default `0` — pinned to `0` so lifting shadows never also pulls highlights | ☑ |
| white balance | `[White Balance]` | `Setting` = `Camera` | — applies the camera's own as-shot coefficients | ☑ |
| `denoise_luma` | `[Directional Pyramid Denoising]` | `Enabled`, `Luma` | bool, 0–100 | ☑ |
| `denoise_chroma` | same | `Chroma` | 0–100, default `15` | ☑ |
| `sharpen_amount` | `[PostDemosaicSharpening]` | `Enabled`, `Contrast` | bool, 0–100 | ☑ |
| — | `[PostDemosaicSharpening]` | `DeconvRadius` | float, default `0.75` | ☑ |
| `lens_correct` | `[LensProfile]` | `LcMode` | `none` (default) \| `lfauto` | ☑ |
| `lens_correct` | `[LensProfile]` | `UseDistortion`, `UseVignette`, `UseCA` | bool | ☑ |

## Silent no-op traps

**These are the reason this document exists.** Each one is a key whose default
causes a *neighbouring* key we do set to be ignored. RawTherapee reports nothing;
the render simply comes out as if we had never asked.

| Trap | Default | What it breaks | Fix |
|---|---|---|---|
| `[White Balance] Setting` | `Camera` | `Temperature` and `Green` are ignored under `Camera` — they apply only under `Custom` | **v1 wants `Camera`**, so emit it explicitly rather than relying on the default (see the note below) |
| `[White Balance] Enabled` | `true` | — (already correct, but must not be flipped off) | emit `Enabled=true` |
| `[PostDemosaicSharpening] AutoContrast` | `true` | `Contrast` is ignored; RawTherapee picks its own | emit `AutoContrast=false` |
| `[PostDemosaicSharpening] AutoRadius` | `true` | `DeconvRadius` is ignored | emit `AutoRadius=false` |
| `[Directional Pyramid Denoising] CMethod` | `MAN` on a neutral profile, but `AUT` in several bundled profiles | under `AUT`, `Chroma` is ignored | emit `CMethod=MAN` explicitly |
| `[Directional Pyramid Denoising] Method` | `Lab` | under `Lab`, chroma is split into `Redchro`/`Bluechro`; a single `Chroma` slider is the `Lab`+`MAN` combination | emit `Method=Lab` and `CMethod=MAN` |

## Why v1 uses `Setting=Camera`

An earlier revision converted the as-shot coefficients into `Temperature`/`Green`
and emitted `Setting=Custom`. That conversion was wrong in two independent ways:

- **Temperature was inverted.** `5000 · (b/r)^0.85` maps warm light to a high
  kelvin value — tungsten coefficients gave 8214 K, daylight 3713 K, shade
  5838 K. A high `b/r` means the camera is boosting blue hard, which happens
  under *warm* low-kelvin light; the relationship ran backwards.
- **`Green` was systematically offset.** `g / √(r·b)` with camera coefficients,
  which normalise `g` to 1.0 while `r` and `b` both exceed 1.0, always lands near
  0.5 against the 1.0 neutral — a magenta cast on every frame.

RawTherapee derives its own multipliers from the camera profile, so asking it for
the camera's white balance is exact, where restating that white balance in a
foreign parameterisation was not. The PCA illuminant estimate is still measured
and persisted in `raw_stats`; acting on it needs a conversion that can be
validated against reference values, and is deferred to its own spec.

## Ranges and units

- `Compensation` is in **EV**, signed, and maps 1:1 from `EditRecipe::exposure_ev`.
- `Shadows`, `Luma`, `Chroma`, `Contrast` all run **0–100**, so a 0..1 recipe
  field emits `round(v * 100)`.
- `Temperature` is an **integer** kelvin value. `Green` is a float around 1.0.
- `DeconvRadius` is a float in pixels; `0.75` is RawTherapee's own default and a
  safe fixed choice.

## Keys deliberately NOT written

`base.pp3` owns these; the per-photo profile must never set them or it would
override the baseline and change the input distribution the Phase 2 look model
was trained on:

- `[Film Simulation]` — anything.
- `[Exposure] Curve`, `Curve2`, `Brightness`, `Contrast`, `Saturation`.
- `[Exposure] HistogramMatching` / `CurveFromHistogramMatching` — these apply an
  automatic tone curve derived from the embedded JPEG. Several bundled profiles
  (`Auto-Matched Curve - ISO *.pp3`) enable them; ours must not.
- `[ColorToning]`, and any `[Color Management]` change away from sRGB output.

## Related

- Bundled reference profiles, useful for cross-checking real-world values:
  `/Applications/RawTherapee.app/Contents/Resources/share/profiles/*.pp3`
- Profile stacking order is documented in `rawtherapee-cli -h`: neutral → `-d`
  → each `-p` in command-line order → `-s` sidecar. photopipe uses
  `-p base.pp3 -p <photo>.pp3` and **never** `-d`, which would read the user's
  GUI-configured default and make output depend on machine-local state.
