//! Frame storage shared between the SDK callback thread, the TCP subscribers
//! and the local window loop.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, Result};
use tokio::sync::Notify;

use crate::inu::StreamKind;

/// Pixel format of a received frame (`InuDev::EImageFormat`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PixelFormat {
    /// 16-bit Z-buffer in millimetres (`eDepth` = 1). 0 means "no depth".
    Depth16,
    Bgra,
    Bgr,
    Rgba,
    /// 16-bit: 4 MSB confidence + 12 LSB disparity (`eDisparity` = 4).
    Disparity16,
    Unknown,
}

impl PixelFormat {
    pub fn from_raw(raw: i32) -> Self {
        match raw {
            1 => PixelFormat::Depth16,
            2 => PixelFormat::Bgr,
            3 => PixelFormat::Bgra,
            4 => PixelFormat::Disparity16,
            7 => PixelFormat::Rgba,
            _ => PixelFormat::Unknown,
        }
    }

    pub fn bytes_per_pixel(self) -> usize {
        match self {
            PixelFormat::Depth16 | PixelFormat::Disparity16 => 2,
            PixelFormat::Bgr => 3,
            _ => 4,
        }
    }

    /// Is this a 16-bit single channel format (depth / disparity)?
    pub fn is_16bit(self) -> bool {
        matches!(self, PixelFormat::Depth16 | PixelFormat::Disparity16)
    }

    /// Short lowercase name, for log lines and messages.
    pub fn label(self) -> &'static str {
        match self {
            PixelFormat::Depth16 => "depth16",
            PixelFormat::Bgra => "bgra",
            PixelFormat::Bgr => "bgr",
            PixelFormat::Rgba => "rgba",
            PixelFormat::Disparity16 => "disparity16",
            PixelFormat::Unknown => "unknown",
        }
    }
}

/// One frame from the sensor, tightly packed (no row padding).
pub struct RawFrame {
    pub stream: StreamKind,
    pub data: Vec<u8>,
    pub width: usize,
    pub height: usize,
    pub format: PixelFormat,
    pub timestamp: u64,
    pub seq: u64,
}

impl RawFrame {
    /// Copy one SDK callback buffer into a tightly packed frame. Invalid
    /// geometry returns `None` instead of panicking: this runs on the SDK
    /// thread and must never unwind into it.
    pub fn from_callback(
        stream: StreamKind,
        data: *const u8,
        width: i32,
        height: i32,
        stride: i32,
        format_raw: i32,
        timestamp: u64,
        seq: u64,
    ) -> Option<Self> {
        if data.is_null() || width <= 0 || height <= 0 {
            return None;
        }

        let w = width as usize;
        let h = height as usize;
        let src_stride = stride.max(0) as usize;
        let format = PixelFormat::from_raw(format_raw);
        let bpp = match src_stride.checked_div(w) {
            Some(bpp) if bpp > 0 => bpp,
            _ => format.bytes_per_pixel(),
        };
        let row_bytes = w * bpp;
        if row_bytes == 0 || src_stride < row_bytes {
            return None;
        }

        let src = unsafe { std::slice::from_raw_parts(data, src_stride * h) };
        let mut packed = Vec::with_capacity(row_bytes * h);
        for row in 0..h {
            let start = row * src_stride;
            packed.extend_from_slice(&src[start..start + row_bytes]);
        }

        Some(Self {
            stream,
            data: packed,
            width: w,
            height: h,
            format,
            timestamp,
            seq,
        })
    }

