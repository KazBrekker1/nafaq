import type { Contact } from "./useContacts";
import type { Ref } from "vue";

const onlineStatus = ref<Record<string, boolean>>({});

export function usePresence() {
  let probeInterval: ReturnType<typeof setInterval> | null = null;

  async function probeAll(contacts: Contact[]) {
    const { invoke } = await import("@tauri-apps/api/core");
    const results = await Promise.allSettled(
      contacts.map(async (contact) => {
        const online = await invoke<boolean>("check_presence", { nodeId: contact.node_id }).catch(() => false);
        return { nodeId: contact.node_id, online };
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

  function startProbing(contacts: Ref<Contact[]>) {
    probeAll(contacts.value);
    probeInterval = setInterval(() => probeAll(contacts.value), 30_000);
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
