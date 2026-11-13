# Work status

## Objective

One place for making sound in Mnemosyne: the complement of ekho (which
transcribes). Speech (IPA's formant voice now, text-to-speech backends
later) and music synthesis share rendering, export, and playback. Modules
decide what to play; phone decides how it sounds.

## Plan (agreed 2026-11-12)

1. **Done.** Workspace skeleton; IPA's synthesizer moved here.
2. **Done.** Music clips with FunDSP, `phone-cli`, `phone-mcp`, host module
   `phone`. (The clip type is `Pcm` for now; an enum with encoded audio
   waits for the TTS backend, phase 6.)
3. **Done.** Browser playback: `phone-leptos` owns the page's single
   `AudioContext` and plays clips; IPA switched from WAV-blob `<audio>`.
4. **Done** for clips: native playback through cpal behind a feature.
   Live (pull-based) synthesis waits for a caller that needs it.
5. Real time in the browser: a `phone-worklet` wasm module with plain
   C-style exports (no wasm-bindgen, so no atomics/nightly or cross-origin
   isolation), a small JS AudioWorklet processor, commands over a
   MessagePort. Prototype the Trunk build of the second wasm artifact
   first. Then move the music keyboard and delete the Web Audio node graph
   (do not maintain both engines long-term).
6. Text-to-speech backend modelled on ekho's `openai_compatible`.

## Done

- 2026-11-12: workspace (`phone-core` only; the hello-world `src/main.rs`
  removed). `phone_core::formant` (`Params`, `Key`, `render(keys,
  sample_rate, seed)`) is ipa-core's former `sound/synth.rs` with public
  visibility and unchanged code; `phone_core::pcm` has `normalize` and
  `wav` (from ipa-core's `speak`/`wav`). ipa-core depends on it. Root
  workspace member; package root excluded; root, `mnemosyne-web`, and
  `mnemosyne-leptos` patch `phone-core` to this checkout.
- Checked: phone-core tests (3 formant, 2 pcm), ipa workspace tests,
  clippy clean (phone-core, ipa-core), fmt; seven ipa renders
  byte-identical before and after the move; `just verify` passed (includes
  the wasm check).

- 2026-11-12, phase 2: `phone_core::instrument` (`Waveform`, `Envelope`
  with `released_level_at`, `Sound` with `clamped`/`validate`/`customized`,
  `SoundChanges`, `Preset`, limits) and `phone_core::tone` (`Tone`,
  `sequence`/`together`/`groups_in_turn`, `peak_polyphony`, `validate`,
  typed `ToneError` whose messages are the browser's old strings, and
  `render` on FunDSP 0.23 with `default-features = false, features =
  ["std"]`), moved out of `mnemosyne-leptos/src/music_audio/{mod,
  instrument}.rs`, which now re-export them; the leptos side keeps
  `InstrumentSettings` (euterpe tuning), the Web Audio gain automation
  (`GainPoint`, `envelope_points`, `release_points`), and the engine
  (`VOICE_LEVEL` now from phone-core; `oscillator_type` mapping local).
  `pcm::Pcm`; `dto` (catalog, `preset`/`waveform` lookups, `write_wav`
  with `Existing::{Replace, Refuse}`), serde always, `schema` feature for
  JsonSchema. `phone-cli` (`tones`, `presets`; replaces existing files, as
  a command the user typed), `phone-mcp` (`phone.render_tones` refuses to
  replace files and requires `.wav`; `phone.presets`; rendering in
  `spawn_blocking`). Host: `src/phone.rs`, `src/mcp_server/phone.rs`,
  `ModuleId::Phone` after `Ekho`, `MNEMOSYNE_DISABLED_MODULES=phone`.
- Checked (phase 2): phone-core 22 tests (pitch by Goertzel, length incl.
  release, timing, filter darkening, chord limiting, determinism, every
  rejection), phone-cli 3, phone-mcp 2; mnemosyne-leptos 151 (157 before;
  the 6 moved tests now run in phone-core); host lib 295 (2 new);
  `scripts/dev-web.sh check-wasm`; clippy clean on the phone crates; `just
  verify` passed. Live: `phone tones` and `mnemosyne phone tones` wrote
  48 kHz 16-bit mono WAVs, bad preset and disabled module errors,
  `phone-mcp summary`. Firefox `/music` on the user's watcher-built bundle
  (built after the change): preset/waveform menus list phone-core's ids;
  "Try it" played and the scope measured 261.63 Hz (C4) then went flat; no
  error text. Not checked: listening by a person (samples per preset in
  `/tmp/phone-live-vbuJ/`), `phone-mcp` over real stdio (in-process tests
  only).

- 2026-11-12, phase 3: `phone-leptos` (`js_message`, `context`,
  `with_context(then, done)`, `play(pcm, done)` replacing the clip playing,
  `stop`); web-sys on every target (leptos already pulls it in), no Leptos
  dependency. `mnemosyne-leptos/src/music_audio/engine.rs` lost its own
  context and `js_message`; its `with_context` forwards to phone-leptos,
  and `stop_all` also stops a clip. ipa-core `dto::speech` returns `Pcm`
  (`speech_wav` is built on it, same bytes); ipa-leptos plays through
  `phone_leptos::play` (wasm-only dependency; dropped js-sys and the Blob,
  Url, and audio-element web-sys features). Patches for `phone-leptos`
  added next to `phone-core`'s (root, both UI manifests, ipa).
- Checked (phase 3): ipa-core 43 + 16, ipa-leptos 8, mnemosyne-leptos 151;
  clippy clean for phone-leptos and ipa-leptos on wasm32 and native; `just
  verify` passed. Firefox on the user's watcher bundle (built after the
  edits): `/ipa` "Say the transcription" then "Play æ" created 22050 Hz
  buffers (30981 and 12568 samples), started both, the second stopped the
  first, no blob URLs; `/music` "Try it" still measured C4. Not checked:
  counting contexts from the page (the glue holds its own constructor
  reference, so a page-level wrapper saw none), a page with IPA and music
  together, Chrome/Safari, hearing it.

