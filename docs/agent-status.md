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
5. **Done** (see below): the live synthesizer in an AudioWorklet, and
   Mnemosyne's music blocks on it. Original plan: real time in the browser: a `phone-worklet` wasm module with plain
   C-style exports (no wasm-bindgen, so no atomics/nightly or cross-origin
   isolation), a small JS AudioWorklet processor, commands over a
   MessagePort. Prototype the Trunk build of the second wasm artifact
   first. Then move the music keyboard and delete the Web Audio node graph
   (do not maintain both engines long-term).
6. **Done** (native): text-to-speech, streamed. Not done: streamed to the
   browser (needs the host's web server to stream responses).

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

- 2026-11-12, phase 5 prototype: `phone_core::live` (`Command` with a
  byte encoding, f64 sound values so it is exact; `DecodeError`; `Synth`
  with 48 voices, same-key restart, oldest dropped, output clamped);
  `tone::voice` shared by rendering and live voices (all six preset renders
  byte-identical to phase 2's). New `phone-worklet` (cdylib + rlib; exports
  `phone_init`, `phone_command_buffer`, `phone_command`, `phone_render`,
  `phone_active_voices`; 185,968 bytes, no imports). `phone-leptos`
  `worklet/phone-processor.js` (instantiates the module, queues commands
  until ready, copies frames to every channel) and `live` (`start` from a
  gesture, `send`, `status`, `analyser`, `take_error`; node → analyser →
  speakers). Mnemosyne: Trunk pre-build hook builds the module with its own
  target dir (`crates/mnemosyne-web/target/phone-worklet`, ignored by the
  watcher), `index.html` copies it and the processor script; `/phone` page
  (`crates/mnemosyne-leptos/src/phone.rs`: preset, 8 keys by pointer or
  Space/Enter, measured pitch, rendered comparison, stop); host route and
  `web` capability `/phone`, shell `demos::phone_html`.
- Decision (with the user): two wasm modules, not one. A worklet cannot
  share the page instance without shared memory; loading the bundle twice
  needs stubbed imports and a full app copy on the audio thread. Types are
  shared through phone-core's source and the `Command` encoding.
- Checked (phase 5 prototype): phone-core 29 (live matches rendered within
  1e-4; commands round-trip; bad bytes rejected; voice limits), phone-worklet
  1 (through the exports); clippy clean native and wasm32; module run in
  Node with an empty import object (440.1 Hz, peak 0.250);
  mnemosyne-leptos 151; host lib 296; `just verify`. Firefox `/phone` on
  the user's watcher bundle (the hook ran and both files were copied;
  served as `text/javascript` and `application/wasm`): a real click started
  it ("AudioWorklet at 48000 Hz"), A4 held measured 440.00 Hz and went
  silent after release, G4 on pluck 392.00 Hz. Not checked: Chrome/Safari,
  hearing it, glitch behaviour under main-thread load, the music keyboard
  (not moved yet).

