//! Live synthesis: notes started and released while sound is playing.
//!
//! [`Synth`] renders blocks of samples on demand, for an audio callback (a
//! browser AudioWorklet through `phone-worklet`, or a native output
//! stream). It is driven by [`Command`]s, which have a small byte encoding
//! ([`Command::encode`], [`Command::decode`]) so the side that decides what
//! to play and the audio thread can pass them as plain bytes while both use
//! this one definition.
//!
//! Like MIDI channels, every note belongs to a bus (`0..MAX_BUSES`); the
//! synthesizer mixes each bus separately, with its own volume, and renders
//! each to its own output. Notes may start now or at a time on the audio
//! clock, and may be held until released or for a set time, so a melody
//! can be handed over in one go and play sample-accurately.
//!
//! Voices are the same oscillators and filters as rendered tones
//! ([`crate::tone::render`]), shaped by the same envelope, so a preset
//! sounds the same live and rendered.

use std::fmt;

use fundsp::prelude32::AudioUnit;

use crate::instrument::{Envelope, Sound, Waveform};
use crate::tone::{MAX_HZ, MIN_HZ, voice};

/// The buses the synthesizer mixes, each its own output.
pub const MAX_BUSES: usize = 8;
/// The most voices sounding at once; starting another fades out the oldest.
pub const MAX_VOICES: usize = 48;
/// The most notes waiting for their start time; more are dropped.
pub const MAX_PENDING: usize = 1_024;
/// The longest command encoding, in bytes.
pub const MAX_COMMAND_BYTES: usize = 72;
/// The fade when a voice is cut off rather than released, in seconds.
pub const CUT_SECONDS: f64 = 0.03;

/// What the synthesizer should do next.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Command {
    /// Start a note on `bus`, identified by `key`, at `hz`, peaking at
    /// `level` (0 to 1), on `sound`; at `at` seconds on the audio clock (now
    /// when that has passed, or for 0). With `hold` > 0 it is a timed note,
    /// released after `hold` seconds; otherwise it is held until
    /// [`Command::NoteOff`], and a note already held under `key` on the bus
    /// is released first.
    NoteOn {
        bus: u8,
        key: u32,
        hz: f32,
        level: f32,
        sound: Sound,
        at: f64,
        hold: f64,
    },
    /// Release the note held under `key` on `bus`.
    NoteOff { bus: u8, key: u32 },
    /// Release every held note on `bus`.
    ReleaseBus { bus: u8 },
    /// Fade out the timed notes on `bus` (sounding or waiting); held notes
    /// keep sounding.
    CutTimed { bus: u8 },
    /// Fade out everything on `bus`.
    CutBus { bus: u8 },
    /// Fade out everything.
    CutAll,
    /// Set `bus`'s output level (0 to 1).
    SetVolume { bus: u8, volume: f32 },
}

/// Why bytes are not a command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DecodeError {
    Empty,
    UnknownTag(u8),
    Length {
        tag: u8,
        expected: usize,
        found: usize,
    },
    InvalidValue(&'static str),
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(f, "empty command"),
            Self::UnknownTag(tag) => write!(f, "unknown command tag {tag}"),
            Self::Length {
                tag,
                expected,
                found,
            } => write!(f, "command {tag} needs {expected} bytes, not {found}"),
            Self::InvalidValue(what) => write!(f, "command has an invalid {what}"),
        }
    }
}

impl std::error::Error for DecodeError {}

const TAG_NOTE_ON: u8 = 1;
const TAG_NOTE_OFF: u8 = 2;
const TAG_RELEASE_BUS: u8 = 3;
const TAG_CUT_TIMED: u8 = 4;
const TAG_CUT_BUS: u8 = 5;
const TAG_CUT_ALL: u8 = 6;
const TAG_SET_VOLUME: u8 = 7;

