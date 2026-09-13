//! Running the YOLO detector as a child process.
//!
//! A `.pt` is a Python pickle, so the Rust binary cannot load one. `display`
//! therefore starts `inu_yolo_detector.py` (which has torch and ultralytics) and
//! talks to it over its stdin/stdout: one length-prefixed JPEG per message in,
//! one length-prefixed JSON message with the boxes out.
//!
//! Inference happens on a worker thread with *latest frame* semantics: the
//! render loop never waits for it, a frame that arrives while the detector is
//! busy replaces the one that is waiting, and the boxes stay on screen until the
//! next answer arrives. That keeps the window at full frame rate no matter how
//! slow the model is.

use std::collections::VecDeque;
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use log::{debug, info, warn};

/// How many stderr lines of the sidecar to keep for error messages.
const STDERR_TAIL: usize = 12;

/// One detected box, in the pixel coordinates of the frame it was found in.
#[derive(Clone, Debug, PartialEq)]
pub struct Detection {
    pub model: String,
    pub label: String,
    pub class_id: i32,
    pub conf: f32,
    pub x1: f32,
    pub y1: f32,
    pub x2: f32,
    pub y2: f32,
}

/// A model the sidecar loaded.
#[derive(Clone, Debug)]
pub struct ModelInfo {
    pub name: String,
    pub path: String,
    pub classes: Vec<(i32, String)>,
}

/// What the detector most recently produced.
#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub detections: Vec<Detection>,
    /// Size of the frame the detections were computed on, for rescaling.
    pub width: usize,
    pub height: usize,
    /// Round trip of the last answer, in milliseconds.
    pub millis: f32,
    /// Set when the sidecar failed; detections are empty from then on.
    pub error: Option<String>,
}

#[derive(Clone, Debug)]
pub struct DetectorConfig {
    pub python: PathBuf,
    pub script: PathBuf,
    pub models: Vec<PathBuf>,
    pub conf: f32,
    pub imgsz: u32,
    pub max_det: u32,
}

struct Inner {
    pending: Option<Vec<u8>>,
    snapshot: Snapshot,
    models: Vec<ModelInfo>,
    ready: bool,
    stop: bool,
    submitted: u64,
    completed: u64,
    dropped: u64,
}

impl Inner {
    fn new() -> Self {
        Self {
            pending: None,
            snapshot: Snapshot::default(),
            models: Vec::new(),
            ready: false,
            stop: false,
            submitted: 0,
            completed: 0,
            dropped: 0,
        }
    }
}

struct Shared {
    inner: Mutex<Inner>,
    wake: Condvar,
}

impl Shared {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }
}

/// A running detector process plus its worker thread.
pub struct Detector {
    shared: Arc<Shared>,
    child: Mutex<Child>,
    /// Taken by `stop`; behind a lock so the detector can live in an `Arc`.
    worker: Mutex<Option<JoinHandle<()>>>,
    stderr_tail: Arc<Mutex<VecDeque<String>>>,
}

