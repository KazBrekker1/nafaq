import { join } from "path";
import type { Subprocess } from "bun";

export interface SidecarOptions {
  port: number;
  binaryPath?: string;
  onExit?: (exitCode: number | null) => void;
  onStdout?: (line: string) => void;
  onStderr?: (line: string) => void;
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

  private getBinaryPath(): string {
    if (this.options.binaryPath) return this.options.binaryPath;

    // In dev: use cargo build output relative to project root
    // In production: use bundled binary from Electrobun resources
    try {
      const PATHS = require("electrobun/bun").default;
      return join(PATHS.RESOURCES_FOLDER, "bin", "nafaq-sidecar");
    } catch {
      return join(import.meta.dir, "..", "..", "sidecar", "target", "debug", "nafaq-sidecar");
    }
  }

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

    await Bun.sleep(500);
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
