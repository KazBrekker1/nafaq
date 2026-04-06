export function formatTime(ts: number): string {
  const d = new Date(ts);
  return `${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}`;
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
