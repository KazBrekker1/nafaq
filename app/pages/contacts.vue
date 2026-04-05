<script setup lang="ts">
import QRCode from "qrcode";
import { truncateNodeId } from "~/utils/format";

const { nodeId, displayName } = useCall();
const { contacts, remove } = useContacts();
const { isOnline, startProbing, stopProbing } = usePresence();
const { settings } = useSettings();

// ── Identity card ─────────────────────────────────────────
const truncatedNodeId = computed(() => {
  const id = nodeId.value;
  if (!id) return "—";
  return truncateNodeId(id);
});

const nodeCopied = ref(false);
async function copyNodeId() {
  if (!nodeId.value) return;
  await navigator.clipboard.writeText(nodeId.value);
  nodeCopied.value = true;
  setTimeout(() => { nodeCopied.value = false; }, 1500);
}

const qrModalOpen = ref(false);
const qrDataUrl = ref<string | null>(null);

watch([qrModalOpen, nodeId], async ([open, id]) => {
  if (!open || !id) { qrDataUrl.value = null; return; }
  try {
    qrDataUrl.value = await QRCode.toDataURL(id, {
      width: 256,
      margin: 1,
      color: { dark: "#000", light: "#fff" },
    });
  } catch {
    qrDataUrl.value = null;
  }
});

// ── Add modal ─────────────────────────────────────────────
const addModalOpen = ref(false);

// ── Contact actions ───────────────────────────────────────
function avatarLetter(name: string) {
  return (name || "?")[0].toUpperCase();
}

function handleCall(nodeId: string) {
  navigateTo('/dm/' + nodeId);
}

// ── Presence ──────────────────────────────────────────────
const contactNodeIds = computed(() => contacts.value.map(c => c.node_id));

watch(contactNodeIds, (ids) => {
  if (ids.length > 0) startProbing(contactNodeIds);
}, { immediate: true });

onUnmounted(() => {
  stopProbing();
});
</script>

