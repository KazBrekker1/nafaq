<script setup lang="ts">
import { formatSize } from "~/utils/format";

const { progress, localPath, failed } = defineProps<{
  name: string;
  size: number;
  progress: number;
  localPath: string | null;
  from: "self" | "peer";
  failed?: boolean;
}>();

const isComplete = computed(() => progress >= 1);
const progressPct = computed(() => Math.round(progress * 100));

async function openFile() {
  if (!localPath) return;
  try {
    const { invoke } = await import("@tauri-apps/api/core");
    await invoke("plugin:shell|open", { path: localPath }).catch(() => {});
  } catch {
    // Silently fail — best-effort
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
    <div
      v-if="failed && !isComplete"
      class="border-t border-error/60 px-3 py-2"
    >
      <p class="text-[10px] font-bold tracking-widest text-error">
        TRANSFER FAILED
      </p>
    </div>

    <!-- Progress bar -->
    <div
      v-else-if="!isComplete"
      class="border-t border-primary/40 px-3 py-2"
    >
      <UProgress :model-value="progressPct" :max="100" color="primary" size="xs" />
      <p class="mt-1 text-[9px] text-dimmed">{{ progressPct }}%</p>
    </div>

    <!-- Completion: OPEN button if localPath available, otherwise status badge -->
    <div
      v-if="isComplete && localPath"
      class="border-t border-primary/40"
    >
      <UButton
        label="OPEN"
        variant="ghost"
        color="primary"
        size="xs"
        block
        class="tracking-widest"
        @click="openFile"
      />
    </div>
    <div
      v-else-if="isComplete"
      class="border-t border-primary/40 px-3 py-2"
    >
      <p class="text-[10px] font-bold tracking-widest text-primary">
        {{ from === 'self' ? 'SENT' : 'RECEIVED' }}
      </p>
    </div>
  </div>
</template>
