//! Playing sound through this machine's audio output.
//!
//! [`Output`] plays sound that arrives in pieces (such as streamed speech):
//! each piece is resampled to the output device's rate (with `phone-core`'s
//! streaming, band-limited resampler, so piece boundaries leave no trace)
//! and queued; playback starts once a little is buffered, and
//! [`Output::finish`] returns once everything has played. [`play`] plays
//! one clip that way. Every channel gets the same sound.
//! [`output_devices`] lists the devices; a
//! device is chosen by its id, or by a name (or part of one) that only it
//! has. Audio systems often give several devices the same name (ALSA lists
//! each way of opening a card), so ids are what tell them apart.

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample};
use phone_core::{Pcm, Resampler};

/// How long to wait for the audio system to open a stream.
const OPEN_TIMEOUT: Duration = Duration::from_secs(5);
/// Extra time allowed past a clip's length before giving up on the device.
const STALL_ALLOWANCE: Duration = Duration::from_secs(5);
/// Time left for the device's own buffer to drain after the last sample has
/// been handed over.
const DRAIN: Duration = Duration::from_millis(250);
/// Sound buffered before playback starts, in seconds, so pieces arriving a
/// little unevenly do not leave gaps.
pub const PREROLL_SECONDS: f64 = 0.25;

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
    /// The device's rate, which the sound was resampled to.
    pub sample_rate: u32,
    pub seconds: f64,
    /// Silence played because the next piece had not arrived in time.
    pub gap_seconds: f64,
}

/// Plays `pcm` on the default output, or on the device `device` picks (an
/// id, or a name or part of one that only it has), and returns once it has
/// played.
pub fn play(pcm: &Pcm, device: Option<&str>) -> Result<Played, SpeakerError> {
    if pcm.samples.is_empty() {
        return Err(SpeakerError::Empty);
    }
    let mut output = Output::open(device)?;
    output.push(pcm)?;
    output.finish()
}

/// An open output device being fed sound in pieces. Dropping it stops the
/// sound at once.
pub struct Output {
    device: String,
    sample_rate: u32,
    stream: cpal::Stream,
    /// Pieces for the audio callback; dropped by [`Output::finish`] to mark
    /// the end.
    pieces: Option<Sender<Vec<f32>>>,
    /// The resampler for the current input rate.
    resampler: Option<(u32, Resampler)>,
    /// Frames handed to the callback.
    queued: u64,
    started: bool,
    /// The callback's report: the end reached, or a device error.
    done: Receiver<Result<(), String>>,
    gaps: Arc<AtomicU64>,
}

impl Output {
    /// Opens the default output, or the device `device` picks, at its own
    /// rate. Nothing plays until enough is pushed.
    pub fn open(device: Option<&str>) -> Result<Output, SpeakerError> {
        let device = find_device(device)?;
        let name = device_name(&device);
        let config = device.default_output_config().map_err(|error| {
            SpeakerError::Device(format!("{name} has no usable output format: {error}"))
        })?;
        let (pieces, feed_pieces) = mpsc::channel();
        let (done_tx, done) = mpsc::channel();
        let gaps = Arc::new(AtomicU64::new(0));
        let feed = Feed {
            pieces: feed_pieces,
            current: Vec::new(),
            position: 0,
            gaps: Arc::clone(&gaps),
            done: Some(done_tx.clone()),
        };
        let stream = open_stream(&device, &config, feed, done_tx)?;
        Ok(Output {
            device: name,
            sample_rate: config.sample_rate(),
            stream,
            pieces: Some(pieces),
            resampler: None,
            queued: 0,
            started: false,
            done,
            gaps,
        })
    }

    /// The device's rate.
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Queues `pcm` (at any rate; a change of rate starts a new resampler).
    /// Playback starts once [`PREROLL_SECONDS`] are queued.
    pub fn push(&mut self, pcm: &Pcm) -> Result<(), SpeakerError> {
        let mut samples = Vec::new();
        match &mut self.resampler {
            Some((rate, resampler)) if *rate == pcm.sample_rate => {
                resampler.push(&pcm.samples, &mut samples);
            }
            current => {
                if let Some((_, mut old)) = current.take() {
                    old.finish(&mut samples);
                }
                let mut resampler = Resampler::new(pcm.sample_rate, self.sample_rate);
                resampler.push(&pcm.samples, &mut samples);
                *current = Some((pcm.sample_rate, resampler));
            }
        }
        self.send(samples)?;
        if !self.started && self.queued_seconds() >= PREROLL_SECONDS {
            self.start()?;
        }
        Ok(())
    }

    fn send(&mut self, samples: Vec<f32>) -> Result<(), SpeakerError> {
        if samples.is_empty() {
            return Ok(());
        }
        self.queued += samples.len() as u64;
        let pieces = self.pieces.as_ref().expect("open until finish");
        pieces
            .send(samples)
            .map_err(|_| SpeakerError::Device(format!("{} stopped playing", self.device)))
    }

    fn queued_seconds(&self) -> f64 {
        self.queued as f64 / f64::from(self.sample_rate)
    }

    fn start(&mut self) -> Result<(), SpeakerError> {
        self.started = true;
        self.stream.play().map_err(|error| {
            SpeakerError::Device(format!("could not start {}: {error}", self.device))
        })
    }

