import { ref, watch, type Ref, type WatchStopHandle } from "vue";
import { useIntervalFn } from "@vueuse/core";
import { useNodeRuntime, type PeerConnectionStatus } from "./useNodeRuntime";

const FRONTEND_PRESENCE_PROBE_CONCURRENCY = 3;

const onlineStatus = ref<Record<string, boolean>>({});
let activeNodeIds: Ref<string[]> | null = null;
let activeProbeOwner: symbol | null = null;
let stopActiveWatch: WatchStopHandle | null = null;
let stopRelayWatch: WatchStopHandle | null = null;
let probeInFlight = false;
let pendingProbeIds: string[] | null = null;
let runtime: ReturnType<typeof useNodeRuntime> | null = null;

function nodeRuntime() {
  runtime ??= useNodeRuntime();
  return runtime;
}

export function uniquePresenceIds(nodeIds: string[]): string[] {
  return [...new Set(nodeIds.filter((nodeId) => nodeId.length > 0))];
}

export function peerConnectionStatusToPresence(status: PeerConnectionStatus | undefined): boolean | null {
  switch (status) {
    case "connected":
    case "suspect":
    case "reconnecting":
      return true;
    case "disconnected":
    case "failed":
      return false;
    default:
      return null;
  }
}

function knownPresence(nodeId: string): boolean | null {
  return peerConnectionStatusToPresence(nodeRuntime().peerConnectionStatuses.value[nodeId]);
}

function activePresenceIds(): Set<string> | null {
  return activeNodeIds ? new Set(uniquePresenceIds(activeNodeIds.value)) : null;
}

function applyStatuses(statuses: Record<string, boolean>) {
  const entries = Object.entries(statuses);
  if (entries.length === 0) return;

  const activeIds = activePresenceIds();
  if (!activeIds) return;

  const next = { ...onlineStatus.value };
  let changed = false;
  for (const [nodeId, online] of entries) {
    if (!activeIds.has(nodeId)) continue;
    if (next[nodeId] !== online) {
      next[nodeId] = online;
      changed = true;
    }
  }
  if (changed) onlineStatus.value = next;
}

function pruneOnlineStatus(nodeIds: string[]) {
  const activeIds = new Set(uniquePresenceIds(nodeIds));
  const next: Record<string, boolean> = {};

  for (const nodeId of activeIds) {
    const known = knownPresence(nodeId);
    const value = known ?? onlineStatus.value[nodeId];
    if (value !== undefined) next[nodeId] = value;
  }

  const currentKeys = Object.keys(onlineStatus.value);
  const nextKeys = Object.keys(next);
  const changed =
    currentKeys.length !== nextKeys.length ||
    nextKeys.some((nodeId) => onlineStatus.value[nodeId] !== next[nodeId]);
  if (changed) onlineStatus.value = next;
}

function markIdsOffline(nodeIds: string[]) {
  const statuses: Record<string, boolean> = {};
  for (const nodeId of uniquePresenceIds(nodeIds)) {
    statuses[nodeId] = knownPresence(nodeId) ?? false;
  }
  applyStatuses(statuses);
}

