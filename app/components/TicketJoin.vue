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
</script>

<template>
  <div>
    <p class="label mb-4">ENTER TICKET</p>
    <UInput v-model="ticketInput" placeholder="Paste ticket..." class="mb-4 rounded-none font-mono"
      :disabled="disabled || state === 'joining'" @keyup.enter="submit" />
    <div class="flex gap-0">
      <UButton class="flex-1 rounded-none" :disabled="disabled || !ticketInput.trim() || state === 'joining'" @click="submit">
        {{ state === "joining" ? "Connecting..." : "Connect" }}
      </UButton>
      <UButton variant="outline" class="rounded-none border-l-0" :disabled="disabled || state === 'joining'" @click="showScanner = true">
        <UIcon name="i-heroicons-camera" />
      </UButton>
    </div>
  </div>

  <QrScanner v-if="showScanner" @scan="onScan" @close="showScanner = false" />
</template>
