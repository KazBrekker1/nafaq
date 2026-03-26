<script setup lang="ts">
const open = defineModel<boolean>('open', { required: true });
const emit = defineEmits<{ join: []; cancel: [] }>();

const media = useMedia();
const videoEl = ref<HTMLVideoElement | null>(null);

watch(
  [() => open.value, () => media.localStream.value],
  async ([isOpen, stream]) => {
    if (isOpen && !stream) {
      await media.startPreview();
    }
    if (videoEl.value) {
      videoEl.value.srcObject = media.localStream.value || null;
    }
  },
  { immediate: true },
);
</script>

<template>
  <UModal v-model:open="open">
    <template #content>
      <div class="p-4 sm:p-6 space-y-4">
        <p class="label text-center">READY TO JOIN?</p>

        <!-- Camera preview thumbnail -->
        <div class="relative aspect-video bg-[#111] border border-[var(--color-border)] max-w-[320px] mx-auto overflow-hidden">
          <video ref="videoEl" autoplay muted playsinline class="w-full h-full object-contain bg-black" />
          <p v-if="!media.localStream.value" class="absolute inset-0 flex items-center justify-center text-[var(--color-muted)] text-xs">
            {{ media.error.value || "Starting camera..." }}
          </p>
        </div>

        <!-- Quick toggles -->
        <div class="flex justify-center gap-3">
          <button
            class="w-10 h-10 flex items-center justify-center border-2 transition-colors"
            :class="media.audioMuted.value
              ? 'border-[var(--color-danger)] bg-[var(--color-danger)] text-white'
              : 'border-[var(--color-border-muted)] text-[var(--color-border)] hover:bg-white/5'"
            @click="media.toggleAudio()"
          >
            <UIcon :name="media.audioMuted.value ? 'i-lucide-mic-off' : 'i-heroicons-microphone'" class="text-base" />
          </button>
          <button
            class="w-10 h-10 flex items-center justify-center border-2 transition-colors"
            :class="media.videoMuted.value
              ? 'border-[var(--color-danger)] bg-[var(--color-danger)] text-white'
              : 'border-[var(--color-border-muted)] text-[var(--color-border)] hover:bg-white/5'"
            @click="media.toggleVideo()"
          >
            <UIcon :name="media.videoMuted.value ? 'i-heroicons-video-camera-slash' : 'i-heroicons-video-camera'" class="text-base" />
          </button>
        </div>

        <!-- Actions -->
        <div class="flex gap-0">
          <UButton variant="outline" class="flex-1 rounded-none border-r-0" @click="emit('cancel')">Cancel</UButton>
          <UButton class="flex-1 rounded-none" @click="emit('join')">Join Call</UButton>
        </div>
      </div>
    </template>
  </UModal>
</template>
