import { Channel } from "@tauri-apps/api/core";

const encoding = ref(false);
const connectionQuality = ref<"good" | "degraded" | "poor">("good");

type MediaBridgeMode = "channel_binary" | "event_base64";
type TransportLifecycleState = "idle" | "starting" | "running" | "degraded" | "stopping";

interface MediaSessionProfile {
  sessionId: string;
  receiveBridgeMode: MediaBridgeMode;
  receiveVideoMode: "decoded_jpeg" | "raw_h264_nalu";
  receiveAudioMode: "decoded_pcm";
  sendIngressMode: "invoke_raw" | "invoke_json_fallback";
  playbackReady: boolean;
  bridgeReady: boolean;
}

interface MediaBridgeRegistration {
  sessionId: string;
  preferredBridgeModes: MediaBridgeMode[];
  playbackReady: boolean;
  webcodecs_active: boolean;
}

interface MediaPlaybackStatus {
  sessionId: string;
  audioReady: boolean;
  videoReady: boolean;
  lastFailure: string | null;
}

interface TransportStatus {
  state: TransportLifecycleState;
  sessionId: string | null;
  selectedMode: MediaBridgeMode | null;
  audioReady: boolean;
  videoReady: boolean;
  bridgeReady: boolean;
  lastFailure: string | null;
}

interface PeerNetworkStats {
  peer_id: string;
  rtt_ms: number;
  lost_packets: number;
  lost_bytes: number;
  datagram_send_buffer_space: number;
  latest_video_age_ms: number;
}

interface PendingVideoFrame {
  jpegBytes: Uint8Array;
  width: number;
  height: number;
  timestamp: number;
}

interface LegacyAudioEvent {
  peer_id: string;
  data: string;
  timestamp: number;
}

interface LegacyVideoEvent {
  peer_id: string;
  data: string;
  width: number;
  height: number;
  timestamp: number;
}

interface PeerMediaState {
  canvas: HTMLCanvasElement | null;
  audioGainNode: GainNode | null;
  nextPlayTime: number;
  baseDelay: number | null;
  jitterEstimate: number;
  speaking: boolean;
  lastAudioRms: number;
  speakingSince: number;
  lastSpeakingTime: number;
  lastKeyframeRequestAt: number;
  pendingVideoFrame: PendingVideoFrame | null;
  videoPaused: boolean;
}

interface MediaUploader {
  mode: "invoke";
  sendAudio: (pcmBytes: Uint8Array, timestamp: number) => Promise<void>;
  sendVideo: (
    rgba: Uint8Array,
    width: number,
    height: number,
    keyframe: boolean,
    timestamp: number,
  ) => Promise<void>;
  close: () => void;
}

const transportStatus = ref<TransportStatus>({
  state: "idle",
  sessionId: null,
  selectedMode: null,
  audioReady: false,
  videoReady: false,
  bridgeReady: false,
  lastFailure: null,
});

const activeSpeaker = ref<string | null>(null);
const peerSpeakingMap = ref<Record<string, boolean>>({});

const OPUS_FRAME_SAMPLES = 960;
const SPEAKING_RMS_THRESHOLD = 0.015;
const SPEAKING_DEBOUNCE_MS = 300;
const ACTIVE_SPEAKER_INTERVAL_MS = 300;
const ACTIVE_SPEAKER_SWITCH_THRESHOLD = 1.5;
const ACTIVE_SPEAKER_MIN_DURATION_MS = 500;
const ACTIVE_SPEAKER_SILENCE_MS = 1000;
const KEYFRAME_REQUEST_DEBOUNCE_MS = 300;
const BRIDGE_PROBE_TIMEOUT_MS = 1000;
const BRIDGE_PROBE_PEER_ID = "__bridge_probe__";

let playbackCtx: AudioContext | null = null;
let captureCtx: AudioContext | null = null;
let captureVideoEl: HTMLVideoElement | null = null;
let captureCanvas: OffscreenCanvas | HTMLCanvasElement | null = null;
let captureCanvasCtx: OffscreenCanvasRenderingContext2D | CanvasRenderingContext2D | null = null;
let activeCaptureStream: MediaStream | null = null;
let workletNode: AudioWorkletNode | null = null;
let sourceNode: MediaStreamAudioSourceNode | null = null;
let captureSinkNode: GainNode | null = null;
let unlistenAudio: (() => void) | null = null;
let unlistenVideo: (() => void) | null = null;
let unlistenDisconnect: (() => void) | null = null;
let unlistenStats: (() => void) | null = null;
let unlistenQuality: (() => void) | null = null;
let stopQualityWatch: (() => void) | null = null;
let activeSpeakerInterval: ReturnType<typeof setInterval> | null = null;
let currentWidth = 640;
let currentHeight = 360;
let targetFps = 12;
let peerIdsProvider: (() => string[]) | null = null;
let audioChannel: Channel<ArrayBuffer> | null = null;
let videoChannel: Channel<ArrayBuffer> | null = null;
let mediaUploader: MediaUploader | null = null;
let bridgeProbeResolver: (() => void) | null = null;
let bridgeProbeReceived = false;
let bridgeFallbackUsed = false;

const isAndroid = /android/i.test(navigator.userAgent);
const hasWebCodecs = typeof VideoDecoder !== "undefined";
const sharedTextDecoder = new TextDecoder();
let preferJsonAudioInvoke = isAndroid;
let preferJsonVideoInvoke = isAndroid;
let loggedAudioInvokeFallback = false;
let loggedVideoInvokeFallback = false;

const peerMediaStates = new Map<string, PeerMediaState>();
const peerNetworkStats = new Map<string, PeerNetworkStats>();
const initialKeyframeRequests = new Set<string>();

const peerVideoDecoders = new Map<string, VideoDecoder>();

