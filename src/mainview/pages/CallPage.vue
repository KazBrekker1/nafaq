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
let cleaned = false;

function cleanup() {
  if (cleaned) return;
  cleaned = true;
  if (durationInterval) { clearInterval(durationInterval); durationInterval = null; }
  audioPipeline.stop();
  videoPipeline.stop();
  transport.disconnect();
}

onMounted(async () => {
  if (call.state.value !== "connected") {
    router.push("/");
    return;
  }

  if (!media.localStream.value) {
    await media.startPreview();
  }

  transport.connect();

  transport.setOnAudio((_peerId, data, timestamp) => {
    audioPipeline.decodeChunk(data, timestamp);
  });

  transport.setOnVideo((_peerId, data, timestamp) => {
    videoPipeline.decodeChunk(data, timestamp);
  });

  audioPipeline.startDecoding();
  videoPipeline.startDecoding((frame: VideoFrame) => {
    if (remoteVideoFrame.value) {
      try { remoteVideoFrame.value.close(); } catch {}
    }
    remoteVideoFrame.value = frame;
  });

  if (media.localStream.value && call.peerId.value) {
    startEncoders(call.peerId.value);
  }

  const startTime = Date.now();
  durationInterval = setInterval(() => {
    const elapsed = Math.floor((Date.now() - startTime) / 1000);
    const mins = Math.floor(elapsed / 60);
    const secs = elapsed % 60;
    callDuration.value = `${mins}:${secs.toString().padStart(2, "0")}`;
  }, 1000);
});

onUnmounted(() => { cleanup(); });

function startEncoders(peerId: string) {
  if (!media.localStream.value) return;
  audioPipeline.startEncoding(media.localStream.value, (data) => {
    transport.sendAudio(peerId, data);
  });
  videoPipeline.startEncoding(media.localStream.value, (data) => {
    transport.sendVideo(peerId, data);
  });
}

function handleEndCall() {
  cleanup();
  media.stopPreview();
  chat.clearMessages();
  call.endCall();
}

function handleSendChat(text: string) {
  if (call.peerId.value) {
    chat.sendMessage(call.peerId.value, text);
  }
}

function handleToggle(kind: "audio" | "video") {
  const pipeline = kind === "audio" ? audioPipeline : videoPipeline;
  const muted = kind === "audio" ? media.audioMuted : media.videoMuted;
  kind === "audio" ? media.toggleAudio() : media.toggleVideo();

  if (muted.value) {
    pipeline.stopEncoding();
  } else if (media.localStream.value && call.peerId.value) {
    const send = kind === "audio" ? transport.sendAudio : transport.sendVideo;
    pipeline.startEncoding(media.localStream.value, (data: Uint8Array) => {
      send(call.peerId.value!, data);
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
          @toggleAudio="handleToggle('audio')"
          @toggleVideo="handleToggle('video')"
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
