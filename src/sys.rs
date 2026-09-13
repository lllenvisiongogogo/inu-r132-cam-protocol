//! Runtime loading of the shim and the Inuitive SDK libraries.
//!
//! The main binary deliberately does not link InuDev, so that `serve` can start
//! without any library path setup. `display`/`serve` load the SDK here:
//! `libInuCommonUtilities` first, then `libInuStreams` (both with global
//! visibility on Linux so the former's stale `DT_RUNPATH` is irrelevant), then
//! the shim itself.
//!
//! Paths are baked in by `build.rs`. When the crate was built without an SDK
//! those are empty and a best-effort runtime discovery is attempted instead,
//! which also honours `INUDEV_DIR` / `INUITIVE_*`.

use std::os::raw::{c_char, c_int, c_void};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};

/// Paths discovered by `build.rs` (passed via `cargo:rustc-env`).
pub mod sdk_paths {
    pub const SHIM_LIB: &str = env!("INU_SHIM_LIB");
    pub const INUDEV_ROOT: &str = env!("INU_INUDEV_ROOT");
    pub const LIB_INUSTREAMS: &str = env!("INU_LIB_INUSTREAMS");
    pub const LIB_INUCOMMON: &str = env!("INU_LIB_INUCOMMON");
    pub const SERVICE_BIN: &str = env!("INU_SERVICE_BIN");
}

/// Opaque handle owned by the C++ shim.
pub enum InuContext {}

#[repr(C)]
pub struct InuOpenOptions {
    pub service_name: *const c_char,
    pub fps: u32,
    pub channel_id: i32,
    pub output_format: i32,
    /// Bitmask of `INU_STREAM_*` to open. 0 means RGB.
    pub streams: u32,
    /// 1 asks the chip to register depth to the RGB camera.
    pub registered_depth: i32,
}

pub type InuFrameCallback = extern "C" fn(
    stream: c_int,
    data: *const u8,
    width: c_int,
    height: c_int,
    stride: c_int,
    format: c_int,
    timestamp: u64,
    user: *mut c_void,
);

type OpenFn = unsafe extern "C" fn(*const InuOpenOptions) -> *mut InuContext;
type SetFrameCallbackFn =
    unsafe extern "C" fn(*mut InuContext, Option<InuFrameCallback>, *mut c_void) -> c_int;
type StartFn = unsafe extern "C" fn(*mut InuContext) -> c_int;
type StopFn = unsafe extern "C" fn(*mut InuContext);
type CloseFn = unsafe extern "C" fn(*mut InuContext);
type LastErrorFn = unsafe extern "C" fn() -> *const c_char;
type ChannelIdFn = unsafe extern "C" fn(*const InuContext) -> u32;
type ChannelCountFn = unsafe extern "C" fn(*const InuContext) -> u32;
type ChannelAtFn = unsafe extern "C" fn(*const InuContext, u32) -> u32;
type ChannelTypeFn = unsafe extern "C" fn(*const InuContext, u32) -> c_int;
type SensorCountFn = unsafe extern "C" fn(*const InuContext) -> u32;
type SensorAtFn = unsafe extern "C" fn(*const InuContext, u32) -> u32;
type SensorModelFn = unsafe extern "C" fn(*const InuContext, u32) -> c_int;
type SensorRoleFn = unsafe extern "C" fn(*const InuContext, u32) -> c_int;
type ActiveStreamsFn = unsafe extern "C" fn(*const InuContext) -> u32;

/// The loaded shim plus the SDK libraries it depends on.
pub struct Shim {
    // Keep every library alive for as long as the function pointers are used.
    _common: Option<PlatformLibrary>,
    _streams: Option<PlatformLibrary>,
    _shim: PlatformLibrary,
    pub open: OpenFn,
    pub set_frame_callback: SetFrameCallbackFn,
    pub start: StartFn,
    pub stop: StopFn,
    pub close: CloseFn,
    pub last_error: LastErrorFn,
    pub channel_id: ChannelIdFn,
    pub channel_count: ChannelCountFn,
    pub channel_at: ChannelAtFn,
    pub channel_type: ChannelTypeFn,
    pub sensor_count: SensorCountFn,
    pub sensor_at: SensorAtFn,
    pub sensor_model: SensorModelFn,
    pub sensor_role: SensorRoleFn,
    pub active_streams: ActiveStreamsFn,
}

