<script setup lang="ts">
import { useWakeLock } from "@vueuse/core";
import { useNodeRuntime } from "~/composables/useNodeRuntime";

const call = useCall();
const media = useMedia();
const chat = useChat();
const transport = useMediaTransport();
const { peerConnectionStatuses } = useNodeRuntime();
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

// Brief connectivity blips ("suspect") get a subtle tint so they don't alarm;
// a sustained "reconnecting" state gets the unmistakable full-tile overlay.
function isPeerSuspect(peerId: string): boolean {
  return peerConnectionStatuses.value[peerId] === "suspect";
}

function isPeerReconnecting(peerId: string): boolean {
  return peerConnectionStatuses.value[peerId] === "reconnecting";
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

const shareConnectionTicket = computed(() => call.shareTicket.value);
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

// Any state other than idle/ringing means a call is in progress or being set
// up — ringing is excluded because it's the global incoming-call banner, not
// an active session tied to this page.
function isCallActive() {
  const s = call.state.value;
  return s !== "idle" && s !== "ringing";
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

// Starts (or restarts) the receiving/sending pipeline for an active call.
// Called both on the lobby → connected transition and, defensively, on mount
// when the page is created with a call already connected (e.g. a stale
// remount) — the transition watcher below only fires on a change, so it
// never runs in that second case.
async function startConnectedPipeline() {
  // Guards against starting after an unmount-triggered cleanup — starting
  // transport after cleanup would leak it with no owner to stop it.
  if (cleaned) return;

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
  if (cleaned) return;
  await transport.startReceiving(() => call.peers.value);
  if (cleaned) return;

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

  // Defense-in-depth: the transition watcher below never fires if we
  // mounted already-connected, so start the pipeline directly here.
  if (call.state.value === "connected") {
    await startConnectedPipeline();
  }
});

// ── Leaving /call always means leaving the call — no confirmation prompt,
// the decision is unambiguous. Torn down before navigation resolves so the
// backend session and call state are already reset by the time onUnmounted
// (or a fresh mount of /call) runs. ──────────────────────────────────────
onBeforeRouteLeave(async () => {
  if (!isCallActive()) return;
  await cleanup();
  chat.clearMessages();
  await call.terminateCall({ navigate: false });
});

// ── Transition: lobby → active call when state becomes connected ─────
watch(() => call.state.value, async (newState, oldState) => {
  if (newState === "connected" && oldState !== "connected") {
    await startConnectedPipeline();
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
  if (cleaned) return;
  await transport.syncSubscriptions(peerIds);
  if (cleaned) return;
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
  if (cleaned) return;
  if (!newStream) {
    wasEncoding = transport.encoding.value;
    return;
  }
  // Reset unconditionally — a stale true (stream arrived with no peers yet)
  // must not trigger a restart on a later, unrelated stream change.
  const shouldRestart = wasEncoding && call.peers.value.length > 0;
  wasEncoding = false;
  if (shouldRestart) {
    try {
      await transport.restartSending(newStream);
    } catch (e) {
      console.warn("[call] restartSending failed:", e);
    }
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

onUnmounted(async () => {
  releaseWakeLock();
  videoVisibilityObserver?.disconnect();
  videoVisibilityObserver = null;
  document.removeEventListener("fullscreenchange", onFullscreenChange);

  await cleanup();

  // Fallback teardown: guarantees the Rust-side session and call state don't
  // outlive this page even if a leave path bypassed the route guard above.
  // No-op when handleEndCall or the route guard already tore down the call,
  // since terminateCall() always leaves state at "idle".
  if (isCallActive()) {
    chat.clearMessages();
    await call.terminateCall({ navigate: false });
  }
});

async function handleEndCall() {
  // Finish transport/media teardown before endCall navigates away, so DOM
  // cleanup (canvas dereg, capture element removal) isn't racing navigation.
  await cleanup();
  chat.clearMessages();
  await call.endCall();
}

function handleSendChat(text: string) {
  if (call.peers.value.length > 0) {
    chat.sendMessageToAll(text);
  }
}
</script>

<template>
  <div class="dark h-full flex relative bg-black safe-area-inset overflow-hidden">

    <!-- ═══════════════════════ LOBBY VIEW ═══════════════════════ -->
    <template v-if="isLobby">
      <div class="flex-1 flex flex-col items-center justify-center p-8 gap-8">

        <!-- Camera preview -->
        <div class="relative w-full max-w-md aspect-video bg-black border-2 border-(--ui-border-accented) overflow-hidden">
          <video ref="lobbyVideoEl" autoplay muted playsinline class="w-full h-full object-contain bg-black" />
          <div v-if="!media.localStream.value" class="absolute inset-0 flex flex-col items-center justify-center bg-black gap-2">
            <UIcon name="i-heroicons-video-camera" class="text-2xl text-dimmed" />
            <p class="text-muted text-xs">
              {{ media.error.value || "Starting camera..." }}
            </p>
          </div>
        </div>

        <!-- Mic / camera toggles + VU meter -->
        <div class="flex items-center gap-5">
          <UButton
            :icon="media.audioMuted.value ? 'i-lucide-mic-off' : 'i-heroicons-microphone'"
            :color="media.audioMuted.value ? 'error' : 'neutral'"
            :variant="media.audioMuted.value ? 'solid' : 'subtle'"
            size="xl"
            class="size-[48px]"
            :aria-label="media.audioMuted.value ? 'Unmute microphone' : 'Mute microphone'"
            @click="media.toggleAudio()"
          />
          <div class="h-2 w-32 bg-accented">
            <div class="h-full bg-primary transition-all duration-75" :style="{ width: `${media.micLevel.value * 100}%` }" />
          </div>
          <UButton
            :icon="media.videoMuted.value ? 'i-heroicons-video-camera-slash' : 'i-heroicons-video-camera'"
            :color="media.videoMuted.value ? 'error' : 'neutral'"
            :variant="media.videoMuted.value ? 'solid' : 'subtle'"
            size="xl"
            class="size-[48px]"
            :aria-label="media.videoMuted.value ? 'Turn camera on' : 'Turn camera off'"
            @click="media.toggleVideo()"
          />
        </div>

        <!-- State indicator + ticket display (reuse TicketCreate) -->
        <div v-if="call.state.value === 'joining'" class="text-xs text-muted tracking-widest text-center uppercase">
          Connecting...
        </div>
        <div v-else class="w-full max-w-md">
          <TicketCreate :ticket="shareConnectionTicket" :state="call.state.value" :disabled="true" />
        </div>

        <!-- Cancel -->
        <UButton
          label="Cancel"
          color="neutral"
          variant="subtle"
          size="lg"
          @click="handleEndCall"
        />
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
        <div class="text-center space-y-5">
          <p class="text-sm text-dimmed tracking-wider">Everyone has left</p>
          <UButton label="Leave Call" color="primary" variant="solid" size="lg" @click="handleEndCall" />
        </div>
      </div>

      <div class="flex-1 min-w-0 bg-black relative flex flex-col">
        <!-- Top bar -->
        <div class="absolute top-0 left-0 right-0 flex justify-between px-4 sm:px-5 py-3 sm:py-4 z-20 bg-gradient-to-b from-black/80 to-transparent">
          <div class="flex items-center gap-4 sm:gap-5">
            <span class="text-xs sm:text-sm font-black tracking-widest text-highlighted">{{ callDuration }}</span>
            <span class="text-[9px] sm:text-[10px] text-dimmed tracking-wider">
              {{ call.peers.value.length }} peer{{ call.peers.value.length !== 1 ? "s" : "" }}
            </span>
          </div>
          <div class="flex items-center gap-3 sm:gap-4">
            <CallConnectionQuality :quality="transport.connectionQuality.value" />
            <span class="text-[9px] sm:text-[10px] text-primary tracking-widest font-bold">P2P</span>
            <UButton
              :icon="isFullscreen ? 'i-heroicons-arrows-pointing-in' : 'i-heroicons-arrows-pointing-out'"
              color="neutral"
              variant="ghost"
              size="lg"
              class="size-[40px]"
              :aria-label="isFullscreen ? 'Exit fullscreen' : 'Enter fullscreen'"
              @click="toggleFullscreen"
            />
          </div>
        </div>

        <!-- Video area -->
        <div ref="videoContainer" class="flex-1 min-h-0 relative px-2 pb-2 pt-16 sm:px-3 sm:pb-3 sm:pt-[4.5rem]">
          <!-- Per-peer video grid -->
          <div
            class="w-full h-full grid auto-rows-fr gap-2 sm:gap-3"
            :class="remoteGridClass"
            :style="{ gridTemplateRows: remoteGridRows }"
          >
            <div
              v-for="peer in call.peers.value"
              :key="peer"
              :ref="registerPeerContainerRef(peer)"
              :data-peer-id="peer"
              class="relative min-h-0 bg-black overflow-hidden border-2 flex items-center justify-center"
              :style="{ borderColor: isPeerSuspect(peer) || isPeerReconnecting(peer) ? 'var(--ui-warning)' : 'var(--ui-border-accented)' }"
            >
              <canvas
                :ref="registerPeerCanvasRef(peer)"
                class="block w-auto h-auto max-w-full max-h-full border-2 border-transparent transition-all duration-200 bg-black"
                :class="{ 'speaking-glow': transport.peerSpeakingMap.value[peer] }"
                @dblclick="toggleFullscreen"
              />
              <span
                class="absolute bottom-1.5 left-2 flex items-center gap-1.5 text-[9px] bg-black/70 px-2 py-0.5 transition-colors"
                :class="isPeerSuspect(peer) ? 'text-warning' : 'text-dimmed'"
              >
                <span v-if="isPeerSuspect(peer)" class="suspect-dot" aria-hidden="true" />
                {{ peer.slice(0, 12) }}...
              </span>
              <div class="absolute top-1.5 right-1.5 flex items-center gap-1.5">
                <span v-if="transport.activeSpeaker.value === peer" class="text-[8px] text-primary bg-black/70 px-1.5 py-0.5 font-bold tracking-wider">SPEAKER</span>
                <button
                  class="flex items-center justify-center bg-black/70 p-1.5 transition-colors"
                  :class="isPeerStarred(peer) ? 'text-yellow-400' : 'text-dimmed hover:text-yellow-400'"
                  :title="isPeerStarred(peer) ? 'Saved as contact' : 'Save as contact'"
                  :aria-label="isPeerStarred(peer) ? 'Saved as contact' : 'Save as contact'"
                  @click.stop="handleStar(peer)"
                >
                  <UIcon :name="isPeerStarred(peer) ? 'i-heroicons-star-solid' : 'i-heroicons-star'" class="text-base" />
                </button>
              </div>
              <CallPeerReconnectingOverlay :reconnecting="isPeerReconnecting(peer)" />
            </div>
          </div>

          <!-- Fallback when no peers -->
          <div v-if="call.peers.value.length === 0" class="text-center z-10">
            <span class="text-[10px] text-dimmed">Waiting for peers...</span>
          </div>

          <!-- Self PiP -->
          <div class="absolute bottom-4 right-4 sm:bottom-5 sm:right-5 w-[128px] aspect-video sm:w-[208px] bg-black border-2 border-(--ui-border-accented) overflow-hidden z-10">
            <div v-if="!media.localStream.value" class="absolute inset-0 flex items-center justify-center bg-black">
              <UIcon name="i-heroicons-video-camera" class="text-lg text-dimmed" />
            </div>
            <video ref="localVideoEl" autoplay muted playsinline class="w-full h-full object-contain bg-black" />
            <CallSelfVideoOverlay :audio-muted="media.audioMuted.value" :video-muted="media.videoMuted.value" />
            <span class="absolute bottom-1 left-2 sm:bottom-1.5 sm:left-2.5 text-[8px] sm:text-[9px] text-primary bg-black/70 px-1.5 sm:px-2 py-0.5 font-bold tracking-wider">You</span>
          </div>
        </div>

        <!-- Controls -->
        <div class="shrink-0 border-t-2 border-(--ui-border-accented) bg-black/95 px-4 py-4 sm:px-5 sm:py-5">
          <div class="flex items-center justify-between gap-3">
            <div class="flex min-w-[42px] sm:min-w-[56px] gap-[2px] items-end">
              <div
                v-for="i in 8"
                :key="i"
                class="w-[3px]"
                :style="{
                  height: `${3 + (i <= media.micLevel.value / 12 ? (media.micLevel.value / 12) * 1.5 : 0)}px`,
                  background: i <= media.micLevel.value / 12 ? 'var(--ui-primary)' : 'var(--ui-border-muted)'
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

<style scoped>
.suspect-dot {
  width: 5px;
  height: 5px;
  background: var(--ui-warning);
  animation: call-suspect-pulse 1.4s ease-in-out infinite;
}

@keyframes call-suspect-pulse {
  0%, 100% { opacity: 0.35; }
  50% { opacity: 1; }
}
</style>