/// Tag, bus, key, hz, level, waveform, five sound values, at, hold (sound
/// values and times as `f64`, so they survive the trip exactly).
const NOTE_ON_BYTES: usize = 1 + 1 + 4 + 4 + 4 + 1 + 5 * 8 + 8 + 8;
const NOTE_OFF_BYTES: usize = 1 + 1 + 4;
const BUS_BYTES: usize = 1 + 1;
const SET_VOLUME_BYTES: usize = 1 + 1 + 4;

/// Little-endian fields after a tag byte.
struct Writer(Vec<u8>);

impl Writer {
    fn new(tag: u8) -> Self {
        let mut bytes = Vec::with_capacity(MAX_COMMAND_BYTES);
        bytes.push(tag);
        Self(bytes)
    }
    fn u8(mut self, value: u8) -> Self {
        self.0.push(value);
        self
    }
    fn u32(mut self, value: u32) -> Self {
        self.0.extend_from_slice(&value.to_le_bytes());
        self
    }
    fn f32(mut self, value: f32) -> Self {
        self.0.extend_from_slice(&value.to_le_bytes());
        self
    }
    fn f64(mut self, value: f64) -> Self {
        self.0.extend_from_slice(&value.to_le_bytes());
        self
    }
}

/// Reads the fields [`Writer`] wrote; the length is checked first.
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn take<const N: usize>(&mut self) -> [u8; N] {
        let field = self.bytes[self.at..self.at + N]
            .try_into()
            .expect("length checked");
        self.at += N;
        field
    }
    fn u8(&mut self) -> u8 {
        self.take::<1>()[0]
    }
    fn u32(&mut self) -> u32 {
        u32::from_le_bytes(self.take())
    }
    fn f32(&mut self) -> f32 {
        f32::from_le_bytes(self.take())
    }
    fn f64(&mut self) -> f64 {
        f64::from_le_bytes(self.take())
    }
    fn bus(&mut self) -> Result<u8, DecodeError> {
        let bus = self.u8();
        check(usize::from(bus) < MAX_BUSES, "bus")?;
        Ok(bus)
    }
}

fn check(ok: bool, what: &'static str) -> Result<(), DecodeError> {
    if ok {
        Ok(())
    } else {
        Err(DecodeError::InvalidValue(what))
    }
}

impl Command {
    /// The command as bytes: a tag, then little-endian fields.
    pub fn encode(&self) -> Vec<u8> {
        match *self {
            Command::NoteOn {
                bus,
                key,
                hz,
                level,
                sound,
                at,
                hold,
            } => {
                let waveform = Waveform::ALL
                    .iter()
                    .position(|waveform| *waveform == sound.waveform)
                    .expect("every waveform is in ALL") as u8;
                let env = sound.envelope;
                Writer::new(TAG_NOTE_ON)
                    .u8(bus)
                    .u32(key)
                    .f32(hz)
                    .f32(level)
                    .u8(waveform)
                    .f64(env.attack)
                    .f64(env.decay)
                    .f64(env.sustain)
                    .f64(env.release)
                    .f64(sound.cutoff_hz)
                    .f64(at)
                    .f64(hold)
            }
            Command::NoteOff { bus, key } => Writer::new(TAG_NOTE_OFF).u8(bus).u32(key),
            Command::ReleaseBus { bus } => Writer::new(TAG_RELEASE_BUS).u8(bus),
            Command::CutTimed { bus } => Writer::new(TAG_CUT_TIMED).u8(bus),
            Command::CutBus { bus } => Writer::new(TAG_CUT_BUS).u8(bus),
            Command::CutAll => Writer::new(TAG_CUT_ALL),
            Command::SetVolume { bus, volume } => Writer::new(TAG_SET_VOLUME).u8(bus).f32(volume),
        }
        .0
    }

