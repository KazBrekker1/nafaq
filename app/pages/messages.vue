<script setup lang="ts">
const { conversations, unreadCounts } = useDM();
const { displayName } = useContacts();

function lastMessage(nodeId: string) {
  const msgs = conversations.value[nodeId];
  if (!msgs || msgs.length === 0) return null;
  return msgs[msgs.length - 1];
}

function lastMessagePreview(nodeId: string): string {
  const msg = lastMessage(nodeId);
  if (!msg) return "";
  if (msg.type === "text") {
    return msg.content.length > 50 ? msg.content.slice(0, 50) + "…" : msg.content;
  }
  return `[File] ${msg.name}`;
}

function lastMessageTime(nodeId: string): string {
  const msg = lastMessage(nodeId);
  if (!msg) return "";
  const date = new Date(msg.timestamp);
  const now = new Date();
  const diffMs = now.getTime() - date.getTime();
  const diffMins = Math.floor(diffMs / 60_000);
  const diffHours = Math.floor(diffMins / 60);
  if (diffMins < 1) return "now";
  if (diffMins < 60) return `${diffMins}m`;
  if (diffHours < 24) return `${String(date.getHours()).padStart(2, "0")}:${String(date.getMinutes()).padStart(2, "0")}`;
  return date.toLocaleDateString([], { month: "short", day: "numeric" });
}

// ── Conversation list sorted by last message time ─────────

const sortedConversations = computed(() => {
  return Object.keys(conversations.value).sort((a, b) => {
    const msgA = lastMessage(a);
    const msgB = lastMessage(b);
    if (!msgA) return 1;
    if (!msgB) return -1;
    return msgB.timestamp - msgA.timestamp;
  });
});
</script>

<template>
  <div class="min-h-full bg-[var(--color-surface)] safe-area-inset-min">

    <!-- Header -->
    <div class="border-b border-[var(--color-border-muted)] px-4 py-3 sticky top-0 bg-[var(--color-surface)] z-10">
      <h1 class="label text-[var(--color-border)]" style="letter-spacing: 4px;">MESSAGES</h1>
    </div>

    <div class="max-w-xl mx-auto">

      <!-- Empty state -->
      <div
        v-if="sortedConversations.length === 0"
        class="px-4 py-16 text-center"
      >
        <p class="label text-[var(--color-muted)]">NO CONVERSATIONS</p>
        <p class="text-xs text-[var(--color-muted)] mt-2">Start a DM from the Contacts page.</p>
      </div>

      <!-- Conversation rows -->
      <div
        v-for="nodeId in sortedConversations"
        :key="nodeId"
        class="border-b border-[var(--color-border-muted)] px-4 py-3 flex items-center gap-3 cursor-pointer hover:bg-[var(--color-surface-alt)] transition-colors"
        @click="navigateTo('/dm/' + nodeId)"
      >
        <!-- Unread dot -->
        <div class="shrink-0 w-2 h-2 rounded-full" :class="(unreadCounts[nodeId] ?? 0) > 0 ? 'bg-[var(--color-accent)]' : 'bg-transparent'" />

        <!-- Name + preview -->
        <div class="flex-1 min-w-0">
          <div class="flex items-center justify-between gap-2">
            <span class="text-sm font-bold text-[var(--color-border)] font-mono truncate">
              {{ displayName(nodeId) }}
            </span>
            <span class="text-[10px] text-[var(--color-muted)] shrink-0">{{ lastMessageTime(nodeId) }}</span>
          </div>
          <div class="flex items-center justify-between gap-2 mt-0.5">
            <p class="text-xs text-[var(--color-muted)] truncate">{{ lastMessagePreview(nodeId) }}</p>
            <span
              v-if="(unreadCounts[nodeId] ?? 0) > 0"
              class="shrink-0 text-[9px] font-bold bg-[var(--color-accent)] text-white px-1.5 py-0.5 font-mono"
            >
              {{ unreadCounts[nodeId] }}
            </span>
          </div>
        </div>

        <!-- Chevron -->
        <UIcon name="i-heroicons-chevron-right" class="text-[var(--color-muted)] text-base shrink-0" />
      </div>

    </div>
  </div>
</template>
