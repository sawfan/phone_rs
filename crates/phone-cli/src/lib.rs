//! Command-line adapter over `phone-core`.
//!
//! [`Command::parse`] and [`execute`] are reusable: the `phone` binary calls
//! them through [`run`], and a host (Mnemosyne) runs the same commands
//! under its own name.
//!
//! With the `speaker` feature, `tones --play` plays through this machine's
//! audio output and `devices` lists output devices.

use phone_core::dto::{self, Existing};
use phone_core::{Pcm, Sound, SoundChanges, tone};
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
  devices                  the output devices (JSON)
  presets                  the presets and waveforms (JSON)
  help                     this message

  --play and devices need a build with the `speaker` feature.";

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

/// A parsed command.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    Help,
    Tones(TonesRequest),
    Devices,
    Presets,
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
        let played = phone_speaker::play(pcm, device)?;
        Ok(PlayedInfo {
            device: played.device,
            sample_rate: played.sample_rate,
            seconds: played.seconds,
        })
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

pub fn execute(command: Command) -> Result<Output, String> {
    match command {
        Command::Help => Ok(Output::Text(USAGE.to_string())),
        Command::Presets => Output::json(dto::catalog()),
        Command::Devices => devices(),
        Command::Tones(request) => Output::json(render_tones(&request)?),
    }
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

    #[cfg(not(feature = "speaker"))]
    #[test]
    fn playing_needs_the_speaker_feature() {
        assert!(
            run(&args(&["tones", "440", "--play"]))
                .unwrap_err()
                .contains("speaker")
        );
        assert!(run(&args(&["devices"])).unwrap_err().contains("speaker"));
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
