import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { ref } from "vue";
import { useNodeRuntime } from "./useNodeRuntime";

const onlineStatus = ref<Record<string, boolean>>({});
let bootstrap: Promise<void> | null = null;
let unlisten: UnlistenFn | null = null;
let runtime: ReturnType<typeof useNodeRuntime> | null = null;

function nodeRuntime() {
  runtime ??= useNodeRuntime();
  return runtime;
}

interface PresenceChangedPayload {
  peer_id: string;
  online: boolean;
}

// Listen first, then snapshot: an event racing the snapshot is never lost, and
// the snapshot only fills peers no event has reported yet (events win). A
// failed snapshot is retried by the next usePresence() call; the listener stays.
function ensureBootstrap(): Promise<void> {
  bootstrap ??= (async () => {
    try {
      unlisten ??= await listen<PresenceChangedPayload>("presence-changed", (event) => {
        const { peer_id, online } = event.payload;
        if (onlineStatus.value[peer_id] === online) return;
        onlineStatus.value = { ...onlineStatus.value, [peer_id]: online };
      });
      const snapshot = await invoke<Record<string, boolean>>("get_presence_snapshot");
      onlineStatus.value = { ...snapshot, ...onlineStatus.value };
    } catch (err) {
      console.warn("[presence] bootstrap failed:", err);
      bootstrap = null;
    }
  })();
  return bootstrap;
}

export function usePresence() {
  void ensureBootstrap();

  // A live call connection confirms the peer is online, but a call ending
  // ("disconnected"/"failed") says nothing about whether the peer left the
  // network — defer to gossip presence for everything but "connected".
  function isOnline(nodeId: string): boolean {
    return nodeRuntime().peerConnectionStatuses.value[nodeId] === "connected"
      || (onlineStatus.value[nodeId] ?? false);
  }

  return { onlineStatus, isOnline };
}

if (import.meta.hot) {
  import.meta.hot.dispose(() => {
    unlisten?.();
    unlisten = null;
    bootstrap = null;
    onlineStatus.value = {};
  });
}
