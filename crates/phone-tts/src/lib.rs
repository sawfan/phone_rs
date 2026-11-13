//! Text-to-speech for phone.
//!
//! Speech is made by a backend behind [`TextToSpeech`] and handed out in
//! pieces ([`Pcm`] chunks of about [`CHUNK_SECONDS`]) as the backend
//! produces it, so playback can start before the whole reply exists and
//! can be stopped part way (the callback returns [`ControlFlow::Break`]).
//! [`Tts`] builds the configured backend. [`Tts::speak`] asks for one
//! sentence at a time ([`sentences::split`]), so the first sound comes
//! after the first sentence even from servers that synthesize a large
//! block before sending any; [`Tts::speak_to_pcm`] asks for a whole reply
//! at once (best intonation, for files).
//!
//! Backends:
//!
//! - `openai_compatible` (feature `openai`, default): any server
//!   implementing `POST /v1/audio/speech` (Kokoro-FastAPI, openedai-speech
//!   for Piper, LocalAI, OpenAI), with WAV or raw PCM responses decoded as
//!   they stream.
//!
//! phone's own formant voice (IPA in `ipa-core`) is not a backend here: it
//! speaks transcriptions, not text.

pub mod config;
pub mod decode;
mod error;
#[cfg(feature = "openai")]
pub mod openai;
pub mod sentences;

use std::ops::ControlFlow;

use phone_core::Pcm;
use serde::{Deserialize, Serialize};

pub use config::{AudioFormat, BackendConfig, OpenAiSpeechConfig, TtsConfig};
pub use error::TtsError;

/// The longest text spoken in one request (OpenAI's limit).
pub const MAX_TEXT_CHARS: usize = 4_096;
/// About how much speech each chunk holds, in seconds.
pub const CHUNK_SECONDS: f64 = 0.1;
/// Speaking speeds accepted (1 is normal).
pub const MIN_SPEED: f64 = 0.25;
pub const MAX_SPEED: f64 = 4.0;
/// The longest voice name accepted.
const MAX_VOICE_CHARS: usize = 64;

/// How to say it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SpeakOptions {
    /// A voice the backend knows; its configured voice when absent.
    #[serde(default)]
    pub voice: Option<String>,
    /// Speaking speed, 0.25 to 4 (1 is normal).
    #[serde(default)]
    pub speed: Option<f64>,
}

impl SpeakOptions {
    pub fn validate(&self) -> Result<(), TtsError> {
        if let Some(voice) = &self.voice
            && (voice.trim().is_empty()
                || voice.chars().count() > MAX_VOICE_CHARS
                || voice.chars().any(char::is_control))
        {
            return Err(TtsError::Input(format!(
                "a voice is a name of 1 to {MAX_VOICE_CHARS} characters, not {voice:?}"
            )));
        }
        if let Some(speed) = self.speed
            && !(MIN_SPEED..=MAX_SPEED).contains(&speed)
        {
            return Err(TtsError::Input(format!(
                "speed must be from {MIN_SPEED} to {MAX_SPEED}, not {speed}"
            )));
        }
        Ok(())
    }
}

/// Checks text before it is sent: something to say, within
/// [`MAX_TEXT_CHARS`].
pub fn validate_text(text: &str) -> Result<(), TtsError> {
    if text.trim().is_empty() {
        return Err(TtsError::Input("there is no text to say".to_string()));
    }
    let length = text.chars().count();
    if length > MAX_TEXT_CHARS {
        return Err(TtsError::Input(format!(
            "the text has {length} characters; at most {MAX_TEXT_CHARS} can be said at once"
        )));
    }
    Ok(())
}

/// Which backend answers, for display.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct BackendInfo {
    pub kind: String,
    pub model: String,
    /// The voice used when a request names none.
    pub voice: String,
    pub endpoint: Option<String>,
}

