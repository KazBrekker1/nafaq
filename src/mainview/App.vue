<script setup lang="ts">
import { ref, onMounted, onUnmounted, inject } from "vue";

const nafaq = inject<any>("nafaq");
const connected = ref(false);
const nodeId = ref<string | null>(null);

let unsubEvent: (() => void) | undefined;
let unsubStatus: (() => void) | undefined;

onMounted(() => {
  unsubStatus = nafaq?.onStatus((status: { connected: boolean }) => {
    connected.value = status.connected;
  });

  unsubEvent = nafaq?.onEvent((event: any) => {
    if (event.type === "node_info") {
      nodeId.value = event.id;
    }
  });

  nafaq?.getStatus().then((status: any) => {
    connected.value = status.connected;
    nodeId.value = status.nodeId;
  });
});

onUnmounted(() => {
  unsubEvent?.();
  unsubStatus?.();
});
</script>

<template>
  <div style="font-family: 'JetBrains Mono', monospace; background: #000; color: #e2e8f0; min-height: 100vh; display: flex; align-items: center; justify-content: center;">
    <div style="text-align: center;">
      <h1 style="font-size: 48px; font-weight: 900; letter-spacing: 8px;">NAFAQ</h1>
      <p style="color: #666; font-size: 11px; letter-spacing: 4px; text-transform: uppercase;">P2P Encrypted Calls</p>

      <div style="margin-top: 2rem; border: 2px solid #333; padding: 1rem;">
        <p style="font-size: 10px; text-transform: uppercase; letter-spacing: 3px; color: #666; margin-bottom: 0.5rem;">SIDECAR STATUS</p>
        <div style="display: flex; align-items: center; justify-content: center; gap: 8px;">
          <div :style="{ width: '8px', height: '8px', background: connected ? '#8B5CF6' : '#ff0000' }"></div>
          <span style="font-size: 12px;">{{ connected ? 'Connected' : 'Disconnected' }}</span>
        </div>
        <p v-if="nodeId" style="color: #555; font-size: 11px; margin-top: 0.5rem; word-break: break-all;">
          Node: {{ nodeId.slice(0, 16) }}...
        </p>
      </div>
    </div>
  </div>
</template>
