<template>
  <UApp>
    <div class="min-h-screen flex flex-col">
      <header v-if="showNav" class="flex items-center justify-between px-4 py-3 border-b-2 border-[var(--color-border)]">
        <span class="font-black tracking-[6px] text-lg">NAFAQ</span>
        <NuxtLink to="/settings" class="text-lg text-[var(--color-muted)] hover:text-[var(--color-border)]">&#9881;</NuxtLink>
      </header>
      <main class="flex-1" :class="{ 'pb-16': showNav }">
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
  return path !== '/call' && path !== '/settings' && !path.startsWith('/dm/');
});

useHead({
  link: [
    { rel: "preconnect", href: "https://fonts.googleapis.com" },
    { rel: "preconnect", href: "https://fonts.gstatic.com", crossorigin: "" },
    { rel: "stylesheet", href: "https://fonts.googleapis.com/css2?family=JetBrains+Mono:wght@400;500;700;900&display=swap" },
  ],
});
</script>
