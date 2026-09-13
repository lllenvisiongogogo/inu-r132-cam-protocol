# yolo-cube-detect

The detection half of `inu-r132`: running a YOLO `.pt` over the colour frames
and drawing what it finds.

```
inu_yolo_detector.py   the sidecar: JPEG in on stdin, JSON boxes out on stdout
```

`display` starts it as a child process, because a `.pt` is a Python pickle and
the Rust binary cannot load one. Everything else — discovery, the worker thread,
the overlay — lives in the parent crate:

```
src/detect.rs    spawn, framing, latest-frame worker, JSON, discovery
src/overlay.rs   5x7 font, boxes and labels on the minifb buffer
```

## Protocol

Both directions, on stdin/stdout: `u32 big endian length | payload`.

The parent sends one JPEG per message. The child answers with one JSON message
per frame:

```json
{"detections": [{"model": "cubes-best", "class": "blue_cube", "class_id": 0,
                 "conf": 0.9898, "x1": 538.7, "y1": 635.5,
                 "x2": 844.2, "y2": 978.6}],
 "width": 1600, "height": 1200, "ms": 26.9}
```

The boxes are in the pixel coordinates of the frame they were computed on, so
the parent rescales them if the displayed frame is a different size.

Before the first frame the child sends `{"ready": true, "models": [...]}` so a
model that fails to load is reported before the window opens, or
`{"ready": false, "error": "..."}`.

Anything the libraries print goes to stderr: the script duplicates the real
stdout for the protocol and points `sys.stdout` at stderr, so a stray `print`
cannot corrupt a frame message.

## Running it by hand

```bash
python3 inu_yolo_detector.py --model ../models/cubes-best.pt --conf 0.4
```

It then waits on stdin; `--help` lists the rest. It needs `torch` and
`ultralytics`, which the repository's `../.venv` has — the same environment
`make yolo-setup` creates and `make yolo-train` trains in.

## The model

Trained by `make yolo-train` from `../train/raw` (see `../train/README.md`):
YOLO11n, `0 = blue_cube`, `1 = yellow_cube`, mAP50 0.995 / mAP50-95 0.963, about
27 ms per 1600x1200 frame on CPU. It is published to `../models/`, which is
where `display` and `scan` look by default:

```bash
make yolo-setup && make yolo-train
```
