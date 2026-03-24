<script setup lang="ts">
import { computed, ref, watch } from "vue";

const props = defineProps<{
  localStream: MediaStream | null;
  peers: string[];
  remoteVideoFrame: VideoFrame | null;
}>();

const localVideoEl = ref<HTMLVideoElement | null>(null);
const remoteCanvasEl = ref<HTMLCanvasElement | null>(null);

watch(
  () => props.localStream,
  (stream) => {
    if (localVideoEl.value && stream) {
      localVideoEl.value.srcObject = stream;
    }
  },
);

watch(
  () => props.remoteVideoFrame,
  (frame) => {
    if (!frame || !remoteCanvasEl.value) return;
    const ctx = remoteCanvasEl.value.getContext("2d");
    if (!ctx) return;
    if (
      remoteCanvasEl.value.width !== frame.displayWidth ||
      remoteCanvasEl.value.height !== frame.displayHeight
    ) {
      remoteCanvasEl.value.width = frame.displayWidth;
      remoteCanvasEl.value.height = frame.displayHeight;
    }
    ctx.drawImage(frame, 0, 0);
    frame.close();
  },
);

const gridCols = computed(() => {
  const total = props.peers.length + 1;
  if (total <= 1) return 1;
  if (total <= 4) return 2;
  return 3;
});
</script>

<template>
  <!-- 1-on-1: full screen remote + PiP self -->
  <div v-if="peers.length === 1" class="relative w-full h-full bg-[var(--color-surface-alt)]">
    <canvas ref="remoteCanvasEl" class="w-full h-full object-contain absolute inset-0"></canvas>
    <div v-if="!remoteVideoFrame" class="w-full h-full flex items-center justify-center absolute inset-0">
      <span class="text-[var(--color-border-muted)] text-sm font-bold tracking-widest">Waiting for video...</span>
    </div>

    <div class="absolute bottom-20 right-4 w-[180px] h-[110px] bg-[#111] border-2 border-[var(--color-border)] overflow-hidden z-10">
      <video ref="localVideoEl" autoplay muted playsinline class="w-full h-full object-cover"></video>
      <span v-if="!localStream" class="absolute inset-0 flex items-center justify-center text-[var(--color-muted)] text-[10px] tracking-widest">You</span>
    </div>
  </div>

  <!-- Group: grid layout -->
  <div
    v-else
    class="w-full h-full grid gap-[2px] bg-[var(--color-border)] p-[2px]"
    :style="{ gridTemplateColumns: `repeat(${gridCols}, 1fr)` }"
  >
    <div class="bg-[#111] relative min-h-[140px] flex items-center justify-center">
      <video ref="localVideoEl" autoplay muted playsinline class="w-full h-full object-cover absolute inset-0"></video>
      <span class="absolute bottom-2 left-2.5 text-[10px] text-[var(--color-accent)] bg-black px-2 py-0.5 font-bold tracking-wider z-10">You</span>
      <div class="absolute top-2 right-2.5 w-1.5 h-1.5 bg-[var(--color-accent)] z-10"></div>
    </div>
    <div v-for="peer in peers" :key="peer" class="bg-[#111] relative min-h-[140px] flex items-center justify-center">
      <canvas ref="remoteCanvasEl" class="w-full h-full object-contain absolute inset-0"></canvas>
      <span v-if="!remoteVideoFrame" class="text-[var(--color-border-muted)] text-xs tracking-widest">Connecting...</span>
      <span class="absolute bottom-2 left-2.5 text-[10px] text-white bg-black px-2 py-0.5 font-bold tracking-wider z-10">{{ peer.slice(0, 8) }}...</span>
      <div class="absolute top-2 right-2.5 w-1.5 h-1.5 bg-[var(--color-accent)] z-10"></div>
    </div>
  </div>
</template>
