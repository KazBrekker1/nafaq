export type CallState = "idle" | "creating" | "joining" | "waiting" | "connected";

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

let initialized = false;

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
      // Stay on home page to display/share the ticket. Peer connection auto-navigates to /call.
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
      // Don't navigate — show pre-call overlay on index page (Task 6 will handle this)
    } catch (e) {
      error.value = `Failed to join: ${e}`;
      state.value = "idle";
      connectionProgress.value = "node-ready";
    }
  }

  async function endCall() {
    try {
      const { invoke } = await import("@tauri-apps/api/core");
      for (const p of peers.value) {
        await invoke("end_call", { peerId: p });
      }
    } catch {}
    state.value = "idle";
    peerId.value = null;
    peers.value = [];
    peerNames.value = {};
    ticket.value = null;
    navigateTo("/");
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
    createCall,
    joinCall,
    endCall,
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
      peerId.value = pid;
      state.value = "connected";
      // Send our display name to the new peer
      if (displayName.value && pid) {
        invoke("send_control", {
          peerId: pid,
          action: { action: "set_display_name", name: displayName.value },
        }).catch(() => {});
      }
      navigateTo("/call");
    });

    listen<any>("peer-disconnected", (event) => {
      const data = event.payload;
      const pid = typeof data === "string" ? data : data?.peer_id;
      const idx = peers.value.indexOf(pid);
      if (idx >= 0) peers.value.splice(idx, 1);
      if (peers.value.length === 0) {
        state.value = "idle";
        peerId.value = null;
        ticket.value = null;
        navigateTo("/");
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

    listen<any>("nafaq-error", (event) => {
      error.value = event.payload?.message || String(event.payload);
    });
  } catch {
    nodeReady.value = false;
  }
}
