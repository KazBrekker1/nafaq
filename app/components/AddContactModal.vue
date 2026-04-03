<script setup lang="ts">
const open = defineModel<boolean>('open', { required: true });
const emit = defineEmits<{ added: [] }>();

const { add } = useContacts();

const nodeIdInput = ref("");
const nameInput = ref("");
const saving = ref(false);
const error = ref<string | null>(null);
const showScanner = ref(false);

function close() {
  open.value = false;
}

watch(() => open.value, (isOpen) => {
  if (isOpen) {
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
  <UModal v-model:open="open">
    <template #content>
      <div class="w-full max-w-sm border-2 border-[var(--color-border)] bg-[var(--color-surface-alt)] shadow-2xl">

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
              <UInput
                v-model="nodeIdInput"
                placeholder="Paste node ID..."
                class="flex-1 rounded-none font-mono text-xs"
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
            <UInput
              v-model="nameInput"
              placeholder="Optional name..."
              class="w-full rounded-none font-mono text-xs"
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
            <UButton
              class="flex-1 rounded-none"
              :disabled="saving || !nodeIdInput.trim()"
              @click="handleSave"
            >
              {{ saving ? "SAVING..." : "SAVE" }}
            </UButton>
            <UButton
              variant="outline"
              class="flex-1 rounded-none border-l-0"
              @click="close"
            >
              CANCEL
            </UButton>
          </div>

        </div>
      </div>
    </template>
  </UModal>

  <!-- QR Scanner (rendered outside modal) -->
  <QrScanner
    v-model:open="showScanner"
    @scan="handleScan"
  />
</template>
