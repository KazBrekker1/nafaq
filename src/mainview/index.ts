import "./main.css";
import router from "./router";
import { createApp } from "vue";
import App from "./App.vue";
import type { Event, Command } from "../shared/types";

// ── Event Bus ──────────────────────────────────────────────

type EventHandler = (event: Event) => void;
type StatusHandler = (status: { connected: boolean }) => void;

const eventHandlers: EventHandler[] = [];
const statusHandlers: StatusHandler[] = [];

// ── Direct WebSocket to Sidecar ────────────────────────────
// Bypasses Electrobun RPC — connects the webview directly to the
// sidecar's WebSocket for both JSON commands and binary media.

const SIDECAR_PORT = 9320;
let ws: WebSocket | null = null;
let wsConnected = false;
let nodeId: string | null = null;
let reconnectTimer: ReturnType<typeof setTimeout> | null = null;

function connectToSidecar() {
  if (ws) return;

  ws = new WebSocket(`ws://localhost:${SIDECAR_PORT}`);
  ws.binaryType = "arraybuffer";

  ws.onopen = () => {
    wsConnected = true;
    for (const h of statusHandlers) h({ connected: true });
    // Request node info on connect
    sendCommand({ type: "get_node_info" });
  };

  ws.onmessage = (event: MessageEvent) => {
    if (typeof event.data === "string") {
      try {
        const parsed: Event = JSON.parse(event.data);
        if (parsed.type === "node_info") {
          nodeId = parsed.id;
        }
        for (const h of eventHandlers) {
          try { h(parsed); } catch (e) { console.error("Event handler error:", e); }
        }
      } catch {}
    }
    // Binary frames handled by useMediaTransport composable
  };

  ws.onclose = () => {
    wsConnected = false;
    ws = null;
    for (const h of statusHandlers) h({ connected: false });
    // Auto-reconnect
    if (!reconnectTimer) {
      reconnectTimer = setTimeout(() => {
        reconnectTimer = null;
        connectToSidecar();
      }, 2000);
    }
  };

  ws.onerror = () => {};
}

function sendCommand(command: Command): boolean {
  if (!ws || ws.readyState !== WebSocket.OPEN) return false;
  ws.send(JSON.stringify(command));
  return true;
}

// Start connecting (will keep retrying until sidecar is ready)
connectToSidecar();

// ── Public API ─────────────────────────────────────────────

export const nafaq = {
  async sendCommand(command: Command): Promise<boolean> {
    return sendCommand(command);
  },

  async getStatus(): Promise<{ connected: boolean; nodeId: string | null }> {
    return { connected: wsConnected, nodeId };
  },

  async createCall(): Promise<boolean> {
    return sendCommand({ type: "create_call" });
  },

  async joinCall(ticket: string): Promise<boolean> {
    return sendCommand({ type: "join_call", ticket });
  },

  async endCall(peerId: string): Promise<boolean> {
    return sendCommand({ type: "end_call", peer_id: peerId });
  },

  async sendChat(peerId: string, message: string): Promise<boolean> {
    return sendCommand({ type: "send_chat", peer_id: peerId, message });
  },

  onEvent(handler: EventHandler): () => void {
    eventHandlers.push(handler);
    return () => {
      const idx = eventHandlers.indexOf(handler);
      if (idx >= 0) eventHandlers.splice(idx, 1);
    };
  },

  onStatus(handler: StatusHandler): () => void {
    statusHandlers.push(handler);
    // Fire immediately with current status
    handler({ connected: wsConnected });
    return () => {
      const idx = statusHandlers.indexOf(handler);
      if (idx >= 0) statusHandlers.splice(idx, 1);
    };
  },
};

(window as any).nafaq = nafaq;

// ── Mount Vue App ──────────────────────────────────────────

const app = createApp(App);
app.use(router);
app.provide("nafaq", nafaq);
app.mount("#app");
