# models/

Place ONNX model files here before running ML phases (Phase 3+).

| File | Used by | Export script |
|------|---------|---------------|
| `dinov2_base.onnx` | Embedder (dedupe, Phase 5) | `tools/export_dinov2.py` |
| `rt_detr_l.onnx` | Subject detector (blur ROI, Phase 3) | `tools/export_rt_detr.py` |
| `clip_iqa.onnx` | Image quality assessment (Phase 3) | `tools/export_clip_iqa.py` |

## Exporting

```sh
cd tools
pip install -r requirements.txt
python export_dinov2.py
python export_rt_detr.py
python export_clip_iqa.py
```

Or run `models/download.sh` once pre-exported files are published.
