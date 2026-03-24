# Nafaq Media Pipeline + QR Code Implementation Plan (Plan 4)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add real-time audio/video streaming between peers so 1-on-1 calls actually work — encode local media with WebCodecs, transport via the sidecar's binary frame protocol, decode and render remote media. Also add QR code display for ticket sharing.

**Architecture:** The webview opens a direct WebSocket to the sidecar (port 9320) for binary media frames, bypassing the Bun process bridge for performance. Audio is encoded/decoded with WebCodecs (Opus). Video is encoded/decoded with WebCodecs (VP8). Decoded remote video renders to a `<canvas>` element. Decoded remote audio plays through an AudioContext.

**Tech Stack:** WebCodecs API (AudioEncoder/Decoder, VideoEncoder/Decoder), MediaStreamTrackProcessor, AudioContext, Canvas2D, qrcode lib

**Prerequisites:** Plans 1-3 complete. Sidecar binary exists. The `nafaq` API and call lifecycle work.

---

## File Structure

```
src/mainview/
├── composables/
│   ├── useMedia.ts              # (exists) — no changes
│   ├── useCall.ts               # (exists) — no changes
│   ├── useChat.ts               # (exists) — no changes
│   └── useMediaTransport.ts     # NEW: direct WS to sidecar + encode/decode pipelines
├── lib/
│   └── hex.ts                   # NEW: hex string ↔ Uint8Array conversion
├── components/
│   ├── VideoGrid.vue            # MODIFY: add remote video canvas
│   ├── TicketCreate.vue         # MODIFY: add QR code display
│   └── ...                      # (others unchanged)
└── pages/
    └── CallPage.vue             # MODIFY: wire media transport
```

## Key Design Decisions

**Direct WebSocket for media:** The webview opens its own WebSocket to `ws://127.0.0.1:9320`. The sidecar accepts multiple clients and broadcasts media to all. Binary frames use the existing MediaFrame format (41-byte header). Control/chat still goes through the Bun bridge.

**Peer ID encoding:** The sidecar uses 32-byte raw public keys in MediaFrames but returns hex strings in JSON events. We convert hex → bytes for outgoing frames and bytes → hex for identifying incoming frames.

**WebCodecs config:**
- Audio: Opus, 48kHz, mono, 20ms frames (~960 samples per frame)
- Video: VP8, 640x480@15fps (conservative for v1), keyframe every 2 seconds

**Audio playback:** Decoded AudioData is queued to an AudioContext via scheduled buffers. A simple ring buffer prevents audio glitches.

---

### Task 1: Hex Utility + Media Transport WebSocket

**Files:**
- Create: `src/mainview/lib/hex.ts`
- Create: `src/mainview/composables/useMediaTransport.ts`

- [ ] **Step 1: Create hex conversion utility**

`src/mainview/lib/hex.ts`:
```typescript
/** Convert a hex string (e.g. EndpointId) to a 32-byte Uint8Array */
export function hexToBytes(hex: string): Uint8Array {
  const clean = hex.replace(/^0x/, "");
  const bytes = new Uint8Array(32);
  for (let i = 0; i < 32 && i * 2 < clean.length; i++) {
    bytes[i] = parseInt(clean.substring(i * 2, i * 2 + 2), 16);
  }
  return bytes;
}

/** Convert a 32-byte Uint8Array to a hex string */
export function bytesToHex(bytes: Uint8Array): string {
  return Array.from(bytes)
    .map((b) => b.toString(16).padStart(2, "0"))
    .join("");
}
```

- [ ] **Step 2: Create media transport composable**

