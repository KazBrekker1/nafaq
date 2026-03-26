<script setup lang="ts">
import type { ChatMessage } from "../composables/useChat";

const { messages, displayName = "", peerNames = {} } = defineProps<{
  messages: ChatMessage[];
  peerId: string;
  displayName?: string;
  peerNames?: Record<string, string>;
}>();
const emit = defineEmits<{ send: [text: string]; close: [] }>();

const input = ref("");
const messagesEl = ref<HTMLElement | null>(null);

function submit() {
  const text = input.value.trim();
  if (!text) return;
  emit("send", text);
  input.value = "";
}

watch(() => messages.length, async () => {
  await nextTick();
  if (messagesEl.value) messagesEl.value.scrollTop = messagesEl.value.scrollHeight;
});
</script>

<template>
  <div
    class="fixed inset-0 sm:static sm:inset-auto w-full sm:w-[260px] bg-black border-l-0 sm:border-l-2 border-[var(--color-border)] flex flex-col z-30 safe-area-inset"
  >
    <div class="px-4 py-4 border-b-2 border-[var(--color-border-muted)] flex items-center justify-between">
      <span class="label">MESSAGES</span>
      <button class="sm:hidden text-[var(--color-muted)] hover:text-white" @click="emit('close')">
        <UIcon name="i-heroicons-x-mark" class="text-lg" />
      </button>
    </div>

    <div ref="messagesEl" class="flex-1 overflow-y-auto">
      <div v-for="msg in messages" :key="msg.id"
        class="px-5 py-3 border-b border-[var(--color-border-muted)]"
        :class="msg.sender === 'you' ? 'bg-[var(--color-surface-alt)]' : ''">
        <span class="text-[9px] tracking-widest"
          :style="{ color: msg.sender === 'you' ? 'var(--color-accent)' : 'var(--color-muted)' }">
          {{ msg.sender === "you" ? (displayName || "You") : (peerNames[msg.peerId || ""] || "Peer") }} · {{ new Date(msg.timestamp).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" }) }}
        </span><br />
        <span class="text-xs mt-1 block">{{ msg.text }}</span>
      </div>
      <div v-if="messages.length === 0" class="p-6 text-center text-[var(--color-muted)] text-xs">No messages yet</div>
    </div>

    <div class="border-t-2 border-[var(--color-border-muted)] p-2">
      <UInput v-model="input" placeholder="Type a message..." class="rounded-none border-0 text-xs" @keyup.enter="submit" />
    </div>
  </div>
</template>
