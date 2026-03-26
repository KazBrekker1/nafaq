# Nafaq Electrobun + Bridge Implementation Plan (Plan 2 of 3)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Set up the Electrobun desktop app shell, spawn the Iroh sidecar, and bridge IPC between the webview and sidecar so the Vue frontend (Plan 3) can send commands and receive events.

**Architecture:** Electrobun app with Bun main process that spawns the sidecar binary, connects to it via WebSocket, and relays commands/events to the Vue webview via Electrobun's typed RPC. The webview gets a clean async API (`nafaq.createCall()`, `nafaq.joinCall(ticket)`, etc.) without knowing about WebSockets or the sidecar.

**Tech Stack:** Electrobun v1.16, Bun 1.3, Vue 3 + Vite, TypeScript

**Prerequisites:** Plan 1 (sidecar) must be complete. The sidecar binary at `sidecar/target/debug/nafaq-sidecar` must exist.

**Related plans:**
- Plan 1: `2026-03-25-nafaq-sidecar.md` (Iroh sidecar — complete)
- Plan 3: `2026-03-25-nafaq-frontend.md` (Vue/NuxtUI frontend)

---

## File Structure

```
nafaq/
├── sidecar/                          # (Plan 1 — already complete)
├── src/
│   ├── bun/                          # Bun main process
│   │   ├── index.ts                  # App entry: window creation, startup sequence
│   │   ├── sidecar.ts                # Sidecar process lifecycle manager
│   │   └── bridge.ts                 # WebSocket client + RPC bridge to webview
│   ├── shared/
│   │   └── types.ts                  # IPC types shared between Bun and webview
│   └── mainview/                     # Vue webview (Electrobun view)
│       ├── index.html                # HTML entry point
│       ├── index.ts                  # Electroview RPC setup + nafaq API export
│       └── App.vue                   # Root Vue component (minimal for Plan 2)
├── electrobun.config.ts              # Electrobun build/app config
├── vite.config.ts                    # Vite config for Vue build
├── package.json
└── tsconfig.json
```

## Key Design Decisions

**Sidecar location:** In dev, use `sidecar/target/debug/nafaq-sidecar`. In production, use `PATHS.RESOURCES_FOLDER + "/bin/nafaq-sidecar"`. The sidecar manager resolves the path based on build environment.

**Bridge architecture:** Bun process maintains a single WebSocket connection to the sidecar. Commands from the webview are forwarded as JSON text frames. Events from the sidecar are forwarded to the webview via Electrobun RPC messages. Binary media frames bypass the webview RPC (too large) — they'll be handled separately in Plan 3 via a direct WebSocket from the webview to the sidecar for media-only traffic.

**RPC pattern:** The webview calls `rpc.request.sendCommand(cmd)` which returns a Promise. For events, the Bun process sends `rpc.send.onEvent(event)` messages to the webview. The webview maintains event listeners via a simple pub/sub.

---

### Task 1: Electrobun Project Setup

**Files:**
- Create: `package.json`
- Create: `electrobun.config.ts`
- Create: `vite.config.ts`
- Create: `tsconfig.json`
- Create: `src/mainview/index.html`
- Create: `src/mainview/index.ts` (stub)
- Create: `src/mainview/App.vue` (stub)
- Create: `src/bun/index.ts` (stub)

- [ ] **Step 1: Initialize the project**

Run: `cd /Users/yousseifelshahawy/coding/personal/nafaq && bunx electrobun init`

Select the `vue` template when prompted. If interactive prompts don't work, create files manually (see Step 2).

- [ ] **Step 2: Verify or create project files**

If `electrobun init` created the files, verify they match the structure above and adjust as needed. If it didn't work, create them manually:

