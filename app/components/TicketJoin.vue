<script setup lang="ts">
const { state, disabled = false } = defineProps<{ state: string; disabled?: boolean }>();
const emit = defineEmits<{ join: [ticket: string] }>();
const ticketInput = ref("");

function submit() {
  const t = ticketInput.value.trim();
  if (t) emit("join", t);
}
</script>

<template>
  <div>
    <p class="label mb-4">ENTER TICKET</p>
    <UInput v-model="ticketInput" placeholder="Paste ticket..." class="mb-4 rounded-none font-mono"
      :disabled="disabled || state === 'joining'" @keyup.enter="submit" />
    <UButton class="w-full rounded-none font-mono" :disabled="disabled || !ticketInput.trim() || state === 'joining'" @click="submit">
      {{ state === "joining" ? "Connecting..." : "Connect" }}
    </UButton>
  </div>
</template>
