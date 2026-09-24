<script setup lang="ts">
import { truncateNodeId } from "~/utils/format";

// Own identity: display name, short node id, copy + QR. Shared by the home,
// contacts and settings pages.
const { showName = true } = defineProps<{ showName?: boolean }>();

const { nodeId, displayName } = useCall();
const { settings } = useSettings();

const shortNodeId = computed(() => (nodeId.value ? truncateNodeId(nodeId.value) : "—"));

const { copy, copied } = useClipboard();
function copyNodeId() {
  if (nodeId.value) copy(nodeId.value);
}

const qrModalOpen = ref(false);
</script>

<template>
  <div class="flex items-center justify-between gap-3">
    <div class="min-w-0">
      <p v-if="showName" class="truncate text-sm font-bold text-highlighted">
        {{ displayName || "—" }}
      </p>
      <div class="flex items-center gap-2" :class="{ 'mt-1': showName }">
        <p class="truncate text-xs text-muted">{{ shortNodeId }}</p>
        <UBadge v-if="settings.persistentIdentity" color="primary" class="shrink-0">
          Persistent
        </UBadge>
      </div>
    </div>
    <UFieldGroup class="shrink-0">
      <UButton icon="i-heroicons-qr-code" variant="subtle" color="neutral" @click="() => { qrModalOpen = true }">
        QR
      </UButton>
      <UButton
        :icon="copied ? 'i-heroicons-check' : 'i-heroicons-clipboard-document'"
        :variant="copied ? 'solid' : 'subtle'"
        :color="copied ? 'primary' : 'neutral'"
        @click="copyNodeId"
      >
        {{ copied ? "Copied" : "Copy" }}
      </UButton>
    </UFieldGroup>

    <NodeIdQrModal v-model:open="qrModalOpen" />
  </div>
</template>
