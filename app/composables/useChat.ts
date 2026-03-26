import { ref, onMounted, onUnmounted } from "vue";

export interface ChatMessage {
  id: string;
  sender: "you" | "peer";
  peerId?: string;
  text: string;
  timestamp: number;
}

export function useChat() {
  const messages = ref<ChatMessage[]>([]);
  let unlistener: (() => void) | null = null;

  onMounted(async () => {
    try {
      const { listen } = await import("@tauri-apps/api/event");
      unlistener = await listen<any>("chat-received", (event) => {
        const data = event.payload;
        messages.value.push({
          id: crypto.randomUUID(),
          sender: "peer",
          peerId: data.peer_id,
          text: data.message,
          timestamp: Date.now(),
        });
      });
    } catch {}
  });

  onUnmounted(() => { unlistener?.(); });

  async function sendMessage(peerId: string, text: string) {
    if (!text.trim()) return;
    try {
      const { invoke } = await import("@tauri-apps/api/core");
      await invoke("send_chat", { peerId, message: text });
      messages.value.push({
        id: crypto.randomUUID(),
        sender: "you",
        text,
        timestamp: Date.now(),
      });
    } catch (e) {
      console.error("Failed to send chat:", e);
    }
  }

  async function sendMessageToAll(peerIds: string[], text: string) {
    if (!text.trim() || peerIds.length === 0) return;
    try {
      const { invoke } = await import("@tauri-apps/api/core");
      await Promise.all(peerIds.map((pid) => invoke("send_chat", { peerId: pid, message: text })));
      messages.value.push({
        id: crypto.randomUUID(),
        sender: "you",
        text,
        timestamp: Date.now(),
      });
    } catch (e) {
      console.error("Failed to send chat:", e);
    }
  }

  function clearMessages() { messages.value = []; }

  return { messages, sendMessage, sendMessageToAll, clearMessages };
}
