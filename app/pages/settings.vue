<script setup lang="ts">
import { truncateNodeId } from "~/utils/format";

const { public: { appVersion } } = useRuntimeConfig();
const { nodeId, displayName, relayStatus, nodeError, shareTicket } = useCall();
const { settings, save } = useSettings();

const truncatedNodeId = computed(() => {
  if (!nodeId.value) return "\u2014";
  return truncateNodeId(nodeId.value, 8, 4);
});

const { copy, copied: nodeCopied } = useClipboard();
function copyNodeId() {
  if (nodeId.value) copy(nodeId.value);
}

const qrModalOpen = ref(false);

const relayStatusLabel = computed(() => relayStatus.value.toUpperCase());
const relayStatusColor = computed(() => {
  switch (relayStatus.value) {
    case "online":
      return "success";
    case "degraded":
    case "offline":
      return "error";
    default:
      return "neutral";
  }
});

// ── Devices ───────────────────────────────────────────────
const media = useMedia();
const allDevices = ref<MediaDeviceInfo[]>([]);

async function loadDevices() {
  if (!navigator.mediaDevices?.enumerateDevices) {
    allDevices.value = [];
    return;
  }
  // Request a brief getUserMedia to unlock device labels (browsers hide
  // labels until permission is granted), then always stop the tracks — even
  // if enumeration throws, or the camera LED stays on.
  const stream = await navigator.mediaDevices.getUserMedia({ audio: true, video: true }).catch(() => null);
  try {
    allDevices.value = await navigator.mediaDevices.enumerateDevices();
  } catch {
    allDevices.value = [];
  } finally {
    stream?.getTracks().forEach(t => t.stop());
  }
}

const audioInputs = computed(() =>
  allDevices.value.filter((d) => d.kind === "audioinput").map((d) => ({
    value: d.deviceId,
    label: d.label || `Microphone ${d.deviceId.slice(0, 8)}`,
  }))
);
const videoInputs = computed(() =>
  allDevices.value.filter((d) => d.kind === "videoinput").map((d) => ({
    value: d.deviceId,
    label: d.label || `Camera ${d.deviceId.slice(0, 8)}`,
  }))
);
const audioOutputs = computed(() =>
  allDevices.value.filter((d) => d.kind === "audiooutput").map((d) => ({
    value: d.deviceId,
    label: d.label || `Speaker ${d.deviceId.slice(0, 8)}`,
  }))
);

const selectedMic = ref(settings.value.preferredMic ?? "");
const selectedCamera = ref(settings.value.preferredCamera ?? "");
const selectedSpeaker = ref(settings.value.preferredSpeaker ?? "");

watch(() => settings.value.preferredMic, (v) => { if (v) selectedMic.value = v; });
watch(() => settings.value.preferredCamera, (v) => { if (v) selectedCamera.value = v; });
watch(() => settings.value.preferredSpeaker, (v) => { if (v) selectedSpeaker.value = v; });

async function onMicChange(e: Event) {
  selectedMic.value = (e.target as HTMLSelectElement).value;
  await save({ preferredMic: selectedMic.value || null });
  if (selectedMic.value) media.switchMic(selectedMic.value);
}
async function onCameraChange(e: Event) {
  selectedCamera.value = (e.target as HTMLSelectElement).value;
  await save({ preferredCamera: selectedCamera.value || null });
  if (selectedCamera.value) media.switchCamera(selectedCamera.value);
}
async function onSpeakerChange(e: Event) {
  selectedSpeaker.value = (e.target as HTMLSelectElement).value;
  await save({ preferredSpeaker: selectedSpeaker.value || null });
}

// ── Call Quality ─────────────────────────────────────────
const qualityOptions = [
  { value: "auto", label: "AUTO" },
  { value: "low", label: "LOW" },
  { value: "medium", label: "MEDIUM" },
  { value: "high", label: "HIGH" },
] as const;

const selectedQuality = ref(settings.value.videoQuality ?? "auto");
watch(() => settings.value.videoQuality, (v) => { selectedQuality.value = v; });

async function onQualityChange(e: Event) {
  const val = (e.target as HTMLSelectElement).value as "auto" | "low" | "medium" | "high";
  selectedQuality.value = val;
  await save({ videoQuality: val });
}

