//! Command-line adapter over `phone-core`.
//!
//! [`Command::parse`] and [`execute`] are reusable: the `phone` binary calls
//! them through [`run`], and a host (Mnemosyne) runs the same commands
//! under its own name.
//!
//! With the `speaker` feature, `tones --play` and `say --play` play
//! through this machine's audio output (speech as it arrives from the
//! backend) and `devices` lists output devices. `say` and `tts` use the
//! text-to-speech backend: from `PHONE_TTS_*` variables in [`execute`], or
//! the host's in [`execute_with`].

use phone_core::dto::{self, Existing};
use phone_core::{Pcm, Sound, SoundChanges, tone};
use phone_tts::{SpeakOptions, Spoken, Tts};
use serde::Serialize;
use serde_json::Value;

pub const USAGE: &str = "\
usage: phone <command>

  tones <hz>... (--out <file.wav> | --play [--device <name>])
        [--chord] [--step <seconds>] [--duration <seconds>]
        [--preset <id>] [--waveform <id>] [--attack <s>] [--decay <s>]
        [--sustain <0-1>] [--release <s>] [--cutoff <hz>]
        [--volume <0-1>] [--rate <hz>]
                           render tones to a WAV file, or play them on this
                           machine's audio output (both, if both are
                           given): one after another, --step seconds apart
                           (default 0.5), or with --chord together for
                           --duration seconds (default 1.5); the sound is a
                           preset (default soft) with any of the changes
                           given; --volume defaults to 0.8 and --rate (the
                           sample rate) to 48000; --device is a name or a
                           device id from `devices`, or a name or part of
                           one that only that device has (default: the
                           default output)
  say <text>... (--out <file.wav> | --play [--device <name>])
      [--voice <voice>] [--speed <0.25-4>]
                           say text with the text-to-speech backend: to a
                           WAV file, or aloud as the speech arrives (both,
                           if both are given); --voice is one the backend
                           knows (default: its configured voice)
  tts                      the configured text-to-speech backend
  devices                  the output devices (JSON)
  presets                  the presets and waveforms (JSON)
  help                     this message

  --play and devices need a build with the `speaker` feature.

text-to-speech configuration (environment):
  PHONE_TTS_BASE_URL       the speech server, such as http://127.0.0.1:8880
                           (required for say and tts); it must implement
                           the OpenAI speech API (Kokoro-FastAPI,
                           openedai-speech, LocalAI, OpenAI)
  PHONE_TTS_MODEL          model name (default tts-1)
  PHONE_TTS_VOICE          default voice (default alloy)
  PHONE_TTS_FORMAT         wav (default) or pcm
  PHONE_TTS_SAMPLE_RATE    the rate of pcm (default 24000)
  PHONE_TTS_PATH           request path (default /v1/audio/speech)
  PHONE_TTS_API_KEY_ENV    name of the variable holding a bearer token
  PHONE_TTS_TIMEOUT_SECONDS  request timeout (default 120)";

const TONES_USAGE: &str = "usage: phone tones <hz>... (--out <file.wav> | --play [--device <name>]) [--chord] [--step <seconds>] [--duration <seconds>] [--preset <id>] [--waveform <id>] [--attack <s>] [--decay <s>] [--sustain <0-1>] [--release <s>] [--cutoff <hz>] [--volume <0-1>] [--rate <hz>]";

/// Seconds between the starts of tones in a sequence.
pub const DEFAULT_STEP_SECONDS: f64 = 0.5;
/// How long a chord sounds.
pub const DEFAULT_CHORD_SECONDS: f64 = 1.5;
pub const DEFAULT_VOLUME: f64 = 0.8;
pub const DEFAULT_SAMPLE_RATE: u32 = 48_000;

/// How tones are laid out in time.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Arrangement {
    /// One after another, `step` seconds apart.
    Sequence { step: f64 },
    /// All together for `duration` seconds.
    Chord { duration: f64 },
}

/// Where rendered tones go: a file, this machine's audio output, or both.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Destination {
    pub out: Option<String>,
    /// Play on this output device (`Some(None)`: the default output).
    pub play: Option<Option<String>>,
}

/// What `tones` renders.
#[derive(Debug, Clone, PartialEq)]
pub struct TonesRequest {
    pub hz: Vec<f64>,
    pub arrangement: Arrangement,
    pub sound: Sound,
    pub volume: f64,
    pub sample_rate: u32,
    pub destination: Destination,
}

