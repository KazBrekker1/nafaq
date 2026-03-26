<script setup lang="ts">
import QRCode from "qrcode";

const props = withDefaults(defineProps<{
  open: boolean;
  ticket: string | null;
  title?: string;
  description?: string;
}>(), {
  title: "SHARE CONNECTION",
  description: "Scan the QR or copy the connection string.",
});

const emit = defineEmits<{
  close: [];
}>();

const copied = ref(false);
const qrDataUrl = ref<string | null>(null);

watch(
  () => [props.open, props.ticket] as const,
  async ([open, ticket]) => {
    if (!open || !ticket) {
      qrDataUrl.value = null;
      return;
    }

    try {
      qrDataUrl.value = await QRCode.toDataURL(ticket, {
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
  if (!props.ticket) return;
  await navigator.clipboard.writeText(props.ticket);
  copied.value = true;
  setTimeout(() => {
    copied.value = false;
  }, 2000);
}
</script>

<template>
  <div
    v-if="open"
    class="fixed inset-0 z-50 flex items-center justify-center bg-black/80 p-4"
    @click.self="emit('close')"
  >
    <div class="w-full max-w-lg border-2 border-[var(--color-border)] bg-[var(--color-surface-alt)] shadow-2xl">
      <div class="flex items-start justify-between gap-4 border-b border-[var(--color-border-muted)] p-4 sm:p-5">
        <div>
          <p class="label mb-2">{{ title }}</p>
          <p class="text-xs text-[var(--color-muted)]">{{ description }}</p>
        </div>
        <button
          class="text-[var(--color-muted)] transition-colors hover:text-white"
          aria-label="Close share modal"
          @click="emit('close')"
        >
          <UIcon name="i-heroicons-x-mark" class="text-lg" />
        </button>
      </div>

      <div class="space-y-4 p-4 sm:p-5">
        <div class="flex justify-center bg-white p-3 sm:p-4">
          <img
            v-if="qrDataUrl"
            :src="qrDataUrl"
            alt="Connection QR code"
            class="h-[260px] w-[260px] sm:h-[320px] sm:w-[320px]"
          />
          <div
            v-else
            class="flex h-[260px] w-[260px] items-center justify-center text-center text-xs text-black sm:h-[320px] sm:w-[320px]"
          >
            QR unavailable
          </div>
        </div>

        <div>
          <p class="label mb-2">CONNECTION STRING</p>
          <div class="border-2 border-[var(--color-accent)] bg-black p-3 text-xs break-all text-[var(--color-border)]">
            {{ ticket || "Waiting for connection string..." }}
          </div>
        </div>

        <div class="flex gap-0">
          <UButton class="flex-1 rounded-none" :disabled="!ticket" @click="copyTicket">
            {{ copied ? "Copied!" : "Copy Connection String" }}
          </UButton>
          <UButton variant="outline" class="flex-1 rounded-none border-l-0" @click="emit('close')">
            Close
          </UButton>
        </div>
      </div>
    </div>
  </div>
</template>
