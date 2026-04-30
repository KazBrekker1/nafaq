export type RelayStatus = "starting" | "connecting" | "online" | "degraded" | "offline";
export type PeerConnectionStatus = "idle" | "connecting" | "connected" | "suspect" | "reconnecting" | "disconnected" | "failed";

interface NodeInfoResponse {
  id: string;
  ticket: string | null;
  relayStatus?: RelayStatus;
  relay_status?: RelayStatus;
}

interface RelayStatusChangedPayload {
  type?: string;
  status?: RelayStatus;
  relayStatus?: RelayStatus;
  relay_status?: RelayStatus;
  nodeId?: string;
  node_id?: string;
  ticketAvailable?: boolean;
  ticket_available?: boolean;
  message?: string | null;
}

interface TicketRefreshedPayload {
  type?: string;
  ticket?: string | null;
}

interface PeerConnectionStatusChangedPayload {
  type?: string;
  peerId?: string;
  peer_id?: string;
  status?: PeerConnectionStatus;
  reason?: string | null;
}

const nodeId = ref<string | null>(null);
const relayStatus = ref<RelayStatus>("starting");
const ticket = ref<string | null>(null);
const shareTicket = ticket;
const nodeError = ref<string | null>(null);
const peerConnectionStatuses = ref<Record<string, PeerConnectionStatus>>({});

let initialized = false;
let initPromise: Promise<void> | null = null;
let runtimeEventRevision = 0;

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
  const status = normalizeRelayStatus(payload.status ?? payload.relayStatus ?? payload.relay_status);
  if (status) relayStatus.value = status;

  const nextNodeId = payload.nodeId ?? payload.node_id;
  if (typeof nextNodeId === "string" && nextNodeId.length > 0) {
    nodeId.value = nextNodeId;
  }

  const ticketAvailable = payload.ticketAvailable ?? payload.ticket_available;
  if (ticketAvailable === false || (status !== null && status !== "online")) {
    ticket.value = null;
  }

  nodeError.value = payload.message ?? null;
}

function applyTicket(payload: TicketRefreshedPayload) {
  if (typeof payload.ticket === "string" && payload.ticket.length > 0) {
    ticket.value = payload.ticket;
    nodeError.value = null;
  }
}

async function init(): Promise<void> {
  if (initPromise) return initPromise;

  initPromise = (async () => {
    if (!import.meta.client || initialized) return;
    initialized = true;

    try {
      const { invoke } = await import("@tauri-apps/api/core");
      const { listen } = await import("@tauri-apps/api/event");

      await listen<RelayStatusChangedPayload>("relay-status-changed", (event) => {
        runtimeEventRevision += 1;
        applyRelayStatus(event.payload ?? {});
      });

      await listen<TicketRefreshedPayload>("ticket-refreshed", (event) => {
        runtimeEventRevision += 1;
        applyTicket(event.payload ?? {});
      });

      await listen<PeerConnectionStatusChangedPayload>("peer-connection-status-changed", (event) => {
        const payload = event.payload ?? {};
        const peerId = payload.peerId ?? payload.peer_id;
        const status = normalizePeerStatus(payload.status);
        if (peerId && status) {
          peerConnectionStatuses.value = { ...peerConnectionStatuses.value, [peerId]: status };
        }
      });

      const snapshotRevision = runtimeEventRevision;
      const info = await invoke<NodeInfoResponse>("get_node_info");

      if (typeof info.id === "string" && info.id.length > 0) {
        nodeId.value = info.id;
      }

      if (runtimeEventRevision === snapshotRevision) {
        relayStatus.value = normalizeRelayStatus(info.relayStatus ?? info.relay_status) ?? relayStatus.value;
        ticket.value = info.ticket;
        nodeError.value = null;
      }
    } catch (error) {
      nodeError.value = `Could not load node runtime: ${error}`;
      relayStatus.value = "offline";
    }
  })();

  return initPromise;
}

export function useNodeRuntime() {
  if (import.meta.client && !initialized) {
    void init();
  }

  return {
    nodeId,
    relayStatus,
    ticket,
    shareTicket,
    nodeError,
    peerConnectionStatuses,
    init,
  };
}
