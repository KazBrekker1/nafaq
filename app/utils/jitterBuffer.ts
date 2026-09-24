// Playback jitter buffer for one remote peer's decoded audio.
//
// Runs inside an AudioWorklet: packets are pushed as they arrive and the
// audio thread pulls exactly what it needs every render quantum, so playback
// is paced by the output clock rather than by packet arrival times.
// - Start (and restart after an underrun) waits until `target` is buffered.
// - An underrun raises the target; long stretches without one lower it.
// - Too much buffered (burst after a stall, or sender clock running fast)
//   drops the oldest audio back down to the target, so latency can't creep.
//
// Written without class fields or outside references: the worklet source is
// built from `PcmJitterBuffer.toString()` (see playbackWorkletSource).

export class PcmJitterBuffer {
  sampleRate: number;
  capacity: number;
  ring: Float32Array;
  readPos: number;
  size: number;
  target: number;
  minTarget: number;
  maxTarget: number;
  maxExcess: number;
  targetStep: number;
  relaxAfter: number;
  fadeLen: number;
  fadeRemaining: number;
  playing: boolean;
  stableSamples: number;
  underruns: number;
  droppedSamples: number;

  constructor(sampleRate: number) {
    const ms = (value: number) => Math.round((sampleRate * value) / 1000);
    this.sampleRate = sampleRate;
    this.capacity = ms(2000);
    this.ring = new Float32Array(this.capacity);
    this.readPos = 0;
    this.size = 0;
    this.minTarget = ms(40);
    this.maxTarget = ms(240);
    this.target = ms(60);
    this.maxExcess = ms(100);
    this.targetStep = ms(20);
    this.relaxAfter = sampleRate * 10;
    this.fadeLen = ms(5);
    this.fadeRemaining = 0;
    this.playing = false;
    this.stableSamples = 0;
    this.underruns = 0;
    this.droppedSamples = 0;
  }

  push(samples: Float32Array) {
    let input = samples;
    if (input.length > this.capacity) input = input.subarray(input.length - this.capacity);
    const overflow = this.size + input.length - this.capacity;
    if (overflow > 0) this.skip(overflow);
    let writePos = (this.readPos + this.size) % this.capacity;
    const first = Math.min(input.length, this.capacity - writePos);
    this.ring.set(input.subarray(0, first), writePos);
    if (first < input.length) this.ring.set(input.subarray(first), 0);
    this.size += input.length;
  }

  pull(out: Float32Array) {
    if (!this.playing) {
      if (this.size < this.target) {
        out.fill(0);
        return;
      }
      this.playing = true;
      this.fadeRemaining = this.fadeLen;
    }

    if (this.size > this.target + this.maxExcess) {
      this.skip(this.size - this.target);
      this.fadeRemaining = this.fadeLen;
    }

    const n = Math.min(out.length, this.size);
    for (let i = 0; i < n; i++) {
      let sample = this.ring[this.readPos]!;
      if (this.fadeRemaining > 0) {
        sample *= 1 - this.fadeRemaining / this.fadeLen;
        this.fadeRemaining--;
      }
      out[i] = sample;
      this.readPos = (this.readPos + 1) % this.capacity;
    }
    this.size -= n;

    if (n < out.length) {
      out.fill(0, n);
      this.playing = false;
      this.underruns++;
      this.stableSamples = 0;
      this.target = Math.min(this.maxTarget, this.target + this.targetStep);
      return;
    }

    this.stableSamples += out.length;
    if (this.stableSamples >= this.relaxAfter) {
      this.stableSamples = 0;
      this.target = Math.max(this.minTarget, this.target - Math.round(this.targetStep / 2));
    }
  }

  skip(count: number) {
    const n = Math.min(count, this.size);
    this.readPos = (this.readPos + n) % this.capacity;
    this.size -= n;
    this.droppedSamples += n;
  }

  reset() {
    this.readPos = 0;
    this.size = 0;
    this.playing = false;
  }
}

export const PLAYBACK_PROCESSOR_NAME = "nafaq-playback";

/** Source for the playback AudioWorklet module (one processor per peer). */
export function playbackWorkletSource() {
  return `
const PcmJitterBuffer = (${PcmJitterBuffer.toString()});
class NafaqPlaybackProcessor extends AudioWorkletProcessor {
  constructor() {
    super();
    this.buffer = new PcmJitterBuffer(sampleRate);
    this.port.onmessage = (event) => {
      const data = event.data;
      if (data && data.pcm) this.buffer.push(data.pcm);
      else if (data && data.reset) this.buffer.reset();
    };
  }
  process(_inputs, outputs) {
    const out = outputs[0] && outputs[0][0];
    if (out) this.buffer.pull(out);
    return true;
  }
}
registerProcessor(${JSON.stringify(PLAYBACK_PROCESSOR_NAME)}, NafaqPlaybackProcessor);
`;
}
