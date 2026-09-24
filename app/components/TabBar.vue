<template>
  <nav
    class="fixed inset-x-0 bottom-0 z-50 border-t-2 border-(--ui-border-accented) bg-default"
    style="padding-bottom: env(safe-area-inset-bottom, 0px);"
  >
    <div class="grid grid-cols-4 text-center">
      <NuxtLink
        v-for="(tab, idx) in tabs"
        :key="tab.to"
        :to="tab.to"
        class="flex min-h-[48px] flex-col items-center justify-center gap-1 py-2"
        :class="[isActive(tab.to) ? 'bg-elevated' : '', { 'border-r border-muted': idx < tabs.length - 1 }]"
      >
        <span class="relative inline-block">
          <UIcon :name="tab.icon" class="text-lg" :class="isActive(tab.to) ? 'text-primary' : 'text-muted'" />
          <span
            v-if="tab.badge && unread > 0"
            class="absolute -top-1.5 -right-2.5 flex h-4 w-4 items-center justify-center rounded-full bg-primary text-[9px] font-bold text-inverted"
          >{{ unread > 9 ? '9+' : unread }}</span>
        </span>
        <span class="text-[10px] font-bold tracking-[2px]" :class="isActive(tab.to) ? 'text-primary' : 'text-muted'">{{ tab.label }}</span>
      </NuxtLink>
    </div>
  </nav>
</template>

<script setup lang="ts">
const route = useRoute();
const { totalUnread } = useDM();
const unread = computed(() => totalUnread());

// The tab bar is hidden on /dm/* and /call (see app.vue), so exact matches suffice.
const tabs = [
  { to: "/", label: "HOME", icon: "i-heroicons-home" },
  { to: "/contacts", label: "CONTACTS", icon: "i-heroicons-users" },
  { to: "/messages", label: "MESSAGES", icon: "i-heroicons-chat-bubble-left-right", badge: true },
  { to: "/settings", label: "SETTINGS", icon: "i-heroicons-cog-6-tooth" },
];

function isActive(to: string) {
  return route.path === to;
}
</script>
