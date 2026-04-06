<script setup lang="ts">
import { truncateNodeId, formatTime, avatarLetter } from "~/utils/format";

const { nodeId, displayName, connectionProgress } = useCall();
const { contacts, displayName: contactDisplayName } = useContacts();
const { isOnline, startProbing, stopProbing } = usePresence();
const { conversations, unreadCounts } = useDM();
const { settings } = useSettings();

// ── Identity ─────────────────────────────────────────────

const truncatedNodeId = computed(() => {
  if (!nodeId.value) return "\u2014";
  return truncateNodeId(nodeId.value);
});

const { copy, copied: nodeCopied } = useClipboard();
function copyNodeId() {
  if (nodeId.value) copy(nodeId.value);
}

const qrModalOpen = ref(false);

// ── Online contacts ──────────────────────────────────────

const onlineContacts = computed(() =>
  contacts.value.filter(c => isOnline(c.node_id))
);

const contactNodeIds = computed(() => contacts.value.map(c => c.node_id));

onMounted(() => {
  if (contactNodeIds.value.length > 0) startProbing(contactNodeIds);
});

onUnmounted(() => {
  stopProbing();
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
      preview = last.content.length > 40 ? last.content.slice(0, 40) + "\u2026" : last.content;
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
  <div class="min-h-full bg-[var(--color-surface)] safe-area-inset-min">

    <!-- Header -->
    <div class="border-b border-[var(--color-border-muted)] px-4 py-3 sticky top-0 bg-[var(--color-surface)] z-10">
      <h1 class="label text-[var(--color-border)]" style="letter-spacing: 4px;">HOME</h1>
    </div>

    <div class="max-w-xl mx-auto">

      <!-- ── IDENTITY CARD ── -->
      <section class="border-b-2 border-[var(--color-border)]">
        <div class="px-4 sm:px-6 py-4">
          <div class="flex items-center justify-between gap-3">
            <div class="min-w-0">
              <p class="text-sm font-bold text-[var(--color-border)] font-mono truncate">
                {{ displayName || "\u2014" }}
              </p>
              <div class="flex items-center gap-2 mt-1">
                <p class="text-[10px] text-[var(--color-muted)] font-mono truncate">
                  {{ truncatedNodeId }}
                </p>
                <span
                  v-if="settings.persistentIdentity"
                  class="text-[9px] text-[var(--color-accent)] font-bold tracking-wider shrink-0"
                >
                  PERSISTENT
                </span>
              </div>
            </div>
            <div class="flex gap-0 shrink-0">
              <button
                class="border-2 border-[var(--color-border)] px-3 py-1 text-[10px] font-bold tracking-widest hover:bg-[var(--color-border)] hover:text-black transition-colors"
                @click="qrModalOpen = true"
              >
                QR
              </button>
              <button
                class="border-2 border-l-0 border-[var(--color-border)] px-3 py-1 text-[10px] font-bold tracking-widest hover:bg-[var(--color-border)] hover:text-black transition-colors"
                :class="nodeCopied ? 'bg-[var(--color-accent)] text-white border-[var(--color-accent)]' : ''"
                @click="copyNodeId"
              >
                {{ nodeCopied ? "COPIED" : "COPY" }}
              </button>
            </div>
          </div>

          <div class="mt-3">
            <ConnectionProgress :step="connectionProgress" />
          </div>
        </div>
      </section>

      <!-- ── ONLINE NOW ── -->
      <section class="border-b-2 border-[var(--color-border)]">
        <div class="px-4 sm:px-6 py-3 border-b border-[var(--color-border-muted)]">
          <p class="label" style="letter-spacing: 4px;">
            ONLINE NOW
            <span v-if="onlineContacts.length > 0" class="text-[var(--color-accent)] ml-1">({{ onlineContacts.length }})</span>
          </p>
        </div>

        <div v-if="contacts.length === 0" class="px-4 sm:px-6 py-8 text-center">
          <p class="text-xs text-[var(--color-muted)] mb-3">Add your first contact to get started.</p>
          <button
            class="border-2 border-[var(--color-accent)] text-[var(--color-accent)] px-4 py-2 text-[10px] font-bold tracking-widest hover:bg-[var(--color-accent)] hover:text-white transition-colors"
            @click="navigateTo('/contacts')"
          >
            + ADD CONTACT
          </button>
        </div>

        <div v-else-if="onlineContacts.length === 0" class="px-4 sm:px-6 py-8 text-center">
          <p class="text-xs text-[var(--color-muted)]">No contacts online right now.</p>
        </div>

        <div v-else class="px-4 sm:px-6 py-4 overflow-x-auto">
          <div class="flex gap-3" :style="{ minWidth: 'min-content' }">
            <div
              v-for="contact in onlineContacts"
              :key="contact.node_id"
              class="shrink-0 border-2 border-[var(--color-border)] p-3 w-28 cursor-pointer hover:bg-[var(--color-surface-alt)] transition-colors"
              @click="navigateTo('/dm/' + contact.node_id)"
            >
              <UAvatar
                :text="avatarLetter(contact.display_name)"
                size="md"
                class="mx-auto mb-2 border-2 border-[var(--color-accent)] bg-black text-[var(--color-accent)] font-mono font-bold"
              />
              <p class="text-[10px] font-bold text-[var(--color-border)] font-mono text-center truncate mb-2">
                {{ contact.display_name }}
              </p>
              <div class="flex gap-0 justify-center">
                <UTooltip text="Message">
                  <button
                    class="border-2 border-[var(--color-border)] px-2 py-1 text-[10px] hover:bg-[var(--color-border)] hover:text-black transition-colors"
                    @click.stop="navigateTo('/dm/' + contact.node_id)"
                  >
                    ✉
                  </button>
                </UTooltip>
                <UTooltip text="Call">
                  <button
                    class="border-2 border-l-0 border-[var(--color-border)] px-2 py-1 text-[10px] hover:bg-[var(--color-border)] hover:text-black transition-colors"
                    @click.stop="navigateTo('/dm/' + contact.node_id)"
                  >
                    ☎
                  </button>
                </UTooltip>
              </div>
            </div>
          </div>
        </div>
      </section>

      <!-- ── RECENT ── -->
      <section class="border-b-2 border-[var(--color-border)]">
        <div class="px-4 sm:px-6 py-3 border-b border-[var(--color-border-muted)]">
          <p class="label" style="letter-spacing: 4px;">RECENT</p>
        </div>

        <div v-if="recentItems.length === 0" class="px-4 sm:px-6 py-8 text-center">
          <p class="text-xs text-[var(--color-muted)]">No recent activity.</p>
        </div>

        <div
          v-for="item in recentItems"
          :key="item.nodeId"
          class="border-b border-[var(--color-border-muted)] px-4 sm:px-6 py-3 flex items-center gap-3 cursor-pointer hover:bg-[var(--color-surface-alt)] transition-colors"
          @click="navigateTo('/dm/' + item.nodeId)"
        >
          <div
            class="shrink-0 w-2 h-2 rounded-full"
            :class="item.unread > 0 ? 'bg-[var(--color-accent)]' : 'bg-transparent'"
          />
          <div class="flex-1 min-w-0">
            <div class="flex items-center justify-between gap-2">
              <span class="text-xs font-bold text-[var(--color-border)] font-mono truncate">
                {{ item.name }}
              </span>
              <span class="text-[10px] text-[var(--color-muted)] shrink-0">
                {{ formatTime(item.timestamp) }}
              </span>
            </div>
            <div class="flex items-center justify-between gap-2 mt-0.5">
              <p class="text-[10px] text-[var(--color-muted)] truncate">{{ item.preview }}</p>
              <span
                v-if="item.unread > 0"
                class="shrink-0 text-[9px] font-bold bg-[var(--color-accent)] text-white px-1.5 py-0.5 font-mono"
              >
                {{ item.unread }}
              </span>
            </div>
          </div>
          <UIcon name="i-heroicons-chevron-right" class="text-[var(--color-muted)] text-base shrink-0" />
        </div>
      </section>

    </div>

    <NodeIdQrModal v-model:open="qrModalOpen" />

  </div>
</template>
