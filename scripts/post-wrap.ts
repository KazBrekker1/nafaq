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
