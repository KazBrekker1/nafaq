<script setup lang="ts">
import { formatRelativeTime, avatarLetter, relayStatusColor } from "~/utils/format";

const { connectionProgress, relayStatus, nodeError } = useCall();
const { contacts, displayName: contactDisplayName } = useContacts();
const { isOnline } = usePresence();
const { conversations, unreadCounts } = useDM();
const appVersion = useRuntimeConfig().public.appVersion;
const { status: updateStatus, isUpdateAvailable, latestVersion, checkForUpdateOnce } = useAppUpdate();
const showUpdateModal = ref(false);
const now = useNow({ interval: 60_000 });

function openUpdateModal() {
  showUpdateModal.value = true;
}

// ── Online contacts ──────────────────────────────────────

const onlineContacts = computed(() =>
  contacts.value.filter(c => isOnline(c.node_id))
);

// ── Updates ──────────────────────────────────────────────
// One automatic check per app session; only an available update opens the
// modal on its own — a failed check just shows the footer link.

onMounted(() => {
  void checkForUpdateOnce();
});

watch(isUpdateAvailable, (available) => {
  if (available) openUpdateModal();
});

// ── Recent activity ──────────────────────────────────────

interface RecentItem {
  nodeId: string;
  name: string;
  preview: string;
  timestamp: number;
  unread: number;
}

const recentItems = computed<RecentItem[]>(() => {
  const items: RecentItem[] = [];
  for (const [nid, msgs] of Object.entries(conversations.value)) {
    if (!msgs || msgs.length === 0) continue;
    const last = msgs[msgs.length - 1]!;
    let preview: string;
    if (last.type === "text") {
      preview = last.content.length > 40 ? last.content.slice(0, 40) + "…" : last.content;
    } else {
      preview = `[File] ${last.name}`;
    }
    items.push({
      nodeId: nid,
      name: contactDisplayName(nid),
      preview,
      timestamp: last.timestamp,
      unread: unreadCounts.value[nid] ?? 0,
    });
  }
  items.sort((a, b) => b.timestamp - a.timestamp);
  return items.slice(0, 5);
});
</script>

<template>
  <div class="min-h-full bg-default safe-area-inset-min">

    <!-- Header -->
    <div class="sticky top-0 z-10 border-b border-default bg-default px-5 py-4">
      <h1 class="label">HOME</h1>
    </div>

    <div class="mx-auto max-w-xl space-y-4 p-4">

      <!-- ── IDENTITY CARD ── -->
      <UCard>
        <IdentityCard />

        <div class="mt-4 space-y-2">
          <ConnectionProgress :step="connectionProgress" />
          <div class="flex items-center justify-between gap-3 text-[10px] tracking-wider">
            <span class="text-muted">RELAY</span>
            <UBadge :color="relayStatusColor(relayStatus)" variant="subtle">{{ relayStatus.toUpperCase() }}</UBadge>
          </div>
          <p v-if="nodeError" class="text-[10px] text-error">
            {{ nodeError }}
          </p>
        </div>
      </UCard>

      <!-- ── ONLINE NOW ── -->
      <UCard>
        <template #header>
          <p class="label">
            ONLINE NOW
            <span v-if="onlineContacts.length > 0" class="ml-1 text-primary">({{ onlineContacts.length }})</span>
          </p>
        </template>

        <UEmpty
          v-if="contacts.length === 0"
          icon="i-heroicons-user-plus"
          title="No contacts yet"
          description="Add your first contact to get started."
        >
          <template #actions>
            <UButton variant="solid" color="primary" @click="() => { navigateTo('/contacts') }">+ Add contact</UButton>
          </template>
        </UEmpty>

        <UEmpty
          v-else-if="onlineContacts.length === 0"
          icon="i-heroicons-signal-slash"
          description="No contacts online right now."
        />

        <div v-else class="flex gap-3 overflow-x-auto">
          <div
            v-for="contact in onlineContacts"
            :key="contact.node_id"
            class="w-28 shrink-0 border-2 border-(--ui-border-accented) p-3 transition-colors hover:bg-elevated"
          >
            <NuxtLink :to="`/dm/${contact.node_id}`" class="block">
              <UAvatar
                :text="avatarLetter(contact.display_name)"
                color="primary"
                size="md"
                class="mx-auto mb-2"
              />
              <p class="mb-2 truncate text-center text-[10px] font-bold text-highlighted">
                {{ contact.display_name }}
              </p>
            </NuxtLink>
            <div class="flex justify-center gap-1">
              <UTooltip text="Message">
                <UButton
                  icon="i-heroicons-envelope"
                  variant="ghost"
                  color="neutral"
                  square
                  aria-label="Message"
                  :to="`/dm/${contact.node_id}`"
                />
              </UTooltip>
              <UTooltip text="Call">
                <UButton
                  icon="i-heroicons-phone"
                  variant="ghost"
                  color="neutral"
                  square
                  aria-label="Call"
                  :to="`/dm/${contact.node_id}?call=1`"
                />
              </UTooltip>
            </div>
          </div>
        </div>
      </UCard>

      <!-- ── RECENT ── -->
      <UCard>
        <template #header>
          <p class="label">RECENT</p>
        </template>

        <UEmpty
          v-if="recentItems.length === 0"
          icon="i-heroicons-chat-bubble-left-right"
          description="No recent activity."
        />

        <div v-else class="-mx-4 -my-4 divide-y divide-default sm:-mx-6 sm:-my-6">
          <NuxtLink
            v-for="item in recentItems"
            :key="item.nodeId"
            :to="`/dm/${item.nodeId}`"
            class="flex items-center gap-3 px-4 py-3 transition-colors hover:bg-elevated sm:px-6"
          >
            <div
              class="h-2 w-2 shrink-0 rounded-full"
              :class="item.unread > 0 ? 'bg-primary' : 'bg-transparent'"
            />
            <div class="min-w-0 flex-1">
              <div class="flex items-center justify-between gap-2">
                <span class="truncate text-xs font-bold text-highlighted">
                  {{ item.name }}
                </span>
                <span class="shrink-0 text-[10px] text-muted">
                  {{ formatRelativeTime(item.timestamp, now.getTime()) }}
                </span>
              </div>
              <div class="mt-0.5 flex items-center justify-between gap-2">
                <p class="truncate text-[10px] text-muted">{{ item.preview }}</p>
                <UBadge v-if="item.unread > 0" color="primary" class="shrink-0">
                  {{ item.unread }}
                </UBadge>
              </div>
            </div>
            <UIcon name="i-heroicons-chevron-right" class="shrink-0 text-base text-muted" />
          </NuxtLink>
        </div>
      </UCard>

    </div>

    <footer class="mx-auto max-w-xl border-t border-default px-4 py-3 text-center text-[10px] tracking-wider text-muted">
      <span>Nafaq v{{ appVersion }}</span>
      <UButton
        v-if="isUpdateAvailable"
        size="xs"
        variant="subtle"
        color="primary"
        class="ml-2"
        @click="openUpdateModal"
      >
        Update to v{{ latestVersion }}
      </UButton>
      <UButton
        v-else-if="updateStatus === 'error'"
        size="xs"
        variant="link"
        color="error"
        class="ml-2"
        @click="openUpdateModal"
      >
        Update check failed
      </UButton>
    </footer>

    <AppUpdateModal v-model:open="showUpdateModal" />

  </div>
</template>
