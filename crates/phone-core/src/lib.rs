//! Sound synthesis for Mnemosyne.
//!
//! Phone makes sound; the modules that need it decide what to say or play.
//! Everything here produces samples and knows nothing about where they are
//! played (a WAV file, a sound card, or a browser), so the same rendering
//! serves the command line, MCP, native playback, and wasm.
//!
//! - [`formant`]: a Klatt-style formant voice driven by keyframes (used by
//!   `ipa-core` to speak transcriptions).
//! - [`instrument`]: how a note sounds (waveform, envelope, tone filter)
//!   and named presets.
//! - [`tone`]: notes as frequency and time, arranged (sequences, chords)
//!   and rendered with FunDSP oscillators and filters.
//! - [`pcm`]: rendered samples ([`Pcm`]), level normalization, and WAV
//!   encoding.
//! - [`dto`]: results shared by adapters (the preset catalog, written
//!   files).

pub mod dto;
pub mod formant;
pub mod instrument;
pub mod pcm;
pub mod tone;

pub use instrument::{Envelope, Preset, Sound, SoundChanges, Waveform};
pub use pcm::Pcm;
pub use tone::{Tone, ToneError};
