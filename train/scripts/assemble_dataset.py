#!/usr/bin/env python3
"""Build `dataset/` from your photos and the labels you wrote by hand.

Nothing here generates a box. Every label is a `.txt` next to its photo in
`raw/`, in YOLO format: one line per object, `class cx cy w h`, all normalised
to 0..1. This script copies, splits and writes `data.yaml` - that is all it does.

    raw/<class>/<stem>.jpg      the photo
    raw/<class>/<stem>.txt      your label for it
    raw/negatives/<stem>.jpg    no label needed: trained as "nothing here"

`classes.txt` is the only place the class names live, one per line, and the line
number *is* the class id - the same file labelImg reads and writes. Adding a
class is adding a line, not changing code.

A photo with no `.txt` is reported and skipped, so an unlabelled shot can never
be trained as an empty one by accident.

    python3 scripts/assemble_dataset.py
    python3 scripts/assemble_dataset.py --dry-run
    python3 scripts/assemble_dataset.py --val-every 5
"""

from __future__ import annotations

import argparse
import shutil
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
RAW = ROOT / "raw"
DATASET = ROOT / "dataset"
CLASSES_FILE = ROOT / "classes.txt"

# A subdirectory with this name needs no label files: its photos are empty
# labels, i.e. examples of "there is nothing to find here".
NEGATIVES = "negatives"

PHOTO_SUFFIXES = {".jpg", ".jpeg", ".png"}


def read_classes() -> list[str]:
    if not CLASSES_FILE.is_file():
        raise SystemExit(
            f"{CLASSES_FILE} is missing. It holds the class names, one per line, "
            "and the line number is the class id."
        )
    names = [line.strip() for line in CLASSES_FILE.read_text().splitlines()]
    names = [name for name in names if name and not name.startswith("#")]
    if not names:
        raise SystemExit(f"{CLASSES_FILE} has no class names in it")
    return names


def read_label(path: Path, class_count: int) -> tuple[list[str], list[str]]:
    """The valid lines of a label file, and a complaint per bad line."""
    good: list[str] = []
    problems: list[str] = []
    for number, line in enumerate(path.read_text().splitlines(), start=1):
        line = line.strip()
        if not line:
            continue
        parts = line.split()
        if len(parts) != 5:
            problems.append(f"{path.name}:{number}: expected 5 fields, found {len(parts)}")
            continue
        try:
            class_id = int(parts[0])
            values = [float(value) for value in parts[1:]]
        except ValueError:
            problems.append(f"{path.name}:{number}: not a number: {line}")
            continue
        if not 0 <= class_id < class_count:
            problems.append(f"{path.name}:{number}: class {class_id} is not in classes.txt")
            continue
        if any(not 0.0 <= value <= 1.0 for value in values):
            problems.append(f"{path.name}:{number}: {line} is not normalised to 0..1")
            continue
        if values[2] <= 0.0 or values[3] <= 0.0:
            problems.append(f"{path.name}:{number}: zero width or height")
            continue
        good.append(f"{class_id} " + " ".join(f"{value:.6f}" for value in values))
    return good, problems


def main() -> int:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    parser.add_argument("--dry-run", action="store_true", help="report only, write nothing")
    parser.add_argument(
        "--val-every", type=int, default=5,
        help="every Nth photo of a folder, chronologically, is validation (default 5)",
    )
    args = parser.parse_args()

    classes = read_classes()
    if not RAW.is_dir():
        raise SystemExit(f"no {RAW}")

    folders = sorted(path for path in RAW.iterdir() if path.is_dir())
    if not folders:
        raise SystemExit(f"no photo folders under {RAW}")

    if not args.dry_run:
        for stale in (DATASET / "images", DATASET / "labels"):
            if stale.exists():
                shutil.rmtree(stale)
        for folder in ("images/train", "images/val", "labels/train", "labels/val"):
            (DATASET / folder).mkdir(parents=True, exist_ok=True)

    unlabelled: list[str] = []
    problems: list[str] = []
    counts = {"train": 0, "val": 0}
    boxes = {"train": 0, "val": 0}
    negatives = 0
    empty_labels = 0

    for folder in folders:
        photos = sorted(
            path for path in folder.iterdir() if path.suffix.lower() in PHOTO_SUFFIXES
        )
        if not photos:
            continue
        is_negatives = folder.name == NEGATIVES

        # Chronological, and per folder: a folder is one shooting session, so the
        # last fifth of it is a session the model has not seen. Splitting the
        # whole set at once would put every class of a later session in val.
        val_start = max(1, len(photos) - max(1, len(photos) // max(2, args.val_every)))

        for index, photo in enumerate(photos):
            if is_negatives:
                lines: list[str] = []
            else:
                label = photo.with_suffix(".txt")
                if not label.is_file():
                    unlabelled.append(str(photo.relative_to(ROOT)))
                    continue
                lines, bad = read_label(label, len(classes))
                problems.extend(f"{label.relative_to(ROOT)}: {line}" for line in bad)

            split = "val" if index >= val_start else "train"
            counts[split] += 1
            boxes[split] += len(lines)
            if not lines:
                empty_labels += 1
                if is_negatives:
                    negatives += 1

            if not args.dry_run:
                shutil.copy2(photo, DATASET / "images" / split / photo.name)
                numbered = "".join(f"{line}\n" for line in lines)
                (DATASET / "labels" / split / f"{photo.stem}.txt").write_text(numbered)

    print(f"{len(classes)} classes: {', '.join(classes)}")
    print(f"{counts['train']} train / {counts['val']} val photos, "
          f"{boxes['train']} / {boxes['val']} boxes")
    print(f"{empty_labels} photo(s) with no objects ({negatives} from {NEGATIVES}/)")

    if problems:
        print(f"\n{len(problems)} bad label line(s), skipped:")
        for problem in problems[:20]:
            print(f"  {problem}")
        if len(problems) > 20:
            print(f"  ... and {len(problems) - 20} more")
    if unlabelled:
        print(f"\n{len(unlabelled)} photo(s) have no label file, skipped:")
        for photo in unlabelled[:20]:
            print(f"  {photo}")
        if len(unlabelled) > 20:
            print(f"  ... and {len(unlabelled) - 20} more")

    if not args.dry_run:
        (DATASET / "data.yaml").write_text(
            "# generated by scripts/assemble_dataset.py from raw/ and classes.txt\n"
            f"path: {DATASET}\n"
            "train: images/train\n"
            "val: images/val\n"
            "names:\n"
            + "".join(f"  {index}: {name}\n" for index, name in enumerate(classes))
        )
        print(f"\nwrote {DATASET}")

    # A photo that lost its label is the one mistake worth failing on.
    return 1 if unlabelled else 0


if __name__ == "__main__":
    raise SystemExit(main())
