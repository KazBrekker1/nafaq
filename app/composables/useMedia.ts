export interface MediaDevice {
  deviceId: string;
  label: string;
}

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

let micLevelRafId: number | null = null;
let audioContext: AudioContext | null = null;

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

    const videoConstraint = selectedCamera.value
      ? { deviceId: { exact: selectedCamera.value } }
      : true;
    const audioConstraint = selectedMic.value
      ? { deviceId: { exact: selectedMic.value } }
      : true;

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
      function update() {
        analyser.getByteFrequencyData(dataArray);
        let sum = 0;
        for (let i = 0; i < dataArray.length; i++) sum += dataArray[i]!;
        micLevel.value = sum / (dataArray.length * 255);
        micLevelRafId = requestAnimationFrame(update);
      }
      update();
    } catch (e) {
      console.warn("[media] Mic level monitor failed:", e);
    }
  }

  function stopMicLevelMonitor() {
    if (micLevelRafId !== null) {
      cancelAnimationFrame(micLevelRafId);
      micLevelRafId = null;
    }
    micLevel.value = 0;
  }

  function stopPreview() {
    localStream.value?.getTracks().forEach((t) => t.stop());
    localStream.value = null;
    stopMicLevelMonitor();
    if (audioContext) { audioContext.close(); audioContext = null; }
  }

  function applyMuteState() {
    localStream.value?.getAudioTracks().forEach((t) => { t.enabled = !audioMuted.value; });
    localStream.value?.getVideoTracks().forEach((t) => { t.enabled = !videoMuted.value; });
  }

  function toggleAudio() {
    audioMuted.value = !audioMuted.value;
    applyMuteState();
  }

  function toggleVideo() {
    videoMuted.value = !videoMuted.value;
    applyMuteState();
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
