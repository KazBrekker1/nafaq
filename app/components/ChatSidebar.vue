<script setup lang="ts">
import type { ChatMessage } from "~/composables/useChat";
import { formatTime } from "~/utils/format";

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

onMounted(() => {
  if (window.visualViewport) {
    const handler = () => {
      const vh = window.visualViewport!.height;
      document.documentElement.style.setProperty("--vh", `${vh}px`);
    };
    window.visualViewport.addEventListener("resize", handler);
    handler(); // Initial set
    onUnmounted(() => {
      window.visualViewport?.removeEventListener("resize", handler);
      document.documentElement.style.removeProperty("--vh");
    });
  }
});
</script>

<template>
  <!--
    Forced-dark panel: this sidebar only ever overlays the always-dark
    in-call video screen (app/pages/call.vue), so it intentionally does not
    follow the light/dark theme — it stays dark to match its host regardless
    of app color mode.
  -->
  <div
    class="fixed inset-0 z-30 flex w-full flex-col border-l-0 border-neutral-800 bg-neutral-950 safe-area-inset sm:static sm:inset-auto sm:w-[260px] sm:border-l-2"
    :style="{ height: 'var(--vh, 100dvh)' }"
  >
    <div class="flex items-center justify-between border-b-2 border-neutral-800 px-4 py-4">
      <span class="label">MESSAGES</span>
      <UButton
        icon="i-heroicons-x-mark"
        variant="ghost"
        color="neutral"
        size="xs"
        class="sm:hidden text-neutral-400 hover:text-neutral-100"
        aria-label="Close"
        @click="emit('close')"
      />
    </div>

    <div ref="messagesEl" class="flex-1 overflow-y-auto">
      <div v-for="msg in messages" :key="msg.id"
        class="border-b border-neutral-800 px-5 py-3"
        :class="msg.sender === 'you' ? 'bg-neutral-900' : ''">
        <span class="text-[9px] tracking-widest" :class="msg.sender === 'you' ? 'text-primary' : 'text-neutral-500'">
          {{ msg.sender === "you" ? (displayName || "You") : (peerNames[msg.peerId || ""] || "Peer") }} · {{ formatTime(msg.timestamp) }}
        </span><br />
        <span class="mt-1 block text-xs text-neutral-200">{{ msg.text }}</span>
      </div>
      <div v-if="messages.length === 0" class="p-6 text-center text-xs text-neutral-500">No messages yet</div>
    </div>

    <div class="border-t-2 border-neutral-800 p-2">
      <UInput
        v-model="input"
        placeholder="Type a message..."
        variant="none"
        class="text-xs text-neutral-100 placeholder:text-neutral-500"
        @keyup.enter="submit"
      />
    </div>
  </div>
</template>
