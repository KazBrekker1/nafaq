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
  <div class="flex max-w-full flex-wrap justify-center items-center gap-3 sm:gap-4">
    <!-- Mic group: toggle + device picker -->
    <UFieldGroup>
      <UTooltip text="Toggle mic" :kbds="['M']">
        <UButton
          :icon="audioMuted ? 'i-lucide-mic-off' : 'i-heroicons-microphone'"
          :color="audioMuted ? 'error' : 'neutral'"
          :variant="audioMuted ? 'solid' : 'subtle'"
          size="xl"
          class="size-[48px]"
          :aria-label="audioMuted ? 'Unmute microphone' : 'Mute microphone'"
          @click="emit('toggleAudio')"
        />
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
    </UFieldGroup>

    <!-- Camera group: toggle + device picker -->
    <UFieldGroup>
      <UTooltip text="Toggle camera" :kbds="['V']">
        <UButton
          :icon="videoMuted ? 'i-heroicons-video-camera-slash' : 'i-heroicons-video-camera'"
          :color="videoMuted ? 'error' : 'neutral'"
          :variant="videoMuted ? 'solid' : 'subtle'"
          size="xl"
          class="size-[48px]"
          :aria-label="videoMuted ? 'Turn camera on' : 'Turn camera off'"
          @click="emit('toggleVideo')"
        />
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
    </UFieldGroup>

    <!-- Chat toggle -->
    <UTooltip text="Toggle chat" :kbds="['C']">
      <div class="relative">
        <UButton
          icon="i-heroicons-chat-bubble-left"
          :color="chatOpen ? 'primary' : 'neutral'"
          :variant="chatOpen ? 'solid' : 'subtle'"
          size="xl"
          class="size-[48px]"
          aria-label="Toggle chat"
          @click="emit('toggleChat')"
        />
        <span v-if="unreadCount > 0 && !chatOpen" class="pointer-events-none absolute -top-1 -right-1 size-3 rounded-full bg-primary" />
      </div>
    </UTooltip>

    <!-- Share connection -->
    <UTooltip text="Show QR / connection string">
      <UButton
        icon="i-heroicons-qr-code"
        color="neutral"
        variant="subtle"
        size="xl"
        class="size-[48px]"
        aria-label="Show QR / connection string"
        :disabled="!canShareConnection"
        @click="emit('showConnection')"
      />
    </UTooltip>

    <!-- End call -->
    <UTooltip text="End call">
      <UButton
        icon="i-heroicons-phone-x-mark"
        color="error"
        variant="solid"
        size="xl"
        class="size-[60px]"
        aria-label="End call"
        @click="emit('endCall')"
      />
    </UTooltip>
  </div>
</template>
