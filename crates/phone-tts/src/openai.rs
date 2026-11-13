//! A backend for servers implementing the OpenAI speech API
//! (`POST /v1/audio/speech`).
//!
//! The same request reaches Kokoro-FastAPI, openedai-speech (Piper and
//! others), LocalAI, and OpenAI. The response is read as it streams and
//! decoded piece by piece ([`crate::decode`]), so speech reaches the
//! caller while the server is still producing it.

use std::io::Read;
use std::ops::ControlFlow;

use phone_core::Pcm;
use serde::Serialize;

use crate::config::{AudioFormat, OpenAiSpeechConfig};
use crate::decode::{Encoding, StreamDecoder};
use crate::{BackendInfo, Chunker, SpeakOptions, Spoken, TextToSpeech, TtsError, validate_text};

pub const KIND: &str = "openai_compatible";

/// The most audio read from one response (about 40 minutes of 24 kHz
/// 16-bit mono).
const MAX_RESPONSE_BYTES: u64 = 128 * 1024 * 1024;
/// The largest error response read, and how much of it is quoted.
const MAX_ERROR_BYTES: u64 = 64 * 1024;
const MAX_ERROR_EXCERPT_CHARS: usize = 500;
/// Bytes read from the network at a time.
const READ_BYTES: usize = 8 * 1024;

pub struct OpenAiCompatible {
    config: OpenAiSpeechConfig,
    api_key: Option<String>,
    agent: ureq::Agent,
}

#[derive(Serialize)]
struct SpeechRequest<'a> {
    model: &'a str,
    input: &'a str,
    voice: &'a str,
    response_format: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    speed: Option<f64>,
}

impl OpenAiCompatible {
    /// Checks the configuration and reads the API key from the variable it
    /// names, if any.
    pub fn new(config: OpenAiSpeechConfig) -> Result<Self, TtsError> {
        let api_key = match &config.api_key_env {
            None => None,
            Some(name) => Some(
                std::env::var(name)
                    .ok()
                    .filter(|key| !key.is_empty())
                    .ok_or_else(|| {
                        TtsError::Config(format!(
                            "{name} (api_key_env) is not set to the speech server's API key"
                        ))
                    })?,
            ),
        };
        Self::with_api_key(config, api_key)
    }

    pub(crate) fn with_api_key(
        config: OpenAiSpeechConfig,
        api_key: Option<String>,
    ) -> Result<Self, TtsError> {
        config.validate()?;
        let agent = ureq::Agent::new_with_config(
            ureq::Agent::config_builder()
                .timeout_global(Some(config.timeout()))
                // Error statuses are read so the server's message can be
                // reported.
                .http_status_as_error(false)
                .build(),
        );
        Ok(Self {
            config,
            api_key,
            agent,
        })
    }

    fn failure(&self, message: impl Into<String>) -> TtsError {
        TtsError::Backend {
            backend: KIND.to_string(),
            message: message.into(),
        }
    }

    fn encoding(&self) -> Encoding {
        match self.config.format {
            AudioFormat::Wav => Encoding::Wav,
            AudioFormat::Pcm => Encoding::Pcm16 {
                sample_rate: self.config.sample_rate,
            },
        }
    }
}

impl std::fmt::Debug for OpenAiCompatible {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenAiCompatible")
            .field("config", &self.config)
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

impl TextToSpeech for OpenAiCompatible {
    fn info(&self) -> BackendInfo {
        BackendInfo {
            kind: KIND.to_string(),
            model: self.config.model.clone(),
            voice: self.config.voice.clone(),
            endpoint: Some(self.config.url()),
        }
    }

