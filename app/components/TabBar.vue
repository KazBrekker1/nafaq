<template>
  <nav
    class="fixed inset-x-0 bottom-0 z-50 border-t-2 border-(--ui-border-accented) bg-default"
    style="padding-bottom: env(safe-area-inset-bottom, 0px);"
  >
    <div class="grid grid-cols-4 text-center">
      <NuxtLink
        to="/"
        class="flex min-h-[48px] flex-col items-center justify-center gap-1 border-r border-muted py-2"
        :class="route.path === '/' ? 'bg-elevated' : ''"
      >
        <UIcon name="i-heroicons-home" class="text-lg" :class="route.path === '/' ? 'text-primary' : 'text-muted'" />
        <span class="text-[10px] font-bold tracking-[2px]" :class="route.path === '/' ? 'text-primary' : 'text-muted'">HOME</span>
      </NuxtLink>
      <NuxtLink
        to="/contacts"
        class="flex min-h-[48px] flex-col items-center justify-center gap-1 border-r border-muted py-2"
        :class="route.path === '/contacts' ? 'bg-elevated' : ''"
      >
        <UIcon name="i-heroicons-users" class="text-lg" :class="route.path === '/contacts' ? 'text-primary' : 'text-muted'" />
        <span class="text-[10px] font-bold tracking-[2px]" :class="route.path === '/contacts' ? 'text-primary' : 'text-muted'">CONTACTS</span>
      </NuxtLink>
      <NuxtLink
        to="/messages"
        class="relative flex min-h-[48px] flex-col items-center justify-center gap-1 border-r border-muted py-2"
        :class="route.path.startsWith('/messages') || route.path.startsWith('/dm') ? 'bg-elevated' : ''"
      >
        <span class="relative inline-block">
          <UIcon
            name="i-heroicons-chat-bubble-left-right"
            class="text-lg"
            :class="route.path.startsWith('/messages') || route.path.startsWith('/dm') ? 'text-primary' : 'text-muted'"
          />
          <span
            v-if="unread > 0"
            class="absolute -top-1.5 -right-2.5 flex h-4 w-4 items-center justify-center rounded-full bg-primary text-[9px] font-bold text-inverted"
          >{{ unread > 9 ? '9+' : unread }}</span>
        </span>
        <span
          class="text-[10px] font-bold tracking-[2px]"
          :class="route.path.startsWith('/messages') || route.path.startsWith('/dm') ? 'text-primary' : 'text-muted'"
        >MESSAGES</span>
      </NuxtLink>
      <NuxtLink
        to="/settings"
        class="flex min-h-[48px] flex-col items-center justify-center gap-1 py-2"
        :class="route.path === '/settings' ? 'bg-elevated' : ''"
      >
        <UIcon name="i-heroicons-cog-6-tooth" class="text-lg" :class="route.path === '/settings' ? 'text-primary' : 'text-muted'" />
        <span class="text-[10px] font-bold tracking-[2px]" :class="route.path === '/settings' ? 'text-primary' : 'text-muted'">SETTINGS</span>
      </NuxtLink>
    </div>
  </nav>
</template>

<script setup lang="ts">
const route = useRoute();
const { totalUnread } = useDM();
const unread = computed(() => totalUnread());
</script>
