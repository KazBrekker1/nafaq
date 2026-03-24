<script setup lang="ts">
import { ref, onMounted } from "vue";
import { useRouter } from "vue-router";
import { useCall } from "../composables/useCall";
import { useMedia } from "../composables/useMedia";
import { useChat } from "../composables/useChat";
import CallControls from "../components/CallControls.vue";
import ChatSidebar from "../components/ChatSidebar.vue";
import VideoGrid from "../components/VideoGrid.vue";

const router = useRouter();
const call = useCall();
const media = useMedia();
const chat = useChat();

const chatOpen = ref(true);
const callDuration = ref("0:00");
let durationInterval: ReturnType<typeof setInterval> | null = null;
let startTime = Date.now();

onMounted(() => {
  if (call.state.value !== "connected") {
    router.push("/");
    return;
  }

  if (!media.localStream.value) {
    media.startPreview();
  }

  startTime = Date.now();
  durationInterval = setInterval(() => {
    const elapsed = Math.floor((Date.now() - startTime) / 1000);
    const mins = Math.floor(elapsed / 60);
    const secs = elapsed % 60;
    callDuration.value = `${mins}:${secs.toString().padStart(2, "0")}`;
  }, 1000);
});

function handleEndCall() {
  if (durationInterval) clearInterval(durationInterval);
  media.stopPreview();
  chat.clearMessages();
  call.endCall();
}

function handleSendChat(text: string) {
  if (call.peerId.value) {
    chat.sendMessage(call.peerId.value, text);
  }
}
</script>

<template>
  <div class="h-screen flex">
    <!-- Video area -->
    <div class="flex-1 bg-[var(--color-surface-alt)] relative flex flex-col">
      <!-- Top bar -->
      <div class="absolute top-0 left-0 right-0 flex justify-between px-4 py-3 z-20 bg-gradient-to-b from-black/80 to-transparent">
        <span class="text-sm font-black tracking-widest">{{ callDuration }}</span>
        <div class="flex items-center gap-2">
          <div class="w-2 h-2 bg-[var(--color-accent)]"></div>
          <span class="text-[10px] text-[var(--color-accent)] tracking-widest font-bold">P2P Direct</span>
        </div>
      </div>

      <!-- Video grid -->
      <div class="flex-1">
        <VideoGrid
          :localStream="media.localStream.value"
          :peers="call.peers.value"
        />
      </div>

      <!-- Bottom controls -->
      <div
        class="absolute bottom-0 left-0 py-3.5 z-20 bg-gradient-to-t from-black/80 to-transparent"
        :class="chatOpen ? 'right-[260px]' : 'right-0'"
      >
        <CallControls
          :audioMuted="media.audioMuted.value"
          :videoMuted="media.videoMuted.value"
          :chatOpen="chatOpen"
          @toggleAudio="media.toggleAudio"
          @toggleVideo="media.toggleVideo"
          @toggleChat="chatOpen = !chatOpen"
          @endCall="handleEndCall"
        />
      </div>
    </div>

    <!-- Chat sidebar -->
    <ChatSidebar
      v-if="chatOpen"
      :messages="chat.messages.value"
      :peerId="call.peerId.value || ''"
      @send="handleSendChat"
    />
  </div>
</template>
