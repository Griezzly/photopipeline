# models/

Place ONNX model files here before running ML phases (Phase 3+).
These files are gitignored — run the export scripts once to produce them.

| File | Used by | Status | Export script |
|------|---------|--------|---------------|
| `dinov2_base.onnx` | Embedder (dedupe, Phase 5) | Ready | `tools/export_dinov2.py` |
| `clip_iqa.onnx` | Image quality assessment (Phase 3) | Ready | `tools/export_clip_iqa.py` |
| `rt_detr_l.onnx` | Subject detector (blur ROI, Phase 4) | Ready | `tools/export_rt_detr.py` |

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

## Exporting models

```sh
cd tools
python -m venv .venv && source .venv/bin/activate
pip install -r requirements.txt

python export_dinov2.py     # → ../models/dinov2_base.onnx (~330 MB)
python export_clip_iqa.py   # → ../models/clip_iqa.onnx   (~340 MB)
python export_rt_detr.py    # → ../models/rt_detr_l.onnx (~175 MB)
```

Or run `models/download.sh` once pre-exported files are published.

## CoreML execution provider (macOS) — disabled

CoreML EP is **disabled** in ort rc.12 when models use the ONNX external-data
format (`.onnx` graph + `.onnx.data` weights).  The failure mode is either a
SIGSEGV inside CoreML or an assertion `"model_path must not be empty"` in ORT's
graph optimizer.  Both DinoV2 and CLIP-IQA use external-data format.

**Consequence:** macOS uses the CPU provider.  On Apple Silicon (M-series) this
puts DinoV2 + CLIP-IQA in the 200–400 ms/image range; a 10k-photo library takes
~30–60 minutes for the ML phase.  The CUDA path (Linux/Windows) is unaffected.

**To re-enable CoreML:** uncomment the `eps.push(ort::ep::CoreML...)` line in
`build_session` (crates/pipeline/src/models/mod.rs) and remove the note, then
verify the model tests pass.  Revisit when `ort ≥ 2.0.0` stable releases.

## Execution providers

At runtime `ModelHub::from_config` probes providers in this order:

1. **TensorRT** — Linux only, requires `--features tensorrt` at build time plus
   the TensorRT SDK at link time.
2. **CUDA** — Linux/Windows; compiled in by default on non-macOS targets.
3. **CoreML** — macOS only; disabled pending ort stable (see note above).
4. **CPU** — always available, final fallback.

`photopipe doctor` shows which provider was selected and which models loaded.

## Test-gating

Tests that require a live model check for the file at the start and skip
themselves with an `eprintln!` notice when it is absent:

```rust
fn skip_if_no_model(path: &std::path::Path) -> bool {
    if !path.exists() {
        eprintln!("skipping: model not present at {}", path.display());
        return true;   // true → caller should return early
    }
    false
}
```

CI does not have model files; these tests no-op there and are meant to be run
locally after the export scripts have been executed.

## Manual accuracy test for RT-DETR (once unblocked)

Drop a photo with a clearly visible person at `tests/fixtures/raw/person.jpg`
and run:

```sh
cargo test --features manual-fixtures rt_detr_localizes_person
```

Do not gate CI on this test.