unsafe impl Send for Shim {}
unsafe impl Sync for Shim {}

#[cfg(unix)]
type PlatformLibrary = libloading::os::unix::Library;
#[cfg(windows)]
type PlatformLibrary = libloading::Library;

#[cfg(unix)]
unsafe fn open_library(path: &Path) -> Result<PlatformLibrary> {
    use libloading::os::unix::{Library, RTLD_GLOBAL, RTLD_NOW};
    Library::open(Some(path), RTLD_NOW | RTLD_GLOBAL)
        .with_context(|| format!("loading {}", path.display()))
}

#[cfg(windows)]
unsafe fn open_library(path: &Path) -> Result<PlatformLibrary> {
    libloading::Library::new(path).with_context(|| format!("loading {}", path.display()))
}

impl Shim {
    pub fn load() -> Result<Self> {
        let paths = SdkPaths::resolve();

        // The SDK resolves its config/ and firmware under INUITIVE_PATH.
        if let Some(root) = &paths.inudev_root {
            std::env::set_var("INUITIVE_PATH", root);
        }

        let shim_path = paths.shim.clone().ok_or_else(|| {
            anyhow!(
                "the Inuitive shim was not built: no `InuSensor.h` was found at build \
                 time. Extract the `inudev` package under deps/ (see README), set \
                 INUDEV_DIR, and rebuild with `make build`"
            )
        })?;

        // Preload the SDK libraries with global visibility on Linux so the
        // shim's undefined InuDev symbols resolve and stale DT_RUNPATH entries
        // inside libInuStreams do not matter.
        let common = paths
            .common
            .as_deref()
            .map(|p| unsafe { open_library(p) })
            .transpose()?;
        let streams = paths
            .streams
            .as_deref()
            .map(|p| unsafe { open_library(p) })
            .transpose()?;

        unsafe {
            let shim = open_library(&shim_path)?;
            let open: OpenFn = *shim.get(b"inu_open\0")?;
            let set_frame_callback: SetFrameCallbackFn = *shim.get(b"inu_set_frame_callback\0")?;
            let start: StartFn = *shim.get(b"inu_start\0")?;
            let stop: StopFn = *shim.get(b"inu_stop\0")?;
            let close: CloseFn = *shim.get(b"inu_close\0")?;
            let last_error: LastErrorFn = *shim.get(b"inu_last_error\0")?;
            let channel_id: ChannelIdFn = *shim.get(b"inu_channel_id\0")?;
            let channel_count: ChannelCountFn = *shim.get(b"inu_channel_count\0")?;
            let channel_at: ChannelAtFn = *shim.get(b"inu_channel_at\0")?;
            let channel_type: ChannelTypeFn = *shim.get(b"inu_channel_type\0")?;
            let sensor_count: SensorCountFn = *shim.get(b"inu_sensor_count\0")?;
            let sensor_at: SensorAtFn = *shim.get(b"inu_sensor_at\0")?;
            let sensor_model: SensorModelFn = *shim.get(b"inu_sensor_model\0")?;
            let sensor_role: SensorRoleFn = *shim.get(b"inu_sensor_role\0")?;
            let active_streams: ActiveStreamsFn = *shim.get(b"inu_active_streams\0")?;

            Ok(Self {
                _common: common,
                _streams: streams,
                _shim: shim,
                open,
                set_frame_callback,
                start,
                stop,
                close,
                last_error,
                channel_id,
                channel_count,
                channel_at,
                channel_type,
                sensor_count,
                sensor_at,
                sensor_model,
                sensor_role,
                active_streams,
            })
        }
    }
}

/// Where the pieces of the SDK live, after merging the build-time values with
/// the runtime environment and a filesystem scan.
#[derive(Debug, Clone, Default)]
pub struct SdkPaths {
    pub shim: Option<PathBuf>,
    pub inudev_root: Option<PathBuf>,
    pub streams: Option<PathBuf>,
    pub common: Option<PathBuf>,
    pub service: Option<PathBuf>,
}

impl SdkPaths {
    pub fn resolve() -> Self {
        let inudev_root = option(sdk_paths::INUDEV_ROOT).or_else(find_inudev_root);
        let streams = option(sdk_paths::LIB_INUSTREAMS)
            .or_else(|| inudev_root.as_deref().and_then(|r| find_library(r, "InuStreams")));
        let common = option(sdk_paths::LIB_INUCOMMON)
            .or_else(|| inudev_root.as_deref().and_then(|r| find_library(r, "InuCommonUtilities")));
        let service =
            option(sdk_paths::SERVICE_BIN).or_else(|| inudev_root.as_deref().and_then(find_service));
        let shim = option(sdk_paths::SHIM_LIB).or_else(find_shim);
        Self {
            shim,
            inudev_root,
            streams,
            common,
            service,
        }
    }
}