/// What `say` says, and where.
#[derive(Debug, Clone, PartialEq)]
pub struct SayRequest {
    pub text: String,
    pub options: SpeakOptions,
    pub destination: Destination,
}

/// A parsed command.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    Help,
    Tones(TonesRequest),
    Say(SayRequest),
    Tts,
    Devices,
    Presets,
}

/// What `say` did.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SayOutcome {
    pub spoken: Spoken,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub written: Option<dto::RenderedFile>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub played: Option<PlayedInfo>,
}

/// What `tones` did: the file written (its fields at the top level) and
/// what was played.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TonesOutcome {
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    pub written: Option<dto::RenderedFile>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub played: Option<PlayedInfo>,
}

/// Sound played on an output device.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PlayedInfo {
    pub device: String,
    /// The device's sample rate, which the sound was resampled to.
    pub sample_rate: u32,
    pub seconds: f64,
    /// Silence while waiting for streamed sound that came late.
    pub gap_seconds: f64,
}

#[cfg(feature = "speaker")]
impl From<phone_speaker::Played> for PlayedInfo {
    fn from(played: phone_speaker::Played) -> Self {
        PlayedInfo {
            device: played.device,
            sample_rate: played.sample_rate,
            seconds: played.seconds,
            gap_seconds: played.gap_seconds,
        }
    }
}

/// An output device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DeviceInfo {
    /// What `--device` takes to pick exactly this device.
    pub id: String,
    pub name: String,
    pub is_default: bool,
}

#[cfg(not(feature = "speaker"))]
const NO_SPEAKER: &str = "this phone was built without audio output; rebuild with the `speaker` feature (cargo build -p phone-cli --features speaker)";

/// Plays `pcm` on `device` (the default output when `None`).
pub fn play(pcm: &Pcm, device: Option<&str>) -> Result<PlayedInfo, String> {
    #[cfg(feature = "speaker")]
    {
        Ok(phone_speaker::play(pcm, device)?.into())
    }
    #[cfg(not(feature = "speaker"))]
    {
        let _ = (pcm, device);
        Err(NO_SPEAKER.to_string())
    }
}

/// The output devices.
pub fn devices() -> Result<Output, String> {
    #[cfg(feature = "speaker")]
    {
        let devices: Vec<DeviceInfo> = phone_speaker::output_devices()?
            .into_iter()
            .map(|device| DeviceInfo {
                id: device.id,
                name: device.name,
                is_default: device.is_default,
            })
            .collect();
        Output::json(devices)
    }
    #[cfg(not(feature = "speaker"))]
    Err(NO_SPEAKER.to_string())
}

/// What a command printed.
#[derive(Debug, Clone, PartialEq)]
pub enum Output {
    Json(Value),
    Text(String),
}

impl Output {
    fn json(value: impl serde::Serialize) -> Result<Self, String> {
        serde_json::to_value(value)
            .map(Self::Json)
            .map_err(|error| format!("failed to serialize output: {error}"))
    }

    pub fn render(&self) -> String {
        match self {
            Self::Json(value) => {
                serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
            }
            Self::Text(text) => text.trim_end().to_string(),
        }
    }
}

fn number<T: std::str::FromStr>(flag: &str, value: &str) -> Result<T, String> {
    value
        .parse()
        .map_err(|_| format!("{flag} needs a number, not {value:?}\n{TONES_USAGE}"))
}

