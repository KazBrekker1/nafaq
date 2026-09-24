import { invoke } from "@tauri-apps/api/core";

// The display name is not here: it lives in the pinned-name store
// (get_pinned_name / set_pinned_name, loaded by useCall).
export interface AppSettings {
  preferredMic: string | null;
  preferredCamera: string | null;
  preferredSpeaker: string | null;
  videoQuality: "auto" | "low" | "medium" | "high";
  dataSaver: boolean;
}

const settings = ref<AppSettings>({
  preferredMic: null,
  preferredCamera: null,
  preferredSpeaker: null,
  videoQuality: "auto",
  dataSaver: false,
});

let loadPromise: Promise<void> | null = null;
// Keys saved locally before the initial load resolved; the (older) stored
// values must not overwrite them.
const savedBeforeLoad = new Set<keyof AppSettings>();
let loadDone = false;

function load(): Promise<void> {
  // Shared by every useSettings() caller at startup.
  loadPromise ??= (async () => {
    try {
      const stored = await invoke<Partial<AppSettings>>("get_settings");
      for (const key of Object.keys(stored) as Array<keyof AppSettings>) {
        if (key in settings.value && !savedBeforeLoad.has(key)) {
          (settings.value as Record<string, unknown>)[key] = stored[key];
        }
      }
    } catch (e) {
      console.warn("[settings] get_settings failed:", e);
    } finally {
      loadDone = true;
      savedBeforeLoad.clear();
    }
  })();
  return loadPromise;
}

export function useSettings() {
  // Optimistic: applied immediately, reverted if the backend rejects it.
  // Resolves false (and logs) on failure so callers can surface it.
  async function save(patch: Partial<AppSettings>): Promise<boolean> {
    const keys = Object.keys(patch) as Array<keyof AppSettings>;
    const previous = Object.fromEntries(keys.map(k => [k, settings.value[k]]));
    Object.assign(settings.value, patch);
    if (!loadDone) keys.forEach(k => savedBeforeLoad.add(k));
    try {
      await invoke("update_settings", { settings: patch });
      return true;
    } catch (e) {
      console.warn("[settings] update_settings failed:", e);
      for (const key of keys) {
        // Only undo our own write, not a newer save of the same key.
        if (settings.value[key] === patch[key]) {
          (settings.value as Record<string, unknown>)[key] = previous[key];
          savedBeforeLoad.delete(key);
        }
      }
      return false;
    }
  }

  void load();

  return { settings, save };
}
