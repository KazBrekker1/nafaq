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
const showPreCallOverlay = ref(false);
const lastDisconnectedPeer = ref<{ id: string; name: string } | null>(null);
const allPeersLeft = ref(false);

let initialized = false;

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

  async function endCall() {
    try {
      const { invoke } = await import("@tauri-apps/api/core");
      await Promise.all(peers.value.map((p) => invoke("end_call", { peerId: p })));
    } catch {}
    state.value = "idle";
    peerId.value = null;
    peers.value = [];
    peerNames.value = {};
    ticket.value = null;
    showPreCallOverlay.value = false;
    allPeersLeft.value = false;
    lastDisconnectedPeer.value = null;
    navigateTo("/");
  }

  function joinCallFromOverlay() {
    showPreCallOverlay.value = false;
    navigateTo("/call");
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
    showPreCallOverlay,
    lastDisconnectedPeer,
    allPeersLeft,
    createCall,
    joinCall,
    endCall,
    joinCallFromOverlay,
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

    // Deep link: auto-join when app is opened via nafaq:// URL
    try {
      const { onOpenUrl, getCurrent } = await import("@tauri-apps/plugin-deep-link");

      const handleDeepLink = (urls: string[]) => {
        if (state.value !== "idle" && state.value !== "waiting") return;
        for (const raw of urls) {
          try {
            const parsed = new URL(raw);
            if (parsed.protocol === "nafaq:" && parsed.pathname === "//join") {
              const t = parsed.searchParams.get("ticket");
              if (t) { joinCall(t); return; }
            }
          } catch { /* not a URL */ }
        }
      };

      onOpenUrl(handleDeepLink);

      const pending = await getCurrent();
      if (pending) handleDeepLink(pending);
    } catch {
      // Expected in browser dev mode — plugin only loads in Tauri
    }

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
      // Show pre-call overlay instead of auto-navigating
      showPreCallOverlay.value = true;
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
        // Don't auto-redirect — show "everyone left" prompt
      }
    });

    listen<any>("control-received", (event) => {
      const data = event.payload;
      const pid = data?.peer_id;
      const action = data?.action;
      if (pid && action?.action === "set_display_name" && typeof action.name === "string") {
        peerNames.value[pid] = action.name;
      }
    });

    listen<any>("nafaq-error", (event) => {
      error.value = event.payload?.message || String(event.payload);
    });
  } catch {
    nodeReady.value = false;
  }
}
