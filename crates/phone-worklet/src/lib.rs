//! phone's live synthesizer for a browser AudioWorklet.
//!
//! The module imports nothing and exports a few plain functions, so the
//! worklet's processor (`phone-leptos/worklet/phone-processor.js`)
//! instantiates it with an empty import object. The processor:
//!
//! 1. calls [`phone_init`] with the audio thread's sample rate;
//! 2. for each command from the page (bytes written by
//!    `phone_core::live::Command::encode`), copies the bytes to
//!    [`phone_command_buffer`] and calls [`phone_command`];
//! 3. for each render quantum, calls [`phone_render`] with the quantum's
//!    first frame on the audio clock (`currentFrame`), and copies each bus's
//!    frames from the returned pointer into that bus's output
//!    ([`phone_bus_count`] outputs).
//!
//! The audio thread is single-threaded, so the state lives in a
//! thread-local.

use std::cell::RefCell;

use phone_core::live::{Command, MAX_BUSES, MAX_COMMAND_BYTES, Synth};

/// The most frames one [`phone_render`] call may ask for (a render quantum
/// is 128 frames today).
pub const MAX_FRAMES: usize = 4_096;

/// [`phone_command`]'s results.
pub const OK: i32 = 0;
pub const NOT_INITIALIZED: i32 = -1;
pub const BAD_COMMAND: i32 = -2;

struct State {
    synth: Option<Synth>,
    command: [u8; MAX_COMMAND_BYTES],
    output: Vec<f32>,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State {
        synth: None,
        command: [0; MAX_COMMAND_BYTES],
        output: vec![0.0; MAX_BUSES * MAX_FRAMES],
    });
}

/// Starts (or restarts, silently) the synthesizer at `sample_rate`.
#[unsafe(no_mangle)]
pub extern "C" fn phone_init(sample_rate: f32) {
    STATE.with_borrow_mut(|state| state.synth = Some(Synth::new(f64::from(sample_rate))));
}

/// Where the processor writes a command's bytes ([`MAX_COMMAND_BYTES`] of
/// room) before calling [`phone_command`].
#[unsafe(no_mangle)]
pub extern "C" fn phone_command_buffer() -> *mut u8 {
    STATE.with_borrow_mut(|state| state.command.as_mut_ptr())
}

/// Applies the command in the first `length` bytes of the command buffer:
/// [`OK`], [`NOT_INITIALIZED`], or [`BAD_COMMAND`] (undecodable, or longer
/// than the buffer).
#[unsafe(no_mangle)]
pub extern "C" fn phone_command(length: u32) -> i32 {
    STATE.with_borrow_mut(|state| {
        let length = length as usize;
        if length > MAX_COMMAND_BYTES {
            return BAD_COMMAND;
        }
        let Ok(command) = Command::decode(&state.command[..length]) else {
            return BAD_COMMAND;
        };
        match &mut state.synth {
            Some(synth) => {
                synth.apply(command);
                OK
            }
            None => NOT_INITIALIZED,
        }
    })
}

/// The buses, each rendered to its own output.
#[unsafe(no_mangle)]
pub extern "C" fn phone_bus_count() -> u32 {
    MAX_BUSES as u32
}

/// Renders `frames` frames from `start_frame` on the audio clock and
/// returns where they are: [`phone_bus_count`] blocks of `frames` samples,
/// one per bus. A null pointer before [`phone_init`] or when `frames`
/// exceeds [`MAX_FRAMES`]. The samples stay valid until the next call.
#[unsafe(no_mangle)]
pub extern "C" fn phone_render(frames: u32, start_frame: f64) -> *const f32 {
    STATE.with_borrow_mut(|state| {
        let frames = frames as usize;
        let State { synth, output, .. } = state;
        match synth {
            Some(synth) if frames <= MAX_FRAMES && start_frame >= 0.0 => {
                synth.render(start_frame as u64, frames, output);
                output.as_ptr()
            }
            _ => std::ptr::null(),
        }
    })
}

/// Voices sounding or fading out (0 before [`phone_init`]).
#[unsafe(no_mangle)]
pub extern "C" fn phone_active_voices() -> u32 {
    STATE.with_borrow(|state| {
        state
            .synth
            .as_ref()
            .map_or(0, |synth| synth.active_voices() as u32)
    })
}

#[cfg(test)]
mod tests {
    use phone_core::Preset;

    use super::*;

    fn send(command: Command) -> i32 {
        let bytes = command.encode();
        let buffer = phone_command_buffer();
        // SAFETY: the buffer holds MAX_COMMAND_BYTES, and every encoding
        // fits (checked in phone-core's tests).
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), buffer, bytes.len()) };
        phone_command(bytes.len() as u32)
    }

    /// Bus 0's samples for the next block, continuing the clock.
    fn render(frame: &mut f64, frames: u32) -> Vec<f32> {
        let samples = phone_render(frames, *frame);
        *frame += f64::from(frames);
        assert!(!samples.is_null());
        // SAFETY: phone_render returned MAX_BUSES blocks of `frames` valid
        // samples; bus 0 is the first.
        unsafe { std::slice::from_raw_parts(samples, frames as usize) }.to_vec()
    }

    #[test]
    fn plays_through_the_exports() {
        let note = Command::NoteOn {
            bus: 0,
            key: 1,
            hz: 440.0,
            level: 0.25,
            sound: Preset::Pure.sound(),
            at: 0.0,
            hold: 0.0,
        };
        assert_eq!(send(note), NOT_INITIALIZED);
        assert!(phone_render(128, 0.0).is_null());
        phone_init(48_000.0);
        assert_eq!(phone_bus_count(), MAX_BUSES as u32);
        assert_eq!(send(note), OK);
        let mut frame = 0.0;
        let block: Vec<f32> = (0..20).flat_map(|_| render(&mut frame, 128)).collect();
        assert!(block.iter().any(|s| s.abs() > 0.1));
        assert_eq!(phone_active_voices(), 1);
        assert_eq!(send(Command::ReleaseBus { bus: 0 }), OK);
        (0..100).for_each(|_| {
            render(&mut frame, 128);
        });
        assert_eq!(phone_active_voices(), 0);
        assert!(phone_render(MAX_FRAMES as u32 + 1, frame).is_null());
        assert_eq!(phone_command(MAX_COMMAND_BYTES as u32 + 1), BAD_COMMAND);
    }
}
