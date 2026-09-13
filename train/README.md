# train

Training data and scripts for the YOLO detector that finds the **blue** and
**yellow** cubes in the R132 colour camera. The photos come from
`inu-r132 shot` (CLI) or the Avalonia app's **拍照** button — both write
timestamped JPEGs into the folder you pick, so shooting more into
`raw/blue_cube/` and `raw/yellow_cube/` and re-running the pipeline is the whole
workflow.

Run it from the repository root:

```bash
make yolo-setup                  # once: the Python environment
make yolo-train                  # raw -> labels -> training -> ../models/*.pt
make yolo-train EPOCHS=2         # a smoke run in minutes instead of half an hour
```

or without make:

```bash
cd train
bash scripts/pipeline.sh
```

## Layout

```
classes.txt            the class names, one per line
raw/<class>/           the photos and your labels, hand-made. The source of truth
raw/negatives/         photos of objects that must NOT be detected
scripts/assemble_dataset.py  raw/ + labels -> dataset/, chronological split
scripts/train.sh       ultralytics training, one run into runs/detect/cubes
scripts/pipeline.sh    assemble -> train -> publish into ../models/
scripts/setup.sh       create ../.venv and install the environment
dataset/               generated: images/ + labels/{train,val}, data.yaml
runs/                  generated: curves, confusion matrix, weights/best.pt
weights/               the pretrained base (yolo11n.pt) and the last published
                       model, kept so a bad run cannot lose a good checkpoint
```

`dataset/` and `runs/` are build products: delete them and re-run at any time.
`make yolo-train` rebuilds them, and publishing overwrites
`../models/<name>.pt`, keeping the outgoing one as `<name>.previous.pt`.

**The labels are the only thing here that is not reproducible** - they are yours,
they live next to the photos in `raw/`, and they are tracked in git. Everything
else can be thrown away and regenerated.

## Environment

Lives at the **repository root**, `.venv`, not here — the camera runs the same
`ultralytics` through `inu_yolo_detector.py`, so both share one environment and
`INU_R132_PYTHON` is unnecessary.

```bash
make yolo-setup                  # or: bash train/scripts/setup.sh
```

Installs **CPU-only** torch (`torch 2.14.0+cpu`, `torchvision 0.29.0+cpu`) plus
`ultralytics 8.4.147`, on Python 3.14. This machine has no NVIDIA driver, so the
CUDA wheels PyPI serves by default would be ~2 GB of dead weight; the CPU index
is used explicitly. Everything runs on the CPU (an i7-14650HX here), about
11 s per epoch at imgsz 640.

`ultralytics`' own downloader gives up after 30 s per attempt, which this
network does not always beat. If `yolo11n.pt` will not download, fetch it by
hand once — `train.sh` picks up `weights/yolo11n.pt` automatically:

```bash
curl -L --max-time 300 -o weights/yolo11n.pt \
  https://github.com/ultralytics/assets/releases/download/v8.4.0/yolo11n.pt
```

## Labelling

There is **no automatic labelling**. Every box is yours. `assemble_dataset.py`
only copies, splits and writes `data.yaml`; it never invents a box.

```
classes.txt              the class names, one per line. Line number = class id.
                         This is the file labelImg reads and writes.
raw/<class>/<stem>.jpg   the photo
raw/<class>/<stem>.txt   your label: one line per object, `class cx cy w h`,
                         all normalised to 0..1. Same stem as the photo.
raw/negatives/<stem>.jpg no label needed - trained as "nothing here"
```

`cx cy` is the **centre** of the box, `w h` its size, all divided by the image
width and height. An empty `.txt` is legal and means "no objects in this photo".

```bash
../.venv/bin/python scripts/assemble_dataset.py --dry-run   # report only
../.venv/bin/python scripts/assemble_dataset.py             # build dataset/
```

It reports, and exits non-zero for, any photo without a `.txt`, so an unlabelled
shot cannot be trained as an empty one by accident. Malformed lines are listed
and skipped.

`raw/negatives/` is the place for the confusable object - the yellow box with a
printed pattern, say. One photo there becomes an empty label, which is what
teaches the model that this thing is *not* a cube. **This is the fix for the
weakness below.**

The train/val split is **chronological**, per folder: the last fifth of each
folder is validation. A folder is one shooting session, so that is a session the
model has not seen; splitting the whole set at once would put a whole class of a
later session into validation.

### Adding a class

Add a line to `classes.txt` and label some photos with that class id. Nothing
else changes - not the assembler, not the camera, not the code:

```bash
echo green_cube >> classes.txt      # becomes class id 2
```

The class names travel inside the `.pt`, so `display`, the desktop app and
`scan` pick them up without being told.

## Results

The run behind the shipped model (150 epochs, 28.5 min on CPU, best at epoch
126) on 79 training and 19 validation photos:

| class | images | instances | P | R | mAP50 | mAP50-95 |
|---|---|---|---|---|---|---|
| all | 19 | 19 | 1.000 | 1.000 | 0.995 | **0.963** |
| blue_cube | 8 | 8 | 1.000 | 1.000 | 0.995 | 0.995 |
| yellow_cube | 11 | 11 | 1.000 | 1.000 | 0.995 | 0.930 |

Inference is ~27 ms per 1600x1200 frame on CPU, far faster than the camera.
The yellow boxes are looser than the blue ones — consistent with the yellow
cube's pale, glossy surface making its colour mask less precise, which is also
why its labels use a lower saturation floor.

Re-running that model over the 98 raw photos finds the right cube in 88. The ten
exceptions are explained: 7 really do contain both cubes, 2 have a duplicate
yellow box at `conf=0.4`, and 1 is the washed-out photo that got no label.

## Known weakness: it keys on colour

The model has never seen a yellow object that is *not* a cube, so a yellow box
with a printed pattern on the desk is confidently reported as `yellow_cube`. That
is not an architecture problem, it is the dataset: it contains **no negative
images** (`0 backgrounds` in ultralytics' own scan). Shoot the box and drop it in
`raw/negatives/`; the assembler gives it an empty label and the next training run
learns to leave it alone. A third class would be more reliable still if the arm
must never touch it.

## Checking a trained model

```bash
../.venv/bin/yolo detect predict model=weights/cubes-best.pt \
    source=raw/blue_cube/ conf=0.4 save=True project=../runs/predict name=check
```

Predictions are written to `runs/predict/check/`.
