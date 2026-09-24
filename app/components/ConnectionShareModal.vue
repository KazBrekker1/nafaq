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

function closeModal() {
  open.value = false;
}
</script>

<template>
  <UModal
    v-model:open="open"
    :title="title"
    :description="description"
    :ui="{ title: 'label', description: 'mt-1 text-xs text-muted' }"
  >
    <template #body>
      <div class="space-y-4">
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
          <p class="label mb-2">CONNECTION STRING</p>
          <div class="max-h-16 overflow-y-auto border-2 border-primary bg-elevated p-2 text-[10px] break-all text-highlighted">
            {{ ticket || "Waiting for connection string..." }}
          </div>
        </div>
      </div>
    </template>

    <template #footer>
      <UFieldGroup class="w-full">
        <UButton class="flex-1" block :disabled="!ticket" @click="copyTicket">
          {{ copied ? "Copied!" : "Copy" }}
        </UButton>
        <UButton class="flex-1" block variant="outline" @click="closeModal">
          Close
        </UButton>
      </UFieldGroup>
    </template>
  </UModal>
</template>
