#!/usr/bin/env python3
"""Export the Image-Adaptive-3DLUT predictor to ONNX (opset 18).

One-time tool. Produces:
    ../models/lut3d_predictor.onnx  — the weight-predictor CNN
    ../models/lut3d_basis.npy       — the basis LUTs as a plain [N,3,33,33,33] array

Only the predictor is exported. The reference implementation applies its fused
LUT through a custom CUDA trilinear-interpolation extension that cannot be
traced; photopipe performs the fuse and the apply in Rust at 16-bit precision
instead (spec section 7). Not used at runtime — the shipped photopipe binary
has zero Python dependency.

Licensing: the Image-Adaptive-3DLUT code is Apache-2.0, but its weights derive
from MIT-Adobe FiveK, whose Adobe licence grants use "solely for your own
research purposes". This script fetches the weights onto your machine; the
repository never contains or redistributes them. Revisit before any commercial
distribution.

Usage:
    uv venv .venv && uv pip install --python .venv/bin/python -r requirements.txt
    .venv/bin/python export_lut3d.py                    # fetches the checkpoints
    .venv/bin/python export_lut3d.py --luts LUTs.pth --classifier classifier.pth

The upstream release ships the basis LUTs and the predictor as two separate
files, so this takes two paths rather than one combined checkpoint.
"""
import argparse
import pathlib
import urllib.request

import numpy as np
import torch
import torch.nn as nn

OPSET = 18
INPUT_SIZE = 256
LUT_DIM = 33
N_LUTS = 3

UPSTREAM = "https://raw.githubusercontent.com/HuiZeng/Image-Adaptive-3DLUT/master/pretrained_models/sRGB"


class Predictor(nn.Module):
    """The weight-predictor CNN from Zeng et al., under 600K parameters.

    Mirrors the reference `Classifier` module layer for layer, including the
    indices in the `nn.Sequential`, because the published checkpoint is keyed by
    them (`model.1.weight`, `model.3.weight`, …). Reproduced here rather than
    imported so the export does not depend on cloning the upstream repo.

    Two details differ from a first reading of the architecture, both taken from
    the checkpoint itself rather than assumed:

    * The head is a `Conv2d(128, 3, 8)` over the final 8x8 feature map, not a
      global-pool-and-`Linear`. With five stride-2 convolutions, a 256x256 input
      is exactly what reduces to the 8x8 the kernel expects — the input size is
      load-bearing, not a convention.
    * Index 0 is an `Upsample` to 256x256. It is a no-op for the fixed input
      this graph declares, and is kept only so the checkpoint's keys line up
      without rewriting them.

    `forward` flattens the `[1, 3, 1, 1]` head output to the `[1, N]` the ONNX
    contract promises.
    """

    def __init__(self, n_luts: int = N_LUTS):
        super().__init__()
        self.model = nn.Sequential(
            nn.Upsample(size=(INPUT_SIZE, INPUT_SIZE), mode="bilinear", align_corners=False),
            nn.Conv2d(3, 16, 3, stride=2, padding=1),
            nn.LeakyReLU(0.2),
            nn.InstanceNorm2d(16, affine=True),
            nn.Conv2d(16, 32, 3, stride=2, padding=1),
            nn.LeakyReLU(0.2),
            nn.InstanceNorm2d(32, affine=True),
            nn.Conv2d(32, 64, 3, stride=2, padding=1),
            nn.LeakyReLU(0.2),
            nn.InstanceNorm2d(64, affine=True),
            nn.Conv2d(64, 128, 3, stride=2, padding=1),
            nn.LeakyReLU(0.2),
            nn.InstanceNorm2d(128, affine=True),
            nn.Conv2d(128, 128, 3, stride=2, padding=1),
            nn.LeakyReLU(0.2),
            nn.Dropout(0.5),
            nn.Conv2d(128, n_luts, 8, padding=0),
        )

    def forward(self, image):
        return self.model(image).flatten(1)


def fetch(name: str, dest_dir: pathlib.Path) -> pathlib.Path:
    """Download one upstream checkpoint if it is not already cached locally."""
    dest = dest_dir / name
    if dest.exists():
        print(f"using cached {dest}")
        return dest
    dest_dir.mkdir(parents=True, exist_ok=True)
    url = f"{UPSTREAM}/{name}"
    print(f"fetching {url}")
    urllib.request.urlretrieve(url, dest)
    return dest


