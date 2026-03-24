import { ref } from "vue";

// AudioWorklet processor code — runs off the main thread.
// Inlined as a Blob URL to avoid Vite asset resolution complexity.
const WORKLET_CODE = `
class AudioCaptureProcessor extends AudioWorkletProcessor {
  constructor() {
    super();
    this._frameCount = 0;
  }
  process(inputs) {
    const input = inputs[0];
    if (!input || !input[0] || input[0].length === 0) return true;
    const samples = new Float32Array(input[0]);
    this.port.postMessage({
      samples,
      frameCount: this._frameCount,
      numFrames: samples.length,
    });
    this._frameCount++;
    return true;
  }
}
registerProcessor("audio-capture-processor", AudioCaptureProcessor);
`;

function createWorkletUrl(): string {
  const blob = new Blob([WORKLET_CODE], { type: "application/javascript" });
  return URL.createObjectURL(blob);
}

export function useAudioPipeline() {
  const encoding = ref(false);
  const decoding = ref(false);

  let encoder: AudioEncoder | null = null;
  let decoder: AudioDecoder | null = null;
  let audioCtx: AudioContext | null = null;
  let playbackCtx: AudioContext | null = null;
  let workletNode: AudioWorkletNode | null = null;
  let sourceNode: MediaStreamAudioSourceNode | null = null;
  let workletBlobUrl: string | null = null;
  let onEncodedCallback: ((data: Uint8Array) => void) | null = null;
  let nextPlayTime = 0;

  async function startEncoding(
    stream: MediaStream,
    onEncoded: (data: Uint8Array) => void,
  ) {
    onEncodedCallback = onEncoded;

    encoder = new AudioEncoder({
      output: (chunk: EncodedAudioChunk) => {
        const buf = new Uint8Array(chunk.byteLength);
        chunk.copyTo(buf);
        onEncodedCallback?.(buf);
      },
      error: (e) => console.error("[audio-enc] Error:", e),
    });

    encoder.configure({
      codec: "opus",
      sampleRate: 48000,
      numberOfChannels: 1,
      bitrate: 32000,
    });

    audioCtx = new AudioContext({ sampleRate: 48000 });

    workletBlobUrl = createWorkletUrl();
    await audioCtx.audioWorklet.addModule(workletBlobUrl);

    sourceNode = audioCtx.createMediaStreamSource(stream);
    workletNode = new AudioWorkletNode(audioCtx, "audio-capture-processor");

    let frameCount = 0;

    workletNode.port.onmessage = (event) => {
      if (!encoder || encoder.state !== "configured") return;

      const { samples, numFrames } = event.data as {
        samples: Float32Array;
        numFrames: number;
      };

      const data = new Float32Array(new ArrayBuffer(numFrames * 4));
      data.set(samples);

      const audioData = new AudioData({
        format: "f32-planar" as AudioSampleFormat,
        sampleRate: 48000,
        numberOfFrames: numFrames,
        numberOfChannels: 1,
        timestamp: frameCount * (numFrames / 48000) * 1_000_000,
        data,
      });
      frameCount++;
      encoder.encode(audioData);
      audioData.close();
    };

    sourceNode.connect(workletNode);
    workletNode.connect(audioCtx.destination);
    encoding.value = true;
    console.log("[audio-enc] Started encoding via AudioWorklet");
  }

  function stopEncoding() {
    if (workletNode) {
      workletNode.port.onmessage = null;
      workletNode.disconnect();
      workletNode = null;
    }
    if (sourceNode) {
      sourceNode.disconnect();
      sourceNode = null;
    }
    if (encoder) {
      if (encoder.state !== "closed") encoder.close();
      encoder = null;
    }
    if (audioCtx) {
      audioCtx.close();
      audioCtx = null;
    }
    if (workletBlobUrl) {
      URL.revokeObjectURL(workletBlobUrl);
      workletBlobUrl = null;
    }
    encoding.value = false;
  }

  function startDecoding() {
    playbackCtx = new AudioContext({ sampleRate: 48000 });
    nextPlayTime = playbackCtx.currentTime;

    decoder = new AudioDecoder({
      output: (audioData: AudioData) => {
        if (!playbackCtx) return;
        const buffer = playbackCtx.createBuffer(
          audioData.numberOfChannels,
          audioData.numberOfFrames,
          audioData.sampleRate,
        );
        for (let ch = 0; ch < audioData.numberOfChannels; ch++) {
          const channelData = new Float32Array(audioData.numberOfFrames);
          audioData.copyTo(channelData, { planeIndex: ch });
          buffer.copyToChannel(channelData, ch);
        }
        audioData.close();

        const source = playbackCtx.createBufferSource();
        source.buffer = buffer;
        source.connect(playbackCtx.destination);
        const now = playbackCtx.currentTime;
        if (nextPlayTime < now) nextPlayTime = now;
        source.start(nextPlayTime);
        nextPlayTime += buffer.duration;
      },
      error: (e) => console.error("[audio-dec] Error:", e),
    });

    decoder.configure({
      codec: "opus",
      sampleRate: 48000,
      numberOfChannels: 1,
    });
    decoding.value = true;
    console.log("[audio-dec] Started decoding");
  }

  function decodeChunk(data: Uint8Array, timestamp: number) {
    if (!decoder || decoder.state !== "configured") return;
    const chunk = new EncodedAudioChunk({
      type: "key",
      timestamp: timestamp * 1000,
      data: data,
    });
    decoder.decode(chunk);
  }

  function stopDecoding() {
    if (decoder) {
      if (decoder.state !== "closed") decoder.close();
      decoder = null;
    }
    if (playbackCtx) {
      playbackCtx.close();
      playbackCtx = null;
    }
    decoding.value = false;
  }

  function stop() {
    stopEncoding();
    stopDecoding();
  }

  return {
    encoding,
    decoding,
    startEncoding,
    stopEncoding,
    startDecoding,
    stopDecoding,
    decodeChunk,
    stop,
  };
}
