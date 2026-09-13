//! Build script: compile the C++ Inuitive shim as a shared library.
//!
//! The main binary must not depend on InuDev at load time, otherwise it could
//! not even start when `LD_LIBRARY_PATH`/RPATH are not set up. So the shim is
//! built as `libinu_shim.so` (Linux) or `inu_shim.dll` (Windows), and the Rust
//! side loads it at runtime with `dlopen`/`LoadLibrary` after preloading the
//! SDK libraries.
//!
//! The Inuitive SDK is discovered from, in order:
//!   1. `INUDEV_DIR` (a directory containing `include/` and `lib/`)
//!   2. the SDK's own `INUITIVE_*` environment variables
//!   3. any SDK dropped into `deps/`, `vendor/` or `third_party/` (found by
//!      recursively looking for `InuSensor.h`)
//!   4. the usual install prefixes (`/opt/Inuitive/InuDev`, `C:\Program Files\Inuitive`, ...)
//!   5. the headers vendored in `deps/inuros2/sdk/include` (fallback only)
//!
//! When no SDK is found the shim is **not** built and the path variables are
//! emitted empty: the crate still compiles (so Windows/CI skeleton builds and
//! `cargo check` work) and the runtime reports a clear "SDK not found" error.

use std::path::{Path, PathBuf};

const SDK_ENV_VARS: [&str; 5] = [
    "INUITIVE_STREAMS_INCLUDE",
    "INUITIVE_COMMON_INCLUDE",
    "INUITIVE_INUSW_INCLUDE_DIR",
    "INUITIVE_STREAMS_LIBS",
    "INUITIVE_BIN",
];

fn main() {
    println!("cargo:rerun-if-changed=csrc/inu_shim.cpp");
    println!("cargo:rerun-if-changed=csrc/inu_shim.h");
    println!("cargo:rerun-if-env-changed=INUDEV_DIR");
    for var in SDK_ENV_VARS {
        println!("cargo:rerun-if-env-changed={var}");
    }

    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let deps = manifest_dir.join("deps/inuros2");

    // The crate may be cross compiled (e.g. mingw from Linux), so the target
    // comes from cargo, not from the host cfg.
    let target = std::env::var("TARGET").unwrap_or_default();
    let target_windows = target.contains("windows");
    let target_darwin = target.contains("darwin") || target.contains("apple");

    let (vendored_includes, vendored_libs) = discover_vendored(&manifest_dir);

    let include_dirs = existing_dirs(include_candidates(&deps, vendored_includes));
    let Some(include_dir) = include_dirs
        .iter()
        .find(|dir| dir.join("InuSensor.h").is_file())
        .cloned()
    else {
        println!(
            "cargo:warning=Inuitive SDK not found, building without camera support. \
             Extract the `inudev` package under deps/, or set INUDEV_DIR \
             (e.g. INUDEV_DIR=/opt/Inuitive/InuDev), then rebuild. \
             Looked in:"
        );
        for dir in &include_dirs {
            println!("cargo:warning=  {}", dir.display());
        }
        skip_shim();
        return;
    };

    let lib_dirs = existing_dirs(lib_candidates(&deps, vendored_libs));

    let inustreams = find_library_file(&lib_dirs, "InuStreams", target_windows);
    let inucommon = find_library_file(&lib_dirs, "InuCommonUtilities", target_windows);
    let service = find_service(&lib_dirs, target_windows);

    // The SDK root is the parent of the directory holding InuSensor.h, i.e.
    // .../Inuitive/InuDev for the packaged layout.
    let inudev_root = include_dir.parent().unwrap_or(&include_dir).to_path_buf();

    // The headers alone cannot produce a shim for this target: without the
    // matching libraries the link/load would never work. Skip instead.
    if inustreams.is_none() || inucommon.is_none() {
        println!(
            "cargo:warning=found Inuitive headers in {} but no {} libraries for target `{target}`; \
             building without camera support",
            include_dir.display(),
            if target_windows { "Windows (.dll)" } else { "ELF" }
        );
        skip_shim();
        return;
    }

    // `cc` can only produce static libraries, so drive the C++ compiler
    // directly to build the shared shim.
    let shim_lib = PathBuf::from(std::env::var("OUT_DIR").unwrap())
        .join(shim_file_name(target_windows, target_darwin));
    let compiler = {
        let mut probe = cc::Build::new();
        probe.cpp(true);
        probe.get_compiler()
    };
    let mut command = compiler.to_command();
    command
        .arg("-shared")
        .arg("-fPIC")
        .arg("-O2")
        .arg("-std=c++17")
        .arg("-o")
        .arg(&shim_lib);
    for dir in &include_dirs {
        command.arg(format!("-I{}", dir.display()));
    }
    // The bundled SDK headers are noisy under -Wall; silence just those
    // warnings so our own shim stays warning-clean.
    for flag in [
        "-Wno-template-id-cdtor",
        "-Wno-unused-parameter",
        "-Wno-ignored-qualifiers",
        "-Wno-overloaded-virtual",
    ] {
        command.arg(flag);
    }
    command.arg(manifest_dir.join("csrc/inu_shim.cpp"));
    for dir in &lib_dirs {
        command
            .arg(format!("-L{}", dir.display()))
            .arg(format!("-Wl,-rpath,{}", dir.display()));
    }
    // The SDK libraries are optional at link time: a shared object may keep
    // undefined symbols, which the runtime preload resolves. Linking them when
    // found keeps the intent explicit and works on toolchains that need it.
    if inustreams.is_some() {
        command.arg("-lInuStreams");
    }
    if inucommon.is_some() {
        command.arg("-lInuCommonUtilities");
    }

    let status = command
        .status()
        .unwrap_or_else(|e| panic!("failed to run the C++ compiler: {e}"));
    if !status.success() {
        panic!("failed to build {}", shim_lib.display());
    }

    // Copy the shim next to the eventual binary as well, so `build/` and
    // release archives can ship it and the runtime can find it by its side.
    let shipped_shim = std::env::var("OUT_DIR")
        .ok()
        .map(PathBuf::from)
        .and_then(|out| out.ancestors().nth(3).map(Path::to_path_buf))
        .map(|profile| profile.join(shim_file_name(target_windows, target_darwin)))
        .filter(|dest| {
            std::fs::copy(&shim_lib, dest).is_ok()
        })
        .unwrap_or_else(|| shim_lib.clone());

    // Paths the runtime needs in order to preload the SDK and to spawn the
    // service. Empty strings mean "not found".
    emit_path("INU_SHIM_LIB", Some(&shipped_shim));
    emit_path("INU_INUDEV_ROOT", Some(&inudev_root));
    emit_path("INU_LIB_INUSTREAMS", inustreams.as_deref());
    emit_path("INU_LIB_INUCOMMON", inucommon.as_deref());
    emit_path("INU_SERVICE_BIN", service.as_deref());
}

