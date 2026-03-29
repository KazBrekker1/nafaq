const SCHEME = "nafaq:";
const HOST_PATH = "//join";

export function wrapTicketUrl(ticket: string): string {
  return `${SCHEME}${HOST_PATH}?ticket=${encodeURIComponent(ticket)}`;
}

export function unwrapTicket(input: string): string {
  try {
    const url = new URL(input);
    if (url.protocol === SCHEME && url.pathname === HOST_PATH) {
      const t = url.searchParams.get("ticket");
      if (t && t.length > 0) return t;
    }
  } catch { /* not a URL */ }
  return input;
}
