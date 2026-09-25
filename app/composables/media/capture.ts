import { ref } from "vue";
import {
  DEFAULT_PROFILE,
  baseProfile,
  effectiveProfile,
  refreshBaseProfile,
  resolveCaptureDimensions,
} from "./profile";
import {
  bumpCaptureRun,
  currentCaptureRun,
  ensureCaptureRun,
  invokePromise,
  isAndroid,
  keepContextRunning,
  resumeWithTimeout,
  toBase64,
} from "./shared";

// Local capture: mic PCM through an AudioWorklet, camera frames drawn to a
// canvas on a rAF loop, uploaded to the backend encoders (or encoded to H.264
// in the WebView on Android).

export const encoding = ref(false);

interface MediaUploader {
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
}

const OPUS_FRAME_SAMPLES = 960;
const MAX_PENDING_AUDIO_SENDS = 5;

let captureCtx: AudioContext | null = null;
let captureVideoEl: HTMLVideoElement | null = null;
let captureRafId: number | null = null;
let captureCanvas: OffscreenCanvas | HTMLCanvasElement | null = null;
let captureCanvasCtx: OffscreenCanvasRenderingContext2D | CanvasRenderingContext2D | null = null;
let activeCaptureStream: MediaStream | null = null;
let workletNode: AudioWorkletNode | null = null;
let sourceNode: MediaStreamAudioSourceNode | null = null;
let captureSinkNode: GainNode | null = null;
let currentWidth = 640;
let currentHeight = 360;
let targetFps = DEFAULT_PROFILE.fps;
let videoSendInFlight = false;
let audioSendChain: Promise<void> = Promise.resolve();
let audioSendsPending = 0;
let mediaUploader: MediaUploader | null = null;
let preferJsonAudioInvoke = isAndroid;
let preferJsonVideoInvoke = isAndroid;
let loggedAudioInvokeFallback = false;
let loggedVideoInvokeFallback = false;

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

function createMediaUploader(
  invoke: typeof import("@tauri-apps/api/core").invoke,
): MediaUploader {
  return {
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

async function setupWebEncoder(token: number): Promise<boolean> {
  if (!isAndroid || webEncoderFailed || typeof VideoEncoder === "undefined") return false;
  if (webEncoder) return true;
  try {
    const config = webEncoderConfig();
    const support = await VideoEncoder.isConfigSupported(config);
    // Capture was torn down meanwhile — don't install an orphan encoder.
    if (token !== currentCaptureRun()) return false;
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

// ── Capture lifecycle ───────────────────────────────────────────────

export async function initCodecs(stream?: MediaStream | null) {
  const invoke = await invokePromise;
  refreshBaseProfile(); // settings may have changed since the last call
  const { width, height } = resolveCaptureDimensions(stream);
  currentWidth = width;
  currentHeight = height;
  targetFps = effectiveProfile().fps;
  // Encoder runs at the (user-capped) call-size profile; the backend's
  // quality controller lowers bitrate/fps from there at runtime.
  await invoke("init_codecs", {
    width,
    height,
    bitrateBps: baseProfile.bitrateBps,
    fps: baseProfile.fps,
  });
}

export async function startSending(stream: MediaStream) {
  if (encoding.value) return;
  encoding.value = true;
  activeCaptureStream = stream;
  const token = currentCaptureRun();

  try {
    await startSendingInner(stream, token);
  } catch (error) {
    // Torn down mid-setup (stop/restart): that teardown already released
    // this run's resources, and anything global now belongs to a newer run.
    if (token !== currentCaptureRun()) return;
    // Release everything the partial setup created (capture AudioContext,
    // off-screen video element, worklet) — otherwise a failed start leaks
    // them and blocks the next attempt.
    teardownCapture();
    throw error;
  }
}

async function startSendingInner(stream: MediaStream, token: number) {
  const invoke = await invokePromise;
  ensureCaptureRun(token);
  mediaUploader = createMediaUploader(invoke);

  const audioTrack = stream.getAudioTracks()[0];
  if (audioTrack) {
    captureCtx = new AudioContext({ sampleRate: 48000 });
    const resumeCaptureOnGesture = keepContextRunning(captureCtx, "capture");
    const resumed = await resumeWithTimeout(captureCtx);
    ensureCaptureRun(token);
    if (!resumed) {
      // Don't fail (or hang) the call: capture starts on the next gesture.
      console.warn("[transport] Microphone capture is waiting for user interaction");
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
    ensureCaptureRun(token);

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
          // Uploads are chained so frames reach the encoder in capture
          // order; if IPC falls behind, shed the newest instead of queueing
          // unbounded latency.
          const uploader = mediaUploader;
          if (uploader && audioSendsPending < MAX_PENDING_AUDIO_SENDS) {
            const pcm = new Int16Array(OPUS_FRAME_SAMPLES);
            for (let i = 0; i < OPUS_FRAME_SAMPLES; i++) {
              pcm[i] = Math.max(-32768, Math.min(32767, Math.round(sampleBuffer[i]! * 32767)));
            }
            const timestamp = Date.now();
            audioSendsPending++;
            audioSendChain = audioSendChain
              .then(() => uploader.sendAudio(new Uint8Array(pcm.buffer), timestamp))
              .catch(() => {})
              .finally(() => {
                audioSendsPending--;
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
      // A stale loop must not touch captureRafId — it belongs to the new run.
      if (token !== currentCaptureRun()) return;
      captureRafId = null;
      if (!encoding.value || !captureVideoEl) return;
      // Schedule first: a throw below (drawImage, VideoFrame, encode) must
      // not silently end capture for the rest of the call.
      captureRafId = requestAnimationFrame(rafCaptureLoop);

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
          const keyframe = frameCount % 48 === 0;
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
            // getImageData returns a fresh buffer each call — send it as is.
            const { data } = ctx.getImageData(0, 0, currentWidth, currentHeight);
            const rgba = new Uint8Array(data.buffer, data.byteOffset, data.byteLength);
            videoSendInFlight = true;
            mediaUploader
              .sendVideo(rgba, currentWidth, currentHeight, keyframe, Date.now())
              .catch(() => {})
              .finally(() => {
                videoSendInFlight = false;
              });
          }
        }
      }
    };

    await setupWebEncoder(token);
    ensureCaptureRun(token);

    const startCaptureLoop = () => {
      if (token !== currentCaptureRun() || captureLoopStarted || !captureVideoEl) return;
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

async function updateCaptureDimensions(width: number, height: number) {
  if (width === currentWidth && height === currentHeight) return;
  currentWidth = width;
  currentHeight = height;
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
export async function applyCaptureProfile({ rebuildEncoder }: { rebuildEncoder: boolean }) {
  const profile = effectiveProfile();
  targetFps = profile.fps;
  const { width, height } = resolveCaptureDimensions(activeCaptureStream, profile);
  if (!rebuildEncoder || webEncoder) {
    await updateCaptureDimensions(width, height);
    return;
  }
  currentWidth = width;
  currentHeight = height;
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

export function teardownCapture() {
  bumpCaptureRun();
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

export async function restartSending(getStream: () => MediaStream) {
  teardownCapture();
  const token = currentCaptureRun();
  await initCodecs(getStream());
  if (token !== currentCaptureRun()) return; // stopped meanwhile
  await startSending(getStream());
}

// Per-call capture settings back to defaults (after teardownCapture).
export function resetCaptureState() {
  targetFps = DEFAULT_PROFILE.fps;
  webEncoderFailed = false;
}
