import type { Ref } from "vue";

const onlineStatus = ref<Record<string, boolean>>({});
let probeInterval: ReturnType<typeof setInterval> | null = null;

export function usePresence() {
  async function probeAllByIds(nodeIds: string[]) {
    const { invoke } = await import("@tauri-apps/api/core");
    const results = await Promise.allSettled(
      nodeIds.map(async (nodeId) => {
        const online = await invoke<boolean>("check_presence", { nodeId }).catch(() => false);
        return { nodeId, online };
      })
    );
    const newStatus: Record<string, boolean> = {};
    for (const result of results) {
      if (result.status === "fulfilled") {
        newStatus[result.value.nodeId] = result.value.online;
      }
    }
    onlineStatus.value = newStatus;
  }

  function startProbing(nodeIds: Ref<string[]>) {
    stopProbing(); // Clear any existing interval
    probeAllByIds(nodeIds.value);
    probeInterval = setInterval(() => probeAllByIds(nodeIds.value), 30_000);
  }

  function stopProbing() {
    if (probeInterval) {
      clearInterval(probeInterval);
      probeInterval = null;
    }
  }

  function isOnline(nodeId: string): boolean {
    return onlineStatus.value[nodeId] ?? false;
  }

  return { onlineStatus, startProbing, stopProbing, isOnline };
}