    /// Reads a command written by [`Command::encode`], checking its values
    /// (buses in range; a note's frequency, level, sound, and times
    /// playable; volumes 0 to 1).
    pub fn decode(bytes: &[u8]) -> Result<Command, DecodeError> {
        let &tag = bytes.first().ok_or(DecodeError::Empty)?;
        let expected = match tag {
            TAG_NOTE_ON => NOTE_ON_BYTES,
            TAG_NOTE_OFF => NOTE_OFF_BYTES,
            TAG_RELEASE_BUS | TAG_CUT_TIMED | TAG_CUT_BUS => BUS_BYTES,
            TAG_CUT_ALL => 1,
            TAG_SET_VOLUME => SET_VOLUME_BYTES,
            other => return Err(DecodeError::UnknownTag(other)),
        };
        if bytes.len() != expected {
            return Err(DecodeError::Length {
                tag,
                expected,
                found: bytes.len(),
            });
        }
        let mut read = Reader { bytes, at: 1 };
        let command = match tag {
            TAG_NOTE_ON => {
                let bus = read.bus()?;
                let key = read.u32();
                let hz = read.f32();
                let level = read.f32();
                let waveform = *Waveform::ALL
                    .get(usize::from(read.u8()))
                    .ok_or(DecodeError::InvalidValue("waveform"))?;
                let envelope = Envelope {
                    attack: read.f64(),
                    decay: read.f64(),
                    sustain: read.f64(),
                    release: read.f64(),
                };
                let sound = Sound {
                    waveform,
                    envelope,
                    cutoff_hz: read.f64(),
                };
                let (at, hold) = (read.f64(), read.f64());
                check((MIN_HZ..=MAX_HZ).contains(&f64::from(hz)), "frequency")?;
                check((0.0..=1.0).contains(&level), "level")?;
                check(sound.validate().is_ok(), "sound")?;
                check(at.is_finite() && at >= 0.0, "start time")?;
                check(hold.is_finite() && hold >= 0.0, "hold")?;
                Command::NoteOn {
                    bus,
                    key,
                    hz,
                    level,
                    sound,
                    at,
                    hold,
                }
            }
            TAG_NOTE_OFF => Command::NoteOff {
                bus: read.bus()?,
                key: read.u32(),
            },
            TAG_RELEASE_BUS => Command::ReleaseBus { bus: read.bus()? },
            TAG_CUT_TIMED => Command::CutTimed { bus: read.bus()? },
            TAG_CUT_BUS => Command::CutBus { bus: read.bus()? },
            TAG_CUT_ALL => Command::CutAll,
            _ => {
                let bus = read.bus()?;
                let volume = read.f32();
                check((0.0..=1.0).contains(&volume), "volume")?;
                Command::SetVolume { bus, volume }
            }
        };
        Ok(command)
    }
}

/// A note waiting for its start.
#[derive(Clone, Copy)]
struct Pending {
    bus: u8,
    key: u32,
    hz: f32,
    level: f32,
    sound: Sound,
    start_frame: u64,
    hold: f64,
}

struct Voice {
    bus: u8,
    key: u32,
    /// A timed note (released after its hold), rather than a held one.
    timed: bool,
    unit: Box<dyn AudioUnit>,
    envelope: Envelope,
    level: f64,
    /// Frames of silence left in the current block before the note starts.
    delay: usize,
    /// Frames since the note started.
    elapsed: u64,
    /// Seconds after the start at which the note was (or will be) released.
    released_at: Option<f64>,
    /// Cut off: the gain then, and when (seconds after the start).
    cut: Option<(f64, f64)>,
}

impl Voice {
    fn seconds(&self, sample_rate: f64) -> f64 {
        self.elapsed as f64 / sample_rate
    }

    fn gain(&self, sample_rate: f64) -> f64 {
        let t = self.seconds(sample_rate);
        match self.cut {
            Some((from, at)) => from * (1.0 - (t - at) / CUT_SECONDS).max(0.0),
            None => self
                .envelope
                .released_level_at(t, self.released_at.unwrap_or(f64::INFINITY)),
        }
    }

    fn finished(&self, sample_rate: f64) -> bool {
        let t = self.seconds(sample_rate);
        match self.cut {
            Some((_, at)) => t >= at + CUT_SECONDS,
            None => self
                .released_at
                .is_some_and(|at| t >= at + self.envelope.release),
        }
    }