`src/mainview/composables/useMediaTransport.ts`:
```typescript
import { ref, onUnmounted } from "vue";
import {
  STREAM_AUDIO,
  STREAM_VIDEO,
  MEDIA_FRAME_HEADER_SIZE,
  encodeMediaFrame,
  decodeMediaFrame,
  type MediaFrame,
} from "../../shared/types";
import { hexToBytes, bytesToHex } from "../lib/hex";

const SIDECAR_PORT = 9320;

export type OnAudioFrame = (peerId: string, data: Uint8Array, timestamp: number) => void;
export type OnVideoFrame = (peerId: string, data: Uint8Array, timestamp: number) => void;

/**
 * Direct WebSocket connection to the sidecar for binary media frames.
 * Bypasses the Bun bridge for performance.
 */
export function useMediaTransport() {
  const connected = ref(false);
  let ws: WebSocket | null = null;
  let onAudio: OnAudioFrame | null = null;
  let onVideo: OnVideoFrame | null = null;

  function connect() {
    if (ws) return;

    ws = new WebSocket(`ws://127.0.0.1:${SIDECAR_PORT}`);
    ws.binaryType = "arraybuffer";

    ws.onopen = () => {
      console.log("[media-transport] Connected to sidecar");
      connected.value = true;
    };

    ws.onmessage = (event: MessageEvent) => {
      if (event.data instanceof ArrayBuffer) {
        handleBinaryFrame(new Uint8Array(event.data));
      }
      // Ignore text messages (handled by the Bun bridge)
    };

    ws.onclose = () => {
      console.log("[media-transport] Disconnected");
      connected.value = false;
      ws = null;
    };

    ws.onerror = () => {
      // onclose fires after
    };
  }

  function disconnect() {
    if (ws) {
      ws.close();
      ws = null;
    }
    connected.value = false;
  }

  function handleBinaryFrame(data: Uint8Array) {
    const frame = decodeMediaFrame(data);
    if (!frame) return;

    const peerId = bytesToHex(frame.peerId);
    const timestamp = Number(frame.timestampMs);

    if (frame.streamType === STREAM_AUDIO && onAudio) {
      onAudio(peerId, frame.payload, timestamp);
    } else if (frame.streamType === STREAM_VIDEO && onVideo) {
      onVideo(peerId, frame.payload, timestamp);
    }
  }

  /** Send an encoded audio chunk to a peer */
  function sendAudio(peerIdHex: string, data: Uint8Array) {
    if (!ws || ws.readyState !== WebSocket.OPEN) return;
    const frame = encodeMediaFrame({
      streamType: STREAM_AUDIO,
      peerId: hexToBytes(peerIdHex),
      timestampMs: BigInt(Date.now()),
      payload: data,
    });
    ws.send(frame);
  }

  /** Send an encoded video chunk to a peer */
  function sendVideo(peerIdHex: string, data: Uint8Array) {
    if (!ws || ws.readyState !== WebSocket.OPEN) return;
    const frame = encodeMediaFrame({
      streamType: STREAM_VIDEO,
      peerId: hexToBytes(peerIdHex),
      timestampMs: BigInt(Date.now()),
      payload: data,
    });
    ws.send(frame);
  }

  function setOnAudio(handler: OnAudioFrame) {
    onAudio = handler;
  }

  function setOnVideo(handler: OnVideoFrame) {
    onVideo = handler;
  }

  onUnmounted(() => {
    disconnect();
  });

  return {
    connected,
    connect,
    disconnect,
    sendAudio,
    sendVideo,
    setOnAudio,
    setOnVideo,
  };
}
```

- [ ] **Step 3: Verify build**

Run: `bunx vite build --config vite.config.ts`
Expected: Build succeeds

- [ ] **Step 4: Commit**

```bash
git add src/mainview/lib/hex.ts src/mainview/composables/useMediaTransport.ts
git commit -m "feat(media): media transport WebSocket with binary frame encoding"
```

---

### Task 2: Audio Encoding + Decoding Pipeline

**Files:**
- Create: `src/mainview/composables/useAudioPipeline.ts`

- [ ] **Step 1: Implement audio encode/decode composable**

`src/mainview/composables/useAudioPipeline.ts`:
```typescript
import { ref } from "vue";

export interface AudioPipelineOptions {
  onEncoded: (data: Uint8Array) => void;
}

