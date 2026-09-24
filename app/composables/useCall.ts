import { useNodeRuntime, type RelayStatus } from "./useNodeRuntime";

export type CallState = "idle" | "creating" | "waiting" | "ringing" | "joining" | "connected";
export type ConnectionProgress =
  | "idle"
  | "starting-node"
  | "relay-connecting"
  | "node-ready"
  | "relay-degraded"
  | "relay-offline"
  | "connecting"
  | "securing"
  | "connected";

// Singleton state — shared across all pages/components
const state = ref<CallState>("idle");
const ticket = ref<string | null>(null);
const peerId = ref<string | null>(null);
const error = ref<string | null>(null);
const peers = ref<string[]>([]);
const displayName = ref("");
const peerNames = ref<Record<string, string>>({});
const callConnectionProgress = ref<"idle" | "connecting" | "securing" | "connected">("idle");
const incomingInvite = ref<{ peerId: string; ticket: string } | null>(null);
const missedCall = ref<{ callerName: string; timestamp: number } | null>(null);
const lastDisconnectedPeer = ref<{ id: string; name: string } | null>(null);
const allPeersLeft = ref(false);
// Remote peers' mute / camera-off state, driven by their control messages.
const peerMuted = ref<Record<string, boolean>>({});
const peerVideoOff = ref<Record<string, boolean>>({});
// The peer we invited while state is "waiting" (caller side) — needed so a
// cancel/no-answer timeout knows who to send CallCancel to.
const invitedPeerId = ref<string | null>(null);

let ringingTimer: ReturnType<typeof setTimeout> | null = null;
let missedCallTimer: ReturnType<typeof setTimeout> | null = null;
let answerTimer: ReturnType<typeof setTimeout> | null = null;
let initialized = false;
let callUnlisteners: Array<() => void> = [];

const RING_TIMEOUT_MS = 45_000;

function showMissedCall(callerName: string) {
  missedCall.value = { callerName, timestamp: Date.now() };
  if (missedCallTimer) clearTimeout(missedCallTimer);
  missedCallTimer = setTimeout(() => { missedCall.value = null; }, 5000);
}

function relayUnavailableMessage(relayStatus: RelayStatus) {
  if (relayStatus === "degraded") {
    return "Relay is degraded. New call tickets are unavailable until the relay recovers.";
  }
  if (relayStatus === "offline") {
    return "Relay is offline. New call tickets are unavailable until the relay comes back online.";
  }
  return "Relay is still connecting. New call tickets will be available once the relay is online.";
}

function clearRingingTimer() {
  if (ringingTimer) { clearTimeout(ringingTimer); ringingTimer = null; }
}

function clearAnswerTimer() {
  if (answerTimer) { clearTimeout(answerTimer); answerTimer = null; }
}

// Module-level (not inside useCall()) so both the composable's returned API
// and the module-scope event listeners below can call these directly.

async function joinCall(t: string) {
  error.value = null;
  state.value = "joining";
  ticket.value = t;
  callConnectionProgress.value = "connecting";
  try {
    const { invoke } = await import("@tauri-apps/api/core");
    callConnectionProgress.value = "securing";
    await invoke("join_call", { ticket: t });
    callConnectionProgress.value = "connected";
  } catch (e) {
    error.value = `Failed to join: ${e}`;
    state.value = "idle";
    callConnectionProgress.value = "idle";
  }
}

// Called by the DM page once it has actually sent the CallInvite DM, so we
// know who we're waiting on and can time out the ring if they never answer.
function startWaitingForAnswer(targetPeerId: string) {
  invitedPeerId.value = targetPeerId;
  clearAnswerTimer();
  answerTimer = setTimeout(() => {
    if (state.value === "waiting" && invitedPeerId.value === targetPeerId) {
      error.value = "No answer.";
      void cancelPendingCall();
    }
  }, RING_TIMEOUT_MS);
}

