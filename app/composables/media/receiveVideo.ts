import {
  getOrCreatePeerState,
  peerMediaStates,
  peerStateIfInCall,
  requestKeyframe,
  type PendingVideoFrame,
} from "./shared";

// Remote video: raw H.264 decoded in the WebView (WebCodecs) where supported,
// otherwise JPEG frames decoded by the backend, drawn to each peer's canvas.

// Constrained Baseline, level 3.1: covers openh264's output and the Android
// WebView encoder (which declares 3.1).
const DECODER_CODEC = "avc1.42E01F";
// A VideoDecoder global doesn't guarantee H.264 support (e.g. Linux WebKitGTK
// without the codec plugins); probe once and reuse the answer.
let webCodecsDecodeSupport: Promise<boolean> | null = null;
let webCodecsDisabled = false;
export function supportsWebCodecsDecode(): Promise<boolean> {
  if (webCodecsDisabled) return Promise.resolve(false);
  if (!webCodecsDecodeSupport) {
    webCodecsDecodeSupport = typeof VideoDecoder === "undefined"
      ? Promise.resolve(false)
      : VideoDecoder.isConfigSupported({ codec: DECODER_CODEC })
        .then((support) => support.supported === true, () => false);
  }
  return webCodecsDecodeSupport;
}

const peerVideoDecoders = new Map<string, VideoDecoder>();
// Peers whose decoder hasn't yet seen its first keyframe. Feeding a fresh H.264
// decoder a delta frame before an IDR produces a decode error and a black
// canvas, so we drop deltas until the keyframe (requested when the peer's
// canvas mounts) arrives.
const peersAwaitingKeyframe = new Set<string>();

// Per-peer decode error tracking so a persistently broken stream (e.g. a
// corrupt or unsupported bitstream) doesn't spin the CPU recreating and
// immediately re-erroring the decoder on every incoming frame.
const peerDecoderErrorTimestamps = new Map<string, number[]>();
const peerDecoderCooldownUntil = new Map<string, number>();
const DECODER_ERROR_STORM_LIMIT = 3;
const DECODER_ERROR_STORM_WINDOW_MS = 10_000;
const DECODER_ERROR_COOLDOWN_MS = 10_000;
// isConfigSupported can say yes while real decoding keeps failing (WebKitGTK
// without codec plugins, flaky MediaCodec). After this many error storms —
// or an outright NotSupportedError — stop using WebCodecs for the session
// and have the backend send decoded JPEG frames instead.
const MAX_ERROR_STORMS_BEFORE_FALLBACK = 2;
let errorStorms = 0;
let onWebCodecsUnusable: (() => void) | null = null;

export function setWebCodecsFallbackHandler(handler: (() => void) | null) {
  onWebCodecsUnusable = handler;
}

function disableWebCodecs(reason: string) {
  if (webCodecsDisabled) return;
  webCodecsDisabled = true;
  console.warn(`[transport] WebCodecs decoding unusable (${reason}); falling back to JPEG frames`);
  destroyAllVideoDecoders();
  onWebCodecsUnusable?.();
}

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
function recoverVideoDecoder(peerId: string, reason: string, notSupported = false) {
  if (notSupported) {
    disableWebCodecs(reason);
    return;
  }
  destroyVideoDecoder(peerId);
  peersAwaitingKeyframe.add(peerId);

  const now = Date.now();
  const recent = (peerDecoderErrorTimestamps.get(peerId) ?? []).filter(
    (t) => now - t < DECODER_ERROR_STORM_WINDOW_MS,
  );
  recent.push(now);
  peerDecoderErrorTimestamps.set(peerId, recent);

  if (recent.length > DECODER_ERROR_STORM_LIMIT) {
    errorStorms += 1;
    if (errorStorms >= MAX_ERROR_STORMS_BEFORE_FALLBACK) {
      disableWebCodecs(reason);
      return;
    }
    peerDecoderErrorTimestamps.delete(peerId);
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
      recoverVideoDecoder(peerId, `VideoDecoder error: ${e.message}`, e.name === "NotSupportedError");
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

export function forgetPeerVideoDecoderState(peerId: string) {
  destroyVideoDecoder(peerId);
  peerDecoderErrorTimestamps.delete(peerId);
  peerDecoderCooldownUntil.delete(peerId);
}

/** The chain for this peer is broken (e.g. its tile was paused): wait for,
 *  and ask for, a fresh keyframe before decoding again. */
export function resyncPeerVideo(peerId: string) {
  if (peerVideoDecoders.has(peerId)) peersAwaitingKeyframe.add(peerId);
  requestKeyframe(peerId, true).catch(() => {});
}

export function resetWebCodecsFallback() {
  webCodecsDisabled = false;
  errorStorms = 0;
}

export function destroyAllVideoDecoders() {
  for (const peerId of peerVideoDecoders.keys()) {
    destroyVideoDecoder(peerId);
  }
  peerDecoderErrorTimestamps.clear();
  peerDecoderCooldownUntil.clear();
}

export function handleRawNalu(peerId: string, timestamp: number, isKeyframe: boolean, h264Data: Uint8Array) {
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
    // Still waiting for an IDR — dropping this delta avoids a decoder error.
    // Keep asking (debounced): the keyframe requested when the canvas
    // mounted may have arrived before this bridge was registered.
    requestKeyframe(peerId).catch(() => {});
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

export async function handleIncomingVideoFrame(
  peerId: string,
  timestamp: number,
  width: number,
  height: number,
  jpegBytes: Uint8Array,
) {
  const peerState = peerStateIfInCall(peerId);
  if (!peerState) return;
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
      }
      next = peerState.pendingVideoFrame;
    }
  } catch (error) {
    console.warn("[transport] Video frame render failed:", error);
  } finally {
    peerState.jpegRendering = false;
  }
}

export function registerPeerCanvas(peerId: string, canvas: HTMLCanvasElement | null) {
  // Function refs fire on every re-render of the tile, not just on mount —
  // only a real change may cost a decoder reset and a keyframe request.
  // A tile unmounting after its peer left must not recreate that peer's
  // state; a mounted tile always belongs to a peer in the call.
  const peerState = canvas ? getOrCreatePeerState(peerId) : peerMediaStates.get(peerId);
  if (!peerState || peerState.canvas === canvas) return;
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
    // A new call connection already starts with a keyframe (the sender's
    // writer and our reorder buffer both wait for one), but a canvas that
    // mounts later, or is replaced, needs a fresh one. Bypass the debounce:
    // a request moments ago (e.g. decoder recovery) would otherwise
    // suppress this one and leave the canvas black until the next IDR.
    peersAwaitingKeyframe.add(peerId);
    requestKeyframe(peerId, true).catch(() => {});
  }
}
