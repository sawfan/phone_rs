//! How a note sounds: an oscillator shape, an envelope, and a low-pass
//! tone filter, with named presets.
//!
//! Plain data and arithmetic. [`crate::tone::render`] turns them into
//! samples; a host that plays notes some other way (Mnemosyne's browser
//! music blocks drive Web Audio nodes) reads the same values, so a preset
//! means the same sound everywhere.

/// Shortest attack and release, in seconds; anything shorter clicks.
pub const MIN_ATTACK_SECONDS: f64 = 0.003;
pub const MIN_RELEASE_SECONDS: f64 = 0.01;
/// Longest attack, decay, and release accepted, in seconds.
pub const MAX_STAGE_SECONDS: f64 = 4.0;
/// Tone-filter cutoff range in hertz. At the top the filter is open.
pub const MIN_CUTOFF_HZ: f64 = 100.0;
pub const MAX_CUTOFF_HZ: f64 = 20_000.0;

/// The oscillator shape. Sine is pure; the others add overtones, which make
/// beating between mistuned intervals and low notes easier to hear.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Waveform {
    Sine,
    #[default]
    Triangle,
    Square,
    Sawtooth,
}

impl Waveform {
    pub const ALL: [Waveform; 4] = [
        Waveform::Sine,
        Waveform::Triangle,
        Waveform::Square,
        Waveform::Sawtooth,
    ];

    pub fn id(self) -> &'static str {
        match self {
            Waveform::Sine => "sine",
            Waveform::Triangle => "triangle",
            Waveform::Square => "square",
            Waveform::Sawtooth => "sawtooth",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Waveform::Sine => "Sine (pure)",
            Waveform::Triangle => "Triangle (soft)",
            Waveform::Square => "Square (hollow)",
            Waveform::Sawtooth => "Sawtooth (bright)",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|waveform| waveform.id() == id)
    }
}

/// An attack–decay–sustain–release envelope. Stage lengths are seconds;
/// `sustain` is the held level as a fraction of the peak.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Envelope {
    pub attack: f64,
    pub decay: f64,
    pub sustain: f64,
    pub release: f64,
}

impl Envelope {
    /// The envelope with every value in its accepted range.
    pub fn clamped(self) -> Self {
        let stage = |value: f64, min: f64| {
            if value.is_finite() {
                value.clamp(min, MAX_STAGE_SECONDS)
            } else {
                min
            }
        };
        Self {
            attack: stage(self.attack, MIN_ATTACK_SECONDS),
            decay: stage(self.decay, 0.0),
            sustain: if self.sustain.is_finite() {
                self.sustain.clamp(0.0, 1.0)
            } else {
                1.0
            },
            release: stage(self.release, MIN_RELEASE_SECONDS),
        }
    }

    /// The level, as a fraction of the peak, `elapsed` seconds after the
    /// note started while it is still held.
    pub fn level_at(&self, elapsed: f64) -> f64 {
        let env = self.clamped();
        if elapsed <= 0.0 {
            0.0
        } else if elapsed < env.attack {
            elapsed / env.attack
        } else if elapsed < env.attack + env.decay {
            1.0 - (1.0 - env.sustain) * (elapsed - env.attack) / env.decay
        } else {
            env.sustain
        }
    }

    /// The level `elapsed` seconds after the start of a note released after
    /// `hold` seconds: [`Envelope::level_at`] until the release, then a
    /// linear fall from wherever the note had got to, reaching silence
    /// `release` seconds later.
    pub fn released_level_at(&self, elapsed: f64, hold: f64) -> f64 {
        if elapsed < hold {
            return self.level_at(elapsed);
        }
        let release = self.clamped().release;
        let fall = (elapsed - hold) / release;
        if fall >= 1.0 {
            0.0
        } else {
            self.level_at(hold) * (1.0 - fall)
        }
    }
}

/// The oscillator shape, envelope, and low-pass cutoff of a sound.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sound {
    pub waveform: Waveform,
    pub envelope: Envelope,
    pub cutoff_hz: f64,
}