`package.json`:
```json
{
  "name": "nafaq",
  "version": "0.1.0",
  "private": true,
  "scripts": {
    "dev": "electrobun dev",
    "dev:hmr": "concurrently \"bun run hmr\" \"bun run dev\"",
    "hmr": "vite --port 5173",
    "build": "vite build && electrobun build --env=canary",
    "build:sidecar": "cd sidecar && cargo build --release"
  },
  "dependencies": {
    "electrobun": "latest",
    "vue": "^3.5"
  },
  "devDependencies": {
    "@vitejs/plugin-vue": "^5",
    "vite": "^6",
    "concurrently": "^9",
    "typescript": "^5.7"
  }
}
```

`electrobun.config.ts`:
```typescript
import type { ElectrobunConfig } from "electrobun";

export default {
  app: {
    name: "Nafaq",
    identifier: "com.nafaq.app",
    version: "0.1.0",
  },
  runtime: {
    exitOnLastWindowClosed: true,
  },
  build: {
    bun: {
      entrypoint: "src/bun/index.ts",
    },
    views: {
      mainview: {
        entrypoint: "src/mainview/index.ts",
      },
    },
    copy: {
      "dist/index.html": "views/mainview/index.html",
      "dist/assets": "views/mainview/assets",
    },
    watchIgnore: ["dist/**", "sidecar/**"],
  },
  scripts: {
    postWrap: "./scripts/post-wrap.ts",
  },
} satisfies ElectrobunConfig;
```

`vite.config.ts`:
```typescript
import { defineConfig } from "vite";
import vue from "@vitejs/plugin-vue";

export default defineConfig({
  plugins: [vue()],
  root: "src/mainview",
  build: {
    outDir: "../../dist",
    emptyOutDir: true,
  },
  server: {
    port: 5173,
    strictPort: true,
  },
});
```

`tsconfig.json`:
```json
{
  "compilerOptions": {
    "target": "ES2022",
    "module": "ESNext",
    "moduleResolution": "bundler",
    "strict": true,
    "esModuleInterop": true,
    "skipLibCheck": true,
    "resolveJsonModule": true,
    "jsx": "preserve",
    "paths": {
      "@shared/*": ["./src/shared/*"]
    }
  },
  "include": ["src/**/*.ts", "src/**/*.vue"],
  "exclude": ["node_modules", "dist", "sidecar"]
}
```

`src/mainview/index.html`:
```html
<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1.0" />
  <title>Nafaq</title>
</head>
<body>
  <div id="app"></div>
  <script type="module" src="./index.ts"></script>
</body>
</html>
```

`src/mainview/App.vue`:
```vue
<template>
  <div style="font-family: 'JetBrains Mono', monospace; background: #000; color: #e2e8f0; min-height: 100vh; display: flex; align-items: center; justify-content: center;">
    <div style="text-align: center;">
      <h1 style="font-size: 48px; font-weight: 900; letter-spacing: 8px;">NAFAQ</h1>
      <p style="color: #666; font-size: 11px; letter-spacing: 4px; text-transform: uppercase;">P2P Encrypted Calls</p>
      <p style="color: #8B5CF6; margin-top: 2rem; font-size: 13px;" id="status">Initializing...</p>
    </div>
  </div>
</template>
```

`src/mainview/index.ts`:
```typescript
import { createApp } from "vue";
import App from "./App.vue";

createApp(App).mount("#app");
```

`src/bun/index.ts`:
```typescript
// Nafaq main process — implemented in later tasks
console.log("nafaq main process starting");
```

- [ ] **Step 3: Install dependencies**

Run: `bun install`
Expected: Dependencies install successfully

- [ ] **Step 4: Verify Vite builds the Vue app**

Run: `cd /Users/yousseifelshahawy/coding/personal/nafaq && bunx vite build --config vite.config.ts`
Expected: Build succeeds, `dist/` directory created with `index.html` and `assets/`

- [ ] **Step 5: Commit**

```bash
git add package.json electrobun.config.ts vite.config.ts tsconfig.json src/
git commit -m "feat(app): scaffold Electrobun project with Vue"
```

---

### Task 2: Shared IPC Types

