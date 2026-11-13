# phone

Sound synthesis for Mnemosyne: the complement of
[ekho](https://github.com/sawfan/ekho_rs). Ekho turns sound into text;
phone makes sound. Modules that need sound decide *what* to say or play
(IPA plans speech from a transcription; music turns pitches into tones);
phone decides *how* it sounds, and keeps that in one place.

| Crate        | What it is                                                         |
|--------------|--------------------------------------------------------------------|
| `phone-core` | The library (lib `phone_core`): the formant voice, instruments and tone rendering, PCM and WAV |
| `phone-cli`  | The `phone` command: render tones to WAV, list presets              |
| `phone-mcp`  | The `phone-mcp` MCP server over stdio (`phone.*` tools)             |
| `phone-speaker` | Playback through this machine's audio output (cpal) |
| `phone-leptos` | Browser playback: the page's one `AudioContext` (`with_context`) and rendered clips (`play`, `stop`) |

`phone-core` produces samples and knows nothing about where they are played,
so the same rendering serves a WAV file, the command line and MCP, a sound
card, or the browser (compiled to wasm).

## What it has

- `formant`: a Klatt-style formant voice (Klatt 1980). Keyframes of pitch,
  voicing, aspiration, frication, formants and bandwidths, frication
  filter, and nasal coupling are interpolated and rendered: a Rosenberg
  glottal pulse (with vibrato, jitter, shimmer, and breath noise) and
  aspiration through a cascade of formant resonators with a nasal
  pole–zero pair, frication through a parallel band-pass, lip radiation,
  and high-frequency emphasis. `ipa-core` drives it to speak IPA and
  X-SAMPA transcriptions.
- `instrument`: how a note sounds. A `Sound` is a waveform (sine,
  triangle, square, sawtooth), an ADSR `Envelope`, and a low-pass cutoff;
  six presets (pure, soft, organ, pluck, pad, bright), and `SoundChanges`
  to customize one. Mnemosyne's browser music blocks use these same values.
- `tone`: notes as frequency and time (`Tone`), arranged as sequences,
  chords, or groups in turn, checked by `validate`, and rendered by
  `render(tones, sound, volume, sample_rate)` with
  [FunDSP](https://github.com/SamiPerttu/fundsp)'s band-limited wavetable
  oscillators and a state-variable low-pass filter (resonance matched to the
  browser's Web Audio filter). Levels follow the browser: each tone peaks at
  0.25 × volume, shared between simultaneous tones; sound that would still
  clip is scaled to a 0.98 peak. Deterministic.
- `pcm`: `Pcm` (mono samples and their rate), band-limited (windowed-sinc,
  Blackman) resampling, peak normalization, and 16-bit mono WAV encoding.
- `dto`: the preset catalog, id lookups with helpful errors, and
  `write_wav` (replace, or refuse existing files).

Pitch is not phone's business: callers give hertz (Mnemosyne turns note
names into hertz with euterpe).

## Command line and MCP

```
phone tones 261.63 329.63 392 --preset pluck --step 0.3 --out arpeggio.wav
phone tones 220 277.18 329.63 --chord --duration 2 --preset pad --cutoff 900 --out pad.wav
phone presets

# With the `speaker` feature (cpal; on Linux, the ALSA development headers):
phone devices
phone tones 440 554.37 659.25 --chord --play
phone tones 440 --play --device hw:CARD=Generic,DEV=0
```

`--device` takes an id from `phone devices`, or a name (or part of one)
that only one device has: ALSA lists each way of opening a card as its own
device, often under the same name.

`phone-mcp` has `phone.render_tones` (notes with start and duration, a
preset and changes, volume, sample rate; writes a new `.wav` file and never
replaces one) and `phone.presets`. Mnemosyne serves the same commands as
`mnemosyne phone …` and the tools as `phone_render_tones` and
`phone_presets`.

## Browser playback

`phone-leptos` keeps one `AudioContext` per page, created and resumed on
the first user gesture. `play(pcm, done)` plays a rendered clip through an
`AudioBufferSourceNode` (the browser resamples it), replacing a clip
already playing; `with_context` gives other code (Mnemosyne's live music
voices) the same context. It has no Leptos dependency yet; the bindings
compile on every target but only work in a browser.

## Planned

Recorded in `docs/agent-status.md`: real-time synthesis in an AudioWorklet
(and live native synthesis); text-to-speech backends alongside the formant
voice.

## Building

Inside Mnemosyne, build through the root workspace (the crates are
members; this package root is excluded). Standalone: `cargo test` here.
