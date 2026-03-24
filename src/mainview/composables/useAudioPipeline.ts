import { ref } from "vue";

export function useAudioPipeline() {
  const encoding = ref(false);
  const decoding = ref(false);

  let encoder: AudioEncoder | null = null;
  let decoder: AudioDecoder | null = null;
  let audioCtx: AudioContext | null = null;
  let playbackCtx: AudioContext | null = null;
  let processorNode: ScriptProcessorNode | null = null;
  let sourceNode: MediaStreamAudioSourceNode | null = null;
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
    sourceNode = audioCtx.createMediaStreamSource(stream);
    processorNode = audioCtx.createScriptProcessor(1024, 1, 1);

    let frameCount = 0;

    processorNode.onaudioprocess = (event) => {
      if (!encoder || encoder.state !== "configured") return;
      const inputData = event.inputBuffer.getChannelData(0);
      const audioData = new AudioData({
        format: "f32-planar" as AudioSampleFormat,
        sampleRate: 48000,
        numberOfFrames: inputData.length,
        numberOfChannels: 1,
        timestamp: frameCount * (inputData.length / 48000) * 1_000_000,
        data: inputData,
      });
      frameCount++;
      encoder.encode(audioData);
      audioData.close();
    };

    sourceNode.connect(processorNode);
    processorNode.connect(audioCtx.destination);
    encoding.value = true;
    console.log("[audio-enc] Started encoding");
  }

  function stopEncoding() {
    if (processorNode) {
      processorNode.disconnect();
      processorNode = null;
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