function getOrCreateVideoDecoder(peerId: string, canvas: HTMLCanvasElement): VideoDecoder {
  let decoder = peerVideoDecoders.get(peerId);
  if (decoder) return decoder;

  const ctx = canvas.getContext("2d")!;
  decoder = new VideoDecoder({
    output(frame: VideoFrame) {
      canvas.width = frame.displayWidth;
      canvas.height = frame.displayHeight;
      ctx.drawImage(frame, 0, 0, canvas.width, canvas.height);
      frame.close();
    },
    error(e: DOMException) {
      console.warn(`VideoDecoder error for ${peerId}:`, e);
    },
  });
  decoder.configure({
    codec: "avc1.42001E", // H.264 Constrained Baseline Level 3.0
    optimizeForLatency: true,
  });
  peerVideoDecoders.set(peerId, decoder);
  return decoder;
}

function destroyVideoDecoder(peerId: string) {
  const decoder = peerVideoDecoders.get(peerId);
  if (decoder && decoder.state !== "closed") {
    decoder.close();
  }
  peerVideoDecoders.delete(peerId);
}

const corePromise = import("@tauri-apps/api/core");
const invokePromise = corePromise.then((m) => m.invoke);

function createSessionId() {
  if (typeof crypto !== "undefined" && "randomUUID" in crypto) {
    return crypto.randomUUID();
  }
  return `media-${Date.now()}-${Math.random().toString(16).slice(2)}`;
}

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

class BufferPool<T extends Int16Array | Uint8Array> {
  private pool: T[] = [];
  private factory: () => T;

  constructor(size: number, factory: () => T) {
    this.factory = factory;
    for (let i = 0; i < size; i++) {
      this.pool.push(factory());
    }
  }

  acquire(): T {
    return this.pool.pop() ?? this.factory();
  }

  release(buf: T) {
    if (this.pool.length < 16) {
      this.pool.push(buf);
    }
  }
}

let audioBufferPool: BufferPool<Int16Array> | null = null;
let videoFrameBufferPool: BufferPool<Uint8Array> | null = null;

// Tauri Channel<Vec<u8>> serializes as JSON array, not ArrayBuffer.
function toArrayBuffer(data: unknown): ArrayBuffer {
  if (data instanceof ArrayBuffer) return data;
  if (ArrayBuffer.isView(data)) {
    const { buffer, byteOffset, byteLength } = data;
    return byteOffset === 0 && byteLength === buffer.byteLength
      ? buffer as ArrayBuffer
      : (buffer.slice(byteOffset, byteOffset + byteLength) as ArrayBuffer);
  }
  if (Array.isArray(data)) return new Uint8Array(data).buffer;
  return new ArrayBuffer(0);
}

function packAudioPayloadAll(timestamp: number, pcm: Uint8Array): Uint8Array {
  const payload = new Uint8Array(8 + pcm.length);
  const view = new DataView(payload.buffer);
  view.setBigUint64(0, BigInt(timestamp), true);
  payload.set(pcm, 8);
  return payload;
}

function packVideoPayloadAll(
  width: number,
  height: number,
  keyframe: boolean,
  timestamp: number,
  rgba: Uint8Array,
): Uint8Array {
  const headerSize = 4 + 4 + 1 + 8;
  const payload = new Uint8Array(headerSize + rgba.length);
  const view = new DataView(payload.buffer);
  let offset = 0;
  view.setUint32(offset, width, true); offset += 4;
  view.setUint32(offset, height, true); offset += 4;
  payload[offset] = keyframe ? 1 : 0; offset += 1;
  view.setBigUint64(offset, BigInt(timestamp), true); offset += 8;
  payload.set(rgba, offset);
  return payload;
}

function currentCaptureBounds() {
  return connectionQuality.value === "good"
    ? { maxWidth: 640, maxHeight: 360 }
    : { maxWidth: 320, maxHeight: 180 };
}

function evenDimension(value: number, fallback: number) {
  const normalized = Number.isFinite(value) ? Math.max(2, Math.round(value)) : fallback;
  return normalized % 2 === 0 ? normalized : normalized - 1;
}

function resolveCaptureDimensions(stream?: MediaStream | null, bounds = currentCaptureBounds()) {
  const track = stream?.getVideoTracks()[0];
  const settings = track?.getSettings();
  const sourceWidth = Number(settings?.width || 0);
  const sourceHeight = Number(settings?.height || 0);

  if (sourceWidth > 0 && sourceHeight > 0) {
    const scale = Math.min(bounds.maxWidth / sourceWidth, bounds.maxHeight / sourceHeight, 1);
    return {
      width: evenDimension(sourceWidth * scale, bounds.maxWidth),
      height: evenDimension(sourceHeight * scale, bounds.maxHeight),
    };
  }

  return {
    width: evenDimension(bounds.maxWidth, bounds.maxWidth),
    height: evenDimension(bounds.maxHeight, bounds.maxHeight),
  };
}

function ensureCaptureSurface(width: number, height: number) {
  if (typeof OffscreenCanvas !== "undefined") {
    if (!(captureCanvas instanceof OffscreenCanvas)) {
      captureCanvas = new OffscreenCanvas(width, height);
      captureCanvasCtx = captureCanvas.getContext("2d");
    } else if (captureCanvas.width !== width || captureCanvas.height !== height) {
      captureCanvas.width = width;
      captureCanvas.height = height;
    }
  } else {
    if (!(captureCanvas instanceof HTMLCanvasElement)) {
      captureCanvas = document.createElement("canvas");
      captureCanvasCtx = captureCanvas.getContext("2d");
    }
    if (captureCanvas.width !== width) captureCanvas.width = width;
    if (captureCanvas.height !== height) captureCanvas.height = height;
  }

  return captureCanvasCtx;
}

function clearCaptureSurface() {
  captureCanvas = null;
  captureCanvasCtx = null;
}