    /// Repack into RGB8, the layout the JPEG encoder wants. Only meaningful for
    /// the colour formats; depth/disparity produce an empty buffer.
    pub fn to_rgb(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.width * self.height * 3);
        match self.format {
            PixelFormat::Bgra => {
                for px in self.data.chunks_exact(4) {
                    out.extend_from_slice(&[px[2], px[1], px[0]]);
                }
            }
            PixelFormat::Rgba => {
                for px in self.data.chunks_exact(4) {
                    out.extend_from_slice(&[px[0], px[1], px[2]]);
                }
            }
            PixelFormat::Bgr => out.extend_from_slice(&self.data),
            PixelFormat::Depth16 | PixelFormat::Disparity16 => {}
            PixelFormat::Unknown => {
                if self.data.len() >= self.width * self.height * 4 {
                    for px in self.data.chunks_exact(4) {
                        out.extend_from_slice(&[px[2], px[1], px[0]]);
                    }
                } else {
                    out.extend_from_slice(&self.data);
                }
            }
        }
        out
    }

    /// Repack into minifb's `0x00RRGGBB` buffer. The viewer goes through
    /// `to_rgb` + `rgb_to_u32` so it can hand the same pixels to the detector.
    #[allow(dead_code)]
    pub fn to_rgb_u32(&self) -> Vec<u32> {
        let rgb = self.to_rgb();
        rgb.chunks_exact(3)
            .map(|px| ((px[0] as u32) << 16) | ((px[1] as u32) << 8) | px[2] as u32)
            .collect()
    }

    /// Read a 16-bit single channel frame as little endian `u16` samples.
    /// Depth is in millimetres, 0 means "no measurement".
    #[allow(dead_code)]
    pub fn to_u16(&self) -> Vec<u16> {
        self.data
            .chunks_exact(2)
            .map(|px| u16::from_le_bytes([px[0], px[1]]))
            .collect()
    }

    /// Render a 16-bit frame as near-bright/far-dark grey.
    ///
    /// `near`/`far` are the display window; values outside it clamp to
    /// white/black and `0` (no measurement) is black. `gamma` reshapes the
    /// curve: 1.0 is linear, below 1.0 brightens the middle so a distant
    /// cluster gets more separation. For disparity the caller passes the
    /// disparity range instead, which works the same way.
    pub fn to_gray_u32(&self, near: u16, far: u16, gamma: f32) -> Vec<u32> {
        z16_to_gray_u32(&self.data, near, far, gamma)
    }
}

/// Summary of a Z16 frame, used for the auto window and the window title.
#[derive(Debug, Clone, Copy)]
pub struct Z16Summary {
    pub min: u16,
    pub p2: u16,
    pub p50: u16,
    pub p98: u16,
    pub max: u16,
    /// Fraction of pixels that are 0 (no measurement).
    pub invalid: f32,
}

/// Render raw little-endian Z16 bytes as near-bright/far-dark grey.
pub fn z16_to_gray_u32(bytes: &[u8], near: u16, far: u16, gamma: f32) -> Vec<u32> {
    let span = far.saturating_sub(near).max(1) as f32;
    let gamma = if gamma > 0.0 { gamma } else { 1.0 };
    bytes
        .chunks_exact(2)
        .map(|px| {
            let value = u16::from_le_bytes([px[0], px[1]]);
            if value == 0 {
                return 0;
            }
            let linear = ((value.saturating_sub(near) as f32) / span).clamp(0.0, 1.0);
            // Near is bright (1.0), far is dark (0.0).
            let level = (1.0 - linear).powf(gamma);
            let level = (level * 255.0 + 0.5) as u32;
            (level << 16) | (level << 8) | level
        })
        .collect()
}

/// Percentiles and range of the valid pixels, from a coarse histogram so the
/// pass stays cheap on a 1080p frame.
pub fn z16_summary(bytes: &[u8]) -> Option<Z16Summary> {
    const BINS: usize = 4096; // 16 mm per bin
    let mut histogram = vec![0u32; BINS];
    let mut valid: u64 = 0;
    let mut min = u16::MAX;
    let mut max = 0u16;
    for px in bytes.chunks_exact(2) {
        let value = u16::from_le_bytes([px[0], px[1]]);
        if value == 0 {
            continue;
        }
        histogram[(value >> 4) as usize] += 1;
        valid += 1;
        min = min.min(value);
        max = max.max(value);
    }
    if valid == 0 {
        return None;
    }

    let percentile = |fraction: f64| -> u16 {
        let target = (valid as f64 * fraction) as u64;
        let mut accumulated = 0u64;
        for (bin, count) in histogram.iter().enumerate() {
            accumulated += *count as u64;
            if accumulated >= target {
                return (((bin as u32) << 4) + 15).min(65535) as u16;
            }
        }
        max
    };

    let total = (bytes.len() / 2) as f32;
    Some(Z16Summary {
        min,
        p2: percentile(0.02),
        p50: percentile(0.50),
        p98: percentile(0.98),
        max,
        invalid: 1.0 - valid as f32 / total.max(1.0),
    })
}