**Files:**
- Create: `src/shared/types.ts`

- [ ] **Step 1: Define TypeScript types matching the sidecar's message protocol**

These must exactly mirror the Rust enums in `sidecar/src/messages.rs`.

`src/shared/types.ts`:
```typescript
// ============================================================
// IPC types shared between Bun main process and webview.
// These MUST match the Rust sidecar's message protocol exactly.
// See: sidecar/src/messages.rs
// ============================================================

/** Stream type identifiers for binary media frames */
export const STREAM_AUDIO = 0x01;
export const STREAM_VIDEO = 0x02;
export const STREAM_CHAT = 0x03;
export const STREAM_CONTROL = 0x04;

/** Control actions sent between peers */
export type ControlAction =
  | { action: "mute"; muted: boolean }
  | { action: "video_off"; off: boolean }
  | { action: "peer_announce"; peer_id: string; ticket: string };

/** Commands sent from Bun → Sidecar (JSON text frames) */
export type Command =
  | { type: "get_node_info" }
  | { type: "create_call" }
  | { type: "join_call"; ticket: string }
  | { type: "end_call"; peer_id: string }
  | { type: "send_chat"; peer_id: string; message: string }
  | { type: "send_control"; peer_id: string; action: ControlAction };

/** Events sent from Sidecar → Bun (JSON text frames) */
export type Event =
  | { type: "node_info"; id: string; ticket: string }
  | { type: "call_created"; ticket: string }
  | { type: "peer_connected"; peer_id: string }
  | { type: "peer_disconnected"; peer_id: string }
  | { type: "chat_received"; peer_id: string; message: string }
  | { type: "control_received"; peer_id: string; action: ControlAction }
  | { type: "connection_status"; peer_id: string; status: "direct" | "relayed" | "connecting" }
  | { type: "error"; message: string };

/** Media frame binary header (41 bytes) */
export const MEDIA_FRAME_HEADER_SIZE = 41; // 1 + 32 + 8

export interface MediaFrame {
  streamType: number;
  peerId: Uint8Array; // 32 bytes
  timestampMs: bigint;
  payload: Uint8Array;
}

export function encodeMediaFrame(frame: MediaFrame): Uint8Array {
  const buf = new Uint8Array(MEDIA_FRAME_HEADER_SIZE + frame.payload.length);
  buf[0] = frame.streamType;
  buf.set(frame.peerId, 1);
  const view = new DataView(buf.buffer);
  view.setBigUint64(33, frame.timestampMs, false); // big-endian
  buf.set(frame.payload, MEDIA_FRAME_HEADER_SIZE);
  return buf;
}

export function decodeMediaFrame(data: Uint8Array): MediaFrame | null {
  if (data.length < MEDIA_FRAME_HEADER_SIZE) return null;
  const view = new DataView(data.buffer, data.byteOffset);
  return {
    streamType: data[0],
    peerId: data.slice(1, 33),
    timestampMs: view.getBigUint64(33, false),
    payload: data.slice(MEDIA_FRAME_HEADER_SIZE),
  };
}

// ============================================================
// Electrobun RPC schema (Bun ↔ Webview)
// ============================================================

import type { RPCSchema } from "electrobun/bun";

/** RPC type for the main webview */
export type NafaqRPCType = {
  bun: RPCSchema<{
    requests: {
      /** Send a command to the sidecar, returns true if sent */
      sendCommand: {
        params: { command: Command };
        response: boolean;
      };
      /** Get current sidecar connection status */
      getSidecarStatus: {
        params: {};
        response: { connected: boolean; nodeId: string | null };
      };
    };
    messages: {};
  }>;
  webview: RPCSchema<{
    requests: {};
    messages: {
      /** Sidecar event forwarded to the webview */
      onSidecarEvent: Event;
      /** Sidecar connection status change */
      onSidecarStatus: { connected: boolean };
    };
  }>;
};
```

