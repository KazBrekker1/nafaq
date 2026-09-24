<script setup lang="ts">
const { quality } = defineProps<{
  quality: "good" | "degraded" | "poor";
}>();

const config = computed(() => {
  switch (quality) {
    case "good": return { bars: 4, color: "var(--ui-primary)" };
    case "degraded": return { bars: 2, color: "var(--ui-warning)" };
    case "poor": return { bars: 1, color: "var(--ui-error)" };
    default: return { bars: 1, color: "var(--ui-error)" };
  }
});

const barHeights = [6, 10, 14, 18];
</script>

<template>
  <div class="flex items-end gap-[2px]">
    <div
      v-for="(h, i) in barHeights"
      :key="i"
      class="w-[3px]"
      :style="{
        height: `${h}px`,
        background: i < config.bars ? config.color : 'var(--ui-border-muted)',
      }"
    />
  </div>
</template>
