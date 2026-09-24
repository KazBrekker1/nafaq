<script setup lang="ts">
import { truncateNodeId, formatTime, avatarLetter } from "~/utils/format";

const { nodeId, displayName, connectionProgress, relayStatus, nodeError } = useCall();
const { contacts, displayName: contactDisplayName } = useContacts();
const { isOnline, startProbing, stopProbing } = usePresence();
const { conversations, unreadCounts } = useDM();
const { settings } = useSettings();
const appVersion = useRuntimeConfig().public.appVersion;
const { status: updateStatus, isUpdateAvailable, latestVersion, checkForUpdate } = useAppUpdate();
const showUpdateModal = ref(false);

function openUpdateModal() {
  showUpdateModal.value = true;
}

// ── Identity ─────────────────────────────────────────────

const truncatedNodeId = computed(() => {
  if (!nodeId.value) return "—";
  return truncateNodeId(nodeId.value);
});

const { copy, copied: nodeCopied } = useClipboard();
function copyNodeId() {
  if (nodeId.value) copy(nodeId.value);
}

const qrModalOpen = ref(false);

const relayStatusLabel = computed(() => relayStatus.value.replace("_", " ").toUpperCase());
const relayStatusClass = computed(() => {
  switch (relayStatus.value) {
    case "online":
      return "text-primary";
    case "degraded":
    case "offline":
      return "text-error";
    default:
      return "text-muted";
  }
});

// ── Online contacts ──────────────────────────────────────

const onlineContacts = computed(() =>
  contacts.value.filter(c => isOnline(c.node_id))
);

const contactNodeIds = computed(() => contacts.value.map(c => c.node_id));

onMounted(() => {
  startProbing(contactNodeIds);
  checkForUpdate();
});

onUnmounted(() => {
  stopProbing();
});

watch(isUpdateAvailable, (available) => {
  if (available) openUpdateModal();
});

watch(updateStatus, (status) => {
  if (status === "error") openUpdateModal();
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
        <div class="flex items-center justify-between gap-3">
          <div class="min-w-0">
            <p class="truncate text-sm font-bold text-highlighted">
              {{ displayName || "—" }}
            </p>
            <div class="mt-1 flex items-center gap-2">
              <p class="truncate text-[10px] text-muted">
                {{ truncatedNodeId }}
              </p>
              <UBadge v-if="settings.persistentIdentity" color="primary" class="shrink-0">
                Persistent
              </UBadge>
            </div>
          </div>
          <UFieldGroup class="shrink-0">
            <UButton variant="subtle" color="neutral" @click="() => { qrModalOpen = true }">QR</UButton>
            <UButton
              :variant="nodeCopied ? 'solid' : 'subtle'"
              :color="nodeCopied ? 'primary' : 'neutral'"
              @click="copyNodeId"
            >
              {{ nodeCopied ? "Copied" : "Copy" }}
            </UButton>
          </UFieldGroup>
        </div>

        <div class="mt-4 space-y-2">
          <ConnectionProgress :step="connectionProgress" />
          <div class="flex items-center justify-between gap-3 text-[10px] tracking-wider">
            <span class="text-muted">RELAY</span>
            <span class="font-bold" :class="relayStatusClass">{{ relayStatusLabel }}</span>
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
            class="w-28 shrink-0 cursor-pointer border-2 border-(--ui-border-accented) p-3 transition-colors hover:bg-elevated"
            @click="navigateTo('/dm/' + contact.node_id)"
          >
            <UAvatar
              :text="avatarLetter(contact.display_name)"
              color="primary"
              size="md"
              class="mx-auto mb-2"
            />
            <p class="mb-2 truncate text-center text-[10px] font-bold text-highlighted">
              {{ contact.display_name }}
            </p>
            <div class="flex justify-center gap-1">
              <UTooltip text="Message">
                <UButton
                  variant="ghost"
                  color="neutral"
                  square
                  aria-label="Message"
                  @click.stop="() => { navigateTo('/dm/' + contact.node_id) }"
                >
                  ✉
                </UButton>
              </UTooltip>
              <UTooltip text="Call">
                <UButton
                  variant="ghost"
                  color="neutral"
                  square
                  aria-label="Call"
                  @click.stop="() => { navigateTo('/dm/' + contact.node_id) }"
                >
                  ☎
                </UButton>
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
          <div
            v-for="item in recentItems"
            :key="item.nodeId"
            class="flex cursor-pointer items-center gap-3 px-4 py-3 transition-colors hover:bg-elevated sm:px-6"
            @click="navigateTo('/dm/' + item.nodeId)"
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
                  {{ formatTime(item.timestamp) }}
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
          </div>
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

    <NodeIdQrModal v-model:open="qrModalOpen" />
    <AppUpdateModal v-model:open="showUpdateModal" />

  </div>
</template>
