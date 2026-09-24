import { invoke } from "@tauri-apps/api/core";
import { truncateNodeId } from "~/utils/format";

export interface Contact {
  node_id: string;
  display_name: string;
  added_at: number;
  last_seen: number;
  source: "call" | "manual";
}

const contacts = ref<Contact[]>([]);
// Startup load, shared by the many useContacts() callers. Reset on failure so
// the next caller retries.
let initialLoad: Promise<void> | null = null;
// Bumped by every write to `contacts`, so a slower, older get_contacts
// response can never overwrite a newer list.
let revision = 0;

async function fetchContacts(): Promise<boolean> {
  const token = ++revision;
  try {
    const list = await invoke<Contact[]>("get_contacts");
    if (token === revision) contacts.value = list;
    return true;
  } catch (e) {
    console.warn("[contacts] get_contacts failed:", e);
    return false;
  }
}

// add_contact/remove_contact return the updated list; fall back to a fresh
// fetch (never a possibly-stale in-flight one) if a backend doesn't.
async function applyMutationResult(result: unknown) {
  if (Array.isArray(result)) {
    revision += 1;
    contacts.value = result as Contact[];
  } else {
    await fetchContacts();
  }
}

export function useContacts() {
  async function add(contact: Contact) {
    await applyMutationResult(await invoke<Contact[] | null>("add_contact", { contact }));
  }

  async function remove(nodeId: string) {
    try {
      await applyMutationResult(await invoke<Contact[] | null>("remove_contact", { nodeId }));
    } catch (e) {
      // Backend still has the contact; reload instead of showing a ghost removal.
      console.warn("[contacts] remove failed:", e);
      await fetchContacts();
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

  initialLoad ??= fetchContacts().then((ok) => {
    if (!ok) initialLoad = null;
  });

  return { contacts, add, remove, starFromCall, displayName };
}
