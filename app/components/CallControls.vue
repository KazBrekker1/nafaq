<script setup lang="ts">
defineProps<{ audioMuted: boolean; videoMuted: boolean; chatOpen: boolean }>();
const emit = defineEmits<{ toggleAudio: []; toggleVideo: []; toggleChat: []; endCall: [] }>();
</script>

<template>
  <div class="flex justify-center gap-0">
    <UButton variant="outline" class="w-[52px] h-12 rounded-none text-xs font-mono"
      :class="{ 'bg-[var(--color-danger)] border-[var(--color-danger)] text-white': audioMuted }"
      @click="emit('toggleAudio')">Mic</UButton>
    <UButton variant="outline" class="w-[52px] h-12 rounded-none border-l-0 text-xs font-mono"
      :class="{ 'bg-[var(--color-danger)] border-[var(--color-danger)] text-white': videoMuted }"
      @click="emit('toggleVideo')">Cam</UButton>
    <UButton variant="outline" class="w-[52px] h-12 rounded-none border-l-0 text-xs font-mono"
      :class="{ 'bg-[var(--color-accent)] border-[var(--color-accent)]': chatOpen }"
      @click="emit('toggleChat')">Chat</UButton>
    <UButton color="red" class="w-[52px] h-12 rounded-none border-l-0 text-xs font-mono"
      @click="emit('endCall')">End</UButton>
  </div>
</template>