    fn cut(&mut self, sample_rate: f64) {
        if self.cut.is_none() {
            self.cut = Some((self.gain(sample_rate), self.seconds(sample_rate)));
        }
    }
}

/// A polyphonic, multi-bus synthesizer rendering blocks on demand.
///
/// Starting a note allocates its voice; rendering otherwise does not
/// allocate. Output is not limited: loud chords can exceed full scale, so
/// put a compressor or limiter after it (Mnemosyne's buses have one).
pub struct Synth {
    sample_rate: f64,
    /// Oldest first.
    voices: Vec<Voice>,
    pending: Vec<Pending>,
    volumes: [f32; MAX_BUSES],
    /// The frame the next block starts at, on the audio clock.
    frame: u64,
}

impl Synth {
    /// A silent synthesizer at `sample_rate`, buses at full volume. Also
    /// builds each waveform once, so the oscillators' shared wavetables
    /// exist before the first note (building them is the slow part of a
    /// first note).
    pub fn new(sample_rate: f64) -> Synth {
        for waveform in Waveform::ALL {
            let _ = voice(waveform, 440.0, 1_000.0, sample_rate);
        }
        Synth {
            sample_rate,
            voices: Vec::with_capacity(2 * MAX_VOICES),
            pending: Vec::with_capacity(MAX_PENDING),
            volumes: [1.0; MAX_BUSES],
            frame: 0,
        }
    }

    pub fn sample_rate(&self) -> f64 {
        self.sample_rate
    }

    /// Voices sounding or fading out.
    pub fn active_voices(&self) -> usize {
        self.voices.len()
    }

    /// Notes waiting for their start time.
    pub fn pending_notes(&self) -> usize {
        self.pending.len()
    }

    pub fn apply(&mut self, command: Command) {
        match command {
            Command::NoteOn {
                bus,
                key,
                hz,
                level,
                sound,
                at,
                hold,
            } => {
                if hold <= 0.0 {
                    self.release(|voice| voice.bus == bus && voice.key == key);
                }
                let note = Pending {
                    bus,
                    key,
                    hz,
                    level,
                    sound: sound.clamped(),
                    start_frame: (at * self.sample_rate).round() as u64,
                    hold,
                };
                if note.start_frame <= self.frame {
                    self.start(note, 0);
                } else if self.pending.len() < MAX_PENDING {
                    self.pending.push(note);
                }
            }
            Command::NoteOff { bus, key } => {
                self.release(|voice| voice.bus == bus && voice.key == key)
            }
            Command::ReleaseBus { bus } => self.release(|voice| voice.bus == bus),
            Command::CutTimed { bus } => self.cut(|note_bus, timed| note_bus == bus && timed),
            Command::CutBus { bus } => self.cut(|note_bus, _| note_bus == bus),
            Command::CutAll => self.cut(|_, _| true),
            Command::SetVolume { bus, volume } => self.volumes[usize::from(bus)] = volume,
        }
    }

    /// Starts `note`, silent for its first `delay` frames of this block.
    fn start(&mut self, note: Pending, delay: usize) {
        let rate = self.sample_rate;
        let sounding = self
            .voices
            .iter()
            .filter(|voice| voice.cut.is_none())
            .count();
        if sounding >= MAX_VOICES
            && let Some(oldest) = self.voices.iter_mut().find(|voice| voice.cut.is_none())
        {
            oldest.cut(rate);
        }
        // Fading voices make room too, abruptly, past twice the limit.
        if self.voices.len() >= 2 * MAX_VOICES {
            self.voices.remove(0);
        }
        let timed = note.hold > 0.0;
        self.voices.push(Voice {
            bus: note.bus,
            key: note.key,
            timed,
            unit: voice(
                note.sound.waveform,
                f64::from(note.hz),
                note.sound.cutoff_hz,
                rate,
            ),
            envelope: note.sound.envelope,
            level: f64::from(note.level),
            delay,
            elapsed: 0,
            released_at: timed.then_some(note.hold),
            cut: None,
        });
    }

