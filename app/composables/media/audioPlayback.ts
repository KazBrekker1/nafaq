import { ref } from "vue";
import { useSettings } from "~/composables/useSettings";
import { PLAYBACK_PROCESSOR_NAME, playbackWorkletSource } from "~/utils/jitterBuffer";
import {
  ensureReceiveRun,
  keepContextRunning,
  peerMediaStates,
  peerStateIfInCall,
  resumeWithTimeout,
  type PeerMediaState,
} from "./shared";

// Remote audio: one playback worklet (jitter buffer) per peer, plus speaking /
// active-speaker detection from the decoded PCM levels.

export const activeSpeaker = ref<string | null>(null);
export const peerSpeakingMap = ref<Record<string, boolean>>({});

const SPEAKING_RMS_THRESHOLD = 0.015;
const SPEAKING_DEBOUNCE_MS = 300;
const ACTIVE_SPEAKER_INTERVAL_MS = 300;
const ACTIVE_SPEAKER_SWITCH_THRESHOLD = 1.5;
const ACTIVE_SPEAKER_MIN_DURATION_MS = 500;
const ACTIVE_SPEAKER_SILENCE_MS = 1000;

let playbackCtx: AudioContext | null = null;
let playbackWorkletLoaded: Promise<boolean> | null = null;
let activeSpeakerInterval: ReturnType<typeof setInterval> | null = null;

// Route remote audio to Settings → speaker where the WebView supports it
// (AudioContext.setSinkId); elsewhere the system default output is used.
export function applyPreferredSpeaker({ onlyIfSet = false } = {}) {
  const ctx = playbackCtx as (AudioContext & { setSinkId?: (sinkId: string) => Promise<void> }) | null;
  if (!ctx || typeof ctx.setSinkId !== "function") return;
  const sinkId = useSettings().settings.value.preferredSpeaker ?? "";
  if (onlyIfSet && !sinkId) return;
  ctx.setSinkId(sinkId).catch((e) => {
    console.warn("[transport] setSinkId failed:", e);
  });
}

export async function ensurePlaybackContext(token: number) {
  if (!playbackCtx) {
    playbackCtx = new AudioContext({ sampleRate: 48000 });
    applyPreferredSpeaker({ onlyIfSet: true });
    const resumeOnGesture = keepContextRunning(playbackCtx, "playback");
    const resumed = await resumeWithTimeout(playbackCtx);
    ensureReceiveRun(token);
    if (!resumed) {
      console.warn("[transport] Playback AudioContext resume deferred until user interaction");
      resumeOnGesture();
    }
  } else if (playbackCtx.state !== "running") {
    await resumeWithTimeout(playbackCtx);
    ensureReceiveRun(token);
  }

  if (!playbackWorkletLoaded) {
    const ctx = playbackCtx;
    const blobUrl = URL.createObjectURL(
      new Blob([playbackWorkletSource()], { type: "application/javascript" }),
    );
    playbackWorkletLoaded = ctx.audioWorklet.addModule(blobUrl)
      .then(() => true)
      .catch((error) => {
        console.warn("[transport] Playback worklet failed to load", error);
        return false;
      })
      .finally(() => URL.revokeObjectURL(blobUrl));
  }
  const workletLoaded = await playbackWorkletLoaded;
  ensureReceiveRun(token);
  if (!workletLoaded) {
    playbackWorkletLoaded = null;
    throw new Error("Audio playback worklet unavailable");
  }
}

function ensurePeerAudioNode(peerState: PeerMediaState) {
  if (!playbackCtx || peerState.audioNode) return;
  const node = new AudioWorkletNode(playbackCtx, PLAYBACK_PROCESSOR_NAME, {
    numberOfInputs: 0,
    numberOfOutputs: 1,
    outputChannelCount: [1],
  });
  const gain = playbackCtx.createGain();
  gain.gain.value = 1;
  node.connect(gain);
  gain.connect(playbackCtx.destination);
  peerState.audioNode = node;
  peerState.audioGainNode = gain;
}

export function releasePeerAudio(peerState: PeerMediaState) {
  if (peerState.audioNode) {
    try { peerState.audioNode.disconnect(); } catch {}
    peerState.audioNode.port.close();
    peerState.audioNode = null;
  }
  if (peerState.audioGainNode) {
    try { peerState.audioGainNode.disconnect(); } catch {}
    peerState.audioGainNode = null;
  }
}

export function updateSpeakingMap() {
  const newMap: Record<string, boolean> = {};
  for (const [id, state] of peerMediaStates) {
    if (state.speaking) newMap[id] = true;
  }
  peerSpeakingMap.value = newMap;
}