fn parse_tones(args: &[String]) -> Result<TonesRequest, String> {
    let mut hz = Vec::new();
    let mut out = None;
    let mut play = false;
    let mut device = None;
    let mut chord = false;
    let mut step = None;
    let mut duration = None;
    let mut preset = phone_core::Preset::Soft;
    let mut changes = SoundChanges::default();
    let mut volume = DEFAULT_VOLUME;
    let mut sample_rate = DEFAULT_SAMPLE_RATE;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        let mut value = || {
            args.next()
                .ok_or_else(|| format!("{arg} needs a value\n{TONES_USAGE}"))
        };
        match arg.as_str() {
            "--out" => out = Some(value()?.clone()),
            "--play" => play = true,
            "--device" => device = Some(value()?.clone()),
            "--chord" => chord = true,
            "--step" => step = Some(number(arg, value()?)?),
            "--duration" => duration = Some(number(arg, value()?)?),
            "--preset" => preset = dto::preset(value()?)?,
            "--waveform" => changes.waveform = Some(dto::waveform(value()?)?),
            "--attack" => changes.attack = Some(number(arg, value()?)?),
            "--decay" => changes.decay = Some(number(arg, value()?)?),
            "--sustain" => changes.sustain = Some(number(arg, value()?)?),
            "--release" => changes.release = Some(number(arg, value()?)?),
            "--cutoff" => changes.cutoff_hz = Some(number(arg, value()?)?),
            "--volume" => volume = number(arg, value()?)?,
            "--rate" => sample_rate = number(arg, value()?)?,
            flag if flag.starts_with("--") => {
                return Err(format!("unknown option {flag}\n{TONES_USAGE}"));
            }
            frequency => hz.push(number("a frequency", frequency)?),
        }
    }
    if hz.is_empty() {
        return Err(format!("tones: give at least one frequency\n{TONES_USAGE}"));
    }
    if device.is_some() && !play {
        return Err(format!("tones: --device is for --play\n{TONES_USAGE}"));
    }
    if out.is_none() && !play {
        return Err(format!("tones: give --out, --play, or both\n{TONES_USAGE}"));
    }
    let destination = Destination {
        out,
        play: play.then_some(device),
    };
    let arrangement = match (chord, step, duration) {
        (true, Some(_), _) => return Err("tones: --step is for sequences, not --chord".into()),
        (false, _, Some(_)) => return Err("tones: --duration is for --chord".into()),
        (true, None, duration) => Arrangement::Chord {
            duration: duration.unwrap_or(DEFAULT_CHORD_SECONDS),
        },
        (false, step, None) => Arrangement::Sequence {
            step: step.unwrap_or(DEFAULT_STEP_SECONDS),
        },
    };
    Ok(TonesRequest {
        hz,
        arrangement,
        sound: Sound::customized(preset, &changes)?,
        volume,
        sample_rate,
        destination,
    })
}

const SAY_USAGE: &str = "usage: phone say <text>... (--out <file.wav> | --play [--device <name>]) [--voice <voice>] [--speed <0.25-4>]";

fn parse_say(args: &[String]) -> Result<SayRequest, String> {
    let mut words = Vec::new();
    let mut options = SpeakOptions::default();
    let mut out = None;
    let mut play = false;
    let mut device = None;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        let mut value = || {
            args.next()
                .ok_or_else(|| format!("{arg} needs a value\n{SAY_USAGE}"))
        };
        match arg.as_str() {
            "--out" => out = Some(value()?.clone()),
            "--play" => play = true,
            "--device" => device = Some(value()?.clone()),
            "--voice" => options.voice = Some(value()?.clone()),
            "--speed" => {
                let speed = value()?;
                options.speed =
                    Some(speed.parse().map_err(|_| {
                        format!("--speed needs a number, not {speed:?}\n{SAY_USAGE}")
                    })?);
            }
            flag if flag.starts_with("--") => {
                return Err(format!("unknown option {flag}\n{SAY_USAGE}"));
            }
            word => words.push(word),
        }
    }
    let text = words.join(" ");
    phone_tts::validate_text(&text).map_err(|error| format!("say: {error}\n{SAY_USAGE}"))?;
    options.validate()?;
    if device.is_some() && !play {
        return Err(format!("say: --device is for --play\n{SAY_USAGE}"));
    }
    if out.is_none() && !play {
        return Err(format!("say: give --out, --play, or both\n{SAY_USAGE}"));
    }
    Ok(SayRequest {
        text,
        options,
        destination: Destination {
            out,
            play: play.then_some(device),
        },
    })
}

impl Command {
    pub fn parse(args: &[String]) -> Result<Self, String> {
        let Some((command, rest)) = args.split_first() else {
            return Ok(Self::Help);
        };
        match command.as_str() {
            "help" | "--help" | "-h" => Ok(Self::Help),
            "presets" if rest.is_empty() => Ok(Self::Presets),
            "presets" | "devices" if !rest.is_empty() => {
                Err(format!("unexpected arguments: {}", rest.join(" ")))
            }
            "devices" => Ok(Self::Devices),
            "tones" => parse_tones(rest).map(Self::Tones),
            "say" => parse_say(rest).map(Self::Say),
            "tts" if rest.is_empty() => Ok(Self::Tts),
            "tts" => Err(format!("unexpected arguments: {}", rest.join(" "))),
            other => Err(format!("unknown command {other:?}\n{USAGE}")),
        }
    }
}

