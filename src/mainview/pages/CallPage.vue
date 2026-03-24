<script setup lang="ts">
import { ref, onMounted, onUnmounted } from "vue";
import { useRouter } from "vue-router";
import { useCall } from "../composables/useCall";
import { useMedia } from "../composables/useMedia";
import { useChat } from "../composables/useChat";
import { useMediaTransport } from "../composables/useMediaTransport";
import { useAudioPipeline } from "../composables/useAudioPipeline";
import { useVideoPipeline } from "../composables/useVideoPipeline";
import CallControls from "../components/CallControls.vue";
import ChatSidebar from "../components/ChatSidebar.vue";
import VideoGrid from "../components/VideoGrid.vue";

const router = useRouter();
const call = useCall();
const media = useMedia();
const chat = useChat();
const transport = useMediaTransport();
const audioPipeline = useAudioPipeline();
const videoPipeline = useVideoPipeline();

const chatOpen = ref(true);
const callDuration = ref("0:00");
const remoteVideoFrame = ref<VideoFrame | null>(null);

let durationInterval: ReturnType<typeof setInterval> | null = null;
let startTime = Date.now();

onMounted(async () => {
  if (call.state.value !== "connected") {
    router.push("/");
    return;
  }

  if (!media.localStream.value) {
    await media.startPreview();
  }

  // Connect media transport (direct WS to sidecar)
  transport.connect();

  // Set up receive handlers
  transport.setOnAudio((_peerId, data, timestamp) => {
    audioPipeline.decodeChunk(data, timestamp);
  });

  transport.setOnVideo((_peerId, data, timestamp) => {
    const isKey = data.length > 0 && (data[0] & 0x01) === 0;
    videoPipeline.decodeChunk(data, timestamp, isKey);
  });

  // Start decoders
  audioPipeline.startDecoding();
  videoPipeline.startDecoding((frame: VideoFrame) => {
    if (remoteVideoFrame.value) {
      try { remoteVideoFrame.value.close(); } catch {}
    }
    remoteVideoFrame.value = frame;
  });

  // Start encoders
  if (media.localStream.value && call.peerId.value) {
    const peerId = call.peerId.value;
    audioPipeline.startEncoding(media.localStream.value, (data) => {
      transport.sendAudio(peerId, data);
    });
    videoPipeline.startEncoding(media.localStream.value, (data) => {
      transport.sendVideo(peerId, data);
    });
  }

  // Call duration timer
  startTime = Date.now();
  durationInterval = setInterval(() => {
    const elapsed = Math.floor((Date.now() - startTime) / 1000);
    const mins = Math.floor(elapsed / 60);
    const secs = elapsed % 60;
    callDuration.value = `${mins}:${secs.toString().padStart(2, "0")}`;
  }, 1000);
});

onUnmounted(() => {
  if (durationInterval) clearInterval(durationInterval);
  audioPipeline.stop();
  videoPipeline.stop();
  transport.disconnect();
});

function handleEndCall() {
  if (durationInterval) clearInterval(durationInterval);
  audioPipeline.stop();
  videoPipeline.stop();
  transport.disconnect();
  media.stopPreview();
  chat.clearMessages();
  call.endCall();
}

function handleSendChat(text: string) {
  if (call.peerId.value) {
    chat.sendMessage(call.peerId.value, text);
  }
}

function handleToggleAudio() {
  media.toggleAudio();
  if (media.audioMuted.value) {
    audioPipeline.stopEncoding();
  } else if (media.localStream.value && call.peerId.value) {
    audioPipeline.startEncoding(media.localStream.value, (data) => {
      transport.sendAudio(call.peerId.value!, data);
    });
  }
}

function handleToggleVideo() {
  media.toggleVideo();
  if (media.videoMuted.value) {
    videoPipeline.stopEncoding();
  } else if (media.localStream.value && call.peerId.value) {
    videoPipeline.startEncoding(media.localStream.value, (data) => {
      transport.sendVideo(call.peerId.value!, data);
    });
  }
}
</script>

<template>
  <div class="h-screen flex">
    <div class="flex-1 bg-[var(--color-surface-alt)] relative flex flex-col">
      <div class="absolute top-0 left-0 right-0 flex justify-between px-4 py-3 z-20 bg-gradient-to-b from-black/80 to-transparent">
        <span class="text-sm font-black tracking-widest">{{ callDuration }}</span>
        <div class="flex items-center gap-2">
          <div class="w-2 h-2 bg-[var(--color-accent)]"></div>
          <span class="text-[10px] text-[var(--color-accent)] tracking-widest font-bold">P2P Direct</span>
        </div>
      </div>

      <div class="flex-1">
        <VideoGrid
          :localStream="media.localStream.value"
          :peers="call.peers.value"
          :remoteVideoFrame="remoteVideoFrame"
        />
      </div>

      <div
        class="absolute bottom-0 left-0 py-3.5 z-20 bg-gradient-to-t from-black/80 to-transparent"
        :class="chatOpen ? 'right-[260px]' : 'right-0'"
      >
        <CallControls
          :audioMuted="media.audioMuted.value"
          :videoMuted="media.videoMuted.value"
          :chatOpen="chatOpen"
          @toggleAudio="handleToggleAudio"
          @toggleVideo="handleToggleVideo"
          @toggleChat="chatOpen = !chatOpen"
          @endCall="handleEndCall"
        />
      </div>
    </div>

    <ChatSidebar
      v-if="chatOpen"
      :messages="chat.messages.value"
      :peerId="call.peerId.value || ''"
      @send="handleSendChat"
    />
  </div>
</template>
