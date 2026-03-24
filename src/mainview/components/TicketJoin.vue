<script setup lang="ts">
import { ref } from "vue";

const props = defineProps<{
  state: string;
}>();

const emit = defineEmits<{
  join: [ticket: string];
}>();

const ticketInput = ref("");

function submit() {
  const t = ticketInput.value.trim();
  if (!t) return;
  emit("join", t);
}
</script>

<template>
  <div>
    <p class="label mb-4">ENTER TICKET</p>
    <input
      v-model="ticketInput"
      class="input mb-4"
      placeholder="Paste ticket..."
      @keyup.enter="submit"
      :disabled="state === 'joining'"
    />
    <button
      class="btn btn-primary w-full"
      @click="submit"
      :disabled="!ticketInput.trim() || state === 'joining'"
    >
      {{ state === "joining" ? "Connecting..." : "Connect" }}
    </button>
  </div>
</template>
