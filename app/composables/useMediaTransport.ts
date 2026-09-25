import { sendQualityForLevel } from "~/utils/sendQuality";
import {
  activeSpeaker,
  applyPreferredSpeaker,
  closePlayback,
  ensurePlaybackContext,
  peerSpeakingMap,
  releasePeerAudio,
  resetSpeakers,
  startActiveSpeakerDetection,
  updateSpeakingMap,
} from "./media/audioPlayback";
import {
  applyCaptureProfile,
  encoding,
  initCodecs,
  resetCaptureState,
  restartSending,
  startSending,
  teardownCapture,
} from "./media/capture";
import {
  noteQualityProfileEvent,
  qualityProfileEvents,
  refreshBaseProfile,
  resetProfiles,
  setBaseProfileFromBackend,
  setSendLevel,
  type QualityProfilePayload,
} from "./media/profile";
import { resetBridgeFallback, setupReceiveBridge, teardownReceiveBridge } from "./media/receiveBridge";
import { destroyAllVideoDecoders, forgetPeerVideoDecoderState, registerPeerCanvas } from "./media/receiveVideo";
import {
  bumpReceiveRun,
  currentReceiveRun,
  ensureReceiveRun,
  invokePromise,
  listenForRun,
  peerMediaStates,
  setPeerIdsProvider,
} from "./media/shared";

// Media transport orchestrator: owns the receive/capture lifecycle (start,
// stop, run tokens) and the backend quality signals. The pipeline pieces live
// in ./media/*.

const connectionQuality = ref<"good" | "degraded" | "poor">("good");

// Receive side lifecycle: startReceiving is idempotent while a session is
// starting/running.
let receiveState: "idle" | "starting" | "running" = "idle";

let unlistenDisconnect: (() => void) | null = null;
let unlistenQuality: (() => void) | null = null;
let unlistenSendQuality: (() => void) | null = null;
let stopSettingsWatch: (() => void) | null = null;

function disposeReceiveListeners() {
  unlistenDisconnect?.();
  unlistenDisconnect = null;
  unlistenQuality?.();
  unlistenQuality = null;
  unlistenSendQuality?.();
  unlistenSendQuality = null;
  stopSettingsWatch?.();
  stopSettingsWatch = null;
}

export function useMediaTransport() {
  async function startReceiving(getPeerIds: () => string[]) {
    setPeerIdsProvider(getPeerIds);
    if (receiveState !== "idle") return;

    const token = currentReceiveRun();
    receiveState = "starting";

    // A previous run that failed mid-setup (or a stale degraded session) may
    // have left listeners or the quality watcher behind — dispose them before
    // registering new ones so they never stack.
    disposeReceiveListeners();

    try {
      await startReceivingInner(token);
      receiveState = "running";
    } catch (error) {
      // stop() landed mid-setup and already tore everything down.
      if (token !== currentReceiveRun()) return;
      // Leave nothing half-registered so a later retry starts clean.
      disposeReceiveListeners();
      await teardownReceiveBridge(true).catch(() => {});
      receiveState = "idle";
      throw error;
    }
  }

  async function startReceivingInner(token: number) {
    // Quality listeners go first: a profile change emitted while the playback
    // context and bridge probe are still settling must not be missed.
    // Call-size profile: rebuild the encoder at the new base; the backend
    // controller applies its congestion level on top.
    unlistenQuality = await listenForRun<QualityProfilePayload>(token, "quality-profile-changed", async (event) => {
      noteQualityProfileEvent();
      setBaseProfileFromBackend(event.payload);
      await applyCaptureProfile({ rebuildEncoder: true });
    });

    // Single source of truth for outbound quality: the backend's controller
    // (driven by skipped frames, loss and queueing delay — not raw RTT).
    unlistenSendQuality = await listenForRun<{ level: number }>(token, "send-quality-changed", async (event) => {
      setSendLevel(event.payload.level);
      connectionQuality.value = sendQualityForLevel(event.payload.level);
      await applyCaptureProfile({ rebuildEncoder: false });
    });

    // Events only report changes; pick up the profile already in effect
    // (e.g. joining a call that already has 3+ peers) before codecs init.
    const eventsBeforeFetch = qualityProfileEvents;
    const invoke = await invokePromise;
    const current = await invoke<QualityProfilePayload>("get_quality_profile").catch((e) => {
      console.warn("[transport] get_quality_profile failed:", e);
      return null;
    });
    ensureReceiveRun(token);
    if (current && qualityProfileEvents === eventsBeforeFetch) setBaseProfileFromBackend(current);

    // Settings can change mid-call: quality caps rebuild the encoder, the
    // speaker choice re-routes playback.
    const { settings } = useSettings();
    const stopQualityWatch = watch(
      () => [settings.value.videoQuality, settings.value.dataSaver] as const,
      async () => {
        refreshBaseProfile();
        if (encoding.value) await applyCaptureProfile({ rebuildEncoder: true });
      },
    );
    const stopSpeakerWatch = watch(() => settings.value.preferredSpeaker, () => applyPreferredSpeaker());
    stopSettingsWatch = () => {
      stopQualityWatch();
      stopSpeakerWatch();
    };

    await ensurePlaybackContext(token);
    resetBridgeFallback();
    await teardownReceiveBridge(false);
    ensureReceiveRun(token);
    await setupReceiveBridge(token, false);

    unlistenDisconnect = await listenForRun<{ peer_id: string }>(token, "peer-disconnected", (event) => {
      const pid = typeof event.payload === "string" ? event.payload : event.payload?.peer_id;
      if (!pid) return;
      const state = peerMediaStates.get(pid);
      if (state) releasePeerAudio(state);
      forgetPeerVideoDecoderState(pid);
      peerMediaStates.delete(pid);
      if (activeSpeaker.value === pid) activeSpeaker.value = null;
      updateSpeakingMap();
    });

    startActiveSpeakerDetection();
  }

  async function stop() {
    bumpReceiveRun();

    teardownCapture();
    await teardownReceiveBridge(true);

    await closePlayback();
    disposeReceiveListeners();

    destroyAllVideoDecoders();

    // Release the Rust-side per-peer Opus/H.264 codec state too; the frontend
    // decoders above are separate, and without this the backend maps leak
    // across calls.
    try {
      const invoke = await invokePromise;
      await invoke("destroy_codecs");
    } catch (e) {
      console.warn("[transport] destroy_codecs failed:", e);
    }

    peerMediaStates.clear();
    resetSpeakers();
    setPeerIdsProvider(null);

    connectionQuality.value = "good";
    resetProfiles();
    resetCaptureState();
    receiveState = "idle";
  }

  async function setPeerVideoPaused(peerId: string, paused: boolean) {
    const state = peerMediaStates.get(peerId);
    if (!state || state.videoPaused === paused) return;
    state.videoPaused = paused;
    const invoke = await invokePromise;
    await invoke("send_control", {
      peerId,
      action: {
        action: "video_quality_request",
        layer: paused ? "none" : "high",
      },
    }).catch(() => {});
  }

  return {
    encoding,
    peerSpeakingMap,
    activeSpeaker,
    connectionQuality,
    initCodecs,
    registerPeerCanvas,
    startSending,
    restartSending,
    startReceiving,
    stop,
    setPeerVideoPaused,
  };
}
