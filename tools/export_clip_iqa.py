#!/usr/bin/env python3
"""Export a CLIP-IQA scorer to ONNX for photopipe's `ClipIqaScorer`.

One-time tool (Python lives only in `tools/`; the shipped Rust binary has no
Python dependency). Run inside a venv with torch + transformers + onnx +
onnxruntime + numpy (e.g. the uv venv used for export_dinov2.py).

Output: `models/clip_iqa.onnx`.

Approach (CLIP-IQA, Wang et al.): bake the text features for an antonym prompt
pair ("Good photo." / "Bad photo.") into the graph as constants, then at runtime
encode the image with CLIP's vision tower, cosine-compare to the two text
features, softmax, and emit P("Good photo.") as the quality score.

I/O contract — MUST match `crates/pipeline/src/models/iqa.rs`:
  input  "image"     : float32 [B, 3, 224, 224], ALREADY CLIP-normalized
                       ((px/255 - CLIP_MEAN) / CLIP_STD; the Rust side does this,
                       so this graph must NOT normalize again)
  output "iqa_score" : float32 [B]  in [0, 1]  (probability of "Good photo.")
"""

import sys

import torch
import torch.nn as nn
from transformers import CLIPModel, CLIPTokenizer

CKPT = "openai/clip-vit-base-patch32"
OUT = "models/clip_iqa.onnx"
PROMPTS = ["Good photo.", "Bad photo."]  # antonym pair; index 0 == "good"


class ClipIqa(nn.Module):
    """CLIP image tower + baked text features → softmax quality score.

    `text_features` (L2-normalized, shape [2, dim]) and `logit_scale` are
    precomputed and registered as buffers so they fold into the ONNX graph as
    constants — the text encoder is NOT part of the exported model."""

    def __init__(self, clip: CLIPModel, text_features: torch.Tensor, logit_scale: float) -> None:
        super().__init__()
        self.clip = clip
        self.register_buffer("text_features", text_features)  # [2, dim], normalized
        self.logit_scale = float(logit_scale)

    def forward(self, image: torch.Tensor) -> torch.Tensor:
        # image: [B, 3, 224, 224], already CLIP-normalized.
        # Use the vision submodule + visual_projection directly (stable across
        # transformers versions; equivalent to the old get_image_features).
        pooled = self.clip.vision_model(pixel_values=image).pooler_output  # [B, hidden]
        feats = self.clip.visual_projection(pooled)  # [B, dim]
        feats = feats / feats.norm(p=2, dim=-1, keepdim=True)
        logits = self.logit_scale * feats @ self.text_features.t()  # [B, 2]
        return logits.softmax(dim=-1)[:, 0]  # [B] — P("Good photo.")


def main() -> int:
    clip = CLIPModel.from_pretrained(CKPT).eval()
    tokenizer = CLIPTokenizer.from_pretrained(CKPT)

    text_inputs = tokenizer(PROMPTS, padding=True, return_tensors="pt")
    with torch.no_grad():
        # text_model + text_projection (stable; equivalent to get_text_features).
        text_pooled = clip.text_model(**text_inputs).pooler_output  # [2, hidden]
        text_features = clip.text_projection(text_pooled)  # [2, dim]
        text_features = text_features / text_features.norm(p=2, dim=-1, keepdim=True)
        logit_scale = clip.logit_scale.exp().item()  # ~100

    model = ClipIqa(clip, text_features, logit_scale).eval()

    dummy = torch.randn(1, 3, 224, 224)  # stand-in for CLIP-normalized pixels
    with torch.no_grad():
        ref = model(dummy)
    print(f"torch reference output: shape={tuple(ref.shape)} value={ref.tolist()}")
    assert ref.shape == (1,), ref.shape
    assert 0.0 <= float(ref.item()) <= 1.0, ref.item()

    torch.onnx.export(
        model,
        (dummy,),
        OUT,
        input_names=["image"],
        output_names=["iqa_score"],
        dynamic_axes={"image": {0: "batch"}, "iqa_score": {0: "batch"}},
        opset_version=17,
        do_constant_folding=True,
        # Legacy TorchScript exporter: honors the exact I/O names and folds the
        # text-feature buffers into constants. (Dynamo exporter needs onnxscript
        # and may rename I/O, breaking the Rust `image`/`iqa_score` lookups.)
        dynamo=False,
    )
    print(f"wrote {OUT}")

    import numpy as np
    import onnxruntime as ort

    sess = ort.InferenceSession(OUT, providers=["CPUExecutionProvider"])
    got = sess.run(["iqa_score"], {"image": dummy.numpy()})[0]
    print(f"onnxruntime output: shape={got.shape} value={got.tolist()}")
    max_abs = float(np.abs(got - ref.numpy()).max())
    print(f"max |torch - onnx| = {max_abs:.3e}")
    assert got.shape == (1,), got.shape
    assert 0.0 <= float(got[0]) <= 1.0, got[0]
    assert max_abs < 1e-3, f"torch/onnx mismatch too large: {max_abs}"
    print("validation OK")
    return 0


if __name__ == "__main__":
    sys.exit(main())
