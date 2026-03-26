<script setup lang="ts">
const call = useCall();
const media = useMedia();
const chat = useChat();
const transport = useMediaTransport();
const { playPeerConnected, playPeerLeft, playMessageReceived } = useNotificationSounds();

const chatOpen = ref(false);
const shareModalOpen = ref(false);
const unreadCount = ref(0);
const callDuration = ref("0:00");
const localVideoEl = ref<HTMLVideoElement | null>(null);
const videoContainer = ref<HTMLElement | null>(null);
const isFullscreen = ref(false);
let durationInterval: ReturnType<typeof setInterval> | null = null;
let cleaned = false;

const shareConnectionTicket = computed(() => call.shareTicket.value || call.ticket.value);
const remoteGridClass = computed(() => {
  const count = call.peers.value.length;
  if (count <= 1) return "grid-cols-1";
  if (count <= 4) return "grid-cols-2";
  return "grid-cols-3";
});
const remoteGridRows = computed(() => {
  const count = call.peers.value.length;
  if (count <= 2) return "1fr";
  if (count <= 4) return "repeat(2, minmax(0, 1fr))";
  return `repeat(${Math.ceil(count / 3)}, minmax(0, 1fr))`;
});

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

const peerCanvasRefs = new Map<string, (el: any) => void>();
function registerPeerCanvasRef(peerId: string) {
  let cached = peerCanvasRefs.get(peerId);
  if (!cached) {
    cached = (el: any) => {
      transport.registerPeerCanvas(peerId, el instanceof HTMLCanvasElement ? el : null);
    };
    peerCanvasRefs.set(peerId, cached);
  }
  return cached;
}

defineShortcuts({
  m: () => media.toggleAudio(),
  v: () => media.toggleVideo(),
  c: () => { chatOpen.value = !chatOpen.value; },
  f: () => toggleFullscreen(),
});

onMounted(async () => {
  if (call.state.value !== "connected" || call.peers.value.length === 0) {
    navigateTo("/");
    return;
  }

  if (!media.localStream.value) await media.startPreview();

  // Always initialize codecs — the decoder is needed for incoming media
  // even when the local camera is unavailable
  await transport.initCodecs(media.localStream.value);

  // Start receiving from all peers (per-peer canvases registered via template refs)
  await transport.startReceiving(() => call.peers.value);

  // Start sending media to all connected peers
  if (media.localStream.value && call.peers.value.length > 0) {
    await transport.startSending(media.localStream.value);
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

watch(() => call.peers.value, async (peerIds, oldPeerIds) => {
  await transport.syncSubscriptions(peerIds);
  if (media.localStream.value && peerIds.length > 0 && !transport.encoding.value) {
    await transport.startSending(media.localStream.value);
  }
  // Notification sounds
  if (oldPeerIds && peerIds.length > oldPeerIds.length) playPeerConnected();
  if (oldPeerIds && peerIds.length < oldPeerIds.length) playPeerLeft();
}, { deep: true });

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
    await transport.restartSending(newStream);
  }
});

// Unread message tracking + notification sound
watch(() => chat.messages.value.length, (newLen, oldLen) => {
  if (newLen > (oldLen ?? 0)) {
    const latest = chat.messages.value[newLen - 1];
    if (latest?.sender === "peer") {
      if (!chatOpen.value) {
        unreadCount.value++;
        playMessageReceived();
      }
    }
  }
});