- [ ] **Step 2: Verify TypeScript compiles**

Run: `bunx tsc --noEmit --skipLibCheck`
Expected: No type errors (may have warnings about unresolved Electrobun types if not installed yet — that's OK)

- [ ] **Step 3: Commit**

```bash
git add src/shared/types.ts
git commit -m "feat(app): define shared IPC types matching sidecar protocol"
```

---

### Task 3: Sidecar Process Manager

**Files:**
- Create: `src/bun/sidecar.ts`

- [ ] **Step 1: Implement sidecar lifecycle manager**

`src/bun/sidecar.ts`:
```typescript
import { join } from "path";
import type { Subprocess } from "bun";

export interface SidecarOptions {
  port: number;
  /** Path to the sidecar binary. Auto-detected if not provided. */
  binaryPath?: string;
  onExit?: (exitCode: number | null) => void;
  onStdout?: (line: string) => void;
  onStderr?: (line: string) => void;
}

/**
 * Manages the nafaq-sidecar Rust process lifecycle.
 * Spawns the binary, monitors for crashes, and provides restart capability.
 */
export class SidecarManager {
  private proc: Subprocess | null = null;
  private options: SidecarOptions;
  private shouldRestart = true;
  private restartCount = 0;
  private maxRestarts = 5;
  private restartDelayMs = 1000;

  constructor(options: SidecarOptions) {
    this.options = options;
  }

  /** Resolve the sidecar binary path for current environment. */
  private getBinaryPath(): string {
    if (this.options.binaryPath) return this.options.binaryPath;

    // In dev: use cargo build output
    // In production: use bundled binary from resources
    try {
      // Try Electrobun's PATHS for production
      const PATHS = require("electrobun/bun").default;
      return join(PATHS.RESOURCES_FOLDER, "bin", "nafaq-sidecar");
    } catch {
      // Fallback to dev path (relative to project root)
      return join(import.meta.dir, "..", "..", "sidecar", "target", "debug", "nafaq-sidecar");
    }
  }

  /** Start the sidecar process. */
  async start(): Promise<void> {
    if (this.proc) {
      console.warn("[sidecar] Already running");
      return;
    }

    const binaryPath = this.getBinaryPath();
    console.log(`[sidecar] Starting: ${binaryPath} --port ${this.options.port}`);

    this.proc = Bun.spawn([binaryPath, "--port", String(this.options.port)], {
      stdout: "pipe",
      stderr: "pipe",
      onExit: (proc, exitCode, signalCode, error) => {
        console.log(`[sidecar] Exited with code ${exitCode}, signal ${signalCode}`);
        this.proc = null;
        this.options.onExit?.(exitCode);

        if (this.shouldRestart && this.restartCount < this.maxRestarts) {
          this.restartCount++;
          const delay = this.restartDelayMs * this.restartCount;
          console.log(`[sidecar] Restarting in ${delay}ms (attempt ${this.restartCount}/${this.maxRestarts})`);
          setTimeout(() => this.start(), delay);
        }
      },
    });

    // Stream stdout/stderr
    if (this.proc.stdout) {
      this.pipeStream(this.proc.stdout, (line) => {
        console.log(`[sidecar:out] ${line}`);
        this.options.onStdout?.(line);
      });
    }
    if (this.proc.stderr) {
      this.pipeStream(this.proc.stderr, (line) => {
        console.error(`[sidecar:err] ${line}`);
        this.options.onStderr?.(line);
      });
    }

    // Wait a moment for startup
    await Bun.sleep(500);
    console.log(`[sidecar] Started with PID ${this.proc.pid}`);
    this.restartCount = 0; // Reset on successful start
  }

  /** Stop the sidecar process gracefully. */
  async stop(): Promise<void> {
    this.shouldRestart = false;
    if (!this.proc) return;

    console.log("[sidecar] Stopping...");
    this.proc.kill("SIGTERM");

    // Wait up to 5 seconds for graceful shutdown
    const timeout = setTimeout(() => {
      if (this.proc) {
        console.warn("[sidecar] Force killing...");
        this.proc.kill("SIGKILL");
      }
    }, 5000);

    await this.proc.exited;
    clearTimeout(timeout);
    this.proc = null;
    console.log("[sidecar] Stopped");
  }

  /** Check if the sidecar is running. */
  isRunning(): boolean {
    return this.proc !== null;
  }

  private async pipeStream(
    stream: ReadableStream<Uint8Array>,
    onLine: (line: string) => void,
  ): Promise<void> {
    const reader = stream.getReader();
    const decoder = new TextDecoder();
    let buffer = "";

    try {
      while (true) {
        const { done, value } = await reader.read();
        if (done) break;
        buffer += decoder.decode(value, { stream: true });
        const lines = buffer.split("\n");
        buffer = lines.pop() || "";
        for (const line of lines) {
          if (line.trim()) onLine(line);
        }
      }
    } catch {
      // Stream closed
    }
  }
}
```

- [ ] **Step 2: Verify it compiles**

Run: `bunx tsc --noEmit --skipLibCheck src/bun/sidecar.ts`
Expected: No type errors

- [ ] **Step 3: Commit**

```bash
git add src/bun/sidecar.ts
git commit -m "feat(app): sidecar process manager with auto-restart"
```

---

### Task 4: WebSocket Bridge (Bun → Sidecar → Webview)

**Files:**
- Create: `src/bun/bridge.ts`

- [ ] **Step 1: Implement the WebSocket bridge**

`src/bun/bridge.ts`:
```typescript
import type { Command, Event, NafaqRPCType } from "../shared/types";

type WebviewRPC = {
  send: {
    onSidecarEvent: (event: Event) => void;
    onSidecarStatus: (status: { connected: boolean }) => void;
  };
};

/**
 * Bridges the sidecar's WebSocket to the Electrobun webview RPC.
 * Maintains a persistent WebSocket connection with auto-reconnect.
 */
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

  /** Set the webview RPC reference for forwarding events. */
  setWebviewRPC(rpc: WebviewRPC): void {
    this.webviewRPC = rpc;
  }

  /** Connect to the sidecar's WebSocket. */
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
        // Binary frames (media) will be handled in Plan 3
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
        // onclose will fire after this
        resolve(); // Don't block on error
      };
    });
  }

  /** Send a command to the sidecar. */
  sendCommand(command: Command): boolean {
    if (!this.ws || !this.connected) {
      console.warn("[bridge] Cannot send command: not connected");
      return false;
    }

    const json = JSON.stringify(command);
    this.ws.send(json);
    return true;
  }

  /** Disconnect from the sidecar. */
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

  /** Check connection status. */
  isConnected(): boolean {
    return this.connected;
  }

  /** Get the node ID (available after GetNodeInfo response). */
  getNodeId(): string | null {
    return this.nodeId;
  }

  private handleTextMessage(text: string): void {
    try {
      const event: Event = JSON.parse(text);

      // Track node ID
      if (event.type === "node_info") {
        this.nodeId = event.id;
      }

      // Forward to webview
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
```

- [ ] **Step 2: Verify it compiles**

Run: `bunx tsc --noEmit --skipLibCheck src/bun/bridge.ts`
Expected: No type errors

- [ ] **Step 3: Commit**

```bash
git add src/bun/bridge.ts
git commit -m "feat(app): WebSocket bridge with auto-reconnect for sidecar IPC"
```

---

### Task 5: Main Entry Point (Bun Process)

**Files:**
- Modify: `src/bun/index.ts`

- [ ] **Step 1: Wire up the main process**

`src/bun/index.ts`:
```typescript
import { BrowserWindow, BrowserView, Updater } from "electrobun/bun";
import type { NafaqRPCType } from "../shared/types";
import { SidecarManager } from "./sidecar";
import { SidecarBridge } from "./bridge";

const SIDECAR_PORT = 9320;
const VITE_DEV_PORT = 5173;

// ── Sidecar Setup ──────────────────────────────────────────

const sidecar = new SidecarManager({
  port: SIDECAR_PORT,
  onExit: (code) => {
    console.log(`[main] Sidecar exited with code ${code}`);
  },
});

const bridge = new SidecarBridge(SIDECAR_PORT);

// ── RPC Handlers (Bun side) ────────────────────────────────

const mainviewRPC = BrowserView.defineRPC<NafaqRPCType>({
  maxRequestTime: 10000,
  handlers: {
    requests: {
      sendCommand: ({ command }) => {
        return bridge.sendCommand(command);
      },
      getSidecarStatus: () => {
        return {
          connected: bridge.isConnected(),
          nodeId: bridge.getNodeId(),
        };
      },
    },
    messages: {},
  },
});

// ── Window Setup ───────────────────────────────────────────

async function getViewUrl(): Promise<string> {
  try {
    const channel = await Updater.localInfo.channel();
    if (channel === "dev") {
      const res = await fetch(`http://localhost:${VITE_DEV_PORT}`, { method: "HEAD" });
      if (res.ok) {
        console.log("[main] Using Vite HMR dev server");
        return `http://localhost:${VITE_DEV_PORT}`;
      }
    }
  } catch {}
  return "views://mainview/index.html";
}

