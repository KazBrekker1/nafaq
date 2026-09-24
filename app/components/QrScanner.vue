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
  <UModal v-model:open="open" title="Scan QR Code">
    <template #body>
      <UAlert
        v-if="error"
        color="error"
        variant="subtle"
        icon="i-heroicons-exclamation-triangle"
        :description="error"
      />
      <div v-else class="relative aspect-square w-full overflow-hidden border-2 border-default bg-black">
        <div v-if="!streaming" class="absolute inset-0 z-10 flex flex-col items-center justify-center gap-2 bg-black">
          <UIcon name="i-heroicons-camera" class="text-2xl text-dimmed" />
          <p class="text-xs text-muted">Opening camera...</p>
        </div>
        <video ref="videoRef" class="h-full w-full object-cover" />
      </div>
    </template>

    <template #footer>
      <UButton label="Cancel" variant="outline" class="w-full" @click="closeScanner" />
    </template>
  </UModal>
</template>
