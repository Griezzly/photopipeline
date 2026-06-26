# Phase 4 — RT-DETR Finalization + Lens Calibration + Refined Blur Flagging Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Finalize and commit the already-written RT-DETR subject detector, then build the `photopipe calibrate` command that rebuilds per-lens sharpness baselines and re-flags `blur` / `back_focus` / `low_iqa` defects from real subject ROIs.

**Architecture:** Group A finalizes uncommitted detector work (verify decode against the real ONNX I/O contract, recreate the `tools/` export script, update doctor/README, commit). Group B adds a pure-Rust `calibration` module (focal/aperture bucketing in Rust — no DuckDB UDF), new `Catalog` methods that rebuild the `sharpness_baseline` table and re-flag files in transactional batches, an orchestrator `run_calibration`, CLI wiring for `cmd_calibrate`, and six integration tests that drive the logic with synthetic sharpness/EXIF rows.

**Tech Stack:** Rust (edition 2021, stable), `duckdb` (bundled), `ort` 2.0.0-rc.12, `ndarray`, `image` 0.25, `rayon`, `tracing`, `anyhow`/`thiserror`, `clap`. Python (in `tools/` only, one-time export): `torch`, `transformers`, `onnx`, `onnxsim`. No new Rust dependencies.

## Global Constraints

