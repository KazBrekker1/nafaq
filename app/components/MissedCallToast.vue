<script setup lang="ts">
const { name } = defineProps<{ name: string }>();
const visible = ref(true);
let timer: ReturnType<typeof setTimeout>;

onMounted(() => {
  timer = setTimeout(() => { visible.value = false; }, 4000);
});

onUnmounted(() => clearTimeout(timer));
</script>

<template>
  <Transition name="fade">
    <div
      v-if="visible"
      class="fixed top-4 left-1/2 -translate-x-1/2 z-50 border-2 border-(--ui-border-accented) bg-default shadow-(--ui-shadow-hard) px-5 py-3 text-xs text-muted tracking-wider"
      style="margin-top: env(safe-area-inset-top, 0px);"
    >
      Missed call from {{ name }}
    </div>
  </Transition>
</template>

<style scoped>
.fade-enter-active, .fade-leave-active { transition: opacity 0.3s; }
.fade-enter-from, .fade-leave-to { opacity: 0; }
</style>
