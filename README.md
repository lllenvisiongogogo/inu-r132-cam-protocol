# inu-r132-cam-protocol

Serve and display the **RGB camera of an Inuitive NU4000** (the `CGS_132` /
"R132" color sensor) from a small Rust CLI.

```
inu-r132 serve         start InuService and publish the camera over TCP
inu-r132 display       show the camera in a window (local or remote)
inu-r132 scan          JSON on stdout: where each cube is and how far away
inu-r132 shot          timestamped JPEG stills, for building a dataset
inu-r132 probe         list every channel and sensor the SDK reports
inu-r132 service-stop  stop Inuitive's InuService daemon
```

`serve` starts Inuitive's `InuService` daemon when it is needed, opens the
requested streams (RGB as JPEG, depth as 16-bit Z16, or `--stream all`) and
publishes them over a tiny line-oriented TCP protocol. Everything else either
opens the camera itself or, more usually, connects to a running `serve`, so a
machine without the SDK can still watch the camera. `display`, the desktop app
and `scan` can also run YOLO models over the colour frames and report the
distance at the centre of each box.

```
src/bin/inu_r132.rs   subcommands: serve / display / scan / shot / probe /
                      service-stop, the detector wiring and the overlay
src/cli.rs            clap definitions
src/inu.rs            safe wrapper around the shim
src/sys.rs            runtime loading of the shim + SDK libraries
src/frame.rs          frame hub, JPEG, Z16 depth, distance sampling
src/protocol.rs       the TCP protocol server (tokio)
src/wire.rs           the wire format
src/detect.rs         the YOLO sidecar: spawn, framing, latest-frame worker
src/overlay.rs        boxes and labels drawn into the viewer buffer
csrc/                 C++ shim over libInuStreams (RGB + depth streams)
build.rs              finds the SDK and builds the shim
display/              Avalonia desktop app wrapping the CLI (see below)
yolo-cube-detect/     the Python sidecar that loads the .pt models
```

The main binary does **not** link InuDev: `build.rs` compiles the shim into
`libinu_shim.so` (`inu_shim.dll` on Windows) and the Rust side `dlopen`s it.
That is what lets `serve` start InuService with no library-path setup.

## Linux deployment and startup

From a fresh machine to a running camera.

| steps | when |
|---|---|
| 1–3 | once per machine: dependencies, the SDK, the toolchain |
| 4–7 | to get running, and after every code change |
| 8 | only if you want object detection |

