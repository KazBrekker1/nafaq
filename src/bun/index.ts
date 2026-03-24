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

// ── Electrobun Integration ─────────────────────────────────

async function main() {
  console.log("[main] Nafaq starting...");

  // 1. Start sidecar
  await sidecar.start();

  // 2. Connect bridge to sidecar
  await bridge.connect();

  // 3. Try to create Electrobun window
  try {
    // Dynamic import — only available inside the Electrobun runtime
    const electrobun: any = await import("electrobun/bun");
    const { BrowserWindow, BrowserView, Updater } = electrobun;

    // Define RPC handlers for the webview
    BrowserView.defineRPC({
      maxRequestTime: 10000,
      handlers: {
        requests: {
          sendCommand: (params: any) => {
            return bridge.sendCommand(params.command);
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

    // Determine view URL (Vite dev server or built assets)
    let url = "views://mainview/index.html";
    try {
      const channel = await Updater.localInfo.channel();
      if (channel === "dev") {
        const res = await fetch(`http://localhost:${VITE_DEV_PORT}`, { method: "HEAD" });
        if (res.ok) {
          console.log("[main] Using Vite HMR dev server");
          url = `http://localhost:${VITE_DEV_PORT}`;
        }
      }
    } catch {}

    const mainWindow: any = new BrowserWindow({
      title: "Nafaq",
      url,
      frame: {
        width: 1100,
        height: 750,
        x: 200,
        y: 100,
      },
    });

    // Give bridge access to webview RPC for forwarding events
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
  } catch (e) {
    console.log("[main] Electrobun not available, running in headless mode");
    console.log("[main] Bridge connected:", bridge.isConnected());
  }

  // 4. Request initial node info
  bridge.sendCommand({ type: "get_node_info" });

  console.log("[main] Nafaq ready");
}

main().catch((err) => {
  console.error("[main] Fatal error:", err);
  process.exit(1);
});

async function shutdown() {
  console.log("[main] Shutting down...");
  bridge.disconnect();
  await sidecar.stop();
  process.exit(0);
}

process.on("SIGINT", shutdown);
process.on("SIGTERM", shutdown);
