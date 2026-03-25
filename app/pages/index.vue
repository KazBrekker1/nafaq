<script setup lang="ts">
const { state, ticket, nodeId, sidecarConnected, error, createCall, joinCall } = useCall();
</script>

<template>
  <div class="min-h-screen flex items-center justify-center p-8">
    <div class="w-full max-w-xl">
      <div class="text-center mb-12">
        <h1 class="text-5xl font-black tracking-[8px]">NAFAQ</h1>
        <p class="label mt-2">P2P Encrypted Calls</p>
      </div>

      <div class="flex items-center justify-center gap-2 mb-8">
        <div class="w-2 h-2" :style="{ background: sidecarConnected ? '#8B5CF6' : '#ff0000' }" />
        <span class="text-xs text-[var(--color-muted)]">
          {{ sidecarConnected ? "Connected" : "Connecting..." }}
        </span>
        <span v-if="nodeId" class="text-xs text-[var(--color-muted)]">
          · {{ nodeId.slice(0, 12) }}...
        </span>
      </div>

      <div v-if="error" class="border-2 border-[var(--color-danger)] p-3 mb-6 text-xs text-[var(--color-danger)]">
        {{ error }}
      </div>

      <div class="grid grid-cols-2 gap-0">
        <div class="border-2 border-[var(--color-border)] p-6 border-r-0">
          <TicketCreate :ticket="ticket" :state="state" @create="createCall" />
        </div>
        <div class="border-2 border-[var(--color-border)] p-6">
          <TicketJoin :state="state" @join="joinCall" />
        </div>
      </div>

      <div class="border-t border-[var(--color-border-muted)] mt-8 pt-4 text-center">
        <p class="label">YOUR NODE</p>
        <p class="text-xs text-[var(--color-muted)] mt-1">{{ nodeId || "Initializing Iroh..." }}</p>
      </div>
    </div>
  </div>
</template>
