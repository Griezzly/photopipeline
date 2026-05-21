# models/

Place ONNX model files here before running ML phases (Phase 3+).
These files are gitignored — run the export scripts once to produce them.

| File | Used by | Status | Export script |
|------|---------|--------|---------------|
| `dinov2_base.onnx` | Embedder (dedupe, Phase 5) | Ready | `tools/export_dinov2.py` |
| `clip_iqa.onnx` | Image quality assessment (Phase 3) | Ready | `tools/export_clip_iqa.py` |
| `rt_detr_l.onnx` | Subject detector (blur ROI, Phase 3) | **Deferred** | `tools/export_rt_detr.py` |

## RT-DETR deferral

`rt_detr_l.onnx` is deferred. The ORT CPU kernel does not implement `Cos` for
`int64` inputs, and the `PekingU/rtdetr_r50vd` positional encodings emit that
op regardless of whether the legacy or dynamo ONNX exporter is used.  The
detector slot in `ModelHub` stays as `Option<Arc<dyn SubjectDetector>>`.  When
`rt_detr_l.onnx` is absent, sharpness analysis falls back to center-crop ROI,
which is the correct pre-Phase-3 behaviour.

Possible fixes (tracked separately):
- Graph surgery to cast the int64 → float32 before the `Cos` node.
- Switch to a different detector checkpoint that doesn't use the problematic
  positional encoding variant.

## Exporting models

```sh
cd tools
python -m venv .venv && source .venv/bin/activate
pip install -r requirements.txt

python export_dinov2.py     # → ../models/dinov2_base.onnx (~330 MB)
python export_clip_iqa.py   # → ../models/clip_iqa.onnx   (~340 MB)
# python export_rt_detr.py  # deferred — see above
```

Or run `models/download.sh` once pre-exported files are published.

## Execution providers

At runtime `ModelHub::from_config` probes providers in this order:

1. **TensorRT** — Linux only, requires `--features tensorrt` at build time plus
   the TensorRT SDK at link time.
2. **CUDA** — Linux/Windows; compiled in by default on non-macOS targets.
3. **CoreML** — macOS only; compiled in by default on macOS targets.
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
