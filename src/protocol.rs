//! The TCP camera protocol served by `inu-r132 serve`.
//!
//! Every stream the sensor opened has one preparation task that turns the
//! newest frame into wire bytes at most once, and one broadcast channel that
//! every subscribed connection reads from. A connection picks the stream it
//! wants with `subscribe`/`stream`, so switching rgb/depth never restarts the
//! server or touches the camera. A slow client only drops frames for itself.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use log::{debug, info, warn};
use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::broadcast;

use crate::frame::{self, EncodedFrame, FrameHub};
use crate::inu::{OutputFormat, StreamKind};
use crate::wire;

/// What a connection subscribed to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamMode {
    Rgb,
    Depth,
    /// Both streams on one connection; the client overlays them.
    Mix,
}

impl StreamMode {
    pub fn label(self) -> &'static str {
        match self {
            StreamMode::Rgb => "rgb",
            StreamMode::Depth => "depth",
            StreamMode::Mix => "mix",
        }
    }

    /// Does this mode want frames of `kind`?
    pub fn wants(self, kind: StreamKind) -> bool {
        match self {
            StreamMode::Rgb => kind == StreamKind::Rgb,
            StreamMode::Depth => kind == StreamKind::Depth,
            StreamMode::Mix => true,
        }
    }

    /// Human readable codec(s) a subscriber receives.
    pub fn codec_label(self) -> &'static str {
        match self {
            StreamMode::Rgb => "jpeg",
            StreamMode::Depth => "z16",
            StreamMode::Mix => "jpeg+z16",
        }
    }
}

/// Settings of one `serve` run.
#[derive(Debug, Clone, Copy)]
pub struct ServeConfig {
    pub port: u16,
    /// Rate cap per stream. 0 means "as fast as the camera produces".
    pub fps: u32,
    pub quality: u8,
    /// Bitmask of streams to open. RGB is JPEG, depth is raw Z16.
    pub streams: u32,
    /// Mode a subscriber gets when it does not ask for one.
    pub default_mode: StreamMode,
}

/// Static facts about the camera, reported by `status`.
#[derive(Debug, Clone)]
pub struct CameraMeta {
    pub channel: u32,
    pub format: OutputFormat,
    pub source_fps: u32,
}

pub struct ServerState {
    hub: Arc<FrameHub>,
    channels: [broadcast::Sender<Arc<EncodedFrame>>; 2],
    subscribers: [AtomicUsize; 2],
    frames_encoded: [AtomicU64; 2],
    available: [bool; 2],
    started: Instant,
    meta: CameraMeta,
    config: ServeConfig,
}

impl ServerState {
    fn is_available(&self, stream: StreamKind) -> bool {
        self.available[stream.index()]
    }

    /// A mode is available when every stream it needs is open.
    fn mode_available(&self, mode: StreamMode) -> bool {
        StreamKind::ALL
            .iter()
            .all(|kind| !mode.wants(*kind) || self.is_available(*kind))
    }

    fn codec_for(stream: StreamKind) -> u8 {
        match stream {
            StreamKind::Rgb => wire::CODEC_JPEG,
            StreamKind::Depth => wire::CODEC_Z16,
        }
    }

    fn codec_name(stream: StreamKind) -> &'static str {
        match stream {
            StreamKind::Rgb => "jpeg",
            StreamKind::Depth => "z16",
        }
    }

    fn stream_json(&self, stream: StreamKind) -> serde_json::Value {
        let latest = self.hub.latest(stream);
        let index = stream.index();
        json!({
            "available": self.available[index],
            "codec": Self::codec_name(stream),
            "format": if stream == StreamKind::Depth { "z16" } else { self.meta.format.as_str() },
            "width": latest.as_ref().map(|f| f.width).unwrap_or(0),
            "height": latest.as_ref().map(|f| f.height).unwrap_or(0),
            "frame_seq": latest.as_ref().map(|f| f.seq).unwrap_or(0),
            "frames_encoded": self.frames_encoded[index].load(Ordering::Relaxed),
            "subscribers": self.subscribers[index].load(Ordering::Relaxed),
        })
    }

    fn status_json(&self) -> serde_json::Value {
        let mix_available =
            self.is_available(StreamKind::Rgb) && self.is_available(StreamKind::Depth);
        json!({
            "protocol": 3,
            "default_mode": self.config.default_mode.label(),
            "mix_available": mix_available,
            "streams": {
                "rgb": self.stream_json(StreamKind::Rgb),
                "depth": self.stream_json(StreamKind::Depth),
            },
            "channel": if self.meta.channel == u32::MAX { json!(null) } else { json!(self.meta.channel) },
            "source_fps": self.meta.source_fps,
            "encode_fps": self.config.fps,
            "quality": self.config.quality,
            "uptime_s": self.started.elapsed().as_secs(),
        })
    }
}

