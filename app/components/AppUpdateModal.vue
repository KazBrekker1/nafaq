<script setup lang="ts">
import { computed } from "vue";
import { useAppUpdate } from "~/composables/useAppUpdate";

const open = defineModel<boolean>("open", { required: true });

const {
  status,
  latestVersion,
  releaseNotes,
  downloadProgress,
  progressKnown,
  errorMessage,
  downloadAndInstall,
  checkForUpdate,
} = useAppUpdate();

function assertNever(value: never): never {
  throw new Error(`Unhandled update status: ${value}`);
}

const checking = computed(() => status.value === "checking");
const busy = computed(() => checking.value || status.value === "downloading" || status.value === "installing");
const installable = computed(() => status.value === "available");

const title = computed(() => {
  switch (status.value) {
    case "idle":
    case "checking":
      return "CHECKING FOR UPDATES";
    case "available":
    case "downloading":
    case "installing":
      return "UPDATE AVAILABLE";
    case "uptodate":
      return "NAFAQ IS UP TO DATE";
    case "unsupported":
      return "UPDATES UNAVAILABLE";
    case "error":
      return "UPDATE CHECK FAILED";
    default:
      return assertNever(status.value);
  }
});

const description = computed(() => {
  switch (status.value) {
    case "idle":
    case "checking":
      return "Checking GitHub releases for a signed Nafaq update.";
    case "available":
    case "downloading":
    case "installing":
      return latestVersion.value
        ? `Nafaq v${latestVersion.value} is ready to install.`
        : "A new Nafaq release is ready.";
    case "uptodate":
      return "You are already running the latest signed release.";
    case "unsupported":
      return "Automatic updates are only available in the desktop app.";
    case "error":
      return "Nafaq could not check for updates.";
    default:
      return assertNever(status.value);
  }
});

function close() {
  if (busy.value) return;
  open.value = false;
}
</script>

<template>
  <UModal v-model:open="open" :dismissible="!busy">
    <template #content>
      <div class="border-2 border-[var(--color-border)] bg-[var(--color-surface-alt)] shadow-2xl">
        <div class="flex items-start justify-between gap-4 border-b border-[var(--color-border-muted)] p-3 sm:p-4">
          <div>
            <p class="label mb-1">{{ title }}</p>
            <p class="text-xs text-[var(--color-muted)]">
              {{ description }}
            </p>
          </div>
          <button
            class="text-[var(--color-muted)] transition-colors hover:text-white disabled:opacity-40"
            aria-label="Close update modal"
            :disabled="busy"
            @click="close"
          >
            <UIcon name="i-heroicons-x-mark" class="text-lg" />
          </button>
        </div>

        <div class="space-y-3 p-3 sm:p-4">
          <div
            v-if="releaseNotes"
            class="max-h-40 overflow-y-auto border-2 border-[var(--color-border-muted)] bg-black p-3 text-xs whitespace-pre-line text-[var(--color-muted)]"
          >
            {{ releaseNotes }}
          </div>

          <div v-if="checking" class="flex items-center gap-2 text-xs text-[var(--color-muted)]">
            <UIcon name="i-heroicons-arrow-path" class="animate-spin text-base" />
            <span>Checking for updates…</span>
          </div>

          <div v-else-if="status === 'downloading'" class="space-y-2">
            <div class="flex items-center justify-between text-xs text-[var(--color-muted)]">
              <span>Downloading…</span>
              <span v-if="progressKnown">{{ downloadProgress }}%</span>
            </div>
            <UProgress :model-value="progressKnown ? downloadProgress : null" :max="100" />
          </div>

          <div v-else-if="status === 'installing'" class="flex items-center gap-2 text-xs text-[var(--color-muted)]">
            <UIcon name="i-heroicons-arrow-path" class="animate-spin text-base" />
            <span>Installing — Nafaq will restart shortly.</span>
          </div>

          <div
            v-else-if="status === 'error'"
            class="border-2 border-[var(--color-danger)] p-3 text-xs text-[var(--color-danger)]"
          >
            {{ errorMessage || "Update failed. Please try again." }}
          </div>

          <div
            v-else-if="status === 'uptodate' || status === 'unsupported'"
            class="border-2 border-[var(--color-border-muted)] p-3 text-xs text-[var(--color-muted)]"
          >
            {{ description }}
          </div>

          <div class="flex gap-0">
            <UButton variant="outline" class="flex-1 rounded-none" :disabled="busy" @click="close">
              {{ busy ? "Please wait…" : "Later" }}
            </UButton>
            <UButton
              v-if="status === 'error'"
              class="flex-1 rounded-none border-l-0"
              :loading="checking"
              :disabled="checking"
              @click="checkForUpdate"
            >
              Retry
            </UButton>
            <UButton
              v-else-if="installable"
              class="flex-1 rounded-none border-l-0"
              :loading="busy"
              :disabled="busy"
              @click="downloadAndInstall"
            >
              Update & Restart
            </UButton>
          </div>
        </div>
      </div>
    </template>
  </UModal>
</template>
