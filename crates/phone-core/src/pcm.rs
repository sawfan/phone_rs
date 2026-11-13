//! Rendered samples: level normalization and WAV encoding.

/// Mono samples at a sample rate: what every voice and instrument renders.
#[derive(Clone, Debug, PartialEq)]
pub struct Pcm {
    pub sample_rate: u32,
    pub samples: Vec<f32>,
}

impl Pcm {
    /// How long the sound lasts, in seconds.
    pub fn seconds(&self) -> f64 {
        self.samples.len() as f64 / f64::from(self.sample_rate)
    }

    /// The samples in a 16-bit WAV file (see [`wav`]).
    pub fn wav(&self) -> Vec<u8> {
        wav(&self.samples, self.sample_rate)
    }

    /// The sound at `sample_rate`, by band-limited (windowed-sinc)
    /// interpolation: when lowering the rate, frequencies above the new
    /// Nyquist limit are filtered out rather than folded back. A copy when
    /// the rate is already `sample_rate`. (See [`Resampler`] for sound that
    /// arrives in pieces.)
    pub fn resampled(&self, sample_rate: u32) -> Pcm {
        let mut resampler = Resampler::new(self.sample_rate, sample_rate);
        let mut samples = Vec::new();
        resampler.push(&self.samples, &mut samples);
        resampler.finish(&mut samples);
        Pcm {
            sample_rate,
            samples,
        }
    }
}

/// Resamples sound that arrives in pieces (such as streamed speech), giving
/// exactly what [`Pcm::resampled`] gives for the whole: each output sample
/// is written once the input it depends on has arrived, so piece
/// boundaries leave no trace.
#[derive(Clone, Debug)]
pub struct Resampler {
    ratio: f64,
    /// The filter's cutoff in cycles per input sample.
    cutoff: f64,
    /// Half the filter's width in input samples.
    half_width: f64,
    from: u64,
    to: u64,
    /// Input not yet needed by every output, from input index `start`.
    buffer: Vec<f32>,
    start: u64,
    received: u64,
    produced: u64,
}

impl Resampler {
    /// A resampler from `from` to `to` samples per second.
    pub fn new(from: u32, to: u32) -> Resampler {
        let ratio = f64::from(to) / f64::from(from);
        // A little below the lower of the two Nyquist limits.
        let cutoff = 0.5 * ratio.min(1.0) * RESAMPLE_PASSBAND;
        Resampler {
            ratio,
            cutoff,
            // Wider when the cutoff is lower, keeping the same number of
            // zero crossings.
            half_width: RESAMPLE_ZERO_CROSSINGS / (2.0 * cutoff),
            from: u64::from(from),
            to: u64::from(to),
            buffer: Vec::new(),
            start: 0,
            received: 0,
            produced: 0,
        }
    }

    fn passthrough(&self) -> bool {
        self.from == self.to
    }

    /// Takes more input and appends the output it completes to `out`.
    pub fn push(&mut self, input: &[f32], out: &mut Vec<f32>) {
        if self.passthrough() {
            out.extend_from_slice(input);
            self.received += input.len() as u64;
            return;
        }
        self.buffer.extend_from_slice(input);
        self.received += input.len() as u64;
        loop {
            let centre = self.produced as f64 / self.ratio;
            // Every input the sample needs has arrived.
            if (centre + self.half_width).floor() >= self.received as f64 {
                break;
            }
            out.push(self.sample(centre));
            self.produced += 1;
        }
        // Input before the next sample's window is no longer needed.
        let centre = self.produced as f64 / self.ratio;
        let needed = (centre - self.half_width).ceil().max(0.0) as u64;
        if needed > self.start {
            let drop = ((needed - self.start) as usize).min(self.buffer.len());
            self.buffer.drain(..drop);
            self.start += drop as u64;
        }
    }

    /// Appends the rest of the output, once the input has ended: the output
    /// covers the input's duration, rounded up.
    pub fn finish(&mut self, out: &mut Vec<f32>) {
        if self.passthrough() {
            return;
        }
        let total = (self.received * self.to).div_ceil(self.from);
        while self.produced < total {
            let centre = self.produced as f64 / self.ratio;
            out.push(self.sample(centre));
            self.produced += 1;
        }
    }

    /// The output sample centred on input position `centre`, from the
    /// inputs that exist.
    fn sample(&self, centre: f64) -> f32 {
        if self.received == 0 {
            return 0.0;
        }
        let first = (centre - self.half_width).ceil().max(0.0) as u64;
        let end = ((centre + self.half_width).floor().max(0.0) as u64).min(self.received - 1);
        let sum: f64 = (first.max(self.start)..=end)
            .map(|k| {
                let offset = centre - k as f64;
                let x = 2.0 * self.cutoff * offset;
                let sinc = if x == 0.0 {
                    1.0
                } else {
                    (std::f64::consts::PI * x).sin() / (std::f64::consts::PI * x)
                };
                let weight = 2.0 * self.cutoff * sinc * blackman(offset / self.half_width);
                weight * f64::from(self.buffer[(k - self.start) as usize])
            })
            .sum();
        sum as f32
    }
}

/// The resampling filter passes this fraction of the lower Nyquist limit.
const RESAMPLE_PASSBAND: f64 = 0.95;
/// Zero crossings of the sinc on each side of the centre.
const RESAMPLE_ZERO_CROSSINGS: f64 = 16.0;

/// A Blackman window over −1..1 (zero outside).
fn blackman(x: f64) -> f64 {
    if x.abs() >= 1.0 {
        return 0.0;
    }
    let phase = std::f64::consts::PI * (x + 1.0);
    0.42 - 0.5 * phase.cos() + 0.08 * (2.0 * phase).cos()
}

