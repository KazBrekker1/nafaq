<script setup lang="ts">
import { useQRCode } from "@vueuse/integrations/useQRCode";

const open = defineModel<boolean>("open", { required: true });
const { nodeId } = useCall();

const qrDataUrl = useQRCode(
  computed(() => nodeId.value || ""),
  { width: 256, margin: 1, color: { dark: "#000", light: "#fff" } }
);

function closeModal() {
  open.value = false;
}
</script>

<template>
  <UModal v-model:open="open" title="Node ID">
    <template #body>
      <div class="space-y-3">
        <div class="flex justify-center bg-white p-3">
          <img
            v-if="qrDataUrl && nodeId"
            :src="qrDataUrl"
            alt="Node ID QR code"
            class="h-48 w-48"
          />
          <div
            v-else
            class="flex h-48 w-48 items-center justify-center text-center text-xs text-black"
          >
            {{ nodeId ? "Generating..." : "No node ID" }}
          </div>
        </div>
        <p class="break-all text-center text-xs text-muted">{{ nodeId || "—" }}</p>
      </div>
    </template>

    <template #footer>
      <UButton label="Close" variant="outline" class="w-full" @click="closeModal" />
    </template>
  </UModal>
</template>