async function main() {
  console.log("[main] Nafaq starting...");

  // 1. Start sidecar
  await sidecar.start();

  // 2. Connect bridge to sidecar
  await bridge.connect();

  // 3. Create the main window
  const url = await getViewUrl();
  const mainWindow = new BrowserWindow({
    title: "Nafaq",
    url,
    frame: {
      width: 1100,
      height: 750,
      x: 200,
      y: 100,
    },
  });

  // 4. Give bridge access to webview RPC for forwarding events
  bridge.setWebviewRPC({
    send: {
      onSidecarEvent: (event) => {
        mainWindow.defaultView.rpc.send.onSidecarEvent(event);
      },
      onSidecarStatus: (status) => {
        mainWindow.defaultView.rpc.send.onSidecarStatus(status);
      },
    },
  });

  // 5. Request initial node info
  bridge.sendCommand({ type: "get_node_info" });

  console.log("[main] Nafaq ready");
}

main().catch((err) => {
  console.error("[main] Fatal error:", err);
  process.exit(1);
});

// ── Cleanup ────────────────────────────────────────────────

process.on("SIGINT", async () => {
  console.log("[main] Shutting down...");
  bridge.disconnect();
  await sidecar.stop();
  process.exit(0);
});

process.on("SIGTERM", async () => {
  bridge.disconnect();
  await sidecar.stop();
  process.exit(0);
});
```

- [ ] **Step 2: Verify it compiles**

Run: `bunx tsc --noEmit --skipLibCheck src/bun/index.ts`
Expected: No type errors

- [ ] **Step 3: Commit**

```bash
git add src/bun/index.ts
git commit -m "feat(app): main entry point wiring sidecar, bridge, and window"
```

---

### Task 6: Webview RPC Setup

**Files:**
- Modify: `src/mainview/index.ts`

- [ ] **Step 1: Set up Electroview RPC and expose nafaq API**

`src/mainview/index.ts`:
```typescript
import { createApp } from "vue";
import { Electroview } from "electrobun/view";
import App from "./App.vue";
import type { NafaqRPCType, Event, Command } from "../shared/types";

