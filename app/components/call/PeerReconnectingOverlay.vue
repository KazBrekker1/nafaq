<script setup lang="ts">
defineProps<{
  /** Shown when the peer's connection is actively being re-established. */
  reconnecting: boolean;
}>();
</script>

<template>
  <div
    v-if="reconnecting"
    class="absolute inset-0 z-20 flex flex-col items-center justify-center gap-3 bg-black/80 pointer-events-none"
  >
    <div class="reconnect-spinner" aria-hidden="true" />
    <span class="text-[10px] font-black uppercase tracking-[3px] text-warning">Reconnecting&hellip;</span>
  </div>
</template>

<style scoped>
.reconnect-spinner {
  width: 22px;
  height: 22px;
  border: 3px solid var(--ui-warning);
  border-right-color: transparent;
  border-bottom-color: transparent;
  animation: call-reconnect-spin 0.9s linear infinite;
}

@keyframes call-reconnect-spin {
  to { transform: rotate(360deg); }
}
</style>