/// Average the valid samples of a 3x3 window at a normalised point of a 16-bit
/// frame, plus how many of the nine were valid. A single pixel is noisy and
/// depth has holes, so "0 valid" means the whole neighbourhood is invalid.
/// Everything that reports a distance goes through here.
pub fn sample_z16(raw: &[u8], width: usize, height: usize, u: f64, v: f64) -> (Option<u16>, usize) {
    if raw.len() < 2 || width == 0 || height == 0 || !u.is_finite() || !v.is_finite() {
        return (None, 0);
    }

    let centre_x = (u * width as f64).clamp(0.0, (width - 1) as f64) as i64;
    let centre_y = (v * height as f64).clamp(0.0, (height - 1) as f64) as i64;

    let mut sum: u32 = 0;
    let mut count = 0usize;
    for dy in -1i64..=1 {
        for dx in -1i64..=1 {
            let px = centre_x + dx;
            let py = centre_y + dy;
            if px < 0 || py < 0 || px >= width as i64 || py >= height as i64 {
                continue;
            }

            let index = (py as usize * width + px as usize) * 2;
            if index + 1 >= raw.len() {
                continue;
            }

            let value = u16::from_le_bytes([raw[index], raw[index + 1]]);
            if value == 0 {
                continue;
            }

            sum += value as u32;
            count += 1;
        }
    }

    if count == 0 {
        (None, 0)
    } else {
        (Some((sum / count as u32) as u16), count)
    }
}

/// Distance at the centre of a box, with the box given in the coordinates of the
/// frame the detector saw. Registered depth is aligned to the colour camera, so
/// normalising both puts the box centre on the same point of the depth image
/// even if the two frames differ in size.
#[allow(clippy::too_many_arguments)]
pub fn sample_box_centre(
    raw: &[u8],
    width: usize,
    height: usize,
    x1: f32,
    y1: f32,
    x2: f32,
    y2: f32,
    frame_width: usize,
    frame_height: usize,
) -> (Option<u16>, usize) {
    if frame_width == 0 || frame_height == 0 {
        return (None, 0);
    }
    let u = ((x1 + x2) as f64 / 2.0) / frame_width as f64;
    let v = ((y1 + y2) as f64 / 2.0) / frame_height as f64;
    sample_z16(raw, width, height, u, v)
}

/// The newest frame of every stream, shared with the SDK callback and the
/// consumers. One slot per `StreamKind`.
pub struct FrameHub {
    latest: [Mutex<Option<Arc<RawFrame>>>; 2],
    seq: AtomicU64,
    notify: Notify,
}

impl FrameHub {
    pub fn new() -> Self {
        Self {
            latest: [Mutex::new(None), Mutex::new(None)],
            seq: AtomicU64::new(0),
            notify: Notify::new(),
        }
    }

    /// Store a frame and wake every consumer. Called from the SDK thread, so
    /// it never blocks on a slow consumer.
    pub fn publish(&self, mut frame: RawFrame) {
        let seq = self.seq.fetch_add(1, Ordering::Relaxed) + 1;
        frame.seq = seq;
        let index = frame.stream.index();
        if let Ok(mut slot) = self.latest[index].try_lock() {
            *slot = Some(Arc::new(frame));
        }
        self.notify.notify_waiters();
    }

