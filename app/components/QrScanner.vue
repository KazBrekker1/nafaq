<script setup lang="ts">
import QrScanner from "qr-scanner";

const emit = defineEmits<{ scan: [ticket: string]; close: [] }>();

const videoRef = ref<HTMLVideoElement | null>(null);
const scanner = ref<QrScanner | null>(null);
const error = ref<string | null>(null);
const streaming = ref(false);

onMounted(async () => {
  if (!videoRef.value) return;

  scanner.value = new QrScanner(
    videoRef.value,
    (result) => {
      if (result.data) {
        scanner.value?.stop();
        emit("scan", result.data);
      }
    },
    {
      preferredCamera: "environment",
      highlightScanRegion: true,
      highlightCodeOutline: true,
    },
  );

  try {
    await scanner.value.start();
    streaming.value = true;
  } catch {
    error.value = "Camera access denied.";
  }
});

onBeforeUnmount(() => {
  scanner.value?.destroy();
  scanner.value = null;
});
</script>

<template>
  <div
    class="fixed inset-0 z-50 flex items-center justify-center overflow-y-auto bg-black/80 p-4"
    @click.self="emit('close')"
  >
    <div class="w-full max-w-sm my-auto border-2 border-[var(--color-border)] bg-[var(--color-surface-alt)] shadow-2xl">
      <div class="flex items-center justify-between border-b border-[var(--color-border-muted)] p-3 sm:p-4">
        <p class="label">SCAN QR CODE</p>
        <button
          class="text-[var(--color-muted)] transition-colors hover:text-white"
          aria-label="Close scanner"
          @click="emit('close')"
        >
          <UIcon name="i-heroicons-x-mark" class="text-lg" />
        </button>
      </div>

      <div class="p-3 sm:p-4">
        <div v-if="error" class="border-2 border-[var(--color-danger)] p-3 text-xs text-[var(--color-danger)] text-center">
          {{ error }}
        </div>
        <div v-else class="relative aspect-square w-full overflow-hidden bg-black">
          <div v-if="!streaming" class="absolute inset-0 z-10 flex flex-col items-center justify-center gap-2 bg-black">
            <UIcon name="i-heroicons-camera" class="text-2xl text-[var(--color-border-muted)]" />
            <p class="text-xs text-[var(--color-muted)]">Opening camera...</p>
          </div>
          <video ref="videoRef" class="h-full w-full object-cover" />
        </div>

        <UButton variant="outline" class="w-full rounded-none mt-3" @click="emit('close')">
          Cancel
        </UButton>
      </div>
    </div>
  </div>
</template>