/// Renders a [`TonesRequest`], writes it (replacing an existing file), and
/// plays it, as asked. The file is written before playing starts.
pub fn render_tones(request: &TonesRequest) -> Result<TonesOutcome, String> {
    let tones = match request.arrangement {
        Arrangement::Sequence { step } => tone::sequence(&request.hz, step),
        Arrangement::Chord { duration } => tone::together(&request.hz, duration),
    };
    let pcm = tone::render(&tones, &request.sound, request.volume, request.sample_rate)?;
    let written = match &request.destination.out {
        Some(out) => Some(dto::write_wav(&pcm, out, Existing::Replace)?),
        None => None,
    };
    let played = match &request.destination.play {
        Some(device) => Some(play(&pcm, device.as_deref())?),
        None => None,
    };
    Ok(TonesOutcome { written, played })
}

/// Says a [`SayRequest`] with `tts`. Played speech streams: it starts
/// once a little has arrived. A file, if asked for, is written (replacing
/// an existing one) after the speech has ended.
pub fn say(tts: &Tts, request: &SayRequest) -> Result<SayOutcome, String> {
    let SayRequest {
        text,
        options,
        destination,
    } = request;
    let (spoken, pcm, played) = match &destination.play {
        None => {
            let pcm = tts.speak_to_pcm(text, options)?;
            let spoken = Spoken {
                sample_rate: pcm.sample_rate,
                seconds: pcm.seconds(),
                stopped: false,
            };
            (spoken, Some(pcm), None)
        }
        Some(device) => {
            let (spoken, pcm, played) = say_aloud(
                tts,
                text,
                options,
                device.as_deref(),
                destination.out.is_some(),
            )?;
            (spoken, pcm, Some(played))
        }
    };
    let written = match (&destination.out, pcm) {
        (Some(out), Some(pcm)) => Some(dto::write_wav(&pcm, out, Existing::Replace)?),
        _ => None,
    };
    Ok(SayOutcome {
        spoken,
        written,
        played,
    })
}

/// Plays speech on `device` as it arrives, keeping a copy when `keep`.
#[cfg(feature = "speaker")]
fn say_aloud(
    tts: &Tts,
    text: &str,
    options: &SpeakOptions,
    device: Option<&str>,
    keep: bool,
) -> Result<(Spoken, Option<Pcm>, PlayedInfo), String> {
    use std::ops::ControlFlow;

    // Checked before the device opens.
    phone_tts::validate_text(text)?;
    options.validate()?;
    let mut output = phone_speaker::Output::open(device)?;
    let mut kept = Vec::new();
    let mut failure = None;
    let spoken = tts.speak(text, options, &mut |pcm| {
        if keep {
            kept.extend_from_slice(&pcm.samples);
        }
        match output.push(&pcm) {
            Ok(()) => ControlFlow::Continue(()),
            Err(error) => {
                failure = Some(error);
                ControlFlow::Break(())
            }
        }
    })?;
    if let Some(error) = failure {
        return Err(error.into());
    }
    let played = output.finish()?;
    let pcm = keep.then_some(Pcm {
        sample_rate: spoken.sample_rate,
        samples: kept,
    });
    Ok((spoken, pcm, played.into()))
}

#[cfg(not(feature = "speaker"))]
fn say_aloud(
    _tts: &Tts,
    _text: &str,
    _options: &SpeakOptions,
    _device: Option<&str>,
    _keep: bool,
) -> Result<(Spoken, Option<Pcm>, PlayedInfo), String> {
    Err(NO_SPEAKER.to_string())
}

/// Runs `command`, building the text-to-speech backend with `tts` when the
/// command needs one (so commands that do not work unconfigured).
pub fn execute_with(
    command: Command,
    tts: &dyn Fn() -> Result<Tts, String>,
) -> Result<Output, String> {
    match command {
        Command::Help => Ok(Output::Text(USAGE.to_string())),
        Command::Presets => Output::json(dto::catalog()),
        Command::Devices => devices(),
        Command::Tones(request) => Output::json(render_tones(&request)?),
        Command::Say(request) => {
            // Say so before asking the backend for speech.
            #[cfg(not(feature = "speaker"))]
            if request.destination.play.is_some() {
                return Err(NO_SPEAKER.to_string());
            }
            Output::json(say(&tts()?, &request)?)
        }
        Command::Tts => Output::json(tts()?.info()),
    }
}

