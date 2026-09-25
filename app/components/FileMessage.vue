<script setup lang="ts">
import { formatSize } from "~/utils/format";

const { name, progress, localPath, failed } = defineProps<{
  name: string;
  size: number;
  progress: number;
  localPath: string | null;
  from: "self" | "peer";
  failed?: boolean;
  failReason?: string;
}>();

const isComplete = computed(() => progress >= 1);
const progressPct = computed(() => Math.round(progress * 100));

const toast = useToast();

// Android's opener can't hand a private path to another app, so there's
// nothing useful to offer there.
const canOpen = !/android/i.test(navigator.userAgent);

// Files we write carry no "downloaded from the internet" mark, so opening an
// executable a peer sent would run it without any OS prompt. Show those in
// the file manager instead.
const EXECUTABLE_EXTENSIONS = new Set([
  "exe", "msi", "msix", "appx", "bat", "cmd", "com", "scr", "pif", "cpl", "msc",
  "lnk", "url", "reg", "ps1", "psm1", "vbs", "vbe", "js", "jse", "wsf", "wsh",
  "hta", "jar", "app", "command", "tool", "pkg", "dmg", "sh", "bash", "zsh",
  "run", "bin", "appimage", "deb", "rpm", "desktop",
]);
const isExecutable = computed(() => {
  const ext = name.split(".").pop()?.toLowerCase() ?? "";
  return name.includes(".") && EXECUTABLE_EXTENSIONS.has(ext);
});

// Received files land in ~/Downloads (see dm_files.rs); the opener
// capability is scoped to that directory.
async function openFile() {
  if (!localPath) return;
  try {
    const { openPath, revealItemInDir } = await import("@tauri-apps/plugin-opener");
    if (isExecutable.value) await revealItemInDir(localPath);
    else await openPath(localPath);
  } catch (e) {
    toast.add({ title: "Could not open file", description: String(e), color: "error" });
  }
}
</script>

<template>
  <div
    class="max-w-[240px] border-2 border-primary bg-elevated"
    :class="from === 'self' ? 'ml-auto' : ''"
  >
    <!-- File info row -->
    <div class="flex items-center gap-2 px-3 py-2">
      <UIcon name="i-heroicons-document" class="shrink-0 text-base text-primary" />
      <div class="min-w-0 flex-1">
        <p class="truncate text-xs font-bold text-highlighted">{{ name }}</p>
        <p class="text-[10px] text-dimmed">{{ formatSize(size) }}</p>
      </div>
    </div>

    <!-- Failed transfer -->
    <!-- Checked before completion: a transfer can fail after the last byte
         (size mismatch, save error). -->
    <div
      v-if="failed"
      class="border-t border-error/60 px-3 py-2"
    >
      <p class="text-[10px] font-bold tracking-widest text-error">
        TRANSFER FAILED
      </p>
      <p v-if="failReason" class="mt-0.5 text-[10px] text-dimmed">{{ failReason }}</p>
    </div>

    <!-- Progress bar -->
    <div
      v-else-if="!isComplete"
      class="border-t border-primary/40 px-3 py-2"
    >
      <UProgress :model-value="progressPct" :max="100" color="primary" size="xs" />
      <p class="mt-1 text-[9px] text-dimmed">{{ progressPct }}%</p>
    </div>

    <!-- Completion: OPEN button for received files (the only paths the
         opener scope allows), otherwise status badge -->
    <div
      v-if="canOpen && isComplete && !failed && localPath && from === 'peer'"
      class="border-t border-primary/40"
    >
      <UButton
        :label="isExecutable ? 'SHOW IN FOLDER' : 'OPEN'"
        variant="ghost"
        color="primary"
        size="xs"
        block
        class="tracking-widest"
        @click="openFile"
      />
    </div>
    <div
      v-else-if="isComplete && !failed"
      class="border-t border-primary/40 px-3 py-2"
    >
      <p class="text-[10px] font-bold tracking-widest text-primary">
        {{ from === 'self' ? 'SENT' : 'RECEIVED' }}
      </p>
    </div>
  </div>
</template>