    /// Releases the held notes `which` picks.
    fn release(&mut self, which: impl Fn(&Voice) -> bool) {
        let rate = self.sample_rate;
        for voice in &mut self.voices {
            if !voice.timed && voice.released_at.is_none() && voice.cut.is_none() && which(voice) {
                voice.released_at = Some(voice.seconds(rate));
            }
        }
    }

    /// Fades out voices, and drops waiting notes, that `which` picks
    /// (given the bus and whether the note is timed).
    fn cut(&mut self, which: impl Fn(u8, bool) -> bool) {
        let rate = self.sample_rate;
        for voice in &mut self.voices {
            if which(voice.bus, voice.timed) {
                voice.cut(rate);
            }
        }
        self.pending
            .retain(|note| !which(note.bus, note.hold > 0.0));
    }

    /// Renders `frames` frames starting at `start_frame` on the audio clock
    /// into `out`, which holds [`MAX_BUSES`] blocks of `frames` samples, one
    /// per bus (bus `b` at `out[b * frames..(b + 1) * frames]`). Notes due
    /// within the block start on their exact frame; voices that have faded
    /// out are dropped.
    pub fn render(&mut self, start_frame: u64, frames: usize, out: &mut [f32]) {
        let out = &mut out[..MAX_BUSES * frames];
        out.fill(0.0);
        self.frame = start_frame;
        let end_frame = start_frame + frames as u64;
        let mut index = 0;
        while index < self.pending.len() {
            let note = self.pending[index];
            if note.start_frame < end_frame {
                self.pending.swap_remove(index);
                let delay = note.start_frame.saturating_sub(start_frame) as usize;
                self.start(note, delay);
            } else {
                index += 1;
            }
        }
        let rate = self.sample_rate;
        for voice in &mut self.voices {
            let bus = usize::from(voice.bus);
            let volume = self.volumes[bus];
            let skip = std::mem::take(&mut voice.delay).min(frames);
            for sample in &mut out[bus * frames + skip..(bus + 1) * frames] {
                let gain = voice.gain(rate) * voice.level;
                *sample += voice.unit.get_mono() * gain as f32 * volume;
                voice.elapsed += 1;
            }
        }
        self.voices.retain(|voice| !voice.finished(rate));
        self.frame = end_frame;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::instrument::Preset;
    use crate::tone::{self, Tone};

    const RATE: f64 = 48_000.0;
    const BLOCK: usize = 128;

    fn note_on(bus: u8, key: u32, hz: f32, preset: Preset) -> Command {
        Command::NoteOn {
            bus,
            key,
            hz,
            level: 0.25,
            sound: preset.sound(),
            at: 0.0,
            hold: 0.0,
        }
    }

    fn timed(bus: u8, key: u32, hz: f32, preset: Preset, at: f64, hold: f64) -> Command {
        Command::NoteOn {
            bus,
            key,
            hz,
            level: 0.25,
            sound: preset.sound(),
            at,
            hold,
        }
    }

    /// Renders `seconds` in render-quantum blocks, as an AudioWorklet asks
    /// for them, continuing the synthesizer's clock; returns each bus.
    fn render(synth: &mut Synth, seconds: f64) -> Vec<Vec<f32>> {
        let blocks = (seconds * RATE / BLOCK as f64).round() as usize;
        let mut buses: Vec<Vec<f32>> = (0..MAX_BUSES)
            .map(|_| Vec::with_capacity(blocks * BLOCK))
            .collect();
        let mut out = vec![0.0; MAX_BUSES * BLOCK];
        for _ in 0..blocks {
            let frame = synth.frame;
            synth.render(frame, BLOCK, &mut out);
            for (bus, samples) in buses.iter_mut().enumerate() {
                samples.extend_from_slice(&out[bus * BLOCK..(bus + 1) * BLOCK]);
            }
        }
        buses
    }

    fn peak(samples: &[f32]) -> f32 {
        samples.iter().fold(0.0, |peak, s| peak.max(s.abs()))
    }

    fn largest_difference(a: &[f32], b: &[f32]) -> f32 {
        a.iter()
            .zip(b)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0, f32::max)
    }

