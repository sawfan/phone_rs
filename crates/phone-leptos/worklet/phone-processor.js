// phone's AudioWorklet processor: a fixed shim around the `phone-worklet`
// WebAssembly module, which does all the synthesis.
//
// Messages from the page (phone_leptos::live):
//   { type: "module", bytes: ArrayBuffer }  the module, once; answered with
//                                           { type: "ready", sampleRate } or
//                                           { type: "error", message }
//   Uint8Array                              a command, encoded by
//                                           phone_core::live::Command::encode
// Commands that arrive before the module is ready are kept and applied
// once it is. Each bus renders to its own output (the node is created with
// phone_bus_count() outputs); times in commands are on the audio clock,
// which `currentFrame` counts.

const MAX_COMMAND_BYTES = 72;

class PhoneSynthProcessor extends AudioWorkletProcessor {
  constructor() {
    super();
    this.wasm = null;
    this.pending = [];
    this.port.onmessage = (event) => this.receive(event.data);
  }

  receive(data) {
    if (data && data.type === "module") {
      WebAssembly.instantiate(data.bytes, {}).then(
        ({ instance }) => {
          this.wasm = instance.exports;
          this.wasm.phone_init(sampleRate);
          for (const bytes of this.pending) this.command(bytes);
          this.pending = [];
          this.port.postMessage({ type: "ready", sampleRate });
        },
        (error) => this.port.postMessage({ type: "error", message: String(error) }),
      );
    } else if (data instanceof Uint8Array) {
      if (this.wasm) this.command(data);
      else this.pending.push(data);
    }
  }

  command(bytes) {
    if (bytes.length > MAX_COMMAND_BYTES) {
      this.port.postMessage({ type: "error", message: "command too long" });
      return;
    }
    const exports = this.wasm;
    // A fresh view each time: the module's memory may have grown.
    new Uint8Array(exports.memory.buffer, exports.phone_command_buffer(), bytes.length).set(bytes);
    const result = exports.phone_command(bytes.length);
    if (result !== 0) {
      this.port.postMessage({ type: "error", message: `command rejected (${result})` });
    }
  }

  process(_inputs, outputs) {
    if (!this.wasm || outputs.length === 0 || outputs[0].length === 0) return true;
    const frames = outputs[0][0].length;
    const pointer = this.wasm.phone_render(frames, currentFrame);
    if (pointer === 0) return true;
    const buses = Math.min(outputs.length, this.wasm.phone_bus_count());
    const all = new Float32Array(this.wasm.memory.buffer, pointer, buses * frames);
    for (let bus = 0; bus < buses; bus++) {
      const samples = all.subarray(bus * frames, (bus + 1) * frames);
      for (const channel of outputs[bus]) channel.set(samples);
    }
    return true;
  }
}

registerProcessor("phone-synth", PhoneSynthProcessor);
