//! inu-r132 - serve and display the RGB camera of an Inuitive NU4000.
//!
//! `serve` starts Inuitive's InuService daemon (when needed), opens the R132
//! color sensor and publishes JPEG frames over a small TCP protocol.
//! `display` shows the camera in a window, either from the local sensor or from
//! a remote `serve`.
//! `shot` saves stills as JPEG under `shot/` next to the program.
//!
//! Usage is documented in src/bin/inu_r132_help.txt and printed by --help.

#[path = "../cli.rs"]
mod cli;
#[path = "../detect.rs"]
mod detect;
#[path = "../frame.rs"]
mod frame;
#[path = "../inu.rs"]
mod inu;
#[path = "../overlay.rs"]
mod overlay;
#[path = "../protocol.rs"]
mod protocol;
#[path = "../sys.rs"]
mod sys;
#[path = "../wire.rs"]
mod wire;

use std::io::Write;
use std::os::raw::{c_int, c_void};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime};

use anyhow::{anyhow, bail, Context, Result};
use clap::Parser;
use log::{debug, error, info, warn};
use minifb::{Key, KeyRepeat, ScaleMode, Window, WindowOptions};

use cli::{Cli, Command};
use frame::{FrameHub, RawFrame};
use inu::{InuCamera, OutputFormat, StreamKind};
use protocol::{CameraMeta, ServeConfig};

const WINDOW_TITLE: &str = "NU4000 - R132 (CGS_132 RGB)";
const DEFAULT_REMOTE_PORT: u16 = wire::DEFAULT_PORT;

/// The SDK invokes its callback on its own thread; a global keeps the Rust
/// callback free of user-data lifetime juggling. This is a one-camera app.
static FRAME_HUB: OnceLock<Arc<FrameHub>> = OnceLock::new();

/// C callback invoked by the shim for every frame, tagged with its stream.
extern "C" fn on_frame(
    stream: c_int,
    data: *const u8,
    width: c_int,
    height: c_int,
    stride: c_int,
    format: c_int,
    timestamp: u64,
    _user: *mut c_void,
) {
    let Some(hub) = FRAME_HUB.get() else {
        return;
    };
    let Some(stream) = StreamKind::from_bit(stream as u32) else {
        return;
    };
    if let Some(frame) =
        RawFrame::from_callback(stream, data, width, height, stride, format, timestamp, 0)
    {
        hub.publish(frame);
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format_target(false)
        .init();

    let args = Cli::parse();

    if args.help {
        print!("{}", include_str!("inu_r132_help.txt"));
        return ExitCode::SUCCESS;
    }

    let Some(command) = &args.command else {
        print!("{}", include_str!("inu_r132_help.txt"));
        return ExitCode::FAILURE;
    };

    let result = match command {
        Command::Serve {
            port,
            fps,
            quality,
            attach,
            mock,
            no_registered,
            service,
            format,
            stream,
            channel,
        } => {
            serve(ServeArgs {
                config: ServeConfig {
                    port: *port,
                    fps: *fps,
                    quality: *quality,
                    streams: stream.mask(),
                    default_mode: stream.default_mode(),
                },
                attach: *attach,
                mock: *mock,
                registered: !*no_registered,
                service: service.clone(),
                format: (*format).into(),
                channel: channel.unwrap_or(-1),
            })
            .await
        }
        Command::Display {
            remote,
            service,
            fps,
            format,
            stream,
            near,
            far,
            gamma,
            auto,
            no_registered,
            model,
            models_dir,
            conf,
            imgsz,
            no_detect,
            python,
            detector,
            channel,
        } => display(DisplayArgs {
            remote: remote.clone(),
            service: service.clone(),
            fps: *fps,
            format: (*format).into(),
            stream: (*stream).into(),
            near: *near,
            far: *far,
            gamma: *gamma,
            auto: *auto,
            registered: !*no_registered,
            detect: DetectArgs {
                models: model.clone(),
                models_dir: models_dir.clone(),
                conf: *conf,
                imgsz: *imgsz,
                enabled: !*no_detect,
                python: python.clone(),
                script: detector.clone(),
            },
            channel: channel.unwrap_or(-1),
        }),
        Command::Shot {
            dir,
            count,
            interval,
            warmup,
            quality,
            attach,
            mock,
            service,
            format,
            channel,
        } => {
            shot(ShotArgs {
                dir: dir.clone(),
                count: *count,
                warmup_ms: *warmup,
                interval_ms: *interval,
                quality: *quality,
                attach: *attach,
                mock: *mock,
                service: service.clone(),
                format: (*format).into(),
                channel: channel.unwrap_or(-1),
            })
            .await
        }
        Command::Scan {
            model,
            models_dir,
            conf,
            imgsz,
            count,
            warmup,
            interval,
            timeout,
            pretty,
            remote,
            attach,
            service,
            python,
            detector,
            format,
            no_registered,
            channel,
        } => {
            scan(ScanArgs {
                detect: DetectArgs {
                    models: model.clone(),
                    models_dir: models_dir.clone(),
                    conf: *conf,
                    imgsz: *imgsz,
                    enabled: true,
                    python: python.clone(),
                    script: detector.clone(),
                },
                count: *count,
                warmup_ms: *warmup,
                interval_ms: *interval,
                timeout_ms: *timeout,
                pretty: *pretty,
                remote: remote.clone(),
                attach: *attach,
                service: service.clone(),
                format: (*format).into(),
                registered: !*no_registered,
                channel: channel.unwrap_or(-1),
            })
            .await
        }
        Command::ServiceStop => service_stop(),
        Command::Probe { attach, service } => probe(ProbeArgs {
            attach: *attach,
            service: service.clone(),
        }),
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            error!("{e}");
            ExitCode::FAILURE
        }
    }
}

struct ServeArgs {
    config: ServeConfig,
    attach: bool,
    mock: bool,
    registered: bool,
    service: Option<String>,
    format: OutputFormat,
    channel: i32,
}

struct DisplayArgs {
    remote: Option<String>,
    service: Option<String>,
    fps: u32,
    format: OutputFormat,
    /// Stream shown first; number keys switch in the window.
    stream: StreamKind,
    /// Depth display window in mm.
    near: u16,
    far: u16,
    /// Depth greyscale curve; 1.0 is linear.
    gamma: f32,
    /// Start with the window auto-ranged to each frame's 2%..98%.
    auto: bool,
    /// Register depth to the RGB camera (default on).
    registered: bool,
    /// Where the detector's models come from.
    detect: DetectArgs,
    channel: i32,
}

/// Everything needed to start the YOLO sidecar.
struct DetectArgs {
    models: Vec<PathBuf>,
    models_dir: PathBuf,
    conf: f32,
    imgsz: u32,
    enabled: bool,
    python: Option<PathBuf>,
    script: Option<PathBuf>,
}

struct ProbeArgs {
    attach: bool,
    service: Option<String>,
}

struct ScanArgs {
    detect: DetectArgs,
    count: u32,
    warmup_ms: u64,
    interval_ms: u64,
    timeout_ms: u64,
    pretty: bool,
    remote: Option<String>,
    attach: bool,
    service: Option<String>,
    format: OutputFormat,
    registered: bool,
    channel: i32,
}

struct ShotArgs {
    /// Destination directory; `None` means `shot/` next to the program.
    dir: Option<PathBuf>,
    count: u32,
    interval_ms: u64,
    warmup_ms: u64,
    quality: u8,
    attach: bool,
    mock: bool,
    service: Option<String>,
    format: OutputFormat,
    channel: i32,
}

/// Report every channel and sensor the SDK discovered. This is what decides
/// whether depth / IR / point cloud are available: look for a `depth` or
/// `stereo (IR)` channel. Opening the sensor is enough, no stream is started.
fn probe(args: ProbeArgs) -> Result<()> {
    let paths = sys::SdkPaths::resolve();
    let service = if args.attach {
        info!("inu-r132: --attach given, using the running InuService");
        ServiceHandle::attached()
    } else {
        start_inu_service(&paths)?
    };

    let result = probe_inner(args.service.as_deref());

    service.stop_if_started();
    result
}