    pub fn latest(&self, stream: StreamKind) -> Option<Arc<RawFrame>> {
        self.latest[stream.index()]
            .lock()
            .ok()
            .and_then(|slot| slot.clone())
    }

    /// Wait until `stream` has a frame newer than `last_seq`.
    pub async fn changed(&self, stream: StreamKind, last_seq: u64) {
        loop {
            // Register the waiter before checking, so a frame published in
            // between cannot be missed.
            let notified = self.notify.notified();
            if let Some(frame) = self.latest(stream) {
                if frame.seq > last_seq {
                    return;
                }
            }
            notified.await;
        }
    }

    /// Sequence of the newest frame of `stream`, 0 when there is none yet.
    pub fn latest_seq(&self, stream: StreamKind) -> u64 {
        self.latest(stream).map(|frame| frame.seq).unwrap_or(0)
    }
}

impl Default for FrameHub {
    fn default() -> Self {
        Self::new()
    }
}

/// A frame prepared once and shared with every subscriber. `payload` is the
/// encoded bytes for `codec` (`wire::CODEC_JPEG` or `wire::CODEC_Z16`).
#[allow(dead_code)]
pub struct EncodedFrame {
    pub stream: StreamKind,
    pub codec: u8,
    pub width: usize,
    pub height: usize,
    pub seq: u64,
    pub timestamp: u64,
    pub payload: Vec<u8>,
}

/// Encode an RGB8 buffer as JPEG.
pub fn encode_jpeg(rgb: &[u8], width: usize, height: usize, quality: u8) -> Result<Vec<u8>> {
    use image::codecs::jpeg::JpegEncoder;
    use image::ExtendedColorType;

    let mut out = Vec::new();
    let mut encoder = JpegEncoder::new_with_quality(&mut out, quality.clamp(1, 100));
    encoder
        .encode(rgb, width as u32, height as u32, ExtendedColorType::Rgb8)
        .map_err(|e| anyhow!("JPEG encode failed: {e}"))?;
    Ok(out)
}

/// Decode a JPEG frame received from a remote `serve`.
pub fn decode_jpeg(jpeg: &[u8]) -> Result<(usize, usize, Vec<u8>)> {
    let image = image::load_from_memory_with_format(jpeg, image::ImageFormat::Jpeg)
        .map_err(|e| anyhow!("JPEG decode failed: {e}"))?
        .to_rgb8();
    let (width, height) = image.dimensions();
    Ok((width as usize, height as usize, image.into_raw()))
}

/// Convert an RGB8 buffer into minifb's `0x00RRGGBB` layout.
pub fn rgb_to_u32(rgb: &[u8]) -> Vec<u32> {
    rgb.chunks_exact(3)
        .map(|px| ((px[0] as u32) << 16) | ((px[1] as u32) << 8) | px[2] as u32)
        .collect()
}

/// A synthetic moving BGR frame, used by `serve --mock` to exercise the
/// protocol (and `display --remote`) without an NU4000 attached.
pub fn mock_bgra(width: usize, height: usize, tick: u64) -> Vec<u8> {
    let mut data = Vec::with_capacity(width * height * 4);
    for y in 0..height {
        for x in 0..width {
            let b = ((x as u64 + tick * 4) % 256) as u8;
            let g = (y % 256) as u8;
            let r = (((x + y) as u64 + tick * 8) % 256) as u8;
            data.extend_from_slice(&[b, g, r, 255]);
        }
    }
    data
}

/// Publish synthetic frames of `stream` at a steady rate until dropped.
pub async fn run_mock(
    hub: Arc<FrameHub>,
    width: usize,
    height: usize,
    fps: u32,
    stream: StreamKind,
) {
    let period = Duration::from_secs_f64(1.0 / fps.max(1) as f64);
    let mut ticker = tokio::time::interval(period);
    let mut tick = 0u64;
    loop {
        ticker.tick().await;
        tick += 1;
        let (data, format) = match stream {
            StreamKind::Rgb => (mock_bgra(width, height, tick), PixelFormat::Bgra),
            StreamKind::Depth => (mock_z16(width, height, tick), PixelFormat::Depth16),
        };
        hub.publish(RawFrame {
            stream,
            data,
            width,
            height,
            format,
            timestamp: tick,
            seq: 0,
        });
    }
}

