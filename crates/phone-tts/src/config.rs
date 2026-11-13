//! Which backend to use and how to reach it.
//!
//! Configuration is plain serde data, so a host can embed it in its own
//! file (Mnemosyne: TOML `[phone.tts.backend]` with
//! `kind = "openai_compatible"`), and [`TtsConfig::from_env`] reads it from
//! `PHONE_TTS_*` variables for the standalone tools. Credentials are never
//! stored here: only the name of the environment variable holding them.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::error::TtsError;

/// The default request path of the OpenAI speech API.
pub const DEFAULT_SPEECH_PATH: &str = "/v1/audio/speech";
/// The model and voice sent when none is configured (OpenAI's names; local
/// servers usually need their own, such as Kokoro's `kokoro` and
/// `af_heart`).
pub const DEFAULT_MODEL: &str = "tts-1";
pub const DEFAULT_VOICE: &str = "alloy";
/// The rate of raw PCM from OpenAI and Kokoro.
pub const DEFAULT_PCM_SAMPLE_RATE: u32 = 24_000;
pub const DEFAULT_TIMEOUT_SECONDS: u64 = 120;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct TtsConfig {
    pub backend: BackendConfig,
}

/// A speech backend and its settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BackendConfig {
    /// A server implementing the OpenAI speech API (`POST
    /// /v1/audio/speech`): Kokoro-FastAPI, openedai-speech (Piper and
    /// others), LocalAI, or OpenAI. The model runs wherever that server
    /// runs.
    #[serde(rename = "openai_compatible")]
    OpenAiCompatible(OpenAiSpeechConfig),
}

impl BackendConfig {
    /// The backend kind's identifier, as in the `kind` field.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::OpenAiCompatible(_) => "openai_compatible",
        }
    }
}

/// How the server sends audio.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum AudioFormat {
    /// A WAV stream: the rate and channels come from its header.
    #[default]
    Wav,
    /// Raw 16-bit little-endian mono samples at the configured
    /// `sample_rate`.
    Pcm,
}

impl AudioFormat {
    /// The name the speech API uses (`response_format`).
    pub fn id(self) -> &'static str {
        match self {
            Self::Wav => "wav",
            Self::Pcm => "pcm",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct OpenAiSpeechConfig {
    /// The server's base URL, such as "http://127.0.0.1:8880".
    pub base_url: String,
    /// The request path under the base URL.
    #[serde(default = "default_path")]
    pub path: String,
    /// The model name sent with each request.
    #[serde(default = "default_model")]
    pub model: String,
    /// The voice used when a request names none.
    #[serde(default = "default_voice")]
    pub voice: String,
    #[serde(default)]
    pub format: AudioFormat,
    /// The rate of raw PCM (`format = "pcm"`); WAV carries its own.
    #[serde(default = "default_pcm_sample_rate")]
    pub sample_rate: u32,
    /// The name of the environment variable holding a bearer token, for
    /// servers that need one.
    #[serde(default)]
    pub api_key_env: Option<String>,
    /// The longest a request may take, speech included.
    #[serde(default = "default_timeout_seconds")]
    pub timeout_seconds: u64,
}

fn default_path() -> String {
    DEFAULT_SPEECH_PATH.to_string()
}

fn default_model() -> String {
    DEFAULT_MODEL.to_string()
}

fn default_voice() -> String {
    DEFAULT_VOICE.to_string()
}

fn default_pcm_sample_rate() -> u32 {
    DEFAULT_PCM_SAMPLE_RATE
}

fn default_timeout_seconds() -> u64 {
    DEFAULT_TIMEOUT_SECONDS
}

impl OpenAiSpeechConfig {
    /// A configuration for the server at `base_url` with every other
    /// setting at its default.
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            path: default_path(),
            model: default_model(),
            voice: default_voice(),
            format: AudioFormat::default(),
            sample_rate: DEFAULT_PCM_SAMPLE_RATE,
            api_key_env: None,
            timeout_seconds: DEFAULT_TIMEOUT_SECONDS,
        }
    }

