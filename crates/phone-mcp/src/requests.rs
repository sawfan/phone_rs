//! Request types shared by the standalone server and hosts.

use phone_core::{Sound, SoundChanges, Tone, dto};
use schemars::JsonSchema;
use serde::Deserialize;

/// One note: a frequency from a start time, for a duration before its
/// release.
#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
pub struct ToneRequest {
    /// Frequency in hertz (8 to 20000, and below half the sample rate).
    pub hz: f64,
    /// Seconds from the beginning of the sound (default 0).
    #[serde(default)]
    pub start: f64,
    /// Seconds the note is held before its release (up to 30).
    pub duration: f64,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct RenderTonesRequest {
    /// The notes, at most 256. Notes may overlap (chords).
    pub tones: Vec<ToneRequest>,
    /// Path of the WAV file to create on the machine running this server.
    /// It must end in .wav and must not exist yet.
    pub out: String,
    /// The starting sound: pure, soft (default), organ, pluck, pad, or
    /// bright (see the presets tool).
    #[serde(default)]
    pub preset: Option<String>,
    /// Replaces the preset's oscillator: sine, triangle, square, or
    /// sawtooth.
    #[serde(default)]
    pub waveform: Option<String>,
    /// Envelope changes, in seconds (0 to 4); sustain is the held level, 0
    /// to 1.
    #[serde(default)]
    pub attack: Option<f64>,
    #[serde(default)]
    pub decay: Option<f64>,
    #[serde(default)]
    pub sustain: Option<f64>,
    #[serde(default)]
    pub release: Option<f64>,
    /// Low-pass cutoff in hertz (100 to 20000; 20000 is open).
    #[serde(default)]
    pub cutoff_hz: Option<f64>,
    /// Output level, 0 to 1 (default 0.8).
    #[serde(default)]
    pub volume: Option<f64>,
    /// Samples per second, 8000 to 192000 (default 48000).
    #[serde(default)]
    pub sample_rate: Option<u32>,
}

/// Defaults for [`RenderTonesRequest`].
pub const DEFAULT_VOLUME: f64 = 0.8;
pub const DEFAULT_SAMPLE_RATE: u32 = 48_000;

impl RenderTonesRequest {
    pub fn tones(&self) -> Vec<Tone> {
        self.tones
            .iter()
            .map(|tone| Tone {
                hz: tone.hz,
                start: tone.start,
                duration: tone.duration,
            })
            .collect()
    }

    /// The preset (default soft) with the requested changes.
    pub fn sound(&self) -> Result<Sound, String> {
        let preset = match &self.preset {
            Some(id) => dto::preset(id)?,
            None => phone_core::Preset::Soft,
        };
        let changes = SoundChanges {
            waveform: self.waveform.as_deref().map(dto::waveform).transpose()?,
            attack: self.attack,
            decay: self.decay,
            sustain: self.sustain,
            release: self.release,
            cutoff_hz: self.cutoff_hz,
        };
        Sound::customized(preset, &changes)
    }
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct SayRequest {
    /// What to say (at most 4,096 characters).
    pub text: String,
    /// Path of the WAV file to create on the machine running this server.
    /// It must end in .wav and must not exist yet.
    pub out: String,
    /// A voice the speech backend knows; its configured voice when absent.
    #[serde(default)]
    pub voice: Option<String>,
    /// Speaking speed, 0.25 to 4 (1 is normal).
    #[serde(default)]
    pub speed: Option<f64>,
}

impl SayRequest {
    pub fn options(&self) -> phone_tts::SpeakOptions {
        phone_tts::SpeakOptions {
            voice: self.voice.clone(),
            speed: self.speed,
        }
    }
}
