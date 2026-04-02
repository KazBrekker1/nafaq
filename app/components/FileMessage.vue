<script setup lang="ts">
import { formatSize } from "~/utils/format";

const props = defineProps<{
  name: string;
  size: number;
  progress: number;
  localPath: string | null;
  from: "self" | "peer";
}>();

async function saveFile() {
  if (!props.localPath) return;
  try {
    const { invoke } = await import("@tauri-apps/api/core");
    await invoke("reveal_file", { path: props.localPath }).catch(() => {});
  } catch {
    // Silently fail — file reveal is best-effort
  }
}

const isComplete = computed(() => props.progress >= 1);
const progressPct = computed(() => Math.round(props.progress * 100));
</script>

<template>
  <div
    class="border-2 border-[var(--color-accent)] bg-black max-w-[240px]"
    :class="from === 'self' ? 'ml-auto' : ''"
  >
    <!-- File info row -->
    <div class="px-3 py-2 flex items-center gap-2">
      <span class="text-[var(--color-accent)] text-base shrink-0 font-mono">■</span>
      <div class="flex-1 min-w-0">
        <p class="text-xs font-bold text-[var(--color-border)] font-mono truncate">{{ name }}</p>
        <p class="text-[10px] text-[var(--color-muted)] font-mono">{{ formatSize(size) }}</p>
      </div>
    </div>

    <!-- Progress bar -->
    <div
      v-if="!isComplete"
      class="border-t border-[var(--color-accent)]/40 px-3 py-2"
    >
      <div class="h-1 bg-[var(--color-surface-alt)] w-full">
        <div
          class="h-1 bg-[var(--color-accent)] transition-all duration-300"
          :style="{ width: progressPct + '%' }"
        />
      </div>
      <p class="text-[9px] text-[var(--color-muted)] font-mono mt-1">{{ progressPct }}%</p>
    </div>

    <!-- Save button when complete -->
    <div
      v-if="isComplete && localPath"
      class="border-t border-[var(--color-accent)]/40"
    >
      <button
        class="w-full px-3 py-1.5 text-[10px] font-bold tracking-widest text-[var(--color-accent)] hover:bg-[var(--color-accent)] hover:text-black transition-colors"
        @click="saveFile"
      >
        SAVE
      </button>
    </div>
  </div>
</template>