impl Detector {
    /// Start the sidecar. Returns as soon as the process is spawned; the model
    /// load happens on the worker, and `snapshot()` reports it.
    pub fn start(config: DetectorConfig) -> Result<Detector> {
        if config.models.is_empty() {
            bail!("no model to load");
        }

        let mut command = Command::new(&config.python);
        command
            .arg(&config.script)
            .arg("--conf")
            .arg(config.conf.to_string())
            .arg("--imgsz")
            .arg(config.imgsz.to_string())
            .arg("--max-det")
            .arg(config.max_det.to_string());
        for model in &config.models {
            command.arg("--model").arg(model);
        }
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        debug!(
            "inu-r132: detector: {} {}",
            config.python.display(),
            config.script.display()
        );

        let mut child = command
            .spawn()
            .with_context(|| format!("could not start {}", config.python.display()))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("no stdin on the detector"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("no stdout on the detector"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow!("no stderr on the detector"))?;

        let shared = Arc::new(Shared {
            inner: Mutex::new(Inner::new()),
            wake: Condvar::new(),
        });
        let stderr_tail = Arc::new(Mutex::new(VecDeque::with_capacity(STDERR_TAIL)));

        spawn_stderr_reader(stderr, stderr_tail.clone());
        let worker = spawn_worker(shared.clone(), stdin, stdout, stderr_tail.clone());

        Ok(Detector {
            shared,
            child: Mutex::new(child),
            worker: Mutex::new(Some(worker)),
            stderr_tail,
        })
    }

    /// Hand the newest frame to the detector. Never blocks; an unprocessed frame
    /// is replaced, which is what keeps the display real time.
    pub fn submit(&self, jpeg: Vec<u8>) {
        let mut inner = self.shared.lock();
        if inner.stop {
            return;
        }
        inner.submitted += 1;
        if inner.pending.is_some() {
            inner.dropped += 1;
        }
        inner.pending = Some(jpeg);
        drop(inner);
        self.shared.wake.notify_one();
    }

    /// The boxes of the last frame the detector answered.
    pub fn snapshot(&self) -> Snapshot {
        self.shared.lock().snapshot.clone()
    }

    /// True once the sidecar has reported that its models loaded.
    pub fn is_ready(&self) -> bool {
        self.shared.lock().ready
    }

    /// Models the sidecar reported, empty until it is ready.
    pub fn models(&self) -> Vec<ModelInfo> {
        self.shared.lock().models.clone()
    }

    /// `submitted/completed/dropped` frame counters.
    pub fn counters(&self) -> (u64, u64, u64) {
        let inner = self.shared.lock();
        (inner.submitted, inner.completed, inner.dropped)
    }

    pub fn stderr_tail(&self) -> String {
        self.stderr_tail
            .lock()
            .map(|lines| lines.iter().cloned().collect::<Vec<_>>().join("\n"))
            .unwrap_or_default()
    }

    pub fn stop(&self) {
        {
            let mut inner = self.shared.lock();
            inner.stop = true;
            inner.pending = None;
        }
        self.shared.wake.notify_all();

        let worker = self.worker.lock().ok().and_then(|mut slot| slot.take());
        if let Some(worker) = worker {
            let _ = worker.join();
        }
        if let Ok(mut child) = self.child.lock() {
            // Closing the pipes usually makes it exit on its own; kill is just a
            // guarantee that nothing is left behind.
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for Detector {
    fn drop(&mut self) {
        self.stop();
    }
}

fn spawn_stderr_reader(
    stderr: std::process::ChildStderr,
    tail: Arc<Mutex<VecDeque<String>>>,
) -> JoinHandle<()> {
    std::thread::spawn(move || {
        use std::io::BufRead;
        let reader = BufReader::new(stderr);
        for line in reader.lines().map_while(Result::ok) {
            info!("inu-r132: detector: {line}");
            if let Ok(mut lines) = tail.lock() {
                if lines.len() == STDERR_TAIL {
                    lines.pop_front();
                }
                lines.push_back(line);
            }
        }
    })
}

fn spawn_worker(
    shared: Arc<Shared>,
    stdin: ChildStdin,
    stdout: std::process::ChildStdout,
    stderr_tail: Arc<Mutex<VecDeque<String>>>,
) -> JoinHandle<()> {
    std::thread::spawn(move || {
        let mut stdin = stdin;
        let mut stdout = BufReader::new(stdout);

        match read_message(&mut stdout) {
            Ok(Some(bytes)) => match parse_response(&bytes) {
                Ok(Parsed::Ready { models }) => {
                    let mut inner = shared.lock();
                    inner.models = models;
                    inner.ready = true;
                }
                Ok(Parsed::Failed { error }) => {
                    fail(&shared, error);
                    return;
                }
                Ok(Parsed::Frame(_)) | Err(_) => {
                    fail(&shared, "the detector did not send a ready message".into());
                    return;
                }
            },
            Ok(None) => {
                fail(&shared, "the detector exited before it was ready".into());
                return;
            }
            Err(error) => {
                fail(
                    &shared,
                    format!("could not read from the detector: {error}"),
                );
                return;
            }
        }

        loop {
            let frame = {
                let mut inner = shared.lock();
                while inner.pending.is_none() && !inner.stop {
                    inner = shared
                        .wake
                        .wait(inner)
                        .unwrap_or_else(|poison| poison.into_inner());
                }
                if inner.stop {
                    return;
                }
                inner.pending.take()
            };
            let Some(jpeg) = frame else { return };

            if let Err(error) = write_message(&mut stdin, &jpeg) {
                fail_with(
                    &shared,
                    stderr_tail.clone(),
                    format!("detector write failed: {error}"),
                );
                return;
            }

            let response = match read_message(&mut stdout) {
                Ok(Some(bytes)) => bytes,
                Ok(None) => {
                    fail_with(
                        &shared,
                        stderr_tail.clone(),
                        "the detector closed its output".into(),
                    );
                    return;
                }
                Err(error) => {
                    fail_with(
                        &shared,
                        stderr_tail.clone(),
                        format!("detector read failed: {error}"),
                    );
                    return;
                }
            };

            match parse_response(&response) {
                Ok(Parsed::Frame(snapshot)) => {
                    let mut inner = shared.lock();
                    inner.snapshot = snapshot;
                    inner.completed += 1;
                }
                Ok(Parsed::Failed { error }) => {
                    // A per-frame failure is not fatal: report it and keep going.
                    warn!("inu-r132: detector: {error}");
                    let mut inner = shared.lock();
                    inner.snapshot = Snapshot {
                        error: Some(error),
                        ..Snapshot::default()
                    };
                }
                _ => {}
            }
        }
    })
}

fn fail(shared: &Arc<Shared>, message: String) {
    warn!("inu-r132: detector: {message}");
    let mut inner = shared.lock();
    inner.snapshot = Snapshot {
        error: Some(message),
        ..Snapshot::default()
    };
    inner.stop = true;
}

fn fail_with(shared: &Arc<Shared>, tail: Arc<Mutex<VecDeque<String>>>, message: String) {
    let detail = tail
        .lock()
        .map(|lines| lines.iter().cloned().collect::<Vec<_>>().join("\n"))
        .unwrap_or_default();
    let full = if detail.is_empty() {
        message
    } else {
        format!("{message}\n{detail}")
    };
    fail(shared, full);
}

/// One message of the sidecar protocol: `u32 be length | payload`.
fn read_message(reader: &mut impl Read) -> std::io::Result<Option<Vec<u8>>> {
    let mut header = [0u8; 4];
    match reader.read_exact(&mut header) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(error) => return Err(error),
    }
    let length = u32::from_be_bytes(header) as usize;
    if length == 0 || length > 64 * 1024 * 1024 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("implausible message length {length}"),
        ));
    }
    let mut payload = vec![0u8; length];
    reader.read_exact(&mut payload)?;
    Ok(Some(payload))
}

