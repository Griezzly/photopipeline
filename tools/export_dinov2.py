#!/usr/bin/env python3
"""Export facebook/dinov2-base to ONNX for photopipe's `DinoV2Embedder`.

One-time tool (Python lives only in `tools/`; the shipped Rust binary has no
Python dependency). Run from the repo root:

    uv run --python 3.12 --with torch --with transformers --with onnx \
            --with onnxruntime --with numpy tools/export_dinov2.py

or inside a venv that has torch + transformers + onnx + onnxruntime + numpy.

Output: `models/dinov2_base.onnx`.

I/O contract — MUST match `crates/pipeline/src/models/embedder.rs`:
  input  "image"     : float32 [B, 3, 224, 224], pixel values in [0, 1] (NOT pre-normalized;
                       ImageNet normalization is applied INSIDE this graph)
  output "embedding" : float32 [B, 768]  (the DINOv2 CLS token)
"""

import sys

import torch
import torch.nn as nn
from transformers import Dinov2Model

CKPT = "facebook/dinov2-base"
OUT = "models/dinov2_base.onnx"
DIM = 768


class Embedder(nn.Module):
    """DINOv2-base wrapped to take [0,1] pixels and emit the CLS embedding.

    ImageNet normalization is baked in so the Rust side only has to feed raw
    pixel/255 values (see embedder.rs::preprocess)."""

    def __init__(self) -> None:
        super().__init__()
        self.backbone = Dinov2Model.from_pretrained(CKPT)
        self.backbone.eval()
        self.register_buffer("mean", torch.tensor([0.485, 0.456, 0.406]).view(1, 3, 1, 1))
        self.register_buffer("std", torch.tensor([0.229, 0.224, 0.225]).view(1, 3, 1, 1))

    def forward(self, image: torch.Tensor) -> torch.Tensor:
        x = (image - self.mean) / self.std
        out = self.backbone(pixel_values=x)
        # last_hidden_state: [B, 1+num_patches, 768]; token 0 is the CLS token.
        return out.last_hidden_state[:, 0]


def main() -> int:
    model = Embedder().eval()
    dummy = torch.rand(1, 3, 224, 224)
    with torch.no_grad():
        ref = model(dummy)
    print(f"torch reference output: shape={tuple(ref.shape)}")
    assert ref.shape == (1, DIM), ref.shape

    torch.onnx.export(
        model,
        (dummy,),
        OUT,
        input_names=["image"],
        output_names=["embedding"],
        dynamic_axes={"image": {0: "batch"}, "embedding": {0: "batch"}},
        opset_version=17,
        do_constant_folding=True,
        # Use the legacy TorchScript exporter: it honors the exact input/output
        # names above (the dynamo exporter needs onnxscript and may rename I/O,
        # which would break the Rust embedder's `image`/`embedding` lookups).
        dynamo=False,
    )
    print(f"wrote {OUT}")

    # Validate the exported graph against the torch reference.
    import numpy as np
    import onnxruntime as ort

    sess = ort.InferenceSession(OUT, providers=["CPUExecutionProvider"])
    got = sess.run(["embedding"], {"image": dummy.numpy()})[0]
    print(f"onnxruntime output: shape={got.shape}")
    max_abs = float(np.abs(got - ref.numpy()).max())
    print(f"max |torch - onnx| = {max_abs:.3e}")
    assert got.shape == (1, DIM), got.shape
    assert max_abs < 1e-3, f"torch/onnx mismatch too large: {max_abs}"
    print("validation OK")
    return 0


if __name__ == "__main__":
    sys.exit(main())
