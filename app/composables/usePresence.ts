import { ref, type Ref } from "vue";
import { useNodeRuntime, type PeerConnectionStatus } from "./useNodeRuntime";

const onlineStatus = ref<Record<string, boolean>>({});
let bootstrapped = false;
let unlistenPromise: Promise<() => void> | null = null;
let runtime: ReturnType<typeof useNodeRuntime> | null = null;

function nodeRuntime() {
  runtime ??= useNodeRuntime();
  return runtime;
}

export function uniquePresenceIds(nodeIds: string[]): string[] {
  return [...new Set(nodeIds.filter((nodeId) => nodeId.length > 0))];
}

export function peerConnectionStatusToPresence(status: PeerConnectionStatus | undefined): boolean | null {
  switch (status) {
    case "connected":
    case "suspect":
    case "reconnecting":
      return true;
    case "disconnected":
    case "failed":
      return false;
    default:
      return null;
  }
}

function knownPresence(nodeId: string): boolean | null {
  return peerConnectionStatusToPresence(nodeRuntime().peerConnectionStatuses.value[nodeId]);
}

interface PresenceChangedPayload {
  peer_id: string;
  online: boolean;
}

async function ensureBootstrap() {
  if (bootstrapped) return;
  bootstrapped = true;

  const { invoke } = await import("@tauri-apps/api/core");
  const { listen } = await import("@tauri-apps/api/event");

  try {
    const snapshot = await invoke<Record<string, boolean>>("get_presence_snapshot");
    onlineStatus.value = { ...snapshot };
  } catch (err) {
    console.warn("get_presence_snapshot failed", err);
  }

  unlistenPromise = listen<PresenceChangedPayload>("presence-changed", (event) => {
    const { peer_id, online } = event.payload;
    if (onlineStatus.value[peer_id] === online) return;
    onlineStatus.value = { ...onlineStatus.value, [peer_id]: online };
  });
}

export function usePresence() {
  void ensureBootstrap();

  // Kept for backwards compatibility with existing callers; gossip pushes
  // updates automatically, so these are no-ops.
  function startProbing(_nodeIds: Ref<string[]>) {
    void ensureBootstrap();
  }

  function stopProbing() {
    // intentionally empty — presence is managed by gossip backend
  }

  function isOnline(nodeId: string): boolean {
    return knownPresence(nodeId) ?? onlineStatus.value[nodeId] ?? false;
  }

  return { onlineStatus, startProbing, stopProbing, isOnline };
}

// HMR-safe cleanup
if (typeof import.meta.hot !== "undefined") {
  import.meta.hot?.dispose(() => {
    void unlistenPromise?.then((u) => u());
    bootstrapped = false;
    unlistenPromise = null;
    onlineStatus.value = {};
  });
}
