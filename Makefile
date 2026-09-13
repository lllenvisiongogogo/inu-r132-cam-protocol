# inu-r132-cam-protocol
#
# setup        install/verify the dependencies for the current system
# build        release build of the current platform into build/
# clean        remove build/ and the cargo artifacts
# serve        start InuService and serve the camera protocol
# display      show the camera in a window (cli, minifb)
# yolo-train   label ./train/raw, train, publish into ./models/
# train-label  hand-label train/raw in a window (no camera needed)
# yolo-setup   create the Python environment for training and the detector
# scan         print where the cubes are and how far, as JSON
# shot         save timestamped JPEG photos under build/shot/
# gui          build and run the Avalonia desktop app (display/)
# gui-build    build the Avalonia desktop app only
# service      start Inuitive's InuService daemon (sudo)
# service-status  show whether InuService is running
# service-stop stop Inuitive's InuService daemon

DEPS_DIR    := deps
INUDEV_DIR  ?= $(DEPS_DIR)/inudev
# The vendor .deb is not in the repo; pick one up from the usual drop points.
INUDEV_DEB  ?= $(firstword $(wildcard *.deb) $(wildcard .temp/*.deb) $(wildcard ../inu-cam/.temp/*.deb))
PORT        ?= 5534
REMOTE      ?= 0.0.0.0:5534
# Extra arguments for `make shot`, e.g. make shot SHOT_ARGS="-n 20 --interval 1000"
SHOT_ARGS   ?=
DETECT_ARGS ?=
SCAN_ARGS   ?=
# The Python `display` and `scan` use to run .pt models, and the one training
# runs in. $INU_R132_PYTHON from the environment always wins inside the program.
PYTHON      ?= $(firstword $(wildcard .venv/bin/python) $(wildcard .venv/Scripts/python.exe))
# The extracted SDK root, e.g. deps/inudev/opt/inudev-<ver>/Inuitive/InuDev
INUDEV_ROOT := $(firstword $(wildcard $(INUDEV_DIR)/opt/*/Inuitive/InuDev) $(wildcard /opt/Inuitive/InuDev))

UNAME_S := $(shell uname -s 2>/dev/null)
IS_WINDOWS := $(if $(findstring MINGW,$(UNAME_S)),1,$(if $(findstring MSYS,$(UNAME_S)),1,$(if $(findstring CYGWIN,$(UNAME_S)),1,)))

.PHONY: setup setup-linux setup-windows check build build-linux build-win \
        serve display scan shot gui gui-build service service-status service-stop clean clean-deps \
        yolo-train yolo-setup train-label

SLN := inu-r132-cam-protocol.sln
GUI_BIN := display/bin/Release/net8.0/inu-r132-display

# ---------------------------------------------------------------- setup ----

ifeq ($(IS_WINDOWS),1)
setup: setup-windows
else ifeq ($(UNAME_S),Linux)
setup: setup-linux
else
setup:
	@echo "Unsupported system '$(UNAME_S)'. On Windows use MSYS2/WSL, then re-run make setup"
	@exit 1
endif