There is a short [everyday startup](#everyday-startup) at the end, and a
[troubleshooting](#troubleshooting) table after it.

### 1. What you need

| | |
|---|---|
| Camera | the NU4000 on USB. A **vendor-specific** interface, not UVC: there is no `/dev/video*` and nothing else can read it |
| System | Linux. `serve` and `scan` are headless; `display` and the desktop app need a desktop session |
| Toolchain | Rust (rustup) and a C++ compiler — the shim is C++ |
| SDK | Inuitive's Linux `inudev` `.deb`. Not in this repository |
| Optional | .NET 8 SDK, for the desktop app |
| Optional | Python 3 with `ultralytics` for object detection; `make yolo-setup` builds it |

`make setup` only *checks* the toolchain and extracts the SDK. It installs
nothing and never runs as root.

### 2. Toolchain

Debian/Ubuntu names; adapt for another distribution.

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh   # Rust
sudo apt install build-essential binutils tar xz-utils            # C++ and .deb extraction
sudo apt install libx11-6 libxkbcommon0 libwayland-client0        # the viewer windows
sudo apt install dotnet-sdk-8.0                                   # optional: desktop app
```

### 3. The Inuitive SDK

Get `inudev_<version>_amd64.deb` from Inuitive, drop it in the repository root
or `.temp/`, and run:

```bash
make setup
```

That extracts the SDK under `deps/inudev/` and flips the client IPC method to
TCP-local, so a normal user can reach the root-owned daemon:

```xml
<IPCApproach>8</IPCApproach>
```

By hand instead:

```bash
mkdir -p deps/inudev && cd deps/inudev
ar x ../../inudev_4.36.0016.06-1_amd64.deb
tar xJf data.tar.xz
```

`make setup` is idempotent — it reports an already-extracted SDK and stops. Set
`INUDEV_DIR` to use an install somewhere else.

**Without the SDK the build still succeeds.** `build.rs` skips the shim and
prints a cargo warning; `display` and `serve` then say that camera support is
missing. That is what keeps `cargo check`, the Windows skeleton and CI working
on machines without the vendor SDK.

### 4. Build

```bash
make build
```

`cargo build --release`, then the pieces everything else expects are copied into
`build/`:

```
build/inu-r132                 the CLI
build/libinu_shim.so           the C++ shim over the vendor SDK
build/inu_yolo_detector.py     the detector sidecar (object detection only)
```

`make check`, `make build-linux` and `make build-win` build the other variants.

### 5. InuService

`InuService` is Inuitive's daemon: it owns the USB device, boots the firmware and
serves the SDK over IPC. It needs **root** (USB, and raising the kernel usbfs
buffer size) and it **daemonizes itself** — new session, no terminal — so once
started it stays up until killed or the machine reboots.

```bash
make service            # sudo env INUITIVE_PATH=... LD_LIBRARY_PATH=... InuService
make service-status     # pgrep -a -x InuService
make service-stop       # sudo pkill -x InuService
```

or by hand:

```bash
ROOT=$(ls -d deps/inudev/opt/*/Inuitive/InuDev)
sudo env INUITIVE_PATH="$ROOT" LD_LIBRARY_PATH="$ROOT/bin" "$ROOT/bin/InuService"
```

You normally do not have to: `serve`, `display`, `scan` and `shot` start it
through `sudo`, reuse one that is already running, and `--attach` never starts
one. On exit they try `sudo -n pkill` to stop a daemon *they* started, and
otherwise leave it alone.

If the `.deb` was installed normally rather than extracted, Inuitive's own
tooling works too:

```bash
sudo systemctl enable --now inuservice                  # the unit from the .deb
sudo /opt/Inuitive/InuDev/bin/inuservice.sh start|stop|restart
```

The `.deb` post-install runs `SetPermissionRT`, which makes `InuService` setuid
root so a normal user can start it without `sudo`. An extracted-only SDK has no
setuid bit, so `sudo` is needed — or just leave the daemon running.

### 6. Check the camera

```bash
./build/inu-r132 probe
```

This opens the sensor and prints every channel and sensor the SDK reports, as
JSON. Look for `"depth": true`, a `depth` channel and a `general-camera (RGB)`
channel before doing anything else.

### 7. Run

```bash
./build/inu-r132 serve                    # terminal 1: hold the camera, serve it
./build/inu-r132 display --remote         # terminal 2: view that stream
make gui                                  # ...or the desktop app
```

The camera is held by one process at a time; everything else connects to a
running `serve` over TCP, which is what `--remote` is for. Without a value it
means `0.0.0.0:5534`, a `serve` on this machine.

| command | what it does |
|---|---|
| `serve` | starts InuService, opens the streams, serves the protocol |
| `serve --stream all` | opens rgb **and** depth, so clients can switch without a restart |
| `serve --mock` | a synthetic pattern, no SDK and no hardware |
| `display` | the local camera in a window |
| `display --remote HOST:5534` | a served stream in a window |
| `scan` | one JSON line per scan: every cube, its box and its distance |
| `shot` | timestamped JPEG stills under `shot/` |
| `make gui` | the desktop app, which starts and speaks `serve` itself |

`Esc` or closing the window stops `display`; `Ctrl+C` stops `serve`.

### 8. Object detection (optional)

`display`, the desktop app and `scan` can run YOLO `.pt` models over the colour
frames. That needs a Python with `ultralytics`, which is `.venv` at the
repository root - the same environment the training uses:

```bash
make yolo-setup                  # once, ~200 MB of CPU-only torch
make yolo-train                  # label ./train/raw, train, publish to ./models
```

`models/*.pt` is the default model source, relative to the working directory, so
the model `make yolo-train` publishes is picked up with no further setup. To use
an interpreter somewhere else, `export INU_R132_PYTHON=/path/to/.venv/bin/python`.
Detection is skipped with a message when there is no model or no usable Python:
each candidate interpreter is asked (`python -c "import ultralytics"`) before it
is used, so a system Python cannot turn into a confusing `ModuleNotFoundError`
later. See [Object detection](#object-detection) and
[Scanning from a script](#scanning-from-a-script).

### Everyday startup

Once deployed, this is the whole sequence:

```bash
cd inu-r132-cam-protocol
make build                                # only after a code change
./build/inu-r132 serve --stream all       # terminal 1
make gui                                  # terminal 2
```

and to shut down:

```bash
make service-stop                         # sudo pkill -x InuService
```

### Troubleshooting

| symptom | likely cause | fix |
|---|---|---|
| `0x05000033 Call to InuService timed out`, once | the first connection boots the device firmware | retry; `serve` already retries six times |
| the same, every time | a stale daemon or a wedged device | `sudo pkill -x InuService`, then replug the USB cable |
| the same, with a daemon running | InuService is not root, or `INUITIVE_PATH` is wrong | `make service-status`, then `make service-stop && make service` |
| `Inuitive SDK not found` while building | the `.deb` was never extracted | `make setup`, or set `INUDEV_DIR` |
| `no InuService process is running` | nothing started it | `make service`, or run without `--attach` |
| the app cannot find `inu-r132` | the binary is not where it looks | `make build`, or set `INU_R132_BIN` |
| detection: `no Python that can import ultralytics` | no venv was found | step 8 |
| `distance_mm` is always `null` | no depth stream, or `--no-registered` | `serve --stream all`; registered depth is the default |
| `make scan \| jq` sees a stray first line | make echoes the recipe | already silenced with `@`; scripts should call the binary directly |
| the viewer window does not open | no desktop session, or `libx11`/`libxkbcommon` missing | step 2; `serve` and `scan` are headless and still work |

## Taking photos

`shot` saves stills as JPEG into `shot/` next to the program — with the binary
in `build/`, that is `build/shot/`. Each file is named after its capture time in
local time, so the directory sorts chronologically:

```bash
./build/inu-r132 shot                       # one photo -> build/shot/2026-02-14_09-31-07.123.jpg
./build/inu-r132 shot -n 20 --interval 1000 # one photo per second
./build/inu-r132 shot --dir ~/dataset/cubes # somewhere else
./build/inu-r132 shot --mock                # no hardware, writes a test pattern
```

Every written path is printed on stdout, one per line, so a script can collect
the files (e.g. to build a training set for an object detector on the colour
image). `shot` is colour only: it never opens or registers the depth stream.
`--warmup` (default 500 ms) lets auto exposure settle before the first photo,
and each photo of a burst waits for a frame newer than the previous one, so a
burst is never several copies of the same picture. By default `shot` starts its
own `InuService` and stops it again; `--attach` reuses a running daemon.

The Avalonia app has the same thing as a **拍照** button; it saves the frames it
is already receiving, so it can take photos while the preview stays live (see
[Desktop app](#desktop-app-avalonia)).


## Desktop app (Avalonia)

`inu-r132-cam-protocol.sln` at the repository root plus the Avalonia project in
`display/` wrap the CLI in a desktop app. It renders the stream **inside the
window** (it speaks the protocol itself, no minifb and no external viewer) and
controls `serve` for you.

```
inu-r132-cam-protocol.sln   solution at the repo root
display/Display.csproj      Avalonia 11 desktop app (net8.0)
display/Views/              MainWindow, ServiceErrorWindow
display/Services/           InuService check, CliRunner, FrameClient, Z16 maths,
                            PhotoSaver
```

```bash
make gui-build              # dotnet build -c Release
make gui                    # build and run
```

Requires the .NET 8 SDK (or newer). The app finds `inu-r132` next to itself, in
`build/` or `target/release` above it, or from `INU_R132_BIN`.

What it does:

* **InuService gate** — the main window only opens when `InuService` is running.
  Otherwise an error window explains how to start it and offers **重试**.
* **Controls** — start/stop `serve`, connect/disconnect the preview, pick the
  port.
* **Preview** — decodes JPEG (rgb) and raw Z16 (depth) frames and shows them,
  with a live status line (fps, resolution, depth min/median/max and invalid
  ratio) and the child process log.
* **Real time, not queued** — the socket reader only stores the newest frame; a
  worker thread does the JPEG decode and the per pixel depth conversion; the UI
  presents at ~60 Hz from a double buffered result. Frames that arrive while the
  UI is busy are dropped (the status line counts them) instead of building a
  backlog, so the view never drifts behind the camera. Depth pixels are never
  written while the UI copies them.
* **Depth** — `near` / `far` / `gamma` controls and an auto-range checkbox that
  eases the window towards each frame's 2%..98% percentile.
* **Detection** — the 目标检测 checkbox runs the same YOLO sidecar the CLI uses
  and draws its boxes over the preview, with a live `检测:` line (found classes,
  inference time, completed/submitted frames). 模型目录 and conf are editable;
  the directory defaults to `models` relative to the working directory. The
  detector starts lazily on the first colour frame and never blocks the UI: its
  model load happens on its own thread, and a failure is reported in the status
  line and the log instead of stopping the preview. Depth frames are not sent —
  a box found on the colour image would not line up with a different picture, so
  the depth view says `检测: 只在彩色视图显示`.
* **Distance probe** — click anywhere in the picture; a green dot with the
  distance in millimetres follows that point and updates every frame (`无测量`
  when the 3x3 window under it has no valid depth). The dot keeps its place
  when the window is resized or the stream changes. The app subscribes to the
  widest stream the server has (`mix` when both are open) and the selector only
  changes what is drawn, so the probe works in the colour view too.
* **Detection** — a YOLO `.pt` runs over every colour frame and its boxes are
  drawn with their class and confidence, in both the CLI viewer and the desktop
  app. See below.
* **Stream switch** — the rgb/depth/mix selector sends `stream <name>` on the
  open connection. `serve --stream all` opens both, so switching is instant and
  never restarts the camera. If the server only opened one, the command is
  answered with `err ...` and shown in the log.
* **拍照** — the 拍照 button saves the frame that is on screen as a timestamped
  JPEG into the directory in the box (default `shot/` next to the app); 打开目录
  shows it in the file manager. 张数 and 间隔 ms take a burst: the first photo is
  the frame being displayed, and every further photo waits for a frame the
  preview has not shown yet, so a burst is never the same picture twice. It
  writes the JPEG `serve` already sent, so it does not open the camera a second
  time and it works against a remote `serve` too. File naming is identical to
  `inu-r132 shot`.
* **mix** — overlays the depth image on the colour image. Invalid depth pixels
  are transparent, so the colour image shows through the holes. There is **no
  manual offset**: a plain x/y shift cannot align the two cameras, because the
  parallax depends on the distance and on the pixel position, and the two
  lenses have different distortion and focal length. Depth is registered to the
  colour camera by default, so mix already lines up; see below.

## Scanning from a script

`scan` prints what it sees as JSON, for a robot or a script to consume:

```bash
./build/inu-r132 scan                              # one scan of the local camera
./build/inu-r132 scan --pretty                     # indented, for reading
./build/inu-r132 scan --remote -n 10 --interval 200 # ten scans of a running serve
./build/inu-r132 scan --model models/cubes-best.pt --conf 0.5
./build/inu-r132 scan | jq '.objects[] | {label, distance_mm}'
```

One scan is one frame: wait for a fresh colour frame, hand that JPEG to the
detector, measure every box against the newest depth frame, print the JSON — and
with the default `-n 1`, exit. `--warmup` (500 ms) lets the streams settle
first, because the first frames after a cold start still carry the sensor's boot
exposure and the depth stream trails the colour one by a frame or two.

That also means a one-shot run pays the whole start-up cost every time: opening
the camera and booting InuService takes seconds, and `scan` stops the InuService
it started. A loop should either keep one process alive (`-n 100 --interval 200`)
or point at a long-running `serve`:

```bash
inu-r132 serve --stream all                        # terminal 1, holds the camera
inu-r132 scan --remote -n 100 --interval 200       # terminal 2
```

One compact JSON object per line on stdout, so it pipes straight into `jq`;
everything else logs to stderr.

```json
{"seq":1,
 "timestamp":"2026-09-12T12:47:51.078+08:00",
 "image":{"width":1600,"height":1200},
 "depth":{"available":true,"width":1600,"height":1200},
 "detect_ms":27.3,
 "objects":[
   {"model":"cubes-best","label":"blue_cube","class_id":0,"confidence":0.9898,
    "box":{"x1":538.7,"y1":635.5,"x2":844.2,"y2":978.6},
    "centre":{"x":691.5,"y":807.1},
    "distance_mm":412,"depth_samples":9}]}
```

`objects` is always there and empty when nothing was found. `distance_mm` is the
depth at the centre of the box, `null` when that 3x3 window holds no valid
depth, with `depth_samples` (0..9) saying how many of the nine were. Boxes and
centres are in pixels of `image`. Anything that goes wrong exits non-zero with a
message on stderr and no JSON, so a script can just check the exit code.

`scan` opens the local camera unless `--remote` points it at a running `serve` —
useful because only one process can hold the camera, so the GUI (or another
`display`) can keep it while a script scans.

## Object detection

`display` can run one or more YOLO models over the colour frames and draw what
they find:

```bash
make yolo-setup                                    # once
make yolo-train                                    # ./train/raw -> ./models
make build
make display                                       # picks up models/*.pt
make display DETECT_ARGS="--conf 0.5"

./build/inu-r132 display --model models/cubes-best.pt
./build/inu-r132 display --models-dir /opt/models
./build/inu-r132 display --no-detect
```

Without `--model`, every `*.pt` in `--models-dir` (default `models`, relative to
the working directory) is loaded. Each box is drawn in a colour picked from its
class id and labelled `CLASS NN% 412MM`, with a dot marking where that distance
was measured and the same list repeated in the title bar. Several models each
keep their own class names — they travel inside the `.pt`.

**Distance.** The number is the depth at the centre of the box, averaged over
the valid pixels of a 3x3 window: one pixel is noisy and depth has holes, so the
window size and the sample count are reported when it is not a full 3x3
(`412mm (6/9)` in the GUI, `412MM 6/9` in the viewer, which only has an
upper-case ASCII font), and an all-invalid window says `NO DEPTH` / `无测量`
rather than `0mm`. Registered depth shares the colour camera's frame, so the box
centre needs no reprojection; under `--no-registered` the two do not line up and
the distances are meaningless. The GUI samples the same way through
`display/Services/DepthSampler.cs`.

**Why a child process.** A `.pt` is a Python pickle, so the Rust binary cannot
open one. `display` starts `yolo-cube-detect/inu_yolo_detector.py`, writes one
length-prefixed JPEG per message to its stdin and reads length-prefixed JSON
back.

**The interpreter must have `ultralytics`.** It is looked up through
`--python` / `$INU_R132_PYTHON`, then a `.venv` next to the program or in the
working directory, then `python3` and `python` on `PATH`. Each candidate is
*asked* (`python -c "import ultralytics"`) before it is used, because a Python
without it starts the sidecar happily and only then dies on the import — so if
none of them works, the program says exactly that instead of printing a bare
`ModuleNotFoundError`:

```bash
make yolo-setup                                          # the normal way
# or point at any interpreter that can `import ultralytics`:
export INU_R132_PYTHON=/path/to/.venv/bin/python
```

The environment lives at the repository root as `.venv`, so it is found
automatically from any directory, including when the app is launched from an IDE
rather than from make. `make yolo-setup` creates it; `make display` and
`make gui` also pass it through as `INU_R132_PYTHON=$(PYTHON)`.

**It never blocks the view.** Inference runs on its own thread with
latest-frame semantics: a frame that arrives while the detector is busy replaces
the one waiting, so a slow model drops frames instead of slowing the window, and
the last boxes stay on screen until the next answer. The title bar shows
`completed/submitted` frames so a backlog is visible.

Depth frames are never sent to the detector: its boxes were computed on the
colour image and would not line up with a different picture, so the depth view
says `detect: colour view only`. If the models or the interpreter are missing,
`display` explains it and shows the picture without boxes rather than failing.

Verified end to end without a window by two ignored tests, which need the venv
and a model in `models/`:

```bash
make yolo-setup     # the tests discover ./.venv on their own
cargo test -- --ignored --nocapture detector
```

`detector_round_trip` drives the pipe, and `detector_draws_boxes_on_a_photo`
runs a real photo through the model and writes the rendered overlay to
`target/detector-preview.jpg`.

The desktop app has the same feature, with its own `DetectorClient`
(`display/Services/DetectorClient.cs`) speaking the identical protocol — same
sidecar script, same `models/*.pt` default, same latest-frame worker.

## Training

The data, the labelling scripts and the trainer live in `train/`, and
`make yolo-train` runs the whole pipeline:

```bash
make yolo-setup                  # once: the Python environment
make yolo-train                  # assemble -> train -> publish to ./models
make yolo-train EPOCHS=2         # a smoke run in minutes instead of half an hour
make yolo-train MODEL_NAME=test  # publish somewhere else, leave the good one
```

```
train/classes.txt          the class names, one per line; line number = class id
train/raw/<class>/         the photos and your hand-made labels. Source of truth
train/raw/negatives/       photos of objects that must NOT be detected
train/scripts/             assemble_dataset.py, train.sh, pipeline.sh, setup.sh
train/dataset/             generated: images + labels, 80/20 by time, data.yaml
train/runs/                ultralytics output: curves, confusion matrix, weights
train/weights/             the pretrained base and the last published model
```

**Labelling is manual**: each photo has a `.txt` beside it in `train/raw/`
holding `class cx cy w h` per object, and `assemble_dataset.py` only copies,
splits and writes `data.yaml`. `raw/negatives/` needs no label file - those
photos train as "nothing here", which is how a confusable object is taught to be
ignored. Adding a class is a line in `classes.txt`; the names travel inside the
`.pt`, so the camera needs no change.

Publishing overwrites `models/<name>.pt`, keeping the outgoing one as
`models/<name>.previous.pt`. See `train/README.md`.

The shipped model is YOLO11n, `0 = blue_cube`, `1 = yellow_cube`, mAP50 0.995 /
mAP50-95 0.963 on the held-out split.

## What data can be read

`serve` and `display` open the RGB (`CImageStream`) and the **depth**
(`CDepthStream`) stream; the other SDK streams are not wired up yet:

| SDK stream | Data | Status |
| --- | --- | --- |
| `CImageStream` | RGB / BGRA / BGR / RGBA | **done** |
| `CDepthStream` | 16-bit depth in millimetres (Z16) | **done** |
| `CDepthStream` | raw disparity / coloured Z-map | not exposed |
| `CPointCloudStream` | XYZ point cloud (voxelized, with calibration) | not exposed |
| `CStereoImageStream` | the two IR sensors | not exposed |
| `CImuStream`, `CSlamStream`, `CnnStream`, ... | IMU, SLAM, CNN | not exposed |

Which of them work depends on the hardware: they need a channel of the matching
`EChannelType` (1 = general camera/RGB, 3 = stereo/IR, 4 = depth, 6 = disparity)
to be present. `probe` prints exactly what this device reports:

```bash
make service
./build/inu-r132 probe            # starts the daemon if needed
./build/inu-r132 probe --attach   # use the daemon that is already running
```

The JSON lists every channel and sensor plus an `available` summary; opening the
sensor is enough, no stream is started.

### Depth

* 16-bit per pixel, **millimetres**, `0` = "no measurement".
* `serve --stream depth` publishes it as raw little-endian Z16 (codec 2);
  `display --stream depth` renders it. In the local window `1`/`2` switch
  between rgb and depth.
* The window is `[--near, --far]` (default `300..5000` mm), mapped
  near-bright to far-dark through `--gamma` (1.0 linear, below 1.0 brightens
  the middle). Everything outside the window clamps to white/black and invalid
  pixels stay black. A plain `Z / 65535` mapping would be almost black, which is
  why the window exists.
* Keys: `z`/`x` move `far`, `n`/`m` move `near` (any manual change stops auto),
  `g` cycles gamma `1.0 -> 0.5 -> 0.25`, `a` toggles auto range. Auto range
  eases the window towards each frame's 2%..98% percentile, so it follows the
  scene without flickering.
* The title bar shows the window, gamma, auto state and the frame's
  min/median/max plus invalid ratio.
* A scene with a near object and a far background but **nothing in between**
  (for example a face at 0.5 m and a wall at 3 m) is bimodal: any monotonic
  grey mapping shows it as two tones. That is geometry, not the renderer.
  `scripts/depth_stats.py HOST:PORT` prints the raw value distribution so the
  two can be told apart.

### Aligning depth with RGB

The colour camera and the depth (stereo IR) cameras are physically apart, so a
depth pixel and the matching colour pixel are not at the same image position.
A constant x/y offset **cannot** fix this: the parallax is inversely
proportional to distance and varies across the frame, and the two lenses have
different intrinsics and distortion.

The SDK can do it properly, using the factory calibration, and it is the
**default**:

```bash
./build/inu-r132 serve --stream all          # depth is registered
./build/inu-r132 serve --stream all --no-registered   # raw sensor depth
```

Depth is started with `ActivateRegisteredDepth`, which is what Inuitive's own
`pythonsdk/examples/depth_reg_example.py` does. The chip then reprojects the
depth map into the colour camera's frame, so `mix` lines up without any manual
tuning. `--no-registered` opts out and streams the raw depth; the CLI `display`
and the Avalonia app default to registered too (the app's "配准深度" checkbox).

Notes:

* Registered depth is a resampled image: expect its resolution and its holes
  (occlusions) to differ from raw depth.
* If the device or firmware refuses registration the depth stream does not
  start at all, so `serve` falls back once to raw depth and logs a warning
  (`mix` then shows unaligned depth). The `[inu-r132]` lines on stderr say
  whether registration actually ran.
* The app takes the truth from the server: the hello/`status` JSON reports
  `mix_available` and per stream availability, so the status line shows
  `rgb=y/n depth=y/n mix=y/n` and a switch the server rejects is rolled back
  instead of leaving a black preview.
* The alternative is doing it yourself: unproject each depth pixel with the
  depth intrinsics, transform by the depth→colour extrinsics, project with the
  colour intrinsics (and undistort). That needs the calibration data and is
  what `AlgorithmBySDK` registration does inside the SDK.
* `CImageRegisteredStream` (`CreateImageRegisteredStream`) is the other SDK
  entry point for the same job.

## Protocol

Commands are newline terminated UTF-8, one per line:

| Command | Effect |
| --- | --- |
| `status` | one `status <json>` reply, including every stream's availability |
| `subscribe [fps] [stream]` | start receiving `stream` (`rgb`/`depth`/`mix`, default rgb), optionally rate-capped |
| `stream <rgb\|depth\|mix>` | switch the connection to another stream, instantly |
| `unsubscribe` | stop receiving frames |
| `fps <n>` | set this connection's frame rate cap |
| `ping` | replies `pong` |
| `help` | lists the commands |
| `quit` | closes the connection |

`serve --stream all` opens every stream the camera has, so a client can switch
with `stream` without restarting anything. A `stream` command for a stream the
server did not open is answered with `err ...`.

**`mix`** subscribes to rgb and depth at once; the server alternates the two
frame types on the same connection (tell them apart by codec) and the client
overlays them. The two cameras are not co-located, so the client needs an x/y
offset to line them up.

Every server message is a length-prefixed envelope so text replies and binary
frames can share one connection:

```
u32 be  length of the rest of the envelope
u8      type: 0 = text, 1 = frame, 2 = bye
...     payload
```

A frame payload is a 10 byte header followed by codec bytes:

```
u32 be  width
u32 be  height
u8      codec (1 = JPEG, 2 = raw Z16)
u8      flags (reserved, 0)
...     codec payload
```

Codec `1` is a JPEG image (rgb). Codec `2` is one little-endian `u16` per
pixel: depth in millimetres (`0` = no measurement), or disparity
(4 MSB confidence + 12 LSB disparity) if a disparity stream is served.

A minimal client:

```python
import socket, struct

s = socket.create_connection(("127.0.0.1", 5534))
s.sendall(b"subscribe 10\n")
while True:
    (length,) = struct.unpack(">I", s.recv(4))
    data = b""
    while len(data) < length:
        data += s.recv(length - len(data))
    kind, payload = data[0], data[1:]
    if kind == 1:
        w, h, codec, _ = struct.unpack(">IIBB", payload[:10])
        if codec == 1:
            open("frame.jpg", "wb").write(payload[10:])
        else:
            open("depth.z16", "wb").write(payload[10:])   # w*h little endian u16
        print("frame", w, h, codec)
        break
```

## Windows

The flow above is for Linux. On Windows install Inuitive's `InuDriver` (the USB
driver) and the Windows `inudev` SDK, then point `INUDEV_DIR` at the SDK root
before building:

```bat
set INUDEV_DIR=C:\Program Files\Inuitive\InuDev
cargo build --release
```

It is a best-effort skeleton: the crate compiles, the protocol is identical, and
`make build-win` cross-compiles from Linux with mingw-w64. But `InuService` runs
elevated there and the local viewer path is only exercised where the SDK is
installed, so the Linux path is the supported one.

## Notes

- The `CGS_132` sensor is the RGB/color camera; depth, IR, IMU, etc. are not
  read by this program. (See "What data can be read" above for what is exposed
  now.)
- On Linux `InuService` needs root for USB and to raise the kernel usbfs buffer
  size; `serve` runs it through `sudo`.
- The very first connection after starting `serve` can time out: initialization
  boots the on-device firmware, which takes a few seconds. `serve` retries while
  the daemon comes up.
- If `make build` builds without camera support, `Inuitive SDK not found` was
  printed as a cargo warning: set `INUDEV_DIR` and rebuild.

## Releases

`.github/workflows/snapshot.yml` builds Linux and Windows binaries on every
push to `main` and publishes them as a `Snapshot-<sha>-<date>` GitHub release,
like `jaka-cli`. Because the vendor SDK is not in the repository, CI binaries
are built without camera support; build locally with the SDK for a functional
binary.
