// Outbound video profile as decided by the Rust send-quality controller
// (src-tauri/src/quality.rs). The backend owns the decision and emits a level;
// this mirrors `apply_level` so capture resolution/fps follow the encoder.

export interface VideoProfile {
  bitrateBps: number;
  fps: number;
  maxWidth: number;
  maxHeight: number;
}

export type SendQuality = "good" | "degraded" | "poor";

export const MAX_SEND_LEVEL = 3;

export function applySendLevel(base: VideoProfile, level: number): VideoProfile {
  const clamped = Math.max(0, Math.min(MAX_SEND_LEVEL, Math.round(level)));
  const [bitratePct, fpsCap, small] = ([
    [100, Infinity, false],
    [60, 8, false],
    [35, 6, true],
    [20, 5, true],
  ] as const)[clamped]!;
  return {
    bitrateBps: Math.min(base.bitrateBps, Math.max(80_000, Math.floor(base.bitrateBps / 100) * bitratePct)),
    fps: Math.min(base.fps, fpsCap),
    maxWidth: small ? Math.min(base.maxWidth, 320) : base.maxWidth,
    maxHeight: small ? Math.min(base.maxHeight, 180) : base.maxHeight,
  };
}

export function sendQualityForLevel(level: number): SendQuality {
  if (level <= 0) return "good";
  if (level === 1) return "degraded";
  return "poor";
}
