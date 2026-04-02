export interface AppSettings {
  displayName: string;
  persistentIdentity: boolean;
  nodeId: string | null;
  preferredMic: string | null;
  preferredCamera: string | null;
  preferredSpeaker: string | null;
  videoQuality: "auto" | "low" | "medium" | "high";
  dataSaver: boolean;
}

const settings = ref<AppSettings>({
  displayName: "",
  persistentIdentity: false,
  nodeId: null,
  preferredMic: null,
  preferredCamera: null,
  preferredSpeaker: null,
  videoQuality: "auto",
  dataSaver: false,
});

const loaded = ref(false);

export function useSettings() {
  async function load() {
    const { invoke } = await import("@tauri-apps/api/core");
    const stored = await invoke<Partial<AppSettings>>("get_settings").catch(() => ({}));
    Object.assign(settings.value, stored);
    loaded.value = true;
  }

  async function save(patch: Partial<AppSettings>) {
    Object.assign(settings.value, patch);
    const { invoke } = await import("@tauri-apps/api/core");
    await invoke("update_settings", { settings: patch }).catch(() => {});
  }

  async function togglePersistentIdentity(enabled: boolean) {
    const { invoke } = await import("@tauri-apps/api/core");
    await invoke("toggle_persistent_identity", { enabled });
    settings.value.persistentIdentity = enabled;
  }

  if (!loaded.value) load();

  return { settings, loaded, save, togglePersistentIdentity };
}
