<script setup lang="ts">
const model = defineModel<string>({ required: true });

const pinned = ref(false);
const loaded = ref(false);

onMounted(async () => {
  try {
    const { invoke } = await import("@tauri-apps/api/core");
    const savedName = await invoke<string | null>("get_pinned_name");
    if (savedName) {
      model.value = savedName;
      pinned.value = true;
    }
  } catch {}
  loaded.value = true;
});

async function togglePin() {
  pinned.value = !pinned.value;
  try {
    const { invoke } = await import("@tauri-apps/api/core");
    await invoke("set_pinned_name", {
      name: pinned.value ? model.value : null,
      pinned: pinned.value,
    });
  } catch {}
}

// Debounced persist — avoids an IPC call on every keystroke
let persistTimer: ReturnType<typeof setTimeout>;
watch(() => model.value, (name) => {
  if (!pinned.value || !loaded.value) return;
  clearTimeout(persistTimer);
  persistTimer = setTimeout(async () => {
    try {
      const { invoke } = await import("@tauri-apps/api/core");
      await invoke("set_pinned_name", { name, pinned: true });
    } catch {}
  }, 400);
});
</script>

<template>
  <div class="flex items-center gap-2">
    <UInput
      v-model="model"
      placeholder="Your name"
      class="flex-1 rounded-none text-sm text-center"
    />
    <button
      v-if="loaded"
      class="w-8 h-8 flex items-center justify-center transition-colors"
      :class="pinned ? 'text-[var(--color-accent)]' : 'text-[var(--color-muted)] hover:text-[var(--color-border)]'"
      :title="pinned ? 'Name pinned — persists across sessions' : 'Pin name to remember it'"
      @click="togglePin"
    >
      <UIcon :name="pinned ? 'i-heroicons-lock-closed' : 'i-heroicons-lock-open'" class="text-sm" />
    </button>
  </div>
</template>
