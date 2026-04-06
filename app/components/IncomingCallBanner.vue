<script setup lang="ts">
const { caller } = defineProps<{ caller: { peerId: string; ticket: string } }>();
const emit = defineEmits<{ accept: []; decline: [] }>();

const { playIncomingRing } = useNotificationSounds();

let stopRing: (() => void) | null = null;

onMounted(() => {
  stopRing = playIncomingRing();
});

onUnmounted(() => {
  stopRing?.();
});
</script>

<template>
  <Transition name="slide-down">
    <div
      class="fixed top-0 left-0 right-0 z-50 border-b-2 border-[var(--color-border)] bg-black"
      style="padding-top: calc(env(safe-area-inset-top, 0px) + 0.5rem);"
    >
      <div class="px-4 pb-3 flex items-center gap-3">
        <div class="flex-1 min-w-0">
          <p class="text-[10px] font-bold tracking-[3px] text-[var(--color-muted)] uppercase">Incoming Call</p>
          <p class="text-sm font-bold font-mono text-[var(--color-border)] truncate mt-0.5">
            {{ caller.peerId.slice(0, 16) }}...
          </p>
        </div>

        <button
          class="shrink-0 border-2 border-[var(--color-danger)] px-4 py-2 text-[10px] font-bold tracking-widest text-[var(--color-danger)] hover:bg-[var(--color-danger)] hover:text-black transition-colors"
          @click="emit('decline')"
        >
          DECLINE
        </button>

        <button
          class="shrink-0 border-2 border-[var(--color-accent)] bg-[var(--color-accent)] px-4 py-2 text-[10px] font-bold tracking-widest text-black hover:bg-transparent hover:text-[var(--color-accent)] transition-colors"
          @click="emit('accept')"
        >
          ACCEPT
        </button>
      </div>
    </div>
  </Transition>
</template>

<style scoped>
.slide-down-enter-active,
.slide-down-leave-active {
  transition: transform 0.3s ease, opacity 0.3s ease;
}
.slide-down-enter-from,
.slide-down-leave-to {
  transform: translateY(-100%);
  opacity: 0;
}
</style>
