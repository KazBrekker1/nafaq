<script setup lang="ts">
const call = useCall();
const media = useMedia();
const chat = useChat();

const chatOpen = ref(true);
const callDuration = ref("0:00");
let durationInterval: ReturnType<typeof setInterval> | null = null;
let cleaned = false;

function cleanup() {
  if (cleaned) return;
  cleaned = true;
  if (durationInterval) { clearInterval(durationInterval); durationInterval = null; }
}

onMounted(async () => {
  if (call.state.value !== "connected") {
    navigateTo("/");
    return;
  }

  if (!media.localStream.value) await media.startPreview();

  const startTime = Date.now();
  durationInterval = setInterval(() => {
    const elapsed = Math.floor((Date.now() - startTime) / 1000);
    const mins = Math.floor(elapsed / 60);
    const secs = elapsed % 60;
    callDuration.value = `${mins}:${secs.toString().padStart(2, "0")}`;
  }, 1000);
});

onUnmounted(() => { cleanup(); });

function handleEndCall() {
  cleanup();
  media.stopPreview();
  chat.clearMessages();
  call.endCall();
}

function handleSendChat(text: string) {
  if (call.peerId.value) chat.sendMessage(call.peerId.value, text);
}
</script>

<template>
  <div class="h-screen flex">
    <div class="flex-1 bg-[var(--color-surface-alt)] relative flex flex-col">
      <!-- Top bar -->
      <div class="absolute top-0 left-0 right-0 flex justify-between px-4 py-3 z-20 bg-gradient-to-b from-black/80 to-transparent">
        <span class="text-sm font-black tracking-widest">{{ callDuration }}</span>
        <div class="flex items-center gap-2">
          <div class="w-2 h-2 bg-[var(--color-accent)]" />
          <span class="text-[10px] text-[var(--color-accent)] tracking-widest font-bold">P2P Direct</span>
        </div>
      </div>

      <!-- Video area -->
      <div class="flex-1 relative flex items-center justify-center">
        <!-- Remote placeholder -->
        <span class="text-[var(--color-border-muted)] text-sm font-bold tracking-widest">Waiting for video...</span>
        <!-- Self PiP -->
        <div class="absolute bottom-20 right-4 w-[180px] h-[110px] bg-[#111] border-2 border-[var(--color-border)] overflow-hidden z-10">
          <video ref="localVideoEl" autoplay muted playsinline class="w-full h-full object-cover" />
        </div>
      </div>

      <!-- Controls -->
      <div class="absolute bottom-0 left-0 py-3.5 z-20 bg-gradient-to-t from-black/80 to-transparent"
        :class="chatOpen ? 'right-[260px]' : 'right-0'">
        <CallControls
          :audio-muted="media.audioMuted.value"
          :video-muted="media.videoMuted.value"
          :chat-open="chatOpen"
          @toggle-audio="media.toggleAudio()"
          @toggle-video="media.toggleVideo()"
          @toggle-chat="chatOpen = !chatOpen"
          @end-call="handleEndCall"
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
