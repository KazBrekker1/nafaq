import { isTauri } from "@tauri-apps/api/core";
import { platform } from "@tauri-apps/plugin-os";
import { relaunch } from "@tauri-apps/plugin-process";
import { check, type Update } from "@tauri-apps/plugin-updater";
import { computed, readonly, ref } from "vue";

export type UpdateStatus =
  | "idle"
  | "checking"
  | "available"
  | "downloading"
  | "installing"
  | "uptodate"
  | "unsupported"
  | "error";

const status = ref<UpdateStatus>("idle");
const latestVersion = ref<string | null>(null);
const releaseNotes = ref<string | null>(null);
const downloadProgress = ref(0);
const progressKnown = ref(false);
const errorMessage = ref<string | null>(null);

let pendingUpdate: Update | null = null;
let sessionCheck: Promise<void> | null = null;

const isUpdateAvailable = computed(() => status.value === "available");

function updatesSupported(): boolean {
  if (typeof window === "undefined" || !isTauri()) return false;
  const os = platform();
  return os !== "android" && os !== "ios";
}

async function checkForUpdate(): Promise<void> {
  if (status.value === "checking" || status.value === "downloading" || status.value === "installing") {
    return;
  }

  if (!updatesSupported()) {
    status.value = "unsupported";
    return;
  }

  status.value = "checking";
  errorMessage.value = null;

  try {
    const update = await check();
    if (!update) {
      pendingUpdate = null;
      latestVersion.value = null;
      releaseNotes.value = null;
      status.value = "uptodate";
      return;
    }

    pendingUpdate = update;
    latestVersion.value = update.version;
    releaseNotes.value = update.body ?? null;
    status.value = "available";
  } catch (error) {
    errorMessage.value = error instanceof Error ? error.message : String(error);
    status.value = "error";
  }
}

// Automatic check: runs at most once per app session, however many times the
// caller (e.g. the home page) remounts. Manual retries use checkForUpdate.
function checkForUpdateOnce(): Promise<void> {
  sessionCheck ??= checkForUpdate();
  return sessionCheck;
}

async function downloadAndInstall(): Promise<void> {
  if (!updatesSupported()) {
    status.value = "unsupported";
    return;
  }
  if (!pendingUpdate) {
    await checkForUpdate();
    if (!pendingUpdate) return;
  }

  status.value = "downloading";
  downloadProgress.value = 0;
  progressKnown.value = false;
  errorMessage.value = null;

  try {
    let downloaded = 0;
    let contentLength = 0;
    await pendingUpdate.downloadAndInstall((event) => {
      switch (event.event) {
        case "Started":
          contentLength = event.data.contentLength ?? 0;
          progressKnown.value = contentLength > 0;
          break;
        case "Progress":
          downloaded += event.data.chunkLength;
          if (contentLength > 0) {
            downloadProgress.value = Math.round((downloaded / contentLength) * 100);
          }
          break;
        case "Finished":
          downloadProgress.value = 100;
          progressKnown.value = true;
          status.value = "installing";
          break;
      }
    });

    status.value = "installing";
    await new Promise((resolve) => setTimeout(resolve, 700));
    await relaunch();
  } catch (error) {
    errorMessage.value = error instanceof Error ? error.message : String(error);
    status.value = "error";
  }
}

export function useAppUpdate() {
  return {
    status: readonly(status),
    latestVersion: readonly(latestVersion),
    releaseNotes: readonly(releaseNotes),
    downloadProgress: readonly(downloadProgress),
    progressKnown: readonly(progressKnown),
    errorMessage: readonly(errorMessage),
    isUpdateAvailable,
    checkForUpdate,
    checkForUpdateOnce,
    downloadAndInstall,
  };
}
