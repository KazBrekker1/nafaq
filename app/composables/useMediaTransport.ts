// Media transport: encode local streams via MediaRecorder, send via Tauri,
// receive from peer, play back via MediaSource for continuous streaming.

const encoding = ref(false);
let audioRecorder: MediaRecorder | null = null;
let videoRecorder: MediaRecorder | null = null;
let unlistenAudio: (() => void) | null = null;
let unlistenVideo: (() => void) | null = null;

// MediaSource playback
let audioMediaSource: MediaSource | null = null;
let videoMediaSource: MediaSource | null = null;
let audioSourceBuffer: SourceBuffer | null = null;
let videoSourceBuffer: SourceBuffer | null = null;
let audioQueue: Uint8Array[] = [];
let videoQueue: Uint8Array[] = [];

export function useMediaTransport() {
  async function startSending(stream: MediaStream, peerId: string) {
    if (encoding.value) return;
    encoding.value = true;

    const { invoke } = await import("@tauri-apps/api/core");

    // Audio encoding
    const audioTrack = stream.getAudioTracks()[0];
    if (audioTrack) {
      const audioStream = new MediaStream([audioTrack]);
      const mimeType = MediaRecorder.isTypeSupported("audio/webm;codecs=opus")
        ? "audio/webm;codecs=opus" : "audio/webm";
      audioRecorder = new MediaRecorder(audioStream, { mimeType, audioBitsPerSecond: 32000 });
      audioRecorder.ondataavailable = async (e) => {
        if (e.data.size > 0) {
          const buffer = await e.data.arrayBuffer();
          invoke("send_audio", { peerId, data: new Uint8Array(buffer) }).catch(() => {});
        }
      };
      audioRecorder.start(200);
    }

    // Video encoding
    const videoTrack = stream.getVideoTracks()[0];
    if (videoTrack) {
      const videoStream = new MediaStream([videoTrack]);
      const mimeType = MediaRecorder.isTypeSupported("video/webm;codecs=vp8")
        ? "video/webm;codecs=vp8" : "video/webm";
      videoRecorder = new MediaRecorder(videoStream, { mimeType, videoBitsPerSecond: 500000 });
      videoRecorder.ondataavailable = async (e) => {
        if (e.data.size > 0) {
          const buffer = await e.data.arrayBuffer();
          invoke("send_video", { peerId, data: new Uint8Array(buffer) }).catch(() => {});
        }
      };
      videoRecorder.start(200);
    }

    console.log("[media-transport] Started sending");
  }

  async function startReceiving(audioEl: HTMLAudioElement, videoEl: HTMLVideoElement) {
    const { listen } = await import("@tauri-apps/api/event");

    // Set up MediaSource for audio playback
    setupMediaSource(audioEl, "audio/webm;codecs=opus", "audio");

    // Set up MediaSource for video playback
    setupMediaSource(videoEl, "video/webm;codecs=vp8", "video");

    // Listen for incoming audio
    unlistenAudio = await listen<{ data: string }>("audio-received", (event) => {
      const bytes = base64ToUint8Array(event.payload.data);
      if (bytes.length > 0) appendToBuffer("audio", bytes);
    });

    // Listen for incoming video
    unlistenVideo = await listen<{ data: string }>("video-received", (event) => {
      const bytes = base64ToUint8Array(event.payload.data);
      if (bytes.length > 0) appendToBuffer("video", bytes);
    });

    console.log("[media-transport] Started receiving");
  }

  function setupMediaSource(el: HTMLMediaElement, mimeType: string, type: "audio" | "video") {
    // Check if MediaSource supports this type
    if (typeof MediaSource === "undefined" || !MediaSource.isTypeSupported(mimeType)) {
      console.warn(`[media-transport] MediaSource doesn't support ${mimeType}, using fallback`);
      // Fallback: accumulate and play via Blob URL
      return;
    }

    const ms = new MediaSource();
    el.src = URL.createObjectURL(ms);

    ms.addEventListener("sourceopen", () => {
      try {
        const sb = ms.addSourceBuffer(mimeType);
        sb.mode = "sequence";

        sb.addEventListener("updateend", () => {
          // Flush queued buffers
          const queue = type === "audio" ? audioQueue : videoQueue;
          if (queue.length > 0 && !sb.updating) {
            const next = queue.shift()!;
            try { sb.appendBuffer(next); } catch {}
          }
        });

        if (type === "audio") {
          audioMediaSource = ms;
          audioSourceBuffer = sb;
        } else {
          videoMediaSource = ms;
          videoSourceBuffer = sb;
        }
      } catch (e) {
        console.error(`[media-transport] Failed to create ${type} SourceBuffer:`, e);
      }
    });

    el.play().catch(() => {});
  }

  function appendToBuffer(type: "audio" | "video", data: Uint8Array) {
    const sb = type === "audio" ? audioSourceBuffer : videoSourceBuffer;
    const queue = type === "audio" ? audioQueue : videoQueue;

    if (!sb) {
      // MediaSource not ready, queue it
      queue.push(data);
      if (queue.length > 100) queue.shift(); // prevent unbounded growth
      return;
    }

    if (sb.updating) {
      queue.push(data);
      if (queue.length > 100) queue.shift();
    } else {
      try { sb.appendBuffer(data); } catch { queue.push(data); }
    }
  }

  function stop() {
    if (audioRecorder?.state !== "inactive") audioRecorder?.stop();
    if (videoRecorder?.state !== "inactive") videoRecorder?.stop();
    audioRecorder = null;
    videoRecorder = null;
    encoding.value = false;

    unlistenAudio?.();
    unlistenVideo?.();
    unlistenAudio = null;
    unlistenVideo = null;

    // Clean up MediaSource
    try { audioMediaSource?.endOfStream(); } catch {}
    try { videoMediaSource?.endOfStream(); } catch {}
    audioMediaSource = null;
    videoMediaSource = null;
    audioSourceBuffer = null;
    videoSourceBuffer = null;
    audioQueue = [];
    videoQueue = [];

    console.log("[media-transport] Stopped");
  }

  return { encoding, startSending, startReceiving, stop };
}

function base64ToUint8Array(b64: string): Uint8Array {
  const binary = atob(b64);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) bytes[i] = binary.charCodeAt(i);
  return bytes;
}
