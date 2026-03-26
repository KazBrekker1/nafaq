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
</script>

<template>
  <div>
    <p class="label mb-4">SHARE THIS TICKET</p>
    <div v-if="!ticket && state === 'idle'">
      <UButton class="w-full rounded-none font-mono" :disabled="disabled" @click="emit('create')">New Call</UButton>
    </div>
    <div v-else-if="state === 'creating'" class="text-[var(--color-muted)] text-xs tracking-widest">Creating...</div>
    <div v-else-if="ticket" class="space-y-4">
      <div class="border-2 border-[var(--color-accent)] p-4 text-xs break-all text-[var(--color-border)] bg-[#111]">{{ ticket }}</div>
      <div class="flex gap-0">
        <UButton class="flex-1 rounded-none border-r-0" @click="copyTicket">{{ copied ? "Copied!" : "Copy" }}</UButton>
        <UButton variant="outline" class="flex-1 rounded-none" @click="showShareModal = true">Show QR</UButton>
      </div>
      <p class="text-[var(--color-muted)] text-xs tracking-widest text-center">
        Waiting for peer<span class="text-[var(--color-accent)]">_</span>
      </p>
    </div>
  </div>

  <ConnectionShareModal
    :open="showShareModal"
    :ticket="ticket"
    title="SHARE THIS TICKET"
    description="Open a larger QR code or copy the full ticket."
    @close="showShareModal = false"
  />
</template>
