import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";

const releaseWorkflow = readFileSync(resolve(".github/workflows/release.yml"), "utf8");

describe("release workflow", () => {
  it("requires updater signing and uploads latest.json after signed desktop artifacts", () => {
    expect(releaseWorkflow).toContain("branches:\n      - main");
    expect(releaseWorkflow).toContain("TAURI_SIGNING_PRIVATE_KEY: ${{ secrets.TAURI_SIGNING_PRIVATE_KEY }}");
    expect(releaseWorkflow).toContain("TAURI_SIGNING_PRIVATE_KEY is required for signed updater artifacts");
    expect(releaseWorkflow).toContain("updater-manifest:");
    expect(releaseWorkflow).toContain("needs: [create-release, publish-desktop]");
    expect(releaseWorkflow).toContain('SIG_NAME="${ART}.sig"');
    expect(releaseWorkflow).toContain("ERROR: missing signature $SIG_NAME for $TARGET");
    expect(releaseWorkflow).toContain("gh release upload \"$TAG\" latest.json --repo \"$REPO\" --clobber");
  });

  it("pins GitHub Actions to immutable commit SHAs", () => {
    const mutableActionRefs = releaseWorkflow.match(/^\s*uses:\s+[^\s@]+\/[^\s@]+@(?:v\d+|main|master|stable)\s*$/gm);

    expect(mutableActionRefs).toBeNull();
  });
});
