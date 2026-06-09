import type { RelayStatus } from "./useNodeRuntime";

export type IdentityStatus = "loaded_persistent" | "created_persistent" | "reset_required";

export interface AppSettings {
  displayName: string;
  persistentIdentity: boolean;
  identityStatus: IdentityStatus | null;
  nodeId: string | null;
  relayStatus: RelayStatus;
  preferredMic: string | null;
  preferredCamera: string | null;
  preferredSpeaker: string | null;
  videoQuality: "auto" | "low" | "medium" | "high";
  dataSaver: boolean;
}

const settings = ref<AppSettings>({
  displayName: "",
  persistentIdentity: true,
  identityStatus: null,
  nodeId: null,
  relayStatus: "starting",
  preferredMic: null,
  preferredCamera: null,
  preferredSpeaker: null,
  videoQuality: "auto",
  dataSaver: false,
});

const loaded = ref(false);
let loadPromise: Promise<void> | null = null;

export function useSettings() {
  function load(): Promise<void> {
    // Cache the in-flight promise so concurrent useSettings() callers at
    // startup share one get_settings invoke instead of racing several.
    if (!loadPromise) {
      loadPromise = (async () => {
        const { invoke } = await import("@tauri-apps/api/core");
        const stored = await invoke<Partial<AppSettings>>("get_settings").catch(() => ({}));
        Object.assign(settings.value, stored);
        loaded.value = true;
      })().finally(() => {
        loadPromise = null;
      });
    }
    return loadPromise;
  }

  async function save(patch: Partial<AppSettings>) {
    Object.assign(settings.value, patch);
    const { invoke } = await import("@tauri-apps/api/core");
    await invoke("update_settings", { settings: patch }).catch(() => {});
  }

  if (!loaded.value) void load();

  return { settings, loaded, save };
}
