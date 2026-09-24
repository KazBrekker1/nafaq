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
      class="fixed top-0 left-0 right-0 z-50 border-b-2 border-(--ui-border-accented) bg-default shadow-(--ui-shadow-hard-lg)"
      style="padding-top: calc(env(safe-area-inset-top, 0px) + 0.5rem);"
    >
      <div class="px-5 pb-4 flex items-center gap-4">
        <div class="flex-1 min-w-0">
          <p class="label mb-1">Incoming Call</p>
          <p class="text-sm font-bold text-highlighted truncate mt-0.5">
            {{ caller.peerId.slice(0, 16) }}...
          </p>
        </div>

        <UButton
          label="DECLINE"
          color="error"
          variant="solid"
          size="lg"
          class="shrink-0 min-h-[44px]"
          @click="emit('decline')"
        />

        <UButton
          label="ACCEPT"
          color="success"
          variant="solid"
          size="lg"
          class="shrink-0 min-h-[44px]"
          @click="emit('accept')"
        />
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
