// usePresence — lightweight ping-based online detection.
// Tries to connect to a peer node; marks online if successful within timeout.

const onlineSet = ref<Set<string>>(new Set());
const probingSet = ref<Set<string>>(new Set());
let probeInterval: ReturnType<typeof setInterval> | null = null;
let probeTargets: string[] = [];

export function usePresence() {
  function isOnline(nodeId: string): boolean {
    return onlineSet.value.has(nodeId);
  }

  async function probeOne(nodeId: string): Promise<void> {
    if (probingSet.value.has(nodeId)) return;
    probingSet.value.add(nodeId);
    try {
      const { invoke } = await import("@tauri-apps/api/core");
      const reachable = await invoke<boolean>("probe_contact", { nodeId }).catch(() => false);
      if (reachable) {
        onlineSet.value = new Set([...onlineSet.value, nodeId]);
      } else {
        const next = new Set(onlineSet.value);
        next.delete(nodeId);
        onlineSet.value = next;
      }
    } finally {
      probingSet.value.delete(nodeId);
    }
  }

  function startProbing(nodeIds: string[], intervalMs = 30_000) {
    probeTargets = nodeIds;
    // Immediate first pass
    for (const id of nodeIds) probeOne(id);

    if (probeInterval) return; // already running
    probeInterval = setInterval(() => {
      for (const id of probeTargets) probeOne(id);
    }, intervalMs);
  }

  function stopProbing() {
    if (probeInterval) {
      clearInterval(probeInterval);
      probeInterval = null;
    }
    probeTargets = [];
  }

  return { isOnline, startProbing, stopProbing, onlineSet };
}
