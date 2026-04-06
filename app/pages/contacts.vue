<script setup lang="ts">
import { truncateNodeId, avatarLetter } from "~/utils/format";

const { nodeId, displayName } = useCall();
const { contacts, remove } = useContacts();
const { isOnline, startProbing, stopProbing } = usePresence();
const { settings } = useSettings();

const truncatedNodeId = computed(() => {
  if (!nodeId.value) return "\u2014";
  return truncateNodeId(nodeId.value);
});

const { copy, copied: nodeCopied } = useClipboard();
function copyNodeId() {
  if (nodeId.value) copy(nodeId.value);
}

const qrModalOpen = ref(false);
const addModalOpen = ref(false);

const contactNodeIds = computed(() => contacts.value.map(c => c.node_id));

onMounted(() => {
  if (contactNodeIds.value.length > 0) startProbing(contactNodeIds);
});

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
            <UAvatar
              :text="avatarLetter(contact.display_name)"
              size="md"
              class="shrink-0 border-2 border-[var(--color-accent)] bg-black text-[var(--color-accent)] font-mono font-bold"
            />

            <!-- Name + node ID -->
            <div class="flex-1 min-w-0">
              <div class="flex items-center gap-2">
                <span class="text-sm font-bold text-[var(--color-border)] font-mono truncate">{{ contact.display_name }}</span>
                <!-- Online dot -->
                <span
                  class="shrink-0 inline-block w-2 h-2 rounded-full"
                  :style="isOnline(contact.node_id) ? 'background:var(--color-online)' : 'background:var(--color-muted)'"
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
              <UTooltip :text="isOnline(contact.node_id) ? 'Message' : 'Offline'">
                <button
                  class="border-2 border-[var(--color-border)] px-3 py-2.5 text-xs font-bold tracking-widest hover:bg-[var(--color-border)] hover:text-black transition-colors"
                  :disabled="!isOnline(contact.node_id)"
                  @click="navigateTo('/dm/' + contact.node_id)"
                >
                  ✉
                </button>
              </UTooltip>
              <UTooltip :text="isOnline(contact.node_id) ? 'Call' : 'Offline'">
                <button
                  class="border-2 border-l-0 border-[var(--color-border)] px-3 py-2.5 text-xs font-bold tracking-widest hover:bg-[var(--color-border)] hover:text-black transition-colors"
                  :disabled="!isOnline(contact.node_id)"
                  @click="navigateTo('/dm/' + contact.node_id)"
                >
                  ☎
                </button>
              </UTooltip>
              <UTooltip text="Remove">
                <button
                  class="border-2 border-l-0 border-[var(--color-border-muted)] px-3 py-2.5 text-xs text-[var(--color-muted)] hover:border-[var(--color-danger)] hover:text-[var(--color-danger)] transition-colors"
                  @click="remove(contact.node_id)"
                >
                  ✕
                </button>
              </UTooltip>
            </div>
          </div>
        </div>
      </section>

    </div>

    <NodeIdQrModal v-model:open="qrModalOpen" />

    <!-- Add Contact Modal -->
    <AddContactModal
      v-model:open="addModalOpen"
      @added="addModalOpen = false"
    />

  </div>
</template>
