import type { Contact } from "./useContacts";
import type { Ref } from "vue";

const onlineStatus = ref<Record<string, boolean>>({});

export function usePresence() {
  let probeInterval: ReturnType<typeof setInterval> | null = null;

  async function probeAll(contacts: Contact[]) {
    const { invoke } = await import("@tauri-apps/api/core");
    for (const contact of contacts) {
      const online = await invoke<boolean>("check_presence", { nodeId: contact.node_id }).catch(() => false);
      onlineStatus.value = { ...onlineStatus.value, [contact.node_id]: online };
    }
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