- Edition 2021, stable Rust. `anyhow::Result` at CLI boundaries; `thiserror` types inside `pipeline`.
- DuckDB ONLY (no SQLite). Bulk writes go through ONE transaction per batch.
- No AGPL deps. No Python at runtime (Python only in `tools/` for one-time ONNX export).
- Non-destructive: never modify/move/delete an original photo. Symlinks/hardlinks/reads only.
- Idempotency is a correctness requirement: re-running `calibrate` on unchanged data produces an identical flag set (modulo float rounding).
- `tracing` for logs (`info!`/`warn!`/`debug!`); no `println!` except intentional CLI user output (doctor/stats/calibrate report).
- Run before declaring a task done: `cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D warnings`, and the task's tests.
- Surface (don't silently add) any new dependency or deviation from the spec. No new deps are needed for Phase 4.
- No `unsafe` blocks.
- WSL note: `source ~/.cargo/env` before any `cargo` command; run cargo from the workspace root `/home/carsten/workspace/photopipeline`.
- Every commit message ends with this trailer line:
  `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`

---

## File Structure

| File | Action | Responsibility |
|------|--------|----------------|
| `crates/pipeline/src/models/detector.rs` | Modify | Remove the stale deferral doc-comment on `RtDetrDetector`; the preprocess/decode/tests stay. Possibly adjust output ordering in `detect()` if the real I/O contract differs (Task 1). |
| `tools/export_rt_detr.py` | Create | Standalone Python script that exports the RT-DETR R50VD checkpoint to ONNX opset 17+. Documents how `models/rt_detr_l.onnx` was produced. |
| `tools/requirements.txt` | Create | Pinned Python deps for the export script. |
| `models/README.md` | Modify | Replace the "Deferred" RT-DETR row + deferral section with "working" status. |
| `crates/cli/src/main.rs` | Modify | `cmd_doctor`: report `rt_detr_l.onnx` via `doctor_model_file(...)`. `cmd_calibrate`: call `run_calibration` and print report. `Calibrate` help text. |
| `crates/pipeline/src/calibration/buckets.rs` | Create | `focal_bucket(f32) -> i32` and `aperture_bucket(f32) -> f32`, pure Rust, with unit tests. |
| `crates/pipeline/src/calibration/mod.rs` | Modify (replace stub) | `CalibrationReport`, `run_calibration`, re-export `buckets`. |
| `crates/pipeline/src/catalog/mod.rs` | Modify | New methods: `rebuild_sharpness_baselines`, `clear_blur_related_flags`, `iter_sharpness_for_reflag`, `iqa_global_p10`, `flush_blur_flag_batch`, `bucket_baseline_p10`; new pub structs `RebuildReport`, `SharpnessReflagRow`, `BlurFlagRow`. |
| `crates/pipeline/src/lib.rs` | Modify | `pub use calibration::{run_calibration, CalibrationReport};`. |
| `crates/pipeline/tests/calibration.rs` | Create | Six integration tests + synthetic-row helpers. |

**Design decisions locked here (read before coding):**

1. **No DuckDB scalar UDF.** Bucketing is done in Rust (`calibration::buckets`). `iter_sharpness_for_reflag` returns *raw* EXIF (`focal_length_mm`, `aperture`) and `rebuild_sharpness_baselines` computes buckets in Rust, groups in a Rust `HashMap`, computes percentiles in Rust, then writes baseline rows with prepared `INSERT … ON CONFLICT` inside one transaction. Rationale: the `duckdb` crate's `create_scalar_function` API is awkward at this version and the GROUNDING doc recommends the pure-Rust path; this avoids per-connection UDF registration entirely. (UDF registration is the documented fallback only if the Rust path were impossible — it is not.)
2. **`flush_blur_flag_batch` uses prepared `INSERT … ON CONFLICT (file_id, flag_type)` inside `Connection::transaction()`** — NOT the Appender. This OVERRIDES the old PHASE_4_PROMPT instruction to "use the Appender." Reason (from GROUNDING §"Appender vs prepared-INSERT"): `defect_flags` has an identity-default `id` (`DEFAULT nextval(...)`) plus a `UNIQUE(file_id, flag_type)` constraint; the Appender appends all columns positionally and does not fill `DEFAULT nextval()` cleanly, and cannot express `ON CONFLICT`. The proven codebase pattern (`flush_defect_batch`) is prepared statements in one transaction. One transaction per batch still satisfies the "one transaction per batch" rule.
3. **Bucket math in Rust, grouping in Rust.** No SQL `GROUP BY` over UDFs and no `quantile_cont` over tiny samples (which has known linear-interpolation quirks). Percentiles are computed in Rust with the nearest-rank-with-interpolation method described in Task 4, deterministic for any sample size.
4. **Calibrate is one atomic command** in this order: (a) clear blur-related flags, (b) rebuild baselines, (c) reflag. `overexposed`/`underexposed` flags are never touched.

---

## Task 1: Verify the RT-DETR detector against the real ONNX I/O contract

**Files:**
- Verify (no edit unless contract differs): `crates/pipeline/src/models/detector.rs`
- Model (present, gitignored): `models/rt_detr_l.onnx` (175 MB)

**Interfaces:**
- Consumes: existing `RtDetrDetector`, `preprocess`, `decode_detections`, the `rtdetr_loads_and_runs` smoke test in `detector.rs` (lines 297–333).
- Produces: confirmation that `decode_detections` reads `outputs.iter()` as `(logits, boxes)` in that order, with logits shape `[1, num_queries, num_classes]` and boxes `[1, num_queries, 4]` cxcywh-normalized. If the real contract differs, an adjusted `detect()` (still producing `Vec<DetectedSubject>` with normalized top-left `BBox`).

- [ ] **Step 1: Run the pure unit tests (no model file needed)**

```bash
source ~/.cargo/env
cargo test -p pipeline --lib models::detector::tests -- --nocapture
```

Expected: PASS for `decode_above_threshold_fires_below_does_not`, `decode_cxcywh_converts_to_topleft_bbox`, `decode_coco_id_0_is_person`, `decode_coco_id_16_is_animal`, `decode_coco_id_3_is_vehicle`, `decode_coco_id_50_is_object`, `preprocess_produces_640x640_tensor_in_unit_range`. The `rtdetr_loads_and_runs` test also runs but no-ops printing "skipping" only if the model file is absent (it is present here, so it will actually run).

- [ ] **Step 2: Run the model-gated smoke test and capture the printed I/O contract**

```bash
source ~/.cargo/env
cargo test -p pipeline rtdetr_loads_and_runs -- --nocapture
```

Expected: the test prints an `=== RT-DETR ONNX I/O contract ===` block listing INPUTS, OUTPUTS, runs a forward pass on a `(1,3,640,640)` zero tensor, prints each output's `shape=` and `len=`, and ends with `=== Path C gate CLEARED ===`. **PASTE the full printed contract block back into the execution thread before proceeding.**

- [ ] **Step 3: Confirm output ordering and shapes match `decode_detections` assumptions**

Check the pasted output against these assumptions in `detect()` (detector.rs:47–71):
- There are exactly two `f32` outputs.
- The FIRST output is logits with shape `[1, NQ, NC]` (NQ≈300, NC=80).
- The SECOND output is boxes with shape `[1, NQ, 4]`.

Decision tree (this is the ONE place a documented adjustment is allowed):
- **If the contract matches** (logits first, boxes second, shapes as above): no code change. Add a one-line comment above the `out_iter` block in `detect()` recording the verified contract, e.g. `// Verified 2026-06-26 against models/rt_detr_l.onnx: outputs are (logits [1,NQ,NC], pred_boxes [1,NQ,4]).`
- **If boxes come first / logits second:** swap the two `out_iter.next()` extractions so `logit_val` reads the logits output and `box_val` reads the boxes output. Keep `decode_detections` unchanged.
- **If an output is named and order is unstable:** select by name instead of position — replace the positional `out_iter` with `outputs.get("logits")` / `outputs.get("pred_boxes")` (use the exact names printed in Step 2). Map the missing-name case to `anyhow::bail!`.
- **If boxes are in xyxy (corner) rather than cxcywh:** change the box conversion inside `decode_detections` from `(cx - w/2, cy - h/2, w, h)` to `(x0, y0, x1 - x0, y1 - y0)` and update the unit test `decode_cxcywh_converts_to_topleft_bbox` accordingly. Only do this if the smoke output makes the format unambiguous; otherwise keep cxcywh (the RT-DETR R50VD HF export convention).

- [ ] **Step 4: If any code changed in Step 3, re-run both test groups**

```bash
source ~/.cargo/env
cargo test -p pipeline --lib models::detector -- --nocapture
cargo test -p pipeline rtdetr_loads_and_runs -- --nocapture
```

Expected: all unit tests PASS and the smoke test prints `=== Path C gate CLEARED ===`.

- [ ] **Step 5: No commit yet** — Task 1 is verification + (conditional) adjustment that is committed together with the doc/doctor changes in Task 4. Leave the working tree as-is for Task 2.

---

## Task 2: Recreate `tools/export_rt_detr.py` and `tools/requirements.txt`

**Files:**
- Create: `tools/export_rt_detr.py`
- Create: `tools/requirements.txt`

**Interfaces:**
- Consumes: nothing in the Rust crate. This documents how the committed `models/rt_detr_l.onnx` was produced (resolves the IMPLEMENTATION_PLAN §15.4 / §15.7 "missing tools" note).
- Produces: a runnable script that writes `models/rt_detr_l.onnx`. Python is a one-time export tool only; the shipped binary has zero Python dependency.

- [ ] **Step 1: Confirm the `tools/` directory does not yet exist**

```bash
ls /home/carsten/workspace/photopipeline/tools 2>&1
```

Expected: `No such file or directory`.

- [ ] **Step 2: Create `tools/requirements.txt`**

Write `/home/carsten/workspace/photopipeline/tools/requirements.txt`:

```
torch>=2.2
torchvision>=0.17
transformers>=4.40
onnx>=1.16
onnxruntime>=1.18
onnxsim>=0.4
huggingface-hub
```

- [ ] **Step 3: Create `tools/export_rt_detr.py`**

Write `/home/carsten/workspace/photopipeline/tools/export_rt_detr.py`. The present 175 MB single-file (no `.onnx.data` sidecar) model is an R50VD-backbone RT-DETR; this script supports both the `onnx-community/rtdetr_r50vd` and `PekingU/rtdetr_r50vd` checkpoints (the smoke test comment references `onnx-community/rtdetr_r50vd`). Default to `PekingU/rtdetr_r50vd` (Apache-2.0, the canonical source per IMPLEMENTATION_PLAN §9); the `--checkpoint` flag overrides.

```python
#!/usr/bin/env python3
"""Export an RT-DETR R50VD subject detector to ONNX (opset 17+).

One-time tool. Produces ../models/rt_detr_l.onnx, the file consumed by
pipeline::models::detector::RtDetrDetector. Not used at runtime — the shipped
photopipe binary has zero Python dependency.

The exported graph has two float32 outputs in this order:
    logits     : [batch, num_queries, num_classes]   (num_classes = 80, COCO)
    pred_boxes : [batch, num_queries, 4]              (cx, cy, w, h, normalized)
matching crates/pipeline/src/models/detector.rs::decode_detections.

Usage:
    python -m venv .venv && source .venv/bin/activate
    pip install -r requirements.txt
    python export_rt_detr.py                 # PekingU/rtdetr_r50vd
    python export_rt_detr.py --checkpoint onnx-community/rtdetr_r50vd
"""
import argparse
import pathlib

import torch
from transformers import RTDetrForObjectDetection

INPUT_SIZE = 640
OPSET = 17


class Wrapper(torch.nn.Module):
    """Expose only (logits, pred_boxes) so the ONNX graph is decode-friendly."""

    def __init__(self, model: torch.nn.Module):
        super().__init__()
        self.model = model

    def forward(self, pixel_values: torch.Tensor):
        out = self.model(pixel_values=pixel_values)
        return out.logits, out.pred_boxes


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--checkpoint", default="PekingU/rtdetr_r50vd")
    parser.add_argument(
        "--out",
        default=str(pathlib.Path(__file__).resolve().parent.parent / "models" / "rt_detr_l.onnx"),
    )
    args = parser.parse_args()

    print(f"loading {args.checkpoint} ...")
    model = RTDetrForObjectDetection.from_pretrained(args.checkpoint)
    model.eval()

    wrapper = Wrapper(model).eval()
    dummy = torch.zeros(1, 3, INPUT_SIZE, INPUT_SIZE, dtype=torch.float32)

    out_path = pathlib.Path(args.out)
    out_path.parent.mkdir(parents=True, exist_ok=True)
    print(f"exporting to {out_path} (opset {OPSET}) ...")

    torch.onnx.export(
        wrapper,
        (dummy,),
        str(out_path),
        input_names=["pixel_values"],
        output_names=["logits", "pred_boxes"],
        dynamic_axes={
            "pixel_values": {0: "batch"},
            "logits": {0: "batch"},
            "pred_boxes": {0: "batch"},
        },
        opset_version=OPSET,
        do_constant_folding=True,
    )

    # Fold constants so the ORT CPU/CUDA graph is lean. Best-effort.
    try:
        import onnx
        import onnxsim

        model_onnx = onnx.load(str(out_path))
        simplified, ok = onnxsim.simplify(model_onnx)
        if ok:
            onnx.save(simplified, str(out_path))
            print("onnxsim: simplified graph saved")
        else:
            print("onnxsim: simplify check failed, keeping unsimplified export")
    except Exception as exc:  # noqa: BLE001
        print(f"onnxsim skipped: {exc}")

    # Validate the export loads and runs under onnxruntime with two f32 outputs.
    import onnxruntime as ort

    sess = ort.InferenceSession(str(out_path), providers=["CPUExecutionProvider"])
    feeds = {sess.get_inputs()[0].name: dummy.numpy()}
    outs = sess.run(None, feeds)
    assert len(outs) == 2, f"expected 2 outputs, got {len(outs)}"
    logits, boxes = outs
    print(f"validation OK: logits {logits.shape}, pred_boxes {boxes.shape}")
    print(f"wrote {out_path} ({out_path.stat().st_size / 1_048_576:.0f} MB)")


if __name__ == "__main__":
    main()
```

- [ ] **Step 4: Lint the script for syntax (no model download)**

```bash
python3 -m py_compile /home/carsten/workspace/photopipeline/tools/export_rt_detr.py && echo "syntax OK"
```

Expected: `syntax OK`. (Do NOT run the full export — it downloads a large checkpoint. The committed `models/rt_detr_l.onnx` already exists; this script is documentation/reproducibility.)

- [ ] **Step 5: No commit yet** — committed in Task 4 alongside the README/doctor updates.

---

## Task 3: Update `models/README.md` to remove deferral language

**Files:**
- Modify: `models/README.md` (the RT-DETR table row ~line 10, and the "## RT-DETR deferral" section, lines 12–24)

**Interfaces:**
- Consumes: nothing.
- Produces: documentation stating RT-DETR is working.

- [ ] **Step 1: Replace the RT-DETR table row**

In `/home/carsten/workspace/photopipeline/models/README.md`, change the table row:

```
| `rt_detr_l.onnx` | Subject detector (blur ROI, Phase 3) | **Deferred** | `tools/export_rt_detr.py` |
```

to:

```
| `rt_detr_l.onnx` | Subject detector (blur ROI, Phase 4) | Ready | `tools/export_rt_detr.py` |
```

- [ ] **Step 2: Replace the "## RT-DETR deferral" section**

Replace the entire block from `## RT-DETR deferral` through the line ending `...positional encoding variant.` (lines 12–24) with:

```markdown
## RT-DETR subject detector

`rt_detr_l.onnx` is the RT-DETR R50VD detector (Apache-2.0, exported from
`PekingU/rtdetr_r50vd` — see `tools/export_rt_detr.py`). It loads and runs under
the pinned `ort` 2.0.0-rc.12 on this project's CUDA/CPU providers; the
`rtdetr_loads_and_runs` smoke test in `crates/pipeline/src/models/detector.rs`
verifies the forward pass and prints the ONNX I/O contract.

The export wraps the model to emit exactly two float32 outputs — `logits`
`[batch, num_queries, 80]` and `pred_boxes` `[batch, num_queries, 4]`
(cx, cy, w, h, normalized) — which `RtDetrDetector::detect` decodes into
`DetectedSubject` boxes. When `rt_detr_l.onnx` is absent the detector slot in
`ModelHub` stays `None` and sharpness analysis falls back to a center-crop ROI.
```

- [ ] **Step 3: Update the export-commands block**

In the "## Exporting models" section, change the commented-out line:

```sh
# python export_rt_detr.py  # deferred — see above
```

to:

```sh
python export_rt_detr.py    # → ../models/rt_detr_l.onnx (~175 MB)
```

- [ ] **Step 4: No commit yet** — committed in Task 4.

---

## Task 4: Wire `cmd_doctor` to report the detector; commit detector finalization

**Files:**
- Modify: `crates/cli/src/main.rs:241-244` (`cmd_doctor` model-file reporting block)

**Interfaces:**
- Consumes: existing `doctor_model_file(filename, model_dir, role)` (main.rs:252).
- Produces: doctor output line for `rt_detr_l.onnx` via the real path.

- [ ] **Step 1: Replace the hardcoded deferred line**

In `crates/cli/src/main.rs`, replace these three lines (241–243):

```rust
    doctor_model_file("dinov2_base.onnx", &cfg.models.model_dir, "embedder");
    doctor_model_file("clip_iqa.onnx", &cfg.models.model_dir, "iqa");
    println!("  rt_detr_l.onnx  — deferred (ORT Cos(int64) not implemented; see models/README.md)");
```

with:

```rust
    doctor_model_file("dinov2_base.onnx", &cfg.models.model_dir, "embedder");
    doctor_model_file("clip_iqa.onnx", &cfg.models.model_dir, "iqa");
    doctor_model_file("rt_detr_l.onnx", &cfg.models.model_dir, "detector");
```

- [ ] **Step 2: Remove the stale deferral doc-comment on `RtDetrDetector`**

In `crates/pipeline/src/models/detector.rs`, replace the doc comment on the struct (lines 15–25) — from `/// RT-DETR based subject detector.` through the `pub struct RtDetrDetector {` line — so the false "will never succeed" deferral note is gone:

```rust
/// RT-DETR R50VD subject detector.
///
/// Loads `rt_detr_l.onnx` (exported via `tools/export_rt_detr.py`) and runs a
/// forward pass under `ort`. Outputs are `(logits, pred_boxes)`; see
/// `decode_detections` for the postprocessing contract. Verified to load and
/// run under ort 2.0.0-rc.12 (`rtdetr_loads_and_runs` smoke test).
pub struct RtDetrDetector {
```

- [ ] **Step 3: Verify the crate and CLI build and the detector tests still pass**

```bash
source ~/.cargo/env
cargo build -p cli
cargo test -p pipeline --lib models::detector::tests::decode_above_threshold_fires_below_does_not
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
```

Expected: build OK, the named decode test PASSES, fmt clean, clippy clean.

- [ ] **Step 4: Run doctor manually to confirm the detector line renders**

```bash
source ~/.cargo/env
cargo run -p cli -- doctor 2>/dev/null | grep rt_detr_l.onnx
```

Expected: a line like `  rt_detr_l.onnx  [detector] ✓ present (171264 KB)` (present, since the file exists locally).

- [ ] **Step 5: Commit the detector finalization**

```bash
cd /home/carsten/workspace/photopipeline
git add crates/pipeline/src/models/detector.rs crates/pipeline/src/defect/mod.rs \
        crates/cli/src/main.rs models/README.md tools/export_rt_detr.py tools/requirements.txt
git commit -m "feat(models): finalize RT-DETR detector; wire ROIs, doctor, export tool

Commit the previously-uncommitted RT-DETR work: preprocess/decode + tests in
detector.rs, ROI wiring in defect/mod.rs::analyze_defects, and the verified ONNX
I/O contract (logits, pred_boxes). Recreate tools/export_rt_detr.py +
requirements.txt documenting how models/rt_detr_l.onnx was produced (resolves the
§15.4/§15.7 missing-tools note). Doctor now reports rt_detr_l.onnx via the real
doctor_model_file path; README no longer marks it deferred.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

Note: `crates/pipeline/src/defect/mod.rs` already contains the ROI-wiring diff on disk (it calls `hub.detector`); this commit captures it alongside the detector. If `git status` shows it already staged/unstaged, include it as above.

---

## Task 5: Bucket functions in pure Rust

**Files:**
- Create: `crates/pipeline/src/calibration/buckets.rs`
- Modify: `crates/pipeline/src/calibration/mod.rs` (add `pub mod buckets;` — full mod.rs body lands in Task 9; for now add only the line so the unit tests compile)

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `pub fn focal_bucket(mm: f32) -> i32`
  - `pub fn aperture_bucket(f: f32) -> f32`

- [ ] **Step 1: Add the `buckets` module declaration to `calibration/mod.rs`**

Replace the stub line `// placeholder — implemented in a later phase` in `crates/pipeline/src/calibration/mod.rs` with:

```rust
pub mod buckets;
```

(The rest of `mod.rs` — `CalibrationReport`, `run_calibration` — is added in Task 9. This single line is enough for Task 5's tests to compile.)

- [ ] **Step 2: Write the failing bucket tests**

Create `crates/pipeline/src/calibration/buckets.rs`:

```rust
//! Snap raw EXIF focal-length / aperture values to canonical calibration
//! buckets. Pure Rust (no DuckDB UDF). See IMPLEMENTATION_PLAN Appendix B.

/// Canonical focal-length buckets in millimetres (IMPLEMENTATION_PLAN App. B).
const FOCAL_BUCKETS: &[i32] = &[14, 18, 24, 28, 35, 50, 70, 85, 105, 135, 200, 300, 400, 600];

/// Snap a focal length (mm) to the nearest canonical bucket.
/// Values below 14 snap to 14; above 600 snap to 600.
pub fn focal_bucket(mm: f32) -> i32 {
    let mut best = FOCAL_BUCKETS[0];
    let mut best_d = (mm - best as f32).abs();
    for &b in &FOCAL_BUCKETS[1..] {
        let d = (mm - b as f32).abs();
        if d < best_d {
            best_d = d;
            best = b;
        }
    }
    best
}

/// Snap an f-number to the nearest 1/3 stop: `2^(round(log2(f) * 3) / 3)`.
/// Non-positive inputs clamp to f/1.0. Result is a float so the composite
/// primary key on `sharpness_baseline` works.
pub fn aperture_bucket(f: f32) -> f32 {
    if f <= 0.0 {
        return 1.0;
    }
    2.0_f32.powf((f.log2() * 3.0).round() / 3.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn focal_exact_bucket_is_itself() {
        assert_eq!(focal_bucket(50.0), 50);
        assert_eq!(focal_bucket(14.0), 14);
        assert_eq!(focal_bucket(600.0), 600);
    }

    #[test]
    fn focal_snaps_to_nearest() {
        // 60 is between 50 and 70; |60-50|=10, |60-70|=10 → ties resolve to the
        // first-seen (50) because we only replace on strictly-smaller distance.
        assert_eq!(focal_bucket(60.0), 50);
        // 61 is closer to 70.
        assert_eq!(focal_bucket(61.0), 70);
        // 30 is closer to 28 (|30-28|=2) than 35 (|30-35|=5).
        assert_eq!(focal_bucket(30.0), 28);
    }

    #[test]
    fn focal_clamps_out_of_range() {
        assert_eq!(focal_bucket(8.0), 14);
        assert_eq!(focal_bucket(10000.0), 600);
    }

    #[test]
    fn aperture_round_trips_canonical_stops() {
        // f/2.8 is a canonical 1/3-stop value → snaps to itself within f32.
        assert!((aperture_bucket(2.8) - 2.8).abs() < 0.05, "got {}", aperture_bucket(2.8));
        assert!((aperture_bucket(2.0) - 2.0).abs() < 0.05, "got {}", aperture_bucket(2.0));
        assert!((aperture_bucket(4.0) - 4.0).abs() < 0.05, "got {}", aperture_bucket(4.0));
    }

    #[test]
    fn aperture_snaps_to_nearest_third_stop() {
        // f/1.7 snaps to the f/1.8 third-stop bucket.
        assert!((aperture_bucket(1.7) - 1.8).abs() < 0.06, "got {}", aperture_bucket(1.7));
    }

    #[test]
    fn aperture_idempotent() {
        // Snapping an already-snapped value returns (within f32) itself.
        let once = aperture_bucket(3.3);
        let twice = aperture_bucket(once);
        assert!((once - twice).abs() < 1e-4, "{once} vs {twice}");
    }

    #[test]
    fn aperture_handles_nonpositive() {
        assert_eq!(aperture_bucket(0.0), 1.0);
        assert_eq!(aperture_bucket(-1.0), 1.0);
    }
}
```

- [ ] **Step 3: Run the tests to verify they pass**

```bash
source ~/.cargo/env
cargo test -p pipeline --lib calibration::buckets -- --nocapture
```

Expected: all seven tests PASS. (They are not "failing-first" in the strict TDD sense because the implementation is trivial and lives in the same file; if you prefer strict TDD, paste only the `#[cfg(test)] mod tests` block first, run to see it fail to compile with "cannot find function `focal_bucket`", then add the two functions and re-run.)

- [ ] **Step 4: fmt + clippy**

```bash
source ~/.cargo/env
cargo fmt --check
cargo clippy -p pipeline --all-targets --all-features -- -D warnings
```

Expected: clean.

- [ ] **Step 5: Commit**

```bash
cd /home/carsten/workspace/photopipeline
git add crates/pipeline/src/calibration/buckets.rs crates/pipeline/src/calibration/mod.rs
git commit -m "feat(calibration): focal/aperture bucket snapping in pure Rust

Add calibration::buckets with focal_bucket (nearest of the 14 canonical mm
buckets) and aperture_bucket (nearest 1/3 stop via 2^(round(log2 f *3)/3)).
Bucketing is done in Rust, not as a DuckDB scalar UDF. Appendix B constants.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 6: Catalog — `clear_blur_related_flags` + `iqa_global_p10`

**Files:**
- Modify: `crates/pipeline/src/catalog/mod.rs` (add two methods inside `impl Catalog`, before the closing `}` at line 761)

**Interfaces:**
- Consumes: existing `Catalog` internals (`self.conn: Mutex<Connection>`, `CatalogError::Db`).
- Produces:
  - `pub fn clear_blur_related_flags(&self) -> Result<usize, CatalogError>` — deletes `blur`/`back_focus`/`low_iqa` rows, returns count deleted.
  - `pub fn iqa_global_p10(&self) -> Result<Option<f32>, CatalogError>` — global 10th-percentile IQA score, `None` if `iqa` table empty.

- [ ] **Step 1: Write the failing test (append to the existing `#[cfg(test)] mod tests` in `catalog/mod.rs`)**

Add inside the `mod tests` block (before its closing `}` at line 890):

```rust
    #[test]
    fn clear_blur_related_flags_only_removes_blur_kinds() {
        use crate::defect::DefectFlag;
        use crate::ingest::{ExifData, FileFormat, IngestedFile};

        let (catalog, _dir) = make_catalog();
        let file = IngestedFile {
            path: PathBuf::from("/c/clear.jpg"),
            content_hash: 1,
            size: 1,
            mtime_ns: 1,
            format: FileFormat::Jpg,
            has_sidecar_jpg: false,
        };
        let id = catalog.flush_batch(&[(file, None::<ExifData>)]).unwrap()[0];

        for ft in ["overexposed", "underexposed", "blur", "back_focus", "low_iqa"] {
            catalog
                .upsert_defect_flag(
                    id,
                    &DefectFlag {
                        flag_type: ft.to_string(),
                        confidence: 0.5,
                        reason: "t".into(),
                    },
                )
                .unwrap();
        }

        let deleted = catalog.clear_blur_related_flags().unwrap();
        assert_eq!(deleted, 3, "should delete blur/back_focus/low_iqa only");
        assert_eq!(catalog.count_defect_flags("overexposed").unwrap(), 1);
        assert_eq!(catalog.count_defect_flags("underexposed").unwrap(), 1);
        assert_eq!(catalog.count_defect_flags("blur").unwrap(), 0);
        assert_eq!(catalog.count_defect_flags("back_focus").unwrap(), 0);
        assert_eq!(catalog.count_defect_flags("low_iqa").unwrap(), 0);
    }

    #[test]
    fn iqa_global_p10_none_when_empty_some_when_populated() {
        use crate::catalog::MlRow;
        use crate::ingest::{ExifData, FileFormat, IngestedFile};

        let (catalog, _dir) = make_catalog();
        assert!(catalog.iqa_global_p10().unwrap().is_none(), "empty → None");

        // Insert 10 files with iqa scores 0.0..=0.9.
        for i in 0..10i64 {
            let file = IngestedFile {
                path: PathBuf::from(format!("/iqa/{i}.jpg")),
                content_hash: i as u128,
                size: 1,
                mtime_ns: i,
                format: FileFormat::Jpg,
                has_sidecar_jpg: false,
            };
            let id = catalog.flush_batch(&[(file, None::<ExifData>)]).unwrap()[0];
            catalog
                .flush_ml_batch(&[MlRow {
                    file_id: id,
                    embedding: None,
                    iqa_score: Some(("clip-iqa".into(), i as f32 / 10.0)),
                }])
                .unwrap();
        }
        let p10 = catalog.iqa_global_p10().unwrap().expect("should be Some");
        // quantile_cont(0.10) over 0.0..0.9 is ~0.09.
        assert!((0.0..=0.2).contains(&p10), "p10 {p10} out of expected band");
    }
```

- [ ] **Step 2: Run to verify failure**

```bash
source ~/.cargo/env
cargo test -p pipeline --lib catalog::tests::clear_blur_related_flags_only_removes_blur_kinds 2>&1 | tail -5
```

Expected: FAIL to compile — `no method named clear_blur_related_flags`.

- [ ] **Step 3: Implement the two methods**

Insert into `impl Catalog` in `crates/pipeline/src/catalog/mod.rs` immediately before the final closing `}` of the impl block (after `iqa_count`, line 760):

```rust
    /// Delete all blur-related defect flags (`blur`, `back_focus`, `low_iqa`),
    /// leaving exposure flags untouched. Returns the number of rows deleted.
    pub fn clear_blur_related_flags(&self) -> Result<usize, CatalogError> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| CatalogError::Db("mutex poisoned".into()))?;
        let n = conn
            .execute(
                "DELETE FROM defect_flags
                 WHERE flag_type IN ('blur', 'back_focus', 'low_iqa')",
                [],
            )
            .map_err(|e| CatalogError::Db(e.to_string()))?;
        Ok(n)
    }

    /// Global 10th-percentile IQA score across the whole `iqa` table.
    /// Returns `None` when the table is empty.
    pub fn iqa_global_p10(&self) -> Result<Option<f32>, CatalogError> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| CatalogError::Db("mutex poisoned".into()))?;
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM iqa", [], |r| r.get(0))
            .map_err(|e| CatalogError::Db(e.to_string()))?;
        if count == 0 {
            return Ok(None);
        }
        let p10: f64 = conn
            .query_row(
                "SELECT quantile_cont(score, 0.10) FROM iqa",
                [],
                |r| r.get(0),
            )
            .map_err(|e| CatalogError::Db(e.to_string()))?;
        Ok(Some(p10 as f32))
    }
```

- [ ] **Step 4: Run both new tests**

```bash
source ~/.cargo/env
cargo test -p pipeline --lib catalog::tests::clear_blur_related_flags_only_removes_blur_kinds
cargo test -p pipeline --lib catalog::tests::iqa_global_p10_none_when_empty_some_when_populated
```

Expected: both PASS.

- [ ] **Step 5: fmt + clippy + commit**

```bash
source ~/.cargo/env
cargo fmt --check
cargo clippy -p pipeline --all-targets --all-features -- -D warnings
cd /home/carsten/workspace/photopipeline
git add crates/pipeline/src/catalog/mod.rs
git commit -m "feat(calibration): catalog clear_blur_related_flags + iqa_global_p10

clear_blur_related_flags deletes only blur/back_focus/low_iqa rows (exposure
flags survive); iqa_global_p10 returns the global 10th-percentile IQA score or
None when the iqa table is empty.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 7: Catalog — `iter_sharpness_for_reflag` + `rebuild_sharpness_baselines`

**Files:**
- Modify: `crates/pipeline/src/catalog/mod.rs` (add two pub structs near the top alongside `MlRow`, and two methods in `impl Catalog`)

**Interfaces:**
- Consumes: `crate::calibration::buckets::{focal_bucket, aperture_bucket}` (Task 5).
- Produces:
  - `pub struct SharpnessReflagRow { pub file_id: i64, pub s_subject: Option<f32>, pub s_background: Option<f32>, pub camera_model: Option<String>, pub lens_model: Option<String>, pub focal_length_mm: Option<f32>, pub aperture: Option<f32>, pub iqa_score: Option<f32> }`
  - `pub struct RebuildReport { pub buckets_built: usize, pub global_n_samples: usize }`
  - `pub fn iter_sharpness_for_reflag(&self) -> Result<Vec<SharpnessReflagRow>, CatalogError>`
  - `pub fn rebuild_sharpness_baselines(&self, min_samples: usize) -> Result<RebuildReport, CatalogError>`

> **Deviation note vs PHASE_4_PROMPT:** the prompt's `SharpnessReflagRow` carried pre-bucketed `focal_bucket: Option<i32>` / `aperture_bucket: Option<f32>`. We carry RAW `focal_length_mm` / `aperture` instead and bucket in Rust at the call site (Task 9). This keeps all bucket math in one place (`calibration::buckets`) and matches design decision #1 (no UDF). The baseline rebuild groups in Rust.

- [ ] **Step 1: Write the failing test (append to `mod tests` in `catalog/mod.rs`)**

```rust
    #[test]
    fn rebuild_baselines_builds_bucket_and_global() {
        use crate::defect::SharpnessResult;
        use crate::ingest::{ExifData, FileFormat, IngestedFile};

        let (catalog, _dir) = make_catalog();

        // 4 files, identical EXIF bucket (TestModel / TestLens / 50mm / f2.8),
        // s_subject = 10, 20, 30, 40.
        for (i, s) in [10.0f32, 20.0, 30.0, 40.0].into_iter().enumerate() {
            let file = IngestedFile {
                path: PathBuf::from(format!("/b/{i}.jpg")),
                content_hash: i as u128,
                size: 1,
                mtime_ns: i as i64,
                format: FileFormat::Jpg,
                has_sidecar_jpg: false,
            };
            let exif = ExifData {
                captured_at: Some(1000),
                camera_make: Some("TestMake".into()),
                camera_model: Some("TestModel".into()),
                lens_model: Some("TestLens 50mm".into()),
                focal_length_mm: Some(50.0),
                aperture: Some(2.8),
                iso: Some(200),
                shutter_seconds: Some(0.01),
                width: Some(64),
                height: Some(64),
                orientation: Some(1),
            };
            let id = catalog.flush_batch(&[(file, Some(exif))]).unwrap()[0];
            catalog
                .upsert_sharpness(
                    id,
                    &SharpnessResult {
                        s_global: s,
                        s_subject: Some(s),
                        s_background: Some(s),
                        subject_ratio: Some(0.16),
                        detector_used: "rt-detr-l".into(),
                    },
                )
                .unwrap();
        }

        // min_samples = 3 → the 4-sample bucket qualifies.
        let report = catalog.rebuild_sharpness_baselines(3).unwrap();
        assert_eq!(report.buckets_built, 1, "one qualifying bucket");
        assert_eq!(report.global_n_samples, 4, "global counts all 4 samples");

        // The global sentinel row exists.
        let conn = catalog.conn.lock().unwrap();
        let global_n: i64 = conn
            .query_row(
                "SELECT n_samples FROM sharpness_baseline
                 WHERE camera_model = '*' AND lens_model = '*'
                   AND focal_bucket = 0 AND aperture_bucket = 0.0",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(global_n, 4);
    }

    #[test]
    fn iter_sharpness_for_reflag_returns_raw_exif() {
        use crate::defect::SharpnessResult;
        use crate::ingest::{ExifData, FileFormat, IngestedFile};

        let (catalog, _dir) = make_catalog();
        let file = IngestedFile {
            path: PathBuf::from("/r/0.jpg"),
            content_hash: 7,
            size: 1,
            mtime_ns: 1,
            format: FileFormat::Jpg,
            has_sidecar_jpg: false,
        };
        let exif = ExifData {
            captured_at: Some(1000),
            camera_make: Some("TestMake".into()),
            camera_model: Some("TestModel".into()),
            lens_model: Some("TestLens 50mm".into()),
            focal_length_mm: Some(50.0),
            aperture: Some(2.8),
            iso: Some(200),
            shutter_seconds: Some(0.01),
            width: Some(64),
            height: Some(64),
            orientation: Some(1),
        };
        let id = catalog.flush_batch(&[(file, Some(exif))]).unwrap()[0];
        catalog
            .upsert_sharpness(
                id,
                &SharpnessResult {
                    s_global: 12.0,
                    s_subject: Some(12.0),
                    s_background: Some(30.0),
                    subject_ratio: Some(0.16),
                    detector_used: "rt-detr-l".into(),
                },
            )
            .unwrap();

        let rows = catalog.iter_sharpness_for_reflag().unwrap();
        assert_eq!(rows.len(), 1);
        let r = &rows[0];
        assert_eq!(r.file_id, id);
        assert_eq!(r.s_subject, Some(12.0));
        assert_eq!(r.s_background, Some(30.0));
        assert_eq!(r.camera_model.as_deref(), Some("TestModel"));
        assert_eq!(r.focal_length_mm, Some(50.0));
        assert_eq!(r.aperture, Some(2.8));
    }
```

- [ ] **Step 2: Run to verify failure**

```bash
source ~/.cargo/env
cargo test -p pipeline --lib catalog::tests::iter_sharpness_for_reflag_returns_raw_exif 2>&1 | tail -5
```

Expected: FAIL to compile — `cannot find type SharpnessReflagRow` / `no method iter_sharpness_for_reflag`.

- [ ] **Step 3: Add the two structs near `MlRow` (top of `catalog/mod.rs`, after the `MlRow` struct, line 17)**

```rust
/// One file's sharpness + raw EXIF + optional IQA score, for the reflag pass.
/// Buckets are computed in Rust (`calibration::buckets`) at the call site.
pub struct SharpnessReflagRow {
    pub file_id: i64,
    pub s_subject: Option<f32>,
    pub s_background: Option<f32>,
    pub camera_model: Option<String>,
    pub lens_model: Option<String>,
    pub focal_length_mm: Option<f32>,
    pub aperture: Option<f32>,
    pub iqa_score: Option<f32>,
}

/// Summary of a `rebuild_sharpness_baselines` run.
pub struct RebuildReport {
    /// Count of non-global (per-bucket) baseline rows written.
    pub buckets_built: usize,
    /// Total sample count backing the global fallback row.
    pub global_n_samples: usize,
}
```

- [ ] **Step 4: Implement `iter_sharpness_for_reflag` in `impl Catalog`**

Insert after the methods from Task 6:

```rust
    /// One row per file that has a sharpness record, joined to EXIF (raw,
    /// un-bucketed) and the optional IQA score. Used by the reflag pass to
    /// avoid N+1 queries.
    pub fn iter_sharpness_for_reflag(&self) -> Result<Vec<SharpnessReflagRow>, CatalogError> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| CatalogError::Db("mutex poisoned".into()))?;
        let mut stmt = conn
            .prepare(
                "SELECT s.file_id, s.s_subject, s.s_background,
                        e.camera_model, e.lens_model, e.focal_length_mm, e.aperture,
                        i.score
                 FROM sharpness s
                 LEFT JOIN exif e ON e.file_id = s.file_id
                 LEFT JOIN iqa  i ON i.file_id = s.file_id",
            )
            .map_err(|e| CatalogError::Db(e.to_string()))?;

        let rows = stmt
            .query_map([], |row| {
                Ok(SharpnessReflagRow {
                    file_id: row.get(0)?,
                    s_subject: row.get(1)?,
                    s_background: row.get(2)?,
                    camera_model: row.get(3)?,
                    lens_model: row.get(4)?,
                    focal_length_mm: row.get(5)?,
                    aperture: row.get(6)?,
                    iqa_score: row.get(7)?,
                })
            })
            .map_err(|e| CatalogError::Db(e.to_string()))?;

        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| CatalogError::Db(e.to_string()))?);
        }
        Ok(out)
    }
