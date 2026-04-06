<script setup lang="ts">
import { useWakeLock } from "@vueuse/core";

const call = useCall();
const media = useMedia();
const chat = useChat();
const transport = useMediaTransport();
const { playPeerConnected, playPeerLeft, playMessageReceived } = useNotificationSounds();
const { starFromCall, contacts } = useContacts();
const { request: requestWakeLock, release: releaseWakeLock } = useWakeLock();

const starredPeers = ref<Set<string>>(new Set());

async function handleStar(peerId: string) {
  const name = call.peerNames.value[peerId] || peerId.slice(0, 12);
  await starFromCall(peerId, name);
  starredPeers.value = new Set([...starredPeers.value, peerId]);
}

function isPeerStarred(peerId: string): boolean {
  return starredPeers.value.has(peerId) || contacts.value.some(c => c.node_id === peerId);
}

const chatOpen = ref(false);
const shareModalOpen = ref(false);
const unreadCount = ref(0);
const callDuration = ref("0:00");
const localVideoEl = ref<HTMLVideoElement | null>(null);
const lobbyVideoEl = ref<HTMLVideoElement | null>(null);
const videoContainer = ref<HTMLElement | null>(null);
const isFullscreen = ref(false);
let durationInterval: ReturnType<typeof setInterval> | null = null;
let cleaned = false;

const isLobby = computed(() => call.state.value !== "connected");

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

const peerVideoContainerRefs = new Map<string, (el: any) => void>();
function registerPeerContainerRef(peerId: string) {
  let cached = peerVideoContainerRefs.get(peerId);
  if (!cached) {
    cached = (el: any) => {
      if (el instanceof HTMLElement) {
        el.dataset.peerId = peerId;
        videoVisibilityObserver?.observe(el);
      }
    };
    peerVideoContainerRefs.set(peerId, cached);
  }
  return cached;
}

let videoVisibilityObserver: IntersectionObserver | null = null;

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

// ── Mount: redirect if no reason to be here, start camera preview ────
onMounted(async () => {
  if (call.state.value === "idle" || call.state.value === "ringing") {
    navigateTo("/");
    return;
  }

  requestWakeLock("screen");

  // Start camera preview for lobby (or active call)
  if (!media.localStream.value) await media.startPreview();

  document.addEventListener("fullscreenchange", onFullscreenChange);
});

// ── Transition: lobby → active call when state becomes connected ─────
watch(() => call.state.value, async (newState, oldState) => {
  if (newState === "connected" && oldState !== "connected") {
    // Clean up any previous instances (e.g. peer reconnect scenario)
    if (durationInterval) { clearInterval(durationInterval); durationInterval = null; }
    videoVisibilityObserver?.disconnect();

    videoVisibilityObserver = new IntersectionObserver((entries) => {
      for (const entry of entries) {
        const peerId = (entry.target as HTMLElement).dataset.peerId;
        if (peerId) {
          transport.setPeerVideoPaused(peerId, !entry.isIntersecting);
        }
      }
    }, { threshold: 0.1 });

    await transport.initCodecs(media.localStream.value);
    await transport.startReceiving(() => call.peers.value);

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
  }
});

// ── Bind lobby video element ─────────────────────────────────────────
watch([() => media.localStream.value, lobbyVideoEl], ([stream, el]) => {
  if (el) el.srcObject = stream || null;
}, { immediate: true });