    fn speak(
        &self,
        text: &str,
        options: &SpeakOptions,
        chunk: &mut dyn FnMut(Pcm) -> ControlFlow<()>,
    ) -> Result<Spoken, TtsError> {
        validate_text(text)?;
        options.validate()?;
        let request = SpeechRequest {
            model: &self.config.model,
            input: text,
            voice: options.voice.as_deref().unwrap_or(&self.config.voice),
            response_format: self.config.format.id(),
            speed: options.speed,
        };
        let body = serde_json::to_vec(&request)
            .map_err(|error| self.failure(format!("failed to encode the request: {error}")))?;
        let url = self.config.url();
        let mut call = self
            .agent
            .post(&url)
            .header("Content-Type", "application/json");
        if let Some(key) = &self.api_key {
            call = call.header("Authorization", format!("Bearer {key}"));
        }
        let mut response = call
            .send(body.as_slice())
            .map_err(|error| self.failure(format!("request to {url} failed: {error}")))?;
        let status = response.status();
        if !status.is_success() {
            let body = response
                .body_mut()
                .with_config()
                .limit(MAX_ERROR_BYTES)
                .read_to_string()
                .unwrap_or_default();
            return Err(self.failure(format!(
                "HTTP {} from {url}: {}",
                status.as_u16(),
                excerpt(&body)
            )));
        }

        let mut reader = response
            .body_mut()
            .with_config()
            .limit(MAX_RESPONSE_BYTES)
            .reader();
        let mut decoder = StreamDecoder::new(self.encoding());
        let mut chunker = Chunker::new(chunk);
        let mut bytes = vec![0; READ_BYTES];
        let mut samples = Vec::new();
        loop {
            let read = reader
                .read(&mut bytes)
                .map_err(|error| self.failure(format!("failed to read the speech: {error}")))?;
            if read == 0 {
                break;
            }
            samples.clear();
            decoder
                .push(&bytes[..read], &mut samples)
                .map_err(|error| self.failure(format!("unusable audio: {error}")))?;
            if let Some(rate) = decoder.sample_rate()
                && !chunker.push(&samples, rate)
            {
                // Stopped by the caller: the rest is not read.
                break;
            }
        }
        let sample_rate = decoder
            .sample_rate()
            .ok_or_else(|| self.failure("the response held no audio"))?;
        if !chunker.stopped {
            decoder
                .finish()
                .map_err(|error| self.failure(format!("unusable audio: {error}")))?;
            chunker.finish(sample_rate);
        }
        Ok(Spoken {
            sample_rate,
            seconds: chunker.frames as f64 / f64::from(sample_rate),
            stopped: chunker.stopped,
        })
    }
}

fn excerpt(body: &str) -> String {
    let body = body.trim();
    if body.chars().count() <= MAX_ERROR_EXCERPT_CHARS {
        body.to_string()
    } else {
        let cut: String = body.chars().take(MAX_ERROR_EXCERPT_CHARS).collect();
        format!("{cut}…")
    }
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    use super::*;

    struct Captured {
        head: String,
        body: Vec<u8>,
    }

    /// Answers one request on a local port: sends `first` (after the
    /// response head), waits for `go` (up to a second), then sends `rest`
    /// and closes. Hands back what was sent and, through `released`,
    /// whether `go` came before the wait ran out.
    fn serve_with(
        status: u16,
        content_type: &'static str,
        first: Vec<u8>,
        rest: Vec<u8>,
        go: Option<mpsc::Receiver<()>>,
        released: Option<mpsc::Sender<bool>>,
    ) -> (String, mpsc::Receiver<Captured>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream);
            let mut head = String::new();
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                head.push_str(&line);
                if line == "\r\n" {
                    break;
                }
            }
            let length: usize = head
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse().unwrap())
                })
                .unwrap();
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            sender.send(Captured { head, body }).unwrap();
            let stream = reader.get_mut();
            // No Content-Length: the body runs until the connection closes,
            // as streamed speech does.
            let response = format!(
                "HTTP/1.1 {status} X\r\nContent-Type: {content_type}\r\nConnection: close\r\n\r\n"
            );
            stream.write_all(response.as_bytes()).unwrap();
            stream.write_all(&first).unwrap();
            stream.flush().unwrap();
            if let Some(go) = go {
                let arrived = go.recv_timeout(Duration::from_secs(1)).is_ok();
                if let Some(released) = released {
                    released.send(arrived).unwrap();
                }
            }
            let _ = stream.write_all(&rest);
        });
        (base_url, receiver)
    }

    fn serve(
        status: u16,
        content_type: &'static str,
        body: Vec<u8>,
    ) -> (String, mpsc::Receiver<Captured>) {
        serve_with(status, content_type, body, Vec::new(), None, None)
    }

    fn wav_header(rate: u32) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&u32::MAX.to_le_bytes());
        bytes.extend_from_slice(b"WAVEfmt ");
        bytes.extend_from_slice(&16u32.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&rate.to_le_bytes());
        bytes.extend_from_slice(&(rate * 2).to_le_bytes());
        bytes.extend_from_slice(&2u16.to_le_bytes());
        bytes.extend_from_slice(&16u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&u32::MAX.to_le_bytes());
        bytes
    }

    fn samples(count: usize, value: i16) -> Vec<u8> {
        (0..count).flat_map(|_| value.to_le_bytes()).collect()
    }

    fn backend(base_url: &str, format: AudioFormat, key: Option<&str>) -> OpenAiCompatible {
        let mut config = OpenAiSpeechConfig::new(base_url);
        config.model = "kokoro".to_string();
        config.voice = "af_heart".to_string();
        config.format = format;
        OpenAiCompatible::with_api_key(config, key.map(str::to_string)).unwrap()
    }

    fn header<'a>(head: &'a str, name: &str) -> Option<&'a str> {
        head.lines().find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.eq_ignore_ascii_case(name).then(|| value.trim())
        })
    }

    /// Speech reaches the caller while the server is still sending: the
    /// mock holds back the rest until the first chunk has arrived.
    #[test]
    fn streams_speech_as_it_arrives() {
        let (go_tx, go) = mpsc::channel();
        let mut first = wav_header(24_000);
        // 0.15 s: one full 0.1 s chunk and part of the next.
        first.extend(samples(3_600, 16_384));
        let rest = samples(2_400, -16_384);
        let (released_tx, released) = mpsc::channel();
        let (base_url, requests) =
            serve_with(200, "audio/wav", first, rest, Some(go), Some(released_tx));
        let tts = backend(&base_url, AudioFormat::Wav, Some("secret"));
        let mut chunks = Vec::new();
        let options = SpeakOptions {
            voice: Some("bf_emma".to_string()),
            speed: Some(1.25),
        };
        let spoken = tts
            .speak("Hello, Mnemosyne.", &options, &mut |pcm| {
                if chunks.is_empty() {
                    go_tx.send(()).unwrap();
                }
                chunks.push(pcm);
                ControlFlow::Continue(())
            })
            .unwrap();
        // The first chunk arrived while the server was still holding the
        // rest back.
        assert_eq!(released.recv(), Ok(true));
        assert_eq!(spoken.sample_rate, 24_000);
        assert!((spoken.seconds - 0.25).abs() < 1e-9);
        assert!(!spoken.stopped);
        assert_eq!(
            chunks
                .iter()
                .map(|pcm| pcm.samples.len())
                .collect::<Vec<_>>(),
            vec![2_400, 2_400, 1_200]
        );
        assert_eq!(chunks[0].samples[0], 0.5);
        assert_eq!(chunks[2].samples[0], -0.5);

        let request = requests.recv().unwrap();
        assert!(request.head.starts_with("POST /v1/audio/speech "));
        assert_eq!(
            header(&request.head, "authorization"),
            Some("Bearer secret")
        );
        let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
        assert_eq!(
            body,
            serde_json::json!({
                "model": "kokoro",
                "input": "Hello, Mnemosyne.",
                "voice": "bf_emma",
                "response_format": "wav",
                "speed": 1.25
            })
        );
    }

    #[test]
    fn reads_raw_pcm_and_stops_when_asked() {
        let (base_url, requests) = serve(200, "audio/pcm", samples(24_000, 8_192));
        let tts = backend(&base_url, AudioFormat::Pcm, None);
        let mut received = 0;
        let spoken = tts
            .speak("Stop me.", &SpeakOptions::default(), &mut |pcm| {
                received += pcm.samples.len();
                ControlFlow::Break(())
            })
            .unwrap();
        assert!(spoken.stopped);
        assert_eq!(received, 2_400);
        let request = requests.recv().unwrap();
        assert_eq!(header(&request.head, "authorization"), None);
        let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
        assert_eq!(body["voice"], "af_heart");
        assert_eq!(body["response_format"], "pcm");
        assert!(body.get("speed").is_none());
    }

    #[test]
    fn reports_server_errors_and_bad_audio() {
        let (base_url, _requests) = serve(
            400,
            "application/json",
            br#"{"error":{"message":"voice not found"}}"#.to_vec(),
        );
        let error = backend(&base_url, AudioFormat::Wav, None)
            .speak("Hi.", &SpeakOptions::default(), &mut |_| {
                ControlFlow::Continue(())
            })
            .unwrap_err();
        assert!(
            error.to_string().contains("HTTP 400") && error.to_string().contains("voice not found"),
            "{error}"
        );

        let (base_url, _requests) = serve(200, "audio/mpeg", b"ID3 mp3".to_vec());
        let error = backend(&base_url, AudioFormat::Wav, None)
            .speak("Hi.", &SpeakOptions::default(), &mut |_| {
                ControlFlow::Continue(())
            })
            .unwrap_err();
        assert!(error.to_string().contains("not a WAV stream"), "{error}");

        // Nothing is sent for text that cannot be said.
        let tts = backend("http://127.0.0.1:9", AudioFormat::Wav, None);
        let error = tts
            .speak(" ", &SpeakOptions::default(), &mut |_| {
                ControlFlow::Continue(())
            })
            .unwrap_err();
        assert!(matches!(error, TtsError::Input(_)));
    }

    #[test]
    fn collects_a_whole_reply() {
        let mut body = wav_header(16_000);
        body.extend(samples(4_000, 4_096));
        let (base_url, _requests) = serve(200, "audio/wav", body);
        let tts = crate::Tts::new(Box::new(backend(&base_url, AudioFormat::Wav, None)));
        let pcm = tts
            .speak_to_pcm("All of it.", &SpeakOptions::default())
            .unwrap();
        assert_eq!((pcm.sample_rate, pcm.samples.len()), (16_000, 4_000));
        assert_eq!(tts.info().model, "kokoro");
    }
}
