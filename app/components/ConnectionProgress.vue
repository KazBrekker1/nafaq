<script setup lang="ts">
const { step } = defineProps<{
  step: "idle" | "starting-node" | "node-ready" | "connecting" | "securing" | "connected";
}>();

const steps = [
  { key: "starting-node", label: "Starting node..." },
  { key: "node-ready", label: "Node ready" },
  { key: "connecting", label: "Connecting..." },
  { key: "securing", label: "Establishing secure channel..." },
  { key: "connected", label: "Connected" },
] as const;

const activeIndex = computed(() =>
  steps.findIndex((s) => s.key === step)
);
</script>

<template>
  <div v-if="step !== 'idle'" class="flex items-center gap-2 text-xs">
    <div
      class="w-2 h-2 rounded-full"
      :class="step === 'starting-node' || step === 'connecting' || step === 'securing'
        ? 'bg-[var(--color-accent)] animate-pulse'
        : step === 'node-ready' || step === 'connected'
          ? 'bg-[var(--color-accent)]'
          : 'bg-[var(--color-muted)]'"
    />
    <span class="text-[var(--color-muted)] tracking-wider">
      {{ steps[activeIndex]?.label || "" }}
    </span>
  </div>
</template>
