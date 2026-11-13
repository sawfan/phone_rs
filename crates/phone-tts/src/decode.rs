//! Turning streamed audio bytes into samples as they arrive.
//!
//! [`StreamDecoder`] takes bytes in whatever pieces the network delivers
//! (a sample or a header may be split anywhere) and gives mono samples:
//! raw 16-bit PCM at a known rate, or a WAV stream whose header says the
//! rate and layout. Streamed WAV often has no usable length in its header,
//! so the data runs until the bytes end.

use std::fmt;

/// How the bytes are laid out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encoding {
    /// Little-endian signed 16-bit mono samples at this rate.
    Pcm16 { sample_rate: u32 },
    /// A RIFF/WAVE stream (16-bit integer or 32-bit float PCM, any number
    /// of channels, mixed down to mono).
    Wav,
}

/// Why bytes are not usable audio.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecodeError(pub String);

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for DecodeError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SampleFormat {
    Int16,
    Float32,
}

impl SampleFormat {
    fn bytes(self) -> usize {
        match self {
            Self::Int16 => 2,
            Self::Float32 => 4,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Layout {
    sample_rate: u32,
    channels: usize,
    format: SampleFormat,
}

#[derive(Clone, Debug)]
enum State {
    /// Waiting for the 12-byte RIFF header.
    Riff,
    /// Reading chunk headers; `layout` once `fmt ` has been read.
    Chunks { layout: Option<Layout> },
    /// Skipping the rest of a chunk that is not `fmt ` or `data`.
    Skip {
        remaining: usize,
        layout: Option<Layout>,
    },
    /// Sample frames until the end.
    Data(Layout),
}

/// A decoder fed bytes in pieces.
#[derive(Clone, Debug)]
pub struct StreamDecoder {
    state: State,
    /// Bytes not yet decoded: part of a header, or of a frame.
    pending: Vec<u8>,
}

/// The longest WAV header chunk read before the data (`fmt ` and its
/// neighbours are small; this bounds a hostile stream).
const MAX_HEADER_CHUNK_BYTES: usize = 64 * 1024;

impl StreamDecoder {
    pub fn new(encoding: Encoding) -> StreamDecoder {
        let state = match encoding {
            Encoding::Pcm16 { sample_rate } => State::Data(Layout {
                sample_rate,
                channels: 1,
                format: SampleFormat::Int16,
            }),
            Encoding::Wav => State::Riff,
        };
        StreamDecoder {
            state,
            pending: Vec::new(),
        }
    }

    /// The sample rate, once known (at once for raw PCM; after the WAV
    /// header otherwise).
    pub fn sample_rate(&self) -> Option<u32> {
        match self.state {
            State::Data(layout) => Some(layout.sample_rate),
            _ => None,
        }
    }

    /// Decodes `bytes`, appending the samples they complete to `out`.
    pub fn push(&mut self, bytes: &[u8], out: &mut Vec<f32>) -> Result<(), DecodeError> {
        self.pending.extend_from_slice(bytes);
        loop {
            match self.state.clone() {
                State::Riff => {
                    // Say so as soon as the start shows it is not WAV.
                    if self.pending.len() >= 4 && &self.pending[..4] != b"RIFF" {
                        return Err(DecodeError("not a WAV stream".to_string()));
                    }
                    if self.pending.len() < 12 {
                        return Ok(());
                    }
                    if &self.pending[..4] != b"RIFF" || &self.pending[8..12] != b"WAVE" {
                        return Err(DecodeError("not a WAV stream".to_string()));
                    }
                    self.pending.drain(..12);
                    self.state = State::Chunks { layout: None };
                }
                State::Chunks { layout } => {
                    if self.pending.len() < 8 {
                        return Ok(());
                    }
                    let id: [u8; 4] = self.pending[..4].try_into().expect("checked");
                    let size = u32::from_le_bytes(self.pending[4..8].try_into().expect("checked"))
                        as usize;
                    if &id == b"data" {
                        let layout = layout.ok_or_else(|| {
                            DecodeError("WAV data came before its format".to_string())
                        })?;
                        self.pending.drain(..8);
                        // The size is ignored: streamed WAV often has 0 or
                        // u32::MAX there.
                        self.state = State::Data(layout);
                        continue;
                    }
                    // Chunks are padded to an even length.
                    let padded = size + size % 2;
                    if &id == b"fmt " {
                        if size > MAX_HEADER_CHUNK_BYTES {
                            return Err(DecodeError("WAV format chunk is too long".to_string()));
                        }
                        if self.pending.len() < 8 + padded {
                            return Ok(());
                        }
                        let layout = parse_format(&self.pending[8..8 + size])?;
                        self.pending.drain(..8 + padded);
                        self.state = State::Chunks {
                            layout: Some(layout),
                        };
                    } else {
                        self.pending.drain(..8);
                        self.state = State::Skip {
                            remaining: padded,
                            layout,
                        };
                    }
                }
                State::Skip { remaining, layout } => {
                    let skipped = remaining.min(self.pending.len());
                    self.pending.drain(..skipped);
                    if skipped < remaining {
                        self.state = State::Skip {
                            remaining: remaining - skipped,
                            layout,
                        };
                        return Ok(());
                    }
                    self.state = State::Chunks { layout };
                }
                State::Data(layout) => {
                    let frame = layout.channels * layout.format.bytes();
                    let frames = self.pending.len() / frame;
                    out.reserve(frames);
                    for bytes in self.pending[..frames * frame].chunks_exact(frame) {
                        let sum: f32 = bytes
                            .chunks_exact(layout.format.bytes())
                            .map(|sample| match layout.format {
                                SampleFormat::Int16 => {
                                    f32::from(i16::from_le_bytes([sample[0], sample[1]])) / 32_768.0
                                }
                                SampleFormat::Float32 => {
                                    f32::from_le_bytes(sample.try_into().expect("four bytes"))
                                }
                            })
                            .sum();
                        out.push(sum / layout.channels as f32);
                    }
                    self.pending.drain(..frames * frame);
                    return Ok(());
                }
            }
        }
    }

    /// Checks that the bytes ended where audio may end: after the WAV
    /// header, on a whole frame.
    pub fn finish(&self) -> Result<(), DecodeError> {
        match self.state {
            State::Data(_) if self.pending.is_empty() => Ok(()),
            State::Data(_) => Err(DecodeError("the audio ended mid-sample".to_string())),
            _ => Err(DecodeError("the audio ended before its data".to_string())),
        }
    }
}

fn parse_format(chunk: &[u8]) -> Result<Layout, DecodeError> {
    if chunk.len() < 16 {
        return Err(DecodeError("WAV format chunk is too short".to_string()));
    }
    let u16_at = |at: usize| u16::from_le_bytes([chunk[at], chunk[at + 1]]);
    let mut tag = u16_at(0);
    let channels = usize::from(u16_at(2));
    let sample_rate = u32::from_le_bytes(chunk[4..8].try_into().expect("checked"));
    let bits = u16_at(14);
    // WAVE_FORMAT_EXTENSIBLE: the real format is in the sub-format GUID.
    if tag == 0xFFFE && chunk.len() >= 26 {
        tag = u16_at(24);
    }
    let format = match (tag, bits) {
        (1, 16) => SampleFormat::Int16,
        (3, 32) => SampleFormat::Float32,
        _ => {
            return Err(DecodeError(format!(
                "unsupported WAV encoding (format {tag}, {bits}-bit); use 16-bit or float PCM"
            )));
        }
    };
    if channels == 0 || channels > 8 {
        return Err(DecodeError(format!("unsupported channel count {channels}")));
    }
    if !(phone_core::tone::MIN_SAMPLE_RATE..=phone_core::tone::MAX_SAMPLE_RATE)
        .contains(&sample_rate)
    {
        return Err(DecodeError(format!(
            "unsupported sample rate {sample_rate}"
        )));
    }
    Ok(Layout {
        sample_rate,
        channels,
        format,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A streamed-style WAV: an extra chunk before the data, and a data
    /// size of u32::MAX.
    fn streamed_wav(samples: &[i16], channels: u16, rate: u32) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&u32::MAX.to_le_bytes());
        bytes.extend_from_slice(b"WAVEfmt ");
        bytes.extend_from_slice(&16u32.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&channels.to_le_bytes());
        bytes.extend_from_slice(&rate.to_le_bytes());
        bytes.extend_from_slice(&(rate * 2 * u32::from(channels)).to_le_bytes());
        bytes.extend_from_slice(&(2 * channels).to_le_bytes());
        bytes.extend_from_slice(&16u16.to_le_bytes());
        bytes.extend_from_slice(b"LIST");
        bytes.extend_from_slice(&3u32.to_le_bytes());
        bytes.extend_from_slice(b"abc\0");
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&u32::MAX.to_le_bytes());
        for sample in samples {
            bytes.extend_from_slice(&sample.to_le_bytes());
        }
        bytes
    }

    /// Decodes `bytes` fed `size` bytes at a time.
    fn decode(encoding: Encoding, bytes: &[u8], size: usize) -> (Vec<f32>, StreamDecoder) {
        let mut decoder = StreamDecoder::new(encoding);
        let mut out = Vec::new();
        for piece in bytes.chunks(size) {
            decoder.push(piece, &mut out).unwrap();
        }
        (out, decoder)
    }

    #[test]
    fn decodes_streamed_wav_split_anywhere() {
        let bytes = streamed_wav(&[16_384, -16_384, 0, 32_767], 1, 24_000);
        for size in [1, 3, 7, 44, 1_000] {
            let (samples, decoder) = decode(Encoding::Wav, &bytes, size);
            assert_eq!(samples, vec![0.5, -0.5, 0.0, 32_767.0 / 32_768.0], "{size}");
            assert_eq!(decoder.sample_rate(), Some(24_000));
            assert!(decoder.finish().is_ok());
        }
    }

    #[test]
    fn mixes_stereo_down_and_reads_raw_pcm() {
        let stereo = streamed_wav(&[16_384, 0, -16_384, -16_384], 2, 48_000);
        assert_eq!(decode(Encoding::Wav, &stereo, 5).0, vec![0.25, -0.5]);
        let raw: Vec<u8> = [8_192_i16, -8_192]
            .iter()
            .flat_map(|sample| sample.to_le_bytes())
            .collect();
        let (samples, decoder) = decode(
            Encoding::Pcm16 {
                sample_rate: 24_000,
            },
            &raw,
            1,
        );
        assert_eq!(samples, vec![0.25, -0.25]);
        assert_eq!(decoder.sample_rate(), Some(24_000));
    }

    #[test]
    fn rejects_what_it_cannot_play() {
        let mut decoder = StreamDecoder::new(Encoding::Wav);
        let mut out = Vec::new();
        assert!(decoder.push(b"ID3\x04 not a wav", &mut out).is_err());
        // Ending inside the header or mid-sample is reported.
        let bytes = streamed_wav(&[1, 2], 1, 24_000);
        let (_, decoder) = decode(Encoding::Wav, &bytes[..20], 4);
        assert!(decoder.finish().is_err());
        let (_, decoder) = decode(Encoding::Wav, &bytes[..bytes.len() - 1], 4);
        assert!(decoder.finish().is_err());
        // 8-bit audio is not supported.
        let mut eight_bit = streamed_wav(&[], 1, 24_000);
        eight_bit[34] = 8;
        let mut decoder = StreamDecoder::new(Encoding::Wav);
        let error = decoder.push(&eight_bit, &mut out).unwrap_err();
        assert!(error.0.contains("unsupported"), "{error}");
    }
}
