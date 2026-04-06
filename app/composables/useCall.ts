export type CallState = "idle" | "creating" | "waiting" | "ringing" | "joining" | "connected";

// Singleton state — shared across all pages/components
const state = ref<CallState>("idle");
const ticket = ref<string | null>(null);
const shareTicket = ref<string | null>(null);
const peerId = ref<string | null>(null);
const nodeId = ref<string | null>(null);
const nodeReady = ref(false);
const error = ref<string | null>(null);
const peers = ref<string[]>([]);
const displayName = ref("");
const peerNames = ref<Record<string, string>>({});
const connectionProgress = ref<"idle" | "starting-node" | "node-ready" | "connecting" | "securing" | "connected">("idle");
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

export function useCall() {
  if (!initialized) {
    initialized = true;
    initCallListeners();
  }

  async function createCall() {
    error.value = null;
    state.value = "creating";
    try {
      const { invoke } = await import("@tauri-apps/api/core");
      const t = await invoke<string>("create_call");
      ticket.value = t;
      shareTicket.value = t;
      state.value = "waiting";
    } catch (e) {
      error.value = `Failed to create call: ${e}`;
      state.value = "idle";
    }
  }

  async function joinCall(t: string) {
    error.value = null;
    state.value = "joining";
    ticket.value = t;
    connectionProgress.value = "connecting";
    try {
      const { invoke } = await import("@tauri-apps/api/core");
      connectionProgress.value = "securing";
      await invoke("join_call", { ticket: t });
      connectionProgress.value = "connected";
    } catch (e) {
      error.value = `Failed to join: ${e}`;
      state.value = "idle";
      connectionProgress.value = "node-ready";
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
  }

  return {
    state,
    ticket,
    shareTicket,
    peerId,
    nodeId,
    peers,
    nodeReady,
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
  try {
    const { invoke } = await import("@tauri-apps/api/core");
    const { listen } = await import("@tauri-apps/api/event");

    // Get initial node info (retry if Iroh still initializing)
    let retries = 0;
    async function fetchNodeInfo() {
      connectionProgress.value = "starting-node";
      try {
        const info = await invoke<{ id: string; ticket: string }>("get_node_info");
        nodeId.value = info.id;
        shareTicket.value = info.ticket;
        nodeReady.value = true;
        connectionProgress.value = "node-ready";
      } catch {
        if (++retries < 15) {
          setTimeout(fetchNodeInfo, 2000);
        } else {
          error.value = "Could not start — check your network";
          connectionProgress.value = "idle";
        }
      }
    }
    fetchNodeInfo();

    listen<any>("peer-connected", async (event) => {
      const data = event.payload;
      const pid = typeof data === "string" ? data : data?.peer_id;
      if (pid && !peers.value.includes(pid)) {
        peers.value.push(pid);
      }
      allPeersLeft.value = false;
      peerId.value = pid;
      state.value = "connected";
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
    nodeReady.value = false;
  }
}
