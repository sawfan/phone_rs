//! Browser playback for phone.
//!
//! One `AudioContext` serves the page: browsers limit how many may exist,
//! and the Web Audio specification recommends one per document. It is
//! created by the first sound, which must come from a user gesture, and
//! everything that plays sound on the page (rendered clips here, the music
//! blocks' live voices in Mnemosyne) runs through [`with_context`].
//!
//! [`play`] plays a rendered [`Pcm`] clip; a new clip replaces the one
//! playing. The browser resamples clips to the context's rate.

use std::cell::RefCell;

use phone_core::Pcm;
use wasm_bindgen::JsValue;
use web_sys::{AudioBufferSourceNode, AudioContext, AudioContextState, AudioScheduledSourceNode};

#[derive(Default)]
struct Audio {
    context: Option<AudioContext>,
    /// The clip playing, stopped when the next starts.
    clip: Option<AudioBufferSourceNode>,
}

thread_local! {
    static AUDIO: RefCell<Audio> = RefCell::new(Audio::default());
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

/// Plays `pcm` now, stopping a clip already playing, and hands the outcome
/// to `done` (an empty clip is an error). Call from a user gesture.
pub fn play(pcm: Pcm, done: impl FnOnce(Result<(), String>) + 'static) {
    if pcm.samples.is_empty() {
        return done(Err("Nothing to play.".to_string()));
    }
    with_context(
        move |context| start_clip(context, &pcm).map_err(js_message),
        done,
    );
}

fn start_clip(context: &AudioContext, pcm: &Pcm) -> Result<(), JsValue> {
    let buffer = context.create_buffer(1, pcm.samples.len() as u32, pcm.sample_rate as f32)?;
    buffer.copy_to_channel(&pcm.samples, 0)?;
    let source = context.create_buffer_source()?;
    source.set_buffer(Some(&buffer));
    source.connect_with_audio_node(&context.destination())?;
    source.start()?;
    let previous = AUDIO.with_borrow_mut(|audio| audio.clip.replace(source));
    if let Some(previous) = previous {
        // Stopping a clip that already ended is allowed and does nothing.
        let _ = AudioScheduledSourceNode::stop(&previous);
    }
    Ok(())
}

/// Stops the clip playing, if any.
pub fn stop() {
    if let Some(clip) = AUDIO.with_borrow_mut(|audio| audio.clip.take()) {
        let _ = AudioScheduledSourceNode::stop(&clip);
    }
}