function drawContainedVideoFrame(
  ctx: OffscreenCanvasRenderingContext2D | CanvasRenderingContext2D,
  targetWidth: number,
  targetHeight: number,
) {
  if (!captureVideoEl) return false;
  if (captureVideoEl.readyState < HTMLMediaElement.HAVE_CURRENT_DATA) return false;

  const sourceWidth = captureVideoEl.videoWidth;
  const sourceHeight = captureVideoEl.videoHeight;
  if (!sourceWidth || !sourceHeight) return false;

  const scale = Math.min(targetWidth / sourceWidth, targetHeight / sourceHeight);
  const drawWidth = sourceWidth * scale;
  const drawHeight = sourceHeight * scale;
  const offsetX = (targetWidth - drawWidth) / 2;
  const offsetY = (targetHeight - drawHeight) / 2;

  ctx.fillStyle = "#000";
  ctx.fillRect(0, 0, targetWidth, targetHeight);
  try {
    ctx.drawImage(captureVideoEl, offsetX, offsetY, drawWidth, drawHeight);
    return true;
  } catch {
    return false;
  }
}

function getOrCreatePeerState(peerId: string): PeerMediaState {
  let state = peerMediaStates.get(peerId);
  if (!state) {
    state = {
      canvas: null,
      audioGainNode: null,
      nextPlayTime: 0,
      baseDelay: null,
      jitterEstimate: 0,
      speaking: false,
      lastAudioRms: 0,
      speakingSince: 0,
      lastSpeakingTime: 0,
      lastKeyframeRequestAt: 0,
      pendingVideoFrame: null,
      videoPaused: false,
    };
    peerMediaStates.set(peerId, state);
  }
  return state;
}

function ensurePeerAudioNode(peerState: PeerMediaState) {
  if (!playbackCtx || peerState.audioGainNode) return;
  const gain = playbackCtx.createGain();
  gain.gain.value = 1;
  gain.connect(playbackCtx.destination);
  peerState.audioGainNode = gain;
  peerState.nextPlayTime = playbackCtx.currentTime;
}

function scheduleAudioBuffer(peerState: PeerMediaState, buffer: AudioBuffer, captureTimestamp: number) {
  if (!playbackCtx || !peerState.audioGainNode) return;

  const now = Date.now();
  const oneWayDelay = now - captureTimestamp;
  if (peerState.baseDelay === null) peerState.baseDelay = oneWayDelay;
  peerState.baseDelay = Math.min(peerState.baseDelay, oneWayDelay);

  const jitter = Math.abs(oneWayDelay - peerState.baseDelay);
  peerState.jitterEstimate = 0.9 * peerState.jitterEstimate + 0.1 * jitter;
  const jitterBufferSec = Math.max(40, Math.min(120, peerState.jitterEstimate * 2)) / 1000;

  const source = playbackCtx.createBufferSource();
  source.buffer = buffer;
  source.connect(peerState.audioGainNode);

  const ctxNow = playbackCtx.currentTime;
  if (peerState.nextPlayTime < ctxNow - jitterBufferSec) {
    peerState.nextPlayTime = ctxNow + jitterBufferSec;
  }
  const scheduledAt = Math.max(peerState.nextPlayTime, ctxNow + jitterBufferSec);
  source.start(scheduledAt);
  peerState.nextPlayTime = scheduledAt + buffer.duration;
}

function unpackAudioChannelPacket(packet: ArrayBuffer) {
  const view = new DataView(packet);
  let offset = 0;
  const peerIdLen = view.getUint16(offset, true); offset += 2;
  const peerIdBytes = new Uint8Array(packet, offset, peerIdLen); offset += peerIdLen;
  const timestamp = Number(view.getBigUint64(offset, true)); offset += 8;
  const pcmLen = view.getUint32(offset, true); offset += 4;
  const pcmBytes = new Uint8Array(packet, offset, pcmLen);
  return {
    peerId: sharedTextDecoder.decode(peerIdBytes),
    timestamp,
    pcmBytes,
  };
}

function unpackVideoChannelPacket(packet: ArrayBuffer) {
  const view = new DataView(packet);
  let offset = 0;
  const peerIdLen = view.getUint16(offset, true); offset += 2;
  const peerIdBytes = new Uint8Array(packet, offset, peerIdLen); offset += peerIdLen;
  const timestamp = Number(view.getBigUint64(offset, true)); offset += 8;
  const width = view.getUint32(offset, true); offset += 4;
  const height = view.getUint32(offset, true); offset += 4;
  const jpegLen = view.getUint32(offset, true); offset += 4;
  const jpegBytes = new Uint8Array(packet, offset, jpegLen);
  return {
    peerId: sharedTextDecoder.decode(peerIdBytes),
    timestamp,
    width,
    height,
    jpegBytes,
  };
}

function parseRawNaluPacket(buf: ArrayBuffer) {
  const view = new DataView(buf);
  let offset = 0;
  const peerIdLen = view.getUint16(offset, true); offset += 2;
  const peerId = sharedTextDecoder.decode(new Uint8Array(buf, offset, peerIdLen)); offset += peerIdLen;
  const timestamp = Number(view.getBigUint64(offset, true)); offset += 8;
  const isKeyframe = view.getUint8(offset) === 1; offset += 1;
  const dataLen = view.getUint32(offset, true); offset += 4;
  const h264Data = new Uint8Array(buf, offset, dataLen);
  return { peerId, timestamp, isKeyframe, h264Data };
}

function setTransportFailure(message: string) {
  transportStatus.value = {
    ...transportStatus.value,
    state: transportStatus.value.state === "starting" ? "degraded" : transportStatus.value.state,
    lastFailure: message,
  };
}

async function sendControl(peerId: string, action: Record<string, unknown>) {
  const invoke = await invokePromise;
  await invoke("send_control", { peerId, action });
}