/// Scales `samples` so the loudest reaches `peak` (all zero when the input
/// is silent).
pub fn normalize(samples: &[f64], peak: f64) -> Vec<f32> {
    let loudest = samples
        .iter()
        .fold(0.0_f64, |loudest, s| loudest.max(s.abs()));
    let scale = if loudest > 0.0 { peak / loudest } else { 0.0 };
    samples.iter().map(|s| (s * scale) as f32).collect()
}

/// 16-bit mono PCM samples in a WAV file. Samples outside −1..1 are clipped.
pub fn wav(samples: &[f32], sample_rate: u32) -> Vec<u8> {
    let data_len = (samples.len() * 2) as u32;
    let mut bytes = Vec::with_capacity(44 + data_len as usize);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes()); // PCM
    bytes.extend_from_slice(&1u16.to_le_bytes()); // mono
    bytes.extend_from_slice(&sample_rate.to_le_bytes());
    bytes.extend_from_slice(&(sample_rate * 2).to_le_bytes()); // bytes per second
    bytes.extend_from_slice(&2u16.to_le_bytes()); // block align
    bytes.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_len.to_le_bytes());
    for sample in samples {
        let value = (sample.clamp(-1.0, 1.0) * f32::from(i16::MAX)) as i16;
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pcm_knows_its_length() {
        let pcm = Pcm {
            sample_rate: 8_000,
            samples: vec![0.0; 4_000],
        };
        assert_eq!(pcm.seconds(), 0.5);
        assert_eq!(pcm.wav().len(), 44 + 8_000);
    }

    fn sine(hz: f64, sample_rate: u32, seconds: f64) -> Pcm {
        let count = (seconds * f64::from(sample_rate)) as usize;
        Pcm {
            sample_rate,
            samples: (0..count)
                .map(|n| {
                    let t = n as f64 / f64::from(sample_rate);
                    (0.5 * (2.0 * std::f64::consts::PI * hz * t).sin()) as f32
                })
                .collect(),
        }
    }

    fn rms(samples: &[f32]) -> f64 {
        (samples.iter().map(|s| f64::from(*s).powi(2)).sum::<f64>() / samples.len() as f64).sqrt()
    }

    #[test]
    fn resampling_keeps_the_sound() {
        let speech_rate = sine(440.0, 22_050, 0.5);
        let up = speech_rate.resampled(48_000);
        assert_eq!(up.sample_rate, 48_000);
        assert_eq!(up.samples.len(), 24_000);
        // Away from the edges, the result is the same sine at the new rate.
        let expected = sine(440.0, 48_000, 0.5);
        let error = up.samples[2_000..22_000]
            .iter()
            .zip(&expected.samples[2_000..22_000])
            .map(|(a, b)| f64::from(a - b).abs())
            .fold(0.0, f64::max);
        assert!(error < 1e-3, "{error}");
        assert_eq!(speech_rate.resampled(22_050), speech_rate);
    }

    #[test]
    fn resampling_in_pieces_matches_resampling_the_whole() {
        for (from, to) in [(24_000, 48_000), (22_050, 44_100), (48_000, 16_000)] {
            let whole = sine(440.0, from, 0.3);
            let expected = whole.resampled(to);
            let mut resampler = Resampler::new(from, to);
            let mut pieces = Vec::new();
            // Uneven pieces, some smaller than the filter.
            let mut rest = whole.samples.as_slice();
            for size in [1, 7, 30, 500, 3, 2_048].iter().cycle() {
                if rest.is_empty() {
                    break;
                }
                let (piece, tail) = rest.split_at((*size).min(rest.len()));
                resampler.push(piece, &mut pieces);
                rest = tail;
            }
            resampler.finish(&mut pieces);
            assert_eq!(pieces, expected.samples, "{from} → {to}");
        }
    }

    #[test]
    fn lowering_the_rate_filters_what_it_cannot_hold() {
        // 6 kHz cannot exist at 8 kHz (its limit is 4 kHz): removed, not
        // folded back to 2 kHz. 1 kHz passes.
        let high = sine(6_000.0, 48_000, 0.25).resampled(8_000);
        let low = sine(1_000.0, 48_000, 0.25).resampled(8_000);
        assert_eq!(low.samples.len(), 2_000);
        let middle = |pcm: &Pcm| rms(&pcm.samples[200..1_800]);
        assert!(middle(&high) < 0.01 * middle(&low), "{}", middle(&high));
        assert!((middle(&low) - 0.5 / 2f64.sqrt()).abs() < 0.01);
    }

    #[test]
    fn normalizes_to_the_peak() {
        let samples = normalize(&[0.5, -2.0, 1.0], 0.8);
        assert_eq!(samples, vec![0.2, -0.8, 0.4]);
        assert_eq!(normalize(&[0.0, 0.0], 0.8), vec![0.0, 0.0]);
        assert!(normalize(&[], 0.8).is_empty());
    }

    #[test]
    fn writes_a_wav_header_and_clipped_samples() {
        let bytes = wav(&[0.0, 1.0, -2.0], 22_050);
        assert_eq!(&bytes[..4], b"RIFF");
        assert_eq!(&bytes[8..16], b"WAVEfmt ");
        assert_eq!(
            u32::from_le_bytes(bytes[24..28].try_into().unwrap()),
            22_050
        );
        assert_eq!(&bytes[36..40], b"data");
        assert_eq!(bytes.len(), 44 + 3 * 2);
        let sample = |i: usize| i16::from_le_bytes([bytes[44 + 2 * i], bytes[45 + 2 * i]]);
        assert_eq!((sample(0), sample(1), sample(2)), (0, i16::MAX, -i16::MAX));
    }
}
