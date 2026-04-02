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

export function useDM() {
  async function initListener() {
    if (dmListenerInitialized) return;
    dmListenerInitialized = true;
    const { listen } = await import("@tauri-apps/api/event");
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
        // Update progress for the matching file message
        const msgs = conversations.value[peer_id];
        if (msgs) {
          const fileMsg = msgs.find(m => m.type === "file" && m.id === message.id) as DmFileMessage | undefined;
          if (fileMsg && fileMsg.size > 0) {
            fileMsg.progress = Math.min(1, (message.offset + (message.data?.length || 0)) / fileMsg.size);
          }
        }
      } else if (message.type === "file_end") {
        const msgs = conversations.value[peer_id];
        if (msgs) {
          const fileMsg = msgs.find(m => m.type === "file" && m.id === message.id) as DmFileMessage | undefined;
          if (fileMsg) {
            fileMsg.progress = 1;
          }
        }
      }
    });
  }

  async function connect(nodeId: string) {
    await initListener();
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
    const id = await invoke<string>("send_file", { peerId: nodeId, filePath });
    pushMessage(nodeId, {
      type: "file", name, size: 0, id, progress: 0,
      localPath: filePath, from: "self", timestamp: Date.now(),
    });
  }

  function pushMessage(nodeId: string, msg: DmMessageItem) {
    if (!conversations.value[nodeId]) {
      conversations.value[nodeId] = [];
    }
    conversations.value[nodeId] = [...conversations.value[nodeId], msg];
    if (activeConversation.value !== nodeId) {
      unreadCounts.value = { ...unreadCounts.value, [nodeId]: (unreadCounts.value[nodeId] || 0) + 1 };
    }
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
