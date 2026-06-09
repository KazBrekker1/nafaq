import { truncateNodeId } from "~/utils/format";

export interface Contact {
  node_id: string;
  display_name: string;
  added_at: number;
  last_seen: number;
  source: "call" | "manual";
}

const contacts = ref<Contact[]>([]);
const loaded = ref(false);
let loadPromise: Promise<void> | null = null;

export function useContacts() {
  function load(): Promise<void> {
    // Cache the in-flight promise: several composables call useContacts() at
    // startup and would otherwise each fire their own get_contacts invoke.
    if (!loadPromise) {
      loadPromise = (async () => {
        const { invoke } = await import("@tauri-apps/api/core");
        contacts.value = await invoke<Contact[]>("get_contacts").catch(() => []);
        loaded.value = true;
      })().finally(() => {
        loadPromise = null;
      });
    }
    return loadPromise;
  }

  async function add(contact: Contact) {
    const { invoke } = await import("@tauri-apps/api/core");
    await invoke("add_contact", { contact });
    await load(); // Refresh from store
  }

  async function remove(nodeId: string) {
    const { invoke } = await import("@tauri-apps/api/core");
    try {
      await invoke("remove_contact", { nodeId });
      contacts.value = contacts.value.filter(c => c.node_id !== nodeId);
    } catch (e) {
      // Backend still has the contact; reload instead of showing a ghost removal.
      console.warn("[contacts] remove failed:", e);
      await load();
    }
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

  function displayName(nodeId: string): string {
    const contact = contacts.value.find(c => c.node_id === nodeId);
    if (contact?.display_name) return contact.display_name;
    return truncateNodeId(nodeId);
  }

  if (!loaded.value) void load();

  return { contacts, loaded, add, remove, starFromCall, displayName };
}
