//! Playing sound through this machine's audio output.
//!
//! [`play`] resamples a [`Pcm`] clip to the output device's rate (with
//! `phone-core`'s band-limited resampler), sends it to every channel, and
//! returns once it has played. [`output_devices`] lists the devices; a
//! device is chosen by its id, or by a name (or part of one) that only it
//! has. Audio systems often give several devices the same name (ALSA lists
//! each way of opening a card), so ids are what tell them apart.

use std::fmt;
use std::sync::mpsc;
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample};
use phone_core::Pcm;

/// How long to wait for the audio system to open a stream.
const OPEN_TIMEOUT: Duration = Duration::from_secs(5);
/// Extra time allowed past a clip's length before giving up on the device.
const STALL_ALLOWANCE: Duration = Duration::from_secs(5);
/// Time left for the device's own buffer to drain after the last sample has
/// been handed over.
const DRAIN: Duration = Duration::from_millis(250);

/// Why sound could not be played.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpeakerError {
    /// No device matches, or there is no default output.
    NoDevice(String),
    /// The audio system refused or failed.
    Device(String),
    /// The clip has no samples.
    Empty,
}

impl fmt::Display for SpeakerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoDevice(message) | Self::Device(message) => write!(f, "{message}"),
            Self::Empty => write!(f, "nothing to play"),
        }
    }
}

impl std::error::Error for SpeakerError {}

impl From<SpeakerError> for String {
    fn from(error: SpeakerError) -> Self {
        error.to_string()
    }
}

/// An output device, as the audio system names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputDevice {
    /// The audio system's id for the device (for ALSA, its PCM name such as
    /// `hw:CARD=Generic,DEV=0`), unique among the devices.
    pub id: String,
    pub name: String,
    pub is_default: bool,
}

/// The output devices of the default audio host.
pub fn output_devices() -> Result<Vec<OutputDevice>, SpeakerError> {
    let host = cpal::default_host();
    let default = host
        .default_output_device()
        .map(|device| device_id(&device));
    let devices = host
        .output_devices()
        .map_err(|error| SpeakerError::Device(format!("could not list output devices: {error}")))?;
    Ok(devices
        .map(|device| {
            let id = device_id(&device);
            OutputDevice {
                is_default: default.as_ref() == Some(&id),
                name: device_name(&device),
                id,
            }
        })
        .collect())
}

/// What was played.
#[derive(Debug, Clone, PartialEq)]
pub struct Played {
    pub device: String,
    /// The device's rate, which the clip was resampled to.
    pub sample_rate: u32,
    pub seconds: f64,
}

/// Plays `pcm` on the default output, or on the device named (or uniquely
/// named in part) by `device`, and returns once it has played.
pub fn play(pcm: &Pcm, device: Option<&str>) -> Result<Played, SpeakerError> {
    if pcm.samples.is_empty() {
        return Err(SpeakerError::Empty);
    }
    let device = find_device(device)?;
    let name = device_name(&device);
    let config = device.default_output_config().map_err(|error| {
        SpeakerError::Device(format!("{name} has no usable output format: {error}"))
    })?;
    let clip = pcm.resampled(config.sample_rate());
    let seconds = clip.seconds();
    let (done_tx, done_rx) = mpsc::channel();
    let stream = open_stream(&device, &config, clip.samples, done_tx)?;
    stream
        .play()
        .map_err(|error| SpeakerError::Device(format!("could not start {name}: {error}")))?;
    let limit = Duration::from_secs_f64(seconds) + STALL_ALLOWANCE;
    let outcome = done_rx.recv_timeout(limit);
    // The last samples are in the device's buffer, not yet heard.
    std::thread::sleep(DRAIN);
    drop(stream);
    match outcome {
        Ok(Ok(())) => Ok(Played {
            device: name,
            sample_rate: config.sample_rate(),
            seconds,
        }),
        Ok(Err(message)) => Err(SpeakerError::Device(format!(
            "{name} stopped playing: {message}"
        ))),
        Err(_) => Err(SpeakerError::Device(format!(
            "{name} did not play the sound within {:.1} s",
            limit.as_secs_f64()
        ))),
    }
}

/// The default output, or the device `wanted` picks (see [`pick_device`]).
fn find_device(wanted: Option<&str>) -> Result<cpal::Device, SpeakerError> {
    let host = cpal::default_host();
    let Some(wanted) = wanted else {
        return host.default_output_device().ok_or_else(|| {
            SpeakerError::NoDevice("there is no default output device".to_string())
        });
    };
    let devices: Vec<cpal::Device> = host
        .output_devices()
        .map_err(|error| SpeakerError::Device(format!("could not list output devices: {error}")))?
        .collect();
    let labels: Vec<(String, String)> = devices
        .iter()
        .map(|device| (device_id(device), device_name(device)))
        .collect();
    let index = pick_device(&labels, wanted)?;
    Ok(devices
        .into_iter()
        .nth(index)
        .expect("index from the same list"))
}

