<script setup lang="ts">
const props = defineProps<{
  quality: "good" | "degraded" | "poor";
}>();

const config = computed(() => {
  switch (props.quality) {
    case "good": return { bars: 4, color: "var(--color-accent)" };
    case "degraded": return { bars: 2, color: "var(--color-warning)" };
    case "poor": return { bars: 1, color: "var(--color-danger)" };
    default: return { bars: 1, color: "var(--color-danger)" };
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
        background: i < config.bars ? config.color : 'var(--color-border-muted)',
      }"
    />
  </div>
</template>
