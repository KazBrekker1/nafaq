const SCHEME = "nafaq:";
const HOST_PATH = "//join";

export function wrapTicketUrl(ticket: string): string {
  return `nafaq://join?ticket=${encodeURIComponent(ticket)}`;
}

export function unwrapTicket(input: string): string {
  try {
    const url = new URL(input);
    if (url.protocol === SCHEME && url.pathname === HOST_PATH) {
      const t = url.searchParams.get("ticket");
      if (t && t.length > 0) return t;
    }
  } catch {
    // Not a valid URL — treat as raw ticket
  }
  return input;
}
