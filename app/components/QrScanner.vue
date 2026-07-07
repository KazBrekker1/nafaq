<script setup lang="ts">
import QrScanner from "qr-scanner";

const open = defineModel<boolean>('open', { required: true });
const emit = defineEmits<{ scan: [ticket: string] }>();

const videoRef = ref<HTMLVideoElement | null>(null);
const scanner = ref<QrScanner | null>(null);
const error = ref<string | null>(null);
const streaming = ref(false);

watch(() => open.value, async (isOpen) => {
  if (isOpen) {
    await nextTick();
    startScanner();
  } else {
    destroyScanner();
  }
});

async function startScanner() {
  if (!videoRef.value) return;

  const instance = new QrScanner(
    videoRef.value,
    (result) => {
      if (result.data) {
        instance.stop();
        emit("scan", result.data);
        open.value = false;
      }
    },
    {
      preferredCamera: "environment",
      highlightScanRegion: true,
      highlightCodeOutline: true,
    },
  );
  scanner.value = instance;

  try {
    await instance.start();
    // The modal may have closed while start() was in flight — destroyScanner
    // already ran, and a started orphan would hold the camera indefinitely.
    if (scanner.value !== instance) {
      instance.destroy();
      return;
    }
    streaming.value = true;
  } catch {
    if (scanner.value !== instance) {
      instance.destroy();
      return;
    }
    error.value = "Camera access denied.";
  }
}

function destroyScanner() {
  scanner.value?.destroy();
  scanner.value = null;
  streaming.value = false;
  error.value = null;
}

function closeScanner() {
  open.value = false;
}

onBeforeUnmount(() => {
  destroyScanner();
});
</script>

<template>
  <UModal v-model:open="open">
    <template #content>
      <div class="border-2 border-[var(--color-border)] bg-[var(--color-surface-alt)] shadow-2xl">
        <div class="flex items-center justify-between border-b border-[var(--color-border-muted)] p-3 sm:p-4">
          <p class="label">SCAN QR CODE</p>
          <button
            class="text-[var(--color-muted)] transition-colors hover:text-white"
            aria-label="Close scanner"
            @click="closeScanner"
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

          <UButton variant="outline" class="w-full rounded-none mt-3" @click="closeScanner">
            Cancel
          </UButton>
        </div>
      </div>
    </template>
  </UModal>
</template>
