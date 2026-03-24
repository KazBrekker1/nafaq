<script setup lang="ts">
import { ref, nextTick, watch } from "vue";
import type { ChatMessage } from "../composables/useChat";

const props = defineProps<{
  messages: ChatMessage[];
  peerId: string;
}>();

const emit = defineEmits<{
  send: [text: string];
}>();

const input = ref("");
const messagesEl = ref<HTMLElement | null>(null);

function submit() {
  const text = input.value.trim();
  if (!text) return;
  emit("send", text);
  input.value = "";
}

watch(
  () => props.messages.length,
  async () => {
    await nextTick();
    if (messagesEl.value) {
      messagesEl.value.scrollTop = messagesEl.value.scrollHeight;
    }
  },
);
</script>

<template>
  <div class="w-[260px] bg-black border-l-2 border-[var(--color-border)] flex flex-col">
    <div class="p-3 border-b-2 border-[var(--color-border-muted)]">
      <span class="label">MESSAGES</span>
    </div>

    <div ref="messagesEl" class="flex-1 overflow-y-auto">
      <div
        v-for="msg in messages"
        :key="msg.id"
        class="px-4 py-2.5 border-b border-[#1a1a1a]"
        :class="msg.sender === 'you' ? 'bg-[var(--color-surface-alt)]' : ''"
      >
        <span
          class="text-[9px] tracking-widest"
          :style="{ color: msg.sender === 'you' ? 'var(--color-accent)' : 'var(--color-muted)' }"
        >
          {{ msg.sender === "you" ? "You" : "Peer" }} · {{ new Date(msg.timestamp).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" }) }}
        </span>
        <br />
        <span class="text-xs">{{ msg.text }}</span>
      </div>

      <div v-if="messages.length === 0" class="p-4 text-center text-[var(--color-muted)] text-xs">
        No messages yet
      </div>
    </div>

    <div class="border-t-2 border-[var(--color-border-muted)]">
      <input
        v-model="input"
        class="input border-0 text-xs py-3.5 px-4"
        placeholder="Type a message..."
        @keyup.enter="submit"
      />
    </div>
  </div>
</template>
