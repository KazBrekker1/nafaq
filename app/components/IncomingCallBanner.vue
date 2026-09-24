<script setup lang="ts">
// Slide in/out is driven by the <Transition> around this component in app.vue.
defineProps<{ callerName: string }>();
const emit = defineEmits<{ accept: []; decline: [] }>();

const { playIncomingRing } = useNotificationSounds();

let stopRing: (() => void) | null = null;

onMounted(() => {
  stopRing = playIncomingRing();
});

onUnmounted(() => {
  stopRing?.();
});

// Stop ringing on the tap itself rather than after the leave transition.
function respond(action: "accept" | "decline") {
  stopRing?.();
  stopRing = null;
  if (action === "accept") emit("accept");
  else emit("decline");
}
</script>

<template>
  <div
    class="fixed top-0 left-0 right-0 z-50 border-b-2 border-(--ui-border-accented) bg-default shadow-(--ui-shadow-hard-lg)"
    style="padding-top: calc(env(safe-area-inset-top, 0px) + 0.5rem);"
  >
    <div class="px-5 pb-4 flex items-center gap-4">
      <div class="flex-1 min-w-0">
        <p class="label mb-1">Incoming Call</p>
        <p class="text-sm font-bold text-highlighted truncate mt-0.5">
          {{ callerName }}
        </p>
      </div>

      <UButton
        label="DECLINE"
        color="error"
        variant="solid"
        size="lg"
        class="shrink-0 min-h-[44px]"
        @click="respond('decline')"
      />

      <UButton
        label="ACCEPT"
        color="success"
        variant="solid"
        size="lg"
        class="shrink-0 min-h-[44px]"
        @click="respond('accept')"
      />
    </div>
  </div>
</template>