    /// Marks the end of the sound and returns once it has all played.
    pub fn finish(mut self) -> Result<Played, SpeakerError> {
        let mut tail = Vec::new();
        if let Some((_, mut resampler)) = self.resampler.take() {
            resampler.finish(&mut tail);
        }
        self.send(tail)?;
        if self.queued == 0 {
            return Err(SpeakerError::Empty);
        }
        drop(self.pieces.take());
        if !self.started {
            self.start()?;
        }
        let limit = Duration::from_secs_f64(self.queued_seconds()) + STALL_ALLOWANCE;
        let outcome = self.done.recv_timeout(limit);
        // The last samples are in the device's buffer, not yet heard.
        std::thread::sleep(DRAIN);
        let gap_seconds = self.gaps.load(Ordering::Relaxed) as f64 / f64::from(self.sample_rate);
        match outcome {
            Ok(Ok(())) => Ok(Played {
                device: self.device.clone(),
                sample_rate: self.sample_rate,
                seconds: self.queued_seconds(),
                gap_seconds,
            }),
            Ok(Err(message)) => Err(SpeakerError::Device(format!(
                "{} stopped playing: {message}",
                self.device
            ))),
            Err(_) => Err(SpeakerError::Device(format!(
                "{} did not play the sound within {:.1} s",
                self.device,
                limit.as_secs_f64()
            ))),
        }
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

/// The audio callback's side: pieces as they arrive, played in order.
struct Feed {
    pieces: Receiver<Vec<f32>>,
    current: Vec<f32>,
    position: usize,
    /// Frames of silence played while waiting for a piece.
    gaps: Arc<AtomicU64>,
    /// Told once, when the last piece has played.
    done: Option<Sender<Result<(), String>>>,
}

impl Feed {
    /// The next sample: `None` once the sound has ended, silence while the
    /// next piece has not arrived.
    fn next(&mut self) -> Option<f32> {
        loop {
            if let Some(sample) = self.current.get(self.position) {
                self.position += 1;
                return Some(*sample);
            }
            match self.pieces.try_recv() {
                Ok(piece) => {
                    self.current = piece;
                    self.position = 0;
                }
                Err(TryRecvError::Empty) => {
                    self.gaps.fetch_add(1, Ordering::Relaxed);
                    return Some(0.0);
                }
                Err(TryRecvError::Disconnected) => return None,
            }
        }
    }

    /// Fills `out` (interleaved, `channels` per frame), the same sample on
    /// every channel, and silence after the end.
    fn fill<T>(&mut self, out: &mut [T], channels: usize)
    where
        T: SizedSample + FromSample<f32>,
    {
        let mut ended = false;
        for frame in out.chunks_mut(channels.max(1)) {
            let sample = if ended { None } else { self.next() };
            ended = sample.is_none();
            frame.fill(T::from_sample(sample.unwrap_or(0.0)));
        }
        if ended && let Some(done) = self.done.take() {
            let _ = done.send(Ok(()));
        }
    }
}

fn open_stream(
    device: &cpal::Device,
    config: &cpal::SupportedStreamConfig,
    feed: Feed,
    errors: Sender<Result<(), String>>,
) -> Result<cpal::Stream, SpeakerError> {
    let format = config.sample_format();
    let channels = usize::from(config.channels());
    let config = config.config();
    macro_rules! open {
        ($sample:ty) => {
            build::<$sample>(device, config, channels, feed, errors)
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

fn build<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    channels: usize,
    mut feed: Feed,
    errors: Sender<Result<(), String>>,
) -> Result<cpal::Stream, SpeakerError>
where
    T: SizedSample + FromSample<f32>,
{
    device
        .build_output_stream::<T, _, _>(
            config,
            move |out: &mut [T], _info| feed.fill(out, channels),
            move |error| {
                let _ = errors.send(Err(error.to_string()));
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
    fn feeds_pieces_in_order_with_gaps_then_ends() {
        let (send, pieces) = mpsc::channel();
        let (done_tx, done) = mpsc::channel();
        let mut feed = Feed {
            pieces,
            current: Vec::new(),
            position: 0,
            gaps: Arc::new(AtomicU64::new(0)),
            done: Some(done_tx),
        };
        send.send(vec![0.5_f32, -0.5]).unwrap();
        send.send(vec![0.25]).unwrap();
        let mut out = [0.0_f32; 6];
        feed.fill(&mut out, 2);
        // Two channels: the three samples arrive across pieces.
        assert_eq!(out, [0.5, 0.5, -0.5, -0.5, 0.25, 0.25]);
        // The next piece is late: silence, counted as a gap.
        let mut out = [1.0_f32; 2];
        feed.fill(&mut out, 1);
        assert_eq!(out, [0.0, 0.0]);
        assert_eq!(feed.gaps.load(Ordering::Relaxed), 2);
        assert!(done.try_recv().is_err());
        // It arrives, then the sound ends: silence, and done once.
        send.send(vec![0.75]).unwrap();
        drop(send);
        let mut ints = [1_i16; 3];
        feed.fill(&mut ints, 1);
        assert_eq!(ints, [<i16 as FromSample<f32>>::from_sample_(0.75), 0, 0]);
        assert_eq!(done.try_recv(), Ok(Ok(())));
        feed.fill(&mut ints, 1);
        assert!(done.try_recv().is_err());
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
