<script setup lang="ts">
const { name } = defineProps<{ name: string }>();
const visible = ref(true);
let timer: ReturnType<typeof setTimeout>;

onMounted(() => {
  timer = setTimeout(() => { visible.value = false; }, 3000);
});

onUnmounted(() => clearTimeout(timer));
</script>

<template>
  <Transition name="fade">
    <div
      v-if="visible"
      class="fixed top-4 left-1/2 -translate-x-1/2 z-50 bg-black/90 border border-[var(--color-border-muted)] px-4 py-2 text-xs text-[var(--color-muted)] tracking-wider"
    >
      {{ name }} left the call
    </div>
  </Transition>
</template>

<style scoped>
.fade-enter-active, .fade-leave-active { transition: opacity 0.3s; }
.fade-enter-from, .fade-leave-to { opacity: 0; }
</style>
