// Media transport: raw frames → Rust codec pipeline.
// Audio: AudioWorklet → buffer 960 samples → Int16 PCM → invoke("send_audio") → Rust Opus encode
// Video: Canvas capture 15fps → getImageData RGBA → invoke("send_video") → Rust VP8 encode
// Receive: Rust decodes → PCM Int16 / JPEG → JS playback

const encoding = ref(false);
const OPUS_FRAME_SAMPLES = 960; // 20ms at 48kHz

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

export function useMediaTransport() {
  async function startSending(stream: MediaStream, peerId: string) {
    if (encoding.value) return;
    encoding.value = true;

    const { invoke } = await import("@tauri-apps/api/core");

    // Determine resolution — cap at 320x240 on mobile
    const isMobile = /android/i.test(navigator.userAgent);
    const videoTrack = stream.getVideoTracks()[0];
    const maxDim = isMobile ? 320 : 640;
    const width = videoTrack
      ? Math.min(videoTrack.getSettings().width || maxDim, maxDim)
      : maxDim;
    const height = videoTrack
      ? Math.min(
          videoTrack.getSettings().height || (isMobile ? 240 : 480),
          isMobile ? 240 : 480,
        )
      : isMobile
        ? 240
        : 480;

    // Initialize Rust codecs BEFORE audio or video setup — needed for both paths
    await invoke("init_codecs", { width, height });

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
            invoke("send_audio", {
              peerId,
              data: Array.from(new Uint8Array(pcm.buffer)),
            }).catch(() => {});
            bufferOffset = 0;
          }
        }
      };

      sourceNode.connect(workletNode);
      workletNode.connect(captureCtx.destination);
    }

    // --- Video ---
    if (videoTrack) {
      if (captureVideoEl) {
        captureVideoEl.pause();
        captureVideoEl.srcObject = null;
      }
      captureVideoEl = document.createElement("video");
      captureVideoEl.srcObject = stream;
      captureVideoEl.muted = true;
      captureVideoEl.play();

      const canvas = new OffscreenCanvas(width, height);
      const ctx = canvas.getContext("2d")!;
      let vFrameCount = 0;

      captureInterval = setInterval(() => {
        if (!captureVideoEl || captureVideoEl.readyState < 2) return;
        ctx.drawImage(captureVideoEl, 0, 0, width, height);
        const imageData = ctx.getImageData(0, 0, width, height);
        const keyframe = vFrameCount === 0 || vFrameCount % 30 === 0;
        vFrameCount++;
        invoke("send_video", {
          peerId,
          data: Array.from(new Uint8Array(imageData.data.buffer)),
          width,
          height,
          keyframe,
        }).catch(() => {});
      }, 1000 / 15);
    }
  }

  async function startReceiving(canvas: HTMLCanvasElement) {
    remoteCanvas = canvas;
    const { listen } = await import("@tauri-apps/api/event");

    playbackCtx = new AudioContext({ sampleRate: 48000 });
    nextPlayTime = playbackCtx.currentTime;

    // Cache canvas context
    let canvasCtx: CanvasRenderingContext2D | null = null;

    type AudioPayload = {
      stream_type: number;
      data: number[];
      timestamp: number;
    };
    type VideoPayload = {
      stream_type: number;
      data: number[];
      timestamp: number;
      width: number;
      height: number;
    };

    // Audio receive — PCM Int16 from Rust Opus decoder
    unlistenAudio = await listen<AudioPayload>("audio-received", (event) => {
      if (!playbackCtx) return;
      const bytes = new Uint8Array(event.payload.data);
      const int16 = new Int16Array(
        bytes.buffer,
        bytes.byteOffset,
        bytes.byteLength / 2,
      );
      const buffer = playbackCtx.createBuffer(1, int16.length, 48000);
      const ch = buffer.getChannelData(0);
      for (let i = 0; i < int16.length; i++) ch[i] = int16[i]! / 32768;
      scheduleAudioBuffer(buffer);
    });

    // Video receive — JPEG from Rust VP8 decoder
    unlistenVideo = await listen<VideoPayload>("video-received", (event) => {
      if (!remoteCanvas) return;
      const bytes = new Uint8Array(event.payload.data);
      const blob = new Blob([bytes], { type: "image/jpeg" });
      createImageBitmap(blob)
        .then((bitmap) => {
          if (!remoteCanvas) return;
          if (!canvasCtx) canvasCtx = remoteCanvas.getContext("2d");
          if (canvasCtx) {
            if (remoteCanvas.width !== bitmap.width)
              remoteCanvas.width = bitmap.width;
            if (remoteCanvas.height !== bitmap.height)
              remoteCanvas.height = bitmap.height;
            canvasCtx.drawImage(bitmap, 0, 0);
            bitmap.close();
          }
        })
        .catch(() => {});
    });
  }

  async function stop() {
    encoding.value = false;

    if (workletNode) {
      workletNode.port.onmessage = null;
      workletNode.disconnect();
      workletNode = null;
    }
    if (sourceNode) {
      sourceNode.disconnect();
      sourceNode = null;
    }
    if (captureCtx) {
      captureCtx.close();
      captureCtx = null;
    }
    if (captureVideoEl) {
      captureVideoEl.pause();
      captureVideoEl.srcObject = null;
      captureVideoEl = null;
    }
    if (captureInterval) {
      clearInterval(captureInterval);
      captureInterval = null;
    }
    if (playbackCtx) {
      playbackCtx.close();
      playbackCtx = null;
    }

    unlistenAudio?.();
    unlistenVideo?.();
    unlistenAudio = null;
    unlistenVideo = null;
    remoteCanvas = null;

    // Clean up Rust codec state
    try {
      const { invoke } = await import("@tauri-apps/api/core");
      await invoke("destroy_codecs");
    } catch {}
  }

  function scheduleAudioBuffer(buffer: AudioBuffer) {
    if (!playbackCtx) return;
    const source = playbackCtx.createBufferSource();
    source.buffer = buffer;
    source.connect(playbackCtx.destination);
    const now = playbackCtx.currentTime;
    if (nextPlayTime < now - 0.2) nextPlayTime = now;
    source.start(nextPlayTime);
    nextPlayTime += buffer.duration;
  }

  return { encoding, startSending, startReceiving, stop };
}
