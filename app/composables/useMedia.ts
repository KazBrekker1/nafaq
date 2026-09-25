export interface MediaDevice {
  deviceId: string;
  label: string;
}

const MEDIA_API_UNAVAILABLE = "Camera/mic API unavailable in this webview. On macOS, ensure the app has camera & microphone permissions.";

// Singleton state — shared across lobby and call pages
const localStream = ref<MediaStream | null>(null);
const cameras = ref<MediaDevice[]>([]);
const microphones = ref<MediaDevice[]>([]);
const selectedCamera = ref("");
const selectedMic = ref("");
const micLevel = ref(0);
const audioMuted = ref(false);
const videoMuted = ref(false);
const error = ref<string | null>(null);

let micLevelTimer: ReturnType<typeof setInterval> | null = null;
let audioContext: AudioContext | null = null;

// Invalidates in-flight getUserMedia attempts: a startPreview that resolves
// after a newer startPreview/stopPreview must discard (and stop) its stream,
// not leak a live camera/mic capture.
let previewGeneration = 0;

let prefsLoaded = false;

export function useMedia() {
  // Seed device selections from persisted settings on first use
  if (!prefsLoaded) {
    prefsLoaded = true;
    const { settings } = useSettings();
    if (settings.value.preferredCamera) selectedCamera.value = settings.value.preferredCamera;
    if (settings.value.preferredMic) selectedMic.value = settings.value.preferredMic;
  }

  async function enumerateDevices() {
    if (!navigator.mediaDevices?.enumerateDevices) {
      error.value = MEDIA_API_UNAVAILABLE;
      return;
    }
    try {
      const devices = await navigator.mediaDevices.enumerateDevices();
      cameras.value = devices
        .filter((d) => d.kind === "videoinput")
        .map((d) => ({ deviceId: d.deviceId, label: d.label || `Camera ${d.deviceId.slice(0, 8)}` }));
      microphones.value = devices
        .filter((d) => d.kind === "audioinput")
        .map((d) => ({ deviceId: d.deviceId, label: d.label || `Mic ${d.deviceId.slice(0, 8)}` }));
      const firstCamera = cameras.value[0];
      const firstMic = microphones.value[0];
      if (!selectedCamera.value && firstCamera) selectedCamera.value = firstCamera.deviceId;
      if (!selectedMic.value && firstMic) selectedMic.value = firstMic.deviceId;
    } catch (e: unknown) {
      error.value = `Device enumeration failed: ${e instanceof Error ? e.message : String(e)}`;
    }
  }

  async function startPreview() {
    error.value = null;

    if (!navigator.mediaDevices?.getUserMedia) {
      error.value = MEDIA_API_UNAVAILABLE;
      return;
    }

    const generation = ++previewGeneration;

    // Replacing a live stream (device switch): release the current camera
    // first — Android's camera HAL won't open a second one while it's held,
    // so asking for the new device would fail with NotReadableError.
    localStream.value?.getVideoTracks().forEach((t) => t.stop());

    // Capture is downscaled to at most 640x360 @ 12 fps before encoding;
    // asking for more only burns camera/CPU (notably on Android).
    const videoConstraint: MediaTrackConstraints = {
      width: { ideal: 640 },
      height: { ideal: 360 },
      frameRate: { ideal: 15, max: 30 },
      ...(selectedCamera.value ? { deviceId: { exact: selectedCamera.value } } : {}),
    };
    // Remote audio plays through Web Audio, so ask for the platform's voice
    // processing explicitly rather than relying on per-WebView defaults.
    const audioConstraint: MediaTrackConstraints = {
      echoCancellation: true,
      noiseSuppression: true,
      autoGainControl: true,
      channelCount: 1,
      ...(selectedMic.value ? { deviceId: { exact: selectedMic.value } } : {}),
    };

    // Android WebView holds the camera HAL for ~500ms after release;
    // retry with backoff to avoid "NotReadableError" during handoff.
    let stream: MediaStream | null = null;
    for (let attempt = 0; attempt < 3; attempt++) {
      try {
        stream = await navigator.mediaDevices.getUserMedia({
          video: videoConstraint,
          audio: audioConstraint,
        });
        break;
      } catch (e: unknown) {
        const name = e instanceof DOMException ? e.name : "";
        if (name === "NotReadableError" && attempt < 2) {
          await new Promise((r) => setTimeout(r, 500 * (attempt + 1)));
          continue;
        }
        if (attempt === 2 || name !== "NotReadableError") {
          try {
            stream = await navigator.mediaDevices.getUserMedia({ video: false, audio: audioConstraint });
            error.value = "Camera unavailable — audio only.";
          } catch {
            error.value = `Camera/mic access failed: ${e instanceof Error ? e.message : String(e)}`;
            return;
          }
          break;
        }
      }
    }

    if (stream) {
      if (generation !== previewGeneration) {
        // A newer startPreview/stopPreview superseded this attempt while
        // getUserMedia was in flight — release the orphaned capture.
        stream.getTracks().forEach((t) => t.stop());
        return;
      }
      stopPreview();
      localStream.value = stream;
      await enumerateDevices();
      applyMuteState();
      startMicLevelMonitor(stream);
    }
  }

  function startMicLevelMonitor(stream: MediaStream) {
    try {
      audioContext = new AudioContext();
      const source = audioContext.createMediaStreamSource(stream);
      const analyser = audioContext.createAnalyser();
      analyser.fftSize = 256;
      source.connect(analyser);
      const dataArray = new Uint8Array(analyser.frequencyBinCount);
      // ~15 Hz is plenty for a level meter; rAF drove a reactive update
      // (and re-render) every display frame for the whole call.
      function update() {
        analyser.getByteFrequencyData(dataArray);
        let sum = 0;
        for (let i = 0; i < dataArray.length; i++) sum += dataArray[i]!;
        const level = Math.round((sum / (dataArray.length * 255)) * 50) / 50;
        if (level !== micLevel.value) micLevel.value = level;
      }
      update();
      micLevelTimer = setInterval(update, 66);
    } catch (e) {
      console.warn("[media] Mic level monitor failed:", e);
    }
  }

  function stopMicLevelMonitor() {
    if (micLevelTimer !== null) {
      clearInterval(micLevelTimer);
      micLevelTimer = null;
    }
    micLevel.value = 0;
  }

  function stopPreview() {
    previewGeneration++;
    localStream.value?.getTracks().forEach((t) => t.stop());
    localStream.value = null;
    stopMicLevelMonitor();
    if (audioContext) { audioContext.close(); audioContext = null; }
  }

  function applyMuteState() {
    localStream.value?.getAudioTracks().forEach((t) => { t.enabled = !audioMuted.value; });
    localStream.value?.getVideoTracks().forEach((t) => { t.enabled = !videoMuted.value; });
  }

  // Tell connected call peers about a local mute / camera-off change so their
  // UI can reflect it (the local track toggle alone is invisible to them).
  async function broadcastControl(action: Record<string, unknown>) {
    try {
      const { peers } = useCall();
      if (!peers.value.length) return;
      const { invoke } = await import("@tauri-apps/api/core");
      for (const p of peers.value) {
        invoke("send_control", { peerId: p, action }).catch(() => {});
      }
    } catch (e) {
      console.warn("[media] broadcastControl failed:", e);
    }
  }

  function toggleAudio() {
    audioMuted.value = !audioMuted.value;
    applyMuteState();
    void broadcastControl({ action: "mute", muted: audioMuted.value });
  }

  function toggleVideo() {
    videoMuted.value = !videoMuted.value;
    applyMuteState();
    void broadcastControl({ action: "video_off", off: videoMuted.value });
  }

  async function switchCamera(deviceId: string) {
    selectedCamera.value = deviceId;
    if (localStream.value) await startPreview();
  }

  async function switchMic(deviceId: string) {
    selectedMic.value = deviceId;
    if (localStream.value) await startPreview();
  }

  return {
    localStream, cameras, microphones, selectedCamera, selectedMic,
    micLevel, audioMuted, videoMuted, error,
    enumerateDevices, startPreview, stopPreview,
    toggleAudio, toggleVideo, switchCamera, switchMic,
  };
}
