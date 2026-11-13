//! Browser playback for phone.
//!
//! One `AudioContext` serves the page: browsers limit how many may exist,
//! and the Web Audio specification recommends one per document. It is
//! created by the first sound, which must come from a user gesture, and
//! everything that plays sound on the page (rendered clips here, the music
//! blocks' live voices in Mnemosyne) runs through [`with_context`].
//!
//! [`play`] plays a rendered [`Pcm`] clip, replacing what is playing;
//! [`enqueue`] plays one after the clips already queued, starting exactly
//! where the last ends (gapless, for speech arriving a sentence at a time);
//! [`stop`] stops them all. The browser resamples clips to the context's
//! rate. [`live`]
//! runs phone's synthesizer in an AudioWorklet for notes played as they
//! happen.

pub mod live;

use std::cell::RefCell;

use phone_core::Pcm;
use wasm_bindgen::JsValue;
use web_sys::{AudioBufferSourceNode, AudioContext, AudioContextState, AudioScheduledSourceNode};

/// How far ahead of the audio clock a clip that starts "now" is scheduled,
/// so its first moments are not lost while the audio thread catches up.
const START_LEAD_SECONDS: f64 = 0.02;

#[derive(Default)]
struct Audio {
    context: Option<AudioContext>,
    /// Clips playing or waiting, with when each ends on the audio clock.
    clips: Vec<(AudioBufferSourceNode, f64)>,
}

thread_local! {
    static AUDIO: RefCell<Audio> = RefCell::new(Audio::default());
    /// How many times [`stop`] has run.
    static STOPS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// How many times [`stop`] has run on this page. Code queueing clips over
/// time (such as speech arriving a sentence at a time) notes it when it
/// begins and gives up once it changes, whoever stopped the sound.
pub fn stop_count() -> u64 {
    STOPS.get()
}

/// A readable message from a browser exception.
pub fn js_message(error: JsValue) -> String {
    error
        .as_string()
        .or_else(|| {
            js_sys::Reflect::get(&error, &JsValue::from_str("message"))
                .ok()
                .and_then(|message| message.as_string())
        })
        .unwrap_or_else(|| "unknown browser error".to_string())
}

/// The page's audio context, if something has already created it.
pub fn context() -> Option<AudioContext> {
    AUDIO.with_borrow(|audio| audio.context.clone())
}

/// Runs `then` with a running audio context, creating it if needed and
/// resuming it first if the browser suspended it, and hands the outcome to
/// `done`. Call from a user gesture: browsers only start audio then.
pub fn with_context(
    then: impl FnOnce(&AudioContext) -> Result<(), String> + 'static,
    done: impl FnOnce(Result<(), String>) + 'static,
) {
    let context = AUDIO.with_borrow_mut(|audio| -> Result<AudioContext, String> {
        if let Some(context) = &audio.context {
            return Ok(context.clone());
        }
        let context = AudioContext::new().map_err(|error| {
            format!("This browser could not start audio: {}", js_message(error))
        })?;
        audio.context = Some(context.clone());
        Ok(context)
    });
    let context = match context {
        Ok(context) => context,
        Err(error) => return done(Err(error)),
    };
    if context.state() == AudioContextState::Running {
        return done(then(&context));
    }
    // Resume during the gesture, then play once the context runs.
    let resumed = context.resume();
    wasm_bindgen_futures::spawn_local(async move {
        let result = match resumed {
            Ok(promise) => wasm_bindgen_futures::JsFuture::from(promise)
                .await
                .map(|_| ())
                .map_err(js_message),
            Err(error) => Err(js_message(error)),
        }
        .map_err(|error| format!("Audio could not start: {error}"))
        .and_then(|()| then(&context));
        done(result);
    });
}

/// Plays `pcm` now, stopping clips already playing or queued, and hands
/// the outcome to `done` (an empty clip is an error). Call from a user
/// gesture.
pub fn play(pcm: Pcm, done: impl FnOnce(Result<(), String>) + 'static) {
    if pcm.samples.is_empty() {
        return done(Err("Nothing to play.".to_string()));
    }
    with_context(
        move |context| {
            stop();
            schedule_clip(context, &pcm, false).map_err(js_message)
        },
        done,
    );
}

/// Plays `pcm` once the clips already queued have played (now, if none
/// is left), with no gap between them, and hands the outcome to `done`.
/// The first clip of a sequence must come from a user gesture, or after
/// one has started the audio context.
pub fn enqueue(pcm: Pcm, done: impl FnOnce(Result<(), String>) + 'static) {
    if pcm.samples.is_empty() {
        return done(Err("Nothing to play.".to_string()));
    }
    with_context(
        move |context| schedule_clip(context, &pcm, true).map_err(js_message),
        done,
    );
}

/// When the queued clips end on the audio clock, if any are left.
pub fn queued_until() -> Option<f64> {
    let now = context()?.current_time();
    AUDIO.with_borrow(|audio| {
        audio
            .clips
            .iter()
            .map(|(_, end)| *end)
            .fold(None, |latest: Option<f64>, end| {
                Some(latest.map_or(end, |l| l.max(end)))
            })
            .filter(|end| *end > now)
    })
}

fn schedule_clip(context: &AudioContext, pcm: &Pcm, after_queue: bool) -> Result<(), JsValue> {
    let buffer = context.create_buffer(1, pcm.samples.len() as u32, pcm.sample_rate as f32)?;
    buffer.copy_to_channel(&pcm.samples, 0)?;
    let source = context.create_buffer_source()?;
    source.set_buffer(Some(&buffer));
    source.connect_with_audio_node(&context.destination())?;
    let now = context.current_time();
    let start = AUDIO.with_borrow_mut(|audio| {
        // Forget clips that have finished.
        audio.clips.retain(|(_, end)| *end > now);
        let queued = audio.clips.iter().map(|(_, end)| *end).fold(0.0, f64::max);
        let soonest = now + START_LEAD_SECONDS;
        if after_queue {
            queued.max(soonest)
        } else {
            soonest
        }
    });
    source.start_with_when(start)?;
    let end = start + buffer.duration();
    AUDIO.with_borrow_mut(|audio| audio.clips.push((source, end)));
    Ok(())
}

/// Stops every clip playing or queued.
pub fn stop() {
    STOPS.set(STOPS.get() + 1);
    let clips = AUDIO.with_borrow_mut(|audio| std::mem::take(&mut audio.clips));
    for (clip, _) in clips {
        // Stopping a clip that already ended is allowed and does nothing.
        let _ = AudioScheduledSourceNode::stop(&clip);
    }
}
