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
      <UButton
        icon="i-heroicons-chevron-up-20-solid"
        :color="open ? 'primary' : 'neutral'"
        variant="ghost"
        size="xl"
        :class="['h-[48px] w-[28px] transition-transform', !open && 'rotate-180']"
        :aria-label="`Select ${label.toLowerCase()}`"
        @click="emit('update:open', !open)"
      />
    </template>

    <template #content>
      <div class="dark min-w-[15rem] border-2 border-(--ui-border-accented) bg-elevated">
        <div class="label px-3 py-2 border-b border-muted">{{ label }}</div>
        <button
          v-for="device in devices"
          :key="device.deviceId"
          class="w-full text-left px-3 py-3 text-xs flex items-center justify-between hover:bg-accented/60 transition-colors border-b border-muted last:border-b-0"
          :class="device.deviceId === selectedId ? 'text-primary' : 'text-muted'"
          @click="emit('select', device.deviceId); emit('update:open', false)"
        >
          <span class="truncate mr-2">{{ device.label }}</span>
          <UIcon v-if="device.deviceId === selectedId" name="i-heroicons-check-20-solid" class="text-xs shrink-0" />
        </button>
        <div v-if="devices.length === 0" class="px-3 py-3 text-xs text-muted">
          No devices found
        </div>
      </div>
    </template>
  </UPopover>
</template>