fn probe_inner(service: Option<&str>) -> Result<()> {
    let camera = open_camera(service, 0, -1, StreamKind::Rgb.bit(), false, OutputFormat::Bgra)?;
    let channels = camera.channel_infos();
    let sensors = camera.sensors();

    let has_rgb = channels.iter().any(|c| c.channel_type == 1);
    let has_stereo = channels.iter().any(|c| c.channel_type == 3);
    let has_depth = channels.iter().any(|c| c.channel_type == 4);
    let has_disparity = channels.iter().any(|c| c.channel_type == 6);

    if channels.is_empty() {
        warn!("inu-r132: the SDK reported no channels");
    }
    for channel in &channels {
        info!(
            "inu-r132: channel {} type {} ({})",
            channel.id,
            channel.channel_type,
            channel.type_name()
        );
    }
    for sensor in &sensors {
        info!(
            "inu-r132: sensor {} model {} ({}) role {}",
            sensor.id,
            sensor.model,
            sensor.model_name(),
            sensor.role_name()
        );
    }

    let json = serde_json::json!({
        "channels": channels
            .iter()
            .map(|c| serde_json::json!({
                "id": c.id,
                "type": c.channel_type,
                "type_name": c.type_name(),
            }))
            .collect::<Vec<_>>(),
        "sensors": sensors
            .iter()
            .map(|s| serde_json::json!({
                "id": s.id,
                "model": s.model,
                "model_name": s.model_name(),
                "role": s.role,
                "role_name": s.role_name(),
            }))
            .collect::<Vec<_>>(),
        "available": {
            "rgb": has_rgb,
            "stereo_ir": has_stereo,
            "depth": has_depth,
            "disparity": has_disparity,
        },
    });
    println!("{json}");

    if !has_depth {
        info!(
            "inu-r132: no depth channel reported; depth/point cloud may need a \
             different device, firmware or license"
        );
    }
    Ok(())
}

/// Take stills and write them as JPEG under `shot/` next to the program, each
/// named after its capture time. This is the "collect a dataset" command: no
/// window, no server, one file per photo printed on stdout.
async fn shot(args: ShotArgs) -> Result<()> {
    let dir = shot_dir(args.dir.clone())?;
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("could not create the photo directory {}", dir.display()))?;
    info!(
        "inu-r132: saving {} photo(s) to {}",
        args.count,
        dir.display()
    );

    // No hardware needed to check where the files go.
    if args.mock {
        let hub = install_hub()?;
        tokio::spawn(frame::run_mock(hub.clone(), 640, 480, 30, StreamKind::Rgb));
        return capture_burst(&hub, &dir, &args).await;
    }

    let paths = sys::SdkPaths::resolve();
    let service = if args.attach {
        info!("inu-r132: --attach given, using the running InuService");
        ServiceHandle::attached()
    } else {
        start_inu_service(&paths)?
    };

    let hub = match install_hub() {
        Ok(hub) => hub,
        Err(e) => {
            service.stop_if_started();
            return Err(e);
        }
    };

    // A still is colour only, so nothing has to be registered to a depth frame.
    let camera = match open_camera(
        args.service.as_deref(),
        0,
        args.channel,
        StreamKind::Rgb.bit(),
        false,
        args.format,
    ) {
        Ok(camera) => camera,
        Err(e) => {
            service.stop_if_started();
            return Err(e);
        }
    };
    if let Err(e) = camera
        .set_frame_callback(on_frame)
        .and_then(|()| camera.start())
    {
        camera.stop();
        service.stop_if_started();
        return Err(e);
    }

    log_channels(&camera);
    let result = capture_burst(&hub, &dir, &args).await;

    camera.stop();
    service.stop_if_started();
    result
}

/// The frame hub plus the SDK callback, which both only exist once per process.
fn install_hub() -> Result<Arc<FrameHub>> {
    let hub = Arc::new(FrameHub::new());
    FRAME_HUB
        .set(hub.clone())
        .map_err(|_| anyhow!("frame hub already initialized"))?;
    Ok(hub)
}

/// Where photos go: `--dir`, else `shot/` next to the executable.
fn shot_dir(override_dir: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(dir) = override_dir {
        return Ok(dir);
    }
    let exe = std::env::current_exe().context("could not find the program path; pass --dir")?;
    let parent = exe
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    Ok(parent.join("shot"))
}

/// Wait for a fresh frame per photo and write it out. The first frame after a
/// cold start still has the sensor's boot exposure, hence `--warmup`.
async fn capture_burst(hub: &FrameHub, dir: &Path, args: &ShotArgs) -> Result<()> {
    if args.warmup_ms > 0 {
        tokio::time::sleep(Duration::from_millis(args.warmup_ms)).await;
    }

    let mut last_seq = 0u64;
    for index in 1..=args.count {
        if index > 1 && args.interval_ms > 0 {
            tokio::time::sleep(Duration::from_millis(args.interval_ms)).await;
        }

        // Wait for a frame newer than the one the previous photo used, so a
        // burst is not several copies of the same picture.
        let frame = loop {
            hub.changed(StreamKind::Rgb, last_seq).await;
            if let Some(frame) = hub.latest(StreamKind::Rgb) {
                break frame;
            }
        };
        last_seq = frame.seq;

        let path = write_shot(dir, &frame, args.quality)?;
        info!(
            "inu-r132: {}/{} {} ({}x{})",
            index,
            args.count,
            path.display(),
            frame.width,
            frame.height
        );
        // The bare path on stdout is what scripts consume.
        println!("{}", path.display());
    }
    Ok(())
}

/// Encode one colour frame as JPEG and write it under `dir`.
fn write_shot(dir: &Path, frame: &RawFrame, quality: u8) -> Result<PathBuf> {
    if frame.format.is_16bit() {
        bail!(
            "cannot save the {} frame `{}` as a photo, it is not colour",
            frame.stream.as_str(),
            frame.format.label()
        );
    }
    let rgb = frame.to_rgb();
    let expected = frame.width * frame.height * 3;
    if rgb.len() != expected {
        bail!(
            "the {} frame is {} bytes, expected {expected} for {}x{}",
            frame.format.label(),
            rgb.len(),
            frame.width,
            frame.height
        );
    }

    let jpeg = frame::encode_jpeg(&rgb, frame.width, frame.height, quality)?;
    let path = unique_shot_path(dir, SystemTime::now())?;
    std::fs::write(&path, &jpeg).with_context(|| format!("could not write {}", path.display()))?;
    Ok(path)
}

/// `shot/2026-02-14_09-31-07.123.jpg` in local time, which sorts the way it
/// reads. A suffix is added if two photos land in the same millisecond.
fn unique_shot_path(dir: &Path, taken: SystemTime) -> Result<PathBuf> {
    let stamp = chrono::DateTime::<chrono::Local>::from(taken)
        .format("%Y-%m-%d_%H-%M-%S%.3f")
        .to_string();

    let candidate = dir.join(format!("{stamp}.jpg"));
    if !candidate.exists() {
        return Ok(candidate);
    }
    for counter in 2..1000 {
        let candidate = dir.join(format!("{stamp}_{counter}.jpg"));
        if !candidate.exists() {
            return Ok(candidate);
        }
    }
    bail!(
        "a thousand photos already exist for {stamp} in {}",
        dir.display()
    )
}

/// One message from a `serve`, for `scan --remote`.
enum Wire {
    Frame {
        codec: u8,
        width: usize,
        height: usize,
        data: Vec<u8>,
    },
    Text(String),
    Bye,
}

