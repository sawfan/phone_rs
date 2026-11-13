//! Tones: notes as frequency and time, arranged and rendered to samples.
//!
//! The boundary with music theory is frequency and time: callers turn
//! pitches into hertz (Mnemosyne uses `euterpe-core`) and hand this module
//! [`Tone`]s. [`render`] plays them on a [`Sound`] with band-limited
//! oscillators and a low-pass filter from FunDSP, shaped by the sound's
//! envelope.

use std::fmt;

use fundsp::prelude32::{AudioUnit, lowpass_hz, saw_hz, sine_hz, square_hz, triangle_hz};

use crate::instrument::{Sound, Waveform};
use crate::pcm::Pcm;

/// One note to sound: `hz` from `start` seconds, for `duration` seconds
/// before its release.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tone {
    pub hz: f64,
    pub start: f64,
    pub duration: f64,
}

/// Sequences leave this fraction of each step silent, so repeated
/// pitches are heard as separate notes.
const SEQUENCE_GAP_FRACTION: f64 = 0.1;
/// Limits that keep a request from scheduling an unreasonable amount of
/// sound; [`validate`] rejects tones outside them.
pub const MAX_TONES: usize = 256;
pub const MAX_TONE_SECONDS: f64 = 30.0;
/// Frequencies accepted for playback, in hertz.
pub const MIN_HZ: f64 = 8.0;
pub const MAX_HZ: f64 = 20_000.0;
/// Sample rates [`render`] accepts, in hertz.
pub const MIN_SAMPLE_RATE: u32 = 8_000;
pub const MAX_SAMPLE_RATE: u32 = 192_000;
/// The longest sound [`render`] makes, release tails included, in seconds.
pub const MAX_RENDER_SECONDS: f64 = 300.0;
/// Peak level of one tone before the volume. Simultaneous tones share it
/// (divided by the square root of how many sound at once).
pub const VOICE_LEVEL: f64 = 0.25;
/// The low-pass filter's resonance: Web Audio's `Q` of 0.7 dB as a linear
/// Q, so rendered and browser-played sounds match.
const FILTER_Q: f32 = 1.084;
/// The filter's cutoff stays below this fraction of the sample rate.
const MAX_CUTOFF_FRACTION: f64 = 0.45;
/// Rendered sound louder than this is scaled down to it.
const LIMIT_PEAK: f32 = 0.98;

/// Why tones could not be played or rendered.
#[derive(Clone, Debug, PartialEq)]
pub enum ToneError {
    Empty,
    TooMany,
    OutOfRange { hz: f64 },
    InvalidTiming,
    AboveNyquist { hz: f64, sample_rate: u32 },
    TooLong { seconds: f64 },
    InvalidSampleRate { sample_rate: u32 },
    InvalidSound(String),
    InvalidVolume { volume: f64 },
}

impl fmt::Display for ToneError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(f, "Nothing to play."),
            Self::TooMany => write!(f, "Too many notes to play at once (limit {MAX_TONES})."),
            Self::OutOfRange { hz } => write!(
                f,
                "{hz:.2} Hz is outside the playable range ({MIN_HZ}–{MAX_HZ} Hz)."
            ),
            Self::InvalidTiming => write!(f, "A note has an invalid start or length."),
            Self::AboveNyquist { hz, sample_rate } => write!(
                f,
                "{hz:.2} Hz is too high for a sample rate of {sample_rate} Hz (the limit is half the rate)."
            ),
            Self::TooLong { seconds } => write!(
                f,
                "The sound would last {seconds:.1} s; at most {MAX_RENDER_SECONDS} s can be rendered."
            ),
            Self::InvalidSampleRate { sample_rate } => write!(
                f,
                "A sample rate of {sample_rate} Hz is outside {MIN_SAMPLE_RATE}–{MAX_SAMPLE_RATE} Hz."
            ),
            Self::InvalidSound(message) => write!(f, "{message}"),
            Self::InvalidVolume { volume } => {
                write!(f, "volume must be from 0 to 1, not {volume}.")
            }
        }
    }
}

impl std::error::Error for ToneError {}

impl From<ToneError> for String {
    fn from(error: ToneError) -> Self {
        error.to_string()
    }
}