def main() -> None:
    here = pathlib.Path(__file__).parent
    ap = argparse.ArgumentParser()
    ap.add_argument("--luts", help="path to LUTs.pth (downloaded if omitted)")
    ap.add_argument("--classifier", help="path to classifier.pth (downloaded if omitted)")
    ap.add_argument("--n-luts", type=int, default=N_LUTS)
    ap.add_argument("--cache-dir", default=str(here / ".checkpoints"))
    ap.add_argument("--out-dir", default=str(here.parent / "models"))
    args = ap.parse_args()

    cache = pathlib.Path(args.cache_dir)
    luts_path = pathlib.Path(args.luts) if args.luts else fetch("LUTs.pth", cache)
    clf_path = pathlib.Path(args.classifier) if args.classifier else fetch("classifier.pth", cache)

    out_dir = pathlib.Path(args.out_dir)
    out_dir.mkdir(parents=True, exist_ok=True)

    # ── the basis LUTs ──
    # LUTs.pth is a dict keyed by LUT index as a *string*, each holding its own
    # state dict with a single "LUT" tensor of [3, D, D, D].
    state = torch.load(luts_path, map_location="cpu", weights_only=False)
    basis = []
    for i in range(args.n_luts):
        entry = state.get(str(i), state.get(i))
        if entry is None:
            raise SystemExit(
                f"cannot find basis LUT {i} in {luts_path}. Keys present: {sorted(map(str, state))}"
            )
        tensor = entry["LUT"] if hasattr(entry, "get") and "LUT" in entry else entry
        basis.append(torch.as_tensor(tensor).float())

    stacked = torch.stack(basis).reshape(args.n_luts, 3, LUT_DIM, LUT_DIM, LUT_DIM)
    np.save(out_dir / "lut3d_basis.npy", stacked.numpy().astype(np.float32))
    print(f"wrote {out_dir / 'lut3d_basis.npy'}  shape={tuple(stacked.shape)}")

    # ── the predictor ──
    model = Predictor(args.n_luts).eval()
    predictor_state = torch.load(clf_path, map_location="cpu", weights_only=False)
    # The published keys are already `model.N.*`; anything else means the
    # checkpoint layout moved and silently loading nothing would export a graph
    # full of random weights.
    result = model.load_state_dict(predictor_state, strict=True)
    print(f"predictor load: {result}")

    dummy = torch.zeros(1, 3, INPUT_SIZE, INPUT_SIZE)
    onnx_path = out_dir / "lut3d_predictor.onnx"
    torch.onnx.export(
        model,
        dummy,
        str(onnx_path),
        opset_version=OPSET,
        input_names=["image"],
        output_names=["weights"],
        dynamic_axes=None,  # fixed 1x3x256x256; the Rust side always downsamples to this
        dynamo=False,
    )
    print(f"wrote {onnx_path}")

    # ── verify no custom op survived the trace ──
    # The reference trilinear-interpolation extension must NOT appear in the
    # graph; if it does, the export captured the apply step too and ORT will
    # fail to load the model at runtime.
    import onnx

    graph = onnx.load(str(onnx_path)).graph
    ops = {n.op_type for n in graph.node}
    custom = {o for o in ops if "trilinear" in o.lower() or "TrilinearInterpolation" in o}
    if custom:
        raise SystemExit(f"custom ops leaked into the graph: {custom}")
    print(f"graph ops: {sorted(ops)}")

    # ── verify the export actually computes the same thing ──
    # An op-name check cannot catch weights that failed to load or a head that
    # reshapes differently, and either would surface much later as a look that
    # is subtly wrong rather than as an error.
    import onnxruntime as ort

    rng = np.random.default_rng(0)
    sample = rng.random((1, 3, INPUT_SIZE, INPUT_SIZE), dtype=np.float32)
    with torch.no_grad():
        expected = model(torch.from_numpy(sample)).numpy()
    got = ort.InferenceSession(str(onnx_path), providers=["CPUExecutionProvider"]).run(
        None, {"image": sample}
    )[0]
    if got.shape != (1, args.n_luts):
        raise SystemExit(f"expected weights of shape (1, {args.n_luts}), got {got.shape}")
    delta = float(np.abs(expected - got).max())
    if delta > 1e-4:
        raise SystemExit(f"ONNX output diverges from PyTorch by {delta}")
    print(f"parity ok (max |Δ| = {delta:.2e}); sample weights = {got.round(4).tolist()}")


if __name__ == "__main__":
    main()
