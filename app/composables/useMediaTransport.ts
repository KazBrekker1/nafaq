import { Channel } from "@tauri-apps/api/core";
import { PLAYBACK_PROCESSOR_NAME, playbackWorkletSource } from "~/utils/jitterBuffer";
import { applySendLevel, sendQualityForLevel, type VideoProfile } from "~/utils/sendQuality";

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
  webcodecsActive: boolean;
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
  /** Playback worklet holding this peer's jitter buffer. */
  audioNode: AudioWorkletNode | null;
  audioGainNode: GainNode | null;
  speaking: boolean;
  lastAudioRms: number;
  speakingSince: number;
  lastSpeakingTime: number;
  lastKeyframeRequestAt: number;
  pendingVideoFrame: PendingVideoFrame | null;
  /** A JPEG frame is being decoded/drawn; newer ones wait in pendingVideoFrame. */
  jpegRendering: boolean;
  lastDrawnTimestamp: number;
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
  /** Returns whether the next frame must be a keyframe. */
  sendEncodedVideo: (h264: Uint8Array, timestamp: number) => Promise<boolean>;
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
let captureRafId: number | null = null;
let captureCanvas: OffscreenCanvas | HTMLCanvasElement | null = null;
let captureCanvasCtx: OffscreenCanvasRenderingContext2D | CanvasRenderingContext2D | null = null;
let activeCaptureStream: MediaStream | null = null;
let workletNode: AudioWorkletNode | null = null;
let sourceNode: MediaStreamAudioSourceNode | null = null;
let captureSinkNode: GainNode | null = null;
let unlistenAudio: (() => void) | null = null;
let unlistenVideo: (() => void) | null = null;
let unlistenDisconnect: (() => void) | null = null;
let unlistenQuality: (() => void) | null = null;
let activeSpeakerInterval: ReturnType<typeof setInterval> | null = null;
const isAndroidUa = /android/i.test(navigator.userAgent);
const DEFAULT_PROFILE: VideoProfile = {
  bitrateBps: 400_000,
  fps: isAndroidUa ? 8 : 12,
  maxWidth: 640,
  maxHeight: 360,
};
// Call-size profile (quality-profile-changed) and the backend controller's
// congestion level (send-quality-changed); capture follows their combination.
let baseProfile: VideoProfile = { ...DEFAULT_PROFILE };
let sendLevel = 0;
let currentWidth = 640;
let currentHeight = 360;
let targetFps = DEFAULT_PROFILE.fps;
let unlistenSendQuality: (() => void) | null = null;
let playbackWorkletLoaded: Promise<boolean> | null = null;
let videoSendInFlight = false;
let audioSendChain: Promise<void> = Promise.resolve();
let audioSendsPending = 0;
const MAX_PENDING_AUDIO_SENDS = 5;
const AUDIO_CONTEXT_RESUME_TIMEOUT_MS = 1500;
let peerIdsProvider: (() => string[]) | null = null;
let audioChannel: Channel<ArrayBuffer> | null = null;
let videoChannel: Channel<ArrayBuffer> | null = null;
let mediaUploader: MediaUploader | null = null;
let bridgeProbeResolver: (() => void) | null = null;
let bridgeProbeReceived = false;
let bridgeFallbackUsed = false;

const isAndroid = isAndroidUa;
const DECODER_CODEC = "avc1.42001E"; // H.264 Constrained Baseline Level 3.0
// A VideoDecoder global doesn't guarantee H.264 support (e.g. Linux WebKitGTK
// without the codec plugins); probe once and reuse the answer.
let webCodecsDecodeSupport: Promise<boolean> | null = null;
function supportsWebCodecsDecode(): Promise<boolean> {
  if (!webCodecsDecodeSupport) {
    webCodecsDecodeSupport = typeof VideoDecoder === "undefined"
      ? Promise.resolve(false)
      : VideoDecoder.isConfigSupported({ codec: DECODER_CODEC })
        .then((support) => support.supported === true, () => false);
  }
  return webCodecsDecodeSupport;
}
const sharedTextDecoder = new TextDecoder();
let preferJsonAudioInvoke = isAndroid;
let preferJsonVideoInvoke = isAndroid;
let loggedAudioInvokeFallback = false;
let loggedVideoInvokeFallback = false;

