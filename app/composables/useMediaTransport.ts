// Media transport: raw frames → Rust codec pipeline.
// Audio: AudioWorklet → buffer 960 samples → Int16 PCM → invoke("send_audio") → Rust Opus encode
// Video: Canvas capture rVFC → getImageData RGBA → invoke("send_video") → Rust H.264 encode
// Receive: Rust decodes → H.264 NALUs via WebCodecs / PCM Int16 → JS playback

const encoding = ref(false);
const peerSpeaking = ref(false);
const connectionQuality = ref<"good" | "degraded" | "poor">("good");
const OPUS_FRAME_SAMPLES = 960; // 20ms at 48kHz
const SPEAKING_RMS_THRESHOLD = 0.015; // ~-36 dBFS
const SPEAKING_DEBOUNCE_MS = 300;

let playbackCtx: AudioContext | null = null;
let captureCtx: AudioContext | null = null;
let captureVideoEl: HTMLVideoElement | null = null;
let captureInterval: ReturnType<typeof setInterval> | null = null;
let workletNode: AudioWorkletNode | null = null;
let sourceNode: MediaStreamAudioSourceNode | null = null;
let nextPlayTime = 0;
let unlistenAudio: (() => void) | null = null;
let unlistenVideo: (() => void) | null = null;
let remoteCanvas: HTMLCanvasElement | null = null;
let speakingTimeout: ReturnType<typeof setTimeout> | null = null;
let videoFrameTimestamps: number[] = [];
let qualityInterval: ReturnType<typeof setInterval> | null = null;
let videoDecoder: VideoDecoder | null = null;
let currentWidth = 640;
let currentHeight = 480;
let targetFps = 15;

// --- IPC helpers ---

const isAndroid = /android/i.test(navigator.userAgent);

function toBase64(bytes: Uint8Array): string {
  let binary = "";
  const chunk = 8192;
  for (let i = 0; i < bytes.length; i += chunk) {
    binary += String.fromCharCode.apply(null, bytes.subarray(i, i + chunk) as unknown as number[]);
  }
  return btoa(binary);
}

function fromBase64(b64: string): Uint8Array {
  const binary = atob(b64);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) {
    bytes[i] = binary.charCodeAt(i);
  }
  return bytes;
}

function packVideoPayload(
  peerId: string, width: number, height: number,
  keyframe: boolean, timestamp: number, rgba: Uint8Array,
): Uint8Array {
  const peerIdBytes = new TextEncoder().encode(peerId);
  const headerSize = 1 + peerIdBytes.length + 4 + 4 + 1 + 8;
  const payload = new Uint8Array(headerSize + rgba.length);
  const view = new DataView(payload.buffer);
  let offset = 0;
  payload[offset] = peerIdBytes.length; offset += 1;
  payload.set(peerIdBytes, offset); offset += peerIdBytes.length;
  view.setUint32(offset, width, true); offset += 4;
  view.setUint32(offset, height, true); offset += 4;
  payload[offset] = keyframe ? 1 : 0; offset += 1;
  view.setBigUint64(offset, BigInt(timestamp), true); offset += 8;
  payload.set(rgba, offset);
  return payload;
}

function packAudioPayload(
  peerId: string, timestamp: number, pcm: Uint8Array,
): Uint8Array {
  const peerIdBytes = new TextEncoder().encode(peerId);
  const headerSize = 1 + peerIdBytes.length + 8;
  const payload = new Uint8Array(headerSize + pcm.length);
  const view = new DataView(payload.buffer);
  let offset = 0;
  payload[offset] = peerIdBytes.length; offset += 1;
  payload.set(peerIdBytes, offset); offset += peerIdBytes.length;
  view.setBigUint64(offset, BigInt(timestamp), true); offset += 8;
  payload.set(pcm, offset);
  return payload;
}

function detectKeyframe(nalus: Uint8Array): boolean {
  let i = 0;
  while (i < nalus.length - 4) {
    if (nalus[i] === 0 && nalus[i + 1] === 0) {
      let headerIdx: number;
      if (nalus[i + 2] === 1) {
        headerIdx = i + 3;
      } else if (nalus[i + 2] === 0 && nalus[i + 3] === 1) {
        headerIdx = i + 4;
      } else { i++; continue; }
      if (headerIdx < nalus.length) {
        const nalType = nalus[headerIdx]! & 0x1f;
        if (nalType === 5) return true;
      }
      i = headerIdx;
    } else { i++; }
  }
  return false;
}

