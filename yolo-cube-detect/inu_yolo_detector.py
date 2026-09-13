#!/usr/bin/env python3
"""Run the cube detector for `inu-r132 display`.

The Rust program cannot load a `.pt` (it is a Python pickle), so `display` starts
this script and talks to it over pipes. One process, one or more models.

    python3 inu_yolo_detector.py --model models/cubes-best.pt --conf 0.4

Protocol, both directions, on stdin/stdout:

    u32 big endian length | payload

  * the Rust side sends one JPEG frame per message,
  * this script answers with one UTF-8 JSON message per frame:

        {"detections": [{"model": "cubes-best", "class": "blue_cube",
                         "class_id": 0, "conf": 0.9898,
                         "x1": 538.7, "y1": 635.5, "x2": 844.2, "y2": 978.6}],
         "width": 1600, "height": 1200, "ms": 27.3}

  * before the first frame it sends `{"ready": true, "models": [...]}` so the
    caller can report a load failure straight away.

Anything the libraries print goes to stderr; stdout carries only those messages.
"""

from __future__ import annotations

import argparse
import json
import os
import struct
import sys
import time

# Libraries (ultralytics, torch) like to print to stdout. Give the protocol its
# own handle and point sys.stdout at stderr, so a stray print cannot corrupt a
# frame message.
_PROTOCOL = os.fdopen(os.dup(sys.stdout.fileno()), "wb", buffering=0)
sys.stdout = sys.stderr


def send(payload: dict) -> None:
    data = json.dumps(payload).encode("utf-8")
    _PROTOCOL.write(struct.pack(">I", len(data)))
    _PROTOCOL.write(data)


def read_message(stream) -> bytes | None:
    header = stream.read(4)
    if len(header) < 4:
        return None
    (length,) = struct.unpack(">I", header)
    body = stream.read(length)
    if len(body) < length:
        return None
    return body


def load_models(paths: list[str], task: str):
    from ultralytics import YOLO

    loaded = []
    for path in paths:
        started = time.time()
        model = YOLO(path, task=task)
        names = {int(k): str(v) for k, v in model.names.items()}
        loaded.append({
            "path": path,
            "name": os.path.splitext(os.path.basename(path))[0],
            "model": model,
            "names": names,
        })
        print(f"loaded {path}: {len(names)} classes in {time.time() - started:.1f}s",
              file=sys.stderr, flush=True)
    return loaded


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--model", action="append", default=[], required=True,
                        help="a .pt file; repeat for several")
    parser.add_argument("--conf", type=float, default=0.4, help="confidence floor")
    parser.add_argument("--imgsz", type=int, default=640, help="inference size")
    parser.add_argument("--max-det", type=int, default=20, help="boxes per model")
    parser.add_argument("--task", default="detect", help="ultralytics task")
    args = parser.parse_args()

    try:
        models = load_models(args.model, args.task)
    except Exception as error:  # report it instead of dying silently
        send({"ready": False, "error": f"{type(error).__name__}: {error}"})
        print(f"failed to load a model: {error}", file=sys.stderr, flush=True)
        return 1

    send({
        "ready": True,
        "models": [
            {"path": m["path"], "name": m["name"],
             "classes": {str(k): v for k, v in m["names"].items()}}
            for m in models
        ],
    })

    import numpy as np
    import cv2

    stream = sys.stdin.buffer
    while True:
        jpeg = read_message(stream)
        if jpeg is None:
            break

        started = time.time()
        image = cv2.imdecode(np.frombuffer(jpeg, dtype=np.uint8), cv2.IMREAD_COLOR)
        if image is None:
            send({"error": "cannot decode the JPEG"})
            continue

        height, width = image.shape[:2]
        detections = []
        try:
            for entry in models:
                results = entry["model"].predict(
                    image, conf=args.conf, imgsz=args.imgsz,
                    max_det=args.max_det, verbose=False,
                )
                for result in results:
                    boxes = result.boxes
                    if boxes is None:
                        continue
                    for xyxy, conf, cls in zip(boxes.xyxy.tolist(),
                                               boxes.conf.tolist(),
                                               boxes.cls.tolist()):
                        class_id = int(cls)
                        detections.append({
                            "model": entry["name"],
                            "class": entry["names"].get(class_id, str(class_id)),
                            "class_id": class_id,
                            "conf": round(float(conf), 4),
                            "x1": round(xyxy[0], 1), "y1": round(xyxy[1], 1),
                            "x2": round(xyxy[2], 1), "y2": round(xyxy[3], 1),
                        })
        except Exception as error:
            send({"error": f"{type(error).__name__}: {error}"})
            continue

        send({
            "detections": detections,
            "width": width,
            "height": height,
            "ms": round((time.time() - started) * 1000, 1),
        })

    return 0


if __name__ == "__main__":
    sys.exit(main())