const peerMediaStates = new Map<string, PeerMediaState>();
const initialKeyframeRequests = new Set<string>();

const peerVideoDecoders = new Map<string, VideoDecoder>();
// Peers whose decoder hasn't yet seen its first keyframe. Feeding a fresh H.264
// decoder a delta frame before an IDR produces a decode error and a black
// canvas, so we drop deltas until the keyframe (which syncSubscriptions
// requests on join) arrives.
const peersAwaitingKeyframe = new Set<string>();

// Per-peer decode error tracking so a persistently broken stream (e.g. a
// corrupt or unsupported bitstream) doesn't spin the CPU recreating and
// immediately re-erroring the decoder on every incoming frame.
const peerDecoderErrorTimestamps = new Map<string, number[]>();
const peerDecoderCooldownUntil = new Map<string, number>();
const DECODER_ERROR_STORM_LIMIT = 3;
const DECODER_ERROR_STORM_WINDOW_MS = 10_000;
const DECODER_ERROR_COOLDOWN_MS = 10_000;

function isVideoDecoderInCooldown(peerId: string): boolean {
  const until = peerDecoderCooldownUntil.get(peerId);
  if (until === undefined) return false;
  if (Date.now() >= until) {
    peerDecoderCooldownUntil.delete(peerId);
    return false;
  }
  return true;
}

// Called whenever a peer's decoder can no longer be trusted (WebCodecs
// `error` callback, or a synchronous throw from decode()/getOrCreateVideoDecoder
// finding an already-closed decoder). Tears the decoder down, re-arms the
// awaiting-first-keyframe gate so the replacement never gets fed a delta
// before an IDR, and asks the peer for a fresh keyframe. If errors keep
// recurring in a short window, backs off from recreating the decoder for a
// cooldown period instead of spinning.
function recoverVideoDecoder(peerId: string, reason: string) {
  destroyVideoDecoder(peerId);
  peersAwaitingKeyframe.add(peerId);

  const now = Date.now();
  const recent = (peerDecoderErrorTimestamps.get(peerId) ?? []).filter(
    (t) => now - t < DECODER_ERROR_STORM_WINDOW_MS,
  );
  recent.push(now);
  peerDecoderErrorTimestamps.set(peerId, recent);

  if (recent.length > DECODER_ERROR_STORM_LIMIT) {
    peerDecoderCooldownUntil.set(peerId, now + DECODER_ERROR_COOLDOWN_MS);
    console.warn(
      `VideoDecoder for ${peerId} errored ${recent.length} times in ${DECODER_ERROR_STORM_WINDOW_MS}ms (${reason}); pausing decoder recreation for ${DECODER_ERROR_COOLDOWN_MS}ms`,
    );
  } else {
    console.warn(`VideoDecoder recovery for ${peerId}: ${reason}`);
  }

  requestKeyframe(peerId).catch(() => {});
}

