export interface DmTextMessage {
  type: "text";
  content: string;
  timestamp: number;
  from: "self" | "peer";
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
}

export type DmMessageItem = DmTextMessage | DmFileMessage;

const conversations = ref<Record<string, DmMessageItem[]>>({});
const activeConversation = ref<string | null>(null);
const unreadCounts = ref<Record<string, number>>({});

let dmListenerInitialized = false;

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

async function initDmListeners() {
  if (dmListenerInitialized) return;
  dmListenerInitialized = true;
  const { listen } = await import("@tauri-apps/api/event");
  listen<any>("dm-file-saved", (event) => {
    const { peer_id, file_id, local_path } = event.payload;
    if (!peer_id || !file_id) return;
    const fileMsg = findFileMsg(peer_id, file_id);
    if (fileMsg) {
      fileMsg.localPath = local_path;
      conversations.value = { ...conversations.value };
    }
  });

  listen<any>("dm-received", (event) => {
    const { peer_id, message } = event.payload;
    if (!peer_id || !message) return;
    if (message.type === "text") {
      pushMessage(peer_id, {
        type: "text",
        content: message.content,
        timestamp: message.timestamp,
        from: "peer",
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
    } else if (message.type === "file_chunk") {
      const fileMsg = findFileMsg(peer_id, message.id);
      if (fileMsg && fileMsg.size > 0) {
        fileMsg.progress = Math.min(1, (message.offset + (message.data?.length || 0)) / fileMsg.size);
      }
    } else if (message.type === "file_end") {
      const fileMsg = findFileMsg(peer_id, message.id);
      if (fileMsg) {
        fileMsg.progress = 1;
      }
    }
  });
}

export function useDM() {
  // Auto-initialize listeners so passive consumers (TabBar, messages page)
  // receive DM events without needing to call connect() first
  initDmListeners();

  async function connect(nodeId: string) {
    const { invoke } = await import("@tauri-apps/api/core");
    await invoke("connect_dm", { nodeId });
    activeConversation.value = nodeId;
  }

  async function disconnect() {
    if (!activeConversation.value) return;
    const { invoke } = await import("@tauri-apps/api/core");
    await invoke("disconnect_dm", { peerId: activeConversation.value }).catch(() => {});
    activeConversation.value = null;
  }

  async function sendText(nodeId: string, content: string) {
    const { invoke } = await import("@tauri-apps/api/core");
    const timestamp = Date.now();
    await invoke("send_dm", {
      peerId: nodeId,
      message: { type: "text", content, timestamp },
    });
    pushMessage(nodeId, { type: "text", content, timestamp, from: "self" });
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

  function totalUnread(): number {
    return Object.values(unreadCounts.value).reduce((a, b) => a + b, 0);
  }

  return {
    conversations, activeConversation, unreadCounts,
    connect, disconnect, sendText, sendFile,
    pushMessage, markRead, totalUnread,
  };
}
