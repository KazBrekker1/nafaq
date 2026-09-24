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
}

export type DmMessageItem = DmTextMessage | DmFileMessage;

const conversations = ref<Record<string, DmMessageItem[]>>({});
const activeConversation = ref<string | null>(null);
const unreadCounts = ref<Record<string, number>>({});
// Peers the Rust backend has confirmed a live DM connection for (populated
// ONLY by the "dm-connected" event, never optimistically — otherwise the local
// view can claim "connected" when the backend actually rejected the stream and
// then refuse to re-dial).
const connectedPeers = new Set<string>();
// Peers with an in-flight connect_dm invoke, to de-dup concurrent connect()
// calls without falsely marking them connected.
const connectingPeers = new Set<string>();
// Peers whose most recent connect_dm attempt failed — cleared on the next
// successful connect (dm-connected event). Surfaced as a minimal inline
// notice; the failed message itself still retries via retryFailedMessages.
const connectErrors = ref<Record<string, boolean>>({});

let dmListenerInitialized = false;
let dmUnlisteners: Array<() => void> = [];

function findFileMsg(peerId: string, fileId: string): DmFileMessage | undefined {
  const msgs = conversations.value[peerId];
  if (!msgs) return undefined;
  return msgs.find(m => m.type === "file" && (m as DmFileMessage).id === fileId) as DmFileMessage | undefined;
}

function pushMessage(nodeId: string, msg: DmMessageItem) {
  if (!conversations.value[nodeId]) {
    conversations.value[nodeId] = [];
  }
  conversations.value[nodeId].push(msg);
  conversations.value = { ...conversations.value };
  if (activeConversation.value !== nodeId) {
    unreadCounts.value[nodeId] = (unreadCounts.value[nodeId] || 0) + 1;
    unreadCounts.value = { ...unreadCounts.value };
  }
}

// Patch an existing file message in place (by id) and trigger reactivity.
// Used for the sender side of a transfer, whose card is created before the
// transfer completes so a failure mid-transfer stays visible instead of the
// card simply never appearing (see sendFile).
function updateFileMessage(nodeId: string, fileId: string, patch: Partial<DmFileMessage>) {
  const msgs = conversations.value[nodeId];
  if (!msgs) return;
  const idx = msgs.findIndex(m => m.type === "file" && m.id === fileId);
  if (idx === -1) return;
  const updated = { ...(msgs[idx] as DmFileMessage), ...patch };
  const next = [...msgs];
  next[idx] = updated;
  conversations.value = { ...conversations.value, [nodeId]: next };
}

function updateDmTextMessageStatus(
  messages: DmMessageItem[],
  target: DmTextMessage,
  status: DmMessageStatus,
): DmMessageItem[] {
  return messages.map(message => {
    if (
      message.type === "text"
      && (message === target || (target.clientId && message.clientId === target.clientId))
    ) {
      return { ...message, status };
    }
    return message;
  });
}

function setDmTextStatus(nodeId: string, target: DmTextMessage, status: DmMessageStatus) {
  const messages = conversations.value[nodeId] ?? [];
  conversations.value = {
    ...conversations.value,
    [nodeId]: updateDmTextMessageStatus(messages, target, status),
  };
}

// Deliveries currently in flight, keyed by clientId. Prevents a manual resend
// racing the automatic retry on reconnect for the same message — both would
// write the final status, and the loser's verdict would win.
const inFlightTexts = new Set<string>();

// Deliver an existing text message (already in the conversation) and resolve its
// status to "sent" or "failed". Shared by sendText, manual resend, and the
// automatic retry on reconnect. Never throws — failure is surfaced via status.
async function deliverText(nodeId: string, message: DmTextMessage): Promise<boolean> {
  const flightKey = message.clientId ?? `${nodeId}:${message.timestamp}`;
  if (inFlightTexts.has(flightKey)) return false;
  inFlightTexts.add(flightKey);
  try {
    const { invoke } = await import("@tauri-apps/api/core");
    setDmTextStatus(nodeId, message, "sending");
    try {
      await invoke("send_dm", {
        peerId: nodeId,
        message: {
          type: "text",
          content: message.content,
          timestamp: message.timestamp,
          id: message.clientId,
        },
      });
      setDmTextStatus(nodeId, message, "sent");
      return true;
    } catch (error) {
      console.warn("[dm] send failed:", error);
      setDmTextStatus(nodeId, message, "failed");
      return false;
    }
  } finally {
    inFlightTexts.delete(flightKey);
  }
}

// Re-send any text messages still marked "failed" for this peer — called when
// a DM connection (re)establishes, so a message that failed during a transient
// disconnect is delivered automatically once the peer is reachable again.
//
// Sequential, in the conversation's original order — firing them all
// concurrently (as this used to) races multiple DM writes against each
// other with no guarantee the earlier message lands first, so a reader could
// see message 2 delivered before message 1. Each retry is awaited before the
// next starts; a message that fails again is left "failed" (not retried
// again in this pass) and the loop continues to the rest rather than
// aborting, since one still-unreachable message shouldn't block delivery of
// later ones that might succeed (e.g. after a partial network recovery).
async function retryFailedMessages(nodeId: string) {
  const msgs = conversations.value[nodeId];
  if (!msgs) return;
  const toRetry = msgs.filter(
    m => m.type === "text" && m.from === "self" && m.status === "failed",
  ) as DmTextMessage[];
  for (const m of toRetry) {
    await deliverText(nodeId, m);
  }
}

