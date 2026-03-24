<script setup lang="ts">
import { ref } from "vue";

const props = defineProps<{
  ticket: string | null;
  state: string;
}>();

const emit = defineEmits<{
  create: [];
}>();

const copied = ref(false);

function copyTicket() {
  if (!props.ticket) return;
  navigator.clipboard.writeText(props.ticket);
  copied.value = true;
  setTimeout(() => (copied.value = false), 2000);
}
</script>

<template>
  <div>
    <p class="label mb-4">SHARE THIS TICKET</p>
    <div v-if="!ticket && state === 'idle'">
      <button class="btn btn-primary w-full" @click="emit('create')">New Call</button>
    </div>
    <div v-else-if="state === 'creating'">
      <p class="text-[var(--color-muted)] text-xs tracking-widest">Creating...</p>
    </div>
    <div v-else-if="ticket" class="space-y-4">
      <div class="border-2 border-[var(--color-accent)] p-4 text-xs break-all text-[var(--color-border)] bg-[#111]">
        {{ ticket }}
      </div>
      <button class="btn btn-primary w-full" @click="copyTicket">
        {{ copied ? "Copied!" : "Copy Ticket" }}
      </button>
      <p class="text-[var(--color-muted)] text-xs tracking-widest text-center">
        Waiting for peer<span class="text-[var(--color-accent)]">_</span>
      </p>
    </div>
  </div>
</template>
