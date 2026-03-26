<script setup lang="ts">
import type { MediaDevice } from "~/composables/useMedia";

const { unreadCount = 0 } = defineProps<{
  audioMuted: boolean;
  videoMuted: boolean;
  chatOpen: boolean;
  canShareConnection: boolean;
  microphones: MediaDevice[];
  cameras: MediaDevice[];
  selectedMic: string;
  selectedCamera: string;
  unreadCount?: number;
}>();

const emit = defineEmits<{
  toggleAudio: [];
  toggleVideo: [];
  toggleChat: [];
  showConnection: [];
  endCall: [];
  switchMic: [deviceId: string];
  switchCamera: [deviceId: string];
}>();

const micDropdownOpen = ref(false);
const camDropdownOpen = ref(false);
</script>

<template>
  <div class="flex max-w-full flex-wrap justify-center items-center gap-2 sm:gap-3">
    <!-- Media group: mic + cam with device chevrons -->
    <div class="flex border-2 border-[var(--color-border-muted)]">
      <!-- Mic toggle -->
      <UTooltip text="Toggle mic" :kbds="['M']">
        <button
          class="w-10 h-10 sm:w-[46px] sm:h-[46px] flex items-center justify-center transition-colors"
          :class="audioMuted
            ? 'bg-[var(--color-danger)] text-white'
            : 'text-[var(--color-border)] hover:bg-white/5'"
          @click="emit('toggleAudio')"
        >
          <UIcon :name="audioMuted ? 'i-lucide-mic-off' : 'i-heroicons-microphone'" class="text-base sm:text-lg" />
        </button>
      </UTooltip>

      <!-- Mic device picker (desktop only) -->
      <CallDeviceDropdown
        class="hidden sm:flex"
        :open="micDropdownOpen"
        label="MICROPHONE"
        :devices="microphones"
        :selected-id="selectedMic"
        @update:open="micDropdownOpen = $event"
        @select="emit('switchMic', $event)"
      />

      <!-- Cam toggle -->
      <UTooltip text="Toggle camera" :kbds="['V']">
        <button
          class="w-10 h-10 sm:w-[46px] sm:h-[46px] flex items-center justify-center border-l border-[var(--color-border-muted)] transition-colors"
          :class="videoMuted
            ? 'bg-[var(--color-danger)] text-white'
            : 'text-[var(--color-border)] hover:bg-white/5'"
          @click="emit('toggleVideo')"
        >
          <UIcon :name="videoMuted ? 'i-heroicons-video-camera-slash' : 'i-heroicons-video-camera'" class="text-base sm:text-lg" />
        </button>
      </UTooltip>

      <!-- Cam device picker (desktop only) -->
      <CallDeviceDropdown
        class="hidden sm:flex"
        :open="camDropdownOpen"
        label="CAMERA"
        :devices="cameras"
        :selected-id="selectedCamera"
        @update:open="camDropdownOpen = $event"
        @select="emit('switchCamera', $event)"
      />
    </div>

    <!-- Chat toggle -->
    <UTooltip text="Toggle chat" :kbds="['C']">
      <button
        class="relative w-10 h-10 sm:w-[46px] sm:h-[46px] flex items-center justify-center border-2 transition-colors"
        :class="chatOpen
          ? 'bg-[var(--color-accent)] border-[var(--color-accent)] text-white'
          : 'border-[var(--color-border-muted)] text-[var(--color-border)] hover:bg-white/5'"
        @click="emit('toggleChat')"
      >
        <UIcon name="i-heroicons-chat-bubble-left" class="text-base sm:text-lg" />
        <span v-if="unreadCount > 0 && !chatOpen" class="absolute -top-1 -right-1 w-3 h-3 rounded-full bg-[var(--color-accent)]" />
      </button>
    </UTooltip>

    <UTooltip text="Show QR / connection string">
      <button
        class="w-10 h-10 sm:w-[46px] sm:h-[46px] flex items-center justify-center border-2 transition-colors"
        :class="canShareConnection
          ? 'border-[var(--color-border-muted)] text-[var(--color-border)] hover:bg-white/5'
          : 'border-[var(--color-border-muted)] text-[var(--color-muted)] opacity-50 cursor-not-allowed'"
        :disabled="!canShareConnection"
        @click="emit('showConnection')"
      >
        <UIcon name="i-heroicons-qr-code" class="text-base sm:text-lg" />
      </button>
    </UTooltip>

    <!-- End call -->
    <UTooltip text="End call">
      <button
        class="w-10 h-10 sm:w-[46px] sm:h-[46px] flex items-center justify-center border-2 border-[var(--color-danger)] bg-[var(--color-danger)] text-white hover:brightness-110 transition-colors"
        @click="emit('endCall')"
      >
        <UIcon name="i-heroicons-phone-x-mark" class="text-base sm:text-lg" />
      </button>
    </UTooltip>
  </div>
</template>