/// Runs `command` with the backend from `PHONE_TTS_*` variables.
pub fn execute(command: Command) -> Result<Output, String> {
    execute_with(command, &|| Tts::from_env().map_err(String::from))
}

/// Parses and runs `args`, as the `phone` binary does.
pub fn run(args: &[String]) -> Result<Output, String> {
    execute(Command::parse(args)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|arg| arg.to_string()).collect()
    }

    fn tones(list: &[&str]) -> Result<TonesRequest, String> {
        match Command::parse(&args(list))? {
            Command::Tones(request) => Ok(request),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn parses_sequences_chords_and_sounds() {
        let request = tones(&["tones", "440", "550.5", "--out", "a.wav"]).unwrap();
        assert_eq!(request.hz, vec![440.0, 550.5]);
        assert_eq!(request.arrangement, Arrangement::Sequence { step: 0.5 });
        assert_eq!(request.sound, phone_core::Preset::Soft.sound());
        assert_eq!((request.volume, request.sample_rate), (0.8, 48_000));

        let request = tones(&[
            "tones",
            "220",
            "--chord",
            "--duration",
            "2",
            "--preset",
            "organ",
            "--cutoff",
            "800",
            "--rate",
            "22050",
            "--out",
            "b.wav",
        ])
        .unwrap();
        assert_eq!(request.arrangement, Arrangement::Chord { duration: 2.0 });
        assert_eq!(request.sound.waveform, phone_core::Waveform::Square);
        assert_eq!(request.sound.cutoff_hz, 800.0);
        assert_eq!(request.sample_rate, 22_050);

        let request = tones(&["tones", "440", "--play", "--device", "hdmi"]).unwrap();
        assert_eq!(
            request.destination,
            Destination {
                out: None,
                play: Some(Some("hdmi".to_string())),
            }
        );
        assert_eq!(Command::parse(&args(&["devices"])), Ok(Command::Devices));
    }

    /// A backend that says a fixed 0.25 s of a 1000 Hz sine at 8 kHz, in
    /// pieces.
    struct Fake;

    impl phone_tts::TextToSpeech for Fake {
        fn info(&self) -> phone_tts::BackendInfo {
            phone_tts::BackendInfo {
                kind: "fake".to_string(),
                model: "m".to_string(),
                voice: "v".to_string(),
                endpoint: None,
            }
        }

        fn speak(
            &self,
            text: &str,
            options: &SpeakOptions,
            chunk: &mut dyn FnMut(Pcm) -> std::ops::ControlFlow<()>,
        ) -> Result<Spoken, phone_tts::TtsError> {
            phone_tts::validate_text(text)?;
            options.validate()?;
            let samples: Vec<f32> = (0..2_000)
                .map(|n| (n as f32 * std::f32::consts::TAU / 8.0).sin() * 0.5)
                .collect();
            for piece in samples.chunks(800) {
                let _ = chunk(Pcm {
                    sample_rate: 8_000,
                    samples: piece.to_vec(),
                });
            }
            Ok(Spoken {
                sample_rate: 8_000,
                seconds: 0.25,
                stopped: false,
            })
        }
    }

    fn fake() -> Result<Tts, String> {
        Ok(Tts::new(Box::new(Fake)))
    }

    #[test]
    fn parses_say() {
        let Command::Say(request) = Command::parse(&args(&[
            "say",
            "Hello,",
            "Mnemosyne.",
            "--voice",
            "af_heart",
            "--speed",
            "1.2",
            "--out",
            "a.wav",
        ]))
        .unwrap() else {
            panic!("expected say");
        };
        assert_eq!(request.text, "Hello, Mnemosyne.");
        assert_eq!(request.options.voice.as_deref(), Some("af_heart"));
        assert_eq!(request.options.speed, Some(1.2));
        assert!(Command::parse(&args(&["say", "--out", "a.wav"])).is_err());
        assert!(Command::parse(&args(&["say", "hi"])).is_err());
        assert!(Command::parse(&args(&["say", "hi", "--speed", "9", "--play"])).is_err());
        assert_eq!(Command::parse(&args(&["tts"])), Ok(Command::Tts));
    }

    #[test]
    fn says_to_a_file_with_the_given_backend() {
        let dir = std::env::temp_dir().join(format!("phone-cli-say-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("hello.wav").to_string_lossy().into_owned();
        let command = Command::parse(&args(&["say", "Hello.", "--out", &out])).unwrap();
        let Output::Json(value) = execute_with(command, &fake).unwrap() else {
            panic!("expected JSON");
        };
        assert_eq!(value["spoken"]["sample_rate"], 8_000);
        assert_eq!(value["written"]["bytes"], 44 + 2 * 2_000);
        assert_eq!(std::fs::metadata(&out).unwrap().len(), 44 + 4_000);
        std::fs::remove_dir_all(&dir).unwrap();
        let Output::Json(info) = execute_with(Command::Tts, &fake).unwrap() else {
            panic!("expected JSON");
        };
        assert_eq!(info["kind"], "fake");
        // Unconfigured: the error says what to set.
        let unconfigured = || Err("set PHONE_TTS_BASE_URL".to_string());
        assert!(
            execute_with(Command::Tts, &unconfigured)
                .unwrap_err()
                .contains("PHONE_TTS_BASE_URL")
        );
        // Commands that need no backend never ask for one.
        assert!(execute_with(Command::Presets, &unconfigured).is_ok());
    }

    #[cfg(not(feature = "speaker"))]
    #[test]
    fn playing_needs_the_speaker_feature() {
        assert!(
            run(&args(&["tones", "440", "--play"]))
                .unwrap_err()
                .contains("speaker")
        );
        assert!(run(&args(&["devices"])).unwrap_err().contains("speaker"));
        let say = Command::parse(&args(&["say", "Hi.", "--play"])).unwrap();
        assert!(execute_with(say, &fake).unwrap_err().contains("speaker"));
    }

    #[test]
    fn reports_bad_arguments() {
        assert!(tones(&["tones", "--out", "a.wav"]).is_err());
        assert!(tones(&["tones", "440"]).unwrap_err().contains("--out"));
        assert!(tones(&["tones", "440", "--device", "hdmi", "--out", "a.wav"]).is_err());
        assert!(tones(&["tones", "la", "--out", "a.wav"]).is_err());
        assert!(tones(&["tones", "440", "--preset", "kazoo", "--out", "a.wav"]).is_err());
        assert!(tones(&["tones", "440", "--sustain", "3", "--out", "a.wav"]).is_err());
        assert!(tones(&["tones", "440", "--chord", "--step", "1", "--out", "a.wav"]).is_err());
        assert!(tones(&["tones", "440", "--loud", "--out", "a.wav"]).is_err());
        assert!(Command::parse(&args(&["sing"])).is_err());
        assert_eq!(Command::parse(&[]), Ok(Command::Help));
    }

    #[test]
    fn renders_a_wav_file_and_lists_presets() {
        let dir = std::env::temp_dir().join(format!("phone-cli-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("arpeggio.wav").to_string_lossy().into_owned();
        let output = run(&args(&[
            "tones", "261.63", "329.63", "392", "--step", "0.25", "--rate", "8000", "--out", &out,
        ]))
        .unwrap();
        let Output::Json(value) = output else {
            panic!("expected JSON");
        };
        // Three steps, the last note 0.225 s plus the soft preset's 0.2 s
        // release.
        assert!((value["seconds"].as_f64().unwrap() - 0.925).abs() < 1e-3);
        assert_eq!(value["sample_rate"], 8_000);
        let bytes = std::fs::read(&out).unwrap();
        assert_eq!(&bytes[..4], b"RIFF");
        assert_eq!(bytes.len() as u64, value["bytes"].as_u64().unwrap());
        // Out-of-range tones are reported, not written.
        assert!(
            run(&args(&["tones", "4", "--out", &out]))
                .unwrap_err()
                .contains("playable range")
        );
        std::fs::remove_dir_all(&dir).unwrap();

        let Output::Json(catalog) = run(&args(&["presets"])).unwrap() else {
            panic!("expected JSON");
        };
        assert_eq!(catalog["presets"].as_array().unwrap().len(), 6);
    }
}