// ── Bind active-call video element ───────────────────────────────────
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
  releaseWakeLock();
  videoVisibilityObserver?.disconnect();
  videoVisibilityObserver = null;
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
  <div class="h-full flex relative bg-black safe-area-inset overflow-hidden">

    <!-- ═══════════════════════ LOBBY VIEW ═══════════════════════ -->
    <template v-if="isLobby">
      <div class="flex-1 flex flex-col items-center justify-center p-6 gap-6">

        <!-- Camera preview -->
        <div class="relative w-full max-w-md aspect-video bg-black border-2 border-[var(--color-border)] overflow-hidden">
          <video ref="lobbyVideoEl" autoplay muted playsinline class="w-full h-full object-contain bg-black" />
          <div v-if="!media.localStream.value" class="absolute inset-0 flex flex-col items-center justify-center bg-black gap-2">
            <UIcon name="i-heroicons-video-camera" class="text-2xl text-[var(--color-border-muted)]" />
            <p class="text-[var(--color-muted)] text-xs">
              {{ media.error.value || "Starting camera..." }}
            </p>
          </div>
        </div>

        <!-- Mic / camera toggles + VU meter -->
        <div class="flex items-center gap-4">
          <button
            class="w-10 h-10 flex items-center justify-center border-2 transition-colors"
            :class="media.audioMuted.value
              ? 'border-[var(--color-danger)] bg-[var(--color-danger)] text-white'
              : 'border-[var(--color-border-muted)] text-[var(--color-border)] hover:bg-white/5'"
            @click="media.toggleAudio()"
          >
            <UIcon :name="media.audioMuted.value ? 'i-lucide-mic-off' : 'i-heroicons-microphone'" class="text-base" />
          </button>
          <div class="h-1.5 w-24 bg-[var(--color-border-muted)]">
            <div class="h-full bg-[var(--color-accent)] transition-all duration-75" :style="{ width: `${media.micLevel.value * 100}%` }" />
          </div>
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

        <!-- State indicator + ticket display (reuse TicketCreate) -->
        <div v-if="call.state.value === 'joining'" class="text-xs text-[var(--color-muted)] tracking-widest text-center uppercase">
          Connecting...
        </div>
        <div v-else class="w-full max-w-md">
          <TicketCreate :ticket="shareConnectionTicket" :state="call.state.value" :disabled="true" />
        </div>

        <!-- Cancel -->
        <button
          class="border-2 border-[var(--color-border-muted)] px-6 py-2 text-[10px] font-bold tracking-widest text-[var(--color-muted)] hover:border-[var(--color-danger)] hover:text-[var(--color-danger)] transition-colors"
          @click="handleEndCall"
        >
          CANCEL
        </button>
      </div>
    </template>

    <!-- ═══════════════════ ACTIVE CALL VIEW ═══════════════════ -->
    <template v-else>
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
            <button class="text-[var(--color-muted)] hover:text-white transition-colors flex items-center justify-center w-11 h-11 sm:w-auto sm:h-auto" @click="toggleFullscreen">
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
              :ref="registerPeerContainerRef(peer)"
              :data-peer-id="peer"
              class="relative min-h-0 bg-black overflow-hidden border border-[var(--color-border)] flex items-center justify-center"
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
              <div class="absolute top-1 right-1 flex items-center gap-1">
                <span v-if="transport.activeSpeaker.value === peer" class="text-[8px] text-[var(--color-accent)] bg-black/70 px-1.5 py-0.5 font-bold tracking-wider">SPEAKER</span>
                <button
                  class="text-base leading-none bg-black/70 px-1.5 py-0.5 transition-colors"
                  :class="isPeerStarred(peer) ? 'text-yellow-400' : 'text-[var(--color-muted)] hover:text-yellow-400'"
                  :title="isPeerStarred(peer) ? 'Saved as contact' : 'Save as contact'"
                  @click.stop="handleStar(peer)"
                >&#9733;</button>
              </div>
            </div>
          </div>

          <!-- Fallback when no peers -->
          <div v-if="call.peers.value.length === 0" class="text-center z-10">
            <span class="text-[10px] text-[var(--color-muted)]">Waiting for peers...</span>
          </div>

          <!-- Self PiP -->
          <div class="absolute bottom-3 right-3 sm:bottom-4 sm:right-4 w-[128px] aspect-video sm:w-[208px] bg-black border-2 border-[var(--color-border)] overflow-hidden z-10">
            <div v-if="!media.localStream.value" class="absolute inset-0 flex items-center justify-center bg-black">
              <UIcon name="i-heroicons-video-camera" class="text-lg text-[var(--color-border-muted)]" />
            </div>
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
        v-model:open="shareModalOpen"
        :ticket="shareConnectionTicket"
        title="SHARE CONNECTION"
        description="Use this QR code or connection string to bring another device into the call."
      />
    </template>
  </div>
</template>