/// What was said.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Spoken {
    pub sample_rate: u32,
    pub seconds: f64,
    /// The caller stopped the speech before its end.
    pub stopped: bool,
}

/// A speech backend. Implementations block while the backend works.
pub trait TextToSpeech: Send + Sync {
    fn info(&self) -> BackendInfo;

    /// Says `text`, handing each piece of speech to `chunk` as it arrives;
    /// `chunk` returning `Break` stops the speech. Text and options are
    /// checked first.
    fn speak(
        &self,
        text: &str,
        options: &SpeakOptions,
        chunk: &mut dyn FnMut(Pcm) -> ControlFlow<()>,
    ) -> Result<Spoken, TtsError>;
}

/// The configured speech backend.
pub struct Tts {
    backend: Box<dyn TextToSpeech>,
}

impl std::fmt::Debug for Tts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tts")
            .field("backend", &self.backend.info())
            .finish()
    }
}

impl Tts {
    pub fn new(backend: Box<dyn TextToSpeech>) -> Tts {
        Tts { backend }
    }

    /// The backend `config` describes.
    pub fn from_config(config: &TtsConfig) -> Result<Tts, TtsError> {
        config.validate()?;
        match &config.backend {
            #[cfg(feature = "openai")]
            BackendConfig::OpenAiCompatible(speech) => Ok(Tts::new(Box::new(
                openai::OpenAiCompatible::new(speech.clone())?,
            ))),
            #[cfg(not(feature = "openai"))]
            BackendConfig::OpenAiCompatible(_) => Err(TtsError::Config(
                "this build has no openai_compatible backend (feature `openai`)".to_string(),
            )),
        }
    }

    /// The backend `PHONE_TTS_*` variables describe.
    pub fn from_env() -> Result<Tts, TtsError> {
        Tts::from_config(&TtsConfig::from_env()?)
    }

    pub fn info(&self) -> BackendInfo {
        self.backend.info()
    }

    /// Says `text` one sentence at a time, each asked for once the one
    /// before has arrived, handing every chunk to `chunk` (see
    /// [`TextToSpeech::speak`]). Stopping ends the whole reply.
    pub fn speak(
        &self,
        text: &str,
        options: &SpeakOptions,
        chunk: &mut dyn FnMut(Pcm) -> ControlFlow<()>,
    ) -> Result<Spoken, TtsError> {
        validate_text(text)?;
        options.validate()?;
        let mut total = Spoken {
            sample_rate: 0,
            seconds: 0.0,
            stopped: false,
        };
        for piece in sentences::split(text) {
            let mut mismatch = None;
            let spoken = self.backend.speak(&piece, options, &mut |pcm| {
                if total.sample_rate != 0 && pcm.sample_rate != total.sample_rate {
                    mismatch = Some(pcm.sample_rate);
                    return ControlFlow::Break(());
                }
                chunk(pcm)
            })?;
            if let Some(rate) = mismatch {
                return Err(TtsError::Backend {
                    backend: self.backend.info().kind,
                    message: format!(
                        "the speech changed sample rate part way ({} Hz, then {rate} Hz)",
                        total.sample_rate
                    ),
                });
            }
            if total.sample_rate == 0 {
                total.sample_rate = spoken.sample_rate;
            }
            total.seconds += spoken.seconds;
            if spoken.stopped {
                total.stopped = true;
                break;
            }
        }
        Ok(total)
    }

    /// Says `text` in one request and returns the whole of it.
    pub fn speak_to_pcm(&self, text: &str, options: &SpeakOptions) -> Result<Pcm, TtsError> {
        let mut samples = Vec::new();
        let spoken = self.backend.speak(text, options, &mut |piece| {
            samples.extend_from_slice(&piece.samples);
            ControlFlow::Continue(())
        })?;
        Ok(Pcm {
            sample_rate: spoken.sample_rate,
            samples,
        })
    }
}