- 2026-11-12, phase 4: `Pcm::resampled` (windowed sinc, 16 zero
  crossings, Blackman window, passband 0.95 of the lower Nyquist limit;
  exact integer output length). New `phone-speaker` (cpal 0.18.2, already
  in the lockfile via ekho-mic): `output_devices` (id, name, default),
  `play(pcm, device)` resamples to the device's default rate, writes every
  channel, blocks until played (+250 ms drain; gives up after length +
  5 s), `SpeakerError`. Devices are picked by id, then a unique exact name,
  then a unique part of an id or name; ambiguity lists ids (ALSA here gives
  95 devices, many sharing names). `phone-cli` feature `speaker`:
  `tones --play [--device]` (with or without `--out`; JSON gains
  `played`), `devices`; without the feature both say how to rebuild. Host
  feature `phone-speaker = ["phone-cli/speaker"]` (off by default); the
  descriptor lists `--play` and `phone devices` only in that build.
- Checked (phase 4): phone-core 24 (2 resampling: 22050→48000 matches a
  true 48 kHz sine within 1e-3; 48000→8000 removes a 6 kHz tone to under
  1 % instead of aliasing), phone-speaker 3, phone-cli 4 without and 3
  with `speaker`; clippy clean with the feature; host lib with and without
  `phone-speaker`; `just verify` (default features). Live: `phone devices`
  (95 unique ids); through a temporary PipeWire null sink (`PULSE_SINK`,
  device `pulse`; unloaded after, default sink unchanged) a 440 Hz tone
  rendered at 22050 Hz played at 48 kHz and the monitor recording had
  1.07 s of sound (1.08 expected), 440 Hz with 2nd harmonic ~60 dB down,
  peak 0.200 (= 0.25 × 0.8), identical channels; unknown and ambiguous
  `--device` errors through `mnemosyne phone`. Not checked: a person
  listening on real speakers, macOS/Windows hosts.

## Decisions

- Domain modules own the content: ipa-core keeps phonology, targets, and
  planning; euterpe keeps pitch and tuning. phone-core knows neither.
- The formant voice stays hand-written (per-sample parameter interpolation
  and a Klatt cascade); FunDSP is for music sounds. FunDSP 0.23
  (MIT/Apache) with `default-features = false, features = ["std"]` was
  checked to build for `wasm32-unknown-unknown` as a plain-export module
  (about 94 KB for a saw + low-pass probe).
- Types in this crate avoid the name `Phone` (ipa-core has
  `segment::Phone`).
- Rendered sound has no compressor (the browser bus has Web Audio's
  default one), so loud chords are quieter than in the browser rather
  than squashed; a 0.98-peak limiter stops clipping.
- Rendering limits beyond the browser's: sample rate 8–192 kHz, tones
  below half the rate, at most 300 s including release tails.
- Bevy is not used; a later Bevy app can host phone-core through an
  adapter because phone-core only produces samples.

## Next steps

Phase 5: real time in the browser. Start with a prototype of the Trunk
build for a second wasm artifact (`phone-worklet`, plain C exports, no
wasm-bindgen) loaded by a small AudioWorklet processor; only then move the
music keyboard onto it. Smaller options meanwhile: `ipa sound --play`
(ipa-cli depending on phone-speaker behind a feature), a host web route
rendering tones to WAV, an MCP tool for server playback (deliberately not
added: an agent making noise on the machine should be opt-in).
