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
| `phone-tts`  | Text-to-speech behind a backend-neutral trait, speech streamed in pieces (OpenAI-compatible backend: Kokoro, Piper via openedai-speech, LocalAI, OpenAI) |
| `phone-speaker` | Playback through this machine's audio output (cpal), whole clips or streamed pieces |
| `phone-leptos` | Browser playback: the page's one `AudioContext` (`with_context`), rendered clips (`play`, `stop`), and the live synthesizer in an AudioWorklet (`live`) |
| `phone-worklet` | The live synthesizer as a small WebAssembly module for the AudioWorklet (plain exports, no imports, no wasm-bindgen) |

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
  Blackman) resampling, whole or streamed (`Resampler`, giving exactly the
  same result for pieces as for the whole), peak normalization, and 16-bit
  mono WAV encoding.
- `live`: a real-time `Synth` (polyphonic, up to 48 voices, renders blocks
  on demand) driven by `Command`s, which have a byte encoding so the page
  and the audio thread share one definition. Like MIDI channels, every note
  carries a bus (8, each mixed to its own output with its own volume);
  notes start now or at a time on the audio clock, held until released or
  for a set time, so a melody is handed over whole and starts on exact
  frames. Commands: note on/off, release a bus, cut a bus's timed notes,
  cut a bus, cut all, set a bus's volume. Live notes and melodies match
  the same notes rendered by `tone`.
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
replaces one), `phone.presets`, `phone.say` (text to a new `.wav` file), and
`phone.tts_backend`. Mnemosyne serves the same commands as
`mnemosyne phone …` and the tools as `phone_*`.

## Text-to-speech

`phone-tts` says text through a backend behind `TextToSpeech`, handing out
speech in pieces of about 0.1 s as the backend produces it; the caller can
stop part way. The `openai_compatible` backend posts to `/v1/audio/speech`
and decodes the response as it streams (WAV, whose header gives the rate
and channels, or raw 16-bit PCM at a configured rate). Configure it with
`PHONE_TTS_*` variables (or, in Mnemosyne, `[phone.tts.backend]`):

```
PHONE_TTS_BASE_URL=http://127.0.0.1:8880 PHONE_TTS_MODEL=kokoro PHONE_TTS_VOICE=af_heart \
  phone say "Hello from phone." --out hello.wav
phone say "Hello." --play          # with the `speaker` feature: aloud as it arrives
phone tts                          # the configured backend
```

Played speech is asked for one sentence at a time (`sentences::split`:
breaks after `.` `!` `?` `…` and blank lines, not inside numbers or after
abbreviations or initials; short pieces joined, long ones broken at a
comma or space), each once the one before has arrived. Servers such as
Kokoro synthesize a large block before sending any, so this is what gets
the first sound out quickly: with Kokoro, a 30 s passage starts after
0.9 s instead of 4.5 s. Files are asked for whole, for the best
intonation.

With `--play`, `phone-speaker`'s `Output` takes the pieces as they come,
resamples them to the device's rate (with the streaming resampler, so piece
boundaries leave no trace), and starts once a quarter second is buffered;
silence while waiting for a late piece is counted (`gap_seconds`).

phone's formant voice (driven by `ipa-core`) speaks IPA transcriptions, not
text, so it is not a text-to-speech backend.

## Browser playback

`phone-leptos` keeps one `AudioContext` per page, created and resumed on
the first user gesture. `play(pcm, done)` plays a rendered clip through an
`AudioBufferSourceNode` (the browser resamples it), replacing a clip
already playing; `with_context` gives other code (Mnemosyne's live music
voices) the same context. It has no Leptos dependency yet; the bindings
compile on every target but only work in a browser.

## Live synthesis in the browser

The synthesizer runs on the browser's audio thread, as its own module
rather than inside the page's wasm: the audio thread cannot call into the
page's instance without shared memory (nightly Rust, atomics, cross-origin
isolation headers), and a separate module of about 190 KB starts at once
and imports nothing. Types are shared through `phone-core`'s source, not
memory: the page encodes `Command`s, `worklet/phone-processor.js` passes
the bytes to `phone-worklet`, which decodes them with the same code.

To use it, serve two files next to the page and call
`phone_leptos::live::start` from a user gesture, then `live::send`:

- `phone-leptos/worklet/phone-processor.js`
- `phone_worklet.wasm`, from
  `cargo build -p phone-worklet --release --target wasm32-unknown-unknown`

Each bus's output goes through a compressor and an analyser to the
speakers; `live::bus(name)` gives a named bus its slot (at most 8 per
page). Mnemosyne builds the module in a Trunk pre-build hook
(`crates/mnemosyne-web/Trunk.dev.toml`) and copies both files into the
bundle (`index.html`); its music blocks and `/phone` page play through it.

Starting a note allocates its voice on the audio thread (a small
allocation inside the module's own memory); rendering does not allocate.
The wavetables are built when the synthesizer starts, not on the first
note.

In the browser, `phone_leptos::enqueue` plays clips back to back with no
gap (each starts where the last queued one ends), so a page can fetch
speech a sentence at a time and play each as it arrives; Mnemosyne's
`/phone` speech box does that against its `POST /phone/say`, splitting
with `phone_tts::sentences` (phone-tts compiles to wasm without its
`openai` feature). Any `phone_leptos::stop` ends such a run
(`stop_count`).

## Planned

Recorded in `docs/agent-status.md`: live native synthesis, more backends,
and a spoken assistant loop.

## Building

Inside Mnemosyne, build through the root workspace (the crates are
members; this package root is excluded). Standalone: `cargo test` here.
