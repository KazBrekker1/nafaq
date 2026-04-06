<script setup lang="ts">
import { truncateNodeId } from "~/utils/format";

const { public: { appVersion } } = useRuntimeConfig();
const { nodeId, displayName } = useCall();
const { settings, save, togglePersistentIdentity } = useSettings();

const truncatedNodeId = computed(() => {
  if (!nodeId.value) return "\u2014";
  return truncateNodeId(nodeId.value, 8, 4);
});

const { copy, copied: nodeCopied } = useClipboard();
function copyNodeId() {
  if (nodeId.value) copy(nodeId.value);
}

const qrModalOpen = ref(false);

async function handlePersistentIdentity(e: Event) {
  const enabled = (e.target as HTMLInputElement).checked;
  await togglePersistentIdentity(enabled);
}

// ── Devices ───────────────────────────────────────────────
const media = useMedia();
const allDevices = ref<MediaDeviceInfo[]>([]);

async function loadDevices() {
  try {
    // Request a brief getUserMedia to unlock device labels (browsers hide
    // labels until permission is granted), then immediately stop the tracks.
    const stream = await navigator.mediaDevices.getUserMedia({ audio: true, video: true }).catch(() => null);
    allDevices.value = await navigator.mediaDevices.enumerateDevices();
    stream?.getTracks().forEach(t => t.stop());
  } catch {
    allDevices.value = [];
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
  <div class="min-h-full bg-[var(--color-surface)] safe-area-inset-min">
    <!-- Header -->
    <div class="border-b border-[var(--color-border-muted)] px-4 py-3 sticky top-0 bg-[var(--color-surface)] z-10">
      <h1 class="label text-[var(--color-border)]" style="letter-spacing: 4px;">SETTINGS</h1>
    </div>

    <div class="max-w-xl mx-auto">

      <!-- ── IDENTITY ── -->
      <section class="border-b-2 border-[var(--color-border)]">
        <div class="px-4 sm:px-6 py-3 border-b border-[var(--color-border-muted)]">
          <p class="label" style="letter-spacing: 4px;">IDENTITY</p>
        </div>

        <!-- Display Name -->
        <div class="px-4 sm:px-6 py-4 border-b border-[var(--color-border-muted)]">
          <p class="label mb-2">DISPLAY NAME</p>
          <NameInput v-model="displayName" />
        </div>

        <!-- Node ID -->
        <div class="px-4 sm:px-6 py-4 border-b border-[var(--color-border-muted)]">
          <p class="label mb-2">NODE ID</p>
          <div class="flex items-center gap-0">
            <div class="flex-1 border-2 border-[var(--color-border)] px-3 py-2 text-xs text-[var(--color-muted)] font-mono bg-black min-w-0 truncate">
              {{ truncatedNodeId }}
            </div>
            <button
              class="border-2 border-l-0 border-[var(--color-border)] px-3 py-2 text-xs font-bold tracking-widest hover:bg-[var(--color-border)] hover:text-black transition-colors"
              :class="nodeCopied ? 'bg-[var(--color-accent)] text-white border-[var(--color-accent)]' : ''"
              @click="copyNodeId"
            >
              {{ nodeCopied ? "COPIED" : "COPY" }}
            </button>
            <button
              class="border-2 border-l-0 border-[var(--color-border)] px-3 py-2 text-xs font-bold tracking-widest hover:bg-[var(--color-border)] hover:text-black transition-colors"
              @click="qrModalOpen = true"
            >
              QR
            </button>
          </div>
        </div>

        <!-- Persistent Identity -->
        <div class="px-4 sm:px-6 py-4">
          <div class="flex items-center justify-between gap-4">
            <div>
              <p class="label mb-1">PERSISTENT IDENTITY</p>
              <p class="text-xs text-[var(--color-muted)]">Keep your node ID across restarts</p>
            </div>
            <!-- Custom toggle -->
            <label class="relative inline-flex items-center cursor-pointer shrink-0">
              <input
                type="checkbox"
                class="sr-only peer"
                :checked="settings.persistentIdentity"
                @change="handlePersistentIdentity"
              />
              <div
                class="w-10 h-6 border-2 transition-colors bg-black relative"
                :class="settings.persistentIdentity
                  ? 'border-[var(--color-accent)] bg-[var(--color-accent)]'
                  : 'border-[var(--color-border)]'"
              >
                <div
                  class="absolute top-0.5 left-0.5 w-4 h-4 transition-transform"
                  :class="settings.persistentIdentity ? 'translate-x-4' : 'translate-x-0'"
                  :style="settings.persistentIdentity ? 'background: white' : 'background: var(--color-border)'"
                />
              </div>
            </label>
          </div>
        </div>
      </section>

      <!-- ── DEVICES ── -->
      <section class="border-b-2 border-[var(--color-border)]">
        <div class="px-4 sm:px-6 py-3 border-b border-[var(--color-border-muted)]">
          <p class="label" style="letter-spacing: 4px;">DEVICES</p>
        </div>

        <!-- Microphone -->
        <div class="px-4 sm:px-6 py-4 border-b border-[var(--color-border-muted)]">
          <p class="label mb-2">MICROPHONE</p>
          <div class="relative">
            <select
              class="w-full bg-black border-2 border-[var(--color-border)] px-3 py-2 text-sm text-[var(--color-border)] font-mono outline-none appearance-none focus:border-[var(--color-accent)] transition-colors pr-8 cursor-pointer"
              :value="selectedMic"
              @change="onMicChange"
            >
              <option value="" class="bg-black">— Default —</option>
              <option
                v-for="d in audioInputs"
                :key="d.value"
                :value="d.value"
                class="bg-black"
              >
                {{ d.label }}
              </option>
            </select>
            <UIcon name="i-heroicons-chevron-down" class="absolute right-2 top-1/2 -translate-y-1/2 text-[var(--color-muted)] pointer-events-none text-base" />
          </div>
        </div>

        <!-- Camera -->
        <div class="px-4 sm:px-6 py-4 border-b border-[var(--color-border-muted)]">
          <p class="label mb-2">CAMERA</p>
          <div class="relative">
            <select
              class="w-full bg-black border-2 border-[var(--color-border)] px-3 py-2 text-sm text-[var(--color-border)] font-mono outline-none appearance-none focus:border-[var(--color-accent)] transition-colors pr-8 cursor-pointer"
              :value="selectedCamera"
              @change="onCameraChange"
            >
              <option value="" class="bg-black">— Default —</option>
              <option
                v-for="d in videoInputs"
                :key="d.value"
                :value="d.value"
                class="bg-black"
              >
                {{ d.label }}
              </option>
            </select>
            <UIcon name="i-heroicons-chevron-down" class="absolute right-2 top-1/2 -translate-y-1/2 text-[var(--color-muted)] pointer-events-none text-base" />
          </div>
        </div>

        <!-- Speaker -->
        <div class="px-4 sm:px-6 py-4">
          <p class="label mb-2">SPEAKER</p>
          <div class="relative">
            <select
              class="w-full bg-black border-2 border-[var(--color-border)] px-3 py-2 text-sm text-[var(--color-border)] font-mono outline-none appearance-none focus:border-[var(--color-accent)] transition-colors pr-8 cursor-pointer"
              :value="selectedSpeaker"
              @change="onSpeakerChange"
            >
              <option value="" class="bg-black">— Default —</option>
              <option
                v-for="d in audioOutputs"
                :key="d.value"
                :value="d.value"
                class="bg-black"
              >
                {{ d.label }}
              </option>
            </select>
            <UIcon name="i-heroicons-chevron-down" class="absolute right-2 top-1/2 -translate-y-1/2 text-[var(--color-muted)] pointer-events-none text-base" />
          </div>
        </div>
      </section>

      <!-- ── CALL QUALITY ── -->
      <section class="border-b-2 border-[var(--color-border)]">
        <div class="px-4 sm:px-6 py-3 border-b border-[var(--color-border-muted)]">
          <p class="label" style="letter-spacing: 4px;">CALL QUALITY</p>
        </div>

        <!-- Video Quality -->
        <div class="px-4 sm:px-6 py-4 border-b border-[var(--color-border-muted)]">
          <p class="label mb-2">VIDEO QUALITY</p>
          <div class="relative">
            <select
              class="w-full bg-black border-2 border-[var(--color-border)] px-3 py-2 text-sm text-[var(--color-border)] font-mono outline-none appearance-none focus:border-[var(--color-accent)] transition-colors pr-8 cursor-pointer"
              :value="selectedQuality"
              @change="onQualityChange"
            >
              <option
                v-for="opt in qualityOptions"
                :key="opt.value"
                :value="opt.value"
                class="bg-black"
              >
                {{ opt.label }}
              </option>
            </select>
            <UIcon name="i-heroicons-chevron-down" class="absolute right-2 top-1/2 -translate-y-1/2 text-[var(--color-muted)] pointer-events-none text-base" />
          </div>
        </div>

        <!-- Data Saver -->
        <div class="px-4 sm:px-6 py-4">
          <div class="flex items-center justify-between gap-4">
            <div>
              <p class="label mb-1">DATA SAVER</p>
              <p class="text-xs text-[var(--color-muted)]">Lower resolution, less bandwidth</p>
            </div>
            <label class="relative inline-flex items-center cursor-pointer shrink-0">
              <input
                type="checkbox"
                class="sr-only peer"
                :checked="settings.dataSaver"
                @change="handleDataSaver"
              />
              <div
                class="w-10 h-6 border-2 transition-colors bg-black relative"
                :class="settings.dataSaver
                  ? 'border-[var(--color-accent)] bg-[var(--color-accent)]'
                  : 'border-[var(--color-border)]'"
              >
                <div
                  class="absolute top-0.5 left-0.5 w-4 h-4 transition-transform"
                  :class="settings.dataSaver ? 'translate-x-4' : 'translate-x-0'"
                  :style="settings.dataSaver ? 'background: white' : 'background: var(--color-border)'"
                />
              </div>
            </label>
          </div>
        </div>
      </section>

      <!-- ── ABOUT ── -->
      <section class="border-b-2 border-[var(--color-border)]">
        <div class="px-4 sm:px-6 py-3 border-b border-[var(--color-border-muted)]">
          <p class="label" style="letter-spacing: 4px;">ABOUT</p>
        </div>
        <div class="px-4 sm:px-6 py-4 space-y-3">
          <div class="flex items-center justify-between">
            <p class="label">VERSION</p>
            <p class="text-xs text-[var(--color-border)] font-mono">{{ appVersion }}</p>
          </div>
          <div class="flex items-center justify-between">
            <p class="label">TRANSPORT</p>
            <p class="text-xs text-[var(--color-border)] font-mono">Iroh 0.97</p>
          </div>
        </div>
      </section>

    </div>

    <NodeIdQrModal v-model:open="qrModalOpen" />
  </div>
</template>
