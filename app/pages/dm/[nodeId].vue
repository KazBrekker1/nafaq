<script setup lang="ts">
import type { DmMessageItem } from "~/composables/useDM";
import { formatTime } from "~/utils/format";

const route = useRoute();
const peerId = computed(() => route.params.nodeId as string);

const {
  conversations, connect, connectErrors, clearActiveConversation, sendText, resend, sendFile, markRead,
} = useDM();
const { contacts, add: addContact, displayName: resolveDisplayName } = useContacts();
const { isOnline, startProbing, stopProbing } = usePresence();
const { createCall, error: callError, state: callState, startWaitingForAnswer } = useCall();

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
  await sendText(peerId.value, text).catch((error) => {
    console.warn("[dm] Send failed:", error);
  });
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
  // Create a call first, then send the ticket via DM.
  const t = await createCall();
  if (!t) {
    console.warn("[dm] Call invite not sent:", callError.value || "ticket unavailable");
    return;
  }
  try {
    await invoke("send_dm", {
      peerId: peerId.value,
      message: { type: "call_invite", ticket: t },
    });
  } catch (e) {
    // The callee never received the invite — navigating to /call would just
    // wait forever. Reset the half-created call and surface the failure.
    console.warn("[dm] Call invite delivery failed:", e);
    callError.value = "Could not deliver the call invite. Check the connection and try again.";
    callState.value = "idle";
    return;
  }
  startWaitingForAnswer(peerId.value);
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

onUnmounted(() => {
  stopProbing();
  window.visualViewport?.removeEventListener("resize", onViewportResize);
  clearActiveConversation();
});
</script>

<template>
  <div class="flex flex-col bg-default safe-area-inset" :style="{ height: viewportHeight }">

    <!-- Header -->
    <div class="sticky top-0 z-10 flex shrink-0 items-center gap-3 border-b-2 border-(--ui-border-accented) bg-default px-5 py-4">
      <UButton
        icon="i-heroicons-arrow-left"
        variant="ghost"
        color="neutral"
        size="xs"
        aria-label="Back"
        @click="() => { navigateTo('/messages') }"
      />

      <!-- Name + online status -->
      <div class="flex min-w-0 flex-1 items-center gap-2">
        <span class="truncate text-sm font-bold text-highlighted">{{ contactName }}</span>
        <span
          class="inline-block h-2 w-2 shrink-0 rounded-full"
          :class="online ? 'bg-success' : 'bg-accented'"
          :title="online ? 'online' : 'offline'"
        />
        <span class="shrink-0 text-[10px] text-dimmed">{{ online ? "online" : "offline" }}</span>
      </div>

      <!-- Add contact button -->
      <UButton
        v-if="!isContact"
        icon="i-heroicons-user-plus"
        label="ADD"
        variant="subtle"
        color="primary"
        size="xs"
        class="shrink-0"
        @click="handleAddContact"
      />

      <!-- Call button -->
      <UButton
        icon="i-heroicons-phone"
        label="CALL"
        variant="subtle"
        color="neutral"
        size="xs"
        class="shrink-0"
        @click="initiateCall"
      />
    </div>

    <!-- Connect error banner -->
    <div
      v-if="connectErrors[peerId]"
      class="shrink-0 border-b-2 border-(--ui-border-accented) bg-elevated px-5 py-2 text-[9px] tracking-widest text-dimmed"
    >
      CAN'T REACH PEER — MESSAGES WILL RETRY
    </div>

    <!-- Message list -->
    <div ref="messages-el" class="flex-1 overflow-y-auto px-5 py-5 space-y-5">

      <UEmpty
        v-if="messages.length === 0"
        icon="i-heroicons-chat-bubble-left-right"
        title="No messages yet"
        description="Say hello!"
        class="h-full justify-center border-0 shadow-none"
      />

      <div
        v-for="(msg, idx) in messages"
        :key="idx"
        class="flex flex-col"
        :class="msg.from === 'self' ? 'items-end' : 'items-start'"
      >
        <!-- Sender label + time -->
        <div
          class="mb-1 flex items-center gap-2"
          :class="msg.from === 'self' ? 'flex-row-reverse' : ''"
        >
          <span class="text-[9px] font-bold tracking-widest" :class="msg.from === 'self' ? 'text-primary' : 'text-dimmed'">
            {{ msg.from === "self" ? "YOU" : contactName.toUpperCase() }}
          </span>
          <span class="text-[9px] text-dimmed">{{ formatTime(msg.timestamp) }}</span>
        </div>

        <!-- Text message -->
        <div
          v-if="msg.type === 'text'"
          class="select-text max-w-[75%] border-2 px-3 py-2 text-xs"
          :class="msg.from === 'self'
            ? 'border-(--ui-border-accented) bg-primary text-inverted'
            : 'border-(--ui-border-accented) bg-elevated text-highlighted'"
        >
          {{ msg.content }}
        </div>
        <button
          v-if="msg.type === 'text' && msg.from === 'self' && msg.status === 'failed'"
          class="mt-1 text-[9px] tracking-widest text-error hover:underline"
          title="Tap to resend"
          @click="resend(peerId, msg)"
        >
          FAILED · TAP TO RESEND
        </button>
        <div
          v-else-if="msg.type === 'text' && msg.from === 'self' && msg.status === 'sending'"
          class="mt-1 text-[9px] tracking-widest text-dimmed"
        >
          SENDING…
        </div>
        <div
          v-else-if="msg.type === 'text' && msg.from === 'self' && msg.status === 'delivered'"
          class="mt-1 text-[9px] tracking-widest text-dimmed"
        >
          ✓✓ DELIVERED
        </div>
        <div
          v-else-if="msg.type === 'text' && msg.from === 'self' && msg.status === 'sent'"
          class="mt-1 text-[9px] tracking-widest text-dimmed"
        >
          ✓ SENT
        </div>

        <!-- File message -->
        <FileMessage
          v-else-if="msg.type === 'file'"
          :name="msg.name"
          :size="msg.size"
          :progress="msg.progress"
          :local-path="msg.localPath"
          :from="msg.from"
          :failed="msg.failed"
        />
      </div>

    </div>

    <!-- Input bar -->
    <div class="flex shrink-0 items-center gap-2 border-t-2 border-(--ui-border-accented) bg-default px-4 py-3">

      <!-- Attach button -->
      <UButton
        icon="i-heroicons-paper-clip"
        variant="ghost"
        color="neutral"
        size="xs"
        title="Attach file"
        aria-label="Attach file"
        @click="openFilePicker"
      />

      <!-- Text input -->
      <UInput
        v-model="inputText"
        placeholder="Type a message..."
        class="flex-1"
        @keydown.enter="send"
      />

      <!-- Send button -->
      <UButton
        icon="i-heroicons-paper-airplane"
        variant="solid"
        color="primary"
        size="xs"
        class="shrink-0"
        :disabled="!inputText.trim()"
        aria-label="Send"
        @click="send"
      />
    </div>

  </div>
</template>