async function requestKeyframe(peerId: string) {
  const peerState = getOrCreatePeerState(peerId);
  const now = Date.now();
  if (now - peerState.lastKeyframeRequestAt < KEYFRAME_REQUEST_DEBOUNCE_MS) return;
  peerState.lastKeyframeRequestAt = now;
  sendControl(peerId, { action: "keyframe_request", layer: "high" }).catch(() => {});
}

async function reportPlaybackStatus() {
  const sessionId = transportStatus.value.sessionId;
  if (!sessionId) return;
  const invoke = await invokePromise;
  const status: MediaPlaybackStatus = {
    sessionId,
    audioReady: transportStatus.value.audioReady,
    videoReady: transportStatus.value.videoReady,
    lastFailure: transportStatus.value.lastFailure,
  };
  await invoke("report_media_playback_status", { status }).catch(() => {});
}

async function acknowledgeBridgeReady() {
  const sessionId = transportStatus.value.sessionId;
  if (!sessionId || transportStatus.value.bridgeReady) return;
  transportStatus.value = {
    ...transportStatus.value,
    bridgeReady: true,
    state: "running",
  };
  const invoke = await invokePromise;
  await invoke("ack_media_bridge_ready", { sessionId }).catch(() => {});
}

function resolveBridgeProbe() {
  bridgeProbeReceived = true;
  const resolve = bridgeProbeResolver;
  bridgeProbeResolver = null;
  resolve?.();
}

function waitForBridgeProbe(timeoutMs = BRIDGE_PROBE_TIMEOUT_MS) {
  if (bridgeProbeReceived) {
    return Promise.resolve();
  }
  return new Promise<void>((resolve, reject) => {
    const timeout = window.setTimeout(() => {
      bridgeProbeResolver = null;
      reject(new Error("Media bridge probe timed out"));
    }, timeoutMs);

    bridgeProbeResolver = () => {
      clearTimeout(timeout);
      resolve();
    };
  });
}

async function ensurePlaybackContext() {
  if (!playbackCtx) {
    playbackCtx = new AudioContext({ sampleRate: 48000 });
  }

  if (playbackCtx.state !== "running") {
    try {
      await playbackCtx.resume();
    } catch {
      // On mobile WebViews, AudioContext.resume() may require a user gesture.
      // Queue a retry on next interaction instead of blocking the pipeline.
      console.warn("[transport] AudioContext resume deferred until user interaction");
      const handler = async () => {
        document.removeEventListener("touchstart", handler);
        document.removeEventListener("click", handler);
        if (playbackCtx && playbackCtx.state !== "running") {
          await playbackCtx.resume().catch(() => {});
        }
      };
      document.addEventListener("touchstart", handler, { once: true });
      document.addEventListener("click", handler, { once: true });
    }
  }
}

function markAudioReady() {
  if (!transportStatus.value.audioReady) {
    transportStatus.value = {
      ...transportStatus.value,
      audioReady: true,
    };
    reportPlaybackStatus().catch(() => {});
  }
}

function markVideoReady() {
  if (!transportStatus.value.videoReady) {
    transportStatus.value = {
      ...transportStatus.value,
      videoReady: true,
    };
    reportPlaybackStatus().catch(() => {});
  }
}

function handleBridgeProbe(peerId: string) {
  if (peerId !== BRIDGE_PROBE_PEER_ID) return false;
  acknowledgeBridgeReady().catch(() => {});
  resolveBridgeProbe();
  return true;
}

function updateSpeakingMap() {
  const newMap: Record<string, boolean> = {};
  for (const [id, state] of peerMediaStates) {
    if (state.speaking) newMap[id] = true;
  }
  peerSpeakingMap.value = newMap;
}

function handleIncomingPcm(peerId: string, timestamp: number, pcmBytes: Uint8Array) {
  if (handleBridgeProbe(peerId)) return;
  if (!playbackCtx) return;
  if (pcmBytes.byteLength === 0) return;

  const int16 = new Int16Array(pcmBytes.buffer, pcmBytes.byteOffset, pcmBytes.byteLength / 2);
  const buffer = playbackCtx.createBuffer(1, int16.length, 48000);
  const channel = buffer.getChannelData(0);
  let sum = 0;
  for (let i = 0; i < int16.length; i++) {
    const sample = int16[i]! / 32768;
    channel[i] = sample;
    sum += sample * sample;
  }

  const peerState = getOrCreatePeerState(peerId);
  ensurePeerAudioNode(peerState);
  if (peerState.audioGainNode) {
    scheduleAudioBuffer(peerState, buffer, timestamp);
  }

  const rms = Math.sqrt(sum / int16.length);
  const now = Date.now();
  peerState.lastAudioRms = 0.7 * peerState.lastAudioRms + 0.3 * rms;

  if (rms > SPEAKING_RMS_THRESHOLD) {
    if (!peerState.speaking) {
      peerState.speaking = true;
      peerState.speakingSince = now;
    }
    peerState.lastSpeakingTime = now;
  } else if (peerState.speaking && now - peerState.lastSpeakingTime > SPEAKING_DEBOUNCE_MS) {
    peerState.speaking = false;
  }

  updateSpeakingMap();
  markAudioReady();
}

async function drawJpegToCanvas(
  canvas: HTMLCanvasElement,
  jpegBytes: Uint8Array,
  width: number,
  height: number,
) {
  const draw = (source: CanvasImageSource) => {
    if (canvas.width !== width) canvas.width = width;
    if (canvas.height !== height) canvas.height = height;
    const ctx = canvas.getContext("2d");
    if (ctx) {
      ctx.drawImage(source, 0, 0, width, height);
    }
  };

  const blob = new Blob([jpegBytes as BlobPart], { type: "image/jpeg" });
  if (typeof createImageBitmap === "function") {
    const image = await createImageBitmap(blob);
    draw(image);
    image.close();
    return;
  }

  await new Promise<void>((resolve, reject) => {
    const url = URL.createObjectURL(blob);
    const image = new Image();
    image.onload = () => {
      draw(image);
      URL.revokeObjectURL(url);
      resolve();
    };
    image.onerror = () => {
      URL.revokeObjectURL(url);
      reject(new Error("JPEG image failed to load"));
    };
    image.src = url;
  });
}

