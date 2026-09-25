import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { computed, effectScope, ref, watch } from "vue";
import { usePresence } from "./usePresence";

export type DmMessageStatus = "sending" | "sent" | "delivered" | "failed";

export interface DmTextMessage {
  type: "text";
  content: string;
  timestamp: number;
  from: "self" | "peer";
  status: DmMessageStatus;
  clientId?: string;
}

export interface DmFileMessage {
  type: "file";
  name: string;
  size: number;
  id: string;
  progress: number; // 0-1
  localPath: string | null;
  from: "self" | "peer";
  timestamp: number;
  failed?: boolean;
  failReason?: string;
}

export type DmMessageItem = DmTextMessage | DmFileMessage;

// Deep refs: messages are mutated in place.
const conversations = ref<Record<string, DmMessageItem[]>>({});
const unreadCounts = ref<Record<string, number>>({});
const totalUnread = computed(() => Object.values(unreadCounts.value).reduce((a, b) => a + b, 0));
// Conversation the DM page is showing; its incoming messages don't count as unread.
let activeConversation: string | null = null;
// Peers the Rust backend has confirmed a live DM connection for (populated
// ONLY by the "dm-connected" event, never optimistically — otherwise the local
// view can claim "connected" when the backend actually rejected the stream and
// then refuse to re-dial).
const connectedPeers = new Set<string>();
// Peers with an in-flight connect_dm invoke, to de-dup concurrent dials
// without falsely marking them connected.
const connectingPeers = new Set<string>();
// Peers whose most recent connect_dm attempt failed — cleared on the next
// successful connect (dm-connected event). Surfaced as a minimal inline
// notice; the failed message itself still retries via retryFailedMessages.
const connectErrors = ref<Record<string, boolean>>({});

let dmListenerInitialized = false;
let dmUnlisteners: Array<() => void> = [];

function findFileMsg(peerId: string, fileId: string): DmFileMessage | undefined {
  return conversations.value[peerId]?.find(
    (m): m is DmFileMessage => m.type === "file" && m.id === fileId,
  );
}

function findTextMsg(peerId: string, clientId: string): DmTextMessage | undefined {
  return conversations.value[peerId]?.find(
    (m): m is DmTextMessage => m.type === "text" && m.clientId === clientId,
  );
}

function pushMessage(nodeId: string, msg: DmMessageItem) {
  (conversations.value[nodeId] ??= []).push(msg);
  if (activeConversation !== nodeId) {
    unreadCounts.value[nodeId] = (unreadCounts.value[nodeId] ?? 0) + 1;
  }
}

// Status only moves forward: an ack can arrive before send_dm resolves, and
// the late "sent" must not downgrade "delivered". "sending" and "failed" share
// the lowest rank so a failed message can be retried.
const STATUS_RANK: Record<DmMessageStatus, number> = { sending: 0, failed: 0, sent: 1, delivered: 2 };

function setDmTextStatus(nodeId: string, clientId: string, status: DmMessageStatus) {
  const message = findTextMsg(nodeId, clientId);
  if (message && STATUS_RANK[status] >= STATUS_RANK[message.status]) {
    message.status = status;
  }
}

// Per-peer send queue: every text delivery (first send, manual resend,
// reconnect retry) runs strictly one after another in enqueue order, so a
// retry can't race a fresh send and land messages out of order.
const sendQueues = new Map<string, Promise<unknown>>();

function enqueue<T>(nodeId: string, task: () => Promise<T>): Promise<T> {
  const run = (sendQueues.get(nodeId) ?? Promise.resolve()).then(task);
  const tail = run.catch(() => {});
  sendQueues.set(nodeId, tail);
  void tail.then(() => {
    if (sendQueues.get(nodeId) === tail) sendQueues.delete(nodeId);
  });
  return run;
}

// Messages queued or in flight, keyed by clientId, so a manual resend and the
// automatic retry can't both queue the same message.
const queuedTexts = new Set<string>();

// When a send to a peer fails (typically: offline, the dial timed out), every
// text queued behind it would otherwise sit through its own full dial.
// Messages queued before that failure fail fast instead; the dm-connected /
// presence retry sends them once the peer is reachable.
const sendFailures = new Map<string, number>();

// Deliver an existing text message (already in the conversation) and resolve its
// status to "sent" or "failed". Shared by sendText, manual resend, and the
// automatic retry on reconnect. Never throws — failure is surfaced via status.
function deliverText(nodeId: string, message: DmTextMessage): Promise<boolean> {
  const clientId = message.clientId;
  if (!clientId || queuedTexts.has(clientId)) return Promise.resolve(false);
  queuedTexts.add(clientId);
  const failuresAtQueue = sendFailures.get(nodeId) ?? 0;
  // Show a queued retry as in progress right away (not only once it runs).
  if (message.status === "failed") setDmTextStatus(nodeId, clientId, "sending");
  return enqueue(nodeId, async () => {
    try {
      const current = findTextMsg(nodeId, clientId);
      if (!current) return false;
      if (STATUS_RANK[current.status] >= STATUS_RANK.sent) return true;
      if ((sendFailures.get(nodeId) ?? 0) > failuresAtQueue) {
        setDmTextStatus(nodeId, clientId, "failed");
        return false;
      }
      current.status = "sending";
      await invoke("send_dm", {
        peerId: nodeId,
        message: { type: "text", content: current.content, timestamp: current.timestamp, id: clientId },
      });
      setDmTextStatus(nodeId, clientId, "sent");
      return true;
    } catch (error) {
      console.warn("[dm] send failed:", error);
      sendFailures.set(nodeId, (sendFailures.get(nodeId) ?? 0) + 1);
      setDmTextStatus(nodeId, clientId, "failed");
      return false;
    } finally {
      queuedTexts.delete(clientId);
    }
  });
}

