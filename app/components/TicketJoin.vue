<script setup lang="ts">
const { state, disabled = false } = defineProps<{ state: string; disabled?: boolean }>();
const emit = defineEmits<{ join: [ticket: string] }>();
const ticketInput = ref("");
const showScanner = ref(false);

function submit() {
  const t = ticketInput.value.trim();
  if (t) emit("join", t);
}

function onScan(ticket: string) {
  showScanner.value = false;
  ticketInput.value = ticket;
  emit("join", ticket);
}

function openScanner() {
  showScanner.value = true;
}
</script>

<template>
  <div>
    <p class="label mb-3">ENTER TICKET</p>
    <UFieldGroup class="w-full">
      <UInput
        v-model="ticketInput"
        placeholder="Paste ticket..."
        class="flex-1"
        :disabled="disabled || state === 'joining'"
        @keyup.enter="submit"
      />
      <UButton
        icon="i-heroicons-camera"
        variant="subtle"
        color="neutral"
        :disabled="disabled || state === 'joining'"
        aria-label="Scan QR code"
        @click="openScanner"
      />
    </UFieldGroup>
    <UButton
      block
      variant="solid"
      color="primary"
      class="mt-3"
      :disabled="disabled || !ticketInput.trim() || state === 'joining'"
      :loading="state === 'joining'"
      @click="submit"
    >
      {{ state === "joining" ? "Connecting..." : "Connect" }}
    </UButton>
  </div>

  <QrScanner v-model:open="showScanner" @scan="onScan" />
</template>
