import type { RelayStatus } from "~/composables/useNodeRuntime";

export function formatTime(ts: number): string {
  const d = new Date(ts);
  return `${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}`;
}

// Compact list timestamp: "now", "5m", "14:32" (under a day), else "Mar 4".
// Pass `now` from useNow({ interval: 60_000 }) so rendered values advance.
export function formatRelativeTime(ts: number, now: number): string {
  const diffMins = Math.floor((now - ts) / 60_000);
  if (diffMins < 1) return "now";
  if (diffMins < 60) return `${diffMins}m`;
  if (diffMins < 24 * 60) return formatTime(ts);
  return new Date(ts).toLocaleDateString([], { month: "short", day: "numeric" });
}

export type RelayStatusColor = "success" | "error" | "neutral";

export function relayStatusColor(status: RelayStatus): RelayStatusColor {
  switch (status) {
    case "online":
      return "success";
    case "degraded":
    case "offline":
      return "error";
    default:
      return "neutral";
  }
}

export function truncateNodeId(id: string, head = 4, tail = 4): string {
  if (id.length <= head + tail + 3) return id;
  return `${id.slice(0, head)}...${id.slice(-tail)}`;
}

export function formatSize(bytes: number): string {
  if (bytes === 0) return "—";
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

export function avatarLetter(name: string): string {
  return (name || "?").charAt(0).toUpperCase();
}
