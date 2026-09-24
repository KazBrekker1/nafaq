import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

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
  let unmounted = false;

  onMounted(async () => {
    try {
      const unlisten = await listen<{ peer_id: string; message: string }>("chat-received", (event) => {
        const data = event.payload;
        messages.value.push({
          id: crypto.randomUUID(),
          sender: "peer",
          peerId: data.peer_id,
          text: data.message,
          timestamp: Date.now(),
        });
      });
      // Unmounted while listen() was pending: release it right away.
      if (unmounted) unlisten();
      else unlistener = unlisten;
    } catch (e) {
      console.warn("[chat] listen failed:", e);
    }
  });

  onUnmounted(() => {
    unmounted = true;
    unlistener?.();
    unlistener = null;
  });

  async function sendMessage(peerId: string, text: string) {
    if (!text.trim()) return;
    try {
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
