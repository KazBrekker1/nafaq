import { describe, expect, it } from "vitest";
import { applySendLevel, sendQualityForLevel } from "./sendQuality";

const base = { bitrateBps: 400_000, fps: 12, maxWidth: 640, maxHeight: 360 };

describe("applySendLevel", () => {
  // Must stay in step with apply_level in src-tauri/src/quality.rs.
  it("matches the backend level table", () => {
    expect(applySendLevel(base, 0)).toEqual(base);
    expect(applySendLevel(base, 1)).toEqual({ bitrateBps: 240_000, fps: 8, maxWidth: 640, maxHeight: 360 });
    expect(applySendLevel(base, 2)).toEqual({ bitrateBps: 140_000, fps: 6, maxWidth: 320, maxHeight: 180 });
    expect(applySendLevel(base, 3)).toEqual({ bitrateBps: 80_000, fps: 5, maxWidth: 320, maxHeight: 180 });
  });

  it("never exceeds the base profile", () => {
    const small = { bitrateBps: 150_000, fps: 8, maxWidth: 320, maxHeight: 180 };
    expect(applySendLevel(small, 1).fps).toBe(8);
    expect(applySendLevel(small, 3).bitrateBps).toBe(80_000);
    expect(applySendLevel({ ...small, bitrateBps: 60_000 }, 3).bitrateBps).toBe(60_000);
  });

  it("maps levels to the UI quality indicator", () => {
    expect([0, 1, 2, 3].map(sendQualityForLevel)).toEqual(["good", "degraded", "poor", "poor"]);
  });
});
