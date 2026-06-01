export type DmMessageStatus = "sending" | "sent" | "failed";

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

// Deliver an existing text message (already in the conversation) and resolve its
// status to "sent" or "failed". Shared by sendText, manual resend, and the
// automatic retry on reconnect. Never throws — failure is surfaced via status.
async function deliverText(nodeId: string, message: DmTextMessage): Promise<boolean> {
  const { invoke } = await import("@tauri-apps/api/core");
  setDmTextStatus(nodeId, message, "sending");
  try {
    await invoke("send_dm", {
      peerId: nodeId,
      message: { type: "text", content: message.content, timestamp: message.timestamp },
    });
    setDmTextStatus(nodeId, message, "sent");
    return true;
  } catch (error) {
    console.warn("[dm] send failed:", error);
    setDmTextStatus(nodeId, message, "failed");
    return false;
  }
}

// Re-send any text messages still marked "failed" for this peer — called when
// a DM connection (re)establishes, so a message that failed during a transient
// disconnect is delivered automatically once the peer is reachable again.
function retryFailedMessages(nodeId: string) {
  const msgs = conversations.value[nodeId];
  if (!msgs) return;
  for (const m of msgs) {
    if (m.type === "text" && m.from === "self" && m.status === "failed") {
      void deliverText(nodeId, m as DmTextMessage);
    }
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
      // A reconnect just landed — flush anything that failed while we were down.
      retryFailedMessages(pid);
    }
  }));

  dmUnlisteners.push(await listen<any>("dm-disconnected", (event) => {
    const pid = typeof event.payload === "string" ? event.payload : event.payload?.peer_id;
    if (pid) connectedPeers.delete(pid);
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
    const { invoke } = await import("@tauri-apps/api/core");
    const name = filePath.split(/[/\\]/).pop() || "file";
    const result = await invoke<{ id: string; size: number }>("send_file", { peerId: nodeId, filePath });
    pushMessage(nodeId, {
      type: "file", name, size: result.size, id: result.id, progress: 1,
      localPath: filePath, from: "self", timestamp: Date.now(),
    });
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
    conversations, activeConversation, unreadCounts,
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
  });
}
