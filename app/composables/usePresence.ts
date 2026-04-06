import type { Ref } from "vue";
import { useIntervalFn } from "@vueuse/core";

const onlineStatus = ref<Record<string, boolean>>({});
let activeNodeIds: Ref<string[]> | null = null;

async function probeAllByIds(nodeIds: string[]) {
  if (nodeIds.length === 0) return;
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
  // Only update if status actually changed to avoid unnecessary reactivity
  const changed = Object.keys(newStatus).some(
    k => newStatus[k] !== onlineStatus.value[k]
  ) || Object.keys(onlineStatus.value).length !== Object.keys(newStatus).length;
  if (changed) {
    onlineStatus.value = newStatus;
  }
}

const { pause, resume } = useIntervalFn(
  () => {
    if (activeNodeIds) probeAllByIds(activeNodeIds.value);
  },
  30_000,
  { immediate: false }
);

export function usePresence() {
  function startProbing(nodeIds: Ref<string[]>) {
    activeNodeIds = nodeIds;
    probeAllByIds(nodeIds.value);
    resume();
  }

  function stopProbing() {
    pause();
    activeNodeIds = null;
  }

  function isOnline(nodeId: string): boolean {
    return onlineStatus.value[nodeId] ?? false;
  }

  return { onlineStatus, startProbing, stopProbing, isOnline };
}
