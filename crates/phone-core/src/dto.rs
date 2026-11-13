//! Results shared by the command line, MCP servers, and hosts: the sound
//! catalog and rendered files.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;

use serde::Serialize;

use crate::instrument::{Preset, Waveform};
use crate::pcm::Pcm;

/// A preset and the sound it makes.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct PresetInfo {
    pub id: &'static str,
    pub label: &'static str,
    pub waveform: &'static str,
    /// Envelope stages in seconds; sustain as a fraction of the peak.
    pub attack: f64,
    pub decay: f64,
    pub sustain: f64,
    pub release: f64,
    /// Low-pass cutoff in hertz (20000 is open).
    pub cutoff_hz: f64,
}

/// A waveform's id and description.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct WaveformInfo {
    pub id: &'static str,
    pub label: &'static str,
}

/// Every preset and waveform.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Catalog {
    pub presets: Vec<PresetInfo>,
    pub waveforms: Vec<WaveformInfo>,
}

pub fn catalog() -> Catalog {
    Catalog {
        presets: Preset::ALL
            .into_iter()
            .map(|preset| {
                let sound = preset.sound();
                PresetInfo {
                    id: preset.id(),
                    label: preset.label(),
                    waveform: sound.waveform.id(),
                    attack: sound.envelope.attack,
                    decay: sound.envelope.decay,
                    sustain: sound.envelope.sustain,
                    release: sound.envelope.release,
                    cutoff_hz: sound.cutoff_hz,
                }
            })
            .collect(),
        waveforms: Waveform::ALL
            .into_iter()
            .map(|waveform| WaveformInfo {
                id: waveform.id(),
                label: waveform.label(),
            })
            .collect(),
    }
}

/// The preset with `id`, or an error listing the ids.
pub fn preset(id: &str) -> Result<Preset, String> {
    Preset::from_id(id).ok_or_else(|| {
        let ids: Vec<_> = Preset::ALL.iter().map(|preset| preset.id()).collect();
        format!("unknown preset {id:?}; use one of {}", ids.join(", "))
    })
}

/// The waveform with `id`, or an error listing the ids.
pub fn waveform(id: &str) -> Result<Waveform, String> {
    Waveform::from_id(id).ok_or_else(|| {
        let ids: Vec<_> = Waveform::ALL.iter().map(|waveform| waveform.id()).collect();
        format!("unknown waveform {id:?}; use one of {}", ids.join(", "))
    })
}

/// A WAV file that was written.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct RenderedFile {
    pub file: String,
    pub bytes: usize,
    pub seconds: f64,
    pub sample_rate: u32,
}

/// Whether [`write_wav`] may replace an existing file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Existing {
    Replace,
    Refuse,
}

/// Writes `pcm` as a WAV file at `path`, which must end in `.wav`.
pub fn write_wav(pcm: &Pcm, path: &str, existing: Existing) -> Result<RenderedFile, String> {
    let has_wav_extension = Path::new(path)
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("wav"));
    if !has_wav_extension {
        return Err(format!("{path} must end in .wav"));
    }
    let bytes = pcm.wav();
    let mut options = OpenOptions::new();
    options.write(true);
    match existing {
        Existing::Replace => options.create(true).truncate(true),
        Existing::Refuse => options.create_new(true),
    };
    options
        .open(path)
        .and_then(|mut file| file.write_all(&bytes))
        .map_err(|error| match error.kind() {
            std::io::ErrorKind::AlreadyExists => {
                format!("{path} already exists; choose a new file name")
            }
            _ => format!("failed to write {path}: {error}"),
        })?;
    Ok(RenderedFile {
        file: path.to_string(),
        bytes: bytes.len(),
        seconds: pcm.seconds(),
        sample_rate: pcm.sample_rate,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_catalog_lists_every_preset_and_waveform() {
        let catalog = catalog();
        assert_eq!(catalog.presets.len(), Preset::ALL.len());
        assert_eq!(catalog.waveforms.len(), Waveform::ALL.len());
        let pluck = catalog.presets.iter().find(|p| p.id == "pluck").unwrap();
        assert_eq!((pluck.waveform, pluck.sustain), ("sawtooth", 0.0));
        assert_eq!(preset("pad"), Ok(Preset::Pad));
        assert!(preset("kazoo").unwrap_err().contains("pure, soft"));
        assert!(waveform("noise").unwrap_err().contains("sine"));
    }

    #[test]
    fn writes_wav_files_and_refuses_to_replace_when_asked() {
        let dir = std::env::temp_dir().join(format!("phone-dto-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tone.wav").to_string_lossy().into_owned();
        let pcm = Pcm {
            sample_rate: 8_000,
            samples: vec![0.0; 800],
        };
        let written = write_wav(&pcm, &path, Existing::Refuse).unwrap();
        assert_eq!((written.bytes, written.seconds), (44 + 1_600, 0.1));
        assert!(
            write_wav(&pcm, &path, Existing::Refuse)
                .unwrap_err()
                .contains("already exists")
        );
        assert!(write_wav(&pcm, &path, Existing::Replace).is_ok());
        assert!(write_wav(&pcm, &format!("{path}.txt"), Existing::Replace).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
