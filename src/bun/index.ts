import { BrowserWindow, BrowserView, Updater } from "electrobun/bun";
import { SidecarManager, resolveSidecarPath } from "./sidecar";
import { SidecarBridge } from "./bridge";

const SIDECAR_PORT = 9320;
const VITE_DEV_PORT = 5173;

const channel = await Updater.localInfo.channel();

const sidecar = new SidecarManager({
  port: SIDECAR_PORT,
  binaryPath: resolveSidecarPath(channel),
  onExit: (code) => {
    console.log(`[main] Sidecar exited with code ${code}`);
  },
});

const bridge = new SidecarBridge(SIDECAR_PORT);

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

// Start sidecar + bridge
await sidecar.start();
await bridge.connect();

// Determine view URL
let url = "views://mainview/index.html";
if (channel === "dev") {
  try {
    const res = await fetch(`http://localhost:${VITE_DEV_PORT}`, { method: "HEAD" });
    if (res.ok) {
      console.log("[main] Using Vite HMR dev server");
      url = `http://localhost:${VITE_DEV_PORT}`;
    }
  } catch {
    console.log("[main] Vite dev server not running. Run 'bun run dev:hmr' for HMR.");
  }
}

// Create the main window
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

// Wire bridge to forward events to webview
bridge.setWebviewRPC({
  send: {
    onSidecarEvent: (event) => {
      try { (mainWindow as any).defaultView.rpc.send.onSidecarEvent(event); } catch {}
    },
    onSidecarStatus: (status) => {
      try { (mainWindow as any).defaultView.rpc.send.onSidecarStatus(status); } catch {}
    },
  },
});

bridge.sendCommand({ type: "get_node_info" });
console.log("[main] Nafaq ready");

async function shutdown() {
  console.log("[main] Shutting down...");
  bridge.disconnect();
  await sidecar.stop();
  process.exit(0);
}

process.on("SIGINT", shutdown);
process.on("SIGTERM", shutdown);