/// Tones one after another, each `step` seconds apart.
pub fn sequence(hz: &[f64], step: f64) -> Vec<Tone> {
    hz.iter()
        .enumerate()
        .map(|(index, &hz)| Tone {
            hz,
            start: index as f64 * step,
            duration: step * (1.0 - SEQUENCE_GAP_FRACTION),
        })
        .collect()
}

/// Tones sounding together for `duration` seconds.
pub fn together(hz: &[f64], duration: f64) -> Vec<Tone> {
    hz.iter()
        .map(|&hz| Tone {
            hz,
            start: 0.0,
            duration,
        })
        .collect()
}

/// `groups` played one after another, the tones in each group together,
/// each group sounding `duration` seconds with `gap` seconds of silence
/// between groups.
pub fn groups_in_turn(groups: &[Vec<f64>], duration: f64, gap: f64) -> Vec<Tone> {
    groups
        .iter()
        .enumerate()
        .flat_map(|(index, group)| {
            let start = index as f64 * (duration + gap);
            group.iter().map(move |&hz| Tone {
                hz,
                start,
                duration,
            })
        })
        .collect()
}

/// The most tones sounding at once.
pub fn peak_polyphony(tones: &[Tone]) -> usize {
    tones
        .iter()
        .map(|tone| {
            tones
                .iter()
                .filter(|other| {
                    other.start <= tone.start && tone.start < other.start + other.duration
                })
                .count()
        })
        .max()
        .unwrap_or(0)
}

/// Checks tones before anything is scheduled or rendered.
pub fn validate(tones: &[Tone]) -> Result<(), ToneError> {
    if tones.is_empty() {
        return Err(ToneError::Empty);
    }
    if tones.len() > MAX_TONES {
        return Err(ToneError::TooMany);
    }
    for tone in tones {
        if !(MIN_HZ..=MAX_HZ).contains(&tone.hz) {
            return Err(ToneError::OutOfRange { hz: tone.hz });
        }
        let finite = tone.start.is_finite() && tone.duration.is_finite();
        if !finite || tone.start < 0.0 || tone.duration <= 0.0 || tone.duration > MAX_TONE_SECONDS {
            return Err(ToneError::InvalidTiming);
        }
    }
    Ok(())
}

/// An oscillator of `waveform` at `hz` through the low-pass filter (its
/// cutoff kept below [`MAX_CUTOFF_FRACTION`] of the rate), ready to run at
/// `sample_rate`. Used for rendered tones and live voices alike.
pub(crate) fn voice(
    waveform: Waveform,
    hz: f64,
    cutoff_hz: f64,
    sample_rate: f64,
) -> Box<dyn AudioUnit> {
    let (hz, cutoff) = (
        hz as f32,
        cutoff_hz.min(sample_rate * MAX_CUTOFF_FRACTION) as f32,
    );
    let filter = || lowpass_hz(cutoff, FILTER_Q);
    let mut unit: Box<dyn AudioUnit> = match waveform {
        Waveform::Sine => Box::new(sine_hz(hz) >> filter()),
        Waveform::Triangle => Box::new(triangle_hz(hz) >> filter()),
        Waveform::Square => Box::new(square_hz(hz) >> filter()),
        Waveform::Sawtooth => Box::new(saw_hz(hz) >> filter()),
    };
    unit.set_sample_rate(sample_rate);
    unit.reset();
    unit
}