impl Sound {
    /// The sound with every value in its accepted range.
    pub fn clamped(self) -> Self {
        Self {
            envelope: self.envelope.clamped(),
            cutoff_hz: if self.cutoff_hz.is_finite() {
                self.cutoff_hz.clamp(MIN_CUTOFF_HZ, MAX_CUTOFF_HZ)
            } else {
                MAX_CUTOFF_HZ
            },
            ..self
        }
    }

    /// Checks that every value is in its accepted range (stages 0 to
    /// [`MAX_STAGE_SECONDS`], sustain 0 to 1, cutoff [`MIN_CUTOFF_HZ`] to
    /// [`MAX_CUTOFF_HZ`]), naming the first that is not.
    pub fn validate(&self) -> Result<(), String> {
        let env = &self.envelope;
        let checks = [
            ("attack", env.attack, 0.0, MAX_STAGE_SECONDS),
            ("decay", env.decay, 0.0, MAX_STAGE_SECONDS),
            ("sustain", env.sustain, 0.0, 1.0),
            ("release", env.release, 0.0, MAX_STAGE_SECONDS),
            ("cutoff", self.cutoff_hz, MIN_CUTOFF_HZ, MAX_CUTOFF_HZ),
        ];
        for (name, value, min, max) in checks {
            if !(min..=max).contains(&value) {
                return Err(format!("{name} must be from {min} to {max}, not {value}."));
            }
        }
        Ok(())
    }
}

/// Changes to a preset's sound; `None` keeps the preset's value.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SoundChanges {
    pub waveform: Option<Waveform>,
    pub attack: Option<f64>,
    pub decay: Option<f64>,
    pub sustain: Option<f64>,
    pub release: Option<f64>,
    pub cutoff_hz: Option<f64>,
}

impl Sound {
    /// `preset`'s sound with `changes` applied, checked with
    /// [`Sound::validate`].
    pub fn customized(preset: Preset, changes: &SoundChanges) -> Result<Sound, String> {
        let mut sound = preset.sound();
        let env = &mut sound.envelope;
        sound.waveform = changes.waveform.unwrap_or(sound.waveform);
        env.attack = changes.attack.unwrap_or(env.attack);
        env.decay = changes.decay.unwrap_or(env.decay);
        env.sustain = changes.sustain.unwrap_or(env.sustain);
        env.release = changes.release.unwrap_or(env.release);
        sound.cutoff_hz = changes.cutoff_hz.unwrap_or(sound.cutoff_hz);
        sound.validate()?;
        Ok(sound)
    }
}

/// Named starting points for an instrument's sound.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Preset {
    Pure,
    Soft,
    Organ,
    Pluck,
    Pad,
    Bright,
}

impl Preset {
    pub const ALL: [Preset; 6] = [
        Preset::Pure,
        Preset::Soft,
        Preset::Organ,
        Preset::Pluck,
        Preset::Pad,
        Preset::Bright,
    ];

