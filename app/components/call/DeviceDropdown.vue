<script setup lang="ts">
import type { MediaDevice } from "~/composables/useMedia";

defineProps<{
  open: boolean;
  label: string;
  devices: MediaDevice[];
  selectedId: string;
}>();

const emit = defineEmits<{
  "update:open": [value: boolean];
  select: [deviceId: string];
}>();
</script>

<template>
  <UPopover :open="open" :content="{ side: 'top', sideOffset: 8 }" @update:open="emit('update:open', $event)">
    <template #default>
      <button
        class="h-[46px] w-[22px] flex items-center justify-center border-l border-[var(--color-border-muted)] hover:bg-white/5 transition-colors"
        :class="{ 'bg-[var(--color-accent)]/15': open }"
        @click="emit('update:open', !open)"
      >
        <UIcon
          name="i-heroicons-chevron-up-20-solid"
          class="text-[10px] text-[var(--color-muted)] transition-transform"
          :class="{ 'rotate-180': !open }"
        />
      </button>
    </template>

    <template #content>
      <div class="bg-[#111] border border-[var(--color-border-muted)] min-w-[240px] font-mono">
        <div class="label px-3 py-1.5 border-b border-[var(--color-border-muted)]">{{ label }}</div>
        <button
          v-for="device in devices"
          :key="device.deviceId"
          class="w-full text-left px-3 py-2.5 text-xs flex items-center justify-between hover:bg-white/5 transition-colors border-b border-[#222] last:border-b-0"
          :class="device.deviceId === selectedId ? 'text-[var(--color-accent)]' : 'text-[var(--color-muted)]'"
          @click="emit('select', device.deviceId); emit('update:open', false)"
        >
          <span class="truncate mr-2">{{ device.label }}</span>
          <UIcon v-if="device.deviceId === selectedId" name="i-heroicons-check-20-solid" class="text-xs shrink-0" />
        </button>
        <div v-if="devices.length === 0" class="px-3 py-2.5 text-xs text-[var(--color-muted)]">
          No devices found
        </div>
      </div>
    </template>
  </UPopover>
</template>