// Caller gives up on a pending invite before the callee answered — either an
// explicit cancel (hang up while "waiting") or the ring timeout above, or a
// decline arriving from the callee. Best-effort notifies the callee
// (CallCancel) and always tears down our own pending call state, whether or
// not the notification lands. `notifyPeer: false` skips the CallCancel send
// for the decline-received path, where the callee already knows the call is
// over (they're the one who declined) — there's nothing to notify them of.
async function cancelPendingCall({ notifyPeer = true }: { notifyPeer?: boolean } = {}) {
  clearAnswerTimer();
  const target = invitedPeerId.value;
  invitedPeerId.value = null;
  if (notifyPeer && target) {
    try {
      const { invoke } = await import("@tauri-apps/api/core");
      await invoke("cancel_call", { peerId: target });
    } catch (e) {
      console.warn("[call] cancel_call failed:", e);
    }
  }
  await terminateCall({ navigate: true });
}

// Shared teardown for both the explicit "End Call" button and any path
// that leaves /call without clicking it (route navigation, unmount
// fallback) — those callers pass navigate: false since they're already
// handling (or not needing) the redirect themselves.
async function terminateCall({ navigate = true }: { navigate?: boolean } = {}) {
  try {
    const { invoke } = await import("@tauri-apps/api/core");
    // Per-peer: one failed end_call must not leave the remaining peers'
    // backend sessions orphaned.
    for (const p of peers.value) {
      await invoke("end_call", { peerId: p }).catch((e) => {
        console.warn(`[call] end_call failed for ${p}:`, e);
      });
    }
  } catch (e) {
    console.warn("[call] end_call cleanup failed:", e);
  }
  useMedia().stopPreview();
  clearRingingTimer();
  clearAnswerTimer();
  invitedPeerId.value = null;
  state.value = "idle";
  peerId.value = null;
  peers.value = [];
  peerNames.value = {};
  peerMuted.value = {};
  peerVideoOff.value = {};
  ticket.value = null;
  incomingInvite.value = null;
  allPeersLeft.value = false;
  lastDisconnectedPeer.value = null;
  callConnectionProgress.value = "idle";
  if (navigate) navigateTo("/");
}

// Hang up. While still "waiting" for the callee to answer, this is a cancel
// (nobody ever joined) — route it through cancelPendingCall so the callee is
// told and our own session-active flag is cleared, instead of silently
// tearing down local state with nothing sent over the wire.
async function endCall() {
  if (state.value === "waiting") {
    await cancelPendingCall();
    return;
  }
  await terminateCall({ navigate: true });
}

async function acceptInvite() {
  if (!incomingInvite.value) return;
  clearRingingTimer();
  const t = incomingInvite.value.ticket;
  incomingInvite.value = null;
  navigateTo("/call");
  await joinCall(t);
}

async function declineInvite() {
  if (!incomingInvite.value) return;
  clearRingingTimer();
  const callerPeerId = incomingInvite.value.peerId;
  state.value = "idle";
  ticket.value = null;
  peerId.value = null;
  incomingInvite.value = null;
  callConnectionProgress.value = "idle";
  try {
    const { invoke } = await import("@tauri-apps/api/core");
    await invoke("send_call_decline", { peerId: callerPeerId });
  } catch (e) {
    console.warn("[call] failed to send decline:", e);
  }
}

