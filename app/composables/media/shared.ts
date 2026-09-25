// State and helpers shared by the media transport modules (audio playback,
// video receive, receive bridge, capture). useMediaTransport orchestrates them.

export const corePromise = import("@tauri-apps/api/core");
export const invokePromise = corePromise.then((m) => m.invoke);

export const isAndroid = /android/i.test(navigator.userAgent);

const KEYFRAME_REQUEST_DEBOUNCE_MS = 300;
const AUDIO_CONTEXT_RESUME_TIMEOUT_MS = 1500;

// ── Run tokens ──────────────────────────────────────────────────────
// Setup is a chain of awaits that stop() can land in the middle of. Each run
// captures the token it started with and bails out after every await once it
// changed, so a stale setup can't resume and re-register bridges, listeners
// or intervals after teardown. Receive and capture have separate tokens:
// restartSending() tears down capture only and must not cancel receiving.
let receiveRunToken = 0; // bumped by stop()
let captureRunToken = 0; // bumped by teardownCapture() (and so by stop())

export function currentReceiveRun() {
  return receiveRunToken;
}

export function bumpReceiveRun() {
  receiveRunToken++;
}

export function currentCaptureRun() {
  return captureRunToken;
}

export function bumpCaptureRun() {
  captureRunToken++;
}

export class TransportStoppedError extends Error {
  constructor() {
    super("stopped");
  }
}

export function ensureReceiveRun(token: number) {
  if (token !== receiveRunToken) throw new TransportStoppedError();
}

export function ensureCaptureRun(token: number) {
  if (token !== captureRunToken) throw new TransportStoppedError();
}

// listen() for a receive run: a listener that resolves after stop() is
// unregistered on the spot instead of leaking into the next call.
export async function listenForRun<T>(token: number, event: string, handler: (event: { payload: T }) => void) {
  const { listen } = await import("@tauri-apps/api/event");
  const unlisten = await listen<T>(event, handler);
  if (token !== receiveRunToken) {
    unlisten();
    throw new TransportStoppedError();
  }
  return unlisten;
}

// ── Per-peer receive state ──────────────────────────────────────────

export interface PendingVideoFrame {
  jpegBytes: Uint8Array;
  width: number;
  height: number;
  timestamp: number;
}

export interface PeerMediaState {
  canvas: HTMLCanvasElement | null;
  /** Playback worklet holding this peer's jitter buffer. */
  audioNode: AudioWorkletNode | null;
  audioGainNode: GainNode | null;
  speaking: boolean;
  lastAudioRms: number;
  speakingSince: number;
  lastSpeakingTime: number;
  /** When the last audio packet arrived; silence decays speaking state. */
  lastAudioAt: number;
  lastKeyframeRequestAt: number;
  pendingVideoFrame: PendingVideoFrame | null;
  /** A JPEG frame is being decoded/drawn; newer ones wait in pendingVideoFrame. */
  jpegRendering: boolean;
  lastDrawnTimestamp: number;
  videoPaused: boolean;
}

export const peerMediaStates = new Map<string, PeerMediaState>();
let peerIdsProvider: (() => string[]) | null = null;

export function setPeerIdsProvider(provider: (() => string[]) | null) {
  peerIdsProvider = provider;
}

export function getOrCreatePeerState(peerId: string): PeerMediaState {
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
      lastAudioAt: 0,
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

// For paths driven by packets or timers rather than the peer's tile: a peer
// that already left (late audio packet, keyframe retry) must not be
// resurrected with fresh state and an orphan AudioWorkletNode.
export function peerStateIfInCall(peerId: string): PeerMediaState | null {
  const existing = peerMediaStates.get(peerId);
  if (existing) return existing;
  if (!peerIdsProvider?.().includes(peerId)) return null;
  return getOrCreatePeerState(peerId);
}

async function sendControl(peerId: string, action: Record<string, unknown>) {
  const invoke = await invokePromise;
  await invoke("send_control", { peerId, action });
}

export async function requestKeyframe(peerId: string, force = false) {
  const peerState = peerStateIfInCall(peerId);
  if (!peerState) return;
  const now = Date.now();
  if (!force && now - peerState.lastKeyframeRequestAt < KEYFRAME_REQUEST_DEBOUNCE_MS) return;
  peerState.lastKeyframeRequestAt = now;
  sendControl(peerId, { action: "keyframe_request", layer: "high" }).catch(() => {});
}

// ── AudioContext helpers ────────────────────────────────────────────

// AudioContext.resume() can stay pending forever without a user gesture
// (WebKit, Android WebView) — never let it block call setup.
export async function resumeWithTimeout(ctx: AudioContext) {
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
export function keepContextRunning(ctx: AudioContext, label: string) {
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

// ── Byte helpers ────────────────────────────────────────────────────

export const sharedTextDecoder = new TextDecoder();

export function toBase64(bytes: Uint8Array): string {
  let binary = "";
  const chunk = 8192;
  for (let i = 0; i < bytes.length; i += chunk) {
    binary += String.fromCharCode.apply(null, bytes.subarray(i, i + chunk) as unknown as number[]);
  }
  return btoa(binary);
}

export function fromBase64(b64: string): Uint8Array {
  const binary = atob(b64);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) {
    bytes[i] = binary.charCodeAt(i);
  }
  return bytes;
}

// Tauri Channel<Vec<u8>> serializes as JSON array, not ArrayBuffer.
export function toArrayBuffer(data: unknown): ArrayBuffer {
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