/// A synthetic 16-bit depth ramp in millimetres, with a few invalid pixels.
pub fn mock_z16(width: usize, height: usize, tick: u64) -> Vec<u8> {
    let mut data = Vec::with_capacity(width * height * 2);
    for y in 0..height {
        for x in 0..width {
            let value = if (x + y) % 32 == 0 {
                0 // a stripe of "no measurement"
            } else {
                (((x + y) as u64 * 8 + tick * 40) % 8000) as u16
            };
            data.extend_from_slice(&value.to_le_bytes());
        }
    }
    data
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> RawFrame {
        RawFrame {
            stream: StreamKind::Rgb,
            data: mock_bgra(64, 48, 7),
            width: 64,
            height: 48,
            format: PixelFormat::Bgra,
            timestamp: 7,
            seq: 1,
        }
    }

    #[test]
    fn jpeg_roundtrip() {
        let raw = sample();
        let rgb = raw.to_rgb();
        assert_eq!(rgb.len(), 64 * 48 * 3);

        let jpeg = encode_jpeg(&rgb, 64, 48, 80).unwrap();
        assert_eq!(&jpeg[..2], &[0xff, 0xd8]);
        assert_eq!(&jpeg[jpeg.len() - 2..], &[0xff, 0xd9]);

        let (width, height, decoded) = decode_jpeg(&jpeg).unwrap();
        assert_eq!((width, height), (64, 48));
        assert_eq!(decoded.len(), 64 * 48 * 3);
    }

    #[test]
    fn minifb_buffer_layout() {
        let raw = sample();
        let buffer = raw.to_rgb_u32();
        assert_eq!(buffer.len(), 64 * 48);
        // The top-left pixel of tick 7 is B=28, G=0, R=56.
        assert_eq!(buffer[0], (56 << 16) | (0 << 8) | 28);
    }

    #[test]
    fn tight_rows_are_packed() {
        let data: Vec<u8> = (0..16).collect();
        let frame = RawFrame::from_callback(StreamKind::Rgb, data.as_ptr(), 2, 2, 8, 3, 0, 0)
            .expect("valid frame");
        assert_eq!(frame.data, data);
        assert_eq!((frame.width, frame.height), (2, 2));
    }

    #[test]
    fn invalid_geometry_is_rejected() {
        let data: Vec<u8> = (0..16).collect();
        // No dimensions, a null pointer and a stride smaller than a row.
        assert!(
            RawFrame::from_callback(StreamKind::Rgb, data.as_ptr(), 0, 2, 8, 3, 0, 0).is_none()
        );
        assert!(
            RawFrame::from_callback(StreamKind::Rgb, std::ptr::null(), 2, 2, 8, 3, 0, 0).is_none()
        );
        assert!(
            RawFrame::from_callback(StreamKind::Rgb, data.as_ptr(), 4, 1, 3, 3, 0, 0).is_none()
        );
    }

    #[test]
    fn depth_gray_mapping() {
        // near = 1000 mm (bright), far = 3000 mm (dark), 0 = invalid.
        let values: [u16; 5] = [0, 1000, 2000, 3000, 9999];
        let mut bytes = Vec::new();
        for value in values {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        let buffer = z16_to_gray_u32(&bytes, 1000, 3000, 1.0);
        assert_eq!(buffer[0], 0); // invalid -> black
        assert_eq!(buffer[1] & 0xff, 255); // near -> white
        assert_eq!(buffer[2] & 0xff, 128); // middle
        assert_eq!(buffer[3] & 0xff, 0); // far -> black
        assert_eq!(buffer[4] & 0xff, 0); // beyond far -> black
                                         // gamma below 1 brightens the middle
        let brighter = z16_to_gray_u32(&bytes, 1000, 3000, 0.5);
        assert!(brighter[2] & 0xff > buffer[2] & 0xff);
    }

    #[test]
    fn z16_summary_reports_percentiles() {
        // 0..99 mm in 1 mm steps plus one invalid pixel.
        let mut bytes = Vec::new();
        for value in 0u16..100 {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        let summary = z16_summary(&bytes).expect("valid frame");
        assert_eq!(summary.min, 1);
        assert_eq!(summary.max, 99);
        assert!(summary.p2 <= summary.p50 && summary.p50 <= summary.p98);
        // 16 mm histogram bins, so the median is only approximate.
        assert!(summary.p50 >= 40 && summary.p50 <= 70);
        assert!((summary.invalid - 0.01).abs() < 1e-6);
    }
}

#[cfg(test)]
mod sampler_tests {
    use super::*;

    fn frame(width: usize, height: usize, value: impl Fn(usize, usize) -> u16) -> Vec<u8> {
        let mut raw = Vec::with_capacity(width * height * 2);
        for y in 0..height {
            for x in 0..width {
                raw.extend_from_slice(&value(x, y).to_le_bytes());
            }
        }
        raw
    }

    #[test]
    fn samples_a_flat_frame() {
        let raw = frame(8, 6, |_, _| 1000);
        assert_eq!(sample_z16(&raw, 8, 6, 0.5, 0.5), (Some(1000), 9));
    }

    #[test]
    fn the_box_centre_lands_on_the_mapped_pixel() {
        // Box (0,0)-(8,6) of a 16x12 detector frame -> (0.25, 0.25) -> pixel (2,1),
        // whose 3x3 window holds one 2500 and eight 1000s.
        let raw = frame(8, 6, |x, y| if x == 2 && y == 1 { 2500 } else { 1000 });
        assert_eq!(
            sample_box_centre(&raw, 8, 6, 0.0, 0.0, 8.0, 6.0, 16, 12),
            (Some((2500 + 8 * 1000) / 9), 9)
        );
    }

    #[test]
    fn holes_are_skipped_not_averaged_in() {
        let raw = frame(8, 6, |x, y| if x == 4 && y == 3 { 0 } else { 1200 });
        let (value, valid) = sample_z16(&raw, 8, 6, 4.0 / 8.0, 3.0 / 8.0);
        assert_eq!(value, Some(1200));
        assert_eq!(
            valid, 8,
            "the hole is not counted and does not drag the mean to 0"
        );
    }

    #[test]
    fn no_valid_sample_is_reported_as_none() {
        let raw = frame(8, 6, |_, _| 0);
        assert_eq!(sample_z16(&raw, 8, 6, 0.5, 0.5), (None, 0));
    }

    #[test]
    fn nonsense_inputs_do_not_panic() {
        let raw = frame(8, 6, |_, _| 1000);
        assert_eq!(sample_z16(&raw, 8, 6, f64::NAN, 0.5), (None, 0));
        assert_eq!(sample_z16(&raw, 8, 6, -5.0, 99.0).0, Some(1000), "clamped");
        assert_eq!(sample_z16(&[], 8, 6, 0.5, 0.5), (None, 0));
        assert_eq!(sample_z16(&[0, 1], 0, 0, 0.5, 0.5), (None, 0));
        assert_eq!(sample_z16(&[0, 1], 8, 6, 0.5, 0.5), (None, 0), "truncated");
        assert_eq!(
            sample_box_centre(&raw, 8, 6, 1.0, 1.0, 2.0, 2.0, 0, 0),
            (None, 0)
        );
    }

    #[test]
    fn a_mismatched_depth_size_still_maps_by_fraction() {
        let raw = frame(4, 3, |_, _| 1000);
        assert_eq!(
            sample_box_centre(&raw, 4, 3, 0.0, 0.0, 16.0, 12.0, 16, 12),
            (Some(1000), 9)
        );
    }
}
