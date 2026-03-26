<script setup lang="ts">
const call = useCall();
const { state, ticket, nodeId, nodeReady, error, displayName, connectionProgress, showPreCallOverlay, createCall, joinCall, endCall, joinCallFromOverlay } = call;
const hasName = computed(() => displayName.value.trim().length > 0);
</script>

<template>
  <div class="min-h-screen flex items-center justify-center p-4 sm:p-8 safe-area-inset-min">
    <div class="w-full max-w-xl">
      <div class="text-center mb-8 sm:mb-12">
        <h1 class="text-3xl sm:text-5xl font-black tracking-[6px] sm:tracking-[8px]">NAFAQ</h1>
        <p class="label mt-2">P2P Encrypted Calls</p>
      </div>

      <div class="flex items-center justify-center gap-2 mb-6 sm:mb-8">
        <ConnectionProgress :step="connectionProgress" />
        <span v-if="nodeId && connectionProgress === 'node-ready'" class="text-xs text-[var(--color-muted)]">
          · {{ nodeId.slice(0, 12) }}...
        </span>
      </div>

      <div class="mb-6 sm:mb-8">
        <NameInput v-model="displayName" />
      </div>

      <div v-if="error" class="border-2 border-[var(--color-danger)] p-3 mb-6 text-xs text-[var(--color-danger)]">
        {{ error }}
      </div>

      <div class="grid grid-cols-1 sm:grid-cols-2 gap-0">
        <div class="border-2 border-[var(--color-border)] p-4 sm:p-6 sm:border-r-0 border-b-0 sm:border-b-2">
          <TicketCreate :ticket="ticket" :state="state" :disabled="!hasName" @create="createCall" />
        </div>
        <div class="border-2 border-[var(--color-border)] p-4 sm:p-6">
          <TicketJoin :state="state" :disabled="!hasName" @join="joinCall" />
        </div>
      </div>

      <div class="border-t border-[var(--color-border-muted)] mt-6 sm:mt-8 pt-4 text-center">
        <p class="label">YOUR NODE</p>
        <p class="text-[10px] sm:text-xs text-[var(--color-muted)] mt-1 break-all">{{ nodeId || "Initializing Iroh..." }}</p>
      </div>
    </div>

    <PreCallOverlay
      v-model:open="showPreCallOverlay"
      @join="joinCallFromOverlay"
      @cancel="endCall"
    />
  </div>
</template>
