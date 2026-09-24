<script setup lang="ts">
import type { ConnectionProgress } from "~/composables/useCall";

const { step } = defineProps<{
  step: ConnectionProgress;
}>();

const steps = [
  { key: "starting-node", label: "Starting node..." },
  { key: "relay-connecting", label: "Connecting to relay..." },
  { key: "node-ready", label: "Relay online" },
  { key: "relay-degraded", label: "Relay degraded — waiting for recovery" },
  { key: "relay-offline", label: "Relay offline — waiting for recovery" },
  { key: "connecting", label: "Connecting..." },
  { key: "securing", label: "Establishing secure channel..." },
  { key: "connected", label: "Connected" },
] as const;

const current = computed(() => steps.find((s) => s.key === step));
const isActive = computed(() => step === "starting-node" || step === "relay-connecting" || step === "connecting" || step === "securing");
const isHealthy = computed(() => step === "node-ready" || step === "connected");
const isProblem = computed(() => step === "relay-degraded" || step === "relay-offline");
</script>

<template>
  <div v-if="step !== 'idle'" class="flex items-center gap-2 text-xs">
    <div
      class="h-2 w-2 shrink-0 rounded-full"
      :class="isActive
        ? 'bg-primary animate-pulse'
        : isHealthy
          ? 'bg-primary'
          : isProblem
            ? 'bg-error'
            : 'bg-muted'"
    />
    <span class="text-muted tracking-wider">
      {{ current?.label || "" }}
    </span>
  </div>
</template>