/// Groups decoded samples into chunks of about [`CHUNK_SECONDS`] for a
/// [`TextToSpeech::speak`] callback. (Unused in builds without a backend.)
#[cfg_attr(not(feature = "openai"), allow(dead_code))]
pub(crate) struct Chunker<'a> {
    chunk: &'a mut dyn FnMut(Pcm) -> ControlFlow<()>,
    buffer: Vec<f32>,
    pub(crate) frames: u64,
    pub(crate) stopped: bool,
}

#[cfg_attr(not(feature = "openai"), allow(dead_code))]
impl<'a> Chunker<'a> {
    pub(crate) fn new(chunk: &'a mut dyn FnMut(Pcm) -> ControlFlow<()>) -> Self {
        Chunker {
            chunk,
            buffer: Vec::new(),
            frames: 0,
            stopped: false,
        }
    }

    /// Takes samples at `sample_rate`; hands over full chunks. Returns
    /// false once the caller has stopped.
    pub(crate) fn push(&mut self, samples: &[f32], sample_rate: u32) -> bool {
        self.buffer.extend_from_slice(samples);
        let size = (f64::from(sample_rate) * CHUNK_SECONDS).ceil() as usize;
        while !self.stopped && self.buffer.len() >= size {
            let rest = self.buffer.split_off(size);
            let piece = std::mem::replace(&mut self.buffer, rest);
            self.hand_over(piece, sample_rate);
        }
        !self.stopped
    }

    /// Hands over what is left.
    pub(crate) fn finish(&mut self, sample_rate: u32) {
        if !self.stopped && !self.buffer.is_empty() {
            let piece = std::mem::take(&mut self.buffer);
            self.hand_over(piece, sample_rate);
        }
    }