    /// The full request URL.
    pub fn url(&self) -> String {
        format!(
            "{}/{}",
            self.base_url.trim_end_matches('/'),
            self.path.trim_start_matches('/')
        )
    }

    pub fn timeout(&self) -> Duration {
        Duration::from_secs(self.timeout_seconds)
    }

    pub fn validate(&self) -> Result<(), TtsError> {
        let invalid = |message: String| Err(TtsError::Config(message));
        if !(self.base_url.starts_with("http://") || self.base_url.starts_with("https://")) {
            return invalid(format!(
                "base_url must start with http:// or https://, not {:?}",
                self.base_url
            ));
        }
        if self.path.contains(['?', '#']) || self.path.contains(char::is_whitespace) {
            return invalid(format!(
                "path must be a plain URL path, not {:?}",
                self.path
            ));
        }
        if self.model.trim().is_empty() {
            return invalid("model must not be empty".to_string());
        }
        if self.voice.trim().is_empty() {
            return invalid("voice must not be empty".to_string());
        }
        if !(phone_core::tone::MIN_SAMPLE_RATE..=phone_core::tone::MAX_SAMPLE_RATE)
            .contains(&self.sample_rate)
        {
            return invalid(format!(
                "sample_rate must be from {} to {} Hz, not {}",
                phone_core::tone::MIN_SAMPLE_RATE,
                phone_core::tone::MAX_SAMPLE_RATE,
                self.sample_rate
            ));
        }
        if self.api_key_env.as_deref().is_some_and(str::is_empty) {
            return invalid("api_key_env must name a variable when set".to_string());
        }
        if self.timeout_seconds == 0 {
            return invalid("timeout_seconds must be positive".to_string());
        }
        Ok(())
    }
}

/// `value` of the variable `name` as a whole number.
fn number<T: std::str::FromStr>(name: &str, value: String) -> Result<T, TtsError> {
    value
        .parse()
        .map_err(|_| TtsError::Config(format!("{name} must be a whole number, not {value:?}")))
}

impl TtsConfig {
    /// Reads the configuration from the process environment:
    ///
    /// - `PHONE_TTS_BACKEND`: the backend kind (only `openai_compatible`,
    ///   the default);
    /// - `PHONE_TTS_BASE_URL` (required), `PHONE_TTS_PATH`,
    ///   `PHONE_TTS_MODEL`, `PHONE_TTS_VOICE`, `PHONE_TTS_FORMAT` (`wav` or
    ///   `pcm`), `PHONE_TTS_SAMPLE_RATE`, `PHONE_TTS_API_KEY_ENV`,
    ///   `PHONE_TTS_TIMEOUT_SECONDS`: its settings.
    pub fn from_env() -> Result<Self, TtsError> {
        Self::from_lookup(|name| std::env::var(name).ok())
    }

