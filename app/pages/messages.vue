<script setup lang="ts">
import { formatRelativeTime } from "~/utils/format";

const { conversations, unreadCounts } = useDM();
const { displayName } = useContacts();
const now = useNow({ interval: 60_000 });

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
  return msg ? formatRelativeTime(msg.timestamp, now.value.getTime()) : "";
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
  <div class="min-h-full bg-default safe-area-inset-min">

    <!-- Header -->
    <div class="sticky top-0 z-10 border-b border-default bg-default px-5 py-4">
      <h1 class="label">MESSAGES</h1>
    </div>

    <div class="max-w-xl mx-auto">

      <!-- Empty state -->
      <UEmpty
        v-if="sortedConversations.length === 0"
        icon="i-heroicons-chat-bubble-left-right"
        title="No conversations"
        description="Start a DM from the Contacts page."
        class="m-5 border-0 shadow-none"
      />

      <!-- Conversation rows -->
      <NuxtLink
        v-for="nodeId in sortedConversations"
        :key="nodeId"
        :to="`/dm/${nodeId}`"
        class="flex items-center gap-3 border-b border-default px-5 py-4 transition-colors hover:bg-muted"
      >
        <!-- Unread dot -->
        <span
          class="h-2 w-2 shrink-0 rounded-full"
          :class="(unreadCounts[nodeId] ?? 0) > 0 ? 'bg-primary' : 'bg-transparent'"
        />

        <!-- Name + preview -->
        <div class="min-w-0 flex-1">
          <div class="flex items-center justify-between gap-2">
            <span class="truncate text-sm font-bold text-highlighted">
              {{ displayName(nodeId) }}
            </span>
            <span class="shrink-0 text-[10px] text-dimmed">{{ lastMessageTime(nodeId) }}</span>
          </div>
          <div class="mt-1 flex items-center justify-between gap-2">
            <p class="truncate text-xs text-muted">{{ lastMessagePreview(nodeId) }}</p>
            <UBadge
              v-if="(unreadCounts[nodeId] ?? 0) > 0"
              :label="String(unreadCounts[nodeId])"
              color="primary"
              variant="solid"
              size="xs"
              class="shrink-0"
            />
          </div>
        </div>

        <!-- Chevron -->
        <UIcon name="i-heroicons-chevron-right" class="shrink-0 text-base text-dimmed" />
      </NuxtLink>

    </div>
  </div>
</template>
