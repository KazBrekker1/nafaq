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
  <UModal v-model:open="open" title="Add Contact">
    <template #body>
      <div class="space-y-4">

        <!-- Node ID input -->
        <UFormField label="Node ID" :ui="{ label: 'label mb-1' }">
          <UFieldGroup class="w-full">
            <UInput
              v-model="nodeIdInput"
              placeholder="Paste node ID..."
              class="flex-1"
              @keydown.enter="handleSave"
            />
            <UButton
              icon="i-heroicons-qr-code"
              aria-label="Scan QR code"
              @click="() => { showScanner = true }"
            />
          </UFieldGroup>
        </UFormField>

        <!-- Display name input -->
        <UFormField label="Display Name" :ui="{ label: 'label mb-1' }">
          <UInput
            v-model="nameInput"
            placeholder="Optional name..."
            class="w-full"
            @keydown.enter="handleSave"
          />
        </UFormField>

        <!-- Error -->
        <UAlert
          v-if="error"
          color="error"
          variant="subtle"
          icon="i-heroicons-exclamation-triangle"
          :description="error"
        />

      </div>
    </template>

    <template #footer>
      <UFieldGroup class="w-full">
        <UButton
          label="Save"
          class="flex-1"
          :loading="saving"
          :disabled="saving || !nodeIdInput.trim()"
          @click="handleSave"
        />
        <UButton
          label="Cancel"
          variant="outline"
          class="flex-1"
          @click="close"
        />
      </UFieldGroup>
    </template>
  </UModal>

  <!-- QR Scanner (rendered outside modal) -->
  <QrScanner
    v-model:open="showScanner"
    @scan="handleScan"
  />
</template>