function failedTexts(nodeId: string): DmTextMessage[] {
  return (conversations.value[nodeId] ?? []).filter(
    (m): m is DmTextMessage => m.type === "text" && m.from === "self" && m.status === "failed",
  );
}

// Re-send any text messages still marked "failed" for this peer, in their
// original order (the per-peer queue serializes them). A message that fails
// again is left "failed" and doesn't block the rest.
async function retryFailedMessages(nodeId: string) {
  await Promise.all(failedTexts(nodeId).map(m => deliverText(nodeId, m)));
}

async function dial(nodeId: string) {
  // Already connected, or a connect is already in flight — don't double-dial.
  if (connectedPeers.has(nodeId) || connectingPeers.has(nodeId)) return;
  connectingPeers.add(nodeId);
  try {
    await invoke("connect_dm", { nodeId });
    // Intentionally do NOT add to connectedPeers here — the "dm-connected"
    // event is the source of truth. If Rust rejected the stream, no event
    // fires and a later dial correctly re-dials.
    // Rust may already have been connected (no new dm-connected, e.g. after
    // a frontend reload): retry anything still failed now. Deduped by
    // queuedTexts if dm-connected also fires.
    void retryFailedMessages(nodeId);
  } catch (error) {
    console.warn("[dm] connect_dm failed:", error);
    connectErrors.value[nodeId] = true;
  } finally {
    connectingPeers.delete(nodeId);
  }
}

// Failed messages otherwise only retry on dm-connected, which needs someone to
// dial. When presence sees a peer with undelivered messages come online, dial
// it (debounced per peer, since presence can flap) so the retry happens.
const PRESENCE_RETRY_DELAY_MS = 1500;
const presenceRetryTimers = new Map<string, ReturnType<typeof setTimeout>>();
let stopPresenceWatch: (() => void) | null = null;

function schedulePresenceRetry(nodeId: string) {
  clearTimeout(presenceRetryTimers.get(nodeId));
  presenceRetryTimers.set(nodeId, setTimeout(() => {
    presenceRetryTimers.delete(nodeId);
    if (failedTexts(nodeId).length === 0) return;
    if (connectedPeers.has(nodeId)) void retryFailedMessages(nodeId);
    else void dial(nodeId);
  }, PRESENCE_RETRY_DELAY_MS));
}

function watchPresenceForRetries() {
  const { onlineStatus } = usePresence();
  // Detached scope: the first useDM() caller is usually a component, and the
  // watcher must outlive it.
  const scope = effectScope(true);
  scope.run(() => {
    watch(onlineStatus, (now, before) => {
      for (const [peerId, online] of Object.entries(now)) {
        if (online && !before?.[peerId] && failedTexts(peerId).length > 0) {
          schedulePresenceRetry(peerId);
        }
      }
    });
  });
  stopPresenceWatch = () => scope.stop();
}