fn option(value: &str) -> Option<PathBuf> {
    if value.is_empty() {
        None
    } else {
        Some(PathBuf::from(value))
    }
}

/// Find the shim next to the executable or in a cargo target directory.
fn find_shim() -> Option<PathBuf> {
    let name = shim_file_name();
    let mut dirs = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            dirs.push(dir.to_path_buf());
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        dirs.push(cwd.join("target/release"));
        dirs.push(cwd.join("target/debug"));
    }
    dirs.into_iter()
        .map(|dir| dir.join(name))
        .find(|path| path.is_file())
}

fn shim_file_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "inu_shim.dll"
    } else if cfg!(target_os = "macos") {
        "libinu_shim.dylib"
    } else {
        "libinu_shim.so"
    }
}

/// Find the SDK root: the directory holding `include/InuSensor.h`, `lib/` and
/// `config/`. Checks `INUDEV_DIR`, the usual install prefixes and any SDK left
/// in `deps/` next to the executable or the current directory.
fn find_inudev_root() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("INUDEV_DIR").map(PathBuf::from) {
        if dir.join("include/InuSensor.h").is_file() {
            return Some(dir);
        }
    }

    let mut prefixes = vec![PathBuf::from("/opt/Inuitive/InuDev")];
    if let Some(program_files) = std::env::var_os("ProgramFiles") {
        prefixes.push(PathBuf::from(program_files).join("Inuitive/InuDev"));
    }
    for prefix in &prefixes {
        if prefix.join("include/InuSensor.h").is_file() {
            return Some(prefix.clone());
        }
    }

    let mut roots = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        roots.push(cwd.join("deps"));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            roots.push(dir.join("deps"));
            roots.push(dir.to_path_buf());
        }
    }

    for root in roots {
        if let Some(header) = scan_for_file(&root, "InuSensor.h", 0, 8) {
            // .../InuDev/include/InuSensor.h -> .../InuDev
            let inu_root = header
                .parent()
                .and_then(Path::parent)
                .map(Path::to_path_buf);
            if let Some(inu_root) = inu_root {
                return Some(inu_root);
            }
        }
    }
    None
}

fn scan_for_file(dir: &Path, name: &str, depth: usize, max_depth: usize) -> Option<PathBuf> {
    if depth > max_depth {
        return None;
    }
    if dir.join(name).is_file() {
        return Some(dir.join(name));
    }
    let entries = std::fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let file_name = entry.file_name();
        let file_name = file_name.to_string_lossy();
        if file_name.starts_with('.') || file_name == "target" || file_name == "inuros2" {
            continue;
        }
        if let Some(found) = scan_for_file(&path, name, depth + 1, max_depth) {
            return Some(found);
        }
    }
    None
}

/// Walk the SDK root for a platform library file (`libX.so`, `X.dll`, ...).
fn find_library(root: &Path, lib: &str) -> Option<PathBuf> {
    let mut dirs = vec![
        root.to_path_buf(),
        root.join("lib"),
        root.join("bin"),
        root.join("lib/x86_64-linux-gnu"),
    ];
    if let Ok(entries) = std::fs::read_dir(root) {
        for entry in entries.flatten() {
            if entry.path().is_dir() {
                dirs.push(entry.path());
            }
        }
    }
    let plain = format!("lib{lib}.so");
    let versioned = format!("lib{lib}.so.");
    let dll = format!("{lib}.dll");
    let dylib = format!("lib{lib}.dylib");
    for dir in &dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name == plain
                || name.starts_with(versioned.as_str())
                || name.eq_ignore_ascii_case(&dll)
                || name == dylib
            {
                return Some(dir.join(entry.file_name()));
            }
        }
    }
    None
}

fn find_service(root: &Path) -> Option<PathBuf> {
    [
        root.join("bin/InuService"),
        root.join("InuService"),
        root.join("bin/InuService.exe"),
        root.join("InuService.exe"),
    ]
    .into_iter()
    .find(|path| path.is_file())
}