    #[test]
    fn commands_round_trip_through_bytes() {
        let commands = [
            note_on(3, 7, 440.0, Preset::Pluck),
            timed(0, 1, 220.0, Preset::Pad, 12.345, 0.5),
            Command::NoteOff { bus: 1, key: 7 },
            Command::ReleaseBus { bus: 2 },
            Command::CutTimed { bus: 3 },
            Command::CutBus { bus: 4 },
            Command::CutAll,
            Command::SetVolume {
                bus: 7,
                volume: 0.3,
            },
        ];
        for command in commands {
            let bytes = command.encode();
            assert!(bytes.len() <= MAX_COMMAND_BYTES);
            assert_eq!(Command::decode(&bytes), Ok(command));
        }
    }

    #[test]
    fn bad_bytes_are_rejected() {
        assert_eq!(Command::decode(&[]), Err(DecodeError::Empty));
        assert_eq!(Command::decode(&[99]), Err(DecodeError::UnknownTag(99)));
        let mut bytes = note_on(0, 1, 440.0, Preset::Pure).encode();
        bytes.pop();
        assert!(matches!(
            Command::decode(&bytes),
            Err(DecodeError::Length { .. })
        ));
        let decoded = |command: Command| Command::decode(&command.encode());
        assert_eq!(
            decoded(note_on(0, 1, 50_000.0, Preset::Pure)),
            Err(DecodeError::InvalidValue("frequency"))
        );
        assert_eq!(
            decoded(note_on(MAX_BUSES as u8, 1, 440.0, Preset::Pure)),
            Err(DecodeError::InvalidValue("bus"))
        );
        assert_eq!(
            decoded(Command::SetVolume {
                bus: 0,
                volume: 2.0
            }),
            Err(DecodeError::InvalidValue("volume"))
        );
        assert_eq!(
            decoded(timed(0, 1, 440.0, Preset::Pure, f64::NAN, 0.0)),
            Err(DecodeError::InvalidValue("start time"))
        );
        let mut bad_waveform = note_on(0, 1, 440.0, Preset::Pure).encode();
        // Tag, bus, key, hz, level, then the waveform.
        bad_waveform[14] = 9;
        assert_eq!(
            Command::decode(&bad_waveform),
            Err(DecodeError::InvalidValue("waveform"))
        );
    }

    /// A held note played live matches the same note rendered as a tone.
    #[test]
    fn live_notes_sound_like_rendered_tones() {
        let sound = Preset::Soft.sound();
        // One second is a whole number of blocks (375), so the release
        // falls where the rendered tone's does.
        let tone = Tone {
            hz: 330.0,
            start: 0.0,
            duration: 1.0,
        };
        let rendered = tone::render(&[tone], &sound, 1.0, 48_000).unwrap();
        let mut synth = Synth::new(RATE);
        synth.apply(note_on(0, 1, 330.0, Preset::Soft));
        let mut live = render(&mut synth, 1.0).swap_remove(0);
        synth.apply(Command::NoteOff { bus: 0, key: 1 });
        live.extend(render(&mut synth, 0.25).swap_remove(0));
        let difference = largest_difference(&rendered.samples, &live);
        assert!(difference < 1e-4, "{difference}");
        assert_eq!(synth.active_voices(), 0);
    }

    /// A whole melody handed over at once plays like the rendered tones,
    /// each note starting on its exact frame.
    #[test]
    fn timed_notes_match_rendered_melodies() {
        let sound = Preset::Pluck.sound();
        let tones = tone::sequence(&[261.63, 329.63, 392.0], 0.3);
        let rendered = tone::render(&tones, &sound, 1.0, 48_000).unwrap();
        let mut synth = Synth::new(RATE);
        // The melody starts a little after the clock's start, as it would
        // when sent ahead of time, and not on a block boundary.
        let origin = 0.0103;
        for (key, tone) in tones.iter().enumerate() {
            synth.apply(Command::NoteOn {
                bus: 2,
                key: key as u32,
                hz: tone.hz as f32,
                level: 0.25,
                sound,
                at: origin + tone.start,
                hold: tone.duration,
            });
        }
        assert_eq!(synth.pending_notes(), 3);
        let live = render(&mut synth, 1.5).swap_remove(2);
        let offset = (origin * RATE).round() as usize;
        assert!(live[..offset].iter().all(|s| *s == 0.0));
        let difference = largest_difference(&rendered.samples, &live[offset..]);
        assert!(difference < 1e-4, "{difference}");
        assert_eq!((synth.active_voices(), synth.pending_notes()), (0, 0));
    }

