<script setup lang="ts">
import type { DmMessageItem } from "~/composables/useDM";
import { formatTime } from "~/utils/format";

const route = useRoute();
const peerId = computed(() => route.params.nodeId as string);

const { conversations, connect, disconnect, sendText, sendFile, markRead } = useDM();
const { contacts, add: addContact, displayName: resolveDisplayName } = useContacts();
const { isOnline, startProbing, stopProbing } = usePresence();
const { createCall, shareTicket } = useCall();

const isContact = computed(() => contacts.value.some(c => c.node_id === peerId.value));
const contactName = computed(() => resolveDisplayName(peerId.value));

async function handleAddContact() {
  await addContact({
    node_id: peerId.value,
    display_name: contactName.value,
    added_at: Date.now(),
    last_seen: Date.now(),
    source: "manual",
  });
}

const online = computed(() => isOnline(peerId.value));

// ── Messages ──────────────────────────────────────────────

const messages = computed<DmMessageItem[]>(() => conversations.value[peerId.value] ?? []);

// ── Scroll to bottom ──────────────────────────────────────

const messagesEl = useTemplateRef<HTMLElement>("messages-el");

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

async function openFilePicker() {
  try {
    const { open } = await import("@tauri-apps/plugin-dialog");
    const path = await open({ multiple: false });
    if (path) await sendFile(peerId.value, path as string);
  } catch (e) {
    console.warn("[dm] File picker failed:", e);
  }
}

// ── Call escalation ───────────────────────────────────────

async function initiateCall() {
  const { invoke } = await import("@tauri-apps/api/core");
  // Create a call first, then send the ticket via DM
  await createCall();
  const t = shareTicket.value;
  if (t) {
    await invoke("send_dm", {
      peerId: peerId.value,
      message: { type: "call_invite", ticket: t },
    }).catch(() => {});
  }
  navigateTo("/call");
}

// ── Keyboard-aware viewport ──────────────────────────────

const viewportHeight = ref("100%");

function onViewportResize() {
  if (window.visualViewport) {
    viewportHeight.value = `${window.visualViewport.height}px`;
    scrollToBottom();
  }
}

// ── Lifecycle ─────────────────────────────────────────────

const peerIds = computed(() => [peerId.value]);

onMounted(async () => {
  await connect(peerId.value);
  markRead(peerId.value);
  await scrollToBottom();
  startProbing(peerIds);
  window.visualViewport?.addEventListener("resize", onViewportResize);
});

onUnmounted(async () => {
  stopProbing();
  window.visualViewport?.removeEventListener("resize", onViewportResize);
  await disconnect();
});
</script>

<template>
  <div class="flex flex-col bg-[var(--color-surface)] safe-area-inset" :style="{ height: viewportHeight }">

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
          :style="online ? 'background:var(--color-online)' : 'background:var(--color-muted)'"
          :title="online ? 'online' : 'offline'"
        />
        <span class="text-[10px] text-[var(--color-muted)] shrink-0">{{ online ? "online" : "offline" }}</span>
      </div>

      <!-- Add contact button -->
      <button
        v-if="!isContact"
        class="border-2 border-[var(--color-accent)] px-3 py-1.5 text-[10px] font-bold tracking-widest text-[var(--color-accent)] hover:bg-[var(--color-accent)] hover:text-white transition-colors shrink-0"
        @click="handleAddContact"
      >
        + ADD
      </button>

      <!-- Call button -->
      <button
        class="border-2 border-[var(--color-border)] px-3 py-1.5 text-[10px] font-bold tracking-widest hover:bg-[var(--color-border)] hover:text-black transition-colors shrink-0"
        @click="initiateCall"
      >
        ☎ CALL
      </button>
    </div>

    <!-- Message list -->
    <div ref="messages-el" class="flex-1 overflow-y-auto px-4 py-4 space-y-4">

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
          class="select-text max-w-[75%] px-3 py-2 text-xs text-[var(--color-border)] font-mono"
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

      <!-- Text input -->
      <UInput
        v-model="inputText"
        placeholder="Type a message..."
        class="flex-1 rounded-none font-mono text-xs"
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