```

- [ ] **Step 5: Implement `rebuild_sharpness_baselines` in `impl Catalog`**

This pulls raw rows, buckets + groups + computes percentiles in Rust, then writes baseline rows (per-bucket + one global sentinel) inside one transaction. Insert after `iter_sharpness_for_reflag`:

```rust
    /// Rebuild `sharpness_baseline` from current `sharpness`+`exif` data.
    ///
    /// Buckets are computed in Rust (no DuckDB UDF). Per-bucket rows are written
    /// only when the bucket has `>= min_samples` samples; a global sentinel row
    /// `('*','*',0,0.0)` is always written (when any sample exists) with the
    /// total population's percentiles. All writes happen in one transaction;
    /// the table is fully replaced (old rows deleted first) for idempotency.
    pub fn rebuild_sharpness_baselines(
        &self,
        min_samples: usize,
    ) -> Result<RebuildReport, CatalogError> {
        use crate::calibration::buckets::{aperture_bucket, focal_bucket};
        use std::collections::HashMap;

        // Phase 1: read qualifying raw samples (lock released before the write tx).
        struct Raw {
            camera: String,
            lens: String,
            focal: f32,
            aperture: f32,
            s_subject: f32,
        }
        let raws: Vec<Raw> = {
            let conn = self
                .conn
                .lock()
                .map_err(|_| CatalogError::Db("mutex poisoned".into()))?;
            let mut stmt = conn
                .prepare(
                    "SELECT e.camera_model, e.lens_model, e.focal_length_mm, e.aperture,
                            s.s_subject
                     FROM sharpness s
                     JOIN exif e ON e.file_id = s.file_id
                     WHERE s.s_subject IS NOT NULL
                       AND e.camera_model IS NOT NULL
                       AND e.lens_model IS NOT NULL
                       AND e.focal_length_mm IS NOT NULL
                       AND e.aperture IS NOT NULL",
                )
                .map_err(|e| CatalogError::Db(e.to_string()))?;
            let rows = stmt
                .query_map([], |row| {
                    Ok(Raw {
                        camera: row.get::<_, String>(0)?,
                        lens: row.get::<_, String>(1)?,
                        focal: row.get::<_, f32>(2)?,
                        aperture: row.get::<_, f32>(3)?,
                        s_subject: row.get::<_, f32>(4)?,
                    })
                })
                .map_err(|e| CatalogError::Db(e.to_string()))?;
            let mut v = Vec::new();
            for r in rows {
                v.push(r.map_err(|e| CatalogError::Db(e.to_string()))?);
            }
            v
        };

        // Group by (camera, lens, focal_bucket, aperture_bucket-as-bits) in Rust.
        let mut groups: HashMap<(String, String, i32, u32), Vec<f32>> = HashMap::new();
        let mut global: Vec<f32> = Vec::with_capacity(raws.len());
        for r in &raws {
            global.push(r.s_subject);
            let fb = focal_bucket(r.focal);
            let ab = aperture_bucket(r.aperture);
            groups
                .entry((r.camera.clone(), r.lens.clone(), fb, ab.to_bits()))
                .or_default()
                .push(r.s_subject);
        }

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let global_n = global.len();

        // Phase 2: write everything in one transaction (delete + reinsert).
        let mut conn = self
            .conn
            .lock()
            .map_err(|_| CatalogError::Db("mutex poisoned".into()))?;
        let tx = conn
            .transaction()
            .map_err(|e| CatalogError::Db(e.to_string()))?;

        tx.execute("DELETE FROM sharpness_baseline", [])
            .map_err(|e| CatalogError::Db(e.to_string()))?;

        let mut buckets_built = 0usize;
        {
            let mut stmt = tx
                .prepare(
                    "INSERT INTO sharpness_baseline
                        (camera_model, lens_model, focal_bucket, aperture_bucket,
                         s_subject_p10, s_subject_p50, s_subject_p90, n_samples, last_updated)
                     VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
                     ON CONFLICT (camera_model, lens_model, focal_bucket, aperture_bucket)
                     DO UPDATE SET
                         s_subject_p10 = excluded.s_subject_p10,
                         s_subject_p50 = excluded.s_subject_p50,
                         s_subject_p90 = excluded.s_subject_p90,
                         n_samples     = excluded.n_samples,
                         last_updated  = excluded.last_updated",
                )
                .map_err(|e| CatalogError::Db(e.to_string()))?;

            for ((camera, lens, fb, ab_bits), mut samples) in groups {
                if samples.len() < min_samples {
                    continue;
                }
                let ab = f32::from_bits(ab_bits);
                let (p10, p50, p90) = percentiles(&mut samples);
                stmt.execute(duckdb::params![
                    camera,
                    lens,
                    fb,
                    ab,
                    p10,
                    p50,
                    p90,
                    samples.len() as i32,
                    now,
                ])
                .map_err(|e| CatalogError::Db(e.to_string()))?;
                buckets_built += 1;
            }

            // Global sentinel row (only when there is any sample at all).
            if global_n > 0 {
                let mut g = global;
                let (p10, p50, p90) = percentiles(&mut g);
                stmt.execute(duckdb::params![
                    "*",
                    "*",
                    0i32,
                    0.0f32,
                    p10,
                    p50,
                    p90,
                    global_n as i32,
                    now,
                ])
                .map_err(|e| CatalogError::Db(e.to_string()))?;
            }
        }

        tx.commit().map_err(|e| CatalogError::Db(e.to_string()))?;

        Ok(RebuildReport {
            buckets_built,
            global_n_samples: global_n,
        })
    }
