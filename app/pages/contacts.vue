<script setup lang="ts">
import { truncateNodeId, avatarLetter } from "~/utils/format";

const { nodeId, displayName } = useCall();
const { contacts, remove } = useContacts();
const { isOnline, startProbing, stopProbing } = usePresence();
const { settings } = useSettings();

const truncatedNodeId = computed(() => {
  if (!nodeId.value) return "—";
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
  startProbing(contactNodeIds);
});

onUnmounted(() => {
  stopProbing();
});
</script>

<template>
  <div class="min-h-full bg-default safe-area-inset-min">

    <!-- Header -->
    <div class="sticky top-0 z-10 flex items-center justify-between border-b-2 border-default bg-default px-4 py-3">
      <h1 class="label" style="letter-spacing: 4px;">Contacts</h1>
      <UButton
        label="Add"
        icon="i-heroicons-plus"
        color="primary"
        variant="solid"
        @click="() => { addModalOpen = true }"
      />
    </div>

    <div class="mx-auto max-w-xl">

      <!-- Identity card -->
      <section class="border-b-2 border-default">
        <div class="px-4 py-4 sm:px-6">
          <p class="text-sm font-bold text-highlighted">{{ displayName || "—" }}</p>
          <div class="mt-1 flex items-center justify-between gap-2">
            <p class="text-xs text-muted">
              {{ truncatedNodeId }}
              <span v-if="settings.persistentIdentity" class="ml-1 text-primary">· persistent</span>
            </p>
            <UFieldGroup class="shrink-0">
              <UButton label="QR" @click="() => { qrModalOpen = true }" />
              <UButton
                :label="nodeCopied ? 'Copied' : 'Copy'"
                :color="nodeCopied ? 'primary' : 'neutral'"
                @click="copyNodeId"
              />
            </UFieldGroup>
          </div>
        </div>
      </section>

      <!-- Contact list -->
      <section>
        <UEmpty
          v-if="contacts.length === 0"
          icon="i-heroicons-user-group"
          title="No contacts yet"
          description="Tap Add to save a contact."
          class="mx-4 my-6 sm:mx-6"
        />

        <div
          v-for="contact in contacts"
          :key="contact.node_id"
          class="border-b border-muted px-4 py-3 sm:px-6"
        >
          <div class="flex items-center gap-3">
            <UChip
              :color="isOnline(contact.node_id) ? 'success' : 'neutral'"
              position="bottom-right"
              inset
            >
              <UAvatar :text="avatarLetter(contact.display_name)" size="md" />
            </UChip>

            <!-- Name + node ID -->
            <div class="min-w-0 flex-1">
              <div class="flex items-center gap-2">
                <span class="truncate text-sm font-bold text-highlighted">{{ contact.display_name }}</span>
                <span class="shrink-0 text-xs text-dimmed">{{ isOnline(contact.node_id) ? 'online' : 'offline' }}</span>
              </div>
              <p class="truncate text-xs text-muted">
                {{ truncateNodeId(contact.node_id) }}
              </p>
            </div>

            <!-- Action buttons. Presence (the online dot) is a soft hint, not a
                 hard gate: a brief gossip blip can mark a reachable peer
                 "offline", so keep the actions enabled — opening a DM dials on
                 demand and succeeds even when presence lags. -->
            <UFieldGroup class="shrink-0">
              <UTooltip text="Message">
                <UButton icon="i-heroicons-envelope" aria-label="Message" @click="() => { navigateTo('/dm/' + contact.node_id) }" />
              </UTooltip>
              <UTooltip text="Call">
                <UButton icon="i-heroicons-phone" aria-label="Call" @click="() => { navigateTo('/dm/' + contact.node_id) }" />
              </UTooltip>
              <UTooltip text="Remove">
                <UButton
                  icon="i-heroicons-x-mark"
                  color="error"
                  variant="ghost"
                  aria-label="Remove"
                  @click="remove(contact.node_id)"
                />
              </UTooltip>
            </UFieldGroup>
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
