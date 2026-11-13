//! A formant voice: keyframes in, samples out.
//!
//! A Klatt-style design (Klatt 1980), reduced to what a speech planner (such
//! as `ipa-core`'s) needs:
//!
//! - a voicing source (Rosenberg's glottal airflow pulse, evaluated at the
//!   exact phase, with vibrato, jitter, shimmer, and breath noise) plus
//!   aspiration noise, both through a cascade of formant resonators (F1–F3
//!   from the keyframes, F4 and F5 fixed), then lip radiation;
//! - frication noise through one band-pass filter, added in parallel;
//! - a high-frequency emphasis on the sum.
//!
//! Parameters are linearly interpolated between keyframes; filter
//! coefficients are updated every [`UPDATE_SAMPLES`] samples.

use std::f64::consts::PI;

/// The fourth and fifth formants, fixed.
const F4_HZ: f64 = 3500.0;
const F5_HZ: f64 = 4500.0;
const B4_HZ: f64 = 200.0;
const B5_HZ: f64 = 250.0;
/// Filter coefficients are recomputed this often (about 0.7 ms).
const UPDATE_SAMPLES: usize = 16;

/// Gains that set aspiration and frication against voicing (calibrated so a
/// steady `s` is about 9 dB and `h` about 14 dB below a steady `a`).
const ASPIRATION_GAIN: f64 = 0.028;
const FRICATION_GAIN: f64 = 0.055;

/// The voice's natural irregularities.
mod voice {
    /// Fraction of a period the glottis is opening, then closing (Rosenberg
    /// 1971); closed for the rest.
    pub const OPENING: f64 = 0.40;
    pub const CLOSING: f64 = 0.16;
    /// Vibrato: depth (fraction of pitch) and rate (Hz).
    pub const VIBRATO_DEPTH: f64 = 0.010;
    pub const VIBRATO_HZ: f64 = 5.2;
    /// Random change of each period's length and loudness (fractions).
    pub const JITTER: f64 = 0.006;
    pub const SHIMMER: f64 = 0.05;
    /// Breath noise mixed into voicing, following the airflow.
    pub const BREATH: f64 = 0.04;
    /// High-frequency emphasis after the lips (`y = x − k·x₁`).
    pub const EMPHASIS: f64 = 0.75;
}

/// What the synthesizer does at one moment.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Params {
    /// Pitch, in hertz.
    pub f0: f64,
    /// Voicing amplitude, 0–1.
    pub voice: f64,
    /// Aspiration noise through the formants, 0–1.
    pub aspiration: f64,
    /// Frication noise through the band-pass filter, 0–1 (bursts may
    /// exceed 1).
    pub frication: f64,
    /// F1–F3 and their bandwidths, in hertz.
    pub formants: [f64; 3],
    pub bandwidths: [f64; 3],
    /// The frication filter's centre and bandwidth, in hertz.
    pub fric_freq: f64,
    pub fric_bw: f64,
    /// Nasal coupling, 0–1: moves the nasal zero away from the nasal pole.
    pub nasal: f64,
}

impl Params {
    /// Silence with these formants (so later sound can glide from them).
    pub fn silent(formants: [f64; 3]) -> Params {
        Params {
            f0: 120.0,
            voice: 0.0,
            aspiration: 0.0,
            frication: 0.0,
            formants,
            bandwidths: [80.0, 100.0, 140.0],
            fric_freq: 4000.0,
            fric_bw: 2000.0,
            nasal: 0.0,
        }
    }

    fn lerp(&self, other: &Params, t: f64) -> Params {
        let mix = |a: f64, b: f64| a + (b - a) * t;
        let each = |a: [f64; 3], b: [f64; 3]| [mix(a[0], b[0]), mix(a[1], b[1]), mix(a[2], b[2])];
        Params {
            f0: mix(self.f0, other.f0),
            voice: mix(self.voice, other.voice),
            aspiration: mix(self.aspiration, other.aspiration),
            frication: mix(self.frication, other.frication),
            formants: each(self.formants, other.formants),
            bandwidths: each(self.bandwidths, other.bandwidths),
            fric_freq: mix(self.fric_freq, other.fric_freq),
            fric_bw: mix(self.fric_bw, other.fric_bw),
            nasal: mix(self.nasal, other.nasal),
        }
    }
}

/// Parameters at a time, in seconds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Key {
    pub time: f64,
    pub params: Params,
}

/// A two-pole resonator (Klatt 1980): `y = a·x + b·y₁ + c·y₂`, with unity
/// gain at 0 Hz. Its coefficients can change while its state carries on.
#[derive(Debug, Clone, Copy, Default)]
struct Resonator {
    a: f64,
    b: f64,
    c: f64,
    y1: f64,
    y2: f64,
}

impl Resonator {
    fn set(&mut self, frequency: f64, bandwidth: f64, sample_rate: f64) {
        let t = 1.0 / sample_rate;
        self.c = -(-2.0 * PI * bandwidth * t).exp();
        self.b = 2.0 * (-PI * bandwidth * t).exp() * (2.0 * PI * frequency * t).cos();
        self.a = 1.0 - self.b - self.c;
    }