- 2026-11-12, phase 5 completed: buses and timed notes. `Command` gained
  a bus (`MAX_BUSES` = 8; decode rejects others), `at` (audio-clock
  seconds) and `hold` on `NoteOn`, and `ReleaseBus`, `CutTimed`, `CutBus`,
  `CutAll`, `SetVolume` (`ReleaseAll` removed); `MAX_COMMAND_BYTES` 72.
  `Synth::render(start_frame, frames, out)` writes one block per bus;
  waiting notes (up to 1,024) start on their exact frame through a
  per-voice delay; cut voices fade over 30 ms; the output clamp was
  removed (buses have compressors). `phone-worklet` `phone_render(frames,
  start_frame)` and `phone_bus_count`; the processor passes `currentFrame`
  and fills one output per bus. `phone-leptos::live`: 8 outputs, each
  compressor → analyser → speakers; `bus(name)`/`existing_bus`;
  `analyser(bus)`. Mnemosyne's `music_audio/engine.rs` rewritten on it with
  the same public functions (keys hashed FNV-1a; held keys tracked so a key
  let go before the worklet is ready starts nothing; melodies sent whole 30
  ms ahead after `CutTimed`); the Web Audio oscillator graph and
  `instrument.rs`'s gain automation are deleted, with their web-sys
  features. Decision (user's suggestion, agreed): a bus id in commands
  (like MIDI channels) rather than one node per bus.
- Checked (phase 5 completed): phone-core 32 (a melody sent ahead matches
  the rendered one sample for sample from a non-block-aligned start; buses
  mixed separately with their volumes; cutting timed notes keeps held
  ones), phone-worklet 1, mnemosyne-leptos 149 (3 gain-automation tests
  removed with the code, 1 added); clippy clean for the phone crates
  (native, wasm32) and, for the wasm bundle, nothing in `phone.rs`,
  `music_audio/` or the phone crates (clippy-driver as `RUSTC_WRAPPER`
  over `crates/mnemosyne-web` in a throwaway target dir: a plain
  `cargo clippy --target wasm32` of mnemosyne-leptos cannot build, as it
  lacks the bundle's getrandom feature and WASI sysroot; the phase 5
  prototype's "wasm clippy clean" for `phone.rs` was not actually run);
  `scripts/dev-web.sh check-wasm`; `just verify`. Firefox `/music` (the
  watcher's bundle, built after the edits): "Try it" measured C4, E4, G4,
  C5 in turn; keyboard A4 held 440.00 Hz, silent after release, cut by
  "Stop all sound"; "Play both" beat around 442.5 Hz; no error text.
  `/phone` E4 329.63 Hz. Not checked: melody and notes blocks on block
  pages, several buses on one page, Chrome/Safari, hearing it.
- Noticed, not changed: `/music`'s "Stop all sound" silences the keyboard
  but its "Held" list keeps the key (it did before this change too).
- Disk: `/home` filled up during this work (64 KB free); with the user's
  go-ahead the three incremental caches were removed (`target/debug`,
  `crates/mnemosyne-web/target/wasm32-unknown-unknown/debug`,
  `crates/mnemosyne-leptos/target/debug`; 180 GB).

- 2026-11-12, phase 6: `phone_core::pcm::Resampler` (streaming; output
  identical to `Pcm::resampled`, which now uses it). New `phone-tts`
  (separate from phone-core so browser builds carry no HTTP client):
  `TtsConfig`/`BackendConfig::OpenAiCompatible(OpenAiSpeechConfig)` (serde;
  `PHONE_TTS_*`; defaults model `tts-1`, voice `alloy`, WAV), `decode`
  (`StreamDecoder`: WAV header parsed incrementally with chunks skipped and
  the data size ignored, 16-bit or float, mixed to mono; raw 16-bit PCM),
  `TextToSpeech` trait with a chunk callback returning `ControlFlow`,
  `Tts` (`from_config`, `from_env`, `speak`, `speak_to_pcm`), `openai`
  backend on ureq 3 (no `json` feature; body via serde_json; response read
  8 KiB at a time, capped at 128 MiB; text up to 4,096 characters, speed
  0.25–4). `phone-speaker` `Output` (open, push, finish; 0.25 s pre-roll;
  gaps counted; `play` built on it). `phone-cli` `say` and `tts`
  (`execute_with` takes a backend resolver; `execute` uses the
  environment). `phone-mcp` `phone.say` (new files only; the path is
  checked before the backend is asked), `phone.tts_backend`. Host:
  `[phone.tts]` in `MnemosyneFileConfig::phone` (else `PHONE_TTS_*`),
  `crate::phone::configured_tts`, `mnemosyne phone say|tts`, tools
  `phone_say`, `phone_tts_backend`; root README now documents phone.
- Checked (phase 6): phone-core 33 (pieces resample exactly as the whole
  for three rate pairs), phone-tts 12 (config, decoder split anywhere,
  streaming proven by a mock that holds back the rest until the first chunk
  arrives, run five times; raw PCM, stopping, errors), phone-speaker 3,
  phone-cli 6 (5 with `speaker`), phone-mcp 3, host lib 299; clippy clean
  with and without features. Live: a mock server streaming 1.5 s of 440 Hz
  WAV over 1.2 s, `phone say --play` through a temporary null sink: sound
  began 1.08 s before the last byte was sent, 1.51 s of 440 Hz, peak 0.5,
  no gaps (`gap_seconds` 0); `mnemosyne phone say --out` and `phone tts`
  against a PCM mock (model and voice sent as configured and requested);
  unconfigured and no-speaker errors. Not checked: a real TTS server
  (none installed here; Kokoro-FastAPI or openedai-speech would be the
  next check), MCP over real stdio.

- 2026-11-12, against a real server: Kokoro-FastAPI (the user's, Docker
  ROCm image, `127.0.0.1:8880`; 72 voices, models `kokoro`/`tts-1`/…;
  phone's defaults `tts-1`/`alloy` are accepted). `phone say --out` in WAV
  and PCM: a 4.2 s sentence in about 0.6–0.7 s; whisper (through ekho)
  transcribed both correctly apart from the name ("Namocini" for
  Mnemosyne). Streamed WAV from Kokoro is chunked, RIFF size u32::MAX, a
  `LIST` chunk before the data: decoded as designed. WAV and PCM results
  differ sample by sample, but so do two identical PCM requests (99,742 vs
  99,667 samples): Kokoro's output varies run to run. A 30.4 s passage:
  whole file ready after 6.5 s; with `--play` (temporary null sink) the
  first sound came after 4.5 s, no gaps, and the recording transcribed
  cleanly. Kokoro itself sends no audio for about 4 s (measured on the raw
  response, every format, with and without its `stream` option), then
  everything in a burst: it synthesizes a large first block before
  streaming. phone adds about 0.5 s (pre-roll and device).
- 2026-11-12, sentence by sentence (the user agreed): `phone_tts::sentences`
  (`split`; minimum 24 and maximum 400 characters per piece) and
  `Tts::speak` asking for one piece at a time, each once the one before has
  arrived (a stop ends the reply; a change of sample rate part way is an
  error); `Tts::speak_to_pcm` stays one request (files). phone-tts 19
  tests. Kokoro, the 30.4 s passage with `--play`: first sound after
  0.89 s (was 4.53 s), no gaps, command 31.1 s (was 35.2 s); a recording
  left running past the end transcribes completely. (An earlier recording
  looked truncated: the script killed `parecord`, which dropped its
  buffered tail; stopping it after a margin with SIGINT keeps everything.)

- 2026-11-12, speech in the browser (the user chose it over the assistant
  loop). Rather than stream responses from the host's single-threaded web
  server, the page pipelines sentences: `phone_tts::sentences::split` in
  wasm (phone-tts without `openai`), one local-only `POST /phone/say` per
  sentence (`src/web/phone_routes.rs`, also `GET /phone/tts`; JSON body up
  to 6 × 4,096 + 1,024 bytes; 400/503/502), WAV decoded with
  `phone_tts::decode`, and `phone_leptos::enqueue` (gapless: starts where
  the last queued clip ends; `play` now stops the queue and starts 20 ms
  ahead; `stop` stops all and bumps `stop_count`). The speech box is
  `crates/mnemosyne-leptos/src/phone_speech.rs` (text, voice, speed, Say,
  Stop speaking; a run ends on a newer Say, any stop, or leaving the page).
  Fixed along the way: the host's status line had no text for 502 ("502
  Unknown", ekho's backend failures included); now "Bad Gateway".
- Checked: host lib 301 (routes, local-only gate for `/phone/say`),
  mnemosyne-leptos 151, phone-leptos clippy (wasm, native), wasm bundle
  clippy (clippy-driver wrapper) with nothing in changed files,
  check-wasm, `just verify`. Throwaway server on 3199 with `PHONE_TTS_*`
  for Kokoro (stopped): `/phone/tts`, `POST /phone/say` 200 `audio/wav`,
  cross-site 403. Firefox `/phone`: the 3-sentence sample's first clip
  started 733 ms after the click, clips back to back with 0 s gaps (later
  ones queued 3.7 s and 7.9 s ahead); a Stop part way queued nothing more
  and said "Stopped." (found and fixed: the page's other Stop stopped the
  audio but the run went on queueing; now any stop ends it); an unknown
  voice showed Kokoro's 400 with its voice list. Re-checked after all the
  synth changes, on the user's watcher bundle: `/ipa` (phrase and letter
  clips at 22,050 Hz, the second replacing the first, no Blob URLs);
  `/music` (sequence C4–E4–G4–C5, held A4 440.00 Hz, Stop all, beating
  ≈442.5 Hz); `/demos/interactive/blocks` (notes block C4 E4 G4; melody
  block E4 F4 G4 F4 E4 D4 C4 on bus `music:demo`, read by its scope). Not
  checked: Chrome/Safari, the user's watcher server with
  `[phone.tts.backend]` configured (it has none yet).

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

- An assistant loop on the server: `ekho listen --wake` → act → `phone
  say --play`, with barge-in (stop speech when someone talks; the chunk
  callback already supports stopping).
- Smaller: block pages with several buses checked in the browser; `ipa
  sound --play`. Smaller options meanwhile: `ipa sound --play`
(ipa-cli depending on phone-speaker behind a feature), a host web route
rendering tones to WAV, an MCP tool for server playback (deliberately not
added: an agent making noise on the machine should be opt-in).