async function initDmListeners() {
  if (dmListenerInitialized) return;
  dmListenerInitialized = true;
  watchPresenceForRetries();

  dmUnlisteners.push(await listen<any>("dm-file-saved", (event) => {
    const { peer_id, file_id, local_path } = event.payload;
    if (!peer_id || !file_id) return;
    const fileMsg = findFileMsg(peer_id, file_id);
    if (fileMsg) {
      fileMsg.localPath = local_path;
      fileMsg.failed = false;
    }
  }));

  dmUnlisteners.push(await listen<any>("dm-file-transfer-failed", (event) => {
    const { peer_id, file_id, reason } = event.payload;
    if (!peer_id || !file_id) return;
    const fileMsg = findFileMsg(peer_id, file_id);
    if (fileMsg && fileMsg.localPath === null) {
      fileMsg.failed = true;
      fileMsg.failReason = typeof reason === "string" ? reason : undefined;
    }
  }));

  dmUnlisteners.push(await listen<any>("dm-received", (event) => {
    const { peer_id, message } = event.payload;
    if (!peer_id || !message) return;
    if (message.type === "text") {
      pushMessage(peer_id, {
        type: "text",
        content: message.content,
        timestamp: message.timestamp,
        from: "peer",
        status: "sent",
      });
    } else if (message.type === "file_start") {
      pushMessage(peer_id, {
        type: "file",
        name: message.name,
        size: message.size,
        id: message.id,
        progress: 0,
        localPath: null,
        from: "peer",
        timestamp: Date.now(),
      });
    } else if (message.type === "file_end") {
      const fileMsg = findFileMsg(peer_id, message.id);
      if (fileMsg) fileMsg.progress = 1;
    }
  }));

  // Lightweight per-chunk progress for the receiver. Raw chunk payloads are no
  // longer streamed over IPC (they're written to disk in Rust), so this tiny
  // event keeps the receive-side progress bar moving without flooding the bridge.
  dmUnlisteners.push(await listen<any>("dm-file-progress", (event) => {
    const { peer_id, file_id, received } = event.payload;
    if (!peer_id || !file_id) return;
    const fileMsg = findFileMsg(peer_id, file_id);
    if (fileMsg && fileMsg.size > 0) {
      fileMsg.progress = Math.min(1, received / fileMsg.size);
    }
  }));

  dmUnlisteners.push(await listen<{ peer_id?: string }>("dm-connected", (event) => {
    const pid = event.payload?.peer_id;
    if (!pid) return;
    connectedPeers.add(pid);
    delete connectErrors.value[pid];
    // A reconnect just landed — flush anything that failed while we were down.
    void retryFailedMessages(pid);
  }));

  dmUnlisteners.push(await listen<{ peer_id?: string }>("dm-disconnected", (event) => {
    const pid = event.payload?.peer_id;
    if (pid) connectedPeers.delete(pid);
  }));

  // Delivery ack for a Text message we sent — resolves to "delivered".
  // Backward compatible: an older peer never sends this, so our messages to
  // it simply stay at "sent" forever (no regression from today's behavior).
  dmUnlisteners.push(await listen<any>("dm-ack-received", (event) => {
    const { peer_id, id } = event.payload;
    if (peer_id && id) setDmTextStatus(peer_id, id, "delivered");
  }));
}

export function useDM() {
  // Auto-initialize listeners so passive consumers (TabBar, messages page)
  // receive DM events without needing to call connect() first
  void initDmListeners();

  async function connect(nodeId: string) {
    activeConversation = nodeId;
    await dial(nodeId);
  }

  async function sendText(nodeId: string, content: string) {
    const timestamp = Date.now();
    const pendingMessage: DmTextMessage = {
      type: "text",
      content,
      timestamp,
      from: "self",
      status: "sending",
      clientId: `${timestamp}-${Math.random().toString(36).slice(2)}`,
    };
    pushMessage(nodeId, pendingMessage);
    await deliverText(nodeId, pendingMessage);
  }

  // Manually retry a single failed message (e.g. tapping the FAILED label).
  async function resend(nodeId: string, message: DmTextMessage) {
    if (message.status === "sending") return;
    await deliverText(nodeId, message);
  }

  async function sendFile(nodeId: string, filePath: string) {
    const name = filePath.split(/[/\\]/).pop() || "file";
    // Create the card BEFORE the transfer invoke resolves — send_file only
    // returns once the whole file has streamed, so waiting for it to create
    // the card means a failed (or merely slow) outgoing transfer is
    // invisible until it either finishes or throws. A locally-unique
    // placeholder id lets the card render immediately in a pending state;
    // it's swapped for the backend's real transfer id on success.
    const pendingId = `pending-${Date.now()}-${Math.random().toString(36).slice(2)}`;
    pushMessage(nodeId, {
      type: "file", name, size: 0, id: pendingId, progress: 0,
      localPath: null, from: "self", timestamp: Date.now(),
    });
    try {
      const result = await invoke<{ id: string; size: number }>("send_file", { peerId: nodeId, filePath });
      const card = findFileMsg(nodeId, pendingId);
      if (card) {
        Object.assign(card, { id: result.id, size: result.size, progress: 1, localPath: filePath, failed: false });
      }
    } catch (error) {
      console.warn("[dm] send_file failed:", error);
      const card = findFileMsg(nodeId, pendingId);
      if (card) card.failed = true;
      throw error;
    }
  }

  function markRead(nodeId: string) {
    unreadCounts.value[nodeId] = 0;
  }

  // Only clear if it's still ours: on /dm/A → /dm/B the new page may have
  // set B before A's unmount runs.
  function clearActiveConversation(nodeId: string) {
    if (activeConversation === nodeId) activeConversation = null;
  }

  return {
    conversations, unreadCounts, totalUnread, connectErrors,
    connect, clearActiveConversation,
    sendText, resend, sendFile, markRead,
  };
}

// On HMR, tear down listeners and drop cached connection state so a reloaded
// module starts clean instead of leaking duplicate listeners or trusting a
// stale "connected" set.
if (import.meta.hot) {
  import.meta.hot.dispose(() => {
    for (const un of dmUnlisteners) un();
    dmUnlisteners = [];
    dmListenerInitialized = false;
    stopPresenceWatch?.();
    stopPresenceWatch = null;
    for (const timer of presenceRetryTimers.values()) clearTimeout(timer);
    presenceRetryTimers.clear();
    connectedPeers.clear();
    connectingPeers.clear();
    connectErrors.value = {};
  });
}
