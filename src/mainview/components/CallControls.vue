<script setup lang="ts">
defineProps<{
  audioMuted: boolean;
  videoMuted: boolean;
  chatOpen: boolean;
}>();

const emit = defineEmits<{
  toggleAudio: [];
  toggleVideo: [];
  toggleChat: [];
  endCall: [];
}>();
</script>

<template>
  <div class="flex justify-center gap-0">
    <button
      class="w-[52px] h-12 bg-transparent border-2 border-[var(--color-border)] text-[var(--color-border)] text-xs font-bold cursor-pointer font-mono"
      :class="{ 'bg-[var(--color-danger)] border-[var(--color-danger)]': audioMuted }"
      @click="emit('toggleAudio')"
    >Mic</button>
    <button
      class="w-[52px] h-12 bg-transparent border-2 border-[var(--color-border)] border-l-0 text-[var(--color-border)] text-xs font-bold cursor-pointer font-mono"
      :class="{ 'bg-[var(--color-danger)] border-[var(--color-danger)]': videoMuted }"
      @click="emit('toggleVideo')"
    >Cam</button>
    <button
      class="w-[52px] h-12 bg-transparent border-2 border-[var(--color-border)] border-l-0 text-[var(--color-border)] text-xs font-bold cursor-pointer font-mono"
      :class="{ 'bg-[var(--color-accent)] border-[var(--color-accent)]': chatOpen }"
      @click="emit('toggleChat')"
    >Chat</button>
    <button
      class="w-[52px] h-12 bg-[var(--color-danger)] border-2 border-[var(--color-danger)] border-l-0 text-white text-xs font-bold cursor-pointer font-mono"
      @click="emit('endCall')"
    >End</button>
  </div>
</template>
