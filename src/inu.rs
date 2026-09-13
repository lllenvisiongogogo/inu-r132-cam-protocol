//! Safe-ish wrapper around the C++ shim.

use std::ffi::{CStr, CString};
use std::ptr;

use anyhow::{bail, Result};

use crate::sys::{self, Shim};

/// Pixel format requested from the sensor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputFormat {
    Bgra,
    Bgr,
    Rgba,
}

impl OutputFormat {
    pub fn as_raw(self) -> i32 {
        match self {
            OutputFormat::Bgra => 0,
            OutputFormat::Bgr => 1,
            OutputFormat::Rgba => 2,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            OutputFormat::Bgra => "bgra",
            OutputFormat::Bgr => "bgr",
            OutputFormat::Rgba => "rgba",
        }
    }
}

/// Which sensor stream to open. The values mirror the shim's `INU_STREAM_*`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamKind {
    Rgb,
    Depth,
}

impl StreamKind {
    /// Every stream the shim can open, in display order.
    pub const ALL: [StreamKind; 2] = [StreamKind::Rgb, StreamKind::Depth];

    pub fn bit(self) -> u32 {
        match self {
            StreamKind::Rgb => 0x1,
            StreamKind::Depth => 0x2,
        }
    }

    pub fn mask(kinds: &[StreamKind]) -> u32 {
        kinds.iter().fold(0, |mask, kind| mask | kind.bit())
    }

    pub fn from_bit(bit: u32) -> Option<StreamKind> {
        match bit {
            0x1 => Some(StreamKind::Rgb),
            0x2 => Some(StreamKind::Depth),
            _ => None,
        }
    }

    pub fn index(self) -> usize {
        match self {
            StreamKind::Rgb => 0,
            StreamKind::Depth => 1,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            StreamKind::Rgb => "rgb",
            StreamKind::Depth => "depth",
        }
    }
}

/// A connected + initialized NU4000 sensor.
pub struct InuCamera {
    api: Shim,
    ptr: *mut sys::InuContext,
}

// The SDK delivers frames from its own threads; the handle itself is only used
// from the owning thread but must be movable into the window loop.
unsafe impl Send for InuCamera {}

impl InuCamera {
    /// Connect to the sensor and initialize it (does not yet start streaming).
    pub fn open(
        service_name: Option<&str>,
        fps: u32,
        channel_id: i32,
        streams: u32,
        registered: bool,
        output_format: OutputFormat,
    ) -> Result<Self> {
        let api = Shim::load()?;

        let c_service = match service_name {
            Some(name) if !name.is_empty() => Some(CString::new(name)?),
            _ => None,
        };

        let options = sys::InuOpenOptions {
            service_name: c_service.as_ref().map_or(ptr::null(), |s| s.as_ptr()),
            fps,
            channel_id,
            output_format: output_format.as_raw(),
            streams,
            registered_depth: if registered { 1 } else { 0 },
        };

        let ptr = unsafe { (api.open)(&options) };
        if ptr.is_null() {
            bail!("could not open the NU4000: {}", last_error(&api));
        }
        Ok(Self { api, ptr })
    }

    pub fn set_frame_callback(&self, callback: sys::InuFrameCallback) -> Result<()> {
        let rc =
            unsafe { (self.api.set_frame_callback)(self.ptr, Some(callback), ptr::null_mut()) };
        if rc != 0 {
            bail!(
                "could not register the frame callback: {}",
                last_error(&self.api)
            );
        }
        Ok(())
    }

    /// Create and start the RGB stream.
    pub fn start(&self) -> Result<()> {
        let rc = unsafe { (self.api.start)(self.ptr) };
        if rc != 0 {
            bail!("could not start the RGB stream: {}", last_error(&self.api));
        }
        Ok(())
    }

    pub fn channel_id(&self) -> u32 {
        unsafe { (self.api.channel_id)(self.ptr) }
    }

