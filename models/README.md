# models/

Place ONNX model files here before running ML phases (Phase 3+).
These files are gitignored — run the export scripts once to produce them.

| File | Used by | Status | Export script |
|------|---------|--------|---------------|
| `dinov2_base.onnx` | Embedder (dedupe, Phase 5) | Ready | `tools/export_dinov2.py` |
| `clip_iqa.onnx` | Image quality assessment (Phase 3) | Ready | `tools/export_clip_iqa.py` |
| `rt_detr_l.onnx` | Subject detector (blur ROI, Phase 4) | Ready | `tools/export_rt_detr.py` |
| `lut3d_predictor.onnx` | Look weight predictor (develop, Phase 2) | Ready | `tools/export_lut3d.py` |
| `lut3d_basis.npy` | Basis LUTs the predictor mixes | Ready | `tools/export_lut3d.py` |

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

## Image-Adaptive-3DLUT look model

`lut3d_predictor.onnx` is the weight-predictor CNN from Zeng et al.'s
Image-Adaptive-3DLUT, and `lut3d_basis.npy` holds the three basis LUTs it mixes,
as a float32 `[3, 3, 33, 33, 33]` array. Both come from
`tools/export_lut3d.py`, which fetches the upstream sRGB checkpoints
(`LUTs.pth` and `classifier.pth` — the release ships them as two files) and
caches them in `tools/.checkpoints/`.

The predictor takes `image` `[1, 3, 256, 256]` float32 and returns `weights`
`[1, 3]`. The input size is load-bearing rather than conventional: the head is a
`Conv2d(128, 3, 8)` over the final feature map, and 256×256 is exactly what five
stride-2 convolutions reduce to the 8×8 that kernel expects.

Only the predictor is exported. The reference implementation applies its fused
LUT through a custom CUDA trilinear-interpolation extension that will not trace
to ONNX, which suits us — photopipe fuses and applies in Rust at 16-bit
precision instead (spec §7). The exporter asserts no such op reached the graph,
and separately checks the ONNX output against PyTorch on a random input, because
an op-name check alone cannot catch weights that failed to load.

**Licensing — read before distributing anything.** The Image-Adaptive-3DLUT code
is Apache-2.0, but these weights are trained on MIT-Adobe FiveK, whose Adobe
licence grants use *"solely for your own research purposes"*. photopipe is a
personal, non-commercial project and never redistributes the weights: the
exporter downloads them onto your machine, and `.gitignore` covers both the
outputs and the cached checkpoints. **This needs revisiting before any
commercial distribution**, and it is the reason the look is a swappable stage
rather than something baked into the binary.

## Exporting models

```sh
cd tools
python -m venv .venv && source .venv/bin/activate
pip install -r requirements.txt

python export_dinov2.py     # → ../models/dinov2_base.onnx (~330 MB)
python export_clip_iqa.py   # → ../models/clip_iqa.onnx   (~340 MB)
python export_rt_detr.py    # → ../models/rt_detr_l.onnx (~175 MB)
python export_lut3d.py      # → ../models/lut3d_predictor.onnx (~1 MB)
                            #   + ../models/lut3d_basis.npy    (~1.3 MB)
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