watch(chatOpen, (open) => {
  if (open) unreadCount.value = 0;
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
  if (call.peers.value.length > 0) {
    chat.sendMessageToAll(text);
  }
}
</script>

<template>
  <div class="h-screen flex relative bg-black safe-area-inset overflow-hidden">
    <!-- Disconnect toast -->
    <DisconnectToast
      v-if="call.lastDisconnectedPeer.value"
      :key="call.lastDisconnectedPeer.value.id"
      :name="call.lastDisconnectedPeer.value.name"
    />

    <!-- Last peer left prompt -->
    <div
      v-if="call.allPeersLeft.value"
      class="absolute inset-0 z-30 flex items-center justify-center bg-black/80"
    >
      <div class="text-center space-y-4">
        <p class="text-sm text-[var(--color-muted)] tracking-wider">Everyone has left</p>
        <UButton class="rounded-none" @click="handleEndCall">Leave Call</UButton>
      </div>
    </div>

    <div class="flex-1 min-w-0 bg-[var(--color-surface-alt)] relative flex flex-col">
      <!-- Top bar -->
      <div class="absolute top-0 left-0 right-0 flex justify-between px-3 sm:px-4 py-2 sm:py-3 z-20 bg-gradient-to-b from-black/80 to-transparent">
        <div class="flex items-center gap-3 sm:gap-4">
          <span class="text-xs sm:text-sm font-black tracking-widest">{{ callDuration }}</span>
          <span class="text-[9px] sm:text-[10px] text-[var(--color-muted)] tracking-wider">
            {{ call.peers.value.length }} peer{{ call.peers.value.length !== 1 ? "s" : "" }}
          </span>
        </div>
        <div class="flex items-center gap-2 sm:gap-3">
          <CallConnectionQuality :quality="transport.connectionQuality.value" />
          <span class="text-[9px] sm:text-[10px] text-[var(--color-accent)] tracking-widest font-bold">P2P</span>
          <button class="text-[var(--color-muted)] hover:text-white transition-colors hidden sm:block" @click="toggleFullscreen">
            <UIcon
              :name="isFullscreen ? 'i-heroicons-arrows-pointing-in' : 'i-heroicons-arrows-pointing-out'"
              class="text-sm"
            />
          </button>
        </div>
      </div>

      <!-- Video area -->
      <div ref="videoContainer" class="flex-1 min-h-0 relative px-1 pb-1 pt-11 sm:px-2 sm:pb-2 sm:pt-14">
        <!-- Per-peer video grid -->
        <div
          class="w-full h-full grid auto-rows-fr gap-1 sm:gap-2"
          :class="remoteGridClass"
          :style="{ gridTemplateRows: remoteGridRows }"
        >
          <div
            v-for="peer in call.peers.value"
            :key="peer"
            class="relative min-h-0 bg-[#111] overflow-hidden border border-[var(--color-border)] flex items-center justify-center"
          >
            <canvas
              :ref="registerPeerCanvasRef(peer)"
              class="block w-auto h-auto max-w-full max-h-full border-2 border-transparent transition-all duration-200 bg-black"
              :class="{ 'speaking-glow': transport.peerSpeakingMap.value[peer] }"
              @dblclick="toggleFullscreen"
            />
            <span class="absolute bottom-1 left-2 text-[9px] text-[var(--color-muted)] bg-black/70 px-2 py-0.5 font-mono">
              {{ peer.slice(0, 12) }}...
            </span>
            <div v-if="transport.activeSpeaker.value === peer" class="absolute top-1 right-1">
              <span class="text-[8px] text-[var(--color-accent)] bg-black/70 px-1.5 py-0.5 font-bold tracking-wider">SPEAKER</span>
            </div>
          </div>
        </div>

        <!-- Fallback when no peers -->
        <div v-if="call.peers.value.length === 0" class="text-center z-10">
          <span class="text-[10px] text-[var(--color-muted)]">Waiting for peers...</span>
        </div>

        <!-- Self PiP -->
        <div class="absolute bottom-3 right-3 sm:bottom-4 sm:right-4 w-[128px] aspect-video sm:w-[208px] bg-[#111] border-2 border-[var(--color-border)] overflow-hidden z-10">
          <video ref="localVideoEl" autoplay muted playsinline class="w-full h-full object-contain bg-black" />
          <CallSelfVideoOverlay :audio-muted="media.audioMuted.value" :video-muted="media.videoMuted.value" />
          <span class="absolute bottom-0.5 left-1.5 sm:bottom-1 sm:left-2 text-[8px] sm:text-[9px] text-[var(--color-accent)] bg-black/70 px-1.5 sm:px-2 py-0.5 font-bold tracking-wider">You</span>
        </div>
      </div>

      <!-- Controls -->
      <div class="shrink-0 border-t border-[var(--color-border-muted)] bg-black/90 px-3 py-2.5 sm:px-4 sm:py-3">
        <div class="flex items-center justify-between gap-3">
          <div class="flex min-w-[42px] sm:min-w-[56px] gap-[2px] items-end">
            <div
              v-for="i in 8"
              :key="i"
              class="w-[3px]"
              :style="{
                height: `${3 + (i <= media.micLevel.value / 12 ? (media.micLevel.value / 12) * 1.5 : 0)}px`,
                background: i <= media.micLevel.value / 12 ? 'var(--color-accent)' : 'var(--color-border-muted)'
              }"
            />
          </div>
          <div class="flex-1 flex justify-center">
            <CallControls
              :audio-muted="media.audioMuted.value"
              :video-muted="media.videoMuted.value"
              :chat-open="chatOpen"
              :can-share-connection="!!shareConnectionTicket"
              :microphones="media.microphones.value"
              :cameras="media.cameras.value"
              :selected-mic="media.selectedMic.value"
              :selected-camera="media.selectedCamera.value"
              :unread-count="unreadCount"
              @toggle-audio="media.toggleAudio()"
              @toggle-video="media.toggleVideo()"
              @toggle-chat="chatOpen = !chatOpen"
              @show-connection="shareModalOpen = true"
              @end-call="handleEndCall"
              @switch-mic="media.switchMic($event)"
              @switch-camera="media.switchCamera($event)"
            />
          </div>
          <div class="min-w-[42px] sm:min-w-[56px]" />
        </div>
      </div>
    </div>

    <!-- Chat sidebar: overlay on mobile, side panel on desktop -->
    <ChatSidebar
      v-if="chatOpen"
      :messages="chat.messages.value"
      :peer-id="call.peerId.value || ''"
      :display-name="call.displayName.value"
      :peer-names="call.peerNames.value"
      @send="handleSendChat"
      @close="chatOpen = false"
    />

    <ConnectionShareModal
      :open="shareModalOpen"
      :ticket="shareConnectionTicket"
      title="SHARE CONNECTION"
      description="Use this QR code or connection string to bring another device into the call."
      @close="shareModalOpen = false"
    />
  </div>
</template>
