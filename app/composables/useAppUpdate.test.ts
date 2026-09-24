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

function fakeUpdate(version: string, downloadAndInstall = vi.fn()) {
  return { version, body: null, downloadAndInstall, close: vi.fn(async () => {}) };
}

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
      close: vi.fn(async () => {}),
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
    tauriMocks.check.mockResolvedValue(fakeUpdate("0.9.0", downloadAndInstall));
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

  it("checks automatically only once per session", async () => {
    tauriMocks.check.mockResolvedValue(null);
    const { useAppUpdate } = await freshUpdater();
    const updater = useAppUpdate();

    await updater.checkForUpdateOnce();
    await useAppUpdate().checkForUpdateOnce();

    expect(tauriMocks.check).toHaveBeenCalledOnce();
    expect(updater.status.value).toBe("uptodate");
  });

  it("surfaces check failures", async () => {
    tauriMocks.check.mockRejectedValue(new Error("manifest missing"));
    const { useAppUpdate } = await freshUpdater();
    const updater = useAppUpdate();

    await updater.checkForUpdate();

    expect(updater.status.value).toBe("error");
    expect(updater.errorMessage.value).toBe("manifest missing");
  });

  it("keeps automatic check failures silent", async () => {
    tauriMocks.check.mockRejectedValue(new Error("offline"));
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    const { useAppUpdate } = await freshUpdater();
    const updater = useAppUpdate();

    await updater.checkForUpdateOnce();

    expect(updater.status.value).toBe("idle");
    expect(updater.errorMessage.value).toBeNull();
    warn.mockRestore();
  });

  it("remembers the dismissed version", async () => {
    tauriMocks.check.mockResolvedValue(fakeUpdate("0.9.0"));
    const { useAppUpdate } = await freshUpdater();
    const updater = useAppUpdate();

    await updater.checkForUpdate();
    updater.dismissUpdate();

    expect(updater.dismissedVersion.value).toBe("0.9.0");
  });

  it("releases the previous update resource when a new check replaces it", async () => {
    const first = fakeUpdate("0.9.0");
    const second = fakeUpdate("0.9.1");
    tauriMocks.check.mockResolvedValueOnce(first).mockResolvedValueOnce(second);
    const { useAppUpdate } = await freshUpdater();
    const updater = useAppUpdate();

    await updater.checkForUpdate();
    await updater.checkForUpdate();

    expect(first.close).toHaveBeenCalledOnce();
    expect(second.close).not.toHaveBeenCalled();
    expect(updater.latestVersion.value).toBe("0.9.1");
  });

  it("waits for an in-flight check before installing", async () => {
    vi.useFakeTimers();
    let resolveCheck!: (update: unknown) => void;
    const update = fakeUpdate("0.9.0");
    tauriMocks.check.mockReturnValue(new Promise((resolve) => { resolveCheck = resolve; }));
    const { useAppUpdate } = await freshUpdater();
    const updater = useAppUpdate();

    const checking = updater.checkForUpdateOnce();
    const install = updater.downloadAndInstall();
    resolveCheck(update);
    await checking;
    await vi.runAllTimersAsync();
    await install;

    expect(tauriMocks.check).toHaveBeenCalledOnce();
    expect(update.downloadAndInstall).toHaveBeenCalledOnce();
    expect(tauriMocks.relaunch).toHaveBeenCalledOnce();
  });

  it("ignores a second install click while one is running", async () => {
    vi.useFakeTimers();
    const update = fakeUpdate("0.9.0");
    tauriMocks.check.mockResolvedValue(update);
    const { useAppUpdate } = await freshUpdater();
    const updater = useAppUpdate();

    await updater.checkForUpdate();
    const first = updater.downloadAndInstall();
    const second = updater.downloadAndInstall();
    await vi.runAllTimersAsync();
    await Promise.all([first, second]);

    expect(update.downloadAndInstall).toHaveBeenCalledOnce();
    expect(tauriMocks.relaunch).toHaveBeenCalledOnce();
  });

  it("surfaces download failures without relaunching", async () => {
    const update = fakeUpdate("0.9.0", vi.fn(async () => {
      throw new Error("signature rejected");
    }));
    tauriMocks.check.mockResolvedValue(update);
    const { useAppUpdate } = await freshUpdater();
    const updater = useAppUpdate();

    await updater.checkForUpdate();
    await updater.downloadAndInstall();

    expect(updater.status.value).toBe("error");
    expect(updater.errorMessage.value).toBe("signature rejected");
    expect(tauriMocks.relaunch).not.toHaveBeenCalled();
  });
});
