<template>
  <div class="fixed bottom-0 left-0 right-0 border-t-2 border-[var(--color-border)] bg-black z-50 safe-area-inset">
    <div class="grid grid-cols-4 text-center">
      <NuxtLink to="/" class="py-2.5 border-r border-[var(--color-border-muted)]" :class="route.path === '/' ? 'bg-[#111]' : ''">
        <div class="text-sm mb-0.5">&#9742;</div>
        <div class="text-[10px] tracking-[0.15em] font-bold" :class="route.path === '/' ? 'text-[var(--color-accent)]' : 'text-[var(--color-muted)]'">CALLS</div>
      </NuxtLink>
      <NuxtLink to="/contacts" class="py-2.5 border-r border-[var(--color-border-muted)]" :class="route.path === '/contacts' ? 'bg-[#111]' : ''">
        <div class="text-sm mb-0.5">&#9733;</div>
        <div class="text-[10px] tracking-[0.15em] font-bold" :class="route.path === '/contacts' ? 'text-[var(--color-accent)]' : 'text-[var(--color-muted)]'">CONTACTS</div>
      </NuxtLink>
      <NuxtLink to="/messages" class="py-2.5 border-r border-[var(--color-border-muted)] relative" :class="route.path.startsWith('/messages') || route.path.startsWith('/dm') ? 'bg-[#111]' : ''">
        <div class="text-sm mb-0.5 relative inline-block">
          &#9993;
          <span v-if="unread > 0" class="absolute -top-1 -right-3 w-4 h-4 bg-[var(--color-accent)] rounded-full text-[9px] leading-4 text-center font-bold">{{ unread > 9 ? '9+' : unread }}</span>
        </div>
        <div class="text-[10px] tracking-[0.15em] font-bold" :class="route.path.startsWith('/messages') || route.path.startsWith('/dm') ? 'text-[var(--color-accent)]' : 'text-[var(--color-muted)]'">MESSAGES</div>
      </NuxtLink>
      <NuxtLink to="/settings" class="py-2.5" :class="route.path === '/settings' ? 'bg-[#111]' : ''">
        <div class="text-sm mb-0.5">&#9881;</div>
        <div class="text-[10px] tracking-[0.15em] font-bold" :class="route.path === '/settings' ? 'text-[var(--color-accent)]' : 'text-[var(--color-muted)]'">SETTINGS</div>
      </NuxtLink>
    </div>
  </div>
</template>

<script setup lang="ts">
const route = useRoute();
const { totalUnread } = useDM();
const unread = computed(() => totalUnread());
</script>
