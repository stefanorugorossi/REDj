//! Real-time output through cpal.
//!
//! Master and headphones go to any channel pairs of one output device
//! (e.g. channels 1-2 and 3-4 of a controller's 4-channel interface).

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample};

use crate::live::LiveEngine;

/// Maximum frames processed per engine call inside the callback.
const MAX_BLOCK: usize = 512;

/// An output device.
#[derive(Debug, Clone, PartialEq)]
pub struct DeviceInfo {
    pub id: String,
    pub name: String,
    pub channels: u16,
    pub sample_rate: u32,
    pub is_default: bool,
}

/// Where master and headphones go (0-based channel indices).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Routing {
    pub master: (u16, u16),
    pub phones: Option<(u16, u16)>,
}

impl Default for Routing {
    fn default() -> Self {
        Self {
            master: (0, 1),
            phones: None,
        }
    }
}

#[derive(Debug)]
pub enum DriverError {
    NoDevice(String),
    Backend(String),
}

impl std::fmt::Display for DriverError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DriverError::NoDevice(d) => write!(f, "audio device not found: {d}"),
            DriverError::Backend(e) => write!(f, "audio backend error: {e}"),
        }
    }
}

impl std::error::Error for DriverError {}

fn backend(e: impl std::fmt::Display) -> DriverError {
    DriverError::Backend(e.to_string())
}

/// Lists output devices of the default host.
pub fn output_devices() -> Vec<DeviceInfo> {
    let host = cpal::default_host();
    let default_id = host
        .default_output_device()
        .and_then(|d| d.id().ok())
        .map(|i| i.to_string());
    let Ok(devices) = host.output_devices() else {
        return Vec::new();
    };
    devices
        .filter_map(|d| {
            let id = d.id().ok()?.to_string();
            let cfg = d.default_output_config().ok()?;
            let name = d
                .description()
                .map(|x| x.to_string())
                .unwrap_or_else(|_| id.clone());
            Some(DeviceInfo {
                is_default: default_id.as_deref() == Some(id.as_str()),
                id,
                name,
                channels: cfg.channels(),
                sample_rate: cfg.sample_rate(),
            })
        })
        .collect()
}

/// A running output stream. Dropping it stops audio.
pub struct AudioOutput {
    _stream: cpal::Stream,
    pub device: DeviceInfo,
}

impl std::fmt::Debug for AudioOutput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AudioOutput")
            .field("device", &self.device)
            .finish()
    }
}

/// Opens a device (`None` = default) and starts the engine created by
/// `make_engine` for the device's sample rate.
pub fn start(
    device_id: Option<&str>,
    routing: Routing,
    make_engine: impl FnOnce(u32) -> LiveEngine,
) -> Result<AudioOutput, DriverError> {
    let host = cpal::default_host();
    let device = match device_id {
        Some(id) => host
            .output_devices()
            .map_err(backend)?
            .find(|d| d.id().map(|i| i.to_string()).as_deref() == Ok(id))
            .ok_or_else(|| DriverError::NoDevice(id.to_owned()))?,
        None => host
            .default_output_device()
            .ok_or_else(|| DriverError::NoDevice("default".into()))?,
    };
    let supported = device.default_output_config().map_err(backend)?;
    let channels = supported.channels();
    let sample_rate = supported.sample_rate();
    let info = DeviceInfo {
        id: device.id().map(|i| i.to_string()).unwrap_or_default(),
        name: device
            .description()
            .map(|d| d.to_string())
            .unwrap_or_default(),
        channels,
        sample_rate,
        is_default: device_id.is_none(),
    };
    let engine = make_engine(sample_rate);
    let format = supported.sample_format();
    let config: cpal::StreamConfig = supported.into();
    let stream = match format {
        SampleFormat::F32 => build::<f32>(&device, &config, engine, routing, channels),
        SampleFormat::I16 => build::<i16>(&device, &config, engine, routing, channels),
        SampleFormat::I32 => build::<i32>(&device, &config, engine, routing, channels),
        SampleFormat::U16 => build::<u16>(&device, &config, engine, routing, channels),
        f => {
            return Err(DriverError::Backend(format!(
                "unsupported sample format {f}"
            )));
        }
    }?;
    stream.play().map_err(backend)?;
    Ok(AudioOutput {
        _stream: stream,
        device: info,
    })
}

fn build<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    mut engine: LiveEngine,
    routing: Routing,
    channels: u16,
) -> Result<cpal::Stream, DriverError>
where
    T: SizedSample + FromSample<f32>,
{
    let ch = usize::from(channels);
    let valid = move |c: u16| usize::from(c) < ch;
    let master = (routing.master.0, routing.master.1);
    let phones = routing.phones.filter(|p| valid(p.0) && valid(p.1));
    let mut mbuf = vec![0f32; MAX_BLOCK * 2];
    let mut pbuf = vec![0f32; MAX_BLOCK * 2];
    device
        .build_output_stream(
            *config,
            move |data: &mut [T], _| {
                let frames = data.len() / ch;
                let mut done = 0;
                while done < frames {
                    let n = (frames - done).min(MAX_BLOCK);
                    engine.process(&mut mbuf[..n * 2], &mut pbuf[..n * 2]);
                    for k in 0..n {
                        let out = &mut data[(done + k) * ch..(done + k + 1) * ch];
                        for s in out.iter_mut() {
                            *s = T::from_sample(0.0);
                        }
                        if valid(master.0) {
                            out[usize::from(master.0)] = T::from_sample(mbuf[2 * k]);
                        }
                        if valid(master.1) {
                            out[usize::from(master.1)] = T::from_sample(mbuf[2 * k + 1]);
                        }
                        if let Some((a, b)) = phones {
                            out[usize::from(a)] = T::from_sample(pbuf[2 * k]);
                            out[usize::from(b)] = T::from_sample(pbuf[2 * k + 1]);
                        }
                    }
                    done += n;
                }
            },
            |e| eprintln!("audio stream error: {e}"),
            None,
        )
        .map_err(backend)
}
