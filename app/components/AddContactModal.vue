<script setup lang="ts">
const props = defineProps<{ open: boolean }>();
const emit = defineEmits<{
  "update:open": [value: boolean];
  added: [];
}>();

const { add } = useContacts();

const nodeIdInput = ref("");
const nameInput = ref("");
const saving = ref(false);
const error = ref<string | null>(null);
const showScanner = ref(false);

function close() {
  emit("update:open", false);
}

watch(() => props.open, (open) => {
  if (open) {
    nodeIdInput.value = "";
    nameInput.value = "";
    error.value = null;
    showScanner.value = false;
  }
});

function handleScan(scanned: string) {
  nodeIdInput.value = scanned.trim();
  showScanner.value = false;
}

async function handleSave() {
  error.value = null;
  const nodeId = nodeIdInput.value.trim();
  const displayName = nameInput.value.trim();

  if (!nodeId) {
    error.value = "Node ID is required.";
    return;
  }

  saving.value = true;
  try {
    await add({
      node_id: nodeId,
      display_name: displayName || nodeId.slice(0, 12),
      added_at: Date.now(),
      last_seen: 0,
      source: "manual",
    });
    emit("added");
    close();
  } catch (e) {
    error.value = `Failed to save contact: ${e}`;
  } finally {
    saving.value = false;
  }
}
</script>

<template>
  <div
    v-if="open"
    class="fixed inset-0 z-50 flex items-center justify-center overflow-y-auto bg-black/80 p-4"
    @click.self="close"
  >
    <div class="w-full max-w-sm my-auto border-2 border-[var(--color-border)] bg-[var(--color-surface-alt)] shadow-2xl">

      <!-- Header -->
      <div class="flex items-center justify-between border-b border-[var(--color-border-muted)] px-4 py-3">
        <p class="label" style="letter-spacing: 4px;">ADD CONTACT</p>
        <button
          class="text-[var(--color-muted)] transition-colors hover:text-white"
          aria-label="Close"
          @click="close"
        >
          <UIcon name="i-heroicons-x-mark" class="text-lg" />
        </button>
      </div>

      <div class="p-4 space-y-4">

        <!-- Node ID input -->
        <div>
          <p class="label mb-2">NODE ID</p>
          <div class="flex gap-0">
            <input
              v-model="nodeIdInput"
              type="text"
              class="flex-1 bg-black border-2 border-[var(--color-border)] px-3 py-2 text-xs text-[var(--color-border)] font-mono outline-none focus:border-[var(--color-accent)] transition-colors min-w-0"
              placeholder="Paste node ID..."
              @keydown.enter="handleSave"
            />
            <button
              class="border-2 border-l-0 border-[var(--color-border)] px-3 py-2 text-[10px] font-bold tracking-widest hover:bg-[var(--color-border)] hover:text-black transition-colors shrink-0"
              title="Scan QR code"
              @click="showScanner = true"
            >
              QR
            </button>
          </div>
        </div>

        <!-- Display name input -->
        <div>
          <p class="label mb-2">DISPLAY NAME</p>
          <input
            v-model="nameInput"
            type="text"
            class="w-full bg-black border-2 border-[var(--color-border)] px-3 py-2 text-xs text-[var(--color-border)] font-mono outline-none focus:border-[var(--color-accent)] transition-colors"
            placeholder="Optional name..."
            @keydown.enter="handleSave"
          />
        </div>

        <!-- Error -->
        <div
          v-if="error"
          class="border-2 border-[var(--color-danger)] p-2 text-xs text-[var(--color-danger)]"
        >
          {{ error }}
        </div>

        <!-- Actions -->
        <div class="flex gap-0">
          <button
            class="flex-1 border-2 border-[var(--color-border)] py-2 text-xs font-bold tracking-widest hover:bg-[var(--color-border)] hover:text-black transition-colors disabled:opacity-40"
            :disabled="saving || !nodeIdInput.trim()"
            @click="handleSave"
          >
            {{ saving ? "SAVING..." : "SAVE" }}
          </button>
          <button
            class="flex-1 border-2 border-l-0 border-[var(--color-border)] py-2 text-xs font-bold tracking-widest text-[var(--color-muted)] hover:text-[var(--color-border)] transition-colors"
            @click="close"
          >
            CANCEL
          </button>
        </div>

      </div>
    </div>
  </div>

  <!-- QR Scanner (full-screen overlay, rendered outside modal) -->
  <QrScanner
    v-if="showScanner"
    @scan="handleScan"
    @close="showScanner = false"
  />
</template>