export function useCall() {
  const nodeRuntime = useNodeRuntime();
  const nodeId = nodeRuntime.nodeId;
  const shareTicket = nodeRuntime.shareTicket;
  const nodeReady = computed(() => Boolean(nodeId.value && nodeRuntime.relayStatus.value === "online" && shareTicket.value));
  const connectionProgress = computed<ConnectionProgress>(() => {
    if (callConnectionProgress.value !== "idle") return callConnectionProgress.value;
    if (!nodeId.value) return "starting-node";
    switch (nodeRuntime.relayStatus.value) {
      case "starting":
      case "connecting":
        return "relay-connecting";
      case "online":
        return "node-ready";
      case "degraded":
        return "relay-degraded";
      case "offline":
        return "relay-offline";
    }
  });

  if (!initialized) {
    initialized = true;
    initCallListeners();
  }

  async function createCall(): Promise<string | null> {
    error.value = null;
    state.value = "creating";
    await nodeRuntime.init();

    if (!nodeId.value) {
      error.value = "Node identity is still loading. Try again in a moment.";
      state.value = "idle";
      return null;
    }

    if (nodeRuntime.relayStatus.value !== "online") {
      error.value = relayUnavailableMessage(nodeRuntime.relayStatus.value);
      state.value = "idle";
      return null;
    }

    try {
      const { invoke } = await import("@tauri-apps/api/core");
      const t = shareTicket.value ?? await invoke<string>("create_call");
      if (!t) {
        throw new Error("ticket unavailable");
      }
      nodeRuntime.ticket.value = t;
      ticket.value = t;
      state.value = "waiting";
      return t;
    } catch (e) {
      error.value = `Failed to create call ticket: ${e}`;
      state.value = "idle";
      return null;
    }
  }

  return {
    state,
    ticket,
    shareTicket,
    peerId,
    nodeId,
    peers,
    nodeReady,
    relayStatus: nodeRuntime.relayStatus,
    nodeError: nodeRuntime.nodeError,
    error,
    displayName,
    peerNames,
    peerMuted,
    peerVideoOff,
    connectionProgress,
    incomingInvite,
    missedCall,
    lastDisconnectedPeer,
    allPeersLeft,
    createCall,
    joinCall,
    endCall,
    terminateCall,
    acceptInvite,
    declineInvite,
    startWaitingForAnswer,
  };
}

