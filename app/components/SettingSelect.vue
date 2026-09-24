<script setup lang="ts">
export interface SettingSelectOption {
  value: string;
  label: string;
}

defineProps<{
  label: string;
  options: SettingSelectOption[];
  // When set, rendered as an empty-value first option (e.g. "— Default —").
  placeholder?: string;
}>();
const model = defineModel<string>({ required: true });
</script>

<template>
  <div>
    <p class="label mb-2">{{ label }}</p>
    <div class="relative">
      <select
        v-model="model"
        class="w-full appearance-none cursor-pointer border-2 border-(--ui-border-accented) bg-elevated px-4 py-2 pr-9 text-xs text-default outline-none transition-colors focus:border-primary shadow-(--ui-shadow-hard-sm)"
      >
        <option v-if="placeholder !== undefined" value="">{{ placeholder }}</option>
        <option v-for="opt in options" :key="opt.value" :value="opt.value">
          {{ opt.label }}
        </option>
      </select>
      <UIcon name="i-heroicons-chevron-down" class="absolute right-3 top-1/2 -translate-y-1/2 text-muted pointer-events-none text-base" />
    </div>
  </div>
</template>
