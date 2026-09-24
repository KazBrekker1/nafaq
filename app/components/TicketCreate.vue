<script setup lang="ts">
const { ticket, state, disabled = false } = defineProps<{ ticket: string | null; state: string; disabled?: boolean }>();
const emit = defineEmits<{ create: [] }>();

const copied = ref(false);
const showShareModal = ref(false);

function copyTicket() {
  if (!ticket) return;
  navigator.clipboard.writeText(ticket);
  copied.value = true;
  setTimeout(() => (copied.value = false), 2000);
}

function showTicketQr() {
  showShareModal.value = true;
}
</script>

<template>
  <div>
    <p class="label mb-3">SHARE THIS TICKET</p>
    <div v-if="!ticket && state === 'idle'">
      <UButton block variant="solid" color="primary" :disabled="disabled" @click="emit('create')">New Call</UButton>
    </div>
    <p v-else-if="state === 'creating'" class="text-muted text-xs tracking-widest">Creating...</p>
    <p v-else-if="!ticket" class="text-muted text-xs tracking-widest">
      Ticket unavailable — waiting for relay recovery.
    </p>
    <div v-else-if="ticket" class="space-y-3">
      <div class="border-2 border-primary bg-elevated p-3 text-xs break-all text-highlighted">{{ ticket }}</div>
      <UFieldGroup class="w-full">
        <UButton class="flex-1" block @click="copyTicket">{{ copied ? "Copied!" : "Copy" }}</UButton>
        <UButton class="flex-1" block variant="outline" @click="showTicketQr">Show QR</UButton>
      </UFieldGroup>
      <p class="text-muted text-xs tracking-widest text-center">
        Waiting for peer<span class="text-primary">_</span>
      </p>
    </div>
  </div>

  <ConnectionShareModal
    v-model:open="showShareModal"
    :ticket="ticket"
    title="SHARE THIS TICKET"
    description="Open a larger QR code or copy the full ticket."
  />
</template>
