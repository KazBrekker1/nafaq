<script setup lang="ts">
const { ticket, state } = defineProps<{ ticket: string | null; state: string }>();

const { copy, copied } = useClipboard({ copiedDuring: 2000 });
const showShareModal = ref(false);

function copyTicket() {
  if (ticket) copy(ticket);
}
</script>

<template>
  <div>
    <p class="label mb-3">SHARE THIS TICKET</p>
    <p v-if="state === 'creating'" class="text-muted text-xs tracking-widest">Creating...</p>
    <p v-else-if="!ticket" class="text-muted text-xs tracking-widest">
      Ticket unavailable — waiting for relay recovery.
    </p>
    <div v-else class="space-y-3">
      <div class="border-2 border-primary bg-elevated p-3 text-xs break-all text-highlighted">{{ ticket }}</div>
      <UFieldGroup class="w-full">
        <UButton class="flex-1" block @click="copyTicket">{{ copied ? "Copied!" : "Copy" }}</UButton>
        <UButton class="flex-1" block variant="outline" @click="() => { showShareModal = true }">Show QR</UButton>
      </UFieldGroup>
      <p class="text-muted text-xs tracking-widest text-center">
        Waiting for peer<span class="text-primary">_</span>
      </p>
    </div>

    <ConnectionShareModal
      v-model:open="showShareModal"
      :ticket="ticket"
      title="SHARE THIS TICKET"
      description="Open a larger QR code or copy the full ticket."
    />
  </div>
</template>
