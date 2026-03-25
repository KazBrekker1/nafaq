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

let analyserInterval: ReturnType<typeof setInterval> | null = null;
let audioContext: AudioContext | null = null;

export function useMedia() {
  async function enumerateDevices() {
    try {
      const devices = await navigator.mediaDevices.enumerateDevices();
      cameras.value = devices
        .filter((d) => d.kind === "videoinput")
        .map((d) => ({ deviceId: d.deviceId, label: d.label || `Camera ${d.deviceId.slice(0, 8)}` }));
      microphones.value = devices
        .filter((d) => d.kind === "audioinput")
        .map((d) => ({ deviceId: d.deviceId, label: d.label || `Mic ${d.deviceId.slice(0, 8)}` }));
      if (!selectedCamera.value && cameras.value.length > 0) selectedCamera.value = cameras.value[0].deviceId;
      if (!selectedMic.value && microphones.value.length > 0) selectedMic.value = microphones.value[0].deviceId;
    } catch (e: unknown) {
      error.value = `Device enumeration failed: ${e instanceof Error ? e.message : String(e)}`;
    }
  }

  async function startPreview() {
    error.value = null;
    try {
      const stream = await navigator.mediaDevices.getUserMedia({
        video: selectedCamera.value ? { deviceId: { exact: selectedCamera.value } } : true,
        audio: selectedMic.value ? { deviceId: { exact: selectedMic.value } } : true,
      });
      stopPreview();
      localStream.value = stream;
      await enumerateDevices();
      startMicLevelMonitor(stream);
    } catch (e: unknown) {
      error.value = `Camera/mic access failed: ${e instanceof Error ? e.message : String(e)}`;
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
      analyserInterval = setInterval(() => {
        analyser.getByteFrequencyData(dataArray);
        const avg = dataArray.reduce((a, b) => a + b, 0) / dataArray.length;
        micLevel.value = Math.min(100, Math.round((avg / 128) * 100));
      }, 100);
    } catch (e) {
      console.warn("[media] Mic level monitor failed:", e);
    }
  }

  function stopPreview() {
    localStream.value?.getTracks().forEach((t) => t.stop());
    localStream.value = null;
    if (analyserInterval) { clearInterval(analyserInterval); analyserInterval = null; }
    if (audioContext) { audioContext.close(); audioContext = null; }
    micLevel.value = 0;
  }

  function toggleAudio() {
    audioMuted.value = !audioMuted.value;
    localStream.value?.getAudioTracks().forEach((t) => { t.enabled = !audioMuted.value; });
  }

  function toggleVideo() {
    videoMuted.value = !videoMuted.value;
    localStream.value?.getVideoTracks().forEach((t) => { t.enabled = !videoMuted.value; });
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