fn write_message(writer: &mut impl Write, payload: &[u8]) -> std::io::Result<()> {
    writer.write_all(&(payload.len() as u32).to_be_bytes())?;
    writer.write_all(payload)?;
    writer.flush()
}

/// A parsed message from the sidecar.
#[derive(Debug)]
enum Parsed {
    Ready { models: Vec<ModelInfo> },
    Frame(Snapshot),
    Failed { error: String },
}

fn parse_response(bytes: &[u8]) -> Result<Parsed> {
    let value: serde_json::Value =
        serde_json::from_slice(bytes).context("the detector sent invalid JSON")?;

    if let Some(error) = value.get("error").and_then(|v| v.as_str()) {
        return Ok(Parsed::Failed {
            error: error.to_owned(),
        });
    }

    match value.get("ready").and_then(|v| v.as_bool()) {
        Some(true) => {
            let models = value
                .get("models")
                .and_then(|v| v.as_array())
                .map(|entries| entries.iter().map(parse_model).collect())
                .unwrap_or_default();
            return Ok(Parsed::Ready { models });
        }
        Some(false) => {
            let error = value
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("the detector could not load its models")
                .to_owned();
            return Ok(Parsed::Failed { error });
        }
        None => {}
    }

    let detections = value
        .get("detections")
        .and_then(|v| v.as_array())
        .map(|entries| entries.iter().filter_map(parse_detection).collect())
        .unwrap_or_default();

    Ok(Parsed::Frame(Snapshot {
        detections,
        width: value.get("width").and_then(|v| v.as_u64()).unwrap_or(0) as usize,
        height: value.get("height").and_then(|v| v.as_u64()).unwrap_or(0) as usize,
        millis: value.get("ms").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32,
        error: None,
    }))
}

