import { describe, expect, it } from "vitest";
import { PcmJitterBuffer, PLAYBACK_PROCESSOR_NAME, playbackWorkletSource } from "./jitterBuffer";

const RATE = 48_000;
const QUANTUM = 128;
const FRAME = 960; // 20 ms

function frame(value: number, length = FRAME) {
  return new Float32Array(length).fill(value);
}

function pull(buffer: PcmJitterBuffer, length = QUANTUM) {
  const out = new Float32Array(length);
  buffer.pull(out);
  return out;
}

describe("PcmJitterBuffer", () => {
  it("stays silent until the target is buffered, then plays in order", () => {
    const buffer = new PcmJitterBuffer(RATE);
    buffer.push(frame(0.1));
    expect(pull(buffer).every((s) => s === 0)).toBe(true);

    buffer.push(frame(0.2));
    buffer.push(frame(0.3)); // 60 ms buffered = initial target
    const out = pull(buffer, FRAME * 3);
    expect(out[FRAME - 1]).toBeCloseTo(0.1);
    expect(out[FRAME + 10]).toBeCloseTo(0.2);
    expect(out[FRAME * 3 - 1]).toBeCloseTo(0.3);
  });

  it("fades in on start to avoid a click", () => {
    const buffer = new PcmJitterBuffer(RATE);
    for (let i = 0; i < 3; i++) buffer.push(frame(1));
    const out = pull(buffer);
    expect(out[0]).toBeLessThan(0.1);
    expect(out[QUANTUM - 1]).toBeGreaterThan(0.4);
  });

  it("raises the target after an underrun and re-buffers before resuming", () => {
    const buffer = new PcmJitterBuffer(RATE);
    for (let i = 0; i < 3; i++) buffer.push(frame(0.5));
    const initialTarget = buffer.target;
    pull(buffer, FRAME * 3 + 10); // runs dry
    expect(buffer.underruns).toBe(1);
    expect(buffer.target).toBeGreaterThan(initialTarget);

    buffer.push(frame(0.5)); // below the new target: still silent
    expect(pull(buffer).every((s) => s === 0)).toBe(true);
  });

  it("drops the oldest audio when a burst exceeds the latency budget", () => {
    const buffer = new PcmJitterBuffer(RATE);
    for (let i = 0; i < 20; i++) buffer.push(frame(i / 100)); // 400 ms burst
    pull(buffer);
    // Latency is pulled back to about the target, not 400 ms.
    expect(buffer.size).toBeLessThanOrEqual(buffer.target);
    expect(buffer.droppedSamples).toBeGreaterThan(0);
    // What survives is the newest audio.
    expect(pull(buffer, buffer.size).at(-1)).toBeCloseTo(0.19);
  });

  it("relaxes the target after a long stable stretch", () => {
    const buffer = new PcmJitterBuffer(RATE);
    buffer.target = buffer.maxTarget;
    const start = buffer.target;
    for (let i = 0; i < 520; i++) {
      buffer.push(frame(0.1));
      pull(buffer, FRAME);
    }
    expect(buffer.target).toBeLessThan(start);
  });

  it("wraps around the ring without corrupting order", () => {
    const buffer = new PcmJitterBuffer(RATE);
    for (let i = 0; i < 400; i++) {
      buffer.push(frame((i % 7) / 10));
      pull(buffer, FRAME);
    }
    buffer.push(frame(0.9));
    const out = pull(buffer, buffer.size);
    expect(out.at(-1)).toBeCloseTo(0.9);
  });
});

describe("playbackWorkletSource", () => {
  it("registers a processor that pulls from the jitter buffer", () => {
    let registered: any = null;
    class FakeProcessor {
      port = { onmessage: null as ((event: { data: unknown }) => void) | null };
    }
    const run = new Function(
      "AudioWorkletProcessor",
      "registerProcessor",
      "sampleRate",
      playbackWorkletSource(),
    );
    run(FakeProcessor, (name: string, ctor: unknown) => {
      registered = { name, ctor };
    }, RATE);

    expect(registered.name).toBe(PLAYBACK_PROCESSOR_NAME);
    const processor = new registered.ctor();
    for (let i = 0; i < 3; i++) processor.port.onmessage({ data: { pcm: frame(0.25) } });
    const out = new Float32Array(QUANTUM);
    expect(processor.process([], [[out]])).toBe(true);
    processor.process([], [[out]]); // past the 5 ms fade-in
    expect(out[QUANTUM - 1]).toBeCloseTo(0.25);
  });
});