function resolveCaptureDimensions(stream?: MediaStream | null) {
  const isMobile = /android/i.test(navigator.userAgent);
  const videoTrack = stream?.getVideoTracks()[0];
  const maxW = isMobile ? 320 : 640;
  const maxH = isMobile ? 240 : 480;
  return {
    width: videoTrack
      ? Math.min(videoTrack.getSettings().width || maxW, maxW)
      : maxW,
    height: videoTrack
      ? Math.min(videoTrack.getSettings().height || maxH, maxH)
      : maxH,
  };
}

export function useMediaTransport() {
  // Must be called before sending or receiving — the decoder is needed
  // even when the local camera is unavailable, so this cannot live inside startSending.
  async function initCodecs(stream?: MediaStream | null) {
    const { invoke } = await import("@tauri-apps/api/core");
    const { width, height } = resolveCaptureDimensions(stream);
    await invoke("init_codecs", { width, height });
  }

  function initVideoDecoder(canvas: HTMLCanvasElement) {
    if (typeof globalThis.VideoDecoder === "undefined") {
      console.warn("WebCodecs VideoDecoder not available — video disabled");
      return;
    }
    const ctx = canvas.getContext("2d")!;
    videoDecoder = new VideoDecoder({
      output: (frame: VideoFrame) => {
        if (canvas.width !== frame.displayWidth) canvas.width = frame.displayWidth;
        if (canvas.height !== frame.displayHeight) canvas.height = frame.displayHeight;
        ctx.drawImage(frame, 0, 0);
        frame.close();
      },
      error: (e: any) => console.warn("VideoDecoder error:", e),
    });
    videoDecoder.configure({
      codec: "avc1.42001e",
      optimizeForLatency: true,
    });
  }

  async function startSending(stream: MediaStream, getPeerIds: () => string[]) {
    if (encoding.value) return;
    encoding.value = true;

    const { invoke } = await import("@tauri-apps/api/core");
    const { width, height } = resolveCaptureDimensions(stream);

    // --- Audio ---
    const audioTrack = stream.getAudioTracks()[0];
    if (audioTrack) {
      captureCtx = new AudioContext({ sampleRate: 48000 });
      const WORKLET_CODE = `
        class CaptureProcessor extends AudioWorkletProcessor {
          process(inputs) {
            const ch = inputs[0]?.[0];
            if (ch && ch.length > 0) {
              this.port.postMessage({ samples: new Float32Array(ch) });
            }
            return true;
          }
        }
        registerProcessor("capture", CaptureProcessor);
      `;
      const blobUrl = URL.createObjectURL(
        new Blob([WORKLET_CODE], { type: "application/javascript" }),
      );
      await captureCtx.audioWorklet.addModule(blobUrl);
      URL.revokeObjectURL(blobUrl);

      sourceNode = captureCtx.createMediaStreamSource(
        new MediaStream([audioTrack]),
      );
      workletNode = new AudioWorkletNode(captureCtx, "capture");

      // Buffer 128-sample worklet chunks into 960-sample Opus frames
      const sampleBuffer = new Float32Array(OPUS_FRAME_SAMPLES);
      let bufferOffset = 0;

      workletNode.port.onmessage = (event) => {
        const { samples } = event.data as { samples: Float32Array };
        let srcOffset = 0;

        while (srcOffset < samples.length) {
          const remaining = OPUS_FRAME_SAMPLES - bufferOffset;
          const toCopy = Math.min(remaining, samples.length - srcOffset);
          sampleBuffer.set(
            samples.subarray(srcOffset, srcOffset + toCopy),
            bufferOffset,
          );
          bufferOffset += toCopy;
          srcOffset += toCopy;

          if (bufferOffset === OPUS_FRAME_SAMPLES) {
            // Convert Float32 → Int16 LE
            const pcm = new Int16Array(OPUS_FRAME_SAMPLES);
            for (let i = 0; i < OPUS_FRAME_SAMPLES; i++) {
              pcm[i] = Math.max(
                -32768,
                Math.min(32767, Math.round(sampleBuffer[i]! * 32767)),
              );
            }
            const pcmBytes = new Uint8Array(pcm.buffer);
            const ts = Date.now();
            for (const pid of getPeerIds()) {
              if (isAndroid) {
                invoke("send_audio", {
                  peerId: pid,
                  data: toBase64(pcmBytes),
                  timestamp: ts,
                }).catch(() => {});
              } else {
                invoke("send_audio", packAudioPayload(pid, ts, pcmBytes), {
                  headers: { "Content-Type": "application/octet-stream" },
                }).catch(() => {});
              }
            }
            bufferOffset = 0;
          }
        }
      };

      sourceNode.connect(workletNode);
      workletNode.connect(captureCtx.destination);
    }

    // --- Video ---
    const videoTrack = stream.getVideoTracks()[0];
    if (videoTrack) {
      if (captureVideoEl) {
        captureVideoEl.pause();
        captureVideoEl.srcObject = null;
      }
      captureVideoEl = document.createElement("video");
      captureVideoEl.srcObject = stream;
      captureVideoEl.muted = true;
      captureVideoEl.play().catch(() => {});

      const canvas = new OffscreenCanvas(width, height);
      const ctx = canvas.getContext("2d")!;

      targetFps = 15;
      let lastCaptureTime = 0;
      let vFrameCount = 0;

      function captureLoop(_now: number, metadata: any) {
        if (!encoding.value || !captureVideoEl) return;
        const mediaTime = (metadata?.mediaTime ?? _now / 1000) * 1000;
        const elapsed = mediaTime - lastCaptureTime;
        if (elapsed >= 1000 / targetFps) {
          lastCaptureTime = mediaTime;
          ctx.drawImage(captureVideoEl!, 0, 0, width, height);
          const imageData = ctx.getImageData(0, 0, width, height);
          const keyframe = vFrameCount === 0 || vFrameCount % 30 === 0;
          vFrameCount++;
          const rgba = new Uint8Array(imageData.data.buffer);
          const ts = Date.now();
          for (const pid of getPeerIds()) {
            if (isAndroid) {
              invoke("send_video", {
                peerId: pid, data: toBase64(rgba), width, height, keyframe, timestamp: ts,
              }).catch(() => {});
            } else {
              invoke("send_video", packVideoPayload(pid, width, height, keyframe, ts, rgba), {
                headers: { "Content-Type": "application/octet-stream" },
              }).catch(() => {});
            }
          }
        }
        if (captureVideoEl && "requestVideoFrameCallback" in captureVideoEl) {
          captureVideoEl.requestVideoFrameCallback(captureLoop);
        }
      }

      if ("requestVideoFrameCallback" in captureVideoEl) {
        captureVideoEl.requestVideoFrameCallback(captureLoop);
      } else {
        // Fallback: requestAnimationFrame
        function rafLoop() {
          if (!encoding.value) return;
          captureLoop(performance.now(), null);
          requestAnimationFrame(rafLoop);
        }
        requestAnimationFrame(rafLoop);
      }
    }
  }

  async function startReceiving(canvas: HTMLCanvasElement) {
    if (playbackCtx) return;
    remoteCanvas = canvas;
    const { listen } = await import("@tauri-apps/api/event");

    playbackCtx = new AudioContext({ sampleRate: 48000 });
    nextPlayTime = playbackCtx.currentTime;

    initVideoDecoder(canvas);

    type AudioPayload = { data: string; timestamp: number };
    type VideoPayload = { data: string; timestamp: number };

    // Audio receive — PCM Int16 from Rust Opus decoder
    unlistenAudio = await listen<AudioPayload>("audio-received", (event) => {
      if (!playbackCtx) return;
      const bytes = fromBase64(event.payload.data);
      const int16 = new Int16Array(
        bytes.buffer,
        bytes.byteOffset,
        bytes.byteLength / 2,
      );
      const buffer = playbackCtx.createBuffer(1, int16.length, 48000);
      const ch = buffer.getChannelData(0);
      let sum = 0;
      for (let i = 0; i < int16.length; i++) {
        const sample = int16[i]! / 32768;
        ch[i] = sample;
        sum += sample * sample;
      }
      scheduleAudioBuffer(buffer, event.payload.timestamp);

      // Speaking detection via RMS energy
      const rms = Math.sqrt(sum / int16.length);
      if (rms > SPEAKING_RMS_THRESHOLD) {
        if (!peerSpeaking.value) peerSpeaking.value = true;
        if (speakingTimeout) clearTimeout(speakingTimeout);
        speakingTimeout = setTimeout(() => { peerSpeaking.value = false; }, SPEAKING_DEBOUNCE_MS);
      }
    });

    // Video receive — H.264 NALUs decoded via WebCodecs
    unlistenVideo = await listen<VideoPayload>("video-received", (event) => {
      if (!remoteCanvas) return;
      videoFrameTimestamps.push(Date.now());
      if (videoFrameTimestamps.length > 60) videoFrameTimestamps = videoFrameTimestamps.slice(-30);

      if (videoDecoder && videoDecoder.state === "configured") {
        const bytes = fromBase64(event.payload.data);
        const isKf = detectKeyframe(bytes);
        if (videoDecoder.decodeQueueSize > 10) return;
        const chunk = new EncodedVideoChunk({
          type: isKf ? "key" : "delta",
          timestamp: event.payload.timestamp * 1000,
          data: bytes,
        });
        videoDecoder.decode(chunk);
      }
    });

    // Connection quality — measure video frame receipt rate every 2s
    qualityInterval = setInterval(() => {
      const now = Date.now();
      videoFrameTimestamps = videoFrameTimestamps.filter((t) => now - t < 2000);
      const fps = videoFrameTimestamps.length / 2;
      const q = fps >= 10 ? "good" : fps >= 5 ? "degraded" : "poor";
      if (connectionQuality.value !== q) connectionQuality.value = q;
    }, 2000);

    // Adaptive quality — adjust capture dimensions based on connection quality
    const { watch } = await import("vue");
    watch(connectionQuality, async (q) => {
      if (q === "poor") {
        encoding.value = false; // stop video capture
      } else if (q === "degraded") {
        await updateCaptureDimensions(320, 240);
        targetFps = 10;
      } else {
        await updateCaptureDimensions(640, 480);
        targetFps = 15;
      }
    });
  }

  async function updateCaptureDimensions(w: number, h: number) {
    if (w === currentWidth && h === currentHeight) return;
    currentWidth = w;
    currentHeight = h;
    const { invoke } = await import("@tauri-apps/api/core");
    await invoke("reinit_video_encoder", { width: w, height: h });
  }

  function teardownCapture() {
    encoding.value = false;
    if (workletNode) { workletNode.port.onmessage = null; workletNode.disconnect(); workletNode = null; }
    if (sourceNode) { sourceNode.disconnect(); sourceNode = null; }
    if (captureCtx) { captureCtx.close(); captureCtx = null; }
    if (captureVideoEl) { captureVideoEl.pause(); captureVideoEl.srcObject = null; captureVideoEl = null; }
    if (captureInterval) { clearInterval(captureInterval); captureInterval = null; }
  }

  async function restartSending(newStream: MediaStream, getPeerIds: () => string[]) {
    teardownCapture();
    await initCodecs(newStream);
    await startSending(newStream, getPeerIds);
  }

  // Adaptive jitter buffer
  let jitterBufferMs = 60;
  let jitterEstimate = 0;
  let baseDelay: number | null = null;

  function scheduleAudioBuffer(buffer: AudioBuffer, captureTimestamp: number) {
    if (!playbackCtx) return;
    const now = Date.now();
    const oneWayDelay = now - captureTimestamp;
    if (baseDelay === null) baseDelay = oneWayDelay;
    baseDelay = Math.min(baseDelay, oneWayDelay);

    const jitter = Math.abs(oneWayDelay - baseDelay);
    jitterEstimate = 0.9 * jitterEstimate + 0.1 * jitter;
    jitterBufferMs = Math.max(40, Math.min(120, jitterEstimate * 2));

    const jitterBufferSec = jitterBufferMs / 1000;
    const source = playbackCtx.createBufferSource();
    source.buffer = buffer;
    source.connect(playbackCtx.destination);
    const ctxNow = playbackCtx.currentTime;
    if (nextPlayTime < ctxNow - jitterBufferSec) {
      nextPlayTime = ctxNow + jitterBufferSec;
    }
    source.start(nextPlayTime);
    nextPlayTime += buffer.duration;
  }

  async function stop() {
    teardownCapture();

    if (playbackCtx) {
      playbackCtx.close();
      playbackCtx = null;
    }
    if (speakingTimeout) { clearTimeout(speakingTimeout); speakingTimeout = null; }
    if (qualityInterval) { clearInterval(qualityInterval); qualityInterval = null; }

    if (videoDecoder) {
      try { videoDecoder.close(); } catch {}
      videoDecoder = null;
    }
    jitterBufferMs = 60;
    jitterEstimate = 0;
    baseDelay = null;

    unlistenAudio?.();
    unlistenVideo?.();
    unlistenAudio = null;
    unlistenVideo = null;
    remoteCanvas = null;
    peerSpeaking.value = false;
    connectionQuality.value = "good";
    videoFrameTimestamps = [];

    // Clean up Rust codec state
    try {
      const { invoke } = await import("@tauri-apps/api/core");
      await invoke("destroy_codecs");
    } catch {}
  }

  return { encoding, peerSpeaking, connectionQuality, initCodecs, startSending, restartSending, startReceiving, stop, updateCaptureDimensions };
}
