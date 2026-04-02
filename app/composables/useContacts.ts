export interface Contact {
  node_id: string;
  display_name: string;
  added_at: number;
  last_seen: number;
  source: "call" | "manual";
}

const contacts = ref<Contact[]>([]);
const loaded = ref(false);

export function useContacts() {
  async function load() {
    const { invoke } = await import("@tauri-apps/api/core");
    contacts.value = await invoke<Contact[]>("get_contacts").catch(() => []);
    loaded.value = true;
  }

  async function add(contact: Contact) {
    const { invoke } = await import("@tauri-apps/api/core");
    await invoke("add_contact", { contact });
    await load(); // Refresh from store
  }

  async function remove(nodeId: string) {
    const { invoke } = await import("@tauri-apps/api/core");
    await invoke("remove_contact", { nodeId });
    contacts.value = contacts.value.filter(c => c.node_id !== nodeId);
  }

  async function starFromCall(nodeId: string, displayName: string) {
    await add({
      node_id: nodeId,
      display_name: displayName,
      added_at: Date.now(),
      last_seen: Date.now(),
      source: "call",
    });
  }

  if (!loaded.value) load();

  return { contacts, loaded, add, remove, starFromCall };
}