fn parse_model(entry: &serde_json::Value) -> ModelInfo {
    let classes = entry
        .get("classes")
        .and_then(|v| v.as_object())
        .map(|map| {
            let mut classes: Vec<(i32, String)> = map
                .iter()
                .filter_map(|(id, name)| Some((id.parse().ok()?, name.as_str()?.to_owned())))
                .collect();
            classes.sort_by_key(|(id, _)| *id);
            classes
        })
        .unwrap_or_default();
    ModelInfo {
        name: entry
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("model")
            .to_owned(),
        path: entry
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_owned(),
        classes,
    }
}

fn parse_detection(entry: &serde_json::Value) -> Option<Detection> {
    let number = |key: &str| entry.get(key).and_then(|v| v.as_f64()).map(|v| v as f32);
    let (x1, y1, x2, y2) = (number("x1")?, number("y1")?, number("x2")?, number("y2")?);
    Some(Detection {
        model: entry
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_owned(),
        label: entry
            .get("class")
            .and_then(|v| v.as_str())
            .unwrap_or("?")
            .to_owned(),
        class_id: entry.get("class_id").and_then(|v| v.as_i64()).unwrap_or(-1) as i32,
        conf: number("conf").unwrap_or(0.0),
        x1,
        y1,
        x2,
        y2,
    })
}

// ------------------------------------------------------------- discovery ----

/// `*.pt` files in `dir`, sorted. Missing directory means no models.
pub fn scan_models(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .map(|ext| ext.eq_ignore_ascii_case("pt"))
                .unwrap_or(false)
        })
        .collect();
    found.sort();
    found
}

