export type CallState = "idle" | "creating" | "joining" | "waiting" | "connected";

// Singleton state — shared across all pages/components
const state = ref<CallState>("idle");
const ticket = ref<string | null>(null);
const peerId = ref<string | null>(null);
const nodeId = ref<string | null>(null);
const sidecarConnected = ref(false);
const error = ref<string | null>(null);
const peers = ref<string[]>([]);

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
    try {
      const { invoke } = await import("@tauri-apps/api/core");
      await invoke("join_call", { ticket: t });
      navigateTo("/lobby");
    } catch (e) {
      error.value = `Failed to join: ${e}`;
      state.value = "idle";
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
    ticket.value = null;
    navigateTo("/");
  }

  return {
    state,
    ticket,
    peerId,
    nodeId,
    peers,
    sidecarConnected,
    error,
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
      try {
        const info = await invoke<{ id: string; ticket: string }>("get_node_info");
        nodeId.value = info.id;
        sidecarConnected.value = true;
      } catch {
        if (++retries < 15) {
          setTimeout(fetchNodeInfo, 2000);
        } else {
          error.value = "Failed to connect to Iroh after 30s";
        }
      }
    }
    fetchNodeInfo();

    listen<any>("peer-connected", (event) => {
      const data = event.payload;
      const pid = typeof data === "string" ? data : data?.peer_id;
      if (pid && !peers.value.includes(pid)) {
        peers.value.push(pid);
      }
      peerId.value = pid;
      state.value = "connected";
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

    listen<any>("nafaq-error", (event) => {
      error.value = event.payload?.message || String(event.payload);
    });
  } catch {
    sidecarConnected.value = false;
  }
}
