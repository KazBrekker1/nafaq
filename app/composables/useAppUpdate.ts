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
// The version the user closed the modal on, so an automatic check doesn't
// pop the same update at them again this session.
const dismissedVersion = ref<string | null>(null);

let pendingUpdate: Update | null = null;
let inFlightCheck: Promise<void> | null = null;
let inFlightInstall: Promise<void> | null = null;
let sessionCheck: Promise<void> | null = null;

const isUpdateAvailable = computed(() => status.value === "available");

function updatesSupported(): boolean {
  if (typeof window === "undefined" || !isTauri()) return false;
  const os = platform();
  return os !== "android" && os !== "ios";
}

function errorText(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

// Swap the held Update resource, releasing the previous one on the Rust side.
function replacePendingUpdate(next: Update | null) {
  const previous = pendingUpdate;
  pendingUpdate = next;
  if (previous && previous !== next) {
    previous.close().catch((error) => console.warn("[update] failed to release update resource:", error));
  }
}

// `silent` checks (the automatic one) never surface an error: a failed
// background check leaves the status idle instead of flagging the UI.
async function runCheck(silent: boolean): Promise<void> {
  if (!updatesSupported()) {
    status.value = "unsupported";
    return;
  }

  status.value = "checking";
  errorMessage.value = null;

  try {
    const update = await check();
    replacePendingUpdate(update);
    latestVersion.value = update?.version ?? null;
    releaseNotes.value = update?.body ?? null;
    status.value = update ? "available" : "uptodate";
  } catch (error) {
    if (silent) {
      console.warn("[update] automatic update check failed:", error);
      status.value = "idle";
      return;
    }
    errorMessage.value = errorText(error);
    status.value = "error";
  }
}

function startCheck(silent: boolean): Promise<void> {
  if (inFlightCheck) return inFlightCheck;
  if (status.value === "downloading" || status.value === "installing") return Promise.resolve();
  inFlightCheck = runCheck(silent).finally(() => {
    inFlightCheck = null;
  });
  return inFlightCheck;
}

function checkForUpdate(): Promise<void> {
  return startCheck(false);
}

// Automatic check: runs at most once per app session, however many times the
// caller (e.g. the home page) remounts. Manual retries use checkForUpdate.
function checkForUpdateOnce(): Promise<void> {
  sessionCheck ??= startCheck(true);
  return sessionCheck;
}

function dismissUpdate() {
  dismissedVersion.value = latestVersion.value;
}

async function runInstall(): Promise<void> {
  if (!updatesSupported()) {
    status.value = "unsupported";
    return;
  }
  // Wait for a check that's already running (e.g. the automatic one) rather
  // than bailing out, then fall back to a fresh check if nothing is pending.
  if (inFlightCheck) await inFlightCheck;
  if (!pendingUpdate) await checkForUpdate();
  const update = pendingUpdate;
  if (!update) return;

  status.value = "downloading";
  downloadProgress.value = 0;
  progressKnown.value = false;
  errorMessage.value = null;

  try {
    let downloaded = 0;
    let contentLength = 0;
    await update.downloadAndInstall((event) => {
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
    errorMessage.value = errorText(error);
    status.value = "error";
  }
}

// De-duplicated: a second click while an install runs joins the first.
function downloadAndInstall(): Promise<void> {
  inFlightInstall ??= runInstall().finally(() => {
    inFlightInstall = null;
  });
  return inFlightInstall;
}

export function useAppUpdate() {
  return {
    status: readonly(status),
    latestVersion: readonly(latestVersion),
    releaseNotes: readonly(releaseNotes),
    downloadProgress: readonly(downloadProgress),
    progressKnown: readonly(progressKnown),
    errorMessage: readonly(errorMessage),
    dismissedVersion: readonly(dismissedVersion),
    isUpdateAvailable,
    checkForUpdate,
    checkForUpdateOnce,
    dismissUpdate,
    downloadAndInstall,
  };
}