async function handleDataSaver(e: Event) {
  const enabled = (e.target as HTMLInputElement).checked;
  await save({ dataSaver: enabled });
}

onMounted(loadDevices);
</script>

<template>
  <div class="min-h-full bg-default safe-area-inset-min">
    <!-- Header -->
    <div class="border-b-2 border-(--ui-border-accented) px-6 py-4 sticky top-0 bg-default z-10">
      <h1 class="text-sm font-black uppercase tracking-[4px] text-highlighted">Settings</h1>
    </div>

    <div class="max-w-xl mx-auto">

      <!-- ── IDENTITY ── -->
      <section class="border-b-2 border-(--ui-border-accented)">
        <div class="px-6 sm:px-8 py-4 border-b border-muted">
          <p class="label">IDENTITY</p>
        </div>

        <!-- Display Name -->
        <div class="px-6 sm:px-8 py-5 border-b border-muted">
          <p class="label mb-2">DISPLAY NAME</p>
          <NameInput v-model="displayName" />
        </div>

        <!-- Node ID -->
        <div class="px-6 sm:px-8 py-5 border-b border-muted">
          <p class="label mb-2">NODE ID</p>
          <div class="flex items-stretch gap-2">
            <div class="flex flex-1 min-w-0 items-center truncate border-2 border-(--ui-border-accented) bg-elevated px-4 py-2 text-xs text-muted shadow-(--ui-shadow-hard-sm)">
              {{ truncatedNodeId }}
            </div>
            <UButton
              :icon="nodeCopied ? 'i-heroicons-check' : 'i-heroicons-clipboard-document'"
              :color="nodeCopied ? 'primary' : 'neutral'"
              @click="copyNodeId"
            >
              {{ nodeCopied ? "Copied" : "Copy" }}
            </UButton>
            <UButton icon="i-heroicons-qr-code" color="neutral" @click="() => { qrModalOpen = true }">
              QR
            </UButton>
          </div>
        </div>

        <!-- Relay Runtime -->
        <div class="px-6 sm:px-8 py-5 border-b border-muted">
          <div class="flex items-center justify-between gap-5 mb-2">
            <div>
              <p class="label mb-1">RELAY STATUS</p>
              <p class="text-xs text-muted">Tickets are available when the project relay is online.</p>
            </div>
            <UBadge :color="relayStatusColor" class="shrink-0">
              {{ relayStatusLabel }}
            </UBadge>
          </div>
          <p class="text-[10px] text-muted">
            {{ shareTicket ? "Share ticket ready" : "Share ticket unavailable until relay is online" }}
          </p>
          <p v-if="nodeError" class="text-[10px] text-error mt-1">
            {{ nodeError }}
          </p>
        </div>

        <!-- Persistent Identity -->
        <div class="px-6 sm:px-8 py-5">
          <div class="flex items-center justify-between gap-5">
            <div>
              <p class="label mb-1">PERSISTENT IDENTITY</p>
              <p class="text-xs text-muted">Your node ID is persistent by default across restarts. Reset will be an explicit future action.</p>
            </div>
            <UBadge color="primary" class="shrink-0">
              ENABLED
            </UBadge>
          </div>
        </div>
      </section>

      <!-- ── DEVICES ── -->
      <section class="border-b-2 border-(--ui-border-accented)">
        <div class="px-6 sm:px-8 py-4 border-b border-muted">
          <p class="label">DEVICES</p>
        </div>

        <!-- Microphone -->
        <div class="px-6 sm:px-8 py-5 border-b border-muted">
          <p class="label mb-2">MICROPHONE</p>
          <div class="relative">
            <select
              class="w-full appearance-none cursor-pointer border-2 border-(--ui-border-accented) bg-elevated px-4 py-2 pr-9 text-xs text-default outline-none transition-colors focus:border-primary shadow-(--ui-shadow-hard-sm)"
              :value="selectedMic"
              @change="onMicChange"
            >
              <option value="">— Default —</option>
              <option
                v-for="d in audioInputs"
                :key="d.value"
                :value="d.value"
              >
                {{ d.label }}
              </option>
            </select>
            <UIcon name="i-heroicons-chevron-down" class="absolute right-3 top-1/2 -translate-y-1/2 text-muted pointer-events-none text-base" />
          </div>
        </div>

        <!-- Camera -->
        <div class="px-6 sm:px-8 py-5 border-b border-muted">
          <p class="label mb-2">CAMERA</p>
          <div class="relative">
            <select
              class="w-full appearance-none cursor-pointer border-2 border-(--ui-border-accented) bg-elevated px-4 py-2 pr-9 text-xs text-default outline-none transition-colors focus:border-primary shadow-(--ui-shadow-hard-sm)"
              :value="selectedCamera"
              @change="onCameraChange"
            >
              <option value="">— Default —</option>
              <option
                v-for="d in videoInputs"
                :key="d.value"
                :value="d.value"
              >
                {{ d.label }}
              </option>
            </select>
            <UIcon name="i-heroicons-chevron-down" class="absolute right-3 top-1/2 -translate-y-1/2 text-muted pointer-events-none text-base" />
          </div>
        </div>

        <!-- Speaker -->
        <div class="px-6 sm:px-8 py-5">
          <p class="label mb-2">SPEAKER</p>
          <div class="relative">
            <select
              class="w-full appearance-none cursor-pointer border-2 border-(--ui-border-accented) bg-elevated px-4 py-2 pr-9 text-xs text-default outline-none transition-colors focus:border-primary shadow-(--ui-shadow-hard-sm)"
              :value="selectedSpeaker"
              @change="onSpeakerChange"
            >
              <option value="">— Default —</option>
              <option
                v-for="d in audioOutputs"
                :key="d.value"
                :value="d.value"
              >
                {{ d.label }}
              </option>
            </select>
            <UIcon name="i-heroicons-chevron-down" class="absolute right-3 top-1/2 -translate-y-1/2 text-muted pointer-events-none text-base" />
          </div>
        </div>
      </section>

      <!-- ── CALL QUALITY ── -->
      <section class="border-b-2 border-(--ui-border-accented)">
        <div class="px-6 sm:px-8 py-4 border-b border-muted">
          <p class="label">CALL QUALITY</p>
        </div>

        <!-- Video Quality -->
        <div class="px-6 sm:px-8 py-5 border-b border-muted">
          <p class="label mb-2">VIDEO QUALITY</p>
          <div class="relative">
            <select
              class="w-full appearance-none cursor-pointer border-2 border-(--ui-border-accented) bg-elevated px-4 py-2 pr-9 text-xs text-default outline-none transition-colors focus:border-primary shadow-(--ui-shadow-hard-sm)"
              :value="selectedQuality"
              @change="onQualityChange"
            >
              <option
                v-for="opt in qualityOptions"
                :key="opt.value"
                :value="opt.value"
              >
                {{ opt.label }}
              </option>
            </select>
            <UIcon name="i-heroicons-chevron-down" class="absolute right-3 top-1/2 -translate-y-1/2 text-muted pointer-events-none text-base" />
          </div>
        </div>

        <!-- Data Saver -->
        <div class="px-6 sm:px-8 py-5">
          <div class="flex items-center justify-between gap-5">
            <div>
              <p class="label mb-1">DATA SAVER</p>
              <p class="text-xs text-muted">Lower resolution, less bandwidth</p>
            </div>
            <USwitch
              :model-value="settings.dataSaver"
              class="shrink-0"
              @update:model-value="(val) => handleDataSaver({ target: { checked: val } } as unknown as Event)"
            />
          </div>
        </div>
      </section>

      <!-- ── ABOUT ── -->
      <section class="border-b-2 border-(--ui-border-accented)">
        <div class="px-6 sm:px-8 py-4 border-b border-muted">
          <p class="label">ABOUT</p>
        </div>
        <div class="px-6 sm:px-8 py-5 space-y-4">
          <div class="flex items-center justify-between">
            <p class="label">VERSION</p>
            <p class="text-xs text-default">{{ appVersion }}</p>
          </div>
          <div class="flex items-center justify-between">
            <p class="label">TRANSPORT</p>
            <p class="text-xs text-default">Iroh 0.97</p>
          </div>
        </div>
      </section>

    </div>

    <NodeIdQrModal v-model:open="qrModalOpen" />
  </div>
</template>
