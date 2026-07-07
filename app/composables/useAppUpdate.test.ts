import { beforeEach, describe, expect, it, vi } from "vitest";

const tauriMocks = vi.hoisted(() => ({
  check: vi.fn(),
  isTauri: vi.fn(() => true),
  platform: vi.fn(() => "macos"),
  relaunch: vi.fn(),
}));

vi.mock("@tauri-apps/api/core", () => ({
  isTauri: tauriMocks.isTauri,
}));

vi.mock("@tauri-apps/plugin-os", () => ({
  platform: tauriMocks.platform,
}));

vi.mock("@tauri-apps/plugin-process", () => ({
  relaunch: tauriMocks.relaunch,
}));

vi.mock("@tauri-apps/plugin-updater", () => ({
  check: tauriMocks.check,
}));

async function freshUpdater() {
  vi.resetModules();
  return await import("./useAppUpdate");
}

describe("useAppUpdate", () => {
  beforeEach(() => {
    vi.useRealTimers();
    vi.clearAllMocks();
    tauriMocks.isTauri.mockReturnValue(true);
    tauriMocks.platform.mockReturnValue("macos");
  });

  it("reports unsupported when automatic updates cannot run", async () => {
    tauriMocks.platform.mockReturnValue("android");
    const { useAppUpdate } = await freshUpdater();
    const updater = useAppUpdate();

    await updater.checkForUpdate();

    expect(updater.status.value).toBe("unsupported");
    expect(tauriMocks.check).not.toHaveBeenCalled();
  });

  it("stores available update metadata", async () => {
    tauriMocks.check.mockResolvedValue({
      version: "0.9.0",
      body: "Bug fixes",
      downloadAndInstall: vi.fn(),
    });
    const { useAppUpdate } = await freshUpdater();
    const updater = useAppUpdate();

    await updater.checkForUpdate();

    expect(updater.status.value).toBe("available");
    expect(updater.latestVersion.value).toBe("0.9.0");
    expect(updater.releaseNotes.value).toBe("Bug fixes");
    expect(updater.isUpdateAvailable.value).toBe(true);
  });

  it("downloads, tracks progress, and relaunches", async () => {
    vi.useFakeTimers();
    const downloadAndInstall = vi.fn(async (onEvent) => {
      onEvent({ event: "Started", data: { contentLength: 100 } });
      onEvent({ event: "Progress", data: { chunkLength: 40 } });
      onEvent({ event: "Progress", data: { chunkLength: 60 } });
      onEvent({ event: "Finished" });
    });
    tauriMocks.check.mockResolvedValue({
      version: "0.9.0",
      body: null,
      downloadAndInstall,
    });
    const { useAppUpdate } = await freshUpdater();
    const updater = useAppUpdate();

    await updater.checkForUpdate();
    const install = updater.downloadAndInstall();
    await vi.runAllTimersAsync();
    await install;

    expect(downloadAndInstall).toHaveBeenCalledOnce();
    expect(updater.downloadProgress.value).toBe(100);
    expect(updater.progressKnown.value).toBe(true);
    expect(updater.status.value).toBe("installing");
    expect(tauriMocks.relaunch).toHaveBeenCalledOnce();
  });

  it("surfaces check failures", async () => {
    tauriMocks.check.mockRejectedValue(new Error("manifest missing"));
    const { useAppUpdate } = await freshUpdater();
    const updater = useAppUpdate();

    await updater.checkForUpdate();

    expect(updater.status.value).toBe("error");
    expect(updater.errorMessage.value).toBe("manifest missing");
  });
});
