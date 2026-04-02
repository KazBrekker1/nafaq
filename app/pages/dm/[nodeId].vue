<script setup lang="ts">
import type { DmMessageItem } from "../../composables/useDM";

const route = useRoute();
const peerId = computed(() => route.params.nodeId as string);

const { conversations, connect, disconnect, sendText, sendFile, markRead } = useDM();
const { contacts } = useContacts();
const { isOnline } = usePresence();
const { joinCall } = useCall();

// ── Contact info ──────────────────────────────────────────

const contactName = computed(() => {
  const contact = contacts.value.find(c => c.node_id === peerId.value);
  if (contact?.display_name) return contact.display_name;
  const id = peerId.value || "";
  return `${id.slice(0, 4)}…${id.slice(-4)}`;
});

const online = computed(() => isOnline(peerId.value));

// ── Messages ──────────────────────────────────────────────

const messages = computed<DmMessageItem[]>(() => conversations.value[peerId.value] ?? []);

// ── Scroll to bottom ──────────────────────────────────────

const messagesEl = ref<HTMLElement | null>(null);

async function scrollToBottom() {
  await nextTick();
  if (messagesEl.value) {
    messagesEl.value.scrollTop = messagesEl.value.scrollHeight;
  }
}

watch(() => messages.value.length, scrollToBottom);

// ── Text input ────────────────────────────────────────────

const inputText = ref("");

async function send() {
  const text = inputText.value.trim();
  if (!text) return;
  inputText.value = "";
  await sendText(peerId.value, text);
}

// ── File attach ───────────────────────────────────────────

const fileInputEl = ref<HTMLInputElement | null>(null);

async function openFilePicker() {
  try {
    const { open } = await import("@tauri-apps/plugin-dialog");
    const path = await open({ multiple: false });
    if (path) await sendFile(peerId.value, path as string);
  } catch {
    // Fallback to hidden file input if plugin not available
    fileInputEl.value?.click();
  }
}

async function onFileInputChange(e: Event) {
  const input = e.target as HTMLInputElement;
  const file = input.files?.[0];
  if (!file) return;
  // In Tauri, file.name gives the name; for path we use webkitRelativePath or name
  // Best-effort: send the file name as path (backend may need real path)
  const path = (file as any).path || file.name;
  await sendFile(peerId.value, path);
  input.value = "";
}

// ── Call escalation ───────────────────────────────────────

async function initiateCall() {
  // Send a call_invite DM then join
  await sendText(peerId.value, "[call_invite]").catch(() => {});
  await joinCall(peerId.value);
  navigateTo("/");
}

// ── Timestamp formatting ──────────────────────────────────

function formatTime(ts: number): string {
  return new Date(ts).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
}

// ── Lifecycle ─────────────────────────────────────────────

onMounted(async () => {
  await connect(peerId.value);
  markRead(peerId.value);
  await scrollToBottom();
});

onUnmounted(async () => {
  await disconnect();
});
</script>

<template>
  <div class="h-screen flex flex-col bg-[var(--color-surface)] safe-area-inset">

    <!-- Header -->
    <div class="border-b-2 border-[var(--color-border)] px-4 py-3 flex items-center gap-3 shrink-0 sticky top-0 bg-[var(--color-surface)] z-10">
      <button
        class="text-[var(--color-muted)] hover:text-[var(--color-border)] transition-colors flex items-center gap-1.5"
        aria-label="Back"
        @click="navigateTo('/messages')"
      >
        <UIcon name="i-heroicons-arrow-left" class="text-base" />
      </button>

      <!-- Name + online status -->
      <div class="flex items-center gap-2 flex-1 min-w-0">
        <span class="text-sm font-bold text-[var(--color-border)] font-mono truncate">{{ contactName }}</span>
        <span
          class="shrink-0 inline-block w-2 h-2 rounded-full"
          :style="online ? 'background:#4ade80' : 'background:#555'"
          :title="online ? 'online' : 'offline'"
        />
        <span class="text-[10px] text-[var(--color-muted)] shrink-0">{{ online ? "online" : "offline" }}</span>
      </div>

      <!-- Call button -->
      <button
        class="border-2 border-[var(--color-border)] px-3 py-1.5 text-[10px] font-bold tracking-widest hover:bg-[var(--color-border)] hover:text-black transition-colors shrink-0"
        @click="initiateCall"
      >
        ☎ CALL
      </button>
    </div>

    <!-- Message list -->
    <div ref="messagesEl" class="flex-1 overflow-y-auto px-4 py-4 space-y-4">

      <div v-if="messages.length === 0" class="flex items-center justify-center h-full">
        <p class="text-xs text-[var(--color-muted)] text-center">No messages yet.<br />Say hello!</p>
      </div>

      <div
        v-for="(msg, idx) in messages"
        :key="idx"
        class="flex flex-col"
        :class="msg.from === 'self' ? 'items-end' : 'items-start'"
      >
        <!-- Sender label + time -->
        <div
          class="flex items-center gap-2 mb-1"
          :class="msg.from === 'self' ? 'flex-row-reverse' : ''"
        >
          <span class="text-[9px] font-bold tracking-widest font-mono" :class="msg.from === 'self' ? 'text-[var(--color-accent)]' : 'text-[var(--color-muted)]'">
            {{ msg.from === "self" ? "YOU" : contactName.toUpperCase() }}
          </span>
          <span class="text-[9px] text-[var(--color-muted)] font-mono">{{ formatTime(msg.timestamp) }}</span>
        </div>

        <!-- Text message -->
        <div
          v-if="msg.type === 'text'"
          class="max-w-[75%] px-3 py-2 text-xs text-[var(--color-border)] font-mono"
          :class="msg.from === 'self'
            ? 'bg-[var(--color-surface-alt)] border-2 border-[var(--color-border)]'
            : 'border-l-2 border-[var(--color-accent)] pl-3'"
        >
          {{ msg.content }}
        </div>

        <!-- File message -->
        <FileMessage
          v-else-if="msg.type === 'file'"
          :name="msg.name"
          :size="msg.size"
          :progress="msg.progress"
          :local-path="msg.localPath"
          :from="msg.from"
        />
      </div>

    </div>

    <!-- Input bar -->
    <div class="border-t-2 border-[var(--color-border)] px-3 py-2 flex items-center gap-2 shrink-0 bg-[var(--color-surface)]">

      <!-- Attach button -->
      <button
        class="shrink-0 text-[var(--color-muted)] hover:text-[var(--color-border)] transition-colors text-base"
        title="Attach file"
        @click="openFilePicker"
      >
        <UIcon name="i-heroicons-paper-clip" class="text-lg" />
      </button>

      <!-- Hidden fallback file input -->
      <input
        ref="fileInputEl"
        type="file"
        class="hidden"
        @change="onFileInputChange"
      />

      <!-- Text input -->
      <input
        v-model="inputText"
        type="text"
        class="flex-1 bg-black border-2 border-[var(--color-border)] px-3 py-2 text-xs text-[var(--color-border)] font-mono outline-none focus:border-[var(--color-accent)] transition-colors"
        placeholder="Type a message..."
        @keydown.enter="send"
      />

      <!-- Send button -->
      <button
        class="shrink-0 border-2 border-[var(--color-border)] px-3 py-2 text-xs font-bold tracking-widest hover:bg-[var(--color-border)] hover:text-black transition-colors"
        :class="inputText.trim() ? 'opacity-100' : 'opacity-40'"
        :disabled="!inputText.trim()"
        @click="send"
      >
        →
      </button>
    </div>

  </div>
</template>
