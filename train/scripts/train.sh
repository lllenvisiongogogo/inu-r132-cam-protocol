#!/usr/bin/env bash
# Train the cube detector from dataset/ into runs/detect/cubes/.
#
#   bash train/scripts/train.sh
#   EPOCHS=300 bash train/scripts/train.sh
#   bash train/scripts/train.sh epochs=300 imgsz=960
#
# Overridable with the same env vars: EPOCHS, IMGSZ, BATCH, DEVICE, MODEL, VENV.
# To go all the way from raw photos to a published model, use pipeline.sh.
set -euo pipefail

TRAIN="$(cd "$(dirname "$0")/.." && pwd)"
ROOT="$(cd "$TRAIN/.." && pwd)"
VENV="${VENV:-$ROOT/.venv}"
cd "$TRAIN"

# Matplotlib and the ultralytics settings both default to $HOME, which is not
# writable here; keep them inside the project instead of a throwaway /tmp dir.
export YOLO_CONFIG_DIR="${YOLO_CONFIG_DIR:-$TRAIN/.ultralytics}"
export MPLCONFIGDIR="${MPLCONFIGDIR:-$TRAIN/.cache/matplotlib}"
mkdir -p "$YOLO_CONFIG_DIR" "$MPLCONFIGDIR"

# The run directory pipeline.sh publishes from.
RUN_NAME="${RUN_NAME:-cubes}"

YOLO="$VENV/bin/yolo"
[ -x "$YOLO" ] || YOLO="$VENV/Scripts/yolo.exe"
if [ ! -x "$YOLO" ]; then
    echo "no $YOLO - run 'make yolo-setup' (or bash train/scripts/setup.sh) first" >&2
    exit 1
fi

# Prefer the copy in weights/ when it is there: ultralytics' own downloader gives
# up after 30 s per attempt, which this network does not always beat.
DEFAULT_MODEL=yolo11n.pt
if [ -f "$TRAIN/weights/yolo11n.pt" ]; then
    DEFAULT_MODEL="$TRAIN/weights/yolo11n.pt"
fi

# Colour is what separates the two classes, so the saturation/hue augmentation is
# turned well down from the defaults (hsv_s 0.7 would wash blue and yellow into
# each other); brightness still varies because the lighting does.
#
# `project` must be absolute: ultralytics puts a relative one under
# runs/<task>/, which would nest the output as runs/detect/runs/detect/cubes.
"$YOLO" detect train \
    model="${MODEL:-$DEFAULT_MODEL}" \
    data="$TRAIN/dataset/data.yaml" \
    epochs="${EPOCHS:-150}" \
    imgsz="${IMGSZ:-640}" \
    batch="${BATCH:-16}" \
    device="${DEVICE:-cpu}" \
    workers="${WORKERS:-8}" \
    cache=ram \
    project="$TRAIN/runs/detect" \
    name="$RUN_NAME" \
    exist_ok=True \
    hsv_h=0.005 \
    hsv_s=0.2 \
    hsv_v=0.3 \
    patience=40 \
    seed=0 \
    "$@"