    fn step(&mut self, x: f64) -> f64 {
        let y = self.a * x + self.b * self.y1 + self.c * self.y2;
        self.y2 = self.y1;
        self.y1 = y;
        y
    }
}

/// An antiresonator (Klatt 1980): the inverse of a [`Resonator`], putting a
/// zero (a notch) in the spectrum.
#[derive(Debug, Clone, Copy, Default)]
struct AntiResonator {
    a: f64,
    b: f64,
    c: f64,
    x1: f64,
    x2: f64,
}

impl AntiResonator {
    fn set(&mut self, frequency: f64, bandwidth: f64, sample_rate: f64) {
        let mut r = Resonator::default();
        r.set(frequency, bandwidth, sample_rate);
        self.a = 1.0 / r.a;
        self.b = -r.b / r.a;
        self.c = -r.c / r.a;
    }

    fn step(&mut self, x: f64) -> f64 {
        let y = self.a * x + self.b * self.x1 + self.c * self.x2;
        self.x2 = self.x1;
        self.x1 = x;
        y
    }
}

/// The nasal pole and the zero's range (Klatt 1980): with no coupling the
/// zero sits on the pole and they cancel; nasals move it up to 450 Hz.
const NASAL_POLE_HZ: f64 = 270.0;
const NASAL_ZERO_MAX_HZ: f64 = 450.0;
const NASAL_BW_HZ: f64 = 100.0;

/// A band-pass filter with 0 dB peak gain (a constant-peak biquad), for
/// shaping frication noise.
#[derive(Debug, Clone, Copy, Default)]
struct BandPass {
    b0: f64,
    a1: f64,
    a2: f64,
    x1: f64,
    x2: f64,
    y1: f64,
    y2: f64,
}

impl BandPass {
    fn set(&mut self, frequency: f64, bandwidth: f64, sample_rate: f64) {
        let frequency = frequency.clamp(100.0, sample_rate * 0.45);
        let w0 = 2.0 * PI * frequency / sample_rate;
        let q = (frequency / bandwidth.max(50.0)).max(0.3);
        let alpha = w0.sin() / (2.0 * q);
        let a0 = 1.0 + alpha;
        self.b0 = alpha / a0;
        self.a1 = -2.0 * w0.cos() / a0;
        self.a2 = (1.0 - alpha) / a0;
    }

    fn step(&mut self, x: f64) -> f64 {
        let y = self.b0 * x - self.b0 * self.x2 - self.a1 * self.y1 - self.a2 * self.y2;
        self.x2 = self.x1;
        self.x1 = x;
        self.y2 = self.y1;
        self.y1 = y;
        y
    }
}

/// Glottal airflow at `phase` (0..1) of a period: a smooth opening, a
/// quicker closing, then closed (Rosenberg's pulse).
fn glottal_flow(phase: f64) -> f64 {
    let (open, close) = (voice::OPENING, voice::CLOSING);
    if phase < open {
        0.5 * (1.0 - (PI * phase / open).cos())
    } else if phase < open + close {
        (PI / 2.0 * (phase - open) / close).cos()
    } else {
        0.0
    }
}

/// A small deterministic noise source (linear congruential), in −0.5..0.5.
fn noise(seed: &mut u32) -> f64 {
    *seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
    f64::from(*seed) / f64::from(u32::MAX) - 0.5
}

/// The parameters at `time`, interpolated; `cursor` remembers where the
/// last lookup was (times only move forward).
fn params_at(keys: &[Key], time: f64, cursor: &mut usize) -> Params {
    while *cursor + 1 < keys.len() && keys[*cursor + 1].time <= time {
        *cursor += 1;
    }
    let here = &keys[*cursor];
    match keys.get(*cursor + 1) {
        Some(next) if next.time > here.time && time >= here.time => {
            let t = ((time - here.time) / (next.time - here.time)).clamp(0.0, 1.0);
            here.params.lerp(&next.params, t)
        }
        _ => here.params,
    }
}

