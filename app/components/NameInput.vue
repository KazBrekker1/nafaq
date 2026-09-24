<script setup lang="ts">
const model = defineModel<string>({ required: true });

// useCall loads the pinned name at startup; this component only pins/persists.
const { namePinned } = useCall();
const pinned = computed(() => namePinned.value === true);
const loaded = computed(() => namePinned.value !== null);

async function setPinnedName(name: string | null, pin: boolean) {
  const { invoke } = await import("@tauri-apps/api/core");
  await invoke("set_pinned_name", { name, pinned: pin });
}

async function togglePin() {
  const next = !pinned.value;
  namePinned.value = next;
  try {
    await setPinnedName(next ? model.value : null, next);
  } catch {
    namePinned.value = !next;
  }
}

// Debounced persist — avoids an IPC call on every keystroke
let persistTimer: ReturnType<typeof setTimeout> | undefined;
let pendingName: string | null = null;

function flushPersist() {
  clearTimeout(persistTimer);
  if (pendingName === null) return;
  const name = pendingName;
  pendingName = null;
  setPinnedName(name, true).catch(() => {});
}

watch(() => model.value, (name) => {
  if (!pinned.value) return;
  pendingName = name;
  clearTimeout(persistTimer);
  persistTimer = setTimeout(flushPersist, 400);
});

// Flush rather than drop: leaving Settings within the debounce window must
// still persist the last edit.
onUnmounted(flushPersist);
</script>

<template>
  <div class="flex items-center gap-2">
    <UInput
      v-model="model"
      placeholder="Your name"
      class="flex-1"
      :maxlength="64"
      :ui="{ base: 'text-center' }"
    />
    <UTooltip :text="pinned ? 'Name pinned — persists across sessions' : 'Pin name to remember it'">
      <UButton
        v-if="loaded"
        :icon="pinned ? 'i-heroicons-lock-closed' : 'i-heroicons-lock-open'"
        variant="ghost"
        :color="pinned ? 'primary' : 'neutral'"
        square
        :aria-label="pinned ? 'Name pinned — persists across sessions' : 'Pin name to remember it'"
        @click="togglePin"
      />
    </UTooltip>
  </div>
</template>