async function runProbeAllByIds(rawNodeIds: string[]) {
  const nodeIds = uniquePresenceIds(rawNodeIds);
  if (nodeIds.length === 0) return;

  const runtimeState = nodeRuntime();
  await runtimeState.init();

  const knownStatuses: Record<string, boolean> = {};
  const probeIds: string[] = [];
  for (const nodeId of nodeIds) {
    const peerStatus = runtimeState.peerConnectionStatuses.value[nodeId];
    const known = peerConnectionStatusToPresence(peerStatus);
    if (known !== null) {
      knownStatuses[nodeId] = known;
    } else if (peerStatus === "connecting") {
      knownStatuses[nodeId] = onlineStatus.value[nodeId] ?? false;
    } else if (runtimeState.relayStatus.value === "online") {
      probeIds.push(nodeId);
    } else {
      knownStatuses[nodeId] = false;
    }
  }
  applyStatuses(knownStatuses);

  if (runtimeState.relayStatus.value !== "online" || probeIds.length === 0) return;

  const { invoke } = await import("@tauri-apps/api/core");
  const probedStatuses: Record<string, boolean> = {};
  for (let i = 0; i < probeIds.length; i += FRONTEND_PRESENCE_PROBE_CONCURRENCY) {
    if (runtimeState.relayStatus.value !== "online") {
      markIdsOffline(probeIds.slice(i));
      break;
    }

    const batch = probeIds.slice(i, i + FRONTEND_PRESENCE_PROBE_CONCURRENCY);
    const results = await Promise.allSettled(
      batch.map(async (nodeId) => {
        try {
          const online = await invoke<boolean>("check_presence", { nodeId });
          return { nodeId, online };
        } catch {
          return { nodeId, online: null };
        }
      })
    );

    for (const result of results) {
      if (result.status === "fulfilled" && result.value.online !== null) {
        probedStatuses[result.value.nodeId] = result.value.online;
      }
    }
  }
  applyStatuses(probedStatuses);
}

async function probeAllByIds(nodeIds: string[]) {
  const activeIds = activePresenceIds();
  if (!activeIds) return;

  const requestedIds = uniquePresenceIds(nodeIds).filter((nodeId) => activeIds.has(nodeId));
  if (requestedIds.length === 0) return;

  if (probeInFlight) {
    pendingProbeIds = uniquePresenceIds([...(pendingProbeIds ?? []), ...requestedIds]);
    return;
  }

  probeInFlight = true;
  try {
    await runProbeAllByIds(requestedIds);
  } finally {
    probeInFlight = false;
    const nextProbeIds = pendingProbeIds;
    pendingProbeIds = null;
    if (nextProbeIds && nextProbeIds.length > 0) {
      void probeAllByIds(nextProbeIds);
    }
  }
}

function refreshIntervalState() {
  if (
    activeNodeIds &&
    uniquePresenceIds(activeNodeIds.value).length > 0 &&
    nodeRuntime().relayStatus.value === "online"
  ) {
    resume();
  } else {
    pause();
  }
}

const { pause, resume } = useIntervalFn(
  () => {
    if (activeNodeIds) void probeAllByIds(activeNodeIds.value);
  },
  30_000,
  { immediate: false }
);

export function usePresence() {
  const runtimeState = nodeRuntime();
  const probeOwner = Symbol("presence-probe-owner");

  function startProbing(nodeIds: Ref<string[]>) {
    stopActiveWatch?.();
    stopRelayWatch?.();

    activeProbeOwner = probeOwner;
    activeNodeIds = nodeIds;
    pruneOnlineStatus(nodeIds.value);
    void runtimeState.init();

    stopActiveWatch = watch(
      nodeIds,
      (ids) => {
        if (activeProbeOwner !== probeOwner) return;
        pruneOnlineStatus(ids);
        refreshIntervalState();
        if (runtimeState.relayStatus.value === "online") {
          void probeAllByIds(ids);
        } else {
          markIdsOffline(ids);
        }
      },
      { immediate: true }
    );

    stopRelayWatch = watch(runtimeState.relayStatus, (status) => {
      if (activeProbeOwner !== probeOwner) return;
      refreshIntervalState();
      if (!activeNodeIds) return;
      if (status === "online") {
        void probeAllByIds(activeNodeIds.value);
      } else {
        markIdsOffline(activeNodeIds.value);
      }
    });

    refreshIntervalState();
  }

  function stopProbing() {
    if (activeProbeOwner !== probeOwner) return;

    pause();
    stopActiveWatch?.();
    stopRelayWatch?.();
    stopActiveWatch = null;
    stopRelayWatch = null;
    activeNodeIds = null;
    activeProbeOwner = null;
    pendingProbeIds = null;
  }

  function isOnline(nodeId: string): boolean {
    return knownPresence(nodeId) ?? onlineStatus.value[nodeId] ?? false;
  }

  return { onlineStatus, startProbing, stopProbing, isOnline };
}