    /// [`from_env`](Self::from_env) with variables from `lookup`.
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Result<Self, TtsError> {
        let var = |name: &str| lookup(name).filter(|value| !value.is_empty());
        let kind = var("PHONE_TTS_BACKEND").unwrap_or_else(|| "openai_compatible".to_string());
        let backend = match kind.as_str() {
            "openai_compatible" => {
                let base_url = var("PHONE_TTS_BASE_URL").ok_or_else(|| {
                    TtsError::Config(
                        "set PHONE_TTS_BASE_URL to the speech server, such as http://127.0.0.1:8880"
                            .to_string(),
                    )
                })?;
                let mut config = OpenAiSpeechConfig::new(base_url);
                if let Some(path) = var("PHONE_TTS_PATH") {
                    config.path = path;
                }
                if let Some(model) = var("PHONE_TTS_MODEL") {
                    config.model = model;
                }
                if let Some(voice) = var("PHONE_TTS_VOICE") {
                    config.voice = voice;
                }
                if let Some(format) = var("PHONE_TTS_FORMAT") {
                    config.format = match format.as_str() {
                        "wav" => AudioFormat::Wav,
                        "pcm" => AudioFormat::Pcm,
                        other => {
                            return Err(TtsError::Config(format!(
                                "PHONE_TTS_FORMAT must be wav or pcm, not {other:?}"
                            )));
                        }
                    };
                }
                if let Some(rate) = var("PHONE_TTS_SAMPLE_RATE") {
                    config.sample_rate = number("PHONE_TTS_SAMPLE_RATE", rate)?;
                }
                config.api_key_env = var("PHONE_TTS_API_KEY_ENV");
                if let Some(timeout) = var("PHONE_TTS_TIMEOUT_SECONDS") {
                    config.timeout_seconds = number("PHONE_TTS_TIMEOUT_SECONDS", timeout)?;
                }
                BackendConfig::OpenAiCompatible(config)
            }
            other => {
                return Err(TtsError::Config(format!(
                    "unknown PHONE_TTS_BACKEND {other:?}; the known backend is openai_compatible"
                )));
            }
        };
        let config = Self { backend };
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), TtsError> {
        match &self.backend {
            BackendConfig::OpenAiCompatible(config) => config.validate(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn lookup(vars: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let vars: HashMap<String, String> = vars
            .iter()
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .collect();
        move |name| vars.get(name).cloned()
    }

    #[test]
    fn reads_the_environment_with_defaults() {
        let config =
            TtsConfig::from_lookup(lookup(&[("PHONE_TTS_BASE_URL", "http://127.0.0.1:8880/")]))
                .unwrap();
        let BackendConfig::OpenAiCompatible(speech) = config.backend;
        assert_eq!(speech.url(), "http://127.0.0.1:8880/v1/audio/speech");
        assert_eq!(
            (speech.model.as_str(), speech.voice.as_str()),
            ("tts-1", "alloy")
        );
        assert_eq!(speech.format, AudioFormat::Wav);

        let config = TtsConfig::from_lookup(lookup(&[
            ("PHONE_TTS_BASE_URL", "http://localhost:8880"),
            ("PHONE_TTS_MODEL", "kokoro"),
            ("PHONE_TTS_VOICE", "af_heart"),
            ("PHONE_TTS_FORMAT", "pcm"),
            ("PHONE_TTS_SAMPLE_RATE", "22050"),
            ("PHONE_TTS_API_KEY_ENV", "KOKORO_KEY"),
        ]))
        .unwrap();
        let BackendConfig::OpenAiCompatible(speech) = config.backend;
        assert_eq!(
            (speech.format, speech.sample_rate),
            (AudioFormat::Pcm, 22_050)
        );
        assert_eq!(speech.api_key_env.as_deref(), Some("KOKORO_KEY"));
    }

    #[test]
    fn reports_what_to_fix() {
        let missing = TtsConfig::from_lookup(lookup(&[])).unwrap_err();
        assert!(missing.to_string().contains("PHONE_TTS_BASE_URL"));
        let bad = |vars: &[(&str, &str)]| TtsConfig::from_lookup(lookup(vars)).is_err();
        assert!(bad(&[("PHONE_TTS_BASE_URL", "localhost:8880")]));
        assert!(bad(&[
            ("PHONE_TTS_BASE_URL", "http://x"),
            ("PHONE_TTS_FORMAT", "mp3")
        ]));
        assert!(bad(&[
            ("PHONE_TTS_BASE_URL", "http://x"),
            ("PHONE_TTS_SAMPLE_RATE", "100")
        ]));
        assert!(bad(&[
            ("PHONE_TTS_BASE_URL", "http://x"),
            ("PHONE_TTS_BACKEND", "piper")
        ]));
    }

    #[test]
    fn reads_a_host_config_file_section() {
        let config: TtsConfig = serde_json::from_value(serde_json::json!({
            "backend": {
                "kind": "openai_compatible",
                "base_url": "http://127.0.0.1:8880",
                "model": "kokoro",
                "voice": "af_heart"
            }
        }))
        .unwrap();
        assert!(config.validate().is_ok());
        assert_eq!(config.backend.kind(), "openai_compatible");
    }
}
