<script setup lang="ts">
const call = useCall();
const media = useMedia();
const videoEl = ref<HTMLVideoElement | null>(null);

onMounted(async () => {
  if (call.state.value === "idle") {
    navigateTo("/");
    return;
  }
  await media.startPreview();
});

watch(
  () => media.localStream.value,
  (stream) => {
    if (videoEl.value && stream) videoEl.value.srcObject = stream;
  },
);

function cancel() {
  media.stopPreview();
  call.endCall();
}
</script>

<template>
  <div class="min-h-screen flex items-center justify-center p-8">
    <div class="border-2 border-[var(--color-border)] flex max-w-4xl w-full">
      <!-- Camera Preview -->
      <div class="flex-[1.3] bg-[#111] relative border-r-2 border-[var(--color-border)] min-h-[400px] flex items-center justify-center">
        <video ref="videoEl" autoplay muted playsinline class="w-full h-full object-cover absolute inset-0" />
        <p v-if="!media.localStream.value" class="text-[var(--color-muted)] text-sm tracking-widest relative z-10">
          {{ media.error.value || "Starting camera..." }}
        </p>
        <div class="absolute top-3 left-4 z-10">
          <span class="text-[var(--color-accent)] text-xs font-bold tracking-widest">● Live</span>
        </div>
      </div>

      <!-- Controls -->
      <div class="flex-1 p-8 flex flex-col justify-center gap-5">
        <div>
          <p class="label mb-2">CAMERA</p>
          <select class="input text-xs p-2 w-full bg-black text-[var(--color-border)] border-2 border-[var(--color-border)] font-mono"
            :value="media.selectedCamera.value"
            @change="media.switchCamera(($event.target as HTMLSelectElement).value)">
            <option v-for="cam in media.cameras.value" :key="cam.deviceId" :value="cam.deviceId">{{ cam.label }}</option>
          </select>
        </div>

        <div>
          <p class="label mb-2">MICROPHONE</p>
          <select class="input text-xs p-2 w-full bg-black text-[var(--color-border)] border-2 border-[var(--color-border)] font-mono"
            :value="media.selectedMic.value"
            @change="media.switchMic(($event.target as HTMLSelectElement).value)">
            <option v-for="mic in media.microphones.value" :key="mic.deviceId" :value="mic.deviceId">{{ mic.label }}</option>
          </select>
        </div>

        <div>
          <p class="label mb-2">MIC LEVEL</p>
          <div class="flex gap-[3px] h-4 items-end">
            <div v-for="i in 10" :key="i" class="w-1"
              :style="{ height: `${4 + (i <= media.micLevel.value / 10 ? (media.micLevel.value / 10) * 1.2 : 0)}px`,
                background: i <= media.micLevel.value / 10 ? 'var(--color-accent)' : 'var(--color-border-muted)' }" />
          </div>
        </div>

        <div class="flex gap-0 mt-2">
          <UButton variant="outline" class="text-xs flex-1 rounded-none border-r-0" @click="cancel">Cancel</UButton>
          <UButton class="text-xs flex-1 rounded-none" disabled>
            {{ call.state.value === "waiting" ? "Waiting..." : call.state.value === "joining" ? "Connecting..." : "Ready" }}
          </UButton>
        </div>
      </div>
    </div>
  </div>
</template>