async function handleIncomingVideoFrame(
  peerId: string,
  timestamp: number,
  width: number,
  height: number,
  jpegBytes: Uint8Array,
) {
  const peerState = getOrCreatePeerState(peerId);
  const frame: PendingVideoFrame = { jpegBytes, width, height, timestamp };
  if (!peerState.canvas) {
    peerState.pendingVideoFrame = frame;
    return;
  }

  try {
    await drawJpegToCanvas(peerState.canvas, jpegBytes, width, height);
    peerState.pendingVideoFrame = null;
    markVideoReady();
  } catch (error) {
    const message = `Video frame render failed: ${error instanceof Error ? error.message : String(error)}`;
    setTransportFailure(message);
    await reportPlaybackStatus();
  }
}

async function setupReceiveBridge(forceEventMode = false) {
  const { invoke } = await corePromise;
  const { listen } = await import("@tauri-apps/api/event");

  const sessionId = createSessionId();
  const preferredBridgeModes: MediaBridgeMode[] = forceEventMode
    ? ["event_base64"]
    : ["channel_binary", "event_base64"];

  const nextAudioChannel = new Channel<ArrayBuffer>();
  const nextVideoChannel = new Channel<ArrayBuffer>();

  const registration: MediaBridgeRegistration = {
    sessionId,
    preferredBridgeModes,
    playbackReady: playbackCtx?.state === "running",
    webcodecs_active: hasWebCodecs,
  };

  const profile = await invoke<MediaSessionProfile>("register_media_bridge", {
    registration,
    audio: nextAudioChannel,
    video: nextVideoChannel,
  });

  transportStatus.value = {
    state: "starting",
    sessionId: profile.sessionId,
    selectedMode: profile.receiveBridgeMode,
    audioReady: false,
    videoReady: false,
    bridgeReady: false,
    lastFailure: null,
  };

  bridgeProbeReceived = false;

  audioChannel = nextAudioChannel;
  videoChannel = nextVideoChannel;

  if (profile.receiveBridgeMode === "channel_binary") {
    audioChannel.onmessage = (raw) => {
      const packet = toArrayBuffer(raw);
      if (packet.byteLength === 0) return;
      const { peerId, timestamp, pcmBytes } = unpackAudioChannelPacket(packet);
      handleIncomingPcm(peerId, timestamp, pcmBytes);
    };
    unlistenAudio = () => {
      if (audioChannel) {
        audioChannel.onmessage = () => {};
      }
      audioChannel = null;
    };

    videoChannel.onmessage = (raw) => {
      const packet = toArrayBuffer(raw);
      if (packet.byteLength === 0) return;
      if (hasWebCodecs) {
        const { peerId, timestamp, isKeyframe, h264Data } = parseRawNaluPacket(packet);
        const peerState = peerMediaStates.get(peerId);
        if (!peerState || peerState.videoPaused) return;
        if (!peerState.canvas) return;
        const decoder = getOrCreateVideoDecoder(peerId, peerState.canvas);
        if (decoder.state === "closed") return;
        try {
          decoder.decode(new EncodedVideoChunk({
            type: isKeyframe ? "key" : "delta",
            timestamp,
            data: h264Data,
          }));
        } catch (e) {
          console.warn("WebCodecs decode error:", e);
        }
        return; // Skip JPEG path
      }
      const { peerId, timestamp, width, height, jpegBytes } = unpackVideoChannelPacket(packet);
      const peerState = peerMediaStates.get(peerId);
      if (!peerState || peerState.videoPaused) return;
      handleIncomingVideoFrame(peerId, timestamp, width, height, jpegBytes).catch(() => {});
    };
    unlistenVideo = () => {
      if (videoChannel) {
        videoChannel.onmessage = () => {};
      }
      videoChannel = null;
    };
  } else {
    unlistenAudio = await listen<LegacyAudioEvent>("audio-received", (event) => {
      const payload = event.payload;
      handleIncomingPcm(payload.peer_id, payload.timestamp, fromBase64(payload.data));
    });
    unlistenVideo = await listen<LegacyVideoEvent>("video-received", (event) => {
      const payload = event.payload;
      const peerState = peerMediaStates.get(payload.peer_id);
      if (peerState?.videoPaused) return;
      handleIncomingVideoFrame(
        payload.peer_id,
        payload.timestamp,
        payload.width,
        payload.height,
        fromBase64(payload.data),
      ).catch(() => {});
    });
  }

  const probeWait = waitForBridgeProbe();
  await invoke("probe_media_bridge", { sessionId: profile.sessionId });

  try {
    await probeWait;
  } catch (error) {
    await teardownReceiveBridge(false);
    if (!bridgeFallbackUsed && profile.receiveBridgeMode === "channel_binary") {
      bridgeFallbackUsed = true;
      await invoke("clear_media_bridge", { sessionId: profile.sessionId }).catch(() => {});
      await setupReceiveBridge(true);
      return;
    }
    const message = `Media bridge setup failed: ${error instanceof Error ? error.message : String(error)}`;
    setTransportFailure(message);
    await reportPlaybackStatus();
    throw error;
  }
}

async function teardownReceiveBridge(clearBackend = true) {
  const sessionId = transportStatus.value.sessionId;

  unlistenAudio?.();
  unlistenVideo?.();
  unlistenAudio = null;
  unlistenVideo = null;
  audioChannel = null;
  videoChannel = null;
  bridgeProbeResolver = null;
  bridgeProbeReceived = false;

  if (clearBackend && sessionId) {
    const invoke = await invokePromise;
    await invoke("clear_media_bridge", { sessionId }).catch(() => {});
  }
}