function getOrCreateVideoDecoder(peerId: string, canvas: HTMLCanvasElement): VideoDecoder {
  let decoder = peerVideoDecoders.get(peerId);
  if (decoder && decoder.state === "closed") {
    // Per the WebCodecs spec, once `error` fires the decoder is permanently
    // closed — returning it here would silently drop every future frame.
    peerVideoDecoders.delete(peerId);
    decoder = undefined;
  }
  if (decoder) return decoder;

  peersAwaitingKeyframe.add(peerId);
  const ctx = canvas.getContext("2d")!;
  decoder = new VideoDecoder({
    output(frame: VideoFrame) {
      // Assigning width/height reallocates the canvas even when unchanged.
      if (canvas.width !== frame.displayWidth) canvas.width = frame.displayWidth;
      if (canvas.height !== frame.displayHeight) canvas.height = frame.displayHeight;
      ctx.drawImage(frame, 0, 0, canvas.width, canvas.height);
      frame.close();
    },
    error(e: DOMException) {
      recoverVideoDecoder(peerId, `VideoDecoder error: ${e.message}`);
    },
  });
  decoder.configure({
    codec: DECODER_CODEC,
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
  peersAwaitingKeyframe.delete(peerId);
}

function forgetPeerVideoDecoderState(peerId: string) {
  destroyVideoDecoder(peerId);
  peerDecoderErrorTimestamps.delete(peerId);
  peerDecoderCooldownUntil.delete(peerId);
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

function effectiveProfile() {
  return applySendLevel(baseProfile, sendLevel);
}

function currentCaptureBounds() {
  const { maxWidth, maxHeight } = effectiveProfile();
  return { maxWidth, maxHeight };
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
      audioNode: null,
      audioGainNode: null,
      speaking: false,
      lastAudioRms: 0,
      speakingSince: 0,
      lastSpeakingTime: 0,
      lastKeyframeRequestAt: 0,
      pendingVideoFrame: null,
      jpegRendering: false,
      lastDrawnTimestamp: 0,
      videoPaused: false,
    };
    peerMediaStates.set(peerId, state);
  }
  return state;
}

function ensurePeerAudioNode(peerState: PeerMediaState) {
  if (!playbackCtx || peerState.audioNode) return;
  const node = new AudioWorkletNode(playbackCtx, PLAYBACK_PROCESSOR_NAME, {
    numberOfInputs: 0,
    numberOfOutputs: 1,
    outputChannelCount: [1],
  });
  const gain = playbackCtx.createGain();
  gain.gain.value = 1;
  node.connect(gain);
  gain.connect(playbackCtx.destination);
  peerState.audioNode = node;
  peerState.audioGainNode = gain;
}

function releasePeerAudio(peerState: PeerMediaState) {
  if (peerState.audioNode) {
    try { peerState.audioNode.disconnect(); } catch {}
    peerState.audioNode.port.close();
    peerState.audioNode = null;
  }
  if (peerState.audioGainNode) {
    try { peerState.audioGainNode.disconnect(); } catch {}
    peerState.audioGainNode = null;
  }
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

async function requestKeyframe(peerId: string, force = false) {
  const peerState = getOrCreatePeerState(peerId);
  const now = Date.now();
  if (!force && now - peerState.lastKeyframeRequestAt < KEYFRAME_REQUEST_DEBOUNCE_MS) return;
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

// AudioContext.resume() can stay pending forever without a user gesture
// (WebKit, Android WebView) — never let it block call setup.
async function resumeWithTimeout(ctx: AudioContext) {
  if (ctx.state === "running") return true;
  const resumed = await Promise.race([
    ctx.resume().then(() => true, () => false),
    new Promise<boolean>((resolve) => setTimeout(() => resolve(false), AUDIO_CONTEXT_RESUME_TIMEOUT_MS)),
  ]);
  // Re-read state: TS narrowed it above, but resume() changed it.
  return resumed && (ctx.state as AudioContextState) === "running";
}

// Keep a context running across interruptions (incoming phone call, sleep,
// Bluetooth route change): resume on statechange, and on the next user
// gesture when the platform refuses to resume without one.
function keepContextRunning(ctx: AudioContext, label: string) {
  const resumeOnGesture = () => {
    const handler = () => {
      document.removeEventListener("touchstart", handler);
      document.removeEventListener("click", handler);
      if (ctx.state !== "closed" && ctx.state !== "running") ctx.resume().catch(() => {});
    };
    document.addEventListener("touchstart", handler, { once: true });
    document.addEventListener("click", handler, { once: true });
  };
  ctx.addEventListener("statechange", () => {
    // "interrupted" is WebKit's state for OS-level interruptions.
    if (ctx.state === "suspended" || (ctx.state as string) === "interrupted") {
      console.warn(`[transport] ${label} AudioContext ${ctx.state}; resuming`);
      resumeWithTimeout(ctx).then((ok) => { if (!ok) resumeOnGesture(); });
    }
  });
  return resumeOnGesture;
}

async function ensurePlaybackContext() {
  if (!playbackCtx) {
    playbackCtx = new AudioContext({ sampleRate: 48000 });
    const resumeOnGesture = keepContextRunning(playbackCtx, "playback");
    if (!(await resumeWithTimeout(playbackCtx))) {
      console.warn("[transport] Playback AudioContext resume deferred until user interaction");
      resumeOnGesture();
    }
  } else if (playbackCtx.state !== "running") {
    await resumeWithTimeout(playbackCtx);
  }

  if (!playbackWorkletLoaded) {
    const ctx = playbackCtx;
    const blobUrl = URL.createObjectURL(
      new Blob([playbackWorkletSource()], { type: "application/javascript" }),
    );
    playbackWorkletLoaded = ctx.audioWorklet.addModule(blobUrl)
      .then(() => true)
      .catch((error) => {
        console.warn("[transport] Playback worklet failed to load", error);
        return false;
      })
      .finally(() => URL.revokeObjectURL(blobUrl));
  }
  if (!(await playbackWorkletLoaded)) {
    playbackWorkletLoaded = null;
    throw new Error("Audio playback worklet unavailable");
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

function handleIncomingPcm(peerId: string, _timestamp: number, pcmBytes: Uint8Array) {
  if (handleBridgeProbe(peerId)) return;
  if (!playbackCtx) return;
  if (pcmBytes.byteLength < 2) return;

  // Copy out of the IPC buffer (it may be unaligned for Int16Array).
  const int16 = new Int16Array(pcmBytes.slice(0, pcmBytes.byteLength & ~1).buffer);
  const samples = new Float32Array(int16.length);
  let sum = 0;
  for (let i = 0; i < int16.length; i++) {
    const sample = int16[i]! / 32768;
    samples[i] = sample;
    sum += sample * sample;
  }

  const peerState = getOrCreatePeerState(peerId);
  ensurePeerAudioNode(peerState);
  peerState.audioNode?.port.postMessage({ pcm: samples }, [samples.buffer]);

  const rms = Math.sqrt(sum / int16.length);
  const now = Date.now();
  peerState.lastAudioRms = 0.7 * peerState.lastAudioRms + 0.3 * rms;

  const wasSpeaking = peerState.speaking;
  if (rms > SPEAKING_RMS_THRESHOLD) {
    if (!peerState.speaking) {
      peerState.speaking = true;
      peerState.speakingSince = now;
    }
    peerState.lastSpeakingTime = now;
  } else if (peerState.speaking && now - peerState.lastSpeakingTime > SPEAKING_DEBOUNCE_MS) {
    peerState.speaking = false;
  }

  // Only touch reactive state on an actual change — this runs 50x/s per peer.
  if (peerState.speaking !== wasSpeaking) updateSpeakingMap();
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
  // Image decoding is async, so frames could finish out of order. Keep one
  // render in flight per peer; anything arriving meanwhile replaces the
  // waiting frame (latest wins) and is drawn when the current one is done.
  if (!peerState.canvas || peerState.jpegRendering) {
    peerState.pendingVideoFrame = frame;
    return;
  }

  peerState.jpegRendering = true;
  let next: PendingVideoFrame | null = frame;
  try {
    while (next && peerState.canvas) {
      const current: PendingVideoFrame = next;
      peerState.pendingVideoFrame = null;
      if (current.timestamp >= peerState.lastDrawnTimestamp) {
        await drawJpegToCanvas(peerState.canvas, current.jpegBytes, current.width, current.height);
        peerState.lastDrawnTimestamp = current.timestamp;
        markVideoReady();
      }
      next = peerState.pendingVideoFrame;
    }
  } catch (error) {
    const message = `Video frame render failed: ${error instanceof Error ? error.message : String(error)}`;
    setTransportFailure(message);
    await reportPlaybackStatus();
  } finally {
    peerState.jpegRendering = false;
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
    // Raw NALUs only travel over the binary channel; in event mode the
    // backend has to decode to JPEG, or no video would arrive at all.
    webcodecsActive: !forceEventMode && await supportsWebCodecsDecode(),
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
      if (profile.receiveVideoMode === "raw_h264_nalu") {
        const { peerId, timestamp, isKeyframe, h264Data } = parseRawNaluPacket(packet);
        const peerState = peerMediaStates.get(peerId);
        if (!peerState || peerState.videoPaused) return;
        if (!peerState.canvas) return;
        if (isVideoDecoderInCooldown(peerId)) {
          // Backing off from a recent error storm — keep asking for a
          // keyframe (debounced) but don't recreate the decoder every frame.
          requestKeyframe(peerId).catch(() => {});
          return;
        }
        const decoder = getOrCreateVideoDecoder(peerId, peerState.canvas);
        if (decoder.state === "closed") {
          recoverVideoDecoder(peerId, "decoder was already closed before decode");
          return;
        }
        if (isKeyframe) {
          peersAwaitingKeyframe.delete(peerId);
        } else if (peersAwaitingKeyframe.has(peerId)) {
          // Still waiting for the first IDR — dropping this delta avoids a
          // decoder error and the black frame it would cause.
          return;
        }
        try {
          decoder.decode(new EncodedVideoChunk({
            type: isKeyframe ? "key" : "delta",
            timestamp,
            data: h264Data,
          }));
        } catch (e) {
          recoverVideoDecoder(peerId, `decode() threw: ${e instanceof Error ? e.message : String(e)}`);
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

function disposeReceiveListeners() {
  unlistenDisconnect?.();
  unlistenDisconnect = null;
  unlistenQuality?.();
  unlistenQuality = null;
  unlistenSendQuality?.();
  unlistenSendQuality = null;
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
    sendEncodedVideo: async (h264, timestamp) => {
      if (preferJsonVideoInvoke) {
        return await invoke<boolean>("send_encoded_video_all", { data: toBase64(h264), timestamp });
      }
      const payload = new Uint8Array(8 + h264.length);
      new DataView(payload.buffer).setBigUint64(0, BigInt(timestamp), true);
      payload.set(h264, 8);
      return await invoke<boolean>("send_encoded_video_all", payload, {
        headers: { "Content-Type": "application/octet-stream" },
      });
    },
    close: () => {},
  };
}

// ── In-webview H.264 encoding (Android) ─────────────────────────────
// Android's IPC bridge can only carry JSON, so the RGBA path ships ~1.2 MB of
// base64 per frame. Where the WebView can encode H.264 itself, only the
// encoded frame (a few KB) crosses IPC and the capture canvas is never read
// back. Falls back to the RGBA path if unsupported or the encoder fails.

const WEB_ENCODER_CODEC = "avc1.42001f"; // Constrained Baseline 3.1: decodable by openh264 too
let webEncoder: VideoEncoder | null = null;
let webEncoderFailed = false;
let webEncoderForceKeyframe = false;
let webEncoderSendInFlight = false;
let webEncoderConfigKey = "";

function webEncoderConfig(): VideoEncoderConfig {
  return {
    codec: WEB_ENCODER_CODEC,
    width: currentWidth,
    height: currentHeight,
    bitrate: effectiveProfile().bitrateBps,
    framerate: targetFps,
    latencyMode: "realtime",
    avc: { format: "annexb" },
  };
}

async function setupWebEncoder(): Promise<boolean> {
  if (!isAndroid || webEncoderFailed || typeof VideoEncoder === "undefined") return false;
  if (webEncoder) return true;
  try {
    const config = webEncoderConfig();
    const support = await VideoEncoder.isConfigSupported(config);
    if (!support.supported) {
      webEncoderFailed = true;
      return false;
    }
    const encoder = new VideoEncoder({
      output(chunk) {
        const data = new Uint8Array(chunk.byteLength);
        chunk.copyTo(data);
        if (!mediaUploader) return;
        webEncoderSendInFlight = true;
        mediaUploader.sendEncodedVideo(data, Date.now())
          .then((forceKeyframe) => { if (forceKeyframe) webEncoderForceKeyframe = true; })
          .catch(() => {})
          .finally(() => { webEncoderSendInFlight = false; });
      },
      error(e) {
        console.warn("[transport] WebCodecs encoder failed; falling back to RGBA uploads", e);
        webEncoderFailed = true;
        closeWebEncoder();
      },
    });
    encoder.configure(config);
    webEncoderConfigKey = JSON.stringify(config);
    webEncoder = encoder;
    webEncoderForceKeyframe = true;
    return true;
  } catch (e) {
    console.warn("[transport] WebCodecs encoder unavailable", e);
    webEncoderFailed = true;
    return false;
  }
}

function reconfigureWebEncoder() {
  if (!webEncoder || webEncoder.state !== "configured") return;
  const config = webEncoderConfig();
  const key = JSON.stringify(config);
  if (key === webEncoderConfigKey) return;
  try {
    webEncoder.configure(config);
    webEncoderConfigKey = key;
    webEncoderForceKeyframe = true;
  } catch (e) {
    console.warn("[transport] WebCodecs encoder reconfigure failed", e);
  }
}

function closeWebEncoder() {
  if (webEncoder && webEncoder.state !== "closed") {
    try { webEncoder.close(); } catch {}
  }
  webEncoder = null;
  webEncoderConfigKey = "";
  webEncoderSendInFlight = false;
}

export function useMediaTransport() {
  async function initCodecs(stream?: MediaStream | null) {
    const invoke = await invokePromise;
    const { width, height } = resolveCaptureDimensions(stream);
    currentWidth = width;
    currentHeight = height;
    targetFps = effectiveProfile().fps;
    audioBufferPool = new BufferPool(8, () => new Int16Array(OPUS_FRAME_SAMPLES));
    videoFrameBufferPool = new BufferPool(4, () => new Uint8Array(currentWidth * currentHeight * 4));
    // Encoder runs at the call-size profile; the backend's quality controller
    // lowers bitrate/fps from there at runtime.
    await invoke("init_codecs", {
      width,
      height,
      bitrateBps: baseProfile.bitrateBps,
      fps: baseProfile.fps,
    });
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
    // Function refs fire on every re-render of the tile, not just on mount —
    // only a real change may cost a decoder reset and a keyframe request.
    const peerState = getOrCreatePeerState(peerId);
    if (peerState.canvas === canvas) return;
    peerState.canvas = canvas;
    // The decoder's output callback draws to the canvas it was created with.
    destroyVideoDecoder(peerId);
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
      // Bypass the debounce: syncSubscriptions may have already requested a
      // keyframe for this peer moments ago (e.g. on join), which would
      // otherwise suppress this request and leave the canvas black until the
      // next periodic IDR.
      peersAwaitingKeyframe.add(peerId);
      requestKeyframe(peerId, true).catch(() => {});
    }
  }

  async function startSending(stream: MediaStream) {
    if (encoding.value) return;
    encoding.value = true;
    activeCaptureStream = stream;

    try {
      await startSendingInner(stream);
    } catch (error) {
      // Release everything the partial setup created (capture AudioContext,
      // off-screen video element, worklet) — otherwise a failed start leaks
      // them and blocks the next attempt.
      teardownCapture();
      throw error;
    }
  }

  async function startSendingInner(stream: MediaStream) {
    const invoke = await invokePromise;
    mediaUploader = createMediaUploader(invoke);

    const audioTrack = stream.getAudioTracks()[0];
    if (audioTrack) {
      captureCtx = new AudioContext({ sampleRate: 48000 });
      const resumeCaptureOnGesture = keepContextRunning(captureCtx, "capture");
      if (!(await resumeWithTimeout(captureCtx))) {
        // Don't fail (or hang) the call: capture starts on the next gesture.
        setTransportFailure("Microphone capture is waiting for user interaction");
        resumeCaptureOnGesture();
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
      try {
        await captureCtx.audioWorklet.addModule(blobUrl);
      } finally {
        URL.revokeObjectURL(blobUrl);
      }

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
            // Uploads are chained so frames reach the encoder in capture
            // order; if IPC falls behind, shed the newest instead of queueing
            // unbounded latency.
            const uploader = mediaUploader;
            if (!uploader || audioSendsPending >= MAX_PENDING_AUDIO_SENDS) {
              audioBufferPool?.release(pcm);
            } else {
              const timestamp = Date.now();
              audioSendsPending++;
              audioSendChain = audioSendChain
                .then(() => uploader.sendAudio(new Uint8Array(pcm.buffer), timestamp))
                .catch(() => {})
                .finally(() => {
                  audioSendsPending--;
                  audioBufferPool?.release(pcm);
                });
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
        // Detach the old element too, or it stays orphaned in the DOM forever.
        captureVideoEl.remove();
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
        captureRafId = null;
        if (!encoding.value || !captureVideoEl) return;

        const now = performance.now();
        const elapsed = now - lastCaptureTime;
        // One frame in flight: while the previous upload/encode is still
        // running, skip capture instead of queueing frames (and memory)
        // behind a slow encoder or IPC bridge.
        const busy = webEncoder
          ? webEncoderSendInFlight || webEncoder.encodeQueueSize > 0
          : videoSendInFlight;
        if (elapsed >= 1000 / targetFps && !busy) {
          lastCaptureTime = now;
          const ctx = ensureCaptureSurface(currentWidth, currentHeight);
          if (ctx && captureCanvas && drawContainedVideoFrame(ctx, currentWidth, currentHeight)) {
            const keyframe = frameCount === 0 || frameCount % 48 === 0;
            frameCount += 1;
            if (webEncoder && webEncoder.state === "configured") {
              reconfigureWebEncoder();
              const frame = new VideoFrame(captureCanvas, { timestamp: Math.round(now * 1000) });
              try {
                webEncoder.encode(frame, { keyFrame: keyframe || webEncoderForceKeyframe });
                webEncoderForceKeyframe = false;
              } finally {
                frame.close();
              }
            } else if (mediaUploader) {
              const imageData = ctx.getImageData(0, 0, currentWidth, currentHeight);
              if (!videoFrameBufferPool) {
                videoFrameBufferPool = new BufferPool(4, () => new Uint8Array(currentWidth * currentHeight * 4));
              }
              const rgba = videoFrameBufferPool.acquire();
              if (rgba.length !== imageData.data.length) {
                videoFrameBufferPool = null;
              } else {
                rgba.set(imageData.data);
                videoSendInFlight = true;
                mediaUploader
                  .sendVideo(rgba, currentWidth, currentHeight, keyframe, Date.now())
                  .catch(() => {})
                  .finally(() => {
                    videoSendInFlight = false;
                    videoFrameBufferPool?.release(rgba);
                  });
              }
            }
          }
        }

        captureRafId = requestAnimationFrame(rafCaptureLoop);
      };

      await setupWebEncoder();

      const startCaptureLoop = () => {
        if (captureLoopStarted || !captureVideoEl) return;
        captureLoopStarted = true;
        captureRafId = requestAnimationFrame(rafCaptureLoop);
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

    // A previous run that failed mid-setup (or a stale degraded session) may
    // have left listeners or the quality watcher behind — dispose them before
    // registering new ones so they never stack.
    disposeReceiveListeners();

    try {
      await startReceivingInner(getPeerIds);
    } catch (error) {
      // Leave nothing half-registered so a later retry starts clean.
      disposeReceiveListeners();
      await teardownReceiveBridge(true).catch(() => {});
      transportStatus.value = { ...transportStatus.value, state: "idle" };
      throw error;
    }
  }

  async function startReceivingInner(getPeerIds: () => string[]) {
    await ensurePlaybackContext();
    bridgeFallbackUsed = false;
    await teardownReceiveBridge(false);
    await setupReceiveBridge(false);

    const { listen } = await import("@tauri-apps/api/event");
    unlistenDisconnect = await listen<{ peer_id: string }>("peer-disconnected", (event) => {
      const pid = typeof event.payload === "string" ? event.payload : event.payload?.peer_id;
      if (!pid) return;
      const state = peerMediaStates.get(pid);
      if (state) releasePeerAudio(state);
      forgetPeerVideoDecoderState(pid);
      peerMediaStates.delete(pid);
      initialKeyframeRequests.delete(pid);
      if (activeSpeaker.value === pid) activeSpeaker.value = null;
      updateSpeakingMap();
    });

    // Call-size profile: rebuild the encoder at the new base; the backend
    // controller applies its congestion level on top.
    unlistenQuality = await listen<{
      peer_count: number;
      bitrate_bps: number;
      fps: number;
      max_width: number;
      max_height: number;
    }>("quality-profile-changed", async (event) => {
      const { bitrate_bps, fps, max_width, max_height } = event.payload;
      baseProfile = {
        bitrateBps: bitrate_bps,
        fps: isAndroid ? Math.min(fps, DEFAULT_PROFILE.fps) : fps,
        maxWidth: max_width,
        maxHeight: max_height,
      };
      await applyCaptureProfile({ rebuildEncoder: true });
    });

    // Single source of truth for outbound quality: the backend's controller
    // (driven by skipped frames, loss and queueing delay — not raw RTT).
    unlistenSendQuality = await listen<{ level: number }>("send-quality-changed", async (event) => {
      sendLevel = event.payload.level;
      connectionQuality.value = sendQualityForLevel(sendLevel);
      await applyCaptureProfile({ rebuildEncoder: false });
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
    videoFrameBufferPool = null; // Will be lazily recreated with correct dimensions
    clearCaptureSurface();
    if (webEncoder) return; // reconfigured on the next captured frame
    const invoke = await invokePromise;
    // Keeps the encoder's bitrate/fps profile; only the resolution changes.
    await invoke("reinit_video_encoder", { width, height }).catch((e) => {
      console.warn("[transport] reinit_video_encoder failed:", e);
    });
  }

  // Point capture (and the encoder, when the base profile changed) at
  // base profile + controller level.
  async function applyCaptureProfile({ rebuildEncoder }: { rebuildEncoder: boolean }) {
    const profile = effectiveProfile();
    targetFps = profile.fps;
    const { width, height } = resolveCaptureDimensions(activeCaptureStream, profile);
    if (!rebuildEncoder || webEncoder) {
      await updateCaptureDimensions(width, height);
      return;
    }
    currentWidth = width;
    currentHeight = height;
    videoFrameBufferPool = null;
    clearCaptureSurface();
    const invoke = await invokePromise;
    await invoke("reinit_video_encoder_with_config", {
      width,
      height,
      bitrateBps: baseProfile.bitrateBps,
      fps: baseProfile.fps,
    }).catch((e) => {
      console.warn("[transport] reinit_video_encoder_with_config failed:", e);
    });
  }

  function teardownCapture() {
    encoding.value = false;
    closeWebEncoder();
    videoSendInFlight = false;
    audioSendsPending = 0;
    audioSendChain = Promise.resolve();
    if (captureRafId !== null) {
      cancelAnimationFrame(captureRafId);
      captureRafId = null;
    }
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

    for (const [, state] of peerMediaStates) releasePeerAudio(state);
    if (playbackCtx) {
      await playbackCtx.close().catch(() => {});
      playbackCtx = null;
      // The worklet module belongs to the closed context.
      playbackWorkletLoaded = null;
    }
    if (activeSpeakerInterval) {
      clearInterval(activeSpeakerInterval);
      activeSpeakerInterval = null;
    }
    disposeReceiveListeners();

    for (const peerId of peerVideoDecoders.keys()) {
      destroyVideoDecoder(peerId);
    }
    peerDecoderErrorTimestamps.clear();
    peerDecoderCooldownUntil.clear();

    // Release the Rust-side per-peer Opus/H.264 codec state too; the frontend
    // decoders above are separate, and without this the backend maps leak
    // across calls.
    try {
      const invoke = await invokePromise;
      await invoke("destroy_codecs");
    } catch (e) {
      console.warn("[transport] destroy_codecs failed:", e);
    }

    peerMediaStates.clear();
    initialKeyframeRequests.clear();
    activeSpeaker.value = null;
    peerSpeakingMap.value = {};
    peerIdsProvider = null;

    connectionQuality.value = "good";
    baseProfile = { ...DEFAULT_PROFILE };
    sendLevel = 0;
    targetFps = DEFAULT_PROFILE.fps;
    webEncoderFailed = false;
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
