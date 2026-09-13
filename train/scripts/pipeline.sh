#!/usr/bin/env bash
# The whole pipeline: raw photos -> labels -> a trained model in ../models/.
#
#   bash train/scripts/pipeline.sh          # or: make yolo-train
#   EPOCHS=2 bash train/scripts/pipeline.sh # a quick smoke run
#
#   1. assemble_dataset.py   raw/ + your labels -> dataset/
#   2. scripts/train.sh      dataset/           -> runs/detect/cubes/
#   3. the best weights are copied to ../models/, where the camera looks for them
#
# Env: EPOCHS, IMGSZ, BATCH, DEVICE, MODEL, VENV, MODEL_NAME, RUN_NAME.
set -euo pipefail

TRAIN="$(cd "$(dirname "$0")/.." && pwd)"
ROOT="$(cd "$TRAIN/.." && pwd)"
VENV="${VENV:-$ROOT/.venv}"
PY="$VENV/bin/python"
RUN_NAME="${RUN_NAME:-cubes}"
MODEL_NAME="${MODEL_NAME:-cubes-best}"
cd "$TRAIN"

if [ ! -x "$PY" ]; then
    echo "no $PY - run 'make yolo-setup' (or bash train/scripts/setup.sh) first" >&2
    exit 1
fi

if [ ! -d raw ] || [ -z "$(ls -A raw 2>/dev/null)" ]; then
    echo "no photos in $TRAIN/raw - put them in raw/<class>/ first" >&2
    exit 1
fi

echo "==> 1/3 assembling dataset/ from raw/ and your labels"
"$PY" scripts/assemble_dataset.py

echo "==> 2/3 training"
bash scripts/train.sh

BEST="$TRAIN/runs/detect/$RUN_NAME/weights/best.pt"
if [ ! -f "$BEST" ]; then
    echo "training finished but $BEST does not exist" >&2
    exit 1
fi

echo "==> 3/3 publishing to $ROOT/models/$MODEL_NAME.pt"
mkdir -p "$ROOT/models"
# Publishing overwrites, so keep the outgoing model: a smoke run with EPOCHS=2
# would otherwise replace a good one silently.
if [ -f "$ROOT/models/$MODEL_NAME.pt" ]; then
    cp "$ROOT/models/$MODEL_NAME.pt" "$ROOT/models/$MODEL_NAME.previous.pt"
    echo "    the previous model is kept as $MODEL_NAME.previous.pt"
fi
cp "$BEST" "$ROOT/models/$MODEL_NAME.pt"
ls -l "$ROOT/models/$MODEL_NAME.pt"
echo "==> done: the camera picks it up from ./models by default"
