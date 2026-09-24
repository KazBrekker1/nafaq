<template>
  <UApp>
    <div class="h-dvh flex flex-col">
      <header v-if="showNav" class="shrink-0 flex items-center px-8 pb-6 border-b-2 border-(--ui-border-accented) bg-default" style="padding-top: calc(env(safe-area-inset-top, 0px) + 0.75rem);">
        <span class="font-black tracking-[6px] text-lg text-highlighted">NAFAQ</span>
      </header>
      <main class="flex-1 min-h-0 overflow-y-auto" :class="{ 'pb-20': showNav }">
        <NuxtPage />
      </main>
      <TabBar v-if="showNav" />
    </div>

    <!-- The Transition must wrap the v-if for enter/leave to run. -->
    <Transition name="slide-down">
      <IncomingCallBanner
        v-if="state === 'ringing' && incomingInvite"
        :caller-name="contactName(incomingInvite.peerId)"
        @accept="acceptInvite"
        @decline="declineInvite"
      />
    </Transition>
  </UApp>
</template>

<script setup lang="ts">
const route = useRoute();
const showNav = computed(() => {
  const path = route.path;
  return path !== '/call' && !path.startsWith('/dm/');
});

const { state, incomingInvite, missedCall, acceptInvite, declineInvite } = useCall();
const { displayName: contactName } = useContacts();
const toast = useToast();

watch(missedCall, (missed) => {
  if (missed) {
    toast.add({ title: `Missed call from ${missed.callerName}`, icon: "i-heroicons-phone-x-mark", duration: 4000 });
  }
});
</script>

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
