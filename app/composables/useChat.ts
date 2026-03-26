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

  async function sendMessageToAll(text: string) {
    if (!text.trim()) return;
    try {
      const { invoke } = await import("@tauri-apps/api/core");
      const failedPeerIds = await invoke<string[]>("send_chat_all", { message: text });
      messages.value.push({
        id: crypto.randomUUID(),
        sender: "you",
        text,
        timestamp: Date.now(),
      });
      if (failedPeerIds.length > 0) {
        console.warn("Chat delivery failed for peers:", failedPeerIds);
      }
    } catch (e) {
      console.error("Failed to send chat:", e);
    }
  }

  function clearMessages() { messages.value = []; }

  return { messages, sendMessage, sendMessageToAll, clearMessages };
}
