import { join } from "path";
import { existsSync } from "fs";
import type { Subprocess } from "bun";

export interface SidecarOptions {
  port: number;
  binaryPath: string;
  onExit?: (exitCode: number | null) => void;
}

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

  async start(): Promise<void> {
    if (this.proc) return;

    const { binaryPath, port } = this.options;

    if (!existsSync(binaryPath)) {
      console.error(`[sidecar] Binary not found: ${binaryPath}`);
      console.error("[sidecar] Build it with: cd sidecar && cargo build");
      return;
    }

    console.log(`[sidecar] Starting: ${binaryPath} --port ${port}`);

    this.proc = Bun.spawn([binaryPath, "--port", String(port)], {
      stdout: "pipe",
      stderr: "pipe",
      onExit: (_proc, exitCode, signalCode, _error) => {
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

    if (this.proc.stdout) this.pipeStream(this.proc.stdout, (l) => console.log(`[sidecar:out] ${l}`));
    if (this.proc.stderr) this.pipeStream(this.proc.stderr, (l) => console.error(`[sidecar:err] ${l}`));

    await Bun.sleep(2000);
    console.log(`[sidecar] Started with PID ${this.proc?.pid}`);
    this.restartCount = 0;
  }

  async stop(): Promise<void> {
    this.shouldRestart = false;
    if (!this.proc) return;

    console.log("[sidecar] Stopping...");
    this.proc.kill("SIGTERM");

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

  isRunning(): boolean {
    return this.proc !== null;
  }

  private async pipeStream(stream: ReadableStream<Uint8Array>, onLine: (line: string) => void): Promise<void> {
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
    } catch {}
  }
}

/** Resolve the sidecar binary path based on environment. */
export function resolveSidecarPath(channel: string): string {
  if (channel !== "dev") {
    // Production: bundled in app's Resources/bin/
    return join(import.meta.dir, "bin", "nafaq-sidecar");
  }

  // Dev: the Electrobun build dir is .../build/dev-macos-arm64/App.app/Contents/Resources/
  // Walk up to find the project root (contains sidecar/Cargo.toml)
  let dir = import.meta.dir;
  for (let i = 0; i < 10; i++) {
    const candidate = join(dir, "sidecar", "target", "debug", "nafaq-sidecar");
    if (existsSync(candidate)) return candidate;
    const parent = join(dir, "..");
    if (parent === dir) break;
    dir = parent;
  }

  // Last resort: check if it's in PATH
  return "nafaq-sidecar";
}