<template>
  <div class="min-h-full bg-[var(--color-surface)] safe-area-inset-min">

    <!-- Header -->
    <div class="border-b border-[var(--color-border-muted)] px-4 py-3 flex items-center justify-between sticky top-0 bg-[var(--color-surface)] z-10">
      <h1 class="label text-[var(--color-border)]" style="letter-spacing: 4px;">CONTACTS</h1>
      <button
        class="text-xs font-bold tracking-widest text-[var(--color-accent)] hover:text-white transition-colors"
        @click="addModalOpen = true"
      >
        + ADD
      </button>
    </div>

    <div class="max-w-xl mx-auto">

      <!-- Identity card -->
      <section class="border-b-2 border-[var(--color-border)]">
        <div class="px-4 sm:px-6 py-4">
          <p class="text-sm font-bold text-[var(--color-border)] font-mono">{{ displayName || "—" }}</p>
          <div class="flex items-center justify-between mt-1 gap-2">
            <p class="text-[10px] text-[var(--color-muted)] font-mono">
              {{ truncatedNodeId }}
              <span v-if="settings.persistentIdentity" class="ml-1 text-[var(--color-accent)]">· persistent</span>
            </p>
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
        </div>
      </section>

      <!-- Contact list -->
      <section>
        <!-- Empty state -->
        <div
          v-if="contacts.length === 0"
          class="px-4 sm:px-6 py-12 text-center"
        >
          <p class="label text-[var(--color-muted)]">NO CONTACTS YET</p>
          <p class="text-xs text-[var(--color-muted)] mt-2">Tap + ADD to save a contact.</p>
        </div>

        <div
          v-for="contact in contacts"
          :key="contact.node_id"
          class="border-b border-[var(--color-border-muted)] px-4 sm:px-6 py-3"
        >
          <div class="flex items-center gap-3">
            <!-- Avatar -->
            <div
              class="shrink-0 flex items-center justify-center text-sm font-bold text-[var(--color-accent)] font-mono bg-black"
              style="width: 36px; height: 36px; border: 2px solid #8B5CF6;"
            >
              {{ avatarLetter(contact.display_name) }}
            </div>

            <!-- Name + node ID -->
            <div class="flex-1 min-w-0">
              <div class="flex items-center gap-2">
                <span class="text-sm font-bold text-[var(--color-border)] font-mono truncate">{{ contact.display_name }}</span>
                <!-- Online dot -->
                <span
                  class="shrink-0 inline-block w-2 h-2 rounded-full"
                  :style="isOnline(contact.node_id) ? 'background:#4ade80' : 'background:#666666'"
                  :title="isOnline(contact.node_id) ? 'online' : 'offline'"
                />
                <span class="text-[10px] text-[var(--color-muted)]">{{ isOnline(contact.node_id) ? 'online' : 'offline' }}</span>
              </div>
              <p class="text-[10px] text-[var(--color-muted)] font-mono truncate">
                {{ truncateNodeId(contact.node_id) }}
              </p>
            </div>

            <!-- Action buttons -->
            <div
              class="flex gap-0 shrink-0 transition-opacity"
              :class="isOnline(contact.node_id) ? 'opacity-100' : 'opacity-40'"
            >
              <button
                class="border-2 border-[var(--color-border)] px-2 py-1.5 text-[10px] font-bold tracking-widest hover:bg-[var(--color-border)] hover:text-black transition-colors"
                :disabled="!isOnline(contact.node_id)"
                :title="isOnline(contact.node_id) ? 'Send message' : 'Offline'"
                @click="navigateTo('/dm/' + contact.node_id)"
              >
                ✉
              </button>
              <button
                class="border-2 border-l-0 border-[var(--color-border)] px-2 py-1.5 text-[10px] font-bold tracking-widest hover:bg-[var(--color-border)] hover:text-black transition-colors"
                :disabled="!isOnline(contact.node_id)"
                :title="isOnline(contact.node_id) ? 'Call' : 'Offline'"
                @click="handleCall(contact.node_id)"
              >
                ☎
              </button>
              <button
                class="border-2 border-l-0 border-[var(--color-border-muted)] px-2 py-1.5 text-[10px] text-[var(--color-muted)] hover:border-[var(--color-danger)] hover:text-[var(--color-danger)] transition-colors"
                title="Remove contact"
                @click="remove(contact.node_id)"
              >
                ✕
              </button>
            </div>
          </div>
        </div>
      </section>

    </div>

    <!-- QR Modal for own node ID -->
    <UModal v-model:open="qrModalOpen">
      <template #content>
        <div class="border-2 border-[var(--color-border)] bg-[var(--color-surface-alt)]">
          <div class="flex items-center justify-between border-b border-[var(--color-border-muted)] px-4 py-3">
            <p class="label" style="letter-spacing: 4px;">NODE ID</p>
            <button
              class="text-[var(--color-muted)] hover:text-[var(--color-border)] transition-colors"
              aria-label="Close QR modal"
              @click="qrModalOpen = false"
            >
              <UIcon name="i-heroicons-x-mark" class="text-lg" />
            </button>
          </div>
          <div class="p-4 space-y-3">
            <div class="flex justify-center bg-white p-3">
              <img
                v-if="qrDataUrl"
                :src="qrDataUrl"
                alt="Node ID QR code"
                class="w-48 h-48"
              />
              <div
                v-else
                class="w-48 h-48 flex items-center justify-center text-xs text-black text-center"
              >
                {{ nodeId ? "Generating..." : "No node ID" }}
              </div>
            </div>
            <p class="text-[10px] text-[var(--color-muted)] break-all text-center font-mono">{{ nodeId || "—" }}</p>
            <UButton variant="outline" class="w-full rounded-none" @click="qrModalOpen = false">
              CLOSE
            </UButton>
          </div>
        </div>
      </template>
    </UModal>

    <!-- Add Contact Modal -->
    <AddContactModal
      v-model:open="addModalOpen"
      @added="addModalOpen = false"
    />

  </div>
</template>
