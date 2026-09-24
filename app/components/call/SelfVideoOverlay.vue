<script setup lang="ts">
defineProps<{
  audioMuted: boolean;
  videoMuted: boolean;
}>();
</script>

<template>
  <div class="absolute inset-0 pointer-events-none">
    <!-- Camera off overlay -->
    <div v-if="videoMuted" class="absolute inset-0 bg-black/70 flex items-center justify-center">
      <UIcon name="i-heroicons-video-camera-slash" class="text-dimmed text-2xl" />
    </div>

    <!-- Mic off badge -->
    <div
      v-if="audioMuted"
      class="absolute top-1 right-1 size-5 sm:size-[22px] bg-error flex items-center justify-center z-20"
    >
      <UIcon name="i-lucide-mic-off" class="text-inverted text-[10px] sm:text-xs" />
    </div>
  </div>
</template>