setup-linux:
	@command -v cargo >/dev/null 2>&1 || { \
		echo "Rust toolchain not found. Install it from https://rustup.rs"; exit 1; }
	@command -v g++  >/dev/null 2>&1 || command -v c++ >/dev/null 2>&1 || { \
		echo "A C++ compiler (g++/clang++) is required to build the Inuitive shim"; exit 1; }
	@echo "Rust and a C++ compiler are present."
	@if [ -f "$(INUDEV_DIR)/include/InuSensor.h" ] || \
	    ls $(INUDEV_DIR)/opt/*/Inuitive/InuDev/include/InuSensor.h >/dev/null 2>&1; then \
		echo "Inuitive SDK already extracted under $(INUDEV_DIR)"; \
	elif [ -n "$(INUDEV_DEB)" ] && [ -f "$(INUDEV_DEB)" ]; then \
		echo "Extracting $(INUDEV_DEB) into $(INUDEV_DIR)..."; \
		rm -rf "$(INUDEV_DIR)"; mkdir -p "$(INUDEV_DIR)"; \
		( cd "$(INUDEV_DIR)" && ar x "$(abspath $(INUDEV_DEB))" ); \
		data=$$(ls "$(INUDEV_DIR)"/data.tar.* 2>/dev/null | head -n 1); \
		case "$$data" in \
			*.xz)  tar xJf "$$data" -C "$(INUDEV_DIR)" ;; \
			*.zst) tar --zstd -xf "$$data" -C "$(INUDEV_DIR)" ;; \
			*.gz)  tar xzf "$$data" -C "$(INUDEV_DIR)" ;; \
			*)     tar xf "$$data" -C "$(INUDEV_DIR)" ;; \
		esac; \
		rm -f "$(INUDEV_DIR)"/data.tar.* "$(INUDEV_DIR)"/control.tar.* "$(INUDEV_DIR)"/debian-binary; \
		echo "SDK extracted. build.rs finds it automatically."; \
	else \
		echo "Inuitive SDK not found under $(INUDEV_DIR)."; \
		echo "Get the Linux inudev .deb from Inuitive and either drop it in the"; \
		echo "repo root / .temp (make setup extracts it), or extract it yourself:"; \
		echo "    mkdir -p $(INUDEV_DIR) && cd $(INUDEV_DIR) && ar x <path>.deb && tar xJf data.tar.xz"; \
		echo "or point INUDEV_DIR=<dir> at an existing install."; \
	fi
	@if ls $(INUDEV_DIR)/opt/*/Inuitive/InuDev/config/InuStreamsParams.xml >/dev/null 2>&1; then \
		sed -i 's#<IPCApproach>1</IPCApproach>#<IPCApproach>8</IPCApproach>#' \
			$(INUDEV_DIR)/opt/*/Inuitive/InuDev/config/InuStreamsParams.xml; \
		echo "Set the client IPC method to TCP-local (IPCApproach=8) so a normal user can reach InuService."; \
	fi

setup-windows:
	@command -v cargo >/dev/null 2>&1 || { \
		echo "Rust toolchain not found. Install it from https://rustup.rs"; exit 1; }
	@echo "Rust is present."
	@if [ -f "$(INUDEV_DIR)/include/InuSensor.h" ]; then \
		echo "Inuitive SDK found under $(INUDEV_DIR)"; \
	elif [ -n "$$INUDEV_DIR" ] && [ -f "$$INUDEV_DIR/include/InuSensor.h" ]; then \
		echo "Inuitive SDK found at INUDEV_DIR=$$INUDEV_DIR"; \
	else \
		echo "Inuitive Windows SDK not found."; \
		echo "Install Inuitive's InuDriver (USB) and the Windows inudev SDK, then point"; \
		echo "INUDEV_DIR at the SDK root, e.g."; \
		echo "    set INUDEV_DIR=C:\\Program Files\\Inuitive\\InuDev"; \
		echo "Only the Linux SDK ships a normal-user InuService IPC; on Windows"; \
		echo "InuService runs elevated and the client connects over the named pipe."; \
	fi

# ---------------------------------------------------------------- build ----

check:
	cargo check --all-targets

# Current-platform release build, output plus the shim (if any) into build/
# The destination is removed first: a still running inu-r132 (display/serve)
# holds the old binary open and cp would fail with "Text file busy".
build:
	cargo build --release
	mkdir -p build
	@rm -f build/inu-r132 build/inu-r132.exe
	@if [ -f target/release/inu-r132.exe ]; then \
		cp target/release/inu-r132.exe build/; \
	else \
		cp target/release/inu-r132 build/; \
	fi
	@for lib in target/release/libinu_shim.so target/release/libinu_shim.dylib target/release/inu_shim.dll; do \
		if [ -f "$$lib" ]; then cp "$$lib" build/; echo "Copied $$(basename $$lib) to build/"; fi; \
	done
	@if [ -f yolo-cube-detect/inu_yolo_detector.py ]; then \
		cp yolo-cube-detect/inu_yolo_detector.py build/; \
		echo "Copied inu_yolo_detector.py to build/"; fi
	@echo "Built build/inu-r132"

# Linux release build (native or from WSL)
build-linux:
ifeq ($(UNAME_S),Linux)
	cargo build --release
	mkdir -p build/linux
	@rm -f build/linux/inu-r132
	cp target/release/inu-r132 build/linux/
	@[ -f target/release/libinu_shim.so ] && cp target/release/libinu_shim.so build/linux/ || true
	@[ -f yolo-cube-detect/inu_yolo_detector.py ] && cp yolo-cube-detect/inu_yolo_detector.py build/linux/ || true
else
	@echo "Cross-compiling to Linux from $(UNAME_S) is not supported"
	@echo "Build on a Linux machine or inside WSL instead"
endif

# Windows release build. Native MSYS2 uses the default toolchain, Linux
# cross-compiles with mingw-w64 (the gnu target). The shim is only built when
# a Windows Inuitive SDK is discoverable.
build-win:
ifeq ($(UNAME_S),Linux)
	@command -v x86_64-w64-mingw32-gcc >/dev/null 2>&1 || { \
		echo "mingw-w64 not found, install it (e.g. pacman -S mingw-w64-gcc)"; exit 1; }
	cargo build --release --target x86_64-pc-windows-gnu
	mkdir -p build/win
	@rm -f build/win/inu-r132.exe
	cp target/x86_64-pc-windows-gnu/release/inu-r132.exe build/win/
	@[ -f target/x86_64-pc-windows-gnu/release/inu_shim.dll ] && \
		cp target/x86_64-pc-windows-gnu/release/inu_shim.dll build/win/ || true
	@[ -f yolo-cube-detect/inu_yolo_detector.py ] && cp yolo-cube-detect/inu_yolo_detector.py build/win/ || true
else
	cargo build --release
	mkdir -p build/win
	@rm -f build/win/inu-r132.exe
	@if [ -f target/release/inu-r132.exe ]; then cp target/release/inu-r132.exe build/win/; fi
	@[ -f target/release/inu_shim.dll ] && cp target/release/inu_shim.dll build/win/ || true
endif

# ------------------------------------------------------------------ run ----

serve:
	./build/inu-r132 serve --port $(PORT) --attach --stream all

# Add DETECT_ARGS for the detector, e.g. make display DETECT_ARGS="--conf 0.5"
display:
	@$(if $(PYTHON),INU_R132_PYTHON=$(PYTHON)) ./build/inu-r132 display $(if $(REMOTE),--remote $(REMOTE),) $(DETECT_ARGS)

# Detect and print JSON. Put everything in SCAN_ARGS, including --remote, since
# a default REMOTE would otherwise be appended to every scan:
#   make scan SCAN_ARGS="--pretty"
#   make scan SCAN_ARGS="--remote 0.0.0.0:5534"
scan:
	@$(if $(PYTHON),INU_R132_PYTHON=$(PYTHON)) ./build/inu-r132 scan $(SCAN_ARGS)

# Timestamped stills into build/shot/, e.g.
#   make shot SHOT_ARGS="-n 20 --interval 1000"
shot:
	@./build/inu-r132 shot $(SHOT_ARGS)

# ---------------------------------------------------------------- model ----

# Label ./train/raw, train, and publish the best weights into ./models/, which
# is where `display` and `scan` look by default. EPOCHS and the rest of train.sh
# variables pass through from the command line:
#   make yolo-train
#   make yolo-train EPOCHS=2          # a quick smoke run, minutes not hours
yolo-train:
	@bash train/scripts/pipeline.sh

# Hand-label train/raw: directories on the left, the photo on the right, paging
# along the bottom and the training button top right. Every edit is saved beside
# the photo straight away. Needs no InuService - it is offline work.
train-label: gui-build
	@$(GUI_BIN) --label

# The Python environment both the training and the detector use.
yolo-setup:
	@bash train/scripts/setup.sh

# Avalonia desktop app: embeds the preview, controls `serve`, and refuses to
# open the main window until InuService is running.
gui-build:
	dotnet build $(SLN) -c Release

# Object detection needs a Python with ultralytics; hand the app the same one
# the CLI uses. Without it the app still runs, it just says so and shows no boxes.
gui: gui-build
	@$(if $(PYTHON),INU_R132_PYTHON=$(PYTHON)) $(GUI_BIN)

service: service-start

# Start Inuitive's InuService from the extracted SDK. It daemonizes itself, so
# this returns and the daemon keeps running until service-stop / reboot.
service-start:
	@if [ -z "$(INUDEV_ROOT)" ] || [ ! -x "$(INUDEV_ROOT)/bin/InuService" ]; then \
		echo "InuService not found. Run 'make setup' first, or set INUDEV_DIR"; exit 1; \
	fi
	@if pgrep -x InuService >/dev/null 2>&1; then \
		echo "InuService is already running:"; pgrep -a -x InuService; \
	else \
		echo "Starting $(INUDEV_ROOT)/bin/InuService (sudo may ask for your password)..."; \
		sudo env INUITIVE_PATH="$(INUDEV_ROOT)" LD_LIBRARY_PATH="$(INUDEV_ROOT)/bin" \
			"$(INUDEV_ROOT)/bin/InuService"; \
		sleep 1; \
		if pgrep -a -x InuService; then :; else \
			echo "InuService did not stay up, check the output above"; exit 1; \
		fi; \
	fi

service-status:
	@pgrep -a -x InuService || echo "InuService is not running"

service-stop:
	-@if command -v pkill >/dev/null 2>&1; then sudo pkill -x InuService; fi

# ---------------------------------------------------------------- clean ----

clean:
	rm -rf build
	cargo clean
	rm -rf display/bin display/obj

clean-deps:
	rm -rf "$(INUDEV_DIR)"