    #[test]
    fn buses_are_mixed_separately_with_their_own_volume() {
        let mut synth = Synth::new(RATE);
        synth.apply(Command::SetVolume {
            bus: 1,
            volume: 0.5,
        });
        synth.apply(note_on(0, 1, 220.0, Preset::Pure));
        synth.apply(note_on(1, 1, 220.0, Preset::Pure));
        let buses = render(&mut synth, 0.2);
        let (a, b) = (peak(&buses[0][4_800..]), peak(&buses[1][4_800..]));
        assert!(
            (a - 0.25).abs() < 0.01 && (b - 0.125).abs() < 0.01,
            "{a} {b}"
        );
        assert!(buses[2..].iter().all(|bus| peak(bus) == 0.0));
        // Releasing one bus leaves the other.
        synth.apply(Command::ReleaseBus { bus: 0 });
        render(&mut synth, 0.2);
        assert_eq!(synth.active_voices(), 1);
    }

    #[test]
    fn keys_release_their_own_held_notes() {
        let mut synth = Synth::new(RATE);
        synth.apply(note_on(0, 1, 220.0, Preset::Organ));
        synth.apply(note_on(0, 2, 330.0, Preset::Organ));
        render(&mut synth, 0.1);
        synth.apply(Command::NoteOff { bus: 0, key: 1 });
        let after = render(&mut synth, 0.2).swap_remove(0);
        assert_eq!(synth.active_voices(), 1);
        assert!(peak(&after[after.len() - 1_000..]) > 0.05);
        // The same key again restarts rather than stacking.
        synth.apply(note_on(0, 2, 330.0, Preset::Organ));
        render(&mut synth, 0.1);
        assert_eq!(synth.active_voices(), 1);
    }

    #[test]
    fn cutting_timed_notes_keeps_held_ones() {
        let mut synth = Synth::new(RATE);
        synth.apply(note_on(0, 1, 220.0, Preset::Organ));
        for key in 0..3 {
            synth.apply(timed(
                0,
                100 + key,
                330.0,
                Preset::Organ,
                f64::from(key) * 0.2,
                0.15,
            ));
        }
        render(&mut synth, 0.1);
        assert_eq!((synth.active_voices(), synth.pending_notes()), (2, 2));
        synth.apply(Command::CutTimed { bus: 0 });
        render(&mut synth, 0.1);
        assert_eq!((synth.active_voices(), synth.pending_notes()), (1, 0));
        synth.apply(Command::CutAll);
        let tail = render(&mut synth, 0.1).swap_remove(0);
        assert_eq!(synth.active_voices(), 0);
        assert!(tail[tail.len() - 100..].iter().all(|s| *s == 0.0));
    }

    #[test]
    fn the_oldest_voice_makes_way() {
        let mut synth = Synth::new(RATE);
        for key in 0..(MAX_VOICES as u32 + 5) {
            synth.apply(note_on(0, key, 110.0 + key as f32, Preset::Pad));
        }
        let sounding = synth
            .voices
            .iter()
            .filter(|voice| voice.cut.is_none())
            .count();
        assert_eq!(sounding, MAX_VOICES);
        let out = render(&mut synth, 0.1);
        assert!(out[0].iter().all(|s| s.is_finite()));
        // The cut voices have faded and gone.
        assert_eq!(synth.active_voices(), MAX_VOICES);
    }
}
