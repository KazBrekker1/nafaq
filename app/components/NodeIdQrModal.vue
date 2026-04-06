<script setup lang="ts">
import { useQRCode } from "@vueuse/integrations/useQRCode";

const open = defineModel<boolean>("open", { required: true });
const { nodeId } = useCall();

const qrDataUrl = useQRCode(
  computed(() => nodeId.value || ""),
  { width: 256, margin: 1, color: { dark: "#000", light: "#fff" } }
);
</script>

<template>
  <UModal v-model:open="open">
    <template #content>
      <div class="border-2 border-[var(--color-border)] bg-[var(--color-surface-alt)]">
        <div class="flex items-center justify-between border-b border-[var(--color-border-muted)] px-4 py-3">
          <p class="label" style="letter-spacing: 4px;">NODE ID</p>
          <button
            class="text-[var(--color-muted)] hover:text-[var(--color-border)] transition-colors"
            aria-label="Close QR modal"
            @click="open = false"
          >
            <UIcon name="i-heroicons-x-mark" class="text-lg" />
          </button>
        </div>
        <div class="p-4 space-y-3">
          <div class="flex justify-center bg-white p-3">
            <img
              v-if="qrDataUrl && nodeId"
              :src="qrDataUrl"
              alt="Node ID QR code"
              class="w-48 h-48"
            />
            <div
              v-else
              class="w-48 h-48 flex items-center justify-center text-xs text-black text-center"
            >
              {{ nodeId ? "Generating..." : "No node ID" }}
            </div>
          </div>
          <p class="text-[10px] text-[var(--color-muted)] break-all text-center font-mono">{{ nodeId || "\u2014" }}</p>
          <UButton variant="outline" class="w-full rounded-none" @click="open = false">
            CLOSE
          </UButton>
        </div>
      </div>
    </template>
  </UModal>
</template>
