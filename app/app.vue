<template>
  <UApp>
    <div class="h-dvh flex flex-col">
      <header v-if="showNav" class="shrink-0 flex items-center px-4 py-3 border-b-2 border-[var(--color-border)]">
        <span class="font-black tracking-[6px] text-lg">NAFAQ</span>
      </header>
      <main class="flex-1 min-h-0 overflow-y-auto" :class="{ 'pb-16': showNav }">
        <NuxtPage />
      </main>
      <TabBar v-if="showNav" />
    </div>
  </UApp>
</template>

<script setup lang="ts">
const route = useRoute();
const showNav = computed(() => {
  const path = route.path;
  return path !== '/call' && !path.startsWith('/dm/');
});

useHead({
  link: [
    { rel: "preconnect", href: "https://fonts.googleapis.com" },
    { rel: "preconnect", href: "https://fonts.gstatic.com", crossorigin: "" },
    { rel: "stylesheet", href: "https://fonts.googleapis.com/css2?family=JetBrains+Mono:wght@400;500;700;900&display=swap" },
  ],
});
</script>
