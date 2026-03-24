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

// ── RPC Setup ──────────────────────────────────────────────

let rpc: any = null;

try {
  const { Electroview } = require("electrobun/view");

  rpc = Electroview.defineRPC({
    handlers: {
      requests: {},
      messages: {
        onSidecarEvent: (event: Event) => {
          for (const handler of eventHandlers) {
            try { handler(event); } catch (e) { console.error("Event handler error:", e); }
          }
        },
        onSidecarStatus: (status: { connected: boolean }) => {
          for (const handler of statusHandlers) {
            try { handler(status); } catch (e) { console.error("Status handler error:", e); }
          }
        },
      },
    },
  });
} catch {
  console.log("[view] Electrobun not available, running standalone");
}

// ── Public API ─────────────────────────────────────────────

export const nafaq = {
  async sendCommand(command: Command): Promise<boolean> {
    if (!rpc) return false;
    return rpc.request.sendCommand({ command });
  },

  async getStatus(): Promise<{ connected: boolean; nodeId: string | null }> {
    if (!rpc) return { connected: false, nodeId: null };
    return rpc.request.getSidecarStatus({});
  },

  async createCall(): Promise<boolean> {
    return this.sendCommand({ type: "create_call" });
  },

  async joinCall(ticket: string): Promise<boolean> {
    return this.sendCommand({ type: "join_call", ticket });
  },

  async endCall(peerId: string): Promise<boolean> {
    return this.sendCommand({ type: "end_call", peer_id: peerId });
  },

  async sendChat(peerId: string, message: string): Promise<boolean> {
    return this.sendCommand({ type: "send_chat", peer_id: peerId, message });
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
