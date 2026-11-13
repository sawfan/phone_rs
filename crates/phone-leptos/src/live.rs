//! Live synthesis in the browser: phone's synthesizer running in an
//! AudioWorklet on the audio thread.
//!
//! The worklet is two files served next to the page's bundle: the
//! processor script (`worklet/phone-processor.js` in this crate) and the
//! `phone-worklet` WebAssembly module (built for `wasm32-unknown-unknown`
//! on its own). [`start`] loads them into the page's audio context and
//! creates the synthesizer node; [`send`] passes it [`Command`]s, encoded as
//! bytes by `phone-core`, so the page and the audio thread share one
//! definition of every command and sound.
//!
//! The synthesizer mixes [`MAX_BUSES`] buses, each to its own output, and
//! each output goes through a compressor (so chords do not clip) and an
//! analyser (for scopes and pitch readouts) to the speakers. Callers name
//! their buses; [`bus`] gives each name a slot for the commands.

use std::cell::RefCell;

use js_sys::{Array, ArrayBuffer, Object, Reflect, Uint8Array};
use phone_core::live::{Command, MAX_BUSES};
use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{
    AnalyserNode, AudioContext, AudioWorkletNode, AudioWorkletNodeOptions, MessageEvent, Response,
};

use crate::{js_message, with_context};

/// The processor name `phone-processor.js` registers.
const PROCESSOR: &str = "phone-synth";
/// Samples each analyser keeps: enough periods of low notes for pitch
/// measurement.
pub const ANALYSER_FFT_SIZE: u32 = 4_096;
/// How much each analyser's spectrum is averaged over time.
const ANALYSER_SMOOTHING: f64 = 0.6;

/// Where the worklet's two files are served.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkletAssets {
    /// URL of `phone-processor.js`.
    pub processor_url: String,
    /// URL of the `phone-worklet` module (`phone_worklet.wasm`).
    pub module_url: String,
}

type Waiter = Box<dyn FnOnce(Result<(), String>)>;

/// The synthesizer's node, each bus's analyser, and the handler for the
/// node's port messages (kept alive here).
struct Nodes {
    node: AudioWorkletNode,
    analysers: Vec<AnalyserNode>,
    _on_message: Closure<dyn FnMut(MessageEvent)>,
}

enum Live {
    Idle,
    /// Loading; these run once the synthesizer is ready or has failed.
    Loading(Vec<Waiter>),
    Ready {
        nodes: Nodes,
        sample_rate: f32,
    },
    Failed(String),
}

thread_local! {
    static LIVE: RefCell<Live> = const { RefCell::new(Live::Idle) };
    /// The nodes while loading (moved into `Ready` once ready).
    static LOADED: RefCell<Option<Nodes>> = const { RefCell::new(None) };
    /// Bus names, by slot.
    static BUSES: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

/// The slot for the bus called `name`, giving it the next free one the
/// first time. Slots last as long as the page; at most [`MAX_BUSES`].
pub fn bus(name: &str) -> Result<u8, String> {
    BUSES.with_borrow_mut(|buses| {
        if let Some(slot) = buses.iter().position(|bus| bus == name) {
            return Ok(slot as u8);
        }
        if buses.len() >= MAX_BUSES {
            return Err(format!(
                "At most {MAX_BUSES} sound buses can play on one page."
            ));
        }
        buses.push(name.to_string());
        Ok((buses.len() - 1) as u8)
    })
}

/// The slot for `name`, if it has one.
pub fn existing_bus(name: &str) -> Option<u8> {
    BUSES.with_borrow(|buses| {
        buses
            .iter()
            .position(|bus| bus == name)
            .map(|slot| slot as u8)
    })
}

/// Where the synthesizer is.
#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    Idle,
    Loading,
    /// Running at this sample rate.
    Ready(f32),
    Failed(String),
}

pub fn status() -> Status {
    LIVE.with_borrow(|live| match live {
        Live::Idle => Status::Idle,
        Live::Loading(_) => Status::Loading,
        Live::Ready { sample_rate, .. } => Status::Ready(*sample_rate),
        Live::Failed(message) => Status::Failed(message.clone()),
    })
}