// ── Event Bus ──────────────────────────────────────────────

type EventHandler = (event: Event) => void;
type StatusHandler = (status: { connected: boolean }) => void;

const eventHandlers: EventHandler[] = [];
const statusHandlers: StatusHandler[] = [];

// ── Electroview RPC ────────────────────────────────────────

const rpc = Electroview.defineRPC<NafaqRPCType>({
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

// ── Public API ─────────────────────────────────────────────

/** Nafaq API exposed to Vue components */
export const nafaq = {
  /** Send a command to the sidecar via the bridge */
  async sendCommand(command: Command): Promise<boolean> {
    return rpc.request.sendCommand({ command });
  },

  /** Get current sidecar connection status */
  async getStatus(): Promise<{ connected: boolean; nodeId: string | null }> {
    return rpc.request.getSidecarStatus({});
  },

  /** Convenience: create a new call */
  async createCall(): Promise<boolean> {
    return this.sendCommand({ type: "create_call" });
  },

  /** Convenience: join a call with a ticket */
  async joinCall(ticket: string): Promise<boolean> {
    return this.sendCommand({ type: "join_call", ticket });
  },

  /** Convenience: end a call */
  async endCall(peerId: string): Promise<boolean> {
    return this.sendCommand({ type: "end_call", peer_id: peerId });
  },

  /** Convenience: send a chat message */
  async sendChat(peerId: string, message: string): Promise<boolean> {
    return this.sendCommand({ type: "send_chat", peer_id: peerId, message });
  },

  /** Subscribe to sidecar events */
  onEvent(handler: EventHandler): () => void {
    eventHandlers.push(handler);
    return () => {
      const idx = eventHandlers.indexOf(handler);
      if (idx >= 0) eventHandlers.splice(idx, 1);
    };
  },

  /** Subscribe to sidecar connection status changes */
  onStatus(handler: StatusHandler): () => void {
    statusHandlers.push(handler);
    return () => {
      const idx = statusHandlers.indexOf(handler);
      if (idx >= 0) statusHandlers.splice(idx, 1);
    };
  },
};

// Make available globally for debugging
(window as any).nafaq = nafaq;

// ── Mount Vue App ──────────────────────────────────────────

const app = createApp(App);
app.provide("nafaq", nafaq);
app.mount("#app");
```

- [ ] **Step 2: Update App.vue to show connection status**

`src/mainview/App.vue`:
```vue
<script setup lang="ts">
import { ref, onMounted, onUnmounted, inject } from "vue";

const nafaq = inject<any>("nafaq");
const connected = ref(false);
const nodeId = ref<string | null>(null);

let unsubEvent: (() => void) | undefined;
let unsubStatus: (() => void) | undefined;

onMounted(() => {
  unsubStatus = nafaq?.onStatus((status: { connected: boolean }) => {
    connected.value = status.connected;
  });

  unsubEvent = nafaq?.onEvent((event: any) => {
    if (event.type === "node_info") {
      nodeId.value = event.id;
    }
  });

  // Check initial status
  nafaq?.getStatus().then((status: any) => {
    connected.value = status.connected;
    nodeId.value = status.nodeId;
  });
});

onUnmounted(() => {
  unsubEvent?.();
  unsubStatus?.();
});
</script>

<template>
  <div style="font-family: 'JetBrains Mono', monospace; background: #000; color: #e2e8f0; min-height: 100vh; display: flex; align-items: center; justify-content: center;">
    <div style="text-align: center;">
      <h1 style="font-size: 48px; font-weight: 900; letter-spacing: 8px;">NAFAQ</h1>
      <p style="color: #666; font-size: 11px; letter-spacing: 4px; text-transform: uppercase;">P2P Encrypted Calls</p>

      <div style="margin-top: 2rem; border: 2px solid #333; padding: 1rem;">
        <p style="font-size: 10px; text-transform: uppercase; letter-spacing: 3px; color: #666; margin-bottom: 0.5rem;">SIDECAR STATUS</p>
        <div style="display: flex; align-items: center; justify-content: center; gap: 8px;">
          <div :style="{ width: '8px', height: '8px', background: connected ? '#8B5CF6' : '#ff0000' }"></div>
          <span style="font-size: 12px;">{{ connected ? 'Connected' : 'Disconnected' }}</span>
        </div>
        <p v-if="nodeId" style="color: #555; font-size: 11px; margin-top: 0.5rem; word-break: break-all;">
          Node: {{ nodeId.slice(0, 16) }}...
        </p>
      </div>
    </div>
  </div>
</template>
```

- [ ] **Step 3: Rebuild Vue**

Run: `bunx vite build --config vite.config.ts`
Expected: Build succeeds

- [ ] **Step 4: Commit**

```bash
git add src/mainview/index.ts src/mainview/App.vue
git commit -m "feat(app): webview RPC setup with nafaq API and status display"
```

---

### Task 7: Sidecar Bundling Script

**Files:**
- Create: `scripts/post-wrap.ts`

- [ ] **Step 1: Create post-wrap script for production builds**

`scripts/post-wrap.ts`:
```typescript
import { cpSync, mkdirSync } from "fs";
import { join } from "path";

const wrapperPath = process.env.ELECTROBUN_WRAPPER_BUNDLE_PATH!;
const os = process.env.ELECTROBUN_OS!;
const arch = process.env.ELECTROBUN_ARCH!;

console.log(`[post-wrap] Bundling sidecar for ${os}-${arch}`);

// Copy the sidecar binary into the app's Resources directory
const sidecarSrc = join("sidecar", "target", "release", "nafaq-sidecar");
const sidecarDestDir = join(wrapperPath, "Contents", "Resources", "bin");
const sidecarDest = join(sidecarDestDir, "nafaq-sidecar");

mkdirSync(sidecarDestDir, { recursive: true });
cpSync(sidecarSrc, sidecarDest);

// Make executable
Bun.spawnSync(["chmod", "+x", sidecarDest]);

console.log(`[post-wrap] Sidecar bundled to ${sidecarDest}`);
```

- [ ] **Step 2: Commit**

```bash
git add scripts/post-wrap.ts
git commit -m "feat(app): post-wrap script to bundle sidecar binary"
```

---

### Task 8: End-to-End Smoke Test

This is a manual verification task — Electrobun requires a GUI environment.

- [ ] **Step 1: Build the sidecar**

Run: `cd sidecar && cargo build`
Expected: Binary at `sidecar/target/debug/nafaq-sidecar`

- [ ] **Step 2: Build the Vue app**

Run: `bunx vite build --config vite.config.ts`
Expected: `dist/` directory with built assets

- [ ] **Step 3: Start the Electrobun app**

Run: `electrobun dev`
Expected:
1. Console shows `[main] Nafaq starting...`
2. Console shows `[sidecar] Started with PID ...`
3. Console shows `[bridge] Connected to sidecar`
4. Console shows `[main] Nafaq ready`
5. Window appears with "NAFAQ" title and violet "Connected" indicator
6. Node ID fragment appears below the status

- [ ] **Step 4: Verify by inspecting window**

The app window should show:
- "NAFAQ" heading
- "P2P Encrypted Calls" subtitle
- "SIDECAR STATUS" box showing "Connected" with a violet dot
- Truncated node ID

If any step fails, check console output for errors and fix.

- [ ] **Step 5: Commit any fixes**

```bash
git add -u
git commit -m "fix(app): smoke test fixes"
```

---

## Verification Checklist

After completing all tasks:

- [ ] `bun install` succeeds
- [ ] `bunx vite build --config vite.config.ts` produces `dist/`
- [ ] `electrobun dev` starts the app with sidecar
- [ ] The window shows "Connected" status with a node ID
- [ ] Closing the window stops the sidecar process (check with `ps aux | grep nafaq-sidecar`)
- [ ] Killing the sidecar manually causes "Disconnected" to appear, then auto-restart reconnects

## Next Plan

After this plan is complete, proceed to **Plan 3: Frontend** (`2026-03-25-nafaq-frontend.md`) which builds out the Vue/NuxtUI pages (home, lobby, call, chat) using the `nafaq` API exposed by this bridge layer.
