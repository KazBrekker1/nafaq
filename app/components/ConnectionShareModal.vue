<script setup lang="ts">
import QRCode from "qrcode";

const open = defineModel<boolean>('open', { required: true });

const { ticket, title = "SHARE CONNECTION", description = "Scan the QR or copy the connection string." } = defineProps<{
  ticket: string | null;
  title?: string;
  description?: string;
}>();

const copied = ref(false);
const qrDataUrl = ref<string | null>(null);

watch(
  () => [open.value, ticket] as const,
  async ([isOpen, t]) => {
    if (!isOpen || !t) {
      qrDataUrl.value = null;
      return;
    }

    try {
      qrDataUrl.value = await QRCode.toDataURL(t, {
        width: 320,
        margin: 1,
        color: { dark: "#000", light: "#fff" },
      });
    } catch {
      qrDataUrl.value = null;
    }
  },
  { immediate: true },
);

async function copyTicket() {
  if (!ticket) return;
  await navigator.clipboard.writeText(ticket);
  copied.value = true;
  setTimeout(() => {
    copied.value = false;
  }, 2000);
}
</script>

<template>
  <UModal v-model:open="open">
    <template #content>
      <div class="border-2 border-[var(--color-border)] bg-[var(--color-surface-alt)] shadow-2xl">
        <div class="flex items-start justify-between gap-4 border-b border-[var(--color-border-muted)] p-3 sm:p-4">
          <div>
            <p class="label mb-1">{{ title }}</p>
            <p class="text-xs text-[var(--color-muted)]">{{ description }}</p>
          </div>
          <button
            class="text-[var(--color-muted)] transition-colors hover:text-white"
            aria-label="Close share modal"
            @click="open = false"
          >
            <UIcon name="i-heroicons-x-mark" class="text-lg" />
          </button>
        </div>

        <div class="space-y-3 p-3 sm:p-4">
          <div class="flex justify-center bg-white p-2">
            <img
              v-if="qrDataUrl"
              :src="qrDataUrl"
              alt="Connection QR code"
              class="h-[200px] w-[200px] sm:h-[240px] sm:w-[240px]"
            />
            <div
              v-else
              class="flex h-[200px] w-[200px] items-center justify-center text-center text-xs text-black sm:h-[240px] sm:w-[240px]"
            >
              QR unavailable
            </div>
          </div>

          <div>
            <p class="label mb-1">CONNECTION STRING</p>
            <div class="border-2 border-[var(--color-accent)] bg-black p-2 text-[10px] break-all text-[var(--color-border)] max-h-16 overflow-y-auto">
              {{ ticket || "Waiting for connection string..." }}
            </div>
          </div>

          <div class="flex gap-0">
            <UButton class="flex-1 rounded-none" :disabled="!ticket" @click="copyTicket">
              {{ copied ? "Copied!" : "Copy" }}
            </UButton>
            <UButton variant="outline" class="flex-1 rounded-none border-l-0" @click="open = false">
              Close
            </UButton>
          </div>
        </div>
      </div>
    </template>
  </UModal>
</template>