/// Emit "no SDK" for every path the runtime reads.
fn skip_shim() {
    emit_path("INU_SHIM_LIB", None);
    emit_path("INU_INUDEV_ROOT", None);
    emit_path("INU_LIB_INUSTREAMS", None);
    emit_path("INU_LIB_INUCOMMON", None);
    emit_path("INU_SERVICE_BIN", None);
}

fn shim_file_name(target_windows: bool, target_darwin: bool) -> &'static str {
    if target_windows {
        "inu_shim.dll"
    } else if target_darwin {
        "libinu_shim.dylib"
    } else {
        "libinu_shim.so"
    }
}

fn emit_path(key: &str, path: Option<&Path>) {
    println!(
        "cargo:rustc-env={key}={}",
        path.map(|p| p.display().to_string()).unwrap_or_default()
    );
}

fn include_candidates(deps: &Path, vendored: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut dirs = from_env(
        &[
            "INUITIVE_STREAMS_INCLUDE",
            "INUITIVE_COMMON_INCLUDE",
            "INUITIVE_INUSW_INCLUDE_DIR",
        ],
        true,
    );

    if let Some(base) = std::env::var_os("INUDEV_DIR").map(PathBuf::from) {
        dirs.push(base.join("include"));
        dirs.push(base.join("include/InuDev"));
        dirs.push(base.join("include/InuCommon"));
    }

    // A self-contained SDK checked into the project comes next, before any
    // system-wide install, so the vendored headers match its own libraries.
    dirs.extend(vendored);

    for prefix in [
        "/opt/Inuitive/InuDev/include",
        "/opt/Inuitive/InuDev/include/InuDev",
        "/opt/Inuitive/InuDev/include/InuCommon",
        "/usr/include/InuDev",
        "/usr/local/include/InuDev",
    ] {
        dirs.push(PathBuf::from(prefix));
    }
    if let Some(program_files) = std::env::var_os("ProgramFiles") {
        let base = PathBuf::from(program_files).join("Inuitive");
        dirs.push(base.join("InuDev/include"));
        dirs.push(base.join("InuDev/include/InuDev"));
    }

    // Fallback: headers shipped with the inuros2 checkout.
    dirs.push(deps.join("sdk/include"));
    dirs.push(deps.join("inucommon/include"));

    dirs
}

