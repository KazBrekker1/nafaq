import { ref, inject, onMounted, onUnmounted } from "vue";
import { useRouter } from "vue-router";
import type { Event } from "../../shared/types";

export type CallState = "idle" | "creating" | "joining" | "waiting" | "connected";

export function useCall() {
  const nafaq = inject<any>("nafaq");
  const router = useRouter();

  const state = ref<CallState>("idle");
  const ticket = ref<string | null>(null);
  const peerId = ref<string | null>(null);
  const nodeId = ref<string | null>(null);
  const sidecarConnected = ref(false);
  const error = ref<string | null>(null);
  const peers = ref<string[]>([]);

  let unsubEvent: (() => void) | undefined;
  let unsubStatus: (() => void) | undefined;

  function handleEvent(event: Event) {
    switch (event.type) {
      case "node_info":
        nodeId.value = event.id;
        break;
      case "call_created":
        ticket.value = event.ticket;
        state.value = "waiting";
        break;
      case "peer_connected":
        if (!peers.value.includes(event.peer_id)) {
          peers.value.push(event.peer_id);
        }
        peerId.value = event.peer_id;
        state.value = "connected";
        router.push("/call");
        break;
      case "peer_disconnected": {
        const idx = peers.value.indexOf(event.peer_id);
        if (idx >= 0) peers.value.splice(idx, 1);
        if (peers.value.length === 0) {
          state.value = "idle";
          peerId.value = null;
          ticket.value = null;
          router.push("/");
        }
        break;
      }
      case "error":
        error.value = event.message;
        break;
    }
  }

  onMounted(() => {
    unsubEvent = nafaq?.onEvent(handleEvent);
    unsubStatus = nafaq?.onStatus((s: { connected: boolean }) => {
      sidecarConnected.value = s.connected;
    });
    nafaq?.getStatus().then((s: any) => {
      sidecarConnected.value = s.connected;
      nodeId.value = s.nodeId;
    });
  });

  onUnmounted(() => {
    unsubEvent?.();
    unsubStatus?.();
  });

  async function createCall() {
    error.value = null;
    state.value = "creating";
    await nafaq?.createCall();
    router.push("/lobby");
  }

  async function joinCall(t: string) {
    error.value = null;
    state.value = "joining";
    ticket.value = t;
    await nafaq?.joinCall(t);
    router.push("/lobby");
  }

  async function endCall() {
    for (const p of peers.value) {
      await nafaq?.endCall(p);
    }
    state.value = "idle";
    peerId.value = null;
    peers.value = [];
    ticket.value = null;
    router.push("/");
  }

  async function sendControl(action: any) {
    if (!peerId.value) return;
    await nafaq?.sendCommand({
      type: "send_control",
      peer_id: peerId.value,
      action,
    });
  }

  return {
    state, ticket, peerId, nodeId, peers,
    sidecarConnected, error,
    createCall, joinCall, endCall, sendControl,
  };
}
