#!/usr/bin/env bash
# Placeholder — export scripts are the canonical source for now.
# Run the Python exporters in tools/ to generate model files.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TOOLS_DIR="$SCRIPT_DIR/../tools"

echo "Running ONNX export scripts from $TOOLS_DIR ..."
pip install -q -r "$TOOLS_DIR/requirements.txt"
python "$TOOLS_DIR/export_dinov2.py"
python "$TOOLS_DIR/export_rt_detr.py"
python "$TOOLS_DIR/export_clip_iqa.py"
echo "Done. Model files written to $SCRIPT_DIR"
