<script setup lang="ts">
import QRCode from "qrcode";

const props = defineProps<{ ticket: string | null; state: string }>();
const emit = defineEmits<{ create: [] }>();

const copied = ref(false);
const qrDataUrl = ref<string | null>(null);
const showQr = ref(false);

function copyTicket() {
  if (!props.ticket) return;
  navigator.clipboard.writeText(props.ticket);
  copied.value = true;
  setTimeout(() => (copied.value = false), 2000);
}

watch(() => props.ticket, async (t) => {
  if (!t) { qrDataUrl.value = null; return; }
  try {
    qrDataUrl.value = await QRCode.toDataURL(t, { width: 180, margin: 1, color: { dark: "#000", light: "#fff" } });
  } catch {}
});
</script>

<template>
  <div>
    <p class="label mb-4">SHARE THIS TICKET</p>
    <div v-if="!ticket && state === 'idle'">
      <UButton class="w-full rounded-none font-mono" @click="emit('create')">New Call</UButton>
    </div>
    <div v-else-if="state === 'creating'" class="text-[var(--color-muted)] text-xs tracking-widest">Creating...</div>
    <div v-else-if="ticket" class="space-y-4">
      <div class="border-2 border-[var(--color-accent)] p-4 text-xs break-all text-[var(--color-border)] bg-[#111]">{{ ticket }}</div>
      <div class="flex gap-0">
        <UButton class="flex-1 rounded-none border-r-0" @click="copyTicket">{{ copied ? "Copied!" : "Copy" }}</UButton>
        <UButton variant="outline" class="flex-1 rounded-none" @click="showQr = !showQr">{{ showQr ? "Hide QR" : "Show QR" }}</UButton>
      </div>
      <div v-if="showQr && qrDataUrl" class="flex justify-center">
        <img :src="qrDataUrl" alt="QR Code" class="w-[180px] h-[180px]" />
      </div>
      <p class="text-[var(--color-muted)] text-xs tracking-widest text-center">
        Waiting for peer<span class="text-[var(--color-accent)]">_</span>
      </p>
    </div>
  </div>
</template>
