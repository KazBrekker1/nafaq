# Deep Link Design: `nafaq://join?ticket=...`

## Summary

Enable sharing call invitations as `nafaq://join?ticket=<raw_ticket>` URLs. When tapped on a device with the app installed, the app opens and auto-joins the call. No server or domain required.

## Ticket URL Format

```
nafaq://join?ticket=<url_encoded_iroh_endpoint_ticket>
```

- The raw Iroh `EndpointTicket` string (up to 4KB) is URL-encoded and embedded as the `ticket` query parameter.
- Manual paste input accepts both raw tickets and full `nafaq://` URLs.
- QR codes contain the raw ticket (not the URL) to stay within QR size limits. The `nafaq://` URL is used only for the copy/share path.

## Platform Scope

- **Android**: supported via Tauri deep-link plugin config.
- **macOS**: supported via Tauri deep-link plugin config.
- **iOS**: out of scope for now.

## Components

### 1. Dependencies

**Rust** — add to `src-tauri/Cargo.toml`:
```toml
tauri-plugin-deep-link = "2"
```

**JS** — add to `package.json`:
```
@tauri-apps/plugin-deep-link
```

### 2. Plugin Registration

In `src-tauri/src/lib.rs`, register the plugin **before** `.setup()` to ensure cold-launch URLs are captured:

```rust
builder = builder.plugin(tauri_plugin_deep_link::init());
```

### 3. Tauri Config

Add to `src-tauri/tauri.conf.json`:

```json
"plugins": {
  "deep-link": {
    "desktop": { "schemes": ["nafaq"] },
    "mobile": [
      { "host": "join", "pathPrefix": "/", "scheme": "nafaq" }
    ]
  }
}
```

The plugin auto-generates the Android intent-filter — no manual `AndroidManifest.xml` edits needed.

### 4. Capabilities

Add `"deep-link:default"` to both:
- `src-tauri/capabilities/main.json`
- `src-tauri/capabilities/mobile.json`

### 5. Frontend: URL Helpers

Create `app/composables/useTicketUrl.ts` (follows existing composables convention):

```ts
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
```

### 6. Frontend: Deep Link Listener

In `useCall.ts` `initCallListeners()`:

- Import `onOpenUrl` from `@tauri-apps/plugin-deep-link`.
- On URL received, parse with `unwrapTicket()`.
- **If already in a call** (`state !== "idle"` and `state !== "waiting"`): silently ignore.
- Otherwise: call `joinCall(ticket)`.
- **Cold launch**: also call `getCurrent()` from the plugin on init to check for a pending URL that triggered app launch.

### 7. Sharing Changes

- `ConnectionShareModal.vue`: copy button uses `wrapTicketUrl(ticket)`. QR code uses **raw ticket** (avoids QR size limits).
- `TicketCreate.vue`: copy button uses `wrapTicketUrl(ticket)`.
- `TicketJoin.vue`: `submit()` and `onScan()` run `unwrapTicket()` on input before emitting. This handles both raw tickets and `nafaq://` URLs transparently.

## Flow

1. **Creator** clicks "New Call" -> gets ticket -> copies `nafaq://` URL or shows QR (raw ticket).
2. **Joiner (deep link)**: taps URL -> OS launches app -> deep-link handler fires -> `joinCall(ticket)` -> PreCallOverlay -> call page.
3. **Joiner (manual paste)**: pastes raw ticket or `nafaq://` URL -> `unwrapTicket()` normalizes -> same flow.
4. **Joiner (QR scan)**: scans QR containing raw ticket -> same flow.

## Edge Cases

- **App not installed**: nothing happens (custom scheme limitation, no fallback).
- **Already in a call**: deep link is silently ignored.
- **Malformed URL**: `unwrapTicket()` returns the input as-is; `joinCall` will fail with a backend parse error.
- **Cold launch (macOS)**: `getCurrent()` retrieves the buffered URL on init.

## What Doesn't Change

- Iroh ticket format and backend call flow unchanged.
- PreCallOverlay flow unchanged.
- `joinCall()` and `createCall()` Tauri commands unchanged.