/// The Python that runs the sidecar: `--python`, `$INU_R132_PYTHON`, a `.venv`
/// next to the program or in the working directory, then `python3`/`python`.
///
/// A Python without ultralytics starts the sidecar perfectly happily and then
/// dies on `import ultralytics`, which is a confusing way to fail, so each
/// candidate is asked first and the first one that can actually import it wins.
pub fn find_python(explicit: Option<&Path>) -> Option<PathBuf> {
    if let Some(path) = explicit {
        return Some(path.to_path_buf());
    }
    if let Some(path) = std::env::var_os("INU_R132_PYTHON") {
        let path = PathBuf::from(path);
        if path.exists() {
            return Some(path);
        }
        warn!(
            "inu-r132: $INU_R132_PYTHON points at {} which does not exist, looking for \
             another Python",
            path.display()
        );
    }

    // The same places the sidecar is looked for, so a `.venv` next to the
    // script's repository is found no matter which directory the program ran
    // from.
    let mut candidates = Vec::new();
    for root in search_directories() {
        candidates.push(root.join(".venv/bin/python"));
        candidates.push(root.join(".venv/Scripts/python.exe"));
    }
    for name in ["python3", "python"] {
        candidates.extend(find_on_path(name));
    }

    candidates.retain(|path| path.is_file());
    for candidate in &candidates {
        if can_import_ultralytics(candidate) {
            debug!("inu-r132: using {} for the detector", candidate.display());
            return Some(candidate.clone());
        }
    }

    if !candidates.is_empty() {
        warn!(
            "inu-r132: none of these can `import ultralytics`: {}",
            candidates
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    None
}

/// Ask a Python whether ultralytics is importable, with a ceiling so a wedged
/// interpreter cannot hang the start-up. Output is discarded: the exit status is
/// all that matters and draining pipes by hand is how this kind of check
/// deadlocks.
fn can_import_ultralytics(python: &Path) -> bool {
    let Ok(mut child) = Command::new(python)
        .args(["-c", "import ultralytics"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };

    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(50));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
}

/// Every `name` on `PATH`, so a second interpreter can still be tried.
fn find_on_path(name: &str) -> Vec<PathBuf> {
    let Some(paths) = std::env::var_os("PATH") else {
        return Vec::new();
    };
    std::env::split_paths(&paths)
        .map(|dir| dir.join(name))
        .filter(|candidate| candidate.is_file())
        .collect()
}

/// The sidecar script: `--detector`, `$INU_R132_DETECTOR`, next to the program,
/// under a `yolo-cube-detect/` directory above it, or in the working directory.
pub fn find_script(explicit: Option<&Path>) -> Option<PathBuf> {
    if let Some(path) = explicit {
        return Some(path.to_path_buf());
    }
    if let Some(path) = std::env::var_os("INU_R132_DETECTOR") {
        return Some(PathBuf::from(path));
    }

    const NAME: &str = "inu_yolo_detector.py";
    for dir in search_directories() {
        for candidate in [dir.join(NAME), dir.join("yolo-cube-detect").join(NAME)] {
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// Where to look for the sidecar and for a usable Python: next to the program
/// first, then the directories above it (the program lives in `build/`, so the
/// repository root is a couple of levels up), then the working directory. Both
/// lookups use the same list so they cannot drift apart.
fn search_directories() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    let mut push = |dir: PathBuf| {
        if !dirs.contains(&dir) {
            dirs.push(dir);
        }
    };

    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            push(dir.to_path_buf());
            let mut current = dir.parent();
            for _ in 0..6 {
                match current {
                    Some(parent) => {
                        push(parent.to_path_buf());
                        current = parent.parent();
                    }
                    None => break,
                }
            }
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        push(cwd);
    }
    dirs
}

/// Wait for the sidecar to report ready, for the startup messages.
pub fn wait_ready(detector: &Detector, timeout: Duration) -> Result<Vec<ModelInfo>> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if detector.is_ready() {
            return Ok(detector.models());
        }
        if let Some(error) = detector.snapshot().error {
            bail!("{error}");
        }
        if std::time::Instant::now() >= deadline {
            bail!("the detector did not become ready within {timeout:?}");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_ready_message() {
        let json = br#"{"ready": true, "models": [
            {"path": "models/cubes-best.pt", "name": "cubes-best",
             "classes": {"0": "blue_cube", "1": "yellow_cube"}}]}"#;
        match parse_response(json).expect("parses") {
            Parsed::Ready { models } => {
                assert_eq!(models.len(), 1);
                assert_eq!(models[0].name, "cubes-best");
                assert_eq!(
                    models[0].classes,
                    vec![(0, "blue_cube".to_owned()), (1, "yellow_cube".to_owned())]
                );
            }
            other => panic!("expected ready, got {other:?}"),
        }
    }

    #[test]
    fn parses_detections() {
        let json = br#"{"detections": [
            {"model": "cubes-best", "class": "blue_cube", "class_id": 0, "conf": 0.9898,
             "x1": 538.7, "y1": 635.5, "x2": 844.2, "y2": 978.6}],
            "width": 1600, "height": 1200, "ms": 26.9}"#;
        match parse_response(json).expect("parses") {
            Parsed::Frame(snapshot) => {
                assert_eq!((snapshot.width, snapshot.height), (1600, 1200));
                assert!((snapshot.millis - 26.9).abs() < 0.01);
                assert_eq!(snapshot.detections.len(), 1);
                let hit = &snapshot.detections[0];
                assert_eq!(hit.label, "blue_cube");
                assert_eq!(hit.class_id, 0);
                assert!((hit.conf - 0.9898).abs() < 1e-4);
                assert!((hit.x1 - 538.7).abs() < 0.01);
            }
            other => panic!("expected a frame, got {other:?}"),
        }
    }

    #[test]
    fn parses_failures_and_empty_frames() {
        match parse_response(br#"{"ready": false, "error": "ModuleNotFoundError: ultralytics"}"#)
            .expect("parses")
        {
            Parsed::Failed { error } => assert!(error.contains("ultralytics")),
            other => panic!("expected a failure, got {other:?}"),
        }
        match parse_response(br#"{"error": "cannot decode the JPEG"}"#).expect("parses") {
            Parsed::Failed { error } => assert!(error.contains("decode")),
            other => panic!("expected a failure, got {other:?}"),
        }
        match parse_response(br#"{"detections": [], "width": 4, "height": 2, "ms": 1}"#)
            .expect("parses")
        {
            Parsed::Frame(snapshot) => assert!(snapshot.detections.is_empty()),
            other => panic!("expected a frame, got {other:?}"),
        }
        assert!(parse_response(b"not json").is_err());
    }

    #[test]
    fn message_framing_round_trips() {
        let mut buffer = Vec::new();
        write_message(&mut buffer, b"hello").unwrap();
        assert_eq!(&buffer[..4], &5u32.to_be_bytes());
        let mut reader = std::io::Cursor::new(buffer);
        assert_eq!(
            read_message(&mut reader).unwrap().as_deref(),
            Some(&b"hello"[..])
        );
        assert_eq!(read_message(&mut reader).unwrap(), None, "then EOF");
    }

    #[test]
    fn absurd_lengths_are_rejected() {
        let mut reader = std::io::Cursor::new(0xffff_ffffu32.to_be_bytes().to_vec());
        assert!(read_message(&mut reader).is_err());
        let mut zero = std::io::Cursor::new(0u32.to_be_bytes().to_vec());
        assert!(read_message(&mut zero).is_err());
    }

    #[test]
    fn scan_models_filters_and_sorts() {
        let dir = std::env::temp_dir().join(format!("inu-r132-models-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["b.pt", "a.pt", "notes.txt", "c.PT"] {
            std::fs::write(dir.join(name), b"x").unwrap();
        }
        let found: Vec<String> = scan_models(&dir)
            .iter()
            .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(found, vec!["a.pt", "b.pt", "c.PT"]);
        assert!(scan_models(&dir.join("nope")).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Drives the real sidecar: needs a Python with ultralytics and a `*.pt` in
    /// `models/`, so it is ignored by default. Run it with
    ///
    ///   EPOCHS=2 make yolo-train          # once, to have a model
    ///     cargo test -- --ignored --nocapture detector_round_trip
    #[test]
    #[ignore = "needs a Python with ultralytics and a model in models/"]
    fn detector_round_trip() {
        let script = find_script(None).expect("inu_yolo_detector.py should be discoverable");
        let python = find_python(None).expect("a Python interpreter");
        let models = scan_models(Path::new("models"));
        assert!(!models.is_empty(), "no .pt in models/");

        let detector = Detector::start(DetectorConfig {
            python,
            script,
            models,
            conf: 0.4,
            imgsz: 640,
            max_det: 20,
        })
        .expect("the detector should start");

        let loaded = wait_ready(&detector, Duration::from_secs(180)).expect("should become ready");
        assert!(!loaded.is_empty());
        println!("loaded {:?} -> {:?}", loaded[0].name, loaded[0].classes);

        // A flat grey frame: the point is the pipe, not what it finds.
        let rgb = vec![128u8; 640 * 480 * 3];
        let jpeg = crate::frame::encode_jpeg(&rgb, 640, 480, 80).expect("encode");
        detector.submit(jpeg);

        let deadline = std::time::Instant::now() + Duration::from_secs(120);
        while std::time::Instant::now() < deadline {
            let snapshot = detector.snapshot();
            if let Some(error) = &snapshot.error {
                panic!("the detector reported: {error}");
            }
            if snapshot.width == 640 && snapshot.height == 480 {
                println!("answered in {:.0} ms", snapshot.millis);
                assert!(
                    snapshot.detections.is_empty(),
                    "a grey frame has no cube in it"
                );
                detector.stop();
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        detector.stop();
        panic!("the detector never answered a frame");
    }

    /// Runs a real photo through the detector and draws the overlay into
    /// `target/detector-preview.jpg`, so the boxes can be checked without
    /// opening a window. Ignored by default, like `detector_round_trip`.
    #[test]
    #[ignore = "needs a Python with ultralytics, a model and a test photo"]
    fn detector_draws_boxes_on_a_photo() {
        let script = find_script(None).expect("inu_yolo_detector.py");
        let python = find_python(None).expect("a Python interpreter");
        let models = scan_models(Path::new("models"));
        assert!(!models.is_empty(), "no .pt in models/");

        let photo = std::env::var("INU_R132_TEST_FRAME")
            .map(PathBuf::from)
            .ok()
            .or_else(|| {
                let dir = Path::new("train/dataset/images/val");
                let mut found: Vec<PathBuf> = std::fs::read_dir(dir)
                    .ok()?
                    .filter_map(|entry| entry.ok())
                    .map(|entry| entry.path())
                    .filter(|path| path.extension().is_some_and(|ext| ext == "jpg"))
                    .collect();
                found.sort();
                found.into_iter().next()
            })
            .expect("no test photo; set INU_R132_TEST_FRAME");

        let jpeg = std::fs::read(&photo).expect("read the photo");
        let (width, height, rgb) = crate::frame::decode_jpeg(&jpeg).expect("decode the photo");
        let mut buffer = crate::frame::rgb_to_u32(&rgb);

        let detector = Detector::start(DetectorConfig {
            python,
            script,
            models,
            conf: 0.4,
            imgsz: 640,
            max_det: 20,
        })
        .expect("the detector should start");
        wait_ready(&detector, Duration::from_secs(180)).expect("should become ready");
        detector.submit(jpeg);

        let snapshot = loop {
            let snapshot = detector.snapshot();
            assert!(snapshot.error.is_none(), "{:?}", snapshot.error);
            if !snapshot.detections.is_empty() {
                break snapshot;
            }
            std::thread::sleep(Duration::from_millis(100));
        };
        detector.stop();

        println!("{}: {:?}", photo.display(), snapshot.detections);

        // A synthetic depth frame with one valid pixel at the box centre, so the
        // distance in the label and its marker are exercised too.
        let hit = &snapshot.detections[0];
        let centre_x = ((hit.x1 + hit.x2) / 2.0).round() as usize;
        let centre_y = ((hit.y1 + hit.y2) / 2.0).round() as usize;
        let mut depth = vec![0u8; width * height * 2];
        let centre_index = (centre_y * width + centre_x) * 2;
        depth[centre_index..centre_index + 2].copy_from_slice(&412u16.to_le_bytes());

        crate::overlay_detections(
            &mut buffer,
            width,
            height,
            &snapshot,
            Some((&depth, width, height)),
        );

        // The outline has to land where the box is: corners painted, interior
        // left alone. (Counting the colour over the whole frame would also catch
        // the label above the box, and any pixel of the photo that happens to
        // match, so sample the corners instead.)
        let colour = crate::overlay::palette_colour(hit.class_id);
        // Whole pixels, the way the drawing code rounds them.
        let left = hit.x1.floor() as usize;
        let top = hit.y1.floor() as usize;
        let right = hit.x2.ceil() as usize;
        let bottom = hit.y2.ceil() as usize;
        let get = |x: usize, y: usize| buffer[y * width + x];
        assert_eq!(get(left + 1, top + 1), colour, "top left corner");
        assert_eq!(get(right - 2, bottom - 2), colour, "bottom right corner");
        assert_eq!(get((left + right) / 2, top + 1), colour, "top edge");
        assert_eq!(get(left + 1, (top + bottom) / 2), colour, "left edge");
        // Well inside the box, but away from the centre marker.
        assert_ne!(
            get(left + 20, (top + bottom) / 2),
            colour,
            "the interior is left alone"
        );

        // The distance came from the centre, and 412 mm is what the synthetic
        // depth frame holds there.
        assert_eq!(
            get(centre_x, centre_y),
            colour,
            "the marker sits at the centre"
        );
        assert_eq!(
            crate::frame::sample_box_centre(
                &depth, width, height, hit.x1, hit.y1, hit.x2, hit.y2, width, height
            ),
            (Some(412), 1),
            "the box centre reads the synthetic depth"
        );
        // The centre marker is there because the synthetic depth had a reading.
        assert_eq!(
            get(centre_x, centre_y),
            colour,
            "the distance mark sits at the box centre"
        );
        assert_eq!(
            crate::frame::sample_box_centre(
                &depth, width, height, hit.x1, hit.y1, hit.x2, hit.y2, width, height
            ),
            (Some(412), 1),
            "the box centre reads the synthetic depth"
        );

        let painted = buffer.iter().filter(|pixel| **pixel == colour).count();
        assert!(painted > 1000, "only {painted} pixels of the box colour");

        let out = Path::new("target/detector-preview.jpg");
        let jpeg = crate::frame::encode_jpeg(
            &buffer
                .iter()
                .flat_map(|pixel| {
                    [
                        ((pixel >> 16) & 0xff) as u8,
                        ((pixel >> 8) & 0xff) as u8,
                        (pixel & 0xff) as u8,
                    ]
                })
                .collect::<Vec<u8>>(),
            width,
            height,
            90,
        )
        .expect("encode the preview");
        std::fs::write(out, jpeg).expect("write the preview");
        println!("wrote {} ({}x{})", out.display(), width, height);
    }
}
