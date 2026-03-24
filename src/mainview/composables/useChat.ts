import { ref, inject, onMounted, onUnmounted } from "vue";
import type { Event } from "../../shared/types";

export interface ChatMessage {
  id: string;
  sender: "you" | "peer";
  peerId?: string;
  text: string;
  timestamp: number;
}

export function useChat() {
  const nafaq = inject<any>("nafaq");
  const messages = ref<ChatMessage[]>([]);

  let unsubEvent: (() => void) | undefined;

  onMounted(() => {
    unsubEvent = nafaq?.onEvent((event: Event) => {
      if (event.type === "chat_received") {
        messages.value.push({
          id: crypto.randomUUID(),
          sender: "peer",
          peerId: event.peer_id,
          text: event.message,
          timestamp: Date.now(),
        });
      }
    });
  });

  onUnmounted(() => {
    unsubEvent?.();
  });

  async function sendMessage(peerId: string, text: string) {
    if (!text.trim()) return;
    await nafaq?.sendChat(peerId, text);
    messages.value.push({
      id: crypto.randomUUID(),
      sender: "you",
      text,
      timestamp: Date.now(),
    });
  }

  function clearMessages() {
    messages.value = [];
  }

  return { messages, sendMessage, clearMessages };
}