/// Starts the synthesizer if it is not running, and hands the outcome to
/// `done` once it can play (at once when it already runs). Call from a user
/// gesture: it may create or resume the page's audio context. A failure is
/// kept, and later calls report it without retrying until the page reloads.
pub fn start(assets: &WorkletAssets, done: impl FnOnce(Result<(), String>) + 'static) {
    enum Next {
        Now(Result<(), String>),
        Wait,
        Load,
    }
    let mut done: Option<Waiter> = Some(Box::new(done));
    let next = LIVE.with_borrow_mut(|live| match live {
        Live::Ready { .. } => Next::Now(Ok(())),
        Live::Failed(message) => Next::Now(Err(message.clone())),
        Live::Loading(waiters) => {
            waiters.extend(done.take());
            Next::Wait
        }
        Live::Idle => {
            *live = Live::Loading(done.take().into_iter().collect());
            Next::Load
        }
    });
    match next {
        Next::Now(result) => {
            if let Some(done) = done {
                done(result);
            }
        }
        Next::Wait => {}
        Next::Load => {
            let assets = assets.clone();
            with_context(
                |_context| Ok(()),
                move |result| match result.and_then(|()| {
                    crate::context().ok_or_else(|| "Audio could not start.".to_string())
                }) {
                    Ok(context) => wasm_bindgen_futures::spawn_local(async move {
                        if let Err(message) = load(context, assets).await {
                            finish(Err(message));
                        }
                    }),
                    Err(message) => finish(Err(message)),
                },
            );
        }
    }
}

/// Ends loading: the waiters learn the outcome, and the state becomes
/// `Ready` (with the nodes from [`LOADED`]) or `Failed`.
fn finish(result: Result<f32, String>) {
    let waiters = LIVE.with_borrow_mut(|live| {
        let waiters = match std::mem::replace(live, Live::Idle) {
            Live::Loading(waiters) => waiters,
            other => {
                // Already settled (a second message); keep it.
                *live = other;
                return Vec::new();
            }
        };
        *live = match &result {
            Ok(sample_rate) => match LOADED.with_borrow_mut(Option::take) {
                Some(nodes) => Live::Ready {
                    nodes,
                    sample_rate: *sample_rate,
                },
                None => Live::Failed("the synthesizer node is missing".to_string()),
            },
            Err(message) => Live::Failed(message.clone()),
        };
        waiters
    });
    let outcome = result.map(|_| ());
    for waiter in waiters {
        waiter(outcome.clone());
    }
}

/// Connects the node's `output` through a compressor and an analyser to
/// the speakers, and returns the analyser.
fn bus_output(
    context: &AudioContext,
    node: &AudioWorkletNode,
    output: u32,
) -> Result<AnalyserNode, JsValue> {
    let compressor = context.create_dynamics_compressor()?;
    let analyser = context.create_analyser()?;
    analyser.set_fft_size(ANALYSER_FFT_SIZE);
    analyser.set_smoothing_time_constant(ANALYSER_SMOOTHING);
    node.connect_with_audio_node_and_output(&compressor, output)?;
    compressor.connect_with_audio_node(&analyser)?;
    analyser.connect_with_audio_node(&context.destination())?;
    Ok(analyser)
}

/// Turns a browser exception into "`what`: message".
fn fail(what: &'static str) -> impl Fn(JsValue) -> String {
    move |error| format!("{what}: {}", js_message(error))
}