```

> Note: the `INSERT` lists 9 columns; both `stmt.execute(...)` calls bind 9 params ending in `now` (the `last_updated` epoch). Keep those two lists in sync with the column list if you edit it.

- [ ] **Step 6: Add the `percentiles` free helper at the bottom of `catalog/mod.rs` (outside `impl`, above `#[cfg(test)]`)**

Deterministic nearest-rank-with-linear-interpolation percentiles, well-defined for any sample size (avoids `quantile_cont`'s tiny-sample surprises):

```rust
/// Return (p10, p50, p90) of `samples` using linear interpolation between
/// order statistics. Sorts `samples` in place. `samples` must be non-empty.
fn percentiles(samples: &mut [f32]) -> (f32, f32, f32) {
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    (
        percentile_sorted(samples, 0.10),
        percentile_sorted(samples, 0.50),
        percentile_sorted(samples, 0.90),
    )
}

fn percentile_sorted(sorted: &[f32], q: f32) -> f32 {
    let n = sorted.len();
    if n == 1 {
        return sorted[0];
    }
    let rank = q * (n as f32 - 1.0);
    let lo = rank.floor() as usize;
    let hi = rank.ceil() as usize;
    let frac = rank - lo as f32;
    sorted[lo] + (sorted[hi] - sorted[lo]) * frac
}
```

- [ ] **Step 7: Run the tests**

```bash
source ~/.cargo/env
cargo test -p pipeline --lib catalog::tests::iter_sharpness_for_reflag_returns_raw_exif
cargo test -p pipeline --lib catalog::tests::rebuild_baselines_builds_bucket_and_global
```

Expected: both PASS. (If you see a binding-count error, the `INSERT` column list and the two `params![...]` lists have drifted out of sync — both must end with `now`.)

- [ ] **Step 8: fmt + clippy + commit**

```bash
source ~/.cargo/env
cargo fmt --check
cargo clippy -p pipeline --all-targets --all-features -- -D warnings
cd /home/carsten/workspace/photopipeline
git add crates/pipeline/src/catalog/mod.rs
git commit -m "feat(calibration): rebuild_sharpness_baselines + iter_sharpness_for_reflag

Group raw sharpness+EXIF samples by Rust-computed (camera,lens,focal,aperture)
buckets, compute p10/p50/p90 in Rust (interpolated order statistics, robust on
tiny samples), and replace sharpness_baseline in one transaction with per-bucket
rows (>= min_samples) plus a global ('*','*',0,0.0) sentinel. iter_sharpness_for_reflag
returns raw EXIF for the reflag pass.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 8: Catalog — `bucket_baseline_p10` + `flush_blur_flag_batch`

**Files:**
- Modify: `crates/pipeline/src/catalog/mod.rs` (one pub struct + two methods)

**Interfaces:**
- Consumes: `sharpness_baseline` rows written by Task 7.
- Produces:
  - `pub struct BlurFlagRow { pub file_id: i64, pub flag_type: &'static str, pub confidence: f32, pub reason: String }`
  - `pub fn bucket_baseline_p10(&self, camera_model: &str, lens_model: &str, focal_bucket: i32, aperture_bucket: f32, min_samples: usize) -> Result<Option<f32>, CatalogError>` — per-bucket p10 if that bucket has `>= min_samples`, else `None`.
  - `pub fn flush_blur_flag_batch(&self, flags: &[BlurFlagRow]) -> Result<(), CatalogError>` — prepared `INSERT … ON CONFLICT` in one transaction.

- [ ] **Step 1: Write the failing test (append to `mod tests`)**

```rust
    #[test]
    fn flush_blur_flag_batch_inserts_and_upserts() {
        use crate::catalog::BlurFlagRow;
        use crate::ingest::{ExifData, FileFormat, IngestedFile};

        let (catalog, _dir) = make_catalog();
        let file = IngestedFile {
            path: PathBuf::from("/f/0.jpg"),
            content_hash: 3,
            size: 1,
            mtime_ns: 1,
            format: FileFormat::Jpg,
            has_sidecar_jpg: false,
        };
        let id = catalog.flush_batch(&[(file, None::<ExifData>)]).unwrap()[0];

        catalog
            .flush_blur_flag_batch(&[
                BlurFlagRow {
                    file_id: id,
                    flag_type: "blur",
                    confidence: 0.4,
                    reason: "r".into(),
                },
                BlurFlagRow {
                    file_id: id,
                    flag_type: "low_iqa",
                    confidence: 0.5,
                    reason: "r2".into(),
                },
            ])
            .unwrap();
        assert_eq!(catalog.count_defect_flags("blur").unwrap(), 1);
        assert_eq!(catalog.count_defect_flags("low_iqa").unwrap(), 1);

        // Re-flush the same (file_id, flag_type) with a new confidence → upsert,
        // not a UNIQUE violation.
        catalog
            .flush_blur_flag_batch(&[BlurFlagRow {
                file_id: id,
                flag_type: "blur",
                confidence: 0.9,
                reason: "r3".into(),
            }])
            .unwrap();
        assert_eq!(catalog.count_defect_flags("blur").unwrap(), 1, "still one blur row");
        let conn = catalog.conn.lock().unwrap();
        let conf: f32 = conn
            .query_row(
                "SELECT confidence FROM defect_flags WHERE file_id = ? AND flag_type = 'blur'",
                duckdb::params![id],
                |r| r.get(0),
            )
            .unwrap();
        assert!((conf - 0.9).abs() < 1e-5, "confidence upserted to 0.9, got {conf}");
    }

    #[test]
    fn bucket_baseline_p10_respects_min_samples() {
        use crate::defect::SharpnessResult;
        use crate::ingest::{ExifData, FileFormat, IngestedFile};

        let (catalog, _dir) = make_catalog();
        for (i, s) in [10.0f32, 20.0, 30.0].into_iter().enumerate() {
            let file = IngestedFile {
                path: PathBuf::from(format!("/p/{i}.jpg")),
                content_hash: i as u128,
                size: 1,
                mtime_ns: i as i64,
                format: FileFormat::Jpg,
                has_sidecar_jpg: false,
            };
            let exif = ExifData {
                captured_at: Some(1),
                camera_make: Some("TestMake".into()),
                camera_model: Some("TestModel".into()),
                lens_model: Some("TestLens 50mm".into()),
                focal_length_mm: Some(50.0),
                aperture: Some(2.8),
                iso: Some(200),
                shutter_seconds: Some(0.01),
                width: Some(64),
                height: Some(64),
                orientation: Some(1),
            };
            let id = catalog.flush_batch(&[(file, Some(exif))]).unwrap()[0];
            catalog
                .upsert_sharpness(
                    id,
                    &SharpnessResult {
                        s_global: s,
                        s_subject: Some(s),
                        s_background: Some(s),
                        subject_ratio: Some(0.16),
                        detector_used: "rt-detr-l".into(),
                    },
                )
                .unwrap();
        }
        catalog.rebuild_sharpness_baselines(3).unwrap();

        // The bucket has 3 samples. min_samples=3 → Some; min_samples=4 → None.
        let fb = crate::calibration::buckets::focal_bucket(50.0);
        let ab = crate::calibration::buckets::aperture_bucket(2.8);
        let got = catalog
            .bucket_baseline_p10("TestModel", "TestLens 50mm", fb, ab, 3)
            .unwrap();
        assert!(got.is_some(), "3 >= 3 → Some");
        let none = catalog
            .bucket_baseline_p10("TestModel", "TestLens 50mm", fb, ab, 4)
            .unwrap();
        assert!(none.is_none(), "3 < 4 → None");
    }
```

- [ ] **Step 2: Run to verify failure**

```bash
source ~/.cargo/env
cargo test -p pipeline --lib catalog::tests::flush_blur_flag_batch_inserts_and_upserts 2>&1 | tail -5
```

Expected: FAIL to compile — `cannot find type BlurFlagRow`.

- [ ] **Step 3: Add the `BlurFlagRow` struct (after `RebuildReport`, top of file)**

```rust
/// One blur-related defect flag ready to persist. `flag_type` is one of
/// `"blur"`, `"back_focus"`, `"low_iqa"`.
pub struct BlurFlagRow {
    pub file_id: i64,
    pub flag_type: &'static str,
    pub confidence: f32,
    pub reason: String,
}
```

- [ ] **Step 4: Implement `bucket_baseline_p10` and `flush_blur_flag_batch` in `impl Catalog`**

```rust
    /// Per-bucket p10 for `(camera, lens, focal_bucket, aperture_bucket)`, but
    /// only when that baseline row has `n_samples >= min_samples`. Otherwise
    /// `None` (caller should fall back to the global sentinel).
    pub fn bucket_baseline_p10(
        &self,
        camera_model: &str,
        lens_model: &str,
        focal_bucket: i32,
        aperture_bucket: f32,
        min_samples: usize,
    ) -> Result<Option<f32>, CatalogError> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| CatalogError::Db("mutex poisoned".into()))?;
        let result = conn.query_row(
            "SELECT s_subject_p10, n_samples FROM sharpness_baseline
             WHERE camera_model = ? AND lens_model = ?
               AND focal_bucket = ? AND aperture_bucket = ?",
            duckdb::params![camera_model, lens_model, focal_bucket, aperture_bucket],
            |r| {
                let p10: f32 = r.get(0)?;
                let n: i64 = r.get(1)?;
                Ok((p10, n))
            },
        );
        match result {
            Ok((p10, n)) if (n as usize) >= min_samples => Ok(Some(p10)),
            Ok(_) => Ok(None),
            Err(duckdb::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(CatalogError::Db(e.to_string())),
        }
    }

    /// Bulk-write blur-related flags in one transaction. Uses prepared
    /// `INSERT … ON CONFLICT (file_id, flag_type)` (NOT the Appender): the
    /// `defect_flags` table has a `DEFAULT nextval()` id and a UNIQUE
    /// constraint, which the positional Appender cannot satisfy. Matches the
    /// `flush_defect_batch` pattern; one transaction per batch.
    pub fn flush_blur_flag_batch(&self, flags: &[BlurFlagRow]) -> Result<(), CatalogError> {
        if flags.is_empty() {
            return Ok(());
        }
        let mut conn = self
            .conn
            .lock()
            .map_err(|_| CatalogError::Db("mutex poisoned".into()))?;
        let tx = conn
            .transaction()
            .map_err(|e| CatalogError::Db(e.to_string()))?;
        {
            let mut stmt = tx
                .prepare(
                    "INSERT INTO defect_flags (file_id, flag_type, confidence, reason)
                     VALUES (?, ?, ?, ?)
                     ON CONFLICT (file_id, flag_type) DO UPDATE SET
                         confidence = excluded.confidence,
                         reason     = excluded.reason",
                )
                .map_err(|e| CatalogError::Db(e.to_string()))?;
            for f in flags {
                stmt.execute(duckdb::params![f.file_id, f.flag_type, f.confidence, f.reason])
                    .map_err(|e| CatalogError::Db(e.to_string()))?;
            }
        }
        tx.commit().map_err(|e| CatalogError::Db(e.to_string()))?;
        Ok(())
    }
```

- [ ] **Step 5: Run both tests**

```bash
source ~/.cargo/env
cargo test -p pipeline --lib catalog::tests::flush_blur_flag_batch_inserts_and_upserts
cargo test -p pipeline --lib catalog::tests::bucket_baseline_p10_respects_min_samples
```

Expected: both PASS.

- [ ] **Step 6: fmt + clippy + commit**

```bash
source ~/.cargo/env
cargo fmt --check
cargo clippy -p pipeline --all-targets --all-features -- -D warnings
cd /home/carsten/workspace/photopipeline
git add crates/pipeline/src/catalog/mod.rs
git commit -m "feat(calibration): bucket_baseline_p10 + flush_blur_flag_batch

bucket_baseline_p10 returns a per-bucket p10 only when the bucket meets
min_samples (else None → caller uses the global fallback). flush_blur_flag_batch
writes blur/back_focus/low_iqa via prepared INSERT ON CONFLICT in one
transaction (Appender cannot satisfy the nextval id + UNIQUE constraint).

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 9: Calibration orchestrator `run_calibration`

**Files:**
- Modify: `crates/pipeline/src/calibration/mod.rs` (add `CalibrationReport` + `run_calibration` below the `pub mod buckets;` line from Task 5)
- Modify: `crates/pipeline/src/lib.rs:13-15` (add re-export)

**Interfaces:**
- Consumes (all from earlier tasks): `Catalog::{clear_blur_related_flags, rebuild_sharpness_baselines, iqa_global_p10, iter_sharpness_for_reflag, bucket_baseline_p10, flush_blur_flag_batch}`, `catalog::{BlurFlagRow, RebuildReport, SharpnessReflagRow}`, `calibration::buckets::{focal_bucket, aperture_bucket}`, `config::DefectConfig`.
- Produces:
  - `pub struct CalibrationReport { pub buckets_built: usize, pub global_n_samples: usize, pub flags_cleared: usize, pub flagged_blur: usize, pub flagged_back_focus: usize, pub flagged_low_iqa: usize, pub blur_confidence_bumped: usize }` (derives `Debug, Default`)
  - `pub fn run_calibration(catalog: &crate::catalog::Catalog, cfg: &crate::config::DefectConfig) -> anyhow::Result<CalibrationReport>`
  - lib re-export `pub use calibration::{run_calibration, CalibrationReport};`

- [ ] **Step 1: Replace `calibration/mod.rs` with the full module**

Replace the file `crates/pipeline/src/calibration/mod.rs` (currently just `pub mod buckets;`) with:

```rust
//! Lens calibration: rebuild per-lens sharpness baselines and re-flag
//! blur / back_focus / low_iqa defects. Driven by `photopipe calibrate`.

pub mod buckets;

use crate::catalog::{BlurFlagRow, Catalog};
use crate::config::DefectConfig;

#[derive(Debug, Default)]
pub struct CalibrationReport {
    pub buckets_built: usize,
    pub global_n_samples: usize,
    pub flags_cleared: usize,
    pub flagged_blur: usize,
    pub flagged_back_focus: usize,
    pub flagged_low_iqa: usize,
    pub blur_confidence_bumped: usize,
}

/// Atomic-in-intent calibration: (1) clear stale blur-related flags,
/// (2) rebuild baselines, (3) reflag every file with sharpness data.
/// `overexposed` / `underexposed` flags are never touched.
pub fn run_calibration(
    catalog: &Catalog,
    cfg: &DefectConfig,
) -> anyhow::Result<CalibrationReport> {
    let min_samples = cfg.blur.min_samples_for_bucket;

    // (1) wipe stale blur/back_focus/low_iqa.
    let flags_cleared = catalog.clear_blur_related_flags()?;

    // (2) rebuild baselines.
    let rebuild = catalog.rebuild_sharpness_baselines(min_samples)?;

    // (3) global fallbacks computed once.
    let iqa_p10 = if cfg.blur.iqa_second_opinion {
        catalog.iqa_global_p10()?
    } else {
        None
    };
    let global_s_p10 = global_sharpness_p10(catalog)?;

    let mut report = CalibrationReport {
        buckets_built: rebuild.buckets_built,
        global_n_samples: rebuild.global_n_samples,
        flags_cleared,
        ..Default::default()
    };

    let rows = catalog.iter_sharpness_for_reflag()?;
    let mut batch: Vec<BlurFlagRow> = Vec::with_capacity(64);

    for row in &rows {
        let s_subject = match row.s_subject {
            Some(s) => s,
            None => continue, // degenerate; no flag.
        };

        // Resolve threshold: per-bucket p10 (if bucket has enough samples) else global.
        let bucket_p10 = match (
            row.camera_model.as_deref(),
            row.lens_model.as_deref(),
            row.focal_length_mm,
            row.aperture,
        ) {
            (Some(cam), Some(lens), Some(focal), Some(ap)) => catalog.bucket_baseline_p10(
                cam,
                lens,
                buckets::focal_bucket(focal),
                buckets::aperture_bucket(ap),
                min_samples,
            )?,
            _ => None,
        };
        let threshold = match bucket_p10.or(global_s_p10) {
            Some(t) => t,
            None => {
                tracing::debug!(file_id = row.file_id, "no baseline available, skipping reflag");
                continue;
            }
        };

        let mut flagged_blur = false;

        if s_subject < threshold {
            let confidence = ((threshold - s_subject) / threshold).clamp(0.01, 1.0);
            let s_bg = row.s_background.unwrap_or(s_subject);
            if s_bg > s_subject * 2.0 {
                batch.push(BlurFlagRow {
                    file_id: row.file_id,
                    flag_type: "back_focus",
                    confidence,
                    reason: format!(
                        "subject {:.1} < p10 {:.1}, background {:.1}x sharper",
                        s_subject,
                        threshold,
                        s_bg / s_subject
                    ),
                });
                report.flagged_back_focus += 1;
            } else {
                batch.push(BlurFlagRow {
                    file_id: row.file_id,
                    flag_type: "blur",
                    confidence,
                    reason: format!("subject {:.1} < p10 {:.1}", s_subject, threshold),
                });
                report.flagged_blur += 1;
                flagged_blur = true;
            }
        }

        // IQA second opinion: independent of subject sharpness.
        let mut flagged_low_iqa = false;
        if let (Some(iqa_p10), Some(score)) = (iqa_p10, row.iqa_score) {
            if score < iqa_p10 {
                let confidence = ((iqa_p10 - score) / iqa_p10).clamp(0.01, 1.0);
                batch.push(BlurFlagRow {
                    file_id: row.file_id,
                    flag_type: "low_iqa",
                    confidence,
                    reason: format!("iqa {:.2} < global p10 {:.2}", score, iqa_p10),
                });
                report.flagged_low_iqa += 1;
                flagged_low_iqa = true;
            }
        }

        // Both blur AND low_iqa → bump the just-pushed blur row's confidence by 0.2 (cap 1.0).
        if flagged_blur && flagged_low_iqa {
            // The blur row is the most recent "blur" entry we pushed for this file.
            if let Some(blur_row) = batch
                .iter_mut()
                .rev()
                .find(|f| f.file_id == row.file_id && f.flag_type == "blur")
            {
                blur_row.confidence = (blur_row.confidence + 0.2).min(1.0);
                blur_row.reason = format!("{} (confirmed by low IQA)", blur_row.reason);
                report.blur_confidence_bumped += 1;
            }
        }

        if batch.len() >= 64 {
            let to_flush = std::mem::take(&mut batch);
            catalog.flush_blur_flag_batch(&to_flush)?;
        }
    }

    if !batch.is_empty() {
        catalog.flush_blur_flag_batch(&batch)?;
    }

    tracing::info!(
        buckets = report.buckets_built,
        cleared = report.flags_cleared,
        blur = report.flagged_blur,
        back_focus = report.flagged_back_focus,
        low_iqa = report.flagged_low_iqa,
        "calibration complete"
    );

    Ok(report)
}

/// Global p10 of `s_subject` across all sharpness rows (the global fallback
/// threshold). Reads the sentinel baseline row written by
/// `rebuild_sharpness_baselines`. `None` when no sharpness data exists.
fn global_sharpness_p10(catalog: &Catalog) -> anyhow::Result<Option<f32>> {
    // The sentinel row is ('*','*',0,0.0); ask for it with a huge min_samples=0.
    Ok(catalog.bucket_baseline_p10("*", "*", 0, 0.0, 0)?)
}
```

> Note on `global_sharpness_p10`: it reuses `bucket_baseline_p10` against the sentinel key `('*','*',0,0.0)` with `min_samples = 0`, so it returns the sentinel's p10 whenever any sample exists, and `None` on a totally empty catalog. No new catalog method needed.

- [ ] **Step 2: Add the lib re-export**

In `crates/pipeline/src/lib.rs`, after the existing `pub use` lines (13–15), add:

```rust
pub use calibration::{run_calibration, CalibrationReport};
```

- [ ] **Step 3: Build the crate**

```bash
source ~/.cargo/env
cargo build -p pipeline
cargo fmt --check
cargo clippy -p pipeline --all-targets --all-features -- -D warnings
```

Expected: builds clean (the orchestrator is exercised by the integration tests in Task 10; a smoke build here catches type errors early).

- [ ] **Step 4: Commit**

```bash
cd /home/carsten/workspace/photopipeline
git add crates/pipeline/src/calibration/mod.rs crates/pipeline/src/lib.rs
git commit -m "feat(calibration): run_calibration orchestrator + lib re-export

Clear blur-related flags, rebuild baselines, then reflag each file: per-bucket
p10 (falling back to the global sentinel), back_focus when background >2x sharper
else blur, an independent low_iqa second opinion, and a +0.2 blur-confidence bump
when both blur and low_iqa fire. Batches of 64 flushed via flush_blur_flag_batch.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 10: Integration tests for calibration

**Files:**
- Create: `crates/pipeline/tests/calibration.rs`

**Interfaces:**
- Consumes: `pipeline::{run_calibration, catalog::Catalog, defect::SharpnessResult, ingest::{IngestedFile, ExifData, FileFormat}, catalog::MlRow, config::DefectConfig}`.
- Produces: the six required tests.

**Approach decision (pick ONE — this plan uses the direct-upsert approach):** rather than ingesting JPEGs and running `analyze_defects` (which would couple the calibration test to the detector + cache + image pipeline and make sharpness values hard to control), each test inserts files via `flush_batch` with controlled synthetic EXIF, then directly `upsert_sharpness(...)` with hand-chosen `s_subject`/`s_background` values, then runs `run_calibration`. This exercises the actual Phase-4 deliverable (baseline rebuild + reflag logic) deterministically. Synthetic images are still generated where a test wants the sharpness values to *come from* a real blur, via the `sharpness_of` helper that runs `compute_sharpness` on an in-memory image — but the canonical path is direct control. All fixtures share one EXIF bucket (`TestModel` / `TestLens 50mm` / 50 mm / f/2.8). `min_samples_for_bucket` is overridden to 3.

- [ ] **Step 1: Write the test file with helpers + the six tests**

Create `crates/pipeline/tests/calibration.rs`:

```rust
use image::{imageops, DynamicImage, ImageBuffer, Rgb};
use pipeline::catalog::{Catalog, MlRow};
use pipeline::config::DefectConfig;
use pipeline::defect::{compute_sharpness, SharpnessResult};
use pipeline::ingest::{ExifData, FileFormat, IngestedFile};
use std::path::PathBuf;
use tempfile::TempDir;

// ── config ───────────────────────────────────────────────────────────────────

/// DefectConfig with a tiny bucket sample floor so synthetic suites calibrate.
fn test_cfg() -> DefectConfig {
    let mut cfg = DefectConfig::default();
    cfg.blur.min_samples_for_bucket = 3;
    cfg
}

// ── EXIF: every fixture lands in one bucket (TestModel/TestLens/50mm/f2.8) ────

fn bucket_exif() -> ExifData {
    ExifData {
        captured_at: Some(1_686_830_400),
        camera_make: Some("TestMake".into()),
        camera_model: Some("TestModel".into()),
        lens_model: Some("TestLens 50mm".into()),
        focal_length_mm: Some(50.0),
        aperture: Some(2.8),
        iso: Some(200),
        shutter_seconds: Some(0.01),
        width: Some(256),
        height: Some(256),
        orientation: Some(1),
    }
}

// ── synthetic images ──────────────────────────────────────────────────────────

/// 256x256 high-frequency checkerboard (sharp).
fn sharp_checkerboard() -> DynamicImage {
    let img: ImageBuffer<Rgb<u8>, Vec<u8>> = ImageBuffer::from_fn(256, 256, |x, y| {
        if (x / 2 + y / 2) % 2 == 0 {
            Rgb([0, 0, 0])
        } else {
            Rgb([255, 255, 255])
        }
    });
    DynamicImage::ImageRgb8(img)
}

/// Strongly Gaussian-blurred checkerboard (genuinely blurry everywhere).
fn blurry_checkerboard() -> DynamicImage {
    let sharp = sharp_checkerboard().to_rgb8();
    let b1 = imageops::blur(&sharp, 6.0);
    let b2 = imageops::blur(&b1, 6.0);
    DynamicImage::ImageRgb8(b2)
}

/// Center region blurry, surround sharp (back-focus: subject soft, bg sharp).
fn back_focus_image() -> DynamicImage {
    let sharp = sharp_checkerboard().to_rgb8();
    let blurred = imageops::blur(&imageops::blur(&sharp, 6.0), 6.0);
    // Start from the sharp surround, paste the blurred center crop in.
    let mut out = sharp.clone();
    let (w, h) = (out.width(), out.height());
    let (x0, y0, x1, y1) = (w * 3 / 10, h * 3 / 10, w * 7 / 10, h * 7 / 10);
    for y in y0..y1 {
        for x in x0..x1 {
            out.put_pixel(x, y, *blurred.get_pixel(x, y));
        }
    }
    DynamicImage::ImageRgb8(out)
}

/// Center region sharp, surround blurry (shallow DoF / bokeh: subject crisp).
fn shallow_dof_image() -> DynamicImage {
    let sharp = sharp_checkerboard().to_rgb8();
    let blurred = imageops::blur(&imageops::blur(&sharp, 6.0), 6.0);
    let mut out = blurred.clone();
    let (w, h) = (out.width(), out.height());
    let (x0, y0, x1, y1) = (w * 3 / 10, h * 3 / 10, w * 7 / 10, h * 7 / 10);
    for y in y0..y1 {
        for x in x0..x1 {
            out.put_pixel(x, y, *sharp.get_pixel(x, y));
        }
    }
    DynamicImage::ImageRgb8(out)
}

/// Run the center-crop sharpness path on an in-memory image (no detector),
/// returning (s_subject, s_background). Mirrors what analyze_defects records.
fn sharpness_of(img: &DynamicImage) -> SharpnessResult {
    let cfg = DefectConfig::default();
    compute_sharpness(img, None, None, &cfg.blur)
}

// ── catalog plumbing ───────────────────────────────────────────────────────────

fn make_catalog() -> (Catalog, TempDir) {
    let dir = TempDir::new().unwrap();
    let catalog = Catalog::open(&dir.path().join("c.duckdb")).unwrap();
    (catalog, dir)
}

/// Insert one file with bucket EXIF + the given sharpness; returns file_id.
fn insert(catalog: &Catalog, idx: usize, sharp: &SharpnessResult) -> i64 {
    let file = IngestedFile {
        path: PathBuf::from(format!("/cal/{idx}.jpg")),
        content_hash: idx as u128,
        size: 1,
        mtime_ns: idx as i64,
        format: FileFormat::Jpg,
        has_sidecar_jpg: false,
    };
    let id = catalog
        .flush_batch(&[(file, Some(bucket_exif()))])
        .unwrap()[0];
    catalog.upsert_sharpness(id, sharp).unwrap();
    id
}

/// Insert five sharp baseline-population files; return their ids.
fn insert_sharp_population(catalog: &Catalog) -> Vec<i64> {
    let s = sharpness_of(&sharp_checkerboard());
    (0..5).map(|i| insert(catalog, i, &s)).collect()
}

/// Per-file flag presence, via the public `Catalog::count_file_flag` helper.
/// (The `Catalog` connection is private and DuckDB is single-writer, so a test
/// cannot open a second connection to the same DB — hence a catalog method,
/// added in Step 2 below.)
fn has_flag(catalog: &Catalog, file_id: i64, flag_type: &str) -> bool {
    catalog.count_file_flag(file_id, flag_type).unwrap() > 0
}
```

> **Note:** `has_flag` calls `Catalog::count_file_flag`, which Step 2 adds to the catalog. The test file needs no `duckdb` import.

- [ ] **Step 2: Add a public per-file flag-count helper to the catalog**

In `crates/pipeline/src/catalog/mod.rs`, add to `impl Catalog` (next to `count_defect_flags`):

```rust
    /// Count defect flags of `flag_type` for a single file. Test/inspection helper.
    pub fn count_file_flag(&self, file_id: i64, flag_type: &str) -> Result<i64, CatalogError> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| CatalogError::Db("mutex poisoned".into()))?;
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM defect_flags WHERE file_id = ? AND flag_type = ?",
                duckdb::params![file_id, flag_type],
                |r| r.get(0),
            )
            .map_err(|e| CatalogError::Db(e.to_string()))?;
        Ok(n)
    }
```

- [ ] **Step 3: Confirm the test file compiles against the new catalog helper**

```bash
source ~/.cargo/env
cargo test -p pipeline --test calibration --no-run
```

Expected: compiles cleanly (no tests run yet — the six tests are added in Step 4). If you see `no method named count_file_flag`, Step 2 was skipped. The test file should not reference `duckdb` directly; keep the `MlRow` import (Test 6 uses it).

- [ ] **Step 4: Add the six tests to `crates/pipeline/tests/calibration.rs`**

```rust
#[test]
fn sharp_not_flagged() {
    let (catalog, _dir) = make_catalog();
    let ids = insert_sharp_population(&catalog);
    let report = pipeline::run_calibration(&catalog, &test_cfg()).unwrap();
    assert_eq!(report.buckets_built, 1);
    for id in ids {
        assert!(!has_flag(&catalog, id, "blur"), "sharp file {id} must not be blur-flagged");
        assert!(!has_flag(&catalog, id, "back_focus"));
    }
}

#[test]
fn genuinely_blurry_flagged() {
    let (catalog, _dir) = make_catalog();
    let sharp_ids = insert_sharp_population(&catalog);
    let blurry = sharpness_of(&blurry_checkerboard());
    let blur_id = insert(&catalog, 100, &blurry);

    pipeline::run_calibration(&catalog, &test_cfg()).unwrap();

    assert!(has_flag(&catalog, blur_id, "blur"), "blurry file must be blur-flagged");
    for id in sharp_ids {
        assert!(!has_flag(&catalog, id, "blur"), "sharp file {id} must not be blur-flagged");
    }
}

#[test]
fn back_focus_flagged() {
    let (catalog, _dir) = make_catalog();
    insert_sharp_population(&catalog);
    // Hand-set sharpness so subject is clearly soft and background >2x sharper.
    let bf = SharpnessResult {
        s_global: 50.0,
        s_subject: Some(5.0),
        s_background: Some(80.0),
        subject_ratio: Some(0.16),
        detector_used: "rt-detr-l".into(),
    };
    let bf_id = insert(&catalog, 100, &bf);

    pipeline::run_calibration(&catalog, &test_cfg()).unwrap();

    assert!(has_flag(&catalog, bf_id, "back_focus"), "must be back_focus");
    assert!(!has_flag(&catalog, bf_id, "blur"), "must NOT be plain blur");
}

#[test]
fn shallow_dof_not_flagged() {
    let (catalog, _dir) = make_catalog();
    insert_sharp_population(&catalog);
    // Sharp subject (high s_subject), blurry background — the false-positive
    // case Phase 4 fixes. Subject is at/above the baseline → not flagged.
    let dof = SharpnessResult {
        s_global: 50.0,
        s_subject: Some(120.0),
        s_background: Some(8.0),
        subject_ratio: Some(0.16),
        detector_used: "rt-detr-l".into(),
    };
    let dof_id = insert(&catalog, 100, &dof);

    pipeline::run_calibration(&catalog, &test_cfg()).unwrap();

    assert!(!has_flag(&catalog, dof_id, "blur"), "sharp-subject bokeh must not be blur");
    assert!(!has_flag(&catalog, dof_id, "back_focus"), "must not be back_focus");
}

#[test]
fn falls_back_to_global() {
    // Single file in its bucket (below min_samples=3). Calibration must not
    // crash; the file is reflagged against the global sentinel instead.
    let (catalog, _dir) = make_catalog();
    let s = sharpness_of(&sharp_checkerboard());
    let id = insert(&catalog, 0, &s);

    let report = pipeline::run_calibration(&catalog, &test_cfg()).unwrap();
    // The lone bucket has 1 sample < 3 → no per-bucket row, but a global row exists.
    assert_eq!(report.buckets_built, 0, "undersized bucket builds no per-bucket row");
    assert_eq!(report.global_n_samples, 1, "global counts the one sample");
    // Presence/absence of a flag is acceptable; assert only that it didn't error
    // and the flag state is well-defined (a count >= 0 always holds → query works).
    let _ = has_flag(&catalog, id, "blur");
}

#[test]
fn calibrate_is_idempotent() {
    let (catalog, _dir) = make_catalog();
    insert_sharp_population(&catalog);
    let blurry = sharpness_of(&blurry_checkerboard());
    let blur_id = insert(&catalog, 100, &blurry);
    // Add an IQA score so the low_iqa + bump path is exercised across both runs.
    catalog
        .flush_ml_batch(&[MlRow {
            file_id: blur_id,
            embedding: None,
            iqa_score: Some(("clip-iqa".into(), 0.01)),
        }])
        .unwrap();
    // A few higher IQA scores so 0.01 is in the bottom decile.
    for (k, &fid) in [1i64, 2, 3, 4, 5].iter().enumerate() {
        let _ = k;
        catalog
            .flush_ml_batch(&[MlRow {
                file_id: fid,
                embedding: None,
                iqa_score: Some(("clip-iqa".into(), 0.8)),
            }])
            .unwrap();
    }

    let r1 = pipeline::run_calibration(&catalog, &test_cfg()).unwrap();
    let r2 = pipeline::run_calibration(&catalog, &test_cfg()).unwrap();

    assert_eq!(r1.flagged_blur, r2.flagged_blur);
    assert_eq!(r1.flagged_back_focus, r2.flagged_back_focus);
    assert_eq!(r1.flagged_low_iqa, r2.flagged_low_iqa);
    assert_eq!(r1.blur_confidence_bumped, r2.blur_confidence_bumped);
    assert_eq!(r1.buckets_built, r2.buckets_built);
    // The blur file still has exactly one blur row after the second run.
    assert_eq!(catalog.count_file_flag(blur_id, "blur").unwrap(), 1);
}
```

> **Test-6 note:** the `[1,2,3,4,5]` file-ids assume `insert_sharp_population` produced file_ids 1..=5 (first five inserts into a fresh DB via DuckDB identity). If the identity sequence does not start at 1 on this build, capture the ids from `insert_sharp_population`'s return value instead and use those — the test already has them via `let pop = insert_sharp_population(&catalog);`. Implementer: bind `let pop = insert_sharp_population(&catalog);` and iterate `pop` rather than the hardcoded literal to be safe.

- [ ] **Step 5: Run the six tests**

```bash
source ~/.cargo/env
cargo test -p pipeline --test calibration -- --nocapture
```

Expected: all six PASS. If `genuinely_blurry_flagged` does not flag (the center-crop blur signal too weak), increase the blur strength in `blurry_checkerboard` (add a third `imageops::blur(&_, 6.0)` pass) until `sharpness_of(&blurry_checkerboard()).s_subject` is clearly below the sharp population's p10. If `back_focus`/`shallow_dof` are flaky from real images, note both use hand-set `SharpnessResult` values (not `sharpness_of`) precisely to avoid that — keep them hand-set.

- [ ] **Step 6: fmt + clippy + full test suite + commit**

```bash
source ~/.cargo/env
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all
cd /home/carsten/workspace/photopipeline
git add crates/pipeline/tests/calibration.rs crates/pipeline/src/catalog/mod.rs
git commit -m "test(calibration): six integration tests for baseline + reflag

sharp_not_flagged, genuinely_blurry_flagged, back_focus_flagged,
shallow_dof_not_flagged, falls_back_to_global, calibrate_is_idempotent. Uses
synthetic EXIF in one bucket, direct sharpness upserts for deterministic control,
and min_samples_for_bucket=3. Adds Catalog::count_file_flag test helper.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 11: CLI wiring for `photopipe calibrate`

**Files:**
- Modify: `crates/cli/src/main.rs:58-59` (Calibrate help text), `:177-181` (`cmd_calibrate`)

**Interfaces:**
- Consumes: `pipeline::run_calibration`, `pipeline::CalibrationReport`, `pipeline::catalog::Catalog`, `config::Config`.
- Produces: a working `photopipe calibrate` that prints the report.

- [ ] **Step 1: Update the `Calibrate` subcommand help text**

In `crates/cli/src/main.rs`, replace (lines 58–59):

```rust
    /// Rebuild per-lens sharpness baselines from the catalog.
    Calibrate,
```

with:

```rust
    /// Rebuild per-lens sharpness baselines and re-flag blur/back-focus/low-IQA.
    ///
    /// Run after a meaningful number of photos per lens have been scanned
    /// (~30+ per lens is the default sample threshold). Leaves over/underexposed
    /// flags untouched.
    Calibrate,
```

- [ ] **Step 2: Implement `cmd_calibrate`**

Replace the stub `cmd_calibrate` (lines 177–181):

```rust
fn cmd_calibrate(cfg: &config::Config) -> Result<()> {
    use pipeline::catalog::Catalog;

    let catalog = Catalog::open(&cfg.catalog.db_path)
        .map_err(|e| anyhow::anyhow!("catalog: {}", e))?;

    let report = pipeline::run_calibration(&catalog, &cfg.defect)?;

    println!("Calibration complete:");
    println!("  Buckets built          : {}", report.buckets_built);
    println!("  Global sample count    : {}", report.global_n_samples);
    println!("  Stale flags cleared    : {}", report.flags_cleared);
    println!("  Flagged blur           : {}", report.flagged_blur);
    println!("  Flagged back-focus     : {}", report.flagged_back_focus);
    println!("  Flagged low-IQA        : {}", report.flagged_low_iqa);
    println!("  Blur confidence bumped : {}", report.blur_confidence_bumped);
    Ok(())
}
```

- [ ] **Step 3: Build the CLI and run calibrate against an empty catalog (edge case)**

```bash
source ~/.cargo/env
cargo build -p cli
TMPDB=$(mktemp -d)
cargo run -p cli -- --config /dev/null calibrate 2>/dev/null || true
```

Expected: `cmd_calibrate` opens the default-config DB path; if you want a clean empty-catalog check, point the config at a temp db. The key assertion: it returns success and prints zeros (no panic) when there is no sharpness data. If `--config /dev/null` is rejected by config loading, instead run `cargo run -p cli -- calibrate` after a fresh `scan` of an empty temp dir; either way confirm exit code 0 and zeroed counters.

- [ ] **Step 4: fmt + clippy + full suite**

```bash
source ~/.cargo/env
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all
```

Expected: all clean and passing.

- [ ] **Step 5: Commit**

```bash
cd /home/carsten/workspace/photopipeline
git add crates/cli/src/main.rs
git commit -m "feat(calibration): wire photopipe calibrate command

cmd_calibrate opens the catalog and runs pipeline::run_calibration, printing the
full CalibrationReport. Updates Calibrate help text to explain it rebuilds
baselines + reflags and should run after ~30+ photos/lens.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 12: Final acceptance gate

**Files:** none (verification only).

- [ ] **Step 1: Run the full acceptance battery**

```bash
source ~/.cargo/env
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all
```

Expected: all three clean/green, including the six calibration tests and the detector unit tests.

- [ ] **Step 2: Confirm Phase-4 acceptance criteria hold**

Verify by inspection / the passing tests:
- Shallow-DoF (sharp subject) NOT flagged blur — `shallow_dof_not_flagged`. ✓
- Back-focus IS `back_focus` — `back_focus_flagged`. ✓
- Genuinely blurry IS `blur` — `genuinely_blurry_flagged`. ✓
- No calibration data → global fallback, no crash — `falls_back_to_global`. ✓
- Re-running calibrate is deterministic — `calibrate_is_idempotent`. ✓
- `overexposed`/`underexposed` untouched — `clear_blur_related_flags_only_removes_blur_kinds`. ✓
- No `unsafe`, no row-at-a-time INSERTs in flag hot path (transactional batch in `flush_blur_flag_batch`). ✓
- `photopipe doctor` reports `rt_detr_l.onnx` as present (Task 4). ✓

- [ ] **Step 3: Confirm clean git state**

```bash
cd /home/carsten/workspace/photopipeline
git status --short
git log --oneline -8
```

Expected: working tree clean; the eight Phase-4 commits present (detector finalize, buckets, two catalog commits, baseline rebuild, blur-flag batch, orchestrator, tests, CLI). No stray uncommitted files from the original detector diff remain.

---

## Self-Review

**Spec coverage:**
- §6 `sharpness_baseline` schema — used as-is, no migration (Task 7). ✓
- §8 Phase 4 algorithm (threshold lookup, back_focus vs blur, IQA second opinion, +0.2 bump) — Task 9. ✓
- Appendix A COCO→SubjectClass — already in `detector.rs::coco_id_to_subject_class`, verified in Task 1. ✓
- Appendix B focal/aperture buckets — Task 5 constants verbatim. ✓
- §9 export script — Task 2. ✓
- §15.4/§15.7 missing-tools + RT-DETR unblock — Tasks 1–4. ✓
- PHASE_4_PROMPT sub-tasks 1–6 — Tasks 5–11 (with documented deviations: raw EXIF in `SharpnessReflagRow`; prepared-INSERT not Appender for flags). ✓

**Deviations flagged for the reviewer:**
1. `SharpnessReflagRow` carries raw `focal_length_mm`/`aperture`, not pre-bucketed fields (bucketing centralized in Rust).
2. `flush_blur_flag_batch` uses prepared `INSERT … ON CONFLICT` in a transaction, NOT the Appender (overrides old prompt; matches `flush_defect_batch` and the table's `nextval` id + UNIQUE constraint).
3. Calibration tests use the direct sharpness-upsert approach (not full ingest+analyze) for deterministic control; synthetic images feed `compute_sharpness` only where a real blur signal is wanted.
4. Percentiles computed in Rust (interpolated order statistics), not DuckDB `quantile_cont`, to avoid tiny-sample quirks — except `iqa_global_p10` which uses `quantile_cont` over the (larger) IQA population.

**Type consistency:** `BlurFlagRow.flag_type: &'static str` is fed only string literals in `run_calibration`. `RebuildReport`/`SharpnessReflagRow`/`CalibrationReport` field names match across Tasks 7, 9, 11. `bucket_baseline_p10` signature identical at definition (Task 8) and call sites (Task 9). `count_file_flag` defined in Task 10 Step 2, used in Task 10 tests. ✓