    pub fn id(self) -> &'static str {
        match self {
            Preset::Pure => "pure",
            Preset::Soft => "soft",
            Preset::Organ => "organ",
            Preset::Pluck => "pluck",
            Preset::Pad => "pad",
            Preset::Bright => "bright",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Preset::Pure => "Pure tone",
            Preset::Soft => "Soft",
            Preset::Organ => "Organ",
            Preset::Pluck => "Pluck",
            Preset::Pad => "Pad",
            Preset::Bright => "Bright",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|preset| preset.id() == id)
    }

    /// The preset's oscillator, envelope, and filter.
    pub fn sound(self) -> Sound {
        let sound = |waveform, attack, decay, sustain, release, cutoff_hz| Sound {
            waveform,
            envelope: Envelope {
                attack,
                decay,
                sustain,
                release,
            },
            cutoff_hz,
        };
        match self {
            Preset::Pure => sound(Waveform::Sine, 0.01, 0.1, 1.0, 0.08, MAX_CUTOFF_HZ),
            Preset::Soft => sound(Waveform::Triangle, 0.01, 0.25, 0.75, 0.2, MAX_CUTOFF_HZ),
            Preset::Organ => sound(Waveform::Square, 0.005, 0.0, 1.0, 0.04, 3_000.0),
            Preset::Pluck => sound(Waveform::Sawtooth, 0.003, 0.7, 0.0, 0.25, 2_500.0),
            Preset::Pad => sound(Waveform::Sawtooth, 0.45, 0.4, 0.7, 1.2, 1_200.0),
            Preset::Bright => sound(Waveform::Sawtooth, 0.01, 0.15, 0.85, 0.12, MAX_CUTOFF_HZ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ENV: Envelope = Envelope {
        attack: 0.1,
        decay: 0.2,
        sustain: 0.5,
        release: 0.3,
    };

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn envelope_levels() {
        assert_eq!(ENV.level_at(0.0), 0.0);
        assert!(close(ENV.level_at(0.05), 0.5));
        assert!(close(ENV.level_at(0.1), 1.0));
        assert!(close(ENV.level_at(0.2), 0.75));
        assert!(close(ENV.level_at(5.0), 0.5));
        let no_decay = Envelope { decay: 0.0, ..ENV };
        assert!(close(no_decay.level_at(0.2), 0.5));
    }

    #[test]
    fn released_notes_fall_from_where_they_got_to() {
        // Held past the decay: falls from the sustain level.
        assert!(close(ENV.released_level_at(0.5, 1.0), 0.5));
        assert!(close(ENV.released_level_at(1.15, 1.0), 0.25));
        assert_eq!(ENV.released_level_at(1.3, 1.0), 0.0);
        assert_eq!(ENV.released_level_at(9.0, 1.0), 0.0);
        // Released during the attack: falls from half the peak.
        assert!(close(ENV.released_level_at(0.05, 0.05), 0.5));
        assert!(close(ENV.released_level_at(0.2, 0.05), 0.25));
    }

    #[test]
    fn clamping_keeps_values_usable() {
        let wild = Envelope {
            attack: 0.0,
            decay: -1.0,
            sustain: 3.0,
            release: f64::NAN,
        }
        .clamped();
        assert_eq!(wild.attack, MIN_ATTACK_SECONDS);
        assert_eq!(wild.decay, 0.0);
        assert_eq!(wild.sustain, 1.0);
        assert_eq!(wild.release, MIN_RELEASE_SECONDS);
        let sound = Sound {
            cutoff_hz: f64::INFINITY,
            ..Preset::Organ.sound()
        };
        assert_eq!(sound.clamped().cutoff_hz, MAX_CUTOFF_HZ);
    }

    #[test]
    fn sounds_are_validated() {
        assert!(Preset::Pad.sound().validate().is_ok());
        let mut sound = Preset::Soft.sound();
        sound.envelope.sustain = 1.5;
        assert!(sound.validate().unwrap_err().starts_with("sustain"));
        sound.envelope.sustain = 0.5;
        sound.cutoff_hz = f64::NAN;
        assert!(sound.validate().unwrap_err().starts_with("cutoff"));
    }

    #[test]
    fn presets_can_be_customized() {
        let changes = SoundChanges {
            waveform: Some(Waveform::Sine),
            cutoff_hz: Some(900.0),
            ..SoundChanges::default()
        };
        let sound = Sound::customized(Preset::Pluck, &changes).unwrap();
        assert_eq!(sound.waveform, Waveform::Sine);
        assert_eq!(sound.cutoff_hz, 900.0);
        assert_eq!(sound.envelope, Preset::Pluck.sound().envelope);
        let unchanged = Sound::customized(Preset::Pad, &SoundChanges::default());
        assert_eq!(unchanged, Ok(Preset::Pad.sound()));
        let too_long = SoundChanges {
            release: Some(9.0),
            ..SoundChanges::default()
        };
        assert!(Sound::customized(Preset::Pad, &too_long).is_err());
    }

    #[test]
    fn presets_round_trip_and_are_valid() {
        for preset in Preset::ALL {
            assert_eq!(Preset::from_id(preset.id()), Some(preset));
            let sound = preset.sound();
            assert_eq!(sound.envelope, sound.envelope.clamped(), "{preset:?}");
            assert!(sound.validate().is_ok(), "{preset:?}");
        }
    }

    #[test]
    fn waveform_ids_round_trip() {
        for waveform in Waveform::ALL {
            assert_eq!(Waveform::from_id(waveform.id()), Some(waveform));
        }
        assert_eq!(Waveform::from_id("noise"), None);
    }
}
