import { ref } from "vue";

export function useVideoPipeline() {
  const encoding = ref(false);
  const decoding = ref(false);

  let encoder: VideoEncoder | null = null;
  let decoder: VideoDecoder | null = null;
  let captureInterval: ReturnType<typeof setInterval> | null = null;
  let onEncodedCallback:
    | ((data: Uint8Array, isKey: boolean) => void)
    | null = null;
  let onDecodedCallback: ((frame: VideoFrame) => void) | null = null;
  let frameCount = 0;
  const KEYFRAME_INTERVAL = 30;

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
      bitrate: 500_000,
      framerate: 15,
    });

    const videoEl = document.createElement("video");
    videoEl.srcObject = stream;
    videoEl.muted = true;
    videoEl.play();

    const captureCanvas = new OffscreenCanvas(width, height);
    const ctx = captureCanvas.getContext("2d")!;
    frameCount = 0;

    captureInterval = setInterval(() => {
      if (!encoder || encoder.state !== "configured") return;
      if (videoEl.readyState < 2) return;

      ctx.drawImage(videoEl, 0, 0, width, height);
      const frame = new VideoFrame(captureCanvas, {
        timestamp: frameCount * (1_000_000 / 15),
      });
      frameCount++;

      const isKeyFrame = frameCount % KEYFRAME_INTERVAL === 0;
      encoder.encode(frame, { keyFrame: isKeyFrame });
      frame.close();
    }, 1000 / 15);

    encoding.value = true;
    console.log(`[video-enc] Started encoding ${width}x${height}@15fps VP8`);
  }

  function stopEncoding() {
    if (captureInterval) {
      clearInterval(captureInterval);
      captureInterval = null;
    }
    if (encoder) {
      if (encoder.state !== "closed") encoder.close();
      encoder = null;
    }
    encoding.value = false;
  }

  function startDecoding(onDecoded: (frame: VideoFrame) => void) {
    onDecodedCallback = onDecoded;
    decoder = new VideoDecoder({
      output: (frame: VideoFrame) => {
        onDecodedCallback?.(frame);
      },
      error: (e) => console.error("[video-dec] Error:", e),
    });
    decoder.configure({ codec: "vp8" });
    decoding.value = true;
    console.log("[video-dec] Started decoding");
  }

  function decodeChunk(data: Uint8Array, timestamp: number, isKey: boolean) {
    if (!decoder || decoder.state !== "configured") return;
    const chunk = new EncodedVideoChunk({
      type: isKey ? "key" : "delta",
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