/// Renders `tones` played on `sound` at `volume` (0 to 1) as mono samples
/// at `sample_rate`, up to the end of the last release.
///
/// Levels follow the browser music blocks: each tone peaks at
/// [`VOICE_LEVEL`] × `volume`, shared between tones sounding together.
/// Where tones still add up past full scale, the whole sound is scaled
/// down to a peak of 0.98 (the browser uses a compressor instead), so
/// rendered sound never clips. The output is deterministic.
pub fn render(
    tones: &[Tone],
    sound: &Sound,
    volume: f64,
    sample_rate: u32,
) -> Result<Pcm, ToneError> {
    validate(tones)?;
    sound.validate().map_err(ToneError::InvalidSound)?;
    if !(0.0..=1.0).contains(&volume) {
        return Err(ToneError::InvalidVolume { volume });
    }
    if !(MIN_SAMPLE_RATE..=MAX_SAMPLE_RATE).contains(&sample_rate) {
        return Err(ToneError::InvalidSampleRate { sample_rate });
    }
    let rate = f64::from(sample_rate);
    if let Some(tone) = tones.iter().find(|tone| tone.hz >= rate / 2.0) {
        return Err(ToneError::AboveNyquist {
            hz: tone.hz,
            sample_rate,
        });
    }
    let sound = sound.clamped();
    let release = sound.envelope.release;
    let seconds = tones
        .iter()
        .map(|tone| tone.start + tone.duration + release)
        .fold(0.0, f64::max);
    if seconds > MAX_RENDER_SECONDS {
        return Err(ToneError::TooLong { seconds });
    }

    let mut samples = vec![0.0_f32; (seconds * rate).ceil() as usize];
    let level = VOICE_LEVEL * volume / (peak_polyphony(tones).max(1) as f64).sqrt();
    for tone in tones {
        let mut unit = voice(sound.waveform, tone.hz, sound.cutoff_hz, rate);
        let first = (tone.start * rate).round() as usize;
        let length = ((tone.duration + release) * rate).ceil() as usize;
        let end = (first + length).min(samples.len());
        for (index, sample) in samples[first..end].iter_mut().enumerate() {
            let gain = sound
                .envelope
                .released_level_at(index as f64 / rate, tone.duration);
            *sample += unit.get_mono() * (gain * level) as f32;
        }
    }
    let peak = samples.iter().fold(0.0_f32, |peak, s| peak.max(s.abs()));
    if peak > LIMIT_PEAK {
        let scale = LIMIT_PEAK / peak;
        samples.iter_mut().for_each(|s| *s *= scale);
    }
    Ok(Pcm {
        sample_rate,
        samples,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::instrument::Preset;

    fn tone(hz: f64, start: f64, duration: f64) -> Tone {
        Tone {
            hz,
            start,
            duration,
        }
    }

    /// Power near `frequency` (Goertzel).
    fn power_at(samples: &[f32], frequency: f64, rate: f64) -> f64 {
        let w = 2.0 * std::f64::consts::PI * frequency / rate;
        let (mut s1, mut s2) = (0.0, 0.0);
        for x in samples {
            let s0 = f64::from(*x) + 2.0 * w.cos() * s1 - s2;
            s2 = s1;
            s1 = s0;
        }
        s1 * s1 + s2 * s2 - 2.0 * w.cos() * s1 * s2
    }

    fn peak(samples: &[f32]) -> f32 {
        samples.iter().fold(0.0, |peak, s| peak.max(s.abs()))
    }

    #[test]
    fn sequences_and_chords() {
        let notes = sequence(&[440.0, 494.0, 523.0], 0.5);
        assert_eq!(notes[2].start, 1.0);
        assert!((notes[0].duration - 0.45).abs() < 1e-12);
        assert_eq!(peak_polyphony(&notes), 1);

        let chord = together(&[261.6, 329.6, 392.0], 1.0);
        assert!(chord.iter().all(|tone| tone.start == 0.0));
        assert_eq!(peak_polyphony(&chord), 3);

        let turns = groups_in_turn(&[vec![1.0, 2.0], vec![3.0, 4.0]], 1.5, 0.25);
        assert_eq!(turns.len(), 4);
        assert_eq!(turns[2].start, 1.75);
        assert_eq!(peak_polyphony(&turns), 2);
        assert_eq!(peak_polyphony(&[]), 0);
    }

    #[test]
    fn validates_tones() {
        assert!(validate(&[tone(440.0, 0.0, 0.5)]).is_ok());
        assert_eq!(validate(&[]), Err(ToneError::Empty));
        assert!(validate(&[tone(4.0, 0.0, 0.5)]).is_err());
        assert!(validate(&[tone(30_000.0, 0.0, 0.5)]).is_err());
        assert!(validate(&[tone(440.0, -1.0, 0.5)]).is_err());
        assert!(validate(&[tone(440.0, 0.0, 0.0)]).is_err());
        assert!(validate(&[tone(440.0, 0.0, f64::NAN)]).is_err());
        assert!(validate(&[tone(440.0, 0.0, 60.0)]).is_err());
        assert_eq!(
            validate(&vec![tone(440.0, 0.0, 0.5); MAX_TONES + 1]),
            Err(ToneError::TooMany)
        );
        // The messages the browser music blocks show.
        assert_eq!(
            ToneError::OutOfRange { hz: 4.0 }.to_string(),
            "4.00 Hz is outside the playable range (8–20000 Hz)."
        );
    }

    #[test]
    fn renders_the_pitch_for_the_duration_and_release() {
        let sound = Preset::Pure.sound();
        let pcm = render(&[tone(440.0, 0.0, 0.5)], &sound, 1.0, 48_000).unwrap();
        // Half a second plus the 0.08 s release.
        assert_eq!(pcm.samples.len(), (0.58_f64 * 48_000.0).ceil() as usize);
        let steady = &pcm.samples[4_800..19_200];
        let rate = 48_000.0;
        assert!(power_at(steady, 440.0, rate) > 1_000.0 * power_at(steady, 660.0, rate));
        // The pure preset sustains at full level: the peak is VOICE_LEVEL.
        assert!((f64::from(peak(steady)) - VOICE_LEVEL).abs() < 0.01);
        // Silent at the start and after the release.
        assert_eq!(pcm.samples[0], 0.0);
        assert!(pcm.samples.last().unwrap().abs() < 1e-3);
        assert!(pcm.samples.iter().all(|s| s.is_finite()));
    }

    #[test]
    fn rendering_is_deterministic_and_starts_tones_on_time() {
        let sound = Preset::Pluck.sound();
        let tones = [tone(220.0, 0.0, 0.2), tone(330.0, 0.6, 0.2)];
        let a = render(&tones, &sound, 0.8, 22_050).unwrap();
        assert_eq!(a, render(&tones, &sound, 0.8, 22_050).unwrap());
        // The first note's release ends at 0.45 s; the second starts at
        // 0.6 s: silence between, sound just after.
        let at = |seconds: f64| (seconds * 22_050.0) as usize;
        let before = peak(&a.samples[at(0.46)..at(0.6)]);
        let after = peak(&a.samples[at(0.61)..at(0.65)]);
        assert!(before < 1e-3 && after > 0.05, "{before} {after}");
    }

    #[test]
    fn the_filter_darkens_the_sound() {
        let open = Sound {
            cutoff_hz: 20_000.0,
            ..Preset::Bright.sound()
        };
        let dark = Sound {
            cutoff_hz: 400.0,
            ..open
        };
        let tones = [tone(220.0, 0.0, 0.5)];
        let rate = 48_000.0;
        let ratio = |sound: &Sound| {
            let pcm = render(&tones, sound, 1.0, 48_000).unwrap();
            let steady = &pcm.samples[4_800..19_200];
            power_at(steady, 1_760.0, rate) / power_at(steady, 220.0, rate)
        };
        assert!(ratio(&dark) < ratio(&open) / 100.0);
    }

    #[test]
    fn chords_share_the_level_and_never_clip() {
        let sound = Preset::Organ.sound();
        let chord = together(&[261.6, 329.6, 392.0], 0.5);
        let pcm = render(&chord, &sound, 1.0, 44_100).unwrap();
        assert!(peak(&pcm.samples) <= LIMIT_PEAK);
        let crowd = together(&vec![110.0; MAX_TONES], 0.2);
        let pcm = render(&crowd, &sound, 1.0, 44_100).unwrap();
        assert!(peak(&pcm.samples) <= LIMIT_PEAK + 1e-6);
    }

    #[test]
    fn rejects_what_it_cannot_render() {
        let sound = Preset::Soft.sound();
        let one = [tone(440.0, 0.0, 0.5)];
        assert_eq!(
            render(&one, &sound, 1.0, 1_000),
            Err(ToneError::InvalidSampleRate { sample_rate: 1_000 })
        );
        assert!(matches!(
            render(&[tone(5_000.0, 0.0, 0.5)], &sound, 1.0, 8_000),
            Err(ToneError::AboveNyquist { .. })
        ));
        assert!(matches!(
            render(&[tone(440.0, 400.0, 1.0)], &sound, 1.0, 8_000),
            Err(ToneError::TooLong { .. })
        ));
        assert!(matches!(
            render(&one, &sound, 1.5, 8_000),
            Err(ToneError::InvalidVolume { .. })
        ));
        let bad = Sound {
            cutoff_hz: 10.0,
            ..sound
        };
        assert!(matches!(
            render(&one, &bad, 1.0, 8_000),
            Err(ToneError::InvalidSound(_))
        ));
    }
}
