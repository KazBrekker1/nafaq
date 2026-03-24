import type { Command, Event } from "../shared/types";

type WebviewRPC = {
  send: {
    onSidecarEvent: (event: Event) => void;
    onSidecarStatus: (status: { connected: boolean }) => void;
  };
};

export class SidecarBridge {
  private ws: WebSocket | null = null;
  private port: number;
  private webviewRPC: WebviewRPC | null = null;
  private connected = false;
  private shouldReconnect = true;
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  private nodeId: string | null = null;

  constructor(port: number) {
    this.port = port;
  }

  setWebviewRPC(rpc: WebviewRPC): void {
    this.webviewRPC = rpc;
  }

  async connect(): Promise<void> {
    if (this.ws) return;

    const url = `ws://127.0.0.1:${this.port}`;
    console.log(`[bridge] Connecting to sidecar at ${url}`);

    return new Promise<void>((resolve) => {
      this.ws = new WebSocket(url);

      this.ws.onopen = () => {
        console.log("[bridge] Connected to sidecar");
        this.connected = true;
        this.webviewRPC?.send.onSidecarStatus({ connected: true });
        resolve();
      };

      this.ws.onmessage = (event: MessageEvent) => {
        if (typeof event.data === "string") {
          this.handleTextMessage(event.data);
        }
        // Binary frames (media) handled in Plan 3
      };

      this.ws.onclose = () => {
        console.log("[bridge] Disconnected from sidecar");
        this.connected = false;
        this.ws = null;
        this.webviewRPC?.send.onSidecarStatus({ connected: false });

        if (this.shouldReconnect) {
          this.scheduleReconnect();
        }
      };

      this.ws.onerror = (err) => {
        console.error("[bridge] WebSocket error:", err);
        resolve(); // Don't block on error
      };
    });
  }

  sendCommand(command: Command): boolean {
    if (!this.ws || !this.connected) {
      console.warn("[bridge] Cannot send command: not connected");
      return false;
    }

    const json = JSON.stringify(command);
    this.ws.send(json);
    return true;
  }

  disconnect(): void {
    this.shouldReconnect = false;
    if (this.reconnectTimer) {
      clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }
    if (this.ws) {
      this.ws.close();
      this.ws = null;
    }
    this.connected = false;
  }

  isConnected(): boolean {
    return this.connected;
  }

  getNodeId(): string | null {
    return this.nodeId;
  }

  private handleTextMessage(text: string): void {
    try {
      const event: Event = JSON.parse(text);
      if (event.type === "node_info") {
        this.nodeId = event.id;
      }
      this.webviewRPC?.send.onSidecarEvent(event);
    } catch (e) {
      console.error("[bridge] Failed to parse sidecar event:", e);
    }
  }

  private scheduleReconnect(): void {
    if (this.reconnectTimer) return;
    console.log("[bridge] Reconnecting in 2s...");
    this.reconnectTimer = setTimeout(async () => {
      this.reconnectTimer = null;
      await this.connect();
    }, 2000);
  }
}
