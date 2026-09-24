<script setup lang="ts">
import type { AppSettings } from "~/composables/useSettings";
import type { SettingSelectOption } from "~/components/SettingSelect.vue";
import { relayStatusColor } from "~/utils/format";

const { public: { appVersion } } = useRuntimeConfig();
const { displayName, relayStatus, nodeError, shareTicket } = useCall();
const { settings, save: saveSettings } = useSettings();
const toast = useToast();

async function save(patch: Partial<AppSettings>): Promise<boolean> {
  const ok = await saveSettings(patch);
  if (!ok) toast.add({ title: "Could not save setting", color: "error" });
  return ok;
}

// ── Devices ───────────────────────────────────────────────
const media = useMedia();
const allDevices = ref<MediaDeviceInfo[]>([]);

async function enumerate(): Promise<MediaDeviceInfo[]> {
  try {
    return await navigator.mediaDevices.enumerateDevices();
  } catch {
    return [];
  }
}

async function loadDevices() {
  if (!navigator.mediaDevices?.enumerateDevices) {
    allDevices.value = [];
    return;
  }
  let devices = await enumerate();
  // Browsers hide labels until media permission is granted. Only then ask —
  // for just the device kinds that exist — and release the tracks straight
  // away so the camera LED doesn't stay on.
  if (devices.length > 0 && devices.every(d => !d.label)) {
    const stream = await navigator.mediaDevices.getUserMedia({
      audio: devices.some(d => d.kind === "audioinput"),
      video: devices.some(d => d.kind === "videoinput"),
    }).catch(() => null);
    if (stream) {
      stream.getTracks().forEach(t => t.stop());
      devices = await enumerate();
    }
  }
  allDevices.value = devices;
}

function deviceOptions(kind: MediaDeviceKind, fallback: string): SettingSelectOption[] {
  return allDevices.value.filter(d => d.kind === kind).map(d => ({
    value: d.deviceId,
    label: d.label || `${fallback} ${d.deviceId.slice(0, 8)}`,
  }));
}

type SelectKey = "preferredMic" | "preferredCamera" | "preferredSpeaker" | "videoQuality";

interface SelectField {
  key: SelectKey;
  label: string;
  options: SettingSelectOption[];
  placeholder?: string;
  // Applies the choice to live media; "" means the system default.
  apply?: (value: string) => void;
}

const deviceFields = computed<SelectField[]>(() => [
  { key: "preferredMic", label: "MICROPHONE", options: deviceOptions("audioinput", "Microphone"), placeholder: "— Default —", apply: v => media.switchMic(v) },
  { key: "preferredCamera", label: "CAMERA", options: deviceOptions("videoinput", "Camera"), placeholder: "— Default —", apply: v => media.switchCamera(v) },
  { key: "preferredSpeaker", label: "SPEAKER", options: deviceOptions("audiooutput", "Speaker"), placeholder: "— Default —" },
]);

// ── Call Quality ─────────────────────────────────────────
const qualityField: SelectField = {
  key: "videoQuality",
  label: "VIDEO QUALITY",
  options: [
    { value: "auto", label: "AUTO" },
    { value: "low", label: "LOW" },
    { value: "medium", label: "MEDIUM" },
    { value: "high", label: "HIGH" },
  ],
};

async function onSelect(field: SelectField, value: string) {
  if (await save({ [field.key]: value || null } as Partial<AppSettings>)) field.apply?.(value);
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
          <IdentityCard :show-name="false" />
        </div>

        <!-- Relay Runtime -->
        <div class="px-6 sm:px-8 py-5 border-b border-muted">
          <div class="flex items-center justify-between gap-5 mb-2">
            <div>
              <p class="label mb-1">RELAY STATUS</p>
              <p class="text-xs text-muted">Tickets are available when the project relay is online.</p>
            </div>
            <UBadge :color="relayStatusColor(relayStatus)" class="shrink-0">
              {{ relayStatus.toUpperCase() }}
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

        <SettingSelect
          v-for="(field, idx) in deviceFields"
          :key="field.key"
          class="px-6 sm:px-8 py-5"
          :class="{ 'border-b border-muted': idx < deviceFields.length - 1 }"
          :label="field.label"
          :options="field.options"
          :placeholder="field.placeholder"
          :model-value="settings[field.key] ?? ''"
          @update:model-value="onSelect(field, $event)"
        />
      </section>

      <!-- ── CALL QUALITY ── -->
      <section class="border-b-2 border-(--ui-border-accented)">
        <div class="px-6 sm:px-8 py-4 border-b border-muted">
          <p class="label">CALL QUALITY</p>
        </div>

        <!-- Video Quality -->
        <SettingSelect
          class="px-6 sm:px-8 py-5 border-b border-muted"
          :label="qualityField.label"
          :options="qualityField.options"
          :model-value="settings.videoQuality"
          @update:model-value="onSelect(qualityField, $event)"
        />

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
              @update:model-value="(val) => save({ dataSaver: val })"
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

  </div>
</template>