async function initCallListeners() {
  if (!import.meta.client) return;

  const nodeRuntime = useNodeRuntime();
  await nodeRuntime.init();

  try {
    const { invoke } = await import("@tauri-apps/api/core");
    const { listen } = await import("@tauri-apps/api/event");

    callUnlisteners.push(await listen<any>("peer-connected", async (event) => {
      const data = event.payload;
      const pid = typeof data === "string" ? data : data?.peer_id;
      if (!pid) return;

      if (state.value === "idle") {
        // We're not expecting a call — e.g. the caller cancelled and this is
        // the invitee's acceptance arriving late. The Rust side now rejects
        // inbound call dials at the protocol level when no call session is
        // active (ConnectionManager::setup_connection's call_session_active
        // gate, set by create_call/join_call and cleared on cancel/end), so
        // this should be rare in practice. Keep this reject as defense in
        // depth against any race between local "idle" state and backend
        // teardown.
        invoke("end_call", { peerId: pid }).catch((e) => {
          console.warn(`[call] failed to reject ghost peer-connected for ${pid}:`, e);
        });
        return;
      }

      clearAnswerTimer();
      invitedPeerId.value = null;
      if (!peers.value.includes(pid)) {
        peers.value.push(pid);
      }
      allPeersLeft.value = false;
      peerId.value = pid;
      state.value = "connected";
      callConnectionProgress.value = "connected";
      // Send our display name to the new peer
      if (displayName.value && pid) {
        invoke("send_control", {
          peerId: pid,
          action: { action: "set_display_name", name: displayName.value },
        }).catch(() => {});
      }
    }));

    callUnlisteners.push(await listen<any>("peer-disconnected", (event) => {
      const data = event.payload;
      const pid = typeof data === "string" ? data : data?.peer_id;
      const peerName = peerNames.value[pid] || pid?.slice(0, 12) || "Peer";
      const idx = peers.value.indexOf(pid);
      if (idx >= 0) peers.value.splice(idx, 1);
      // Drop the departed peer's media state.
      if (pid && (pid in peerMuted.value || pid in peerVideoOff.value)) {
        const { [pid]: _m, ...restMuted } = peerMuted.value;
        const { [pid]: _v, ...restVideo } = peerVideoOff.value;
        peerMuted.value = restMuted;
        peerVideoOff.value = restVideo;
      }

      lastDisconnectedPeer.value = { id: pid, name: peerName };
      setTimeout(() => {
        if (lastDisconnectedPeer.value?.id === pid) {
          lastDisconnectedPeer.value = null;
        }
      }, 3500);

      if (peers.value.length === 0) {
        allPeersLeft.value = true;
      }
    }));

    callUnlisteners.push(await listen<any>("control-received", (event) => {
      const data = event.payload;
      const pid = data?.peer_id;
      const action = data?.action;
      if (!pid || !action) return;
      if (action.action === "set_display_name" && typeof action.name === "string") {
        peerNames.value = { ...peerNames.value, [pid]: action.name };
      } else if (action.action === "mute" && typeof action.muted === "boolean") {
        peerMuted.value = { ...peerMuted.value, [pid]: action.muted };
      } else if (action.action === "video_off" && typeof action.off === "boolean") {
        peerVideoOff.value = { ...peerVideoOff.value, [pid]: action.off };
      }
    }));

    callUnlisteners.push(await listen<any>("call-invite-received", (event) => {
      const data = event.payload;
      const pid = typeof data === "string" ? data : data?.peer_id;
      const inviteTicket = data?.ticket;
      if (!pid || !inviteTicket) return;

      if (state.value === "idle") {
        // Show incoming call banner
        state.value = "ringing";
        ticket.value = inviteTicket;
        peerId.value = pid;
        incomingInvite.value = { peerId: pid, ticket: inviteTicket };
        // Auto-decline after 30 seconds
        ringingTimer = setTimeout(() => {
          if (state.value === "ringing") {
            const callerName = peerNames.value[pid] || pid.slice(0, 12);
            ringingTimer = null;
            state.value = "idle";
            ticket.value = null;
            peerId.value = null;
            incomingInvite.value = null;
            showMissedCall(callerName);
            invoke("send_call_decline", { peerId: pid }).catch((e) => {
              console.warn(`[call] failed to send auto-decline to ${pid}:`, e);
            });
          }
        }, 30_000);
      } else {
        // Already busy — record as missed call
        const callerName = peerNames.value[pid] || pid.slice(0, 12);
        showMissedCall(callerName);
      }
    }));

    // Callee declined (or its own ring timeout auto-declined) — we're the
    // caller still "waiting". Surface it and stop waiting; nobody ever
    // connected, so a plain terminateCall (no CallCancel to send) is enough.
    callUnlisteners.push(await listen<any>("call-decline-received", (event) => {
      const data = event.payload;
      const pid = typeof data === "string" ? data : data?.peer_id;
      if (!pid) return;
      if (state.value === "waiting" && invitedPeerId.value === pid) {
        error.value = "Call declined.";
        void cancelPendingCall({ notifyPeer: false });
      }
    }));

    // Caller cancelled a pending invite — we're the callee still ringing.
    // Dismiss the banner and record it as missed (distinct from us
    // deliberately declining — see declineInvite/send_call_decline).
    callUnlisteners.push(await listen<any>("call-cancel-received", (event) => {
      const data = event.payload;
      const pid = typeof data === "string" ? data : data?.peer_id;
      if (!pid) return;
      if (state.value === "ringing" && incomingInvite.value?.peerId === pid) {
        const callerName = peerNames.value[pid] || pid.slice(0, 12);
        clearRingingTimer();
        state.value = "idle";
        ticket.value = null;
        peerId.value = null;
        incomingInvite.value = null;
        showMissedCall(callerName);
      }
    }));

    callUnlisteners.push(await listen<any>("nafaq-error", (event) => {
      error.value = event.payload?.message || String(event.payload);
    }));
  } catch (e) {
    console.warn("[call] listener init failed:", e);
    // Surface on the call error ref — nodeError belongs to useNodeRuntime and
    // clobbering it would mask a genuine runtime failure.
    error.value = "Could not initialize call event listeners.";
    // Allow a later useCall() to retry instead of staying half-initialized.
    destroyCallListeners();
  }
}

function destroyCallListeners() {
  for (const un of callUnlisteners) un();
  callUnlisteners = [];
  initialized = false;
}

// HMR-safe: drop the singleton listeners so a reloaded module doesn't stack a
// second copy that double-fires every call event.
if (import.meta.hot) {
  import.meta.hot.dispose(() => {
    destroyCallListeners();
    peerMuted.value = {};
    peerVideoOff.value = {};
  });
}
