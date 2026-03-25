// Media transport: encode local streams, send via Tauri, receive and play remote streams.
// Uses MediaRecorder for encoding (works in WebKit) and Blob URLs for playback.

const encoding = ref(false);
let audioRecorder: MediaRecorder | null = null;
let videoRecorder: MediaRecorder | null = null;
let unlistenAudio: (() => void) | null = null;
let unlistenVideo: (() => void) | null = null;

// Remote playback elements (set by call page)
let remoteAudioEl: HTMLAudioElement | null = null;
let remoteVideoEl: HTMLVideoElement | null = null;

// Accumulate received chunks for playback
let audioChunks: Blob[] = [];
let videoChunks: Blob[] = [];
let audioPlaybackTimer: ReturnType<typeof setInterval> | null = null;
let videoPlaybackTimer: ReturnType<typeof setInterval> | null = null;

export function useMediaTransport() {
  async function startSending(stream: MediaStream, peerId: string) {
    if (encoding.value) return;
    encoding.value = true;

    const { invoke } = await import("@tauri-apps/api/core");

    // Audio encoding via MediaRecorder
    const audioTrack = stream.getAudioTracks()[0];
    if (audioTrack) {
      const audioStream = new MediaStream([audioTrack]);
      try {
        audioRecorder = new MediaRecorder(audioStream, {
          mimeType: "audio/webm;codecs=opus",
          audioBitsPerSecond: 32000,
        });
      } catch {
        // Fallback if opus not supported
        audioRecorder = new MediaRecorder(audioStream);
      }

      audioRecorder.ondataavailable = async (e) => {
        if (e.data.size > 0) {
          const buffer = await e.data.arrayBuffer();
          const data = Array.from(new Uint8Array(buffer));
          invoke("send_audio", { peerId, data }).catch(() => {});
        }
      };
      audioRecorder.start(100); // 100ms chunks
    }

    // Video encoding via MediaRecorder
    const videoTrack = stream.getVideoTracks()[0];
    if (videoTrack) {
      const videoStream = new MediaStream([videoTrack]);
      try {
        videoRecorder = new MediaRecorder(videoStream, {
          mimeType: "video/webm;codecs=vp8",
          videoBitsPerSecond: 500000,
        });
      } catch {
        videoRecorder = new MediaRecorder(videoStream);
      }

      videoRecorder.ondataavailable = async (e) => {
        if (e.data.size > 0) {
          const buffer = await e.data.arrayBuffer();
          const data = Array.from(new Uint8Array(buffer));
          invoke("send_video", { peerId, data }).catch(() => {});
        }
      };
      videoRecorder.start(100); // 100ms chunks
    }

    console.log("[media-transport] Started sending");
  }

  async function startReceiving(audioEl: HTMLAudioElement, videoEl: HTMLVideoElement) {
    remoteAudioEl = audioEl;
    remoteVideoEl = videoEl;

    const { listen } = await import("@tauri-apps/api/event");

    // Listen for incoming audio
    unlistenAudio = await listen<{ data: string; timestamp: number }>("audio-received", (event) => {
      const binary = atob(event.payload.data);
      const bytes = new Uint8Array(binary.length);
      for (let i = 0; i < binary.length; i++) bytes[i] = binary.charCodeAt(i);
      audioChunks.push(new Blob([bytes], { type: "audio/webm;codecs=opus" }));
    });

    // Listen for incoming video
    unlistenVideo = await listen<{ data: string; timestamp: number }>("video-received", (event) => {
      const binary = atob(event.payload.data);
      const bytes = new Uint8Array(binary.length);
      for (let i = 0; i < binary.length; i++) bytes[i] = binary.charCodeAt(i);
      videoChunks.push(new Blob([bytes], { type: "video/webm;codecs=vp8" }));
    });

    // Periodically flush accumulated chunks to playback elements via Blob URLs
    audioPlaybackTimer = setInterval(() => {
      if (audioChunks.length > 0 && remoteAudioEl) {
        const blob = new Blob(audioChunks, { type: "audio/webm;codecs=opus" });
        audioChunks = [];
        remoteAudioEl.src = URL.createObjectURL(blob);
        remoteAudioEl.play().catch(() => {});
      }
    }, 500);

    videoPlaybackTimer = setInterval(() => {
      if (videoChunks.length > 0 && remoteVideoEl) {
        const blob = new Blob(videoChunks, { type: "video/webm;codecs=vp8" });
        videoChunks = [];
        remoteVideoEl.src = URL.createObjectURL(blob);
        remoteVideoEl.play().catch(() => {});
      }
    }, 500);

    console.log("[media-transport] Started receiving");
  }

  function stop() {
    if (audioRecorder && audioRecorder.state !== "inactive") audioRecorder.stop();
    if (videoRecorder && videoRecorder.state !== "inactive") videoRecorder.stop();
    audioRecorder = null;
    videoRecorder = null;
    encoding.value = false;

    unlistenAudio?.();
    unlistenVideo?.();
    unlistenAudio = null;
    unlistenVideo = null;

    if (audioPlaybackTimer) { clearInterval(audioPlaybackTimer); audioPlaybackTimer = null; }
    if (videoPlaybackTimer) { clearInterval(videoPlaybackTimer); videoPlaybackTimer = null; }
    audioChunks = [];
    videoChunks = [];

    remoteAudioEl = null;
    remoteVideoEl = null;

    console.log("[media-transport] Stopped");
  }

  return { encoding, startSending, startReceiving, stop };
}