fn lib_candidates(deps: &Path, vendored: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut dirs = from_env(&["INUITIVE_STREAMS_LIBS", "INUITIVE_BIN"], true);

    if let Some(base) = std::env::var_os("INUDEV_DIR").map(PathBuf::from) {
        dirs.push(base.join("lib"));
        dirs.push(base.join("bin"));
    }

    dirs.extend(vendored);

    for prefix in [
        "/opt/Inuitive/InuDev/lib",
        "/opt/Inuitive/InuDev/bin",
        "/opt/Inuitive/InuDev/lib/x86_64-linux-gnu",
        "/usr/lib",
        "/usr/lib64",
        "/usr/lib/x86_64-linux-gnu",
        "/usr/lib/aarch64-linux-gnu",
        "/lib/x86_64-linux-gnu",
        "/lib/aarch64-linux-gnu",
        "/usr/local/lib",
    ] {
        dirs.push(PathBuf::from(prefix));
    }
    if let Some(program_files) = std::env::var_os("ProgramFiles") {
        let base = PathBuf::from(program_files).join("Inuitive");
        dirs.push(base.join("InuDev/lib"));
        dirs.push(base.join("InuDev/bin"));
    }

    // Prebuilt artifacts of the vendored inuros2 checkout, if any.
    for platform in ["linux_gcc-7.4_x86_64", "linux_gcc-7.3_armv8"] {
        dirs.push(deps.join("sdk/bin").join(platform));
        dirs.push(deps.join("inucommon/bin").join(platform));
    }

    dirs
}

/// Recursively look for an Inuitive SDK dropped anywhere under `deps/`,
/// `vendor/` or `third_party/`. Returns `(include_dirs, lib_dirs)`.
fn discover_vendored(manifest_dir: &Path) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let mut includes = Vec::new();
    let mut libs = Vec::new();
    for name in ["deps", "vendor", "third_party"] {
        let root = manifest_dir.join(name);
        if root.is_dir() {
            scan_for_sdk(&root, 0, 8, &mut includes, &mut libs);
        }
    }
    (includes, libs)
}

fn scan_for_sdk(
    dir: &Path,
    depth: usize,
    max_depth: usize,
    includes: &mut Vec<PathBuf>,
    libs: &mut Vec<PathBuf>,
) {
    if depth > max_depth {
        return;
    }

    if dir.join("InuSensor.h").is_file() {
        includes.push(dir.to_path_buf());
    }
    if find_library_file(std::slice::from_ref(&dir.to_path_buf()), "InuStreams", false).is_some()
        || find_library_file(std::slice::from_ref(&dir.to_path_buf()), "InuStreams", true).is_some()
    {
        libs.push(dir.to_path_buf());
    }

    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        // Skip dot dirs, cargo output, and the inuros2 ROS wrapper (its headers
        // are only a last-resort fallback and would clash with the real SDK).
        if name.starts_with('.') || name == "target" || name == "inuros2" {
            continue;
        }
        scan_for_sdk(&path, depth + 1, max_depth, includes, libs);
    }
}

fn from_env(vars: &[&str], paths: bool) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for var in vars {
        let Ok(value) = std::env::var(var) else {
            continue;
        };
        if paths {
            out.extend(std::env::split_paths(&value));
        } else {
            out.push(PathBuf::from(value));
        }
    }
    out
}

fn existing_dirs(dirs: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut seen = Vec::new();
    for dir in dirs {
        if dir.is_dir() && !seen.contains(&dir) {
            seen.push(dir);
        }
    }
    seen
}

/// Match the library file names of the target platform: `libX.so`, `X.dll` or
/// `libX.dylib`. The host is irrelevant, the crate may be cross compiled.
fn find_library_file(dirs: &[PathBuf], lib: &str, windows: bool) -> Option<PathBuf> {
    let plain = if windows {
        format!("{lib}.dll")
    } else {
        format!("lib{lib}.so")
    };
    let versioned = format!("lib{lib}.so.");
    let dylib = format!("lib{lib}.dylib");
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let matches = if windows {
                name.eq_ignore_ascii_case(&plain)
            } else {
                name == plain || name.starts_with(versioned.as_str()) || name == dylib
            };
            if matches {
                return Some(dir.join(entry.file_name()));
            }
        }
    }
    None
}

fn find_service(dirs: &[PathBuf], windows: bool) -> Option<PathBuf> {
    let names = if windows {
        ["InuService.exe", "InuService"]
    } else {
        ["InuService", "InuService.exe"]
    };
    dirs.iter()
        .flat_map(|dir| names.iter().map(move |name| dir.join(name)))
        .find(|candidate| candidate.is_file())
}
