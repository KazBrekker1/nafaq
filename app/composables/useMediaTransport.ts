// Media transport: WebCodecs per-frame encode/decode pipeline.
// Audio: AudioWorklet → AudioEncoder (Opus or raw PCM fallback) → invoke("send_audio")
// Video: Canvas capture at 15fps → VideoEncoder (VP8) → invoke("send_video")
// Receive: decode → AudioContext scheduled playback / Canvas drawImage

const encoding = ref(false);
const OPUS_CONFIG = { codec: "opus", sampleRate: 48000, numberOfChannels: 1, bitrate: 32000 } as const;

let audioEncoder: AudioEncoder | null = null;
let videoEncoder: VideoEncoder | null = null;
let audioDecoder: AudioDecoder | null = null;
let videoDecoder: VideoDecoder | null = null;
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
let useRawPcm = false;

export function useMediaTransport() {
  async function startSending(stream: MediaStream, peerId: string) {
    if (encoding.value) return;
    encoding.value = true;
    useRawPcm = false;

    const { invoke } = await import("@tauri-apps/api/core");

    // --- Audio ---
    const audioTrack = stream.getAudioTracks()[0];
    if (audioTrack) {
      let codecSupported = false;
      try {
        const support = await AudioEncoder.isConfigSupported(OPUS_CONFIG);
        codecSupported = support.supported === true;
      } catch (e) {
        console.warn("[audio-enc] Opus probe failed:", e);
      }

      if (codecSupported) {
        audioEncoder = new AudioEncoder({
          output: (chunk: EncodedAudioChunk) => {
            const buf = new Uint8Array(chunk.byteLength);
            chunk.copyTo(buf);
            invoke("send_audio", { peerId, data: buf }).catch(() => {});
          },
          error: (e) => console.error("[audio-enc]", e),
        });
        audioEncoder.configure(OPUS_CONFIG);
      } else {
        useRawPcm = true;
      }

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
      const blobUrl = URL.createObjectURL(new Blob([WORKLET_CODE], { type: "application/javascript" }));
      await captureCtx.audioWorklet.addModule(blobUrl);
      URL.revokeObjectURL(blobUrl);

      sourceNode = captureCtx.createMediaStreamSource(new MediaStream([audioTrack]));
      workletNode = new AudioWorkletNode(captureCtx, "capture");

      let frameCount = 0;
      workletNode.port.onmessage = (event) => {
        const { samples } = event.data as { samples: Float32Array };
        if (useRawPcm) {
          const pcm = new Int16Array(samples.length);
          for (let i = 0; i < samples.length; i++) {
            pcm[i] = Math.max(-32768, Math.min(32767, Math.round(samples[i] * 32767)));
          }
          invoke("send_audio", { peerId, data: new Uint8Array(pcm.buffer) }).catch(() => {});
        } else if (audioEncoder?.state === "configured") {
          const data = new Float32Array(samples);
          const audioData = new AudioData({
            format: "f32-planar" as AudioSampleFormat,
            sampleRate: 48000,
            numberOfFrames: samples.length,
            numberOfChannels: 1,
            timestamp: frameCount * (samples.length / 48000) * 1_000_000,
            data,
          });
          frameCount++;
          audioEncoder.encode(audioData);
          audioData.close();
        }
      };

      sourceNode.connect(workletNode);
      workletNode.connect(captureCtx.destination);
    }

    // --- Video ---
    const videoTrack = stream.getVideoTracks()[0];
    if (videoTrack) {
      const settings = videoTrack.getSettings();
      const width = Math.min(settings.width || 640, 640);
      const height = Math.min(settings.height || 480, 480);

      videoEncoder = new VideoEncoder({
        output: (chunk: EncodedVideoChunk) => {
          const buf = new Uint8Array(chunk.byteLength);
          chunk.copyTo(buf);
          invoke("send_video", { peerId, data: buf }).catch(() => {});
        },
        error: (e) => console.error("[video-enc]", e),
      });
      videoEncoder.configure({ codec: "vp8", width, height, bitrate: 500_000, framerate: 15 });

      if (captureVideoEl) { captureVideoEl.pause(); captureVideoEl.srcObject = null; }
      captureVideoEl = document.createElement("video");
      captureVideoEl.srcObject = stream;
      captureVideoEl.muted = true;
      captureVideoEl.play();

      const canvas = new OffscreenCanvas(width, height);
      const ctx = canvas.getContext("2d")!;
      let vFrameCount = 0;

      captureInterval = setInterval(() => {
        if (!videoEncoder || videoEncoder.state !== "configured") return;
        if (!captureVideoEl || captureVideoEl.readyState < 2) return;
        ctx.drawImage(captureVideoEl, 0, 0, width, height);
        const frame = new VideoFrame(canvas, { timestamp: vFrameCount * (1_000_000 / 15) });
        const isKeyFrame = vFrameCount === 0 || vFrameCount % 30 === 0;
        vFrameCount++;
        videoEncoder.encode(frame, { keyFrame: isKeyFrame });
        frame.close();
      }, 1000 / 15);
    }
  }

  async function startReceiving(canvas: HTMLCanvasElement) {
    remoteCanvas = canvas;
    const { listen } = await import("@tauri-apps/api/event");

    // Audio decoder + playback
    playbackCtx = new AudioContext({ sampleRate: 48000 });
    nextPlayTime = playbackCtx.currentTime;

    if (!useRawPcm) {
      audioDecoder = new AudioDecoder({
        output: (audioData: AudioData) => {
          if (!playbackCtx) return;
          const buffer = playbackCtx.createBuffer(1, audioData.numberOfFrames, 48000);
          const ch = new Float32Array(audioData.numberOfFrames);
          audioData.copyTo(ch, { planeIndex: 0 });
          buffer.copyToChannel(ch, 0);
          audioData.close();
          scheduleAudioBuffer(buffer);
        },
        error: (e) => console.error("[audio-dec]", e),
      });
      audioDecoder.configure(OPUS_CONFIG);
    }

    // Video decoder — cache 2d context to avoid lookup per frame
    let canvasCtx: CanvasRenderingContext2D | null = null;

    videoDecoder = new VideoDecoder({
      output: (frame: VideoFrame) => {
        if (remoteCanvas) {
          if (!canvasCtx) canvasCtx = remoteCanvas.getContext("2d");
          if (canvasCtx) {
            if (remoteCanvas.width !== frame.displayWidth) remoteCanvas.width = frame.displayWidth;
            if (remoteCanvas.height !== frame.displayHeight) remoteCanvas.height = frame.displayHeight;
            canvasCtx.drawImage(frame, 0, 0);
          }
        }
        frame.close();
      },
      error: (e) => console.error("[video-dec]", e),
    });
    videoDecoder.configure({ codec: "vp8" });

    // Event listeners — payload is { stream_type, data: number[], timestamp }
    type MediaPayload = { stream_type: number; data: number[]; timestamp: number };

    unlistenAudio = await listen<MediaPayload>("audio-received", (event) => {
      const bytes = new Uint8Array(event.payload.data);
      if (useRawPcm && playbackCtx) {
        const int16 = new Int16Array(bytes.buffer, bytes.byteOffset, bytes.byteLength / 2);
        const buffer = playbackCtx.createBuffer(1, int16.length, 48000);
        const ch = buffer.getChannelData(0);
        for (let i = 0; i < int16.length; i++) ch[i] = int16[i] / 32768;
        scheduleAudioBuffer(buffer);
      } else if (audioDecoder?.state === "configured") {
        audioDecoder.decode(new EncodedAudioChunk({
          type: "key",
          timestamp: event.payload.timestamp * 1000,
          data: bytes,
        }));
      }
    });

    unlistenVideo = await listen<MediaPayload>("video-received", (event) => {
      const bytes = new Uint8Array(event.payload.data);
      if (videoDecoder?.state === "configured" && bytes.length > 0) {
        const isKey = (bytes[0] & 0x01) === 0;
        videoDecoder.decode(new EncodedVideoChunk({
          type: isKey ? "key" : "delta",
          timestamp: event.payload.timestamp * 1000,
          data: bytes,
        }));
      }
    });
  }

  function stop() {
    if (audioEncoder?.state !== "closed") try { audioEncoder?.close(); } catch {}
    if (videoEncoder?.state !== "closed") try { videoEncoder?.close(); } catch {}
    audioEncoder = null;
    videoEncoder = null;
    encoding.value = false;

    if (workletNode) { workletNode.port.onmessage = null; workletNode.disconnect(); workletNode = null; }
    if (sourceNode) { sourceNode.disconnect(); sourceNode = null; }
    if (captureCtx) { captureCtx.close(); captureCtx = null; }
    if (captureVideoEl) { captureVideoEl.pause(); captureVideoEl.srcObject = null; captureVideoEl = null; }
    if (captureInterval) { clearInterval(captureInterval); captureInterval = null; }

    if (audioDecoder?.state !== "closed") try { audioDecoder?.close(); } catch {}
    if (videoDecoder?.state !== "closed") try { videoDecoder?.close(); } catch {}
    audioDecoder = null;
    videoDecoder = null;
    if (playbackCtx) { playbackCtx.close(); playbackCtx = null; }

    unlistenAudio?.(); unlistenVideo?.();
    unlistenAudio = null; unlistenVideo = null;
    remoteCanvas = null;
    useRawPcm = false;
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
