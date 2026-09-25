import { useSettings } from "~/composables/useSettings";
import { applySendLevel, capProfileForPreference, type VideoProfile } from "~/utils/sendQuality";
import { isAndroid } from "./shared";

export const DEFAULT_PROFILE: VideoProfile = {
  bitrateBps: 400_000,
  fps: isAndroid ? 8 : 12,
  maxWidth: 640,
  maxHeight: 360,
};

// Call-size profile (quality-profile-changed), capped by the user's video
// quality / data saver settings, and the backend controller's congestion
// level (send-quality-changed); capture follows their combination.
// Read-only outside this module.
let callSizeProfile: VideoProfile = { ...DEFAULT_PROFILE };
export let baseProfile: VideoProfile = { ...DEFAULT_PROFILE };
let sendLevel = 0;
// Counts quality-profile-changed events so the start-up profile fetch can't
// overwrite a newer event that raced it.
export let qualityProfileEvents = 0;

export interface QualityProfilePayload {
  peer_count: number;
  bitrate_bps: number;
  fps: number;
  max_width: number;
  max_height: number;
}

export function noteQualityProfileEvent() {
  qualityProfileEvents++;
}

export function setBaseProfileFromBackend({ bitrate_bps, fps, max_width, max_height }: QualityProfilePayload) {
  callSizeProfile = {
    bitrateBps: bitrate_bps,
    fps: isAndroid ? Math.min(fps, DEFAULT_PROFILE.fps) : fps,
    maxWidth: max_width,
    maxHeight: max_height,
  };
  refreshBaseProfile();
}

export function refreshBaseProfile() {
  const { videoQuality, dataSaver } = useSettings().settings.value;
  baseProfile = capProfileForPreference(callSizeProfile, videoQuality, dataSaver);
}

export function setSendLevel(level: number) {
  sendLevel = level;
}

export function resetProfiles() {
  callSizeProfile = { ...DEFAULT_PROFILE };
  baseProfile = { ...DEFAULT_PROFILE };
  sendLevel = 0;
}

export function effectiveProfile() {
  return applySendLevel(baseProfile, sendLevel);
}

function evenDimension(value: number, fallback: number) {
  const normalized = Number.isFinite(value) ? Math.max(2, Math.round(value)) : fallback;
  return normalized % 2 === 0 ? normalized : normalized - 1;
}

export function resolveCaptureDimensions(
  stream?: MediaStream | null,
  bounds: Pick<VideoProfile, "maxWidth" | "maxHeight"> = effectiveProfile(),
) {
  const track = stream?.getVideoTracks()[0];
  const settings = track?.getSettings();
  const sourceWidth = Number(settings?.width || 0);
  const sourceHeight = Number(settings?.height || 0);

  if (sourceWidth > 0 && sourceHeight > 0) {
    const scale = Math.min(bounds.maxWidth / sourceWidth, bounds.maxHeight / sourceHeight, 1);
    return {
      width: evenDimension(sourceWidth * scale, bounds.maxWidth),
      height: evenDimension(sourceHeight * scale, bounds.maxHeight),
    };
  }

  return {
    width: evenDimension(bounds.maxWidth, bounds.maxWidth),
    height: evenDimension(bounds.maxHeight, bounds.maxHeight),
  };
}