async fn load(context: AudioContext, assets: WorkletAssets) -> Result<(), String> {
    let worklet = context
        .audio_worklet()
        .map_err(fail("This browser has no AudioWorklet"))?;
    JsFuture::from(
        worklet
            .add_module(&assets.processor_url)
            .map_err(fail("The synthesizer could not load"))?,
    )
    .await
    .map_err(fail("The synthesizer's processor could not load"))?;

    let window = web_sys::window().ok_or("no window")?;
    let response: Response = JsFuture::from(window.fetch_with_str(&assets.module_url))
        .await
        .map_err(fail("The synthesizer module could not be fetched"))?
        .dyn_into()
        .map_err(fail("Unexpected fetch result"))?;
    if !response.ok() {
        return Err(format!(
            "The synthesizer module could not be fetched: HTTP {}",
            response.status()
        ));
    }
    let bytes: ArrayBuffer = JsFuture::from(
        response
            .array_buffer()
            .map_err(fail("The synthesizer module could not be read"))?,
    )
    .await
    .map_err(fail("The synthesizer module could not be read"))?
    .dyn_into()
    .map_err(fail("Unexpected module bytes"))?;

    let options = AudioWorkletNodeOptions::new();
    options.set_number_of_inputs(0);
    options.set_number_of_outputs(MAX_BUSES as u32);
    let channels = Array::new();
    for _ in 0..MAX_BUSES {
        channels.push(&JsValue::from(1));
    }
    options.set_output_channel_count(&channels);
    let node = AudioWorkletNode::new_with_options(&context, PROCESSOR, &options)
        .map_err(fail("The synthesizer could not start"))?;
    let analysers = (0..MAX_BUSES)
        .map(|output| bus_output(&context, &node, output as u32))
        .collect::<Result<Vec<_>, _>>()
        .map_err(fail("The synthesizer could not connect"))?;

    let port = node
        .port()
        .map_err(fail("The synthesizer has no message port"))?;
    let on_message = Closure::<dyn FnMut(MessageEvent)>::new(|event: MessageEvent| {
        let data = event.data();
        let field = |name: &str| Reflect::get(&data, &JsValue::from_str(name)).ok();
        match field("type").and_then(|kind| kind.as_string()).as_deref() {
            Some("ready") => {
                let rate = field("sampleRate")
                    .and_then(|rate| rate.as_f64())
                    .unwrap_or(0.0);
                finish(Ok(rate as f32));
            }
            Some("error") => {
                let message = field("message")
                    .and_then(|message| message.as_string())
                    .unwrap_or_else(|| "unknown error".to_string());
                if matches!(status(), Status::Loading) {
                    finish(Err(format!("The synthesizer could not start: {message}")));
                } else {
                    LAST_ERROR.with_borrow_mut(|last| *last = Some(message));
                }
            }
            _ => {}
        }
    });
    port.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
    LOADED.with_borrow_mut(|loaded| {
        *loaded = Some(Nodes {
            node,
            analysers,
            _on_message: on_message,
        })
    });

    let message = Object::new();
    Reflect::set(&message, &"type".into(), &"module".into())
        .and_then(|_| Reflect::set(&message, &"bytes".into(), &bytes))
        .map_err(fail("The synthesizer module could not be sent"))?;
    port.post_message_with_transferable(&message, &Array::of1(&bytes))
        .map_err(fail("The synthesizer module could not be sent"))?;
    Ok(())
}

thread_local! {
    static LAST_ERROR: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// The last error the running synthesizer reported (such as a rejected
/// command), taken.
pub fn take_error() -> Option<String> {
    LAST_ERROR.with_borrow_mut(Option::take)
}

/// Sends `command` to the running synthesizer.
pub fn send(command: Command) -> Result<(), String> {
    LIVE.with_borrow(|live| match live {
        Live::Ready { nodes, .. } => {
            let port = nodes.node.port().map_err(js_message)?;
            let bytes = Uint8Array::from(command.encode().as_slice());
            port.post_message(&bytes).map_err(js_message)
        }
        Live::Failed(message) => Err(message.clone()),
        _ => Err("The synthesizer has not started.".to_string()),
    })
}

/// The analyser after `bus`'s output, once the synthesizer runs.
pub fn analyser(bus: u8) -> Option<AnalyserNode> {
    LIVE.with_borrow(|live| match live {
        Live::Ready { nodes, .. } => nodes.analysers.get(usize::from(bus)).cloned(),
        _ => None,
    })
}
