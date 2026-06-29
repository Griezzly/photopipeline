#!/usr/bin/env python3
"""Export an RT-DETR R50VD subject detector to ONNX (opset 18).

One-time tool. Produces ../models/rt_detr_l.onnx, the file consumed by
pipeline::models::detector::RtDetrDetector. Not used at runtime — the shipped
photopipe binary has zero Python dependency.

The exported graph has two float32 outputs in this order:
    logits     : [batch, num_queries, num_classes]   (num_classes = 80, COCO)
    pred_boxes : [batch, num_queries, 4]              (cx, cy, w, h, normalized)
matching crates/pipeline/src/models/detector.rs::decode_detections.

Post-export graph surgery inserts Cast(to=float32) before every Cos/Sin node
to work around ORT's lack of a CPU kernel for integer-typed trig inputs (the
RT-DETR positional embedding uses integer position indices fed directly into
Cos/Sin).

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
OPSET = 18


def fix_cos_sin_inputs(model_path: pathlib.Path) -> int:
    """Insert Cast(to=float32) before every Cos/Sin node and fix output type annotations.

    ORT's CPU kernel for Cos/Sin only supports float inputs. RT-DETR's
    positional embeddings pass double (or integer) position indices directly to
    these ops. We prepend a Cast(to=float32) node for each input and also
    update any stale value_info type annotations on the outputs to float32 so
    ORT's type checker agrees with what we actually produce.

    Returns the number of Cast nodes inserted.
    """
    import onnx
    from onnx import TensorProto, helper

    model = onnx.load(str(model_path))
    graph = model.graph

    # Collect Cos/Sin output names so we can fix their value_info annotations.
    cos_sin_outputs: set = set()
    new_nodes = []
    cast_counter = 0

    for node in graph.node:
        if node.op_type in ("Cos", "Sin"):
            new_inputs = []
            for inp in node.input:
                cast_out = f"__cast_f32_{cast_counter}__"
                cast_counter += 1
                new_nodes.append(
                    helper.make_node("Cast", inputs=[inp], outputs=[cast_out], to=TensorProto.FLOAT)
                )
                new_inputs.append(cast_out)
            del node.input[:]
            node.input.extend(new_inputs)
            cos_sin_outputs.update(node.output)
        new_nodes.append(node)

    del graph.node[:]
    graph.node.extend(new_nodes)

    # Clear ALL intermediate value_info so stale double annotations downstream
    # of the Cos/Sin nodes don't cause type-check failures. infer_shapes will
    # rebuild them from scratch based on the updated graph.
    del graph.value_info[:]

    # Re-infer shapes/types so the graph is internally consistent.
    model = onnx.shape_inference.infer_shapes(model)

    onnx.save(model, str(model_path))
    return cast_counter


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

    # Insert Cast(to=float32) before every Cos/Sin node so ORT's CPU kernel
    # can execute them (they receive integer position indices in RT-DETR).
    n_casts = fix_cos_sin_inputs(out_path)
    print(f"graph surgery: inserted {n_casts} Cast(float32) nodes before Cos/Sin ops")

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