async function initDmListeners() {
  if (dmListenerInitialized) return;
  dmListenerInitialized = true;
  const { listen } = await import("@tauri-apps/api/event");
  dmUnlisteners.push(await listen<any>("dm-file-saved", (event) => {
    const { peer_id, file_id, local_path } = event.payload;
    if (!peer_id || !file_id) return;
    const fileMsg = findFileMsg(peer_id, file_id);
    if (fileMsg) {
      fileMsg.localPath = local_path;
      fileMsg.failed = false;
      conversations.value = { ...conversations.value };
    }
  }));

  dmUnlisteners.push(await listen<any>("dm-file-transfer-failed", (event) => {
    const { peer_id, file_id } = event.payload;
    if (!peer_id || !file_id) return;
    const fileMsg = findFileMsg(peer_id, file_id);
    if (fileMsg && fileMsg.localPath === null) {
      fileMsg.failed = true;
      conversations.value = { ...conversations.value };
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
      if (fileMsg) {
        fileMsg.progress = 1;
      }
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
      conversations.value = { ...conversations.value };
    }
  }));

  dmUnlisteners.push(await listen<any>("dm-connected", (event) => {
    const pid = typeof event.payload === "string" ? event.payload : event.payload?.peer_id;
    if (pid) {
      connectedPeers.add(pid);
      if (connectErrors.value[pid]) {
        const { [pid]: _removed, ...rest } = connectErrors.value;
        connectErrors.value = rest;
      }
      // A reconnect just landed — flush anything that failed while we were down.
      void retryFailedMessages(pid);
    }
  }));

  dmUnlisteners.push(await listen<any>("dm-disconnected", (event) => {
    const pid = typeof event.payload === "string" ? event.payload : event.payload?.peer_id;
    if (pid) connectedPeers.delete(pid);
  }));

  // Delivery ack for a Text message we sent — resolves "sent" to "delivered".
  // Backward compatible: an older peer never sends this, so our messages to
  // it simply stay at "sent" forever (no regression from today's behavior).
  dmUnlisteners.push(await listen<any>("dm-ack-received", (event) => {
    const { peer_id, id } = event.payload;
    if (!peer_id || !id) return;
    const msgs = conversations.value[peer_id];
    if (!msgs) return;
    conversations.value = {
      ...conversations.value,
      [peer_id]: msgs.map(m => (
        m.type === "text" && m.clientId === id ? { ...m, status: "delivered" as const } : m
      )),
    };
  }));
}

export function useDM() {
  // Auto-initialize listeners so passive consumers (TabBar, messages page)
  // receive DM events without needing to call connect() first
  initDmListeners();

  async function connect(nodeId: string) {
    activeConversation.value = nodeId;
    // Already connected, or a connect is already in flight — don't double-dial.
    if (connectedPeers.has(nodeId) || connectingPeers.has(nodeId)) return;
    connectingPeers.add(nodeId);
    try {
      const { invoke } = await import("@tauri-apps/api/core");
      await invoke("connect_dm", { nodeId });
      // Intentionally do NOT add to connectedPeers here — the "dm-connected"
      // event is the source of truth. If Rust rejected the stream, no event
      // fires and a later connect() correctly re-dials.
    } catch (error) {
      console.warn("[dm] connect_dm failed:", error);
      connectErrors.value = { ...connectErrors.value, [nodeId]: true };
    } finally {
      connectingPeers.delete(nodeId);
    }
  }

  async function disconnect(nodeId?: string) {
    const target = nodeId || activeConversation.value;
    if (!target) return;
    const { invoke } = await import("@tauri-apps/api/core");
    await invoke("disconnect_dm", { peerId: target }).catch(() => {});
    connectedPeers.delete(target);
    if (activeConversation.value === target) {
      activeConversation.value = null;
    }
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
      const { invoke } = await import("@tauri-apps/api/core");
      const result = await invoke<{ id: string; size: number }>("send_file", { peerId: nodeId, filePath });
      updateFileMessage(nodeId, pendingId, {
        id: result.id, size: result.size, progress: 1, localPath: filePath, failed: false,
      });
    } catch (error) {
      console.warn("[dm] send_file failed:", error);
      updateFileMessage(nodeId, pendingId, { failed: true });
      throw error;
    }
  }

  function markRead(nodeId: string) {
    unreadCounts.value = { ...unreadCounts.value, [nodeId]: 0 };
  }

  function clearActiveConversation() {
    activeConversation.value = null;
  }

  function totalUnread(): number {
    return Object.values(unreadCounts.value).reduce((a, b) => a + b, 0);
  }

  return {
    conversations, activeConversation, unreadCounts, connectErrors,
    connect, disconnect, clearActiveConversation,
    sendText, resend, sendFile, pushMessage, markRead, totalUnread,
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
    connectedPeers.clear();
    connectingPeers.clear();
    connectErrors.value = {};
  });
}
