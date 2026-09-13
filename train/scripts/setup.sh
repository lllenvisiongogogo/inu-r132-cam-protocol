#!/usr/bin/env bash
# Create the Python environment for training *and* for the detector.
#
#   bash train/scripts/setup.sh        # or: make yolo-setup
#
# It lives at the repository root (`.venv`) because `display` and `scan` look for
# it there too - the camera runs the same ultralytics alongside the training.
#
# The torch wheels on PyPI bundle CUDA and are ~2 GB; this machine has no NVIDIA
# driver, so torch and torchvision come from the CPU-only index instead (~200 MB).
set -euo pipefail

TRAIN="$(cd "$(dirname "$0")/.." && pwd)"
ROOT="$(cd "$TRAIN/.." && pwd)"
VENV="${VENV:-$ROOT/.venv}"

if [ ! -d "$VENV" ]; then
    echo "==> creating $VENV (python3 $(python3 -c 'import sys; print(".".join(map(str, sys.version_info[:2])))'))"
    python3 -m venv "$VENV"
fi

PY="$VENV/bin/python"
[ -x "$PY" ] || PY="$VENV/Scripts/python.exe"   # windows

"$PY" -m pip install --upgrade pip
"$PY" -m pip install torch torchvision --index-url https://download.pytorch.org/whl/cpu
"$PY" -m pip install -r "$TRAIN/requirements.txt"

"$PY" -c "import torch, ultralytics; print('torch', torch.__version__, '| cuda', torch.cuda.is_available(), '| ultralytics', ultralytics.__version__)"
echo "==> done: $VENV"