/// Find the cubes and print where they are and how far away, as JSON on stdout.
/// Everything else (the SDK, the detector) logs to stderr, so the output can be
/// piped straight into `jq`.
async fn scan(args: ScanArgs) -> Result<()> {
    let Some(detector) = start_detector(&args.detect) else {
        bail!(
            "no detector: put a .pt in {} or pass --model (see `inu-r132 -h`)",
            args.detect.models_dir.display()
        );
    };

    let result = match args.remote.clone() {
        Some(remote) => scan_remote(&remote, &args, &detector),
        None => scan_local(&args, &detector).await,
    };

    detector.stop();
    result
}

async fn scan_local(args: &ScanArgs, detector: &Arc<detect::Detector>) -> Result<()> {
    let hub = install_hub()?;
    let paths = sys::SdkPaths::resolve();
    let service = if args.attach {
        info!("inu-r132: --attach given, using the running InuService");
        ServiceHandle::attached()
    } else {
        start_inu_service(&paths)?
    };

    // Colour for the boxes, depth to measure them.
    let streams = StreamKind::mask(&[StreamKind::Rgb, StreamKind::Depth]);
    let camera = match open_camera(
        args.service.as_deref(),
        0,
        args.channel,
        streams,
        args.registered,
        args.format,
    ) {
        Ok(camera) => camera,
        Err(error) => {
            service.stop_if_started();
            return Err(error);
        }
    };
    if let Err(error) = camera
        .set_frame_callback(on_frame)
        .and_then(|()| camera.start())
    {
        camera.stop();
        service.stop_if_started();
        return Err(error);
    }

    let has_depth = camera.active_streams() & StreamKind::Depth.bit() != 0;
    if !has_depth {
        warn!("inu-r132: no depth stream, distances will be null");
    }

    // A one-shot scan only gets this one frame, so settle first: the first
    // frames after a cold start still carry the sensor's boot exposure, and the
    // depth stream trails the colour one by a frame or two.
    if args.warmup_ms > 0 {
        tokio::time::sleep(Duration::from_millis(args.warmup_ms)).await;
    }
    if has_depth && hub.latest(StreamKind::Depth).is_none() {
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while hub.latest(StreamKind::Depth).is_none() {
            if std::time::Instant::now() >= deadline {
                warn!("inu-r132: no depth frame arrived, distances will be null");
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    let mut last_seq = 0u64;
    let mut result = Ok(());
    for index in 1..=args.count {
        if index > 1 && args.interval_ms > 0 {
            tokio::time::sleep(Duration::from_millis(args.interval_ms)).await;
        }

        // Wait for a colour frame this scan has not already used.
        let colour = loop {
            hub.changed(StreamKind::Rgb, last_seq).await;
            if let Some(frame) = hub.latest(StreamKind::Rgb) {
                break frame;
            }
        };
        last_seq = colour.seq;

        let jpeg = match frame::encode_jpeg(&colour.to_rgb(), colour.width, colour.height, 90) {
            Ok(jpeg) => jpeg,
            Err(error) => {
                result = Err(error);
                break;
            }
        };

        let snapshot = match detect_once(detector, jpeg, args.timeout_ms) {
            Ok(snapshot) => snapshot,
            Err(error) => {
                result = Err(error);
                break;
            }
        };

        let depth = hub.latest(StreamKind::Depth);
        let depth_view = depth
            .as_ref()
            .map(|frame| (frame.data.as_slice(), frame.width, frame.height));
        let json = scan_json(index, colour.width, colour.height, depth_view, &snapshot);
        if let Err(error) = print_json(&json, args.pretty) {
            result = Err(error);
            break;
        }
    }

    camera.stop();
    service.stop_if_started();
    result
}

fn scan_remote(remote: &str, args: &ScanArgs, detector: &Arc<detect::Detector>) -> Result<()> {
    use std::io::Write;

    let (host, port) = split_host_port(remote)?;
    let address = format!("{host}:{port}");
    info!("inu-r132: scanning {address}");

    let mut stream = std::net::TcpStream::connect(&address)
        .with_context(|| format!("could not connect to {address}"))?;
    stream.set_nodelay(true).ok();
    stream.write_all(b"subscribe mix\n")?;

    // Reading happens on its own thread so a server that stops talking cannot
    // wedge the scan: the main loop waits on a channel with a timeout instead.
    let control = stream.try_clone().context("could not clone the socket")?;
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut reader = std::io::BufReader::new(stream);
        loop {
            match wire::read_message_blocking(&mut reader) {
                Ok((kind, payload)) => {
                    let message = match kind {
                        wire::TYPE_FRAME => match wire::parse_frame(&payload) {
                            Some((header, data)) => Wire::Frame {
                                codec: header.codec,
                                width: header.width as usize,
                                height: header.height as usize,
                                data: data.to_vec(),
                            },
                            None => continue,
                        },
                        wire::TYPE_TEXT => {
                            Wire::Text(String::from_utf8_lossy(&payload).into_owned())
                        }
                        wire::TYPE_BYE => Wire::Bye,
                        _ => continue,
                    };
                    let done = matches!(message, Wire::Bye);
                    if sender.send(Ok(message)).is_err() || done {
                        return;
                    }
                }
                Err(error) => {
                    let _ = sender.send(Err(error));
                    return;
                }
            }
        }
    });

    // `mix` needs both streams on the server; fall back to colour only.
    let mut wants_depth = true;
    match receiver.recv_timeout(Duration::from_secs(10)) {
        Ok(Ok(Wire::Text(text))) if text.starts_with("err") => {
            warn!("inu-r132: mix is not available ({text}); scanning colour only");
            wants_depth = false;
            let mut control = control.try_clone().context("could not clone the socket")?;
            control.write_all(b"subscribe rgb\n")?;
        }
        Ok(Ok(Wire::Text(text))) => info!("inu-r132: server: {text}"),
        Ok(Ok(_)) => debug!("inu-r132: a frame arrived before the subscribe reply"),
        Ok(Err(error)) => return Err(error).context("could not read from {address}"),
        Err(error) => bail!("no reply to `subscribe` from {address}: {error}"),
    }

    let mut latest_colour: Option<(usize, usize, Vec<u8>)> = None;
    let mut latest_depth: Option<(usize, usize, Vec<u8>)> = None;

    let mut result = Ok(());
    for index in 1..=args.count {
        if index > 1 && args.interval_ms > 0 {
            std::thread::sleep(Duration::from_millis(args.interval_ms));
        }

        // Collect for a moment: `mix` alternates, so the colour frame arrives
        // without the depth frame that measures it.
        let deadline = std::time::Instant::now() + Duration::from_millis(1000);
        while std::time::Instant::now() < deadline {
            if latest_colour.is_some() && (latest_depth.is_some() || !wants_depth) {
                break;
            }
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            match receiver.recv_timeout(left) {
                Ok(Ok(Wire::Frame {
                    codec: wire::CODEC_JPEG,
                    width,
                    height,
                    data,
                })) => {
                    latest_colour = Some((width, height, data));
                }
                Ok(Ok(Wire::Frame {
                    codec: wire::CODEC_Z16,
                    width,
                    height,
                    data,
                })) => {
                    latest_depth = Some((width, height, data));
                }
                Ok(Ok(Wire::Frame { .. })) => {}
                Ok(Ok(Wire::Text(text))) => info!("inu-r132: server: {text}"),
                Ok(Ok(Wire::Bye)) => {
                    result = Err(anyhow!("the server closed the connection"));
                    break;
                }
                Ok(Err(error)) => {
                    result = Err(error).with_context(|| format!("reading from {address}"));
                    break;
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => break,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    result = Err(anyhow!("the connection to {address} ended"));
                    break;
                }
            }
        }
        if result.is_err() {
            break;
        }

        let Some((width, height, jpeg)) = latest_colour.take() else {
            result = Err(anyhow!("no colour frame from {address} within 1 s"));
            break;
        };

        let snapshot = match detect_once(detector, jpeg, args.timeout_ms) {
            Ok(snapshot) => snapshot,
            Err(error) => {
                result = Err(error);
                break;
            }
        };

        let depth_view = latest_depth
            .as_ref()
            .map(|(depth_width, depth_height, raw)| (raw.as_slice(), *depth_width, *depth_height));
        let json = scan_json(index, width, height, depth_view, &snapshot);
        if let Err(error) = print_json(&json, args.pretty) {
            result = Err(error);
            break;
        }
    }

    // Unblock the reader so the process can exit cleanly.
    let _ = control.shutdown(std::net::Shutdown::Both);
    result
}

/// Hand one frame to the detector and wait for its answer.
fn detect_once(
    detector: &detect::Detector,
    jpeg: Vec<u8>,
    timeout_ms: u64,
) -> Result<detect::Snapshot> {
    let before = detector.counters().1;
    detector.submit(jpeg);

    let deadline = std::time::Instant::now() + Duration::from_millis(timeout_ms);
    loop {
        let snapshot = detector.snapshot();
        if let Some(error) = snapshot.error {
            bail!("the detector failed: {error}");
        }
        if detector.counters().1 > before {
            return Ok(snapshot);
        }
        if std::time::Instant::now() >= deadline {
            bail!("the detector did not answer within {timeout_ms} ms");
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// The JSON one scan prints.
fn scan_json(
    seq: u32,
    width: usize,
    height: usize,
    depth: Option<(&[u8], usize, usize)>,
    snapshot: &detect::Snapshot,
) -> serde_json::Value {
    let objects: Vec<serde_json::Value> = snapshot
        .detections
        .iter()
        .map(|hit| {
            // The distance at the centre of the box; registered depth shares the
            // colour camera's frame, so no reprojection is needed.
            let sample = depth.map(|(raw, depth_width, depth_height)| {
                frame::sample_box_centre(
                    raw,
                    depth_width,
                    depth_height,
                    hit.x1,
                    hit.y1,
                    hit.x2,
                    hit.y2,
                    snapshot.width,
                    snapshot.height,
                )
            });
            let (distance, samples) = match sample {
                Some((Some(mm), valid)) => (serde_json::json!(mm), valid),
                Some((None, valid)) => (serde_json::Value::Null, valid),
                None => (serde_json::Value::Null, 0),
            };

            // The detector speaks f32; rounding here keeps the JSON free of
            // digits that are only float noise (543.7999877929688 -> 543.8).
            let round1 = |value: f32| ((value as f64) * 10.0).round() / 10.0;
            serde_json::json!({
                "model": hit.model,
                "label": hit.label,
                "class_id": hit.class_id,
                "confidence": ((hit.conf as f64) * 10000.0).round() / 10000.0,
                "box": {
                    "x1": round1(hit.x1),
                    "y1": round1(hit.y1),
                    "x2": round1(hit.x2),
                    "y2": round1(hit.y2),
                },
                "centre": {
                    "x": round1((hit.x1 + hit.x2) / 2.0),
                    "y": round1((hit.y1 + hit.y2) / 2.0),
                },
                "distance_mm": distance,
                "depth_samples": samples,
            })
        })
        .collect();

    serde_json::json!({
        "seq": seq,
        "timestamp": chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, false),
        "image": {"width": width, "height": height},
        "depth": {
            "available": depth.is_some(),
            "width": depth.map(|(_, depth_width, _)| depth_width),
            "height": depth.map(|(_, _, depth_height)| depth_height),
        },
        "detect_ms": ((snapshot.millis as f64) * 10.0).round() / 10.0,
        "objects": objects,
    })
}

fn print_json(value: &serde_json::Value, pretty: bool) -> Result<()> {
    let text = if pretty {
        serde_json::to_string_pretty(value)?
    } else {
        serde_json::to_string(value)?
    };
    println!("{text}");
    // A scan that is piped somewhere is often read line by line.
    std::io::stdout()
        .flush()
        .context("could not write to stdout")
}

/// Start InuService (unless --attach), open the camera and serve the protocol.
async fn serve(args: ServeArgs) -> Result<()> {
    if args.mock {
        return serve_mock(args).await;
    }

    let paths = sys::SdkPaths::resolve();

    let service = if args.attach {
        info!("inu-r132: --attach given, using the running InuService");
        ServiceHandle::attached()
    } else {
        start_inu_service(&paths)?
    };

    let hub = Arc::new(FrameHub::new());
    FRAME_HUB
        .set(hub.clone())
        .map_err(|_| anyhow!("frame hub already initialized"))?;

    // The very first connection boots the on-device firmware and can time out;
    // a fresh process would work too, which is what the demo did by running
    // `show` twice. This applies whether we started the service or reused one.
    let streams = args.config.streams;
    let mut camera = match open_camera(
        args.service.as_deref(),
        0,
        args.channel,
        streams,
        args.registered,
        args.format,
    ) {
        Ok(camera) => camera,
        Err(e) => {
            service.stop_if_started();
            return Err(e);
        }
    };

    camera.set_frame_callback(on_frame)?;
    if let Err(e) = camera.start() {
        service.stop_if_started();
        return Err(e);
    }

    let mut active = camera.active_streams();
    let mut available = [
        active & StreamKind::Rgb.bit() != 0,
        active & StreamKind::Depth.bit() != 0,
    ];

    // Registered depth is the nice path, but if the device or firmware refuses
    // it the depth stream simply does not start. Fall back once to raw depth so
    // that mix still has something to overlay.
    if args.registered
        && streams & StreamKind::Depth.bit() != 0
        && !available[StreamKind::Depth.index()]
    {
        warn!(
            "inu-r132: registered depth did not start, retrying without --registered \
             (mix will not be aligned)"
        );
        camera.stop();
        camera = match open_camera(
            args.service.as_deref(),
            0,
            args.channel,
            streams,
            false,
            args.format,
        ) {
            Ok(camera) => camera,
            Err(e) => {
                service.stop_if_started();
                return Err(e);
            }
        };
        camera.set_frame_callback(on_frame)?;
        if let Err(e) = camera.start() {
            service.stop_if_started();
            return Err(e);
        }
        active = camera.active_streams();
        available = [
            active & StreamKind::Rgb.bit() != 0,
            active & StreamKind::Depth.bit() != 0,
        ];
    }

    if !available.iter().any(|running| *running) {
        camera.stop();
        service.stop_if_started();
        bail!("no requested stream could be started (active streams: 0x{active:x})");
    }

    let missing: Vec<&str> = StreamKind::ALL
        .iter()
        .filter(|kind| streams & kind.bit() != 0 && !available[kind.index()])
        .map(|kind| kind.as_str())
        .collect();
    if !missing.is_empty() {
        warn!(
            "inu-r132: requested but NOT started: {} (see the [inu-r132] lines above); \
             clients will get `err` for it",
            missing.join(", ")
        );
    }

    log_channels(&camera);
    let channel = camera.channel_id();
    if channel == u32::MAX {
        info!("inu-r132: streaming from the default channel");
    } else {
        info!("inu-r132: streaming from channel {channel}");
    }
    info!(
        "inu-r132: serving {}",
        StreamKind::ALL
            .iter()
            .filter(|kind| available[kind.index()])
            .map(|kind| kind.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    );

    let meta = CameraMeta {
        channel,
        format: args.format,
        source_fps: 0,
    };

    let result = protocol::run(hub, args.config, meta, available).await;

    camera.stop();
    service.stop_if_started();
    result
}

/// Log the channel ids the SDK reports, which helps pick `--channel`.
fn log_channels(camera: &InuCamera) {
    let channels = camera.channels();
    if channels.is_empty() {
        info!("inu-r132: the SDK reported no channels");
    } else {
        info!("inu-r132: channels reported by the SDK: {channels:?}");
    }
}

/// Serve a synthetic moving pattern, no SDK or hardware required.
async fn serve_mock(args: ServeArgs) -> Result<()> {
    const WIDTH: usize = 640;
    const HEIGHT: usize = 480;

    let hub = Arc::new(FrameHub::new());
    FRAME_HUB
        .set(hub.clone())
        .map_err(|_| anyhow!("frame hub already initialized"))?;

    let fps = args.config.fps.max(1);
    let mut available = [false, false];
    for kind in StreamKind::ALL {
        if args.config.streams & kind.bit() != 0 {
            tokio::spawn(frame::run_mock(hub.clone(), WIDTH, HEIGHT, fps, kind));
            available[kind.index()] = true;
        }
    }
    info!(
        "inu-r132: mock mode, serving synthetic {WIDTH}x{HEIGHT} {}",
        StreamKind::ALL
            .iter()
            .filter(|kind| available[kind.index()])
            .map(|kind| kind.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    );

    let meta = CameraMeta {
        channel: 0,
        format: OutputFormat::Bgra,
        source_fps: fps,
    };
    protocol::run(hub, args.config, meta, available).await
}

/// Open the camera, retrying through the firmware-boot timeout.
///
/// The SDK boots the device firmware on the first `Init`, which regularly
/// times out that first call, so a few attempts are normal. When InuService is
/// not running at all every call times out the same way, so check for it first
/// and bail out as soon as it disappears.
fn open_camera(
    service: Option<&str>,
    fps: u32,
    channel: i32,
    streams: u32,
    registered: bool,
    format: OutputFormat,
) -> Result<InuCamera> {
    const ATTEMPTS: u32 = 6;

    if !inu_service_running() {
        return Err(no_service_error());
    }

    let mut last_error = None;
    for attempt in 1..=ATTEMPTS {
        match InuCamera::open(service, fps, channel, streams, registered, format) {
            Ok(camera) => return Ok(camera),
            Err(e) => {
                last_error = Some(e);
                if !inu_service_running() {
                    return Err(anyhow!(
                        "{}\nInuService disappeared while connecting",
                        last_error.as_ref().unwrap()
                    ));
                }
                if attempt < ATTEMPTS {
                    info!(
                        "inu-r132: camera not ready, attempt {attempt}/{ATTEMPTS} ({})",
                        last_error.as_ref().unwrap()
                    );
                    std::thread::sleep(Duration::from_secs(1));
                }
            }
        }
    }

    let error = last_error.unwrap_or_else(|| anyhow!("could not open the camera"));
    Err(anyhow!(
        "{error}\n\
         InuService is running but the call to it keeps timing out. It usually is \
         one of:\n  \
         - the first connection is booting the device firmware: try again once\n  \
         - a stale or wedged daemon/device:\n      \
         sudo pkill -x InuService && replug the NU4000 USB cable\n  \
         - the daemon cannot reach the hardware (not root, wrong INUITIVE_PATH):\n      \
         make service-status && make service"
    ))
}

/// The error for "nothing to talk to", with the usual ways to start it.
fn no_service_error() -> anyhow::Error {
    anyhow!(
        "no InuService process is running. Start it with:\n  \
         make service                 # sudo, from the extracted SDK\n  \
         # or let serve start it for you (no --attach), or use the vendor\n  \
         # systemd unit if the .deb was installed"
    )
}


/// Is an InuService daemon running? `pgrep`/`tasklist` need no privileges.
fn inu_service_running() -> bool {
    #[cfg(unix)]
    {
        std::process::Command::new("pgrep")
            .args(["-x", "InuService"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }
    #[cfg(windows)]
    {
        std::process::Command::new("tasklist")
            .args(["/FI", "IMAGENAME eq InuService.exe", "/NH"])
            .output()
            .map(|out| {
                String::from_utf8_lossy(&out.stdout)
                    .to_ascii_lowercase()
                    .contains("inuservice.exe")
            })
            .unwrap_or(false)
    }
}

/// The user(s) owning running InuService processes, when it can be determined.
/// A non-root daemon cannot touch the USB device, which looks exactly like a
/// timeout from the client side.
fn inu_service_owner() -> Option<String> {
    #[cfg(unix)]
    {
        let out = std::process::Command::new("ps")
            .args(["-C", "InuService", "-o", "user="])
            .output()
            .ok()?;
        let mut owners: Vec<String> = String::from_utf8_lossy(&out.stdout)
            .split_whitespace()
            .map(str::to_string)
            .collect();
        owners.sort();
        owners.dedup();
        if owners.is_empty() {
            None
        } else {
            Some(owners.join(","))
        }
    }
    #[cfg(windows)]
    {
        None
    }
}

/// A running InuService. Inuitive's service daemonizes itself ("Running as
/// background process"), so the `sudo` parent we spawn exits right away and the
/// real daemon lives on. We therefore only remember whether we started it.
struct ServiceHandle {
    started_by_us: bool,
}

impl ServiceHandle {
    fn attached() -> Self {
        Self {
            started_by_us: false,
        }
    }

    fn started() -> Self {
        Self { started_by_us: true }
    }

    /// Stop the daemon we started, best effort and without prompting: it runs
    /// as root, so `sudo -n` only succeeds while the credentials are cached.
    fn stop_if_started(&self) {
        if !self.started_by_us {
            return;
        }
        #[cfg(unix)]
        {
            let status = std::process::Command::new("sudo")
                .args(["-n", "pkill", "-x", "InuService"])
                .status();
            match status {
                Ok(status) if status.success() => {
                    info!("inu-r132: stopped the InuService we started")
                }
                _ => info!(
                    "inu-r132: InuService is still running (it daemonized); \
                     stop it with `make service-stop`"
                ),
            }
        }
        #[cfg(windows)]
        {
            let status = std::process::Command::new("taskkill")
                .args(["/IM", "InuService.exe", "/F"])
                .status();
            if !matches!(status, Ok(status) if status.success()) {
                info!("inu-r132: InuService may still be running");
            }
        }
    }
}

/// Start Inuitive's InuService daemon. It drives the hardware over USB and
/// needs root/administrator, so it is started through the platform tooling.
fn start_inu_service(paths: &sys::SdkPaths) -> Result<ServiceHandle> {
    let service = paths.service.as_ref().ok_or_else(|| {
        anyhow!(
            "InuService was not found. Extract the `inudev` package under deps/ \
             (see README) and rebuild, or run with --attach if it is already running"
        )
    })?;

    // InuService daemonizes, so a second instance only fights the first one for
    // the device. Reuse whatever is already up.
    if inu_service_running() {
        match inu_service_owner().as_deref() {
            Some(owner) if owner != "root" => warn!(
                "an InuService is already running as `{owner}`, not root: it cannot \
                 reach the NU4000, so every call will time out. Stop it with \
                 `sudo pkill -x InuService` and start it via `make service`"
            ),
            Some(owner) => info!("inu-r132: reusing the InuService running as {owner}"),
            None => info!("inu-r132: an InuService is already running, reusing it"),
        }
        return Ok(ServiceHandle::attached());
    }

    let bindir = service.parent().unwrap_or_else(|| std::path::Path::new("."));
    let root = paths
        .inudev_root
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_default();

    info!("inu-r132: starting InuService (elevation may ask for your password)...");

    #[cfg(unix)]
    {
        let mut command = std::process::Command::new("sudo");
        command
            .arg("env")
            .arg(format!("INUITIVE_PATH={root}"))
            .arg(format!("LD_LIBRARY_PATH={}", bindir.display()))
            .arg(service);
        let mut child = command
            .spawn()
            .with_context(|| format!("could not start `sudo {}`", service.display()))?;

        // It daemonizes and the foreground parent exits. Wait for the daemon to
        // show up, or report that it died (sudo failure, busy device, ...).
        for _ in 0..30 {
            if inu_service_running() {
                return Ok(ServiceHandle::started());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        if let Ok(Some(status)) = child.try_wait() {
            return Err(anyhow!(
                "InuService exited during startup ({status}); its output is above. \
                 Common causes: the sudo prompt was not answered, the NU4000 is not \
                 connected, or another process holds the device"
            ));
        }
        let _ = child.try_wait();
        info!("inu-r132: InuService did not appear in `pgrep`, continuing anyway");
        Ok(ServiceHandle::started())
    }

    #[cfg(windows)]
    {
        let script = format!(
            "Start-Process -Verb RunAs -FilePath '{}' -WorkingDirectory '{}'",
            service.display(),
            bindir.display()
        );
        // The elevated child inherits this process' environment.
        std::process::Command::new("powershell")
            .env("INUITIVE_PATH", &root)
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .spawn()
            .with_context(|| format!("could not elevate {}", service.display()))?;
        for _ in 0..30 {
            if inu_service_running() {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        Ok(ServiceHandle::started())
    }
}

/// Show the camera: a remote stream when --remote is given, else the local one.
fn display(args: DisplayArgs) -> Result<()> {
    if args.remote.is_some() {
        return display_remote(args);
    }
    display_local(args)
}

/// Start the detector sidecar when there is a model to run, saying why not
/// otherwise. A detector that cannot start is never fatal: the picture still
/// shows, just without boxes.
fn start_detector(args: &DetectArgs) -> Option<Arc<detect::Detector>> {
    if !args.enabled {
        info!("inu-r132: detection disabled with --no-detect");
        return None;
    }

    let models = if args.models.is_empty() {
        detect::scan_models(&args.models_dir)
    } else {
        args.models.clone()
    };
    if models.is_empty() {
        info!(
            "inu-r132: no .pt models ({} is empty and no --model was given), \
             running without detection",
            args.models_dir.display()
        );
        return None;
    }
    let models: Vec<PathBuf> = models.into_iter().filter(|path| path.is_file()).collect();
    if models.is_empty() {
        warn!("inu-r132: none of the given --model paths exist");
        return None;
    }

    let Some(script) = detect::find_script(args.script.as_deref()) else {
        warn!(
            "inu-r132: inu_yolo_detector.py was not found; pass --detector or set \
             INU_R132_DETECTOR"
        );
        return None;
    };
    let Some(python) = detect::find_python(args.python.as_deref()) else {
        warn!(
            "inu-r132: no Python found; pass --python, set INU_R132_PYTHON, or put a \
             .venv next to the program"
        );
        return None;
    };

    info!(
        "inu-r132: detector: {} via {}",
        models
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>()
            .join(", "),
        python.display()
    );

    let config = detect::DetectorConfig {
        python,
        script,
        models,
        conf: args.conf,
        imgsz: args.imgsz,
        max_det: 20,
    };
    let detector = match detect::Detector::start(config) {
        Ok(detector) => Arc::new(detector),
        Err(error) => {
            warn!("inu-r132: detector did not start: {error}");
            return None;
        }
    };

    // The models load inside the sidecar; wait so a failure is reported before
    // the window opens instead of as an empty overlay.
    match detect::wait_ready(&detector, Duration::from_secs(30)) {
        Ok(loaded) => {
            for model in &loaded {
                info!(
                    "inu-r132: detector model {} ({}): {}",
                    model.name,
                    model.path,
                    model
                        .classes
                        .iter()
                        .map(|(id, name)| format!("{id}={name}"))
                        .collect::<Vec<_>>()
                        .join(" ")
                );
            }
            Some(detector)
        }
        Err(error) => {
            warn!("inu-r132: detector failed: {error}");
            for line in detector.stderr_tail().lines().take(6) {
                warn!("inu-r132: detector: {line}");
            }
            detector.stop();
            None
        }
    }
}

/// Draw the detector's boxes onto a frame, rescaling from the size they were
/// computed on to the size being displayed.
/// `depth` is the raw Z16 frame (bytes, width, height) the boxes can be measured
/// against, when one is available.
fn overlay_detections(
    buffer: &mut [u32],
    width: usize,
    height: usize,
    snapshot: &detect::Snapshot,
    depth: Option<(&[u8], usize, usize)>,
) {
    if snapshot.detections.is_empty() || snapshot.width == 0 || snapshot.height == 0 {
        return;
    }
    let scale_x = width as f32 / snapshot.width as f32;
    let scale_y = height as f32 / snapshot.height as f32;

    let mut canvas = overlay::Canvas {
        pixels: buffer,
        width,
        height,
    };
    for hit in &snapshot.detections {
        let colour = overlay::palette_colour(hit.class_id);
        let x = hit.x1 * scale_x;
        let y = hit.y1 * scale_y;
        canvas.rect(
            x,
            y,
            (hit.x2 - hit.x1) * scale_x,
            (hit.y2 - hit.y1) * scale_y,
            colour,
            2,
        );

        // The distance comes from the centre of the box. Registered depth is
        // aligned to the colour camera, so normalising the box in the detector's
        // frame lands on the same point of the depth image.
        let centre_x = x + (hit.x2 - hit.x1) * scale_x / 2.0;
        let centre_y = y + (hit.y2 - hit.y1) * scale_y / 2.0;
        let sample = depth.map(|(raw, depth_width, depth_height)| {
            frame::sample_box_centre(
                raw,
                depth_width,
                depth_height,
                hit.x1,
                hit.y1,
                hit.x2,
                hit.y2,
                snapshot.width,
                snapshot.height,
            )
        });

        let mut text = format!("{} {}%", hit.label, (hit.conf * 100.0).round() as i32);
        // The 5x7 font is upper-case ASCII only, so no 无测量 here.
        match sample {
            Some((Some(mm), 9)) => text.push_str(&format!(" {mm}MM")),
            Some((Some(mm), valid)) => text.push_str(&format!(" {mm}MM {valid}/9")),
            Some((None, _)) => text.push_str(" NO DEPTH"),
            None => {}
        }
        if matches!(sample, Some((Some(_), _))) {
            canvas.dot(centre_x, centre_y, 3.5, colour);
        }

        let (text_width, text_height) = canvas.text_size(&text, 1);
        let label_y = if y >= text_height as f32 + 3.0 {
            y - text_height as f32 - 3.0
        } else {
            y + 3.0
        };
        let label_x = x.clamp(0.0, (width.saturating_sub(text_width)) as f32);
        canvas.label(label_x, label_y, &text, colour, 1);
    }
}

/// One line for the window title: what was found and how far away it is, or why
/// nothing was.
fn detection_summary(
    detector: &detect::Detector,
    snapshot: &detect::Snapshot,
    depth: Option<(&[u8], usize, usize)>,
) -> String {
    if let Some(error) = &snapshot.error {
        let first = error.lines().next().unwrap_or(error.as_str());
        return format!("detect off: {first}");
    }
    if !detector.is_ready() {
        return "detect: loading models...".to_owned();
    }

    let (submitted, completed, _) = detector.counters();
    let throughput = format!("{completed}/{submitted} frames");
    if snapshot.detections.is_empty() {
        return format!("detect: nothing ({:.0} ms, {throughput})", snapshot.millis);
    }
    let found = snapshot
        .detections
        .iter()
        .map(|hit| {
            let mut text = format!("{} {:.2}", hit.label, hit.conf);
            let Some((raw, depth_width, depth_height)) = depth else {
                return text;
            };
            let (value, valid) = frame::sample_box_centre(
                raw,
                depth_width,
                depth_height,
                hit.x1,
                hit.y1,
                hit.x2,
                hit.y2,
                snapshot.width,
                snapshot.height,
            );
            match value {
                Some(mm) if valid == 9 => text.push_str(&format!(" {mm}mm")),
                Some(mm) => text.push_str(&format!(" {mm}mm ({valid}/9)")),
                None => text.push_str(" 无测量"),
            }
            text
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!("detect: {found} ({:.0} ms, {throughput})", snapshot.millis)
}

fn display_local(args: DisplayArgs) -> Result<()> {
    let hub = Arc::new(FrameHub::new());
    FRAME_HUB
        .set(hub.clone())
        .map_err(|_| anyhow!("frame hub already initialized"))?;

    // A viewer opens colour plus depth in one session; a stream the device does
    // not have is simply skipped.
    let streams = StreamKind::mask(&[StreamKind::Rgb, StreamKind::Depth]);
    let camera = InuCamera::open(
        args.service.as_deref(),
        args.fps,
        args.channel,
        streams,
        args.registered,
        args.format,
    )?;
    camera.set_frame_callback(on_frame)?;
    camera.start()?;

    log_channels(&camera);
    let active = camera.active_streams();
    let available: Vec<StreamKind> = StreamKind::ALL
        .iter()
        .copied()
        .filter(|kind| active & kind.bit() != 0)
        .collect();
    if available.is_empty() {
        camera.stop();
        bail!("no stream could be started");
    }
    info!(
        "inu-r132: active streams: {}",
        available
            .iter()
            .map(|kind| kind.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    );
    info!("inu-r132: waiting for the first frame... (Esc quits)");

    let detector = start_detector(&args.detect);
    let for_window = detector.clone();

    let mut current = if available.contains(&args.stream) {
        args.stream
    } else {
        available[0]
    };
    let mut last_seq = [0u64; 2];
    let mut near = args.near;
    let mut far = args.far.max(args.near.saturating_add(1));
    let mut gamma = if args.gamma > 0.0 { args.gamma } else { 1.0 };
    let mut auto = args.auto;
    let mut first_frame = true;
    let mut stats = String::new();

    let result = run_window(move |window| {
        // Number keys pick a stream.
        for (index, kind) in available.iter().enumerate() {
            let key = match index {
                0 => Key::Key1,
                1 => Key::Key2,
                2 => Key::Key3,
                _ => continue,
            };
            if window.is_key_pressed(key, KeyRepeat::No) && current != *kind {
                current = *kind;
                info!("inu-r132: showing the {} stream", kind.as_str());
            }
        }

        // Depth window and curve. Any manual window change turns auto off.
        let mut manual = false;
        if window.is_key_pressed(Key::Z, KeyRepeat::Yes) {
            far = far.saturating_sub(100).max(near.saturating_add(1));
            manual = true;
        }
        if window.is_key_pressed(Key::X, KeyRepeat::Yes) {
            far = far.saturating_add(100);
            manual = true;
        }
        if window.is_key_pressed(Key::N, KeyRepeat::Yes) {
            near = near.saturating_sub(100);
            if near >= far {
                near = far.saturating_sub(1);
            }
            manual = true;
        }
        if window.is_key_pressed(Key::M, KeyRepeat::Yes) {
            near = near.saturating_add(100).min(far.saturating_sub(1));
            manual = true;
        }
        if manual {
            auto = false;
        }
        if window.is_key_pressed(Key::G, KeyRepeat::No) {
            gamma = match gamma {
                g if g >= 0.9 => 0.5,
                g if g >= 0.45 => 0.25,
                _ => 1.0,
            };
            info!("inu-r132: depth gamma {gamma}");
        }
        if window.is_key_pressed(Key::A, KeyRepeat::No) {
            auto = !auto;
            info!("inu-r132: auto range {}", if auto { "on" } else { "off" });
        }

        let frame = hub.latest(current)?;
        if frame.seq == last_seq[current.index()] {
            return None;
        }
        last_seq[current.index()] = frame.seq;
        if first_frame {
            info!(
                "inu-r132: first {} frame {}x{}",
                current.as_str(),
                frame.width,
                frame.height
            );
            first_frame = false;
        }

        let is_16bit = frame.format.is_16bit();
        if is_16bit {
            if let Some(summary) = frame::z16_summary(&frame.data) {
                stats = format!(
                    "{} / {} / {} mm, invalid {:.0}%",
                    summary.min,
                    summary.p50,
                    summary.max,
                    summary.invalid * 100.0
                );
                if auto {
                    // Ease towards each frame's percentiles so the view does not
                    // flicker when a few pixels change.
                    near = ((near as u32 * 3 + summary.p2 as u32) / 4).min(65535) as u16;
                    let target_far = summary.p98.max(summary.p2.saturating_add(1));
                    far = ((far as u32 * 3 + target_far as u32) / 4).min(65535) as u16;
                    if far <= near {
                        far = near.saturating_add(1);
                    }
                }
            }
        }

        // Repack the colour frame once: the buffer, the detector and the overlay
        // all need the same pixels.
        let rgb = if is_16bit { Vec::new() } else { frame.to_rgb() };
        let mut buffer = if is_16bit {
            frame.to_gray_u32(near, far, gamma)
        } else {
            frame::rgb_to_u32(&rgb)
        };

        // The detector only ever sees colour frames: a depth frame is a
        // different picture, so boxes found on the colour image would not line
        // up with it.
        let mut detection = String::new();
        if let Some(detector) = for_window.as_ref() {
            if is_16bit {
                detection = "detect: colour view only".to_owned();
            } else {
                match frame::encode_jpeg(&rgb, frame.width, frame.height, 85) {
                    Ok(jpeg) => detector.submit(jpeg),
                    Err(error) => {
                        warn!("inu-r132: could not encode a frame for the detector: {error}")
                    }
                }
                let snapshot = detector.snapshot();
                // Registered depth shares the colour camera's frame, so its
                // pixels are the ones the boxes are expressed in.
                let depth_frame = hub.latest(StreamKind::Depth);
                let depth = depth_frame
                    .as_ref()
                    .map(|frame| (frame.data.as_slice(), frame.width, frame.height));
                overlay_detections(&mut buffer, frame.width, frame.height, &snapshot, depth);
                detection = detection_summary(detector, &snapshot, depth);
            }
        }

        let keys = (1..=available.len())
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join("/");
        let title = format!(
            "NU4000 - {} - {}x{}  [{keys}] stream  z/x far  n/m near  g gamma  a auto  |  \
             {near}..{far} mm, gamma {gamma}, auto {}  |  {stats}  |  {detection}",
            current.as_str(),
            frame.width,
            frame.height,
            if auto { "on" } else { "off" }
        );
        Some((title, frame.width, frame.height, buffer))
    });

    if let Some(detector) = &detector {
        detector.stop();
    }
    camera.stop();
    result
}

/// A frame received from a remote `serve`, already in minifb layout.
struct RemoteFrame {
    width: usize,
    height: usize,
    buffer: Vec<u32>,
    seq: u64,
    /// JPEG frames are colour; raw Z16 is depth, which the detector never sees.
    colour: bool,
    /// Last raw Z16 frame, kept so a box on the colour image can be measured.
    depth: Option<(usize, usize, Vec<u8>)>,
}

fn display_remote(args: DisplayArgs) -> Result<()> {
    let remote = args.remote.clone().unwrap_or_default();
    let (host, port) = split_host_port(&remote)?;
    let address = format!("{host}:{port}");
    info!("inu-r132: connecting to {address}");

    let detector = start_detector(&args.detect);

    let slot = Arc::new(Mutex::new(RemoteFrame {
        width: 0,
        height: 0,
        buffer: Vec::new(),
        seq: 0,
        colour: false,
        depth: None,
    }));

    let thread_slot = slot.clone();
    let reader_address = address.clone();
    let reader_detector = detector.clone();
    let (near, far, gamma) = (args.near, args.far, args.gamma);
    std::thread::spawn(move || {
        if let Err(e) = remote_reader(
            &reader_address,
            thread_slot,
            near,
            far,
            gamma,
            reader_detector,
        ) {
            error!("inu-r132: remote stream ended: {e}");
        }
    });

    let mut last_seq = 0u64;
    let title = format!("NU4000 - remote {address}");
    let for_window = detector.clone();
    let result = run_window(move |_window| {
        let (width, height, mut buffer, colour, seq, depth) = {
            let slot = slot.lock().ok()?;
            if slot.seq == last_seq || slot.width == 0 {
                return None;
            }
            (
                slot.width,
                slot.height,
                slot.buffer.clone(),
                slot.colour,
                slot.seq,
                // Only copied when something can use it.
                if for_window.is_some() {
                    slot.depth.clone()
                } else {
                    None
                },
            )
        };
        last_seq = seq;

        let mut detection = String::new();
        if let Some(detector) = for_window.as_ref() {
            if colour {
                let snapshot = detector.snapshot();
                let depth = depth.as_ref().map(|(depth_width, depth_height, raw)| {
                    (raw.as_slice(), *depth_width, *depth_height)
                });
                overlay_detections(&mut buffer, width, height, &snapshot, depth);
                detection = detection_summary(detector, &snapshot, depth);
            } else {
                detection = "detect: colour view only".to_owned();
            }
        }

        let title = if detection.is_empty() {
            title.clone()
        } else {
            format!("{title}  |  {detection}")
        };
        Some((title, width, height, buffer))
    });

    if let Some(detector) = &detector {
        detector.stop();
    }
    result
}

/// Read the envelope stream from a remote server and render frames. JPEG is
/// decoded as colour, raw Z16 is mapped through the depth window. Colour frames
/// are also handed to the detector.
fn remote_reader(
    address: &str,
    slot: Arc<Mutex<RemoteFrame>>,
    near: u16,
    far: u16,
    gamma: f32,
    detector: Option<Arc<detect::Detector>>,
) -> Result<()> {
    use std::io::Write;

    let mut stream = std::net::TcpStream::connect(address)
        .with_context(|| format!("could not connect to {address}"))?;
    stream.write_all(b"subscribe\n")?;

    loop {
        let (kind, payload) = wire::read_message_blocking(&mut stream)?;
        match kind {
            wire::TYPE_FRAME => {
                let Some((header, data)) = wire::parse_frame(&payload) else {
                    warn!("inu-r132: malformed frame from {address}");
                    continue;
                };
                let (width, height, buffer, colour) = match header.codec {
                    wire::CODEC_JPEG => {
                        // The serve already encoded this frame; hand those very
                        // bytes to the detector instead of re-encoding.
                        if let Some(detector) = detector.as_ref() {
                            detector.submit(data.to_vec());
                        }
                        let (width, height, rgb) = frame::decode_jpeg(data)?;
                        (width, height, frame::rgb_to_u32(&rgb), true)
                    }
                    wire::CODEC_Z16 => {
                        if let Ok(mut slot) = slot.lock() {
                            slot.depth = Some((
                                header.width as usize,
                                header.height as usize,
                                data.to_vec(),
                            ));
                        }
                        (
                            header.width as usize,
                            header.height as usize,
                            frame::z16_to_gray_u32(data, near, far, gamma),
                            false,
                        )
                    }
                    other => {
                        warn!("inu-r132: unsupported codec {other} in stream");
                        continue;
                    }
                };
                debug!(
                    "inu-r132: frame {}x{} (header said {}x{})",
                    width, height, header.width, header.height
                );
                if let Ok(mut slot) = slot.lock() {
                    slot.width = width;
                    slot.height = height;
                    slot.buffer = buffer;
                    slot.colour = colour;
                    slot.seq = slot.seq.wrapping_add(1);
                }
            }
            wire::TYPE_TEXT => {
                info!("inu-r132: server: {}", String::from_utf8_lossy(&payload));
            }
            wire::TYPE_BYE => {
                info!("inu-r132: server closed the stream");
                break;
            }
            other => warn!("inu-r132: unknown message type {other}"),
        }
    }
    Ok(())
}

fn split_host_port(value: &str) -> Result<(String, u16)> {
    match value.rsplit_once(':') {
        Some((host, port)) if !host.is_empty() && !port.is_empty() => {
            let port = port
                .parse::<u16>()
                .with_context(|| format!("invalid port in `{value}`"))?;
            Ok((host.to_string(), port))
        }
        Some(_) => bail!("invalid --remote `{value}`, expected HOST:PORT"),
        None => Ok((value.to_string(), DEFAULT_REMOTE_PORT)),
    }
}

/// Drive the minifb window, pulling the newest frame through `fetch` whenever
/// one is ready. `fetch` gets the window so it can read keys, and returns
/// `(title, width, height, buffer)` or `None` when nothing new arrived.
fn run_window(
    mut fetch: impl FnMut(&mut Window) -> Option<(String, usize, usize, Vec<u32>)>,
) -> Result<()> {
    let mut window = Window::new(
        WINDOW_TITLE,
        960,
        540,
        WindowOptions {
            resize: true,
            scale_mode: ScaleMode::AspectRatioStretch,
            ..WindowOptions::default()
        },
    )
    .map_err(|e| anyhow!("could not create the window: {e}"))?;

    let mut buffer: Vec<u32> = vec![0];
    let mut size = (1usize, 1usize);
    let mut title = String::new();

    while window.is_open() && !window.is_key_down(Key::Escape) {
        if let Some((new_title, width, height, next)) = fetch(&mut window) {
            buffer = next;
            size = (width, height);
            if new_title != title {
                window.set_title(&new_title);
                title = new_title;
            }
        }
        if buffer.len() < size.0 * size.1 {
            size = (1, 1);
            buffer.clear();
            buffer.push(0);
        }
        window
            .update_with_buffer(&buffer, size.0, size.1)
            .map_err(|e| anyhow!("could not display a frame: {e}"))?;
        std::thread::sleep(Duration::from_millis(2));
    }
    Ok(())
}

/// Stop Inuitive's InuService daemon.
fn service_stop() -> Result<()> {
    #[cfg(unix)]
    {
        let status = std::process::Command::new("sudo")
            .args(["pkill", "-x", "InuService"])
            .status()
            .context("could not run `sudo pkill -x InuService`")?;
        if status.success() {
            info!("inu-r132: InuService stopped");
        } else {
            info!("inu-r132: no InuService process was running");
        }
        Ok(())
    }
    #[cfg(windows)]
    {
        let status = std::process::Command::new("taskkill")
            .args(["/IM", "InuService.exe", "/F"])
            .status()
            .context("could not run taskkill")?;
        if status.success() {
            info!("inu-r132: InuService stopped");
        } else {
            info!("inu-r132: no InuService process was running");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shot_names_are_timestamps_and_do_not_collide() {
        let dir = std::env::temp_dir().join(format!("inu-r132-shot-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");

        let taken = SystemTime::UNIX_EPOCH + Duration::from_millis(1_770_000_000_123);
        let first = unique_shot_path(&dir, taken).expect("first path");
        let name = first.file_name().unwrap().to_str().unwrap().to_string();

        // YYYY-MM-DD_HH-MM-SS.mmm.jpg; the exact digits depend on the time
        // zone, only the shape is asserted.
        assert_eq!(name.len(), 27, "{name}");
        assert!(name.ends_with(".jpg"), "{name}");
        assert_eq!(name.as_bytes()[4], b'-', "{name}");
        assert_eq!(name.as_bytes()[10], b'_', "{name}");
        assert_eq!(name.as_bytes()[19], b'.', "{name}");

        std::fs::write(&first, b"jpeg").unwrap();
        let second = unique_shot_path(&dir, taken).expect("second path");
        assert_eq!(
            second.file_name().unwrap().to_str().unwrap(),
            format!("{}_2.jpg", &name[..name.len() - 4])
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