/// Renders keyframes (sorted by time) as samples at `sample_rate`, up to the
/// last key. The level is not normalized (see [`crate::pcm::normalize`]).
/// `seed` drives the noise, so the same keys and seed give the same samples.
pub fn render(keys: &[Key], sample_rate: f64, mut seed: u32) -> Vec<f64> {
    let Some(last) = keys.last() else {
        return Vec::new();
    };
    let count = (last.time * sample_rate).ceil().max(0.0) as usize;
    let mut cascade = [Resonator::default(); 5];
    cascade[3].set(F4_HZ, B4_HZ, sample_rate);
    cascade[4].set(F5_HZ, B5_HZ, sample_rate);
    let mut frication = BandPass::default();
    let mut nasal_pole = Resonator::default();
    nasal_pole.set(NASAL_POLE_HZ, NASAL_BW_HZ, sample_rate);
    let mut nasal_zero = AntiResonator::default();
    let mut cursor = 0;
    let mut phase = 0.0;
    let (mut period_pitch, mut period_gain) = (1.0, 1.0);
    let (mut previous, mut previous_radiated) = (0.0, 0.0);
    let mut out = Vec::with_capacity(count);
    let mut params = keys[0].params;
    for n in 0..count {
        let time = n as f64 / sample_rate;
        if n % UPDATE_SAMPLES == 0 {
            params = params_at(keys, time, &mut cursor);
            for (index, resonator) in cascade.iter_mut().take(3).enumerate() {
                resonator.set(
                    params.formants[index],
                    params.bandwidths[index],
                    sample_rate,
                );
            }
            frication.set(params.fric_freq, params.fric_bw, sample_rate);
            let zero = NASAL_POLE_HZ + (NASAL_ZERO_MAX_HZ - NASAL_POLE_HZ) * params.nasal;
            nasal_zero.set(zero, NASAL_BW_HZ, sample_rate);
        }
        // Voicing, with natural irregularities.
        let vibrato = 1.0 + voice::VIBRATO_DEPTH * (2.0 * PI * voice::VIBRATO_HZ * time).sin();
        phase += params.f0 * vibrato * period_pitch / sample_rate;
        if phase >= 1.0 {
            phase -= 1.0;
            period_pitch = 1.0 + 2.0 * voice::JITTER * noise(&mut seed);
            period_gain = 1.0 + 2.0 * voice::SHIMMER * noise(&mut seed);
        }
        let flow = glottal_flow(phase) * period_gain * params.voice;
        let breath = voice::BREATH * flow * noise(&mut seed);
        let aspiration = ASPIRATION_GAIN * params.aspiration * noise(&mut seed);
        let mut y = nasal_zero.step(nasal_pole.step(flow + breath + aspiration));
        for resonator in &mut cascade {
            y = resonator.step(y);
        }
        // Radiation at the lips: a first difference.
        let radiated = y - previous;
        previous = y;
        let fricated = FRICATION_GAIN * params.frication * frication.step(noise(&mut seed));
        let sum = radiated + fricated;
        out.push(sum - voice::EMPHASIS * previous_radiated);
        previous_radiated = sum;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn steady(params: Params, seconds: f64) -> Vec<f64> {
        let keys = [
            Key { time: 0.0, params },
            Key {
                time: seconds,
                params,
            },
        ];
        render(&keys, 22_050.0, 1)
    }

    fn rms(samples: &[f64]) -> f64 {
        let middle = &samples[samples.len() / 4..samples.len() * 3 / 4];
        (middle.iter().map(|s| s * s).sum::<f64>() / middle.len() as f64).sqrt()
    }

    /// Without coupling the nasal pole and zero cancel; with it, the
    /// murmur gains low-frequency energy around the pole.
    #[test]
    fn nasal_coupling_changes_the_spectrum() {
        let oral = Params {
            voice: 1.0,
            ..Params::silent([250.0, 1500.0, 2500.0])
        };
        let nasal = Params { nasal: 1.0, ..oral };
        let (o, n) = (steady(oral, 0.4), steady(nasal, 0.4));
        assert!(o.iter().all(|s| s.is_finite()) && n.iter().all(|s| s.is_finite()));
        assert!((rms(&o) - rms(&n)).abs() > 0.01 * rms(&o));
    }

    #[test]
    fn interpolates_between_keys() {
        let a = Params::silent([300.0, 1000.0, 2500.0]);
        let b = Params {
            voice: 1.0,
            ..Params::silent([700.0, 1400.0, 2500.0])
        };
        let keys = [
            Key {
                time: 0.0,
                params: a,
            },
            Key {
                time: 1.0,
                params: b,
            },
        ];
        let mut cursor = 0;
        let half = params_at(&keys, 0.5, &mut cursor);
        assert!((half.voice - 0.5).abs() < 1e-9);
        assert!((half.formants[0] - 500.0).abs() < 1e-9);
        assert_eq!(params_at(&keys, 2.0, &mut cursor).voice, 1.0);
        assert!(render(&[], 22_050.0, 1).is_empty());
    }

    /// The gains put frication and aspiration at sensible levels against a
    /// vowel.
    #[test]
    fn noise_levels_are_calibrated() {
        let vowel = Params {
            voice: 1.0,
            ..Params::silent([800.0, 1650.0, 2800.0])
        };
        let s = Params {
            frication: 0.6,
            fric_freq: 6500.0,
            fric_bw: 2000.0,
            ..Params::silent([250.0, 1700.0, 2700.0])
        };
        let h = Params {
            aspiration: 0.5,
            ..Params::silent([800.0, 1650.0, 2800.0])
        };
        let db = |x: f64, reference: f64| 20.0 * (x / reference).log10();
        let a = rms(&steady(vowel, 0.4));
        let s_db = db(rms(&steady(s, 0.4)), a);
        let h_db = db(rms(&steady(h, 0.4)), a);
        assert!((-14.0..-4.0).contains(&s_db), "s at {s_db:.1} dB");
        assert!((-20.0..-8.0).contains(&h_db), "h at {h_db:.1} dB");
    }
}