function updateConnectionQualityFromStats() {
  const stats = Array.from(peerNetworkStats.values());
  let next: "good" | "degraded" | "poor" = "good";

  if (
    stats.some((stat) =>
      stat.latest_video_age_ms > 300 ||
      stat.rtt_ms > 180 ||
      stat.datagram_send_buffer_space < 4096,
    )
  ) {
    next = "poor";
  } else if (
    stats.some((stat) =>
      stat.latest_video_age_ms > 160 ||
      stat.rtt_ms > 110 ||
      stat.datagram_send_buffer_space < 16384,
    )
  ) {
    next = "degraded";
  }

  connectionQuality.value = next;
}

function createMediaUploader(
  invoke: typeof import("@tauri-apps/api/core").invoke,
): MediaUploader {
  return {
    mode: "invoke",
    sendAudio: async (pcmBytes, timestamp) => {
      if (preferJsonAudioInvoke) {
        await invoke("send_audio_all", { data: toBase64(pcmBytes), timestamp });
        return;
      }

      await invoke("send_audio_all", packAudioPayloadAll(timestamp, pcmBytes), {
        headers: { "Content-Type": "application/octet-stream" },
      }).catch(async (error) => {
        if (!loggedAudioInvokeFallback) {
          loggedAudioInvokeFallback = true;
          console.warn("[transport] Raw audio invoke failed, falling back to JSON payloads.", error);
        }
        preferJsonAudioInvoke = true;
        await invoke("send_audio_all", { data: toBase64(pcmBytes), timestamp });
      });
    },
    sendVideo: async (rgba, width, height, keyframe, timestamp) => {
      if (preferJsonVideoInvoke) {
        await invoke("send_video_all", {
          data: toBase64(rgba),
          width,
          height,
          keyframe,
          timestamp,
        });
        return;
      }

      await invoke(
        "send_video_all",
        packVideoPayloadAll(width, height, keyframe, timestamp, rgba),
        { headers: { "Content-Type": "application/octet-stream" } },
      ).catch(async (error) => {
        if (!loggedVideoInvokeFallback) {
          loggedVideoInvokeFallback = true;
          console.warn("[transport] Raw video invoke failed, falling back to JSON payloads.", error);
        }
        preferJsonVideoInvoke = true;
        await invoke("send_video_all", {
          data: toBase64(rgba),
          width,
          height,
          keyframe,
          timestamp,
        });
      });
    },
    close: () => {},
  };
}

