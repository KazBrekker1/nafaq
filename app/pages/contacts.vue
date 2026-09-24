<script setup lang="ts">
import { truncateNodeId, avatarLetter } from "~/utils/format";

const { contacts, remove } = useContacts();
const { isOnline } = usePresence();

const addModalOpen = ref(false);

// Two-step remove: the first click arms the button, a second click within a
// few seconds removes the contact.
const confirmingRemove = ref<string | null>(null);
const { start: startDisarm, stop: stopDisarm } = useTimeoutFn(() => {
  confirmingRemove.value = null;
}, 3000, { immediate: false });

function onRemove(nodeId: string) {
  if (confirmingRemove.value === nodeId) {
    stopDisarm();
    confirmingRemove.value = null;
    void remove(nodeId);
    return;
  }
  confirmingRemove.value = nodeId;
  startDisarm();
}
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
        <IdentityCard class="px-4 py-4 sm:px-6" />
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
                <UButton icon="i-heroicons-envelope" aria-label="Message" :to="`/dm/${contact.node_id}`" />
              </UTooltip>
              <UTooltip text="Call">
                <UButton icon="i-heroicons-phone" aria-label="Call" :to="`/dm/${contact.node_id}?call=1`" />
              </UTooltip>
              <UTooltip :text="confirmingRemove === contact.node_id ? 'Tap again to remove' : 'Remove'">
                <UButton
                  :icon="confirmingRemove === contact.node_id ? 'i-heroicons-trash' : 'i-heroicons-x-mark'"
                  color="error"
                  :variant="confirmingRemove === contact.node_id ? 'solid' : 'ghost'"
                  :aria-label="confirmingRemove === contact.node_id ? 'Confirm remove' : 'Remove'"
                  @click="onRemove(contact.node_id)"
                />
              </UTooltip>
            </UFieldGroup>
          </div>
        </div>
      </section>

    </div>

    <AddContactModal v-model:open="addModalOpen" />

  </div>
</template>