/// Move a connection to another mode, keeping the per stream subscriber counts
/// accurate. `mix` counts as a subscriber of both streams.
fn set_subscription(state: &ServerState, conn: &mut Connection, mode: Option<StreamMode>) {
    if conn.mode == mode {
        return;
    }
    if let Some(old) = conn.mode {
        for kind in StreamKind::ALL {
            if old.wants(kind) {
                state.subscribers[kind.index()].fetch_sub(1, Ordering::Relaxed);
            }
        }
    }
    if let Some(new) = mode {
        for kind in StreamKind::ALL {
            if new.wants(kind) {
                state.subscribers[kind.index()].fetch_add(1, Ordering::Relaxed);
            }
        }
    }
    conn.mode = mode;
}

/// Let the next frame of every stream through immediately after a switch.
fn reset_send_times(conn: &mut Connection) {
    let start = Instant::now() - Duration::from_secs(1);
    conn.last_sent = [start, start];
}

/// Bind the protocol port and serve until Ctrl+C. The camera is owned by the
/// caller, which stops it once this returns. `available` marks the streams the
/// camera actually started.
pub async fn run(
    hub: Arc<FrameHub>,
    config: ServeConfig,
    meta: CameraMeta,
    available: [bool; 2],
) -> Result<()> {
    let listener = TcpListener::bind(("0.0.0.0", config.port))
        .await
        .with_context(|| format!("could not bind TCP port {}", config.port))?;
    let addr = listener.local_addr()?;
    info!(
        "inu-r132: protocol listening on {addr} (streams: {})",
        StreamKind::ALL
            .iter()
            .filter(|kind| available[kind.index()])
            .map(|kind| kind.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    );

    let (rgb, _) = broadcast::channel(4);
    let (depth, _) = broadcast::channel(4);
    let state = Arc::new(ServerState {
        hub: hub.clone(),
        channels: [rgb, depth],
        subscribers: [AtomicUsize::new(0), AtomicUsize::new(0)],
        frames_encoded: [AtomicU64::new(0), AtomicU64::new(0)],
        available,
        started: Instant::now(),
        meta,
        config,
    });

    for stream in StreamKind::ALL {
        if state.is_available(stream) {
            tokio::spawn(encoder_task(state.clone(), stream));
        }
    }

    loop {
        tokio::select! {
            accepted = listener.accept() => {
                match accepted {
                    Ok((stream, peer)) => {
                        debug!("connection from {peer}");
                        let state = state.clone();
                        tokio::spawn(async move {
                            if let Err(e) = handle_connection(stream, state.clone()).await {
                                debug!("connection {peer} ended: {e}");
                            }
                        });
                    }
                    Err(e) => warn!("accept failed: {e}"),
                }
            }
            _ = tokio::signal::ctrl_c() => {
                info!("inu-r132: Ctrl+C received, shutting down");
                break;
            }
        }
    }

    Ok(())
}

/// Prepare the newest frame of one stream for all its subscribers. RGB frames
/// are encoded to JPEG; depth frames are passed through as raw little endian Z16.
async fn encoder_task(state: Arc<ServerState>, stream: StreamKind) {
    let codec = ServerState::codec_for(stream);
    let index = stream.index();
    let broadcaster = state.channels[index].clone();
    let mut last_seq = state.hub.latest_seq(stream);
    let min_interval = if state.config.fps == 0 {
        Duration::ZERO
    } else {
        Duration::from_secs_f64(1.0 / state.config.fps as f64)
    };
    let mut last_encode = Instant::now() - min_interval;

    loop {
        state.hub.changed(stream, last_seq).await;
        let Some(raw) = state.hub.latest(stream) else {
            continue;
        };
        if raw.seq == last_seq {
            continue;
        }

        if state.subscribers[index].load(Ordering::Relaxed) == 0 {
            // Nobody is listening: track the sequence without doing the work.
            last_seq = raw.seq;
            continue;
        }

        let elapsed = last_encode.elapsed();
        if elapsed < min_interval {
            tokio::time::sleep(min_interval - elapsed).await;
        }
        last_encode = Instant::now();
        last_seq = raw.seq;

        let quality = state.config.quality;
        let prepared = tokio::task::spawn_blocking(move || {
            let payload = match codec {
                wire::CODEC_JPEG => {
                    frame::encode_jpeg(&raw.to_rgb(), raw.width, raw.height, quality)
                }
                // Depth is already the wire format (u16 LE per pixel).
                _ => Ok(raw.data.clone()),
            };
            payload.map(|payload| EncodedFrame {
                stream,
                codec,
                width: raw.width,
                height: raw.height,
                seq: raw.seq,
                timestamp: raw.timestamp,
                payload,
            })
        })
        .await;

        match prepared {
            Ok(Ok(encoded)) => {
                state.frames_encoded[index].fetch_add(1, Ordering::Relaxed);
                let _ = broadcaster.send(Arc::new(encoded));
            }
            Ok(Err(e)) => warn!("{stream:?} frame preparation failed: {e}"),
            Err(e) => warn!("{stream:?} frame preparation task panicked: {e}"),
        }
    }
}

/// Per connection state.
struct Connection {
    /// Currently subscribed mode, `None` while unsubscribed.
    mode: Option<StreamMode>,
    /// Subscriber side rate limit, `None` means "as fast as the server sends".
    fps: Option<u32>,
    /// Last send per stream, so `mix` caps each stream separately.
    last_sent: [Instant; 2],
}

impl Connection {
    fn new() -> Self {
        let start = Instant::now() - Duration::from_secs(1);
        Self {
            mode: None,
            fps: None,
            last_sent: [start, start],
        }
    }

    fn subscribed(&self) -> bool {
        self.mode.is_some()
    }

    fn wants(&self, kind: StreamKind) -> bool {
        self.mode.is_some_and(|mode| mode.wants(kind))
    }

    fn min_interval(&self) -> Duration {
        match self.fps {
            Some(fps) if fps > 0 => Duration::from_secs_f64(1.0 / fps as f64),
            _ => Duration::ZERO,
        }
    }

    fn ready_to_send(&mut self, kind: StreamKind) -> bool {
        let index = kind.index();
        if self.last_sent[index].elapsed() < self.min_interval() {
            return false;
        }
        self.last_sent[index] = Instant::now();
        true
    }
}

async fn handle_connection(stream: TcpStream, state: Arc<ServerState>) -> Result<()> {
    stream.set_nodelay(true).ok();
    let (read_half, mut write_half) = tokio::io::split(stream);
    let mut lines = BufReader::new(read_half).lines();
    let mut rgb = state.channels[0].subscribe();
    let mut depth = state.channels[1].subscribe();
    let mut conn = Connection::new();

    // Greet so a client can tell it reached the camera protocol.
    let hello = wire::encode_text(&format!(
        "inu-r132 protocol 2 (send `help` for commands); {}",
        serde_json::to_string(&state.status_json()).unwrap_or_default()
    ));
    wire::write_message(&mut write_half, &hello).await?;

    let result = loop {
        tokio::select! {
            line = lines.next_line() => {
                let line = match line {
                    Ok(Some(line)) => line,
                    Ok(None) => break Ok(()),
                    Err(e) => break Err(e.into()),
                };
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                match handle_command(line, &state, &mut conn) {
                    CommandOutcome::Reply(messages) => {
                        for message in messages {
                            wire::write_message(&mut write_half, &message).await?;
                        }
                    }
                    CommandOutcome::Close(messages) => {
                        for message in messages {
                            wire::write_message(&mut write_half, &message).await?;
                        }
                        break Ok(());
                    }
                }
            }
            frame = rgb.recv(), if conn.wants(StreamKind::Rgb) => {
                match frame {
                    Ok(frame) => {
                        if conn.ready_to_send(StreamKind::Rgb) {
                            let message = wire::encode_frame(frame.width, frame.height, frame.codec, &frame.payload);
                            wire::write_message(&mut write_half, &message).await?;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        debug!("subscriber lagged, dropped {skipped} rgb frames");
                    }
                    Err(broadcast::error::RecvError::Closed) => break Ok(()),
                }
            }
            frame = depth.recv(), if conn.wants(StreamKind::Depth) => {
                match frame {
                    Ok(frame) => {
                        if conn.ready_to_send(StreamKind::Depth) {
                            let message = wire::encode_frame(frame.width, frame.height, frame.codec, &frame.payload);
                            wire::write_message(&mut write_half, &message).await?;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        debug!("subscriber lagged, dropped {skipped} depth frames");
                    }
                    Err(broadcast::error::RecvError::Closed) => break Ok(()),
                }
            }
        }
    };

    if conn.subscribed() {
        set_subscription(&state, &mut conn, None);
    }
    write_half.shutdown().await.ok();
    result
}

enum CommandOutcome {
    Reply(Vec<Vec<u8>>),
    Close(Vec<Vec<u8>>),
}

fn text(message: String) -> Vec<u8> {
    wire::encode_text(&message)
}

fn stream_from_name(name: &str) -> Option<StreamMode> {
    match name.to_ascii_lowercase().as_str() {
        "rgb" | "color" | "colour" => Some(StreamMode::Rgb),
        "depth" | "z16" => Some(StreamMode::Depth),
        "mix" | "both" | "overlay" => Some(StreamMode::Mix),
        _ => None,
    }
}

fn handle_command(line: &str, state: &Arc<ServerState>, conn: &mut Connection) -> CommandOutcome {
    let mut parts = line.split_whitespace();
    let command = parts.next().unwrap_or_default();
    let args: Vec<&str> = parts.collect();

    match command {
        "help" => CommandOutcome::Reply(vec![text(
            "commands: status | subscribe [fps] [rgb|depth|mix] | stream <rgb|depth|mix> | \
             unsubscribe | fps <n> | ping | help | quit"
                .to_string(),
        )]),
        "ping" => CommandOutcome::Reply(vec![text("pong".to_string())]),
        "status" => {
            let mut status = state.status_json();
            if let Some(object) = status.as_object_mut() {
                object.insert(
                    "connection_mode".to_string(),
                    json!(conn.mode.map(|mode| mode.label())),
                );
            }
            CommandOutcome::Reply(vec![text(format!(
                "status {}",
                serde_json::to_string(&status).unwrap_or_else(|_| "{}".to_string())
            ))])
        }
        "subscribe" => {
            let mut chosen = conn.mode.unwrap_or(state.config.default_mode);
            for value in &args {
                if let Ok(fps) = value.parse::<u32>() {
                    conn.fps = Some(fps);
                    continue;
                }
                match stream_from_name(value) {
                    Some(mode) => chosen = mode,
                    None => {
                        return CommandOutcome::Reply(vec![text(format!(
                            "err expected a frame rate or a stream name, got `{value}`"
                        ))])
                    }
                }
            }

            if !state.mode_available(chosen) {
                return CommandOutcome::Reply(vec![text(format!(
                    "err the {} stream is not available on this server (mix needs rgb and depth)",
                    chosen.label()
                ))]);
            }

            set_subscription(state, conn, Some(chosen));
            reset_send_times(conn);
            let fps = conn
                .fps
                .map(|f| f.to_string())
                .unwrap_or_else(|| state.config.fps.to_string());
            CommandOutcome::Reply(vec![text(format!(
                "ok subscribed stream={} codec={} fps={fps}",
                chosen.label(),
                chosen.codec_label()
            ))])
        }
        "stream" => {
            let Some(name) = args.first() else {
                return CommandOutcome::Reply(vec![text(
                    "err stream needs rgb, depth or mix".to_string(),
                )]);
            };
            let Some(mode) = stream_from_name(name) else {
                return CommandOutcome::Reply(vec![text(format!(
                    "err unknown stream `{name}` (rgb, depth or mix)"
                ))]);
            };
            if !state.mode_available(mode) {
                return CommandOutcome::Reply(vec![text(format!(
                    "err the {} stream is not available on this server (mix needs rgb and depth)",
                    mode.label()
                ))]);
            }
            set_subscription(state, conn, Some(mode));
            reset_send_times(conn);
            CommandOutcome::Reply(vec![text(format!(
                "ok stream={} codec={}",
                mode.label(),
                mode.codec_label()
            ))])
        }
        "unsubscribe" => {
            set_subscription(state, conn, None);
            conn.fps = None;
            CommandOutcome::Reply(vec![text("ok unsubscribed".to_string())])
        }
        "fps" => {
            let Some(value) = args.first() else {
                return CommandOutcome::Reply(vec![text("err fps needs a value".to_string())]);
            };
            match value.parse::<u32>() {
                Ok(fps) => {
                    conn.fps = Some(fps);
                    CommandOutcome::Reply(vec![text(format!("ok fps={fps}"))])
                }
                Err(_) => CommandOutcome::Reply(vec![text(format!("err invalid fps `{value}`"))]),
            }
        }
        "quit" | "exit" => CommandOutcome::Close(vec![wire::encode_bye("bye")]),
        other => CommandOutcome::Reply(vec![text(format!(
            "err unknown command `{other}` (try help)"
        ))]),
    }
}