export function useMediaTransport() {
  async function initCodecs(stream?: MediaStream | null) {
    const invoke = await invokePromise;
    const { width, height } = resolveCaptureDimensions(stream);
    currentWidth = width;
    currentHeight = height;
    targetFps = connectionQuality.value === "good" ? (isAndroid ? 8 : 12) : 8;
    audioBufferPool = new BufferPool(8, () => new Int16Array(OPUS_FRAME_SAMPLES));
    videoFrameBufferPool = new BufferPool(4, () => new Uint8Array(currentWidth * currentHeight * 4));
    await invoke("init_codecs", { width, height });
  }

  async function syncSubscriptions(peerIds = peerIdsProvider?.() ?? []) {
    for (const peerId of Array.from(initialKeyframeRequests)) {
      if (!peerIds.includes(peerId)) {
        initialKeyframeRequests.delete(peerId);
      }
    }

    for (const peerId of peerIds) {
      if (!initialKeyframeRequests.has(peerId)) {
        initialKeyframeRequests.add(peerId);
        requestKeyframe(peerId).catch(() => {});
      }
    }
  }

  function registerPeerCanvas(peerId: string, canvas: HTMLCanvasElement | null) {
    const peerState = getOrCreatePeerState(peerId);
    peerState.canvas = canvas;
    if (!canvas) {
      peerState.pendingVideoFrame = null;
      return;
    }
    if (peerState.pendingVideoFrame) {
      const frame = peerState.pendingVideoFrame;
      peerState.pendingVideoFrame = null;
      handleIncomingVideoFrame(peerId, frame.timestamp, frame.width, frame.height, frame.jpegBytes)
        .catch(() => {});
    } else {
      requestKeyframe(peerId).catch(() => {});
    }
  }

  async function startSending(stream: MediaStream) {
    if (encoding.value) return;
    encoding.value = true;
    activeCaptureStream = stream;

    const invoke = await invokePromise;
    mediaUploader = createMediaUploader(invoke);

    const audioTrack = stream.getAudioTracks()[0];
    if (audioTrack) {
      captureCtx = new AudioContext({ sampleRate: 48000 });
      if (captureCtx.state !== "running") {
        try {
          await captureCtx.resume();
        } catch (error) {
          const message = `Audio capture resume failed: ${error instanceof Error ? error.message : String(error)}`;
          setTransportFailure(message);
          await reportPlaybackStatus();
          throw new Error(message);
        }
      }
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

      sourceNode = captureCtx.createMediaStreamSource(new MediaStream([audioTrack]));
      workletNode = new AudioWorkletNode(captureCtx, "capture");
      captureSinkNode = captureCtx.createGain();
      captureSinkNode.gain.value = 0;

      const sampleBuffer = new Float32Array(OPUS_FRAME_SAMPLES);
      let bufferOffset = 0;

      workletNode.port.onmessage = (event) => {
        const { samples } = event.data as { samples: Float32Array };
        let srcOffset = 0;

        while (srcOffset < samples.length) {
          const remaining = OPUS_FRAME_SAMPLES - bufferOffset;
          const toCopy = Math.min(remaining, samples.length - srcOffset);
          sampleBuffer.set(samples.subarray(srcOffset, srcOffset + toCopy), bufferOffset);
          bufferOffset += toCopy;
          srcOffset += toCopy;

          if (bufferOffset === OPUS_FRAME_SAMPLES) {
            const pcm = audioBufferPool?.acquire() ?? new Int16Array(OPUS_FRAME_SAMPLES);
            for (let i = 0; i < OPUS_FRAME_SAMPLES; i++) {
              pcm[i] = Math.max(-32768, Math.min(32767, Math.round(sampleBuffer[i]! * 32767)));
            }
            const sendPromise = mediaUploader?.sendAudio(new Uint8Array(pcm.buffer), Date.now());
            if (sendPromise) {
              sendPromise.catch(() => {}).finally(() => { audioBufferPool?.release(pcm); });
            } else {
              audioBufferPool?.release(pcm);
            }
            bufferOffset = 0;
          }
        }
      };

      sourceNode.connect(workletNode);
      workletNode.connect(captureSinkNode);
      captureSinkNode.connect(captureCtx.destination);
    }

    const videoTrack = stream.getVideoTracks()[0];
    if (videoTrack) {
      if (captureVideoEl) {
        captureVideoEl.pause();
        captureVideoEl.srcObject = null;
      }
      captureVideoEl = document.createElement("video");
      captureVideoEl.srcObject = stream;
      captureVideoEl.muted = true;
      captureVideoEl.playsInline = true;
      captureVideoEl.preload = "auto";
      // Position off-screen but keep a real render size so WebKit fires
      // requestVideoFrameCallback and updates readyState properly.
      captureVideoEl.style.cssText = "position:fixed;left:-9999px;top:-9999px;width:640px;height:360px;pointer-events:none;z-index:-1";
      document.body.appendChild(captureVideoEl);

      let lastCaptureTime = 0;
      let frameCount = 0;
      let captureLoopStarted = false;

      // Always use requestAnimationFrame for the capture loop.
      // requestVideoFrameCallback exists in WKWebView but never fires for
      // programmatically-created video elements, so RAF is more reliable.
      const rafCaptureLoop = () => {
        if (!encoding.value || !captureVideoEl) return;

        const now = performance.now();
        const elapsed = now - lastCaptureTime;
        if (elapsed >= 1000 / targetFps) {
          lastCaptureTime = now;
          const ctx = ensureCaptureSurface(currentWidth, currentHeight);
          if (ctx && drawContainedVideoFrame(ctx, currentWidth, currentHeight)) {
            const imageData = ctx.getImageData(0, 0, currentWidth, currentHeight);
            const keyframe = frameCount === 0 || frameCount % 48 === 0;
            frameCount += 1;
            const frameSize = currentWidth * currentHeight * 4;
            const rgba = videoFrameBufferPool?.acquire() ?? new Uint8Array(frameSize);
            rgba.set(new Uint8Array(imageData.data.buffer));
            const sendPromise = mediaUploader?.sendVideo(
              rgba,
              currentWidth,
              currentHeight,
              keyframe,
              Date.now(),
            );
            if (sendPromise) {
              sendPromise.catch(() => {}).finally(() => { videoFrameBufferPool?.release(rgba); });
            } else {
              videoFrameBufferPool?.release(rgba);
            }
          }
        }

        requestAnimationFrame(rafCaptureLoop);
      };

      const startCaptureLoop = () => {
        if (captureLoopStarted || !captureVideoEl) return;
        captureLoopStarted = true;
        requestAnimationFrame(rafCaptureLoop);
      };

      if (captureVideoEl.readyState >= HTMLMediaElement.HAVE_METADATA) {
        startCaptureLoop();
      } else {
        captureVideoEl.addEventListener("loadedmetadata", startCaptureLoop, { once: true });
      }

      captureVideoEl.play()
        .then(() => startCaptureLoop())
        .catch((error) => {
          console.warn("[transport] Failed to start capture video element.", error);
        });
    }
  }

  async function startReceiving(getPeerIds: () => string[]) {
    peerIdsProvider = getPeerIds;
    if (transportStatus.value.state === "starting" || transportStatus.value.state === "running") {
      await syncSubscriptions(getPeerIds());
      return;
    }

    transportStatus.value = {
      state: "starting",
      sessionId: null,
      selectedMode: null,
      audioReady: false,
      videoReady: false,
      bridgeReady: false,
      lastFailure: null,
    };

    await ensurePlaybackContext();
    bridgeFallbackUsed = false;
    await teardownReceiveBridge(false);
    await setupReceiveBridge(false);

    const { listen } = await import("@tauri-apps/api/event");
    unlistenDisconnect = await listen<{ peer_id: string }>("peer-disconnected", (event) => {
      const pid = typeof event.payload === "string" ? event.payload : event.payload?.peer_id;
      if (!pid) return;
      const state = peerMediaStates.get(pid);
      if (state?.audioGainNode) {
        try { state.audioGainNode.disconnect(); } catch {}
      }
      destroyVideoDecoder(pid);
      peerMediaStates.delete(pid);
      peerNetworkStats.delete(pid);
      initialKeyframeRequests.delete(pid);
      if (activeSpeaker.value === pid) activeSpeaker.value = null;
      updateSpeakingMap();
    });

    unlistenStats = await listen<PeerNetworkStats>("network-stats", (event) => {
      peerNetworkStats.set(event.payload.peer_id, event.payload);
      updateConnectionQualityFromStats();
    });

    unlistenQuality = await listen<{
      peer_count: number;
      bitrate_bps: number;
      fps: number;
      max_width: number;
      max_height: number;
    }>("quality-profile-changed", async (event) => {
      const { bitrate_bps, fps, max_width, max_height } = event.payload;
      targetFps = fps;
      const { width, height } = resolveCaptureDimensions(activeCaptureStream, {
        maxWidth: max_width,
        maxHeight: max_height,
      });
      currentWidth = width;
      currentHeight = height;
      clearCaptureSurface();
      const invoke = await invokePromise;
      await invoke("reinit_video_encoder_with_config", {
        width,
        height,
        bitrateBps: bitrate_bps,
        fps: fps as number,
      });
    });

    stopQualityWatch = watch(connectionQuality, async (quality) => {
      targetFps = quality === "poor" ? 6 : quality === "degraded" ? 8 : (isAndroid ? 8 : 12);
      const { width, height } = resolveCaptureDimensions(activeCaptureStream);
      await updateCaptureDimensions(width, height);
    });

    startActiveSpeakerDetection();
    await syncSubscriptions(getPeerIds());
  }

  function startActiveSpeakerDetection() {
    if (activeSpeakerInterval) return;
    let speakerCheckRunning = false;
    activeSpeakerInterval = setInterval(async () => {
      if (speakerCheckRunning) return;
      speakerCheckRunning = true;
      try {
        const now = Date.now();
        let loudest: string | null = null;
        let loudestRms = 0;

        for (const [peerId, state] of peerMediaStates) {
          if (state.lastAudioRms > loudestRms) {
            loudestRms = state.lastAudioRms;
            loudest = peerId;
          }
        }

        const current = activeSpeaker.value;
        const currentState = current ? peerMediaStates.get(current) : null;
        let shouldSwitch = false;

        if (!current) {
          shouldSwitch = loudest !== null && loudestRms > SPEAKING_RMS_THRESHOLD;
        } else if (currentState) {
          const currentSilent = now - currentState.lastSpeakingTime > ACTIVE_SPEAKER_SILENCE_MS;
          if (currentSilent && loudest && loudest !== current) {
            shouldSwitch = true;
          } else if (loudest && loudest !== current) {
            const louderEnough = loudestRms > currentState.lastAudioRms * ACTIVE_SPEAKER_SWITCH_THRESHOLD;
            const loudestState = peerMediaStates.get(loudest);
            const speakingLongEnough = loudestState &&
              loudestState.speaking &&
              (now - loudestState.speakingSince) >= ACTIVE_SPEAKER_MIN_DURATION_MS;
            shouldSwitch = louderEnough && !!speakingLongEnough;
          }
        }

        if (shouldSwitch && loudest) {
          activeSpeaker.value = loudest;
        }
      } finally {
        speakerCheckRunning = false;
      }
    }, ACTIVE_SPEAKER_INTERVAL_MS);
  }

  async function updateCaptureDimensions(width: number, height: number) {
    if (width === currentWidth && height === currentHeight) return;
    currentWidth = width;
    currentHeight = height;
    clearCaptureSurface();
    const invoke = await invokePromise;
    await invoke("reinit_video_encoder", { width, height });
  }

  function teardownCapture() {
    encoding.value = false;
    activeCaptureStream = null;
    mediaUploader?.close();
    mediaUploader = null;
    if (workletNode) {
      workletNode.port.onmessage = null;
      workletNode.disconnect();
      workletNode = null;
    }
    if (sourceNode) {
      sourceNode.disconnect();
      sourceNode = null;
    }
    if (captureSinkNode) {
      captureSinkNode.disconnect();
      captureSinkNode = null;
    }
    if (captureCtx) {
      captureCtx.close();
      captureCtx = null;
    }
    if (captureVideoEl) {
      captureVideoEl.pause();
      captureVideoEl.srcObject = null;
      captureVideoEl.remove();
      captureVideoEl = null;
    }
    clearCaptureSurface();
  }

  async function restartSending(newStream: MediaStream) {
    teardownCapture();
    await initCodecs(newStream);
    await startSending(newStream);
  }

  async function stop() {
    transportStatus.value = {
      ...transportStatus.value,
      state: "stopping",
    };

    teardownCapture();
    await teardownReceiveBridge(true);

    if (playbackCtx) {
      await playbackCtx.close();
      playbackCtx = null;
    }
    if (activeSpeakerInterval) {
      clearInterval(activeSpeakerInterval);
      activeSpeakerInterval = null;
    }
    stopQualityWatch?.();
    stopQualityWatch = null;

    for (const [, state] of peerMediaStates) {
      if (state.audioGainNode) {
        try { state.audioGainNode.disconnect(); } catch {}
      }
    }

    for (const peerId of peerVideoDecoders.keys()) {
      destroyVideoDecoder(peerId);
    }

    peerMediaStates.clear();
    peerNetworkStats.clear();
    initialKeyframeRequests.clear();
    activeSpeaker.value = null;
    peerSpeakingMap.value = {};
    peerIdsProvider = null;

    unlistenDisconnect?.();
    unlistenStats?.();
    unlistenQuality?.();
    unlistenDisconnect = null;
    unlistenStats = null;
    unlistenQuality = null;
    connectionQuality.value = "good";
    audioBufferPool = null;
    videoFrameBufferPool = null;

    transportStatus.value = {
      state: "idle",
      sessionId: null,
      selectedMode: null,
      audioReady: false,
      videoReady: false,
      bridgeReady: false,
      lastFailure: null,
    };
  }

  async function setPeerVideoPaused(peerId: string, paused: boolean) {
    const state = peerMediaStates.get(peerId);
    if (!state || state.videoPaused === paused) return;
    state.videoPaused = paused;
    const invoke = await invokePromise;
    await invoke("send_control", {
      peerId,
      action: {
        action: "video_quality_request",
        layer: paused ? "none" : "high",
      },
    }).catch(() => {});
  }

  return {
    encoding,
    peerSpeakingMap,
    activeSpeaker,
    connectionQuality,
    status: transportStatus,
    initCodecs,
    registerPeerCanvas,
    startSending,
    restartSending,
    startReceiving,
    stop,
    syncSubscriptions,
    updateCaptureDimensions,
    setPeerVideoPaused,
  };
}
