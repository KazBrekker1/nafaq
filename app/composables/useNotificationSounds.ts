let audioCtx: AudioContext | null = null;

function getCtx(): AudioContext {
  if (!audioCtx) audioCtx = new AudioContext({ sampleRate: 48000 });
  if (audioCtx.state !== "running") audioCtx.resume().catch(() => {});
  return audioCtx;
}

function playTone(frequency: number, durationMs: number, volume: number, startOffset = 0) {
  const ctx = getCtx();
  const osc = ctx.createOscillator();
  const gain = ctx.createGain();
  osc.type = "sine";
  osc.frequency.value = frequency;
  gain.gain.value = volume;
  const start = ctx.currentTime + startOffset;
  const end = start + durationMs / 1000;
  gain.gain.setValueAtTime(volume, start);
  gain.gain.exponentialRampToValueAtTime(0.001, end);
  osc.connect(gain);
  gain.connect(ctx.destination);
  osc.start(start);
  osc.stop(end + 0.05);
}

export function useNotificationSounds() {
  function playPeerConnected() {
    playTone(523, 120, 0.15, 0);      // C5
    playTone(659, 120, 0.15, 0.12);   // E5
  }

  function playPeerLeft() {
    playTone(659, 120, 0.12, 0);      // E5
    playTone(440, 180, 0.12, 0.12);   // A4
  }

  function playMessageReceived() {
    playTone(784, 80, 0.08);          // G5 short blip
  }

  return { playPeerConnected, playPeerLeft, playMessageReceived };
}