export function handleIncomingPcm(peerId: string, pcmBytes: Uint8Array) {
  if (!playbackCtx) return;
  if (pcmBytes.byteLength < 2) return;
  const peerState = peerStateIfInCall(peerId);
  if (!peerState) return;

  // Copy out of the IPC buffer (it may be unaligned for Int16Array).
  const int16 = new Int16Array(pcmBytes.slice(0, pcmBytes.byteLength & ~1).buffer);
  const samples = new Float32Array(int16.length);
  let sum = 0;
  for (let i = 0; i < int16.length; i++) {
    const sample = int16[i]! / 32768;
    samples[i] = sample;
    sum += sample * sample;
  }

  ensurePeerAudioNode(peerState);
  peerState.audioNode?.port.postMessage({ pcm: samples }, [samples.buffer]);

  const rms = Math.sqrt(sum / int16.length);
  const now = Date.now();
  peerState.lastAudioAt = now;
  peerState.lastAudioRms = 0.7 * peerState.lastAudioRms + 0.3 * rms;

  const wasSpeaking = peerState.speaking;
  if (rms > SPEAKING_RMS_THRESHOLD) {
    if (!peerState.speaking) {
      peerState.speaking = true;
      peerState.speakingSince = now;
    }
    peerState.lastSpeakingTime = now;
  } else if (peerState.speaking && now - peerState.lastSpeakingTime > SPEAKING_DEBOUNCE_MS) {
    peerState.speaking = false;
  }

  // Only touch reactive state on an actual change — this runs 50x/s per peer.
  if (peerState.speaking !== wasSpeaking) updateSpeakingMap();
}

export function startActiveSpeakerDetection() {
  if (activeSpeakerInterval) return;
  activeSpeakerInterval = setInterval(() => {
    const now = Date.now();
    let loudest: string | null = null;
    let loudestRms = 0;
    let speakingChanged = false;

    for (const [peerId, state] of peerMediaStates) {
      // Speaking state is otherwise only updated per packet, so a peer
      // whose audio stops mid-word (muted, stalled, paused) would stay
      // "speaking" and keep its level forever.
      if (now - state.lastAudioAt > ACTIVE_SPEAKER_INTERVAL_MS) {
        state.lastAudioRms *= 0.5;
        if (state.speaking && now - state.lastSpeakingTime > SPEAKING_DEBOUNCE_MS) {
          state.speaking = false;
          speakingChanged = true;
        }
      }
      if (state.lastAudioRms > loudestRms) {
        loudestRms = state.lastAudioRms;
        loudest = peerId;
      }
    }
    if (speakingChanged) updateSpeakingMap();

    const current = activeSpeaker.value;
    const currentState = current ? peerMediaStates.get(current) : null;
    // Background noise alone never makes someone the speaker.
    const loudestAudible = loudest !== null && loudestRms > SPEAKING_RMS_THRESHOLD;
    let shouldSwitch = false;

    if (!current) {
      shouldSwitch = loudestAudible;
    } else if (currentState) {
      const currentSilent = now - currentState.lastSpeakingTime > ACTIVE_SPEAKER_SILENCE_MS;
      if (currentSilent && loudest !== current) {
        shouldSwitch = loudestAudible;
      } else if (loudest && loudest !== current) {
        const louderEnough = loudestRms > currentState.lastAudioRms * ACTIVE_SPEAKER_SWITCH_THRESHOLD;
        const loudestState = peerMediaStates.get(loudest);
        const speakingLongEnough = loudestState &&
          loudestState.speaking &&
          (now - loudestState.speakingSince) >= ACTIVE_SPEAKER_MIN_DURATION_MS;
        shouldSwitch = louderEnough && !!speakingLongEnough;
      }
    }

    if (shouldSwitch && loudest) {
      activeSpeaker.value = loudest;
    }
  }, ACTIVE_SPEAKER_INTERVAL_MS);
}

// Releases every peer's playback nodes and the playback context itself.
export async function closePlayback() {
  for (const [, state] of peerMediaStates) releasePeerAudio(state);
  if (playbackCtx) {
    await playbackCtx.close().catch(() => {});
    playbackCtx = null;
    // The worklet module belongs to the closed context.
    playbackWorkletLoaded = null;
  }
  if (activeSpeakerInterval) {
    clearInterval(activeSpeakerInterval);
    activeSpeakerInterval = null;
  }
}

export function resetSpeakers() {
  activeSpeaker.value = null;
  peerSpeakingMap.value = {};
}
