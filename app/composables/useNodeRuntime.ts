import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

export type RelayStatus = "starting" | "connecting" | "online" | "degraded" | "offline";
export type PeerConnectionStatus = "idle" | "connecting" | "connected" | "suspect" | "reconnecting" | "disconnected" | "failed";

// get_node_info returns a camelCase struct (commands.rs NodeInfo); the events
// below are messages.rs `Event` variants, whose fields stay snake_case.
interface NodeInfoResponse {
  id: string;
  ticket: string | null;
  relayStatus?: RelayStatus;
}

interface RelayStatusChangedPayload {
  status?: RelayStatus;
  node_id?: string;
  ticket_available?: boolean;
  message?: string | null;
}

interface TicketRefreshedPayload {
  ticket?: string | null;
}

interface PeerConnectionStatusChangedPayload {
  peer_id?: string;
  status?: PeerConnectionStatus;
}

const nodeId = ref<string | null>(null);
const relayStatus = ref<RelayStatus>("starting");
const ticket = ref<string | null>(null);
const nodeError = ref<string | null>(null);
const peerConnectionStatuses = ref<Record<string, PeerConnectionStatus>>({});

let initPromise: Promise<void> | null = null;
// Per-field event counters: the get_node_info snapshot only overwrites a
// field no event has touched since the snapshot was requested.
let ticketRev = 0;
let statusRev = 0;
let unlisteners: Array<() => void> = [];

function normalizeRelayStatus(value: unknown): RelayStatus | null {
  if (
    value === "starting" ||
    value === "connecting" ||
    value === "online" ||
    value === "degraded" ||
    value === "offline"
  ) {
    return value;
  }
  return null;
}

function normalizePeerStatus(value: unknown): PeerConnectionStatus | null {
  if (
    value === "idle" ||
    value === "connecting" ||
    value === "connected" ||
    value === "suspect" ||
    value === "reconnecting" ||
    value === "disconnected" ||
    value === "failed"
  ) {
    return value;
  }
  return null;
}

function applyRelayStatus(payload: RelayStatusChangedPayload) {
  const status = normalizeRelayStatus(payload.status);
  if (status) {
    statusRev += 1;
    relayStatus.value = status;
    // "connecting" is transient, not an error; only degraded/offline carry one.
    nodeError.value = status === "degraded" || status === "offline" ? payload.message ?? null : null;
  }

  if (typeof payload.node_id === "string" && payload.node_id.length > 0) {
    nodeId.value = payload.node_id;
  }

  if (payload.ticket_available === false || (status !== null && status !== "online")) {
    ticketRev += 1;
    ticket.value = null;
  }
}

function applyTicket(payload: TicketRefreshedPayload) {
  if (typeof payload.ticket === "string" && payload.ticket.length > 0) {
    ticketRev += 1;
    ticket.value = payload.ticket;
    nodeError.value = null;
  }
}

async function init(): Promise<void> {
  initPromise ??= (async () => {
    try {
      // All listeners are live before the snapshot is requested, so anything
      // emitted meanwhile is either in the snapshot or seen as an event.
      unlisteners.push(...await Promise.all([
        listen<RelayStatusChangedPayload>("relay-status-changed", (event) => {
          applyRelayStatus(event.payload ?? {});
        }),
        listen<TicketRefreshedPayload>("ticket-refreshed", (event) => {
          applyTicket(event.payload ?? {});
        }),
        listen<PeerConnectionStatusChangedPayload>("peer-connection-status-changed", (event) => {
          const peerId = event.payload?.peer_id;
          const status = normalizePeerStatus(event.payload?.status);
          if (peerId && status) {
            peerConnectionStatuses.value = { ...peerConnectionStatuses.value, [peerId]: status };
          }
        }),
      ]));

      const snapshotTicketRev = ticketRev;
      const snapshotStatusRev = statusRev;
      const info = await invoke<NodeInfoResponse>("get_node_info");

      if (typeof info.id === "string" && info.id.length > 0) {
        nodeId.value = info.id;
      }
      if (statusRev === snapshotStatusRev) {
        relayStatus.value = normalizeRelayStatus(info.relayStatus) ?? relayStatus.value;
        nodeError.value = null;
      }
      if (ticketRev === snapshotTicketRev) {
        ticket.value = info.ticket;
      }
    } catch (error) {
      nodeError.value = `Could not load node runtime: ${error}`;
      relayStatus.value = "offline";
      // Allow a later call to retry instead of permanently caching the failure.
      for (const unlisten of unlisteners.splice(0)) unlisten();
      initPromise = null;
    }
  })();

  return initPromise;
}

if (import.meta.hot) {
  import.meta.hot.dispose(() => {
    for (const unlisten of unlisteners.splice(0)) unlisten();
    initPromise = null;
  });
}

export function useNodeRuntime() {
  if (import.meta.client) void init();

  return {
    nodeId,
    relayStatus,
    ticket,
    // Alias kept for useCall/settings, which expose it as the share ticket.
    shareTicket: ticket,
    nodeError,
    peerConnectionStatuses,
    init,
  };
}
