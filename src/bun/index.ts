import { BrowserWindow, Updater } from "electrobun/bun";
import { join } from "path";
import { existsSync } from "fs";
import { SidecarManager, resolveSidecarPath } from "./sidecar";

const SIDECAR_PORT = 9320;
const VIEW_SERVER_PORT = 5500;
const VITE_DEV_PORT = 5173;

const channel = await Updater.localInfo.channel();

// ── Start sidecar ──────────────────────────────────────────

const sidecar = new SidecarManager({
  port: SIDECAR_PORT,
  binaryPath: resolveSidecarPath(channel),
  onExit: (code) => {
    console.log(`[main] Sidecar exited with code ${code}`);
  },
});

await sidecar.start();

// ── Serve view files over HTTP ─────────────────────────────
// WKWebView blocks ws:// connections from the views:// custom scheme
// (opaque origin). Serving over http://localhost gives a proper origin.

function findViewDir(): string {
  // Production: files at Resources/app/views/mainview/
  const prodDir = join(import.meta.dir, "app", "views", "mainview");
  if (existsSync(join(prodDir, "index.html"))) return prodDir;

  // Dev: walk up to find dist/
  let dir = import.meta.dir;
  for (let i = 0; i < 10; i++) {
    const candidate = join(dir, "dist");
    if (existsSync(join(candidate, "index.html"))) return candidate;
    const parent = join(dir, "..");
    if (parent === dir) break;
    dir = parent;
  }

  return prodDir;
}

let viewUrl = `http://localhost:${VIEW_SERVER_PORT}`;

// In dev: prefer Vite dev server for HMR
if (channel === "dev") {
  try {
    const res = await fetch(`http://localhost:${VITE_DEV_PORT}`, { method: "HEAD" });
    if (res.ok) {
      console.log("[main] Using Vite HMR dev server");
      viewUrl = `http://localhost:${VITE_DEV_PORT}`;
    }
  } catch {}
}

// Start static file server for the view (unless using Vite HMR)
if (viewUrl === `http://localhost:${VIEW_SERVER_PORT}`) {
  const viewDir = findViewDir();
  console.log(`[main] Serving views from ${viewDir}`);

  Bun.serve({
    port: VIEW_SERVER_PORT,
    async fetch(req) {
      const url = new URL(req.url);
      let path = url.pathname === "/" ? "/index.html" : url.pathname;
      const filePath = join(viewDir, path);

      const file = Bun.file(filePath);
      if (await file.exists()) {
        return new Response(file);
      }
      // SPA fallback — serve index.html for client-side routing
      return new Response(Bun.file(join(viewDir, "index.html")));
    },
  });
  console.log(`[main] View server at ${viewUrl}`);
}

// ── Create window ──────────────────────────────────────────

const mainWindow = new BrowserWindow({
  title: "Nafaq",
  url: viewUrl,
  frame: {
    width: 1100,
    height: 750,
    x: 200,
    y: 100,
  },
});

console.log("[main] Nafaq ready");

// ── Cleanup ────────────────────────────────────────────────

async function shutdown() {
  console.log("[main] Shutting down...");
  await sidecar.stop();
  process.exit(0);
}

process.on("SIGINT", shutdown);
process.on("SIGTERM", shutdown);