    /// Bitmask of streams that actually got running.
    pub fn active_streams(&self) -> u32 {
        unsafe { (self.api.active_streams)(self.ptr) }
    }

    /// Channel ids discovered on the device.
    pub fn channels(&self) -> Vec<u32> {
        self.channel_infos().into_iter().map(|c| c.id).collect()
    }

    /// Every channel the SDK discovered, with its `EChannelType`.
    pub fn channel_infos(&self) -> Vec<ChannelInfo> {
        let count = unsafe { (self.api.channel_count)(self.ptr) };
        (0..count)
            .map(|i| ChannelInfo {
                id: unsafe { (self.api.channel_at)(self.ptr, i) },
                channel_type: unsafe { (self.api.channel_type)(self.ptr, i) },
            })
            .collect()
    }

    /// Every sensor the SDK discovered, with model and role.
    pub fn sensors(&self) -> Vec<SensorInfo> {
        let count = unsafe { (self.api.sensor_count)(self.ptr) };
        (0..count)
            .map(|i| SensorInfo {
                id: unsafe { (self.api.sensor_at)(self.ptr, i) },
                model: unsafe { (self.api.sensor_model)(self.ptr, i) },
                role: unsafe { (self.api.sensor_role)(self.ptr, i) },
            })
            .collect()
    }

    pub fn stop(&self) {
        unsafe { (self.api.stop)(self.ptr) };
    }
}

/// One hardware channel (`InuDev::CHwChannel`).
#[derive(Debug, Clone, Copy)]
pub struct ChannelInfo {
    pub id: u32,
    /// `InuDev::EChannelType`
    pub channel_type: i32,
}

/// One hardware sensor (`InuDev::CSensorParams`).
#[derive(Debug, Clone, Copy)]
pub struct SensorInfo {
    pub id: u32,
    /// `InuDev::ESensorModel`
    pub model: i32,
    /// `InuDev::ESensorRole`
    pub role: i32,
}

impl ChannelInfo {
    pub fn type_name(&self) -> &'static str {
        channel_type_name(self.channel_type)
    }
}

impl SensorInfo {
    pub fn model_name(&self) -> &'static str {
        sensor_model_name(self.model)
    }

    pub fn role_name(&self) -> &'static str {
        match self.role {
            0 => "left",
            1 => "right",
            2 => "mono/color",
            _ => "unknown",
        }
    }
}

/// `InuDev::EChannelType` -> name. See `HwInformation.h`.
pub fn channel_type_name(channel_type: i32) -> &'static str {
    match channel_type {
        0 => "unknown",
        1 => "general-camera (RGB)",
        2 => "tracking",
        3 => "stereo (IR)",
        4 => "depth",
        5 => "features-tracking",
        6 => "disparity",
        _ => "unknown",
    }
}

/// `InuDev::ESensorModel` -> name, only the models this SDK mentions.
pub fn sensor_model_name(model: i32) -> &'static str {
    match model {
        130 => "AR_130",
        134 => "AR_134",
        135 => "AR_135",
        136 => "AR_135X",
        430 => "AR_430",
        234 => "AR_0234",
        1040 => "APTINA_1040",
        7251 => "OV_7251",
        2685 => "OV_2685",
        2145 => "GC_2145",
        9160 => "XC_9160",
        9282 => "OV_9282",
        5675 => "OV_5675",
        8856 => "OV_8856",
        4689 => "OV_4689",
        132 => "CGS_132 (RGB)",
        31 => "CGS_031",
        9782 => "OV_9782",
        0 => "none",
        _ => "unknown",
    }
}

impl Drop for InuCamera {
    fn drop(&mut self) {
        unsafe { (self.api.close)(self.ptr) };
    }
}

fn last_error(api: &Shim) -> String {
    unsafe {
        let ptr = (api.last_error)();
        if ptr.is_null() {
            return "unknown error".to_owned();
        }
        CStr::from_ptr(ptr).to_string_lossy().into_owned()
    }
}