/**
 * Handles audio encoding (local mic → Opus) and decoding (Opus → speaker).
 * Uses WebCodecs AudioEncoder/AudioDecoder.
 */
export function useAudioPipeline() {
  const encoding = ref(false);
  const decoding = ref(false);

  let encoder: AudioEncoder | null = null;
  let decoder: AudioDecoder | null = null;
  let audioCtx: AudioContext | null = null;
  let processorNode: ScriptProcessorNode | null = null;
  let sourceNode: MediaStreamAudioSourceNode | null = null;
  let onEncodedCallback: ((data: Uint8Array) => void) | null = null;

  // Playback scheduling
  let nextPlayTime = 0;
  let playbackCtx: AudioContext | null = null;

  /** Start encoding audio from a MediaStream track */
  async function startEncoding(stream: MediaStream, onEncoded: (data: Uint8Array) => void) {
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

    // Use ScriptProcessorNode to capture raw audio samples
    // (MediaStreamTrackProcessor may not be available in all webviews)
    audioCtx = new AudioContext({ sampleRate: 48000 });
    sourceNode = audioCtx.createMediaStreamSource(stream);
    // 960 samples = 20ms at 48kHz
    processorNode = audioCtx.createScriptProcessor(960, 1, 1);

    let frameCount = 0;

    processorNode.onaudioprocess = (event) => {
      if (!encoder || encoder.state !== "configured") return;

      const inputData = event.inputBuffer.getChannelData(0);
      const audioData = new AudioData({
        format: "f32-planar",
        sampleRate: 48000,
        numberOfFrames: inputData.length,
        numberOfChannels: 1,
        timestamp: frameCount * (960 / 48000) * 1_000_000, // microseconds
        data: inputData,
      });
      frameCount++;

      encoder.encode(audioData);
      audioData.close();
    };

    sourceNode.connect(processorNode);
    processorNode.connect(audioCtx.destination); // Required for processing to run
    encoding.value = true;
    console.log("[audio-enc] Started encoding");
  }

  /** Stop encoding */
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

  /** Initialize the audio decoder for playback */
  function startDecoding() {
    playbackCtx = new AudioContext({ sampleRate: 48000 });
    nextPlayTime = playbackCtx.currentTime;

    decoder = new AudioDecoder({
      output: (audioData: AudioData) => {
        // Schedule playback
        const buffer = playbackCtx.createBuffer(
          audioData.numberOfChannels,
          audioData.numberOfFrames,
          audioData.sampleRate,
        );

        // Copy decoded samples into buffer
        for (let ch = 0; ch < audioData.numberOfChannels; ch++) {
          const channelData = new Float32Array(audioData.numberOfFrames);
          audioData.copyTo(channelData, { planeIndex: ch });
          buffer.copyToChannel(channelData, ch);
        }
        audioData.close();

        // Schedule buffer playback
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

  /** Feed an encoded audio chunk for decoding + playback */
  function decodeChunk(data: Uint8Array, timestamp: number) {
    if (!decoder || decoder.state !== "configured") return;

    const chunk = new EncodedAudioChunk({
      type: "key", // Opus frames are all keyframes
      timestamp: timestamp * 1000, // ms to microseconds
      data: data,
    });
    decoder.decode(chunk);
  }

  /** Stop decoding */
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
```

- [ ] **Step 2: Verify build**

Run: `bunx vite build --config vite.config.ts`
Expected: Build succeeds (WebCodecs types are available in TypeScript's lib.dom)

- [ ] **Step 3: Commit**

```bash
git add src/mainview/composables/useAudioPipeline.ts
git commit -m "feat(media): audio pipeline with WebCodecs Opus encode/decode"
```

---

### Task 3: Video Encoding + Decoding Pipeline

**Files:**
- Create: `src/mainview/composables/useVideoPipeline.ts`

- [ ] **Step 1: Implement video encode/decode composable**

`src/mainview/composables/useVideoPipeline.ts`:
```typescript
import { ref } from "vue";

/**
 * Handles video encoding (local camera → VP8) and decoding (VP8 → canvas).
 * Uses WebCodecs VideoEncoder/VideoDecoder.
 */
export function useVideoPipeline() {
  const encoding = ref(false);
  const decoding = ref(false);

  let encoder: VideoEncoder | null = null;
  let decoder: VideoDecoder | null = null;
  let captureInterval: ReturnType<typeof setInterval> | null = null;
  let captureCanvas: OffscreenCanvas | null = null;
  let onEncodedCallback: ((data: Uint8Array, isKey: boolean) => void) | null = null;
  let onDecodedCallback: ((frame: VideoFrame) => void) | null = null;

  let frameCount = 0;
  const KEYFRAME_INTERVAL = 30; // Every 30 frames (~2s at 15fps)

  /** Start encoding video from a MediaStream */
  function startEncoding(
    stream: MediaStream,
    onEncoded: (data: Uint8Array, isKey: boolean) => void,
  ) {
    const videoTrack = stream.getVideoTracks()[0];
    if (!videoTrack) {
      console.warn("[video-enc] No video track available");
      return;
    }

    onEncodedCallback = onEncoded;
    const settings = videoTrack.getSettings();
    const width = Math.min(settings.width || 640, 640);
    const height = Math.min(settings.height || 480, 480);

    encoder = new VideoEncoder({
      output: (chunk: EncodedVideoChunk) => {
        const buf = new Uint8Array(chunk.byteLength);
        chunk.copyTo(buf);
        onEncodedCallback?.(buf, chunk.type === "key");
      },
      error: (e) => console.error("[video-enc] Error:", e),
    });

    encoder.configure({
      codec: "vp8",
      width,
      height,
      bitrate: 500_000, // 500kbps
      framerate: 15,
    });

    // Capture frames from video element via canvas
    const videoEl = document.createElement("video");
    videoEl.srcObject = stream;
    videoEl.muted = true;
    videoEl.play();

    captureCanvas = new OffscreenCanvas(width, height);
    const ctx = captureCanvas.getContext("2d")!;
    frameCount = 0;

    captureInterval = setInterval(() => {
      if (!encoder || encoder.state !== "configured") return;
      if (videoEl.readyState < 2) return; // Not enough data yet

      ctx.drawImage(videoEl, 0, 0, width, height);
      const frame = new VideoFrame(captureCanvas, {
        timestamp: frameCount * (1_000_000 / 15), // microseconds
      });
      frameCount++;

      const isKeyFrame = frameCount % KEYFRAME_INTERVAL === 0;
      encoder.encode(frame, { keyFrame: isKeyFrame });
      frame.close();
    }, 1000 / 15); // 15 fps

    encoding.value = true;
    console.log(`[video-enc] Started encoding ${width}x${height}@15fps VP8`);
  }

  /** Stop encoding */
  function stopEncoding() {
    if (captureInterval) {
      clearInterval(captureInterval);
      captureInterval = null;
    }
    if (encoder) {
      if (encoder.state !== "closed") encoder.close();
      encoder = null;
    }
    captureCanvas = null;
    encoding.value = false;
  }

  /** Start decoding video frames */
  function startDecoding(onDecoded: (frame: VideoFrame) => void) {
    onDecodedCallback = onDecoded;

    decoder = new VideoDecoder({
      output: (frame: VideoFrame) => {
        onDecodedCallback?.(frame);
      },
      error: (e) => console.error("[video-dec] Error:", e),
    });

    decoder.configure({
      codec: "vp8",
    });

    decoding.value = true;
    console.log("[video-dec] Started decoding");
  }

  /** Feed an encoded video chunk for decoding */
  function decodeChunk(data: Uint8Array, timestamp: number, isKey: boolean) {
    if (!decoder || decoder.state !== "configured") return;

    const chunk = new EncodedVideoChunk({
      type: isKey ? "key" : "delta",
      timestamp: timestamp * 1000, // ms to microseconds
      data: data,
    });
    decoder.decode(chunk);
  }

  /** Stop decoding */
  function stopDecoding() {
    if (decoder) {
      if (decoder.state !== "closed") decoder.close();
      decoder = null;
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
```

- [ ] **Step 2: Verify build**

Run: `bunx vite build --config vite.config.ts`
Expected: Build succeeds

- [ ] **Step 3: Commit**

```bash
git add src/mainview/composables/useVideoPipeline.ts
git commit -m "feat(media): video pipeline with WebCodecs VP8 encode/decode"
```

---

### Task 4: Update VideoGrid for Remote Video

**Files:**
- Modify: `src/mainview/components/VideoGrid.vue`

- [ ] **Step 1: Add remote video canvas and accept decoded frames**

Replace `src/mainview/components/VideoGrid.vue`:

```vue
<script setup lang="ts">
import { computed, ref, watch, onMounted } from "vue";

const props = defineProps<{
  localStream: MediaStream | null;
  peers: string[];
  remoteVideoFrame: VideoFrame | null;
}>();

const localVideoEl = ref<HTMLVideoElement | null>(null);
const remoteCanvasEl = ref<HTMLCanvasElement | null>(null);

watch(
  () => props.localStream,
  (stream) => {
    if (localVideoEl.value && stream) {
      localVideoEl.value.srcObject = stream;
    }
  },
);

// Draw decoded remote video frames to canvas
watch(
  () => props.remoteVideoFrame,
  (frame) => {
    if (!frame || !remoteCanvasEl.value) return;
    const ctx = remoteCanvasEl.value.getContext("2d");
    if (!ctx) return;

    // Resize canvas to match frame
    if (
      remoteCanvasEl.value.width !== frame.displayWidth ||
      remoteCanvasEl.value.height !== frame.displayHeight
    ) {
      remoteCanvasEl.value.width = frame.displayWidth;
      remoteCanvasEl.value.height = frame.displayHeight;
    }

    ctx.drawImage(frame, 0, 0);
    frame.close();
  },
);

const gridCols = computed(() => {
  const total = props.peers.length + 1;
  if (total <= 1) return 1;
  if (total <= 4) return 2;
  return 3;
});
</script>

<template>
  <!-- 1-on-1: full screen remote + PiP self -->
  <div v-if="peers.length === 1" class="relative w-full h-full bg-[var(--color-surface-alt)]">
    <!-- Remote video canvas -->
    <canvas
      ref="remoteCanvasEl"
      class="w-full h-full object-contain absolute inset-0"
    ></canvas>
    <div v-if="!remoteVideoFrame" class="w-full h-full flex items-center justify-center absolute inset-0">
      <span class="text-[var(--color-border-muted)] text-sm font-bold tracking-widest">Waiting for video...</span>
    </div>

    <!-- Self PiP -->
    <div class="absolute bottom-20 right-4 w-[180px] h-[110px] bg-[#111] border-2 border-[var(--color-border)] overflow-hidden z-10">
      <video ref="localVideoEl" autoplay muted playsinline class="w-full h-full object-cover"></video>
      <span v-if="!localStream" class="absolute inset-0 flex items-center justify-center text-[var(--color-muted)] text-[10px] tracking-widest">You</span>
    </div>
  </div>

  <!-- Group: grid layout -->
  <div
    v-else
    class="w-full h-full grid gap-[2px] bg-[var(--color-border)] p-[2px]"
    :style="{ gridTemplateColumns: `repeat(${gridCols}, 1fr)` }"
  >
    <!-- Self tile -->
    <div class="bg-[#111] relative min-h-[140px] flex items-center justify-center">
      <video ref="localVideoEl" autoplay muted playsinline class="w-full h-full object-cover absolute inset-0"></video>
      <span class="absolute bottom-2 left-2.5 text-[10px] text-[var(--color-accent)] bg-black px-2 py-0.5 font-bold tracking-wider z-10">You</span>
      <div class="absolute top-2 right-2.5 w-1.5 h-1.5 bg-[var(--color-accent)] z-10"></div>
    </div>

    <!-- Peer tiles -->
    <div v-for="peer in peers" :key="peer" class="bg-[#111] relative min-h-[140px] flex items-center justify-center">
      <canvas ref="remoteCanvasEl" class="w-full h-full object-contain absolute inset-0"></canvas>
      <span v-if="!remoteVideoFrame" class="text-[var(--color-border-muted)] text-xs tracking-widest">Connecting...</span>
      <span class="absolute bottom-2 left-2.5 text-[10px] text-white bg-black px-2 py-0.5 font-bold tracking-wider z-10">{{ peer.slice(0, 8) }}...</span>
      <div class="absolute top-2 right-2.5 w-1.5 h-1.5 bg-[var(--color-accent)] z-10"></div>
    </div>
  </div>
</template>
```

- [ ] **Step 2: Verify build**

Run: `bunx vite build --config vite.config.ts`
Expected: Build succeeds

- [ ] **Step 3: Commit**

```bash
git add src/mainview/components/VideoGrid.vue
git commit -m "feat(media): VideoGrid renders remote video via canvas"
```

---

### Task 5: Wire Media Pipeline into CallPage

**Files:**
- Modify: `src/mainview/pages/CallPage.vue`

- [ ] **Step 1: Integrate media transport and pipelines**

Replace `src/mainview/pages/CallPage.vue`:

```vue
<script setup lang="ts">
import { ref, onMounted, onUnmounted } from "vue";
import { useRouter } from "vue-router";
import { useCall } from "../composables/useCall";
import { useMedia } from "../composables/useMedia";
import { useChat } from "../composables/useChat";
import { useMediaTransport } from "../composables/useMediaTransport";
import { useAudioPipeline } from "../composables/useAudioPipeline";
import { useVideoPipeline } from "../composables/useVideoPipeline";
import CallControls from "../components/CallControls.vue";
import ChatSidebar from "../components/ChatSidebar.vue";
import VideoGrid from "../components/VideoGrid.vue";

const router = useRouter();
const call = useCall();
const media = useMedia();
const chat = useChat();
const transport = useMediaTransport();
const audioPipeline = useAudioPipeline();
const videoPipeline = useVideoPipeline();

const chatOpen = ref(true);
const callDuration = ref("0:00");
const remoteVideoFrame = ref<VideoFrame | null>(null);

let durationInterval: ReturnType<typeof setInterval> | null = null;
let startTime = Date.now();

onMounted(async () => {
  if (call.state.value !== "connected") {
    router.push("/");
    return;
  }

  // Start camera if not already running
  if (!media.localStream.value) {
    await media.startPreview();
  }

  // Connect media transport (direct WS to sidecar)
  transport.connect();

  // Set up receive handlers
  transport.setOnAudio((peerId, data, timestamp) => {
    audioPipeline.decodeChunk(data, timestamp);
  });

  transport.setOnVideo((peerId, data, timestamp) => {
    // Determine if keyframe by checking VP8 header
    // VP8 keyframe: first byte bit 0 is 0
    const isKey = data.length > 0 && (data[0] & 0x01) === 0;
    videoPipeline.decodeChunk(data, timestamp, isKey);
  });

  // Start decoders
  audioPipeline.startDecoding();
  videoPipeline.startDecoding((frame: VideoFrame) => {
    // Close previous frame if not yet consumed
    if (remoteVideoFrame.value) {
      try { remoteVideoFrame.value.close(); } catch {}
    }
    remoteVideoFrame.value = frame;
  });

  // Start encoders (send local media to peer)
  if (media.localStream.value && call.peerId.value) {
    const peerId = call.peerId.value;

    audioPipeline.startEncoding(media.localStream.value, (data) => {
      transport.sendAudio(peerId, data);
    });

    videoPipeline.startEncoding(media.localStream.value, (data) => {
      transport.sendVideo(peerId, data);
    });
  }

  // Call duration timer
  startTime = Date.now();
  durationInterval = setInterval(() => {
    const elapsed = Math.floor((Date.now() - startTime) / 1000);
    const mins = Math.floor(elapsed / 60);
    const secs = elapsed % 60;
    callDuration.value = `${mins}:${secs.toString().padStart(2, "0")}`;
  }, 1000);
});

onUnmounted(() => {
  if (durationInterval) clearInterval(durationInterval);
  audioPipeline.stop();
  videoPipeline.stop();
  transport.disconnect();
});

function handleEndCall() {
  if (durationInterval) clearInterval(durationInterval);
  audioPipeline.stop();
  videoPipeline.stop();
  transport.disconnect();
  media.stopPreview();
  chat.clearMessages();
  call.endCall();
}

function handleSendChat(text: string) {
  if (call.peerId.value) {
    chat.sendMessage(call.peerId.value, text);
  }
}

function handleToggleAudio() {
  media.toggleAudio();
  // When muted, stop sending audio frames
  if (media.audioMuted.value) {
    audioPipeline.stopEncoding();
  } else if (media.localStream.value && call.peerId.value) {
    audioPipeline.startEncoding(media.localStream.value, (data) => {
      transport.sendAudio(call.peerId.value!, data);
    });
  }
}

function handleToggleVideo() {
  media.toggleVideo();
  if (media.videoMuted.value) {
    videoPipeline.stopEncoding();
  } else if (media.localStream.value && call.peerId.value) {
    videoPipeline.startEncoding(media.localStream.value, (data) => {
      transport.sendVideo(call.peerId.value!, data);
    });
  }
}
</script>

<template>
  <div class="h-screen flex">
    <!-- Video area -->
    <div class="flex-1 bg-[var(--color-surface-alt)] relative flex flex-col">
      <!-- Top bar -->
      <div class="absolute top-0 left-0 right-0 flex justify-between px-4 py-3 z-20 bg-gradient-to-b from-black/80 to-transparent">
        <span class="text-sm font-black tracking-widest">{{ callDuration }}</span>
        <div class="flex items-center gap-2">
          <div class="w-2 h-2 bg-[var(--color-accent)]"></div>
          <span class="text-[10px] text-[var(--color-accent)] tracking-widest font-bold">P2P Direct</span>
        </div>
      </div>

      <!-- Video grid -->
      <div class="flex-1">
        <VideoGrid
          :localStream="media.localStream.value"
          :peers="call.peers.value"
          :remoteVideoFrame="remoteVideoFrame"
        />
      </div>

      <!-- Bottom controls -->
      <div
        class="absolute bottom-0 left-0 py-3.5 z-20 bg-gradient-to-t from-black/80 to-transparent"
        :class="chatOpen ? 'right-[260px]' : 'right-0'"
      >
        <CallControls
          :audioMuted="media.audioMuted.value"
          :videoMuted="media.videoMuted.value"
          :chatOpen="chatOpen"
          @toggleAudio="handleToggleAudio"
          @toggleVideo="handleToggleVideo"
          @toggleChat="chatOpen = !chatOpen"
          @endCall="handleEndCall"
        />
      </div>
    </div>

    <!-- Chat sidebar -->
    <ChatSidebar
      v-if="chatOpen"
      :messages="chat.messages.value"
      :peerId="call.peerId.value || ''"
      @send="handleSendChat"
    />
  </div>
</template>
```

- [ ] **Step 2: Verify build**

Run: `bunx vite build --config vite.config.ts`
Expected: Build succeeds

- [ ] **Step 3: Commit**

```bash
git add src/mainview/pages/CallPage.vue
git commit -m "feat(media): wire audio/video pipelines into call page"
```

---

### Task 6: QR Code Ticket Display

**Files:**
- Modify: `src/mainview/components/TicketCreate.vue`

- [ ] **Step 1: Install QR code library**

Run: `bun add qrcode`
Run: `bun add -d @types/qrcode`

- [ ] **Step 2: Update TicketCreate to show QR code**

Replace `src/mainview/components/TicketCreate.vue`:

```vue
<script setup lang="ts">
import { ref, watch } from "vue";
import QRCode from "qrcode";

const props = defineProps<{
  ticket: string | null;
  state: string;
}>();

const emit = defineEmits<{
  create: [];
}>();

const copied = ref(false);
const qrDataUrl = ref<string | null>(null);
const showQr = ref(false);

function copyTicket() {
  if (!props.ticket) return;
  navigator.clipboard.writeText(props.ticket);
  copied.value = true;
  setTimeout(() => (copied.value = false), 2000);
}

// Generate QR code when ticket changes
watch(
  () => props.ticket,
  async (ticket) => {
    if (!ticket) {
      qrDataUrl.value = null;
      return;
    }
    try {
      qrDataUrl.value = await QRCode.toDataURL(ticket, {
        width: 180,
        margin: 1,
        color: { dark: "#000000", light: "#ffffff" },
      });
    } catch (e) {
      console.error("[qr] Failed to generate QR code:", e);
    }
  },
);
</script>

<template>
  <div>
    <p class="label mb-4">SHARE THIS TICKET</p>

    <div v-if="!ticket && state === 'idle'">
      <button class="btn btn-primary w-full" @click="emit('create')">New Call</button>
    </div>

    <div v-else-if="state === 'creating'">
      <p class="text-[var(--color-muted)] text-xs tracking-widest">Creating...</p>
    </div>

    <div v-else-if="ticket" class="space-y-4">
      <div class="border-2 border-[var(--color-accent)] p-4 text-xs break-all text-[var(--color-border)] bg-[#111]">
        {{ ticket }}
      </div>

      <div class="flex gap-0">
        <button class="btn btn-primary flex-1 border-r-0" @click="copyTicket">
          {{ copied ? "Copied!" : "Copy" }}
        </button>
        <button class="btn btn-outline flex-1" @click="showQr = !showQr">
          {{ showQr ? "Hide QR" : "Show QR" }}
        </button>
      </div>

      <!-- QR Code -->
      <div v-if="showQr && qrDataUrl" class="flex justify-center">
        <img :src="qrDataUrl" alt="QR Code" class="w-[180px] h-[180px]" />
      </div>

      <p class="text-[var(--color-muted)] text-xs tracking-widest text-center">
        Waiting for peer<span class="text-[var(--color-accent)]">_</span>
      </p>
    </div>
  </div>
</template>
```

- [ ] **Step 3: Verify build**

Run: `bunx vite build --config vite.config.ts`
Expected: Build succeeds

- [ ] **Step 4: Commit**

```bash
git add package.json bun.lock src/mainview/components/TicketCreate.vue
git commit -m "feat(ui): QR code display for call tickets"
```

---

## Verification Checklist

After completing all tasks:

- [ ] `bunx vite build --config vite.config.ts` produces `dist/`
- [ ] Home page shows QR code when "Show QR" clicked after creating a call
- [ ] When two instances connect, the call page starts encoding/sending audio and video
- [ ] Remote video appears on the canvas (replacing "Waiting for video..." placeholder)
- [ ] Remote audio plays through speakers
- [ ] Muting mic stops sending audio frames
- [ ] Muting camera stops sending video frames
- [ ] Ending the call cleans up all pipelines and transport

## Known Limitations (v1)

- **VP8 keyframe detection** uses a simplified heuristic (checking first byte). A proper VP8 parser would be more reliable but unnecessary for v1.
- **Audio playback scheduling** uses a simple next-time tracker. Under heavy network jitter, audio may skip or buffer. A proper jitter buffer is a future improvement.
- **Single remote peer** — video rendering assumes one remote peer (1-on-1). Group call video rendering needs per-peer canvas management.
- **No adaptive bitrate** — encoding is fixed at 500kbps video / 32kbps audio. Adapting to network conditions is a future improvement.