/// The index of the device `wanted` picks among `(id, name)` pairs: the one
/// whose id equals it, else the one whose name equals it, else the one
/// whose id or name contains it (all ignoring case). Several matches at a
/// step are an error listing their ids.
fn pick_device(devices: &[(String, String)], wanted: &str) -> Result<usize, SpeakerError> {
    /// Whether an `(id, name)` pair matches at one step.
    type Matches<'a> = &'a dyn Fn(&(String, String)) -> bool;
    let lower = wanted.to_lowercase();
    let steps: [Matches; 3] = [
        &|(id, _)| id.to_lowercase() == lower,
        &|(_, name)| name.to_lowercase() == lower,
        &|(id, name)| id.to_lowercase().contains(&lower) || name.to_lowercase().contains(&lower),
    ];
    for matches in steps {
        let found: Vec<usize> = devices
            .iter()
            .enumerate()
            .filter(|(_, device)| matches(device))
            .map(|(index, _)| index)
            .collect();
        match found.as_slice() {
            [] => continue,
            [index] => return Ok(*index),
            _ => {
                let ids: Vec<&str> = found
                    .iter()
                    .map(|index| devices[*index].0.as_str())
                    .collect();
                return Err(SpeakerError::NoDevice(format!(
                    "{wanted:?} matches several output devices; give one of their ids: {}",
                    ids.join(", ")
                )));
            }
        }
    }
    Err(SpeakerError::NoDevice(format!(
        "no output device matches {wanted:?} (see `phone devices`)"
    )))
}

fn device_id(device: &cpal::Device) -> String {
    device
        .id()
        .map(|id| id.id().to_string())
        .unwrap_or_else(|_| "unknown".to_string())
}

fn device_name(device: &cpal::Device) -> String {
    device
        .description()
        .map(|description| description.name().to_string())
        .unwrap_or_else(|_| "unnamed device".to_string())
}

fn open_stream(
    device: &cpal::Device,
    config: &cpal::SupportedStreamConfig,
    samples: Vec<f32>,
    done: mpsc::Sender<Result<(), String>>,
) -> Result<cpal::Stream, SpeakerError> {
    let format = config.sample_format();
    let channels = usize::from(config.channels());
    let config = config.config();
    macro_rules! open {
        ($sample:ty) => {
            build::<$sample>(device, config, channels, samples, done)
        };
    }
    match format {
        SampleFormat::F32 => open!(f32),
        SampleFormat::F64 => open!(f64),
        SampleFormat::I16 => open!(i16),
        SampleFormat::I32 => open!(i32),
        SampleFormat::I8 => open!(i8),
        SampleFormat::U16 => open!(u16),
        SampleFormat::U8 => open!(u8),
        other => Err(SpeakerError::Device(format!(
            "unsupported sample format {other:?}"
        ))),
    }
}

/// Fills `out` (interleaved, `channels` per frame) from `samples` starting
/// at `*position`, the same sample on every channel, and silence after the
/// end. Returns whether the clip has ended.
fn fill<T>(out: &mut [T], channels: usize, samples: &[f32], position: &mut usize) -> bool
where
    T: SizedSample + FromSample<f32>,
{
    for frame in out.chunks_mut(channels.max(1)) {
        let sample = samples.get(*position).copied().unwrap_or(0.0);
        *position = (*position + 1).min(samples.len());
        frame.fill(T::from_sample(sample));
    }
    *position >= samples.len()
}

fn build<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    channels: usize,
    samples: Vec<f32>,
    done: mpsc::Sender<Result<(), String>>,
) -> Result<cpal::Stream, SpeakerError>
where
    T: SizedSample + FromSample<f32>,
{
    let mut position = 0;
    let mut finished = false;
    let error_done = done.clone();
    device
        .build_output_stream::<T, _, _>(
            config,
            move |out: &mut [T], _info| {
                if fill(out, channels, &samples, &mut position) && !finished {
                    finished = true;
                    let _ = done.send(Ok(()));
                }
            },
            move |error| {
                let _ = error_done.send(Err(error.to_string()));
            },
            Some(OPEN_TIMEOUT),
        )
        .map_err(|error| SpeakerError::Device(format!("could not open the output stream: {error}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_devices_by_id_or_unique_name() {
        let devices: Vec<(String, String)> = [
            ("default", "Default Audio Device"),
            ("hw:CARD=Generic,DEV=0", "HD-Audio Generic, ALC1220 Analog"),
            (
                "plughw:CARD=Generic,DEV=0",
                "HD-Audio Generic, ALC1220 Analog",
            ),
            ("hdmi:CARD=HDMI,DEV=0", "HDMI Output"),
        ]
        .iter()
        .map(|(id, name)| (id.to_string(), name.to_string()))
        .collect();
        assert_eq!(pick_device(&devices, "hw:CARD=Generic,DEV=0"), Ok(1));
        assert_eq!(pick_device(&devices, "hdmi output"), Ok(3));
        assert_eq!(pick_device(&devices, "plughw"), Ok(2));
        // A name two devices share is ambiguous; the error gives their ids.
        let shared = pick_device(&devices, "HD-Audio Generic, ALC1220 Analog");
        assert!(matches!(
            shared,
            Err(SpeakerError::NoDevice(message))
                if message.contains("several")
                    && message.starts_with("\"HD-Audio Generic")
                    && message.contains("plughw:CARD=Generic,DEV=0")
        ));
        assert!(pick_device(&devices, "usb").is_err());
    }

    #[test]
    fn fills_every_channel_then_silence() {
        let samples = [0.5_f32, -0.5, 0.25];
        let mut position = 0;
        let mut out = [0.0_f32; 4];
        assert!(!fill(&mut out, 2, &samples, &mut position));
        assert_eq!(out, [0.5, 0.5, -0.5, -0.5]);
        assert!(fill(&mut out, 2, &samples, &mut position));
        assert_eq!(out, [0.25, 0.25, 0.0, 0.0]);
        let mut ints = [0_i16; 2];
        assert!(fill(&mut ints, 1, &samples, &mut position));
        assert_eq!(ints, [0, 0]);
    }

    #[test]
    fn empty_clips_are_refused_before_touching_a_device() {
        let empty = Pcm {
            sample_rate: 48_000,
            samples: Vec::new(),
        };
        assert_eq!(play(&empty, None), Err(SpeakerError::Empty));
    }
}