    fn hand_over(&mut self, samples: Vec<f32>, sample_rate: u32) {
        self.frames += samples.len() as u64;
        let pcm = Pcm {
            sample_rate,
            samples,
        };
        if (self.chunk)(pcm).is_break() {
            self.stopped = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checks_text_and_options() {
        assert!(validate_text("Hello.").is_ok());
        assert!(validate_text("  \n").is_err());
        assert!(validate_text(&"a".repeat(MAX_TEXT_CHARS + 1)).is_err());
        let options = |voice: Option<&str>, speed| SpeakOptions {
            voice: voice.map(str::to_string),
            speed,
        };
        assert!(options(Some("af_heart"), Some(1.2)).validate().is_ok());
        assert!(options(Some(""), None).validate().is_err());
        assert!(options(Some("a\nb"), None).validate().is_err());
        assert!(options(None, Some(9.0)).validate().is_err());
        assert!(SpeakOptions::default().validate().is_ok());
    }

    /// Records each request and answers with 0.1 s of silence per word, in
    /// one chunk, at `rate`.
    struct Recorder {
        requests: std::sync::Mutex<Vec<String>>,
        rate: std::sync::Mutex<Vec<u32>>,
    }

    impl TextToSpeech for Recorder {
        fn info(&self) -> BackendInfo {
            BackendInfo {
                kind: "recorder".to_string(),
                model: "m".to_string(),
                voice: "v".to_string(),
                endpoint: None,
            }
        }

        fn speak(
            &self,
            text: &str,
            _options: &SpeakOptions,
            chunk: &mut dyn FnMut(Pcm) -> ControlFlow<()>,
        ) -> Result<Spoken, TtsError> {
            self.requests.lock().unwrap().push(text.to_string());
            let rate = self.rate.lock().unwrap().pop().unwrap_or(1_000);
            let words = text.split_whitespace().count();
            let stopped = chunk(Pcm {
                sample_rate: rate,
                samples: vec![0.0; words * rate as usize / 10],
            })
            .is_break();
            Ok(Spoken {
                sample_rate: rate,
                seconds: words as f64 / 10.0,
                stopped,
            })
        }
    }

    fn recorder(rates: Vec<u32>) -> (Tts, std::sync::Arc<Recorder>) {
        struct Shared(std::sync::Arc<Recorder>);
        impl TextToSpeech for Shared {
            fn info(&self) -> BackendInfo {
                self.0.info()
            }
            fn speak(
                &self,
                text: &str,
                options: &SpeakOptions,
                chunk: &mut dyn FnMut(Pcm) -> ControlFlow<()>,
            ) -> Result<Spoken, TtsError> {
                self.0.speak(text, options, chunk)
            }
        }
        let inner = std::sync::Arc::new(Recorder {
            requests: std::sync::Mutex::new(Vec::new()),
            rate: std::sync::Mutex::new(rates),
        });
        (
            Tts::new(Box::new(Shared(std::sync::Arc::clone(&inner)))),
            inner,
        )
    }

    const TEXT: &str = "Mnemosyne was the Titaness of memory. She was the mother of the Muses. \
                        Poets called on her before they began.";

    #[test]
    fn speaks_a_sentence_at_a_time_but_files_whole() {
        let (tts, backend) = recorder(Vec::new());
        let mut chunks = 0;
        let spoken = tts
            .speak(TEXT, &SpeakOptions::default(), &mut |_| {
                chunks += 1;
                ControlFlow::Continue(())
            })
            .unwrap();
        assert_eq!(chunks, 3);
        assert_eq!(backend.requests.lock().unwrap().len(), 3);
        assert_eq!(
            backend.requests.lock().unwrap()[0],
            "Mnemosyne was the Titaness of memory."
        );
        // 6 + 7 + 7 words at 0.1 s each.
        assert!((spoken.seconds - 2.0).abs() < 1e-9, "{}", spoken.seconds);
        assert!(!spoken.stopped);

        backend.requests.lock().unwrap().clear();
        let pcm = tts.speak_to_pcm(TEXT, &SpeakOptions::default()).unwrap();
        assert_eq!(backend.requests.lock().unwrap().len(), 1);
        assert_eq!(pcm.samples.len(), 2_000);
    }

    #[test]
    fn stopping_ends_the_whole_reply() {
        let (tts, backend) = recorder(Vec::new());
        let spoken = tts
            .speak(TEXT, &SpeakOptions::default(), &mut |_| {
                ControlFlow::Break(())
            })
            .unwrap();
        assert!(spoken.stopped);
        assert_eq!(backend.requests.lock().unwrap().len(), 1);
        // Nothing is asked for text that cannot be said.
        assert!(
            tts.speak(" ", &SpeakOptions::default(), &mut |_| {
                ControlFlow::Continue(())
            })
            .is_err()
        );
        assert_eq!(backend.requests.lock().unwrap().len(), 1);
    }

    #[test]
    fn a_change_of_rate_part_way_is_an_error() {
        // Rates are popped: the first piece at 1000 Hz, the second at 2000.
        let (tts, _backend) = recorder(vec![2_000, 1_000]);
        let error = tts
            .speak(TEXT, &SpeakOptions::default(), &mut |_| {
                ControlFlow::Continue(())
            })
            .unwrap_err();
        assert!(error.to_string().contains("changed sample rate"), "{error}");
    }

    #[test]
    fn chunks_samples_and_stops_when_asked() {
        let mut seen = Vec::new();
        let mut callback = |pcm: Pcm| {
            seen.push(pcm.samples.len());
            if seen.len() == 3 {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        };
        let mut chunker = Chunker::new(&mut callback);
        // 0.1 s at 1000 Hz is 100 samples.
        assert!(chunker.push(&[0.0; 150], 1_000));
        assert!(chunker.push(&[0.0; 30], 1_000));
        assert!(!chunker.push(&[0.0; 500], 1_000));
        chunker.finish(1_000);
        assert!(chunker.stopped);
        assert_eq!(chunker.frames, 300);
        drop(chunker);
        assert_eq!(seen, vec![100, 100, 100]);
    }
}
