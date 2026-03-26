<script setup lang="ts">
const call = useCall();
const media = useMedia();
const chat = useChat();
const transport = useMediaTransport();

const chatOpen = ref(true);
const callDuration = ref("0:00");
const localVideoEl = ref<HTMLVideoElement | null>(null);
const remoteCanvasEl = ref<HTMLCanvasElement | null>(null);
const videoContainer = ref<HTMLElement | null>(null);
const isFullscreen = ref(false);
let durationInterval: ReturnType<typeof setInterval> | null = null;
let cleaned = false;

async function cleanup() {
  if (cleaned) return;
  cleaned = true;
  if (durationInterval) { clearInterval(durationInterval); durationInterval = null; }
  await transport.stop();
  media.stopPreview();
}

function toggleFullscreen() {
  if (!videoContainer.value) return;
  if (document.fullscreenElement) {
    document.exitFullscreen();
  } else {
    videoContainer.value.requestFullscreen();
  }
}

function onFullscreenChange() {
  isFullscreen.value = !!document.fullscreenElement;
}

defineShortcuts({
  m: () => media.toggleAudio(),
  v: () => media.toggleVideo(),
  c: () => { chatOpen.value = !chatOpen.value; },
  f: () => toggleFullscreen(),
});

onMounted(async () => {
  if (call.state.value !== "connected") {
    navigateTo("/");
    return;
  }

  if (!media.localStream.value) await media.startPreview();

  // Always initialize codecs — the decoder is needed for incoming media
  // even when the local camera is unavailable
  await transport.initCodecs(media.localStream.value);

  // Start sending media to all connected peers
  if (media.localStream.value && call.peers.value.length > 0) {
    transport.startSending(media.localStream.value, () => call.peers.value);
  }

  const startTime = Date.now();
  durationInterval = setInterval(() => {
    const elapsed = Math.floor((Date.now() - startTime) / 1000);
    const mins = Math.floor(elapsed / 60);
    const secs = elapsed % 60;
    callDuration.value = `${mins}:${secs.toString().padStart(2, "0")}`;
  }, 1000);

  document.addEventListener("fullscreenchange", onFullscreenChange);
});

watch([() => media.localStream.value, localVideoEl], ([stream, el]) => {
  if (el) el.srcObject = stream || null;
}, { immediate: true });

watch(remoteCanvasEl, (canvas) => {
  if (canvas) transport.startReceiving(canvas);
});

// Restart transport when device is switched mid-call.
// stopPreview() nullifies the stream before startPreview() sets the new one,
// so we can't rely on oldStream — just check if we were actively encoding.
let wasEncoding = false;
watch(() => media.localStream.value, async (newStream) => {
  if (!newStream) {
    wasEncoding = transport.encoding.value;
    return;
  }
  if (wasEncoding && call.peers.value.length > 0) {
    wasEncoding = false;
    await transport.restartSending(newStream, () => call.peers.value);
  }
});

onUnmounted(() => {
  cleanup();
  document.removeEventListener("fullscreenchange", onFullscreenChange);
});

function handleEndCall() {
  cleanup();
  chat.clearMessages();
  call.endCall();
}

function handleSendChat(text: string) {
  if (call.peers.value.length > 0) chat.sendMessageToAll(call.peers.value, text);
}
</script>

<template>
  <div class="h-screen flex">
    <div class="flex-1 bg-[var(--color-surface-alt)] relative flex flex-col">
      <!-- Top bar -->
      <div class="absolute top-0 left-0 right-0 flex justify-between px-4 py-3 z-20 bg-gradient-to-b from-black/80 to-transparent">
        <div class="flex items-center gap-4">
          <span class="text-sm font-black tracking-widest">{{ callDuration }}</span>
          <span class="text-[10px] text-[var(--color-muted)] tracking-wider">
            {{ call.peers.value.length }} peer{{ call.peers.value.length !== 1 ? "s" : "" }}
          </span>
        </div>
        <div class="flex items-center gap-3">
          <CallConnectionQuality :quality="transport.connectionQuality.value" />
          <span class="text-[10px] text-[var(--color-accent)] tracking-widest font-bold">P2P</span>
          <UTooltip text="Fullscreen" :kbds="['F']">
            <button class="text-[var(--color-muted)] hover:text-white transition-colors" @click="toggleFullscreen">
              <UIcon
                :name="isFullscreen ? 'i-heroicons-arrows-pointing-in' : 'i-heroicons-arrows-pointing-out'"
                class="text-sm"
              />
            </button>
          </UTooltip>
        </div>
      </div>

      <!-- Video area -->
      <div ref="videoContainer" class="flex-1 relative flex items-center justify-center">
        <!-- Remote video -->
        <canvas
          ref="remoteCanvasEl"
          class="w-full h-full object-contain absolute inset-0 border-2 border-transparent transition-all duration-200"
          :class="{ 'speaking-glow': transport.peerSpeaking.value }"
          @dblclick="toggleFullscreen"
        />

        <!-- Peer info overlay (shown when no remote video) -->
        <div class="text-center z-10">
          <div v-for="peer in call.peers.value" :key="peer" class="mt-2">
            <span class="text-[10px] text-[var(--color-muted)] bg-black/50 px-3 py-1 font-mono">
              {{ peer.slice(0, 16) }}...
            </span>
          </div>
        </div>

        <!-- Self PiP -->
        <div class="absolute bottom-20 right-4 w-[200px] h-[130px] bg-[#111] border-2 border-[var(--color-border)] overflow-hidden z-10">
          <video ref="localVideoEl" autoplay muted playsinline class="w-full h-full object-cover" />
          <CallSelfVideoOverlay :audio-muted="media.audioMuted.value" :video-muted="media.videoMuted.value" />
          <span class="absolute bottom-1 left-2 text-[9px] text-[var(--color-accent)] bg-black/70 px-2 py-0.5 font-bold tracking-wider">You</span>
        </div>
      </div>

      <!-- Mic level indicator -->
      <div class="absolute bottom-16 left-4 z-20 flex gap-[2px] items-end">
        <div v-for="i in 8" :key="i" class="w-[3px]"
          :style="{
            height: `${3 + (i <= media.micLevel.value / 12 ? (media.micLevel.value / 12) * 1.5 : 0)}px`,
            background: i <= media.micLevel.value / 12 ? 'var(--color-accent)' : 'var(--color-border-muted)'
          }" />
      </div>

      <!-- Controls -->
      <div class="absolute bottom-0 left-0 py-3.5 z-20 bg-gradient-to-t from-black/80 to-transparent"
        :class="chatOpen ? 'right-[260px]' : 'right-0'">
        <CallControls
          :audio-muted="media.audioMuted.value"
          :video-muted="media.videoMuted.value"
          :chat-open="chatOpen"
          :microphones="media.microphones.value"
          :cameras="media.cameras.value"
          :selected-mic="media.selectedMic.value"
          :selected-camera="media.selectedCamera.value"
          @toggle-audio="media.toggleAudio()"
          @toggle-video="media.toggleVideo()"
          @toggle-chat="chatOpen = !chatOpen"
          @end-call="handleEndCall"
          @switch-mic="media.switchMic($event)"
          @switch-camera="media.switchCamera($event)"
        />
      </div>
    </div>

    <ChatSidebar
      v-if="chatOpen"
      :messages="chat.messages.value"
      :peer-id="call.peerId.value || ''"
      @send="handleSendChat"
    />
  </div>
</template>
