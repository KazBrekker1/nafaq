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

let ringingTimer: ReturnType<typeof setTimeout> | null = null;
let missedCallTimer: ReturnType<typeof setTimeout> | null = null;
let initialized = false;

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

  function clearRingingTimer() {
    if (ringingTimer) { clearTimeout(ringingTimer); ringingTimer = null; }
  }

  async function endCall() {
    try {
      const { invoke } = await import("@tauri-apps/api/core");
      for (const p of peers.value) {
        await invoke("end_call", { peerId: p });
      }
    } catch {}
    useMedia().stopPreview();
    clearRingingTimer();
    state.value = "idle";
    peerId.value = null;
    peers.value = [];
    peerNames.value = {};
    ticket.value = null;
    incomingInvite.value = null;
    allPeersLeft.value = false;
    lastDisconnectedPeer.value = null;
    callConnectionProgress.value = "idle";
    navigateTo("/");
  }

  async function acceptInvite() {
    if (!incomingInvite.value) return;
    clearRingingTimer();
    const t = incomingInvite.value.ticket;
    incomingInvite.value = null;
    navigateTo("/call");
    await joinCall(t);
  }

  function declineInvite() {
    clearRingingTimer();
    state.value = "idle";
    ticket.value = null;
    peerId.value = null;
    incomingInvite.value = null;
    callConnectionProgress.value = "idle";
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
    connectionProgress,
    incomingInvite,
    missedCall,
    lastDisconnectedPeer,
    allPeersLeft,
    createCall,
    joinCall,
    endCall,
    acceptInvite,
    declineInvite,
  };
}

async function initCallListeners() {
  if (!import.meta.client) return;

  const nodeRuntime = useNodeRuntime();
  await nodeRuntime.init();

  try {
    const { invoke } = await import("@tauri-apps/api/core");
    const { listen } = await import("@tauri-apps/api/event");

    listen<any>("peer-connected", async (event) => {
      const data = event.payload;
      const pid = typeof data === "string" ? data : data?.peer_id;
      if (pid && !peers.value.includes(pid)) {
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
    });

    listen<any>("peer-disconnected", (event) => {
      const data = event.payload;
      const pid = typeof data === "string" ? data : data?.peer_id;
      const peerName = peerNames.value[pid] || pid?.slice(0, 12) || "Peer";
      const idx = peers.value.indexOf(pid);
      if (idx >= 0) peers.value.splice(idx, 1);

      lastDisconnectedPeer.value = { id: pid, name: peerName };
      setTimeout(() => {
        if (lastDisconnectedPeer.value?.id === pid) {
          lastDisconnectedPeer.value = null;
        }
      }, 3500);

      if (peers.value.length === 0) {
        allPeersLeft.value = true;
      }
    });

    listen<any>("control-received", (event) => {
      const data = event.payload;
      const pid = data?.peer_id;
      const action = data?.action;
      if (pid && action?.action === "set_display_name" && typeof action.name === "string") {
        peerNames.value = { ...peerNames.value, [pid]: action.name };
      }
    });

    listen<any>("call-invite-received", (event) => {
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
          }
        }, 30_000);
      } else {
        // Already busy — record as missed call
        const callerName = peerNames.value[pid] || pid.slice(0, 12);
        showMissedCall(callerName);
      }
    });

    listen<any>("nafaq-error", (event) => {
      error.value = event.payload?.message || String(event.payload);
    });
  } catch {
    nodeRuntime.nodeError.value = "Could not initialize call event listeners.";
  }
}
