# Performance, Social Layer & UX Overhaul Design

**Date:** 2026-04-03
**Status:** Draft
**Scope:** Video pipeline optimization, memory/GC improvements, group call scaling, persistent identity, contacts, direct messaging, file transfer, settings page, tab-based navigation, mobile/in-call UX polish

## Problem

Nafaq's video decode pipeline wastes CPU on JPEG re-encoding every received frame. Memory allocation on hot paths creates GC pressure. Group calls degrade beyond 3 peers with no per-peer quality adaptation. Beyond performance, the app has no persistent identity, no way to save contacts, and no messaging outside of active calls. Users can't share files, have no settings page, and mobile users lack access to device controls.

## Design

### 1. Performance: Video Decode Pipeline

**Primary path — WebCodecs on frontend.** Eliminate Rust-side video decode entirely on supported platforms. Send raw H.264 NALUs over the binary channel to the frontend, where a per-peer `VideoDecoder` (WebCodecs API) decodes to canvas.

- Rust video forwarder stops decoding. Forwards raw H.264 bytes + metadata (peer ID, timestamp, width, height) over the binary channel.
- Frontend creates one `VideoDecoder` per peer. Decoded frames drawn to the peer's `<canvas>` via `VideoFrame`.
- Feature detection at startup: `typeof VideoDecoder !== 'undefined'`.

**Platform support:**

| Platform | Webview | WebCodecs | Status |
|----------|---------|-----------|--------|
| Windows | WebView2 (Chromium) | Yes | Evergreen, auto-updates |
| Android | Android WebView (Chromium) | Yes | Updated via Play Store |
| macOS | WKWebView (Safari) | macOS 13.3+ | Safari 16.4 added support |

**Fallback — Dedicated video runtime.** For platforms without WebCodecs (older macOS), keep Rust-side H.264 decode + JPEG re-encode but on a separate `tokio::Runtime` so audio never starves.

- Spawn a dedicated runtime for all video decode/encode work.
- Main Tauri runtime handles audio, connections, and IPC exclusively.
- Split `CodecState` into `AudioCodecState` (main runtime) and `VideoCodecState` (video runtime).
- Increase audio broadcast channel from 64 to 256 slots.

### 2. Performance: Memory & GC Pressure

**Pre-allocated ring buffers.** Replace per-frame `new Int16Array(960)` and `new Uint8Array(...)` allocations with a pool of ~8 reusable buffers per type, rotated on use. Allocated once at codec init.

**Eliminate base64 on event fallback.** When the binary channel isn't available, use `ArrayBuffer` transfer via Tauri's binary event support instead of base64 string encoding (33% size bloat + allocation per frame).

**Scoped decoder cleanup.** When WebCodecs is active for a peer, never allocate a Rust-side decoder for that peer. On the fallback path, clean up decoders immediately on peer disconnect (existing behavior) plus stale pruning at 5s intervals (existing behavior).

### 3. Performance: Group Call Scaling

Full mesh topology stays. Three improvements to make it sustainable at higher peer counts:

**Per-peer adaptive quality.** When network quality degrades for a specific peer (RTT > threshold, loss > threshold), reduce outbound resolution/bitrate only for that peer's stream. Other peers keep full quality. Currently quality changes are global.

**Receive-side video pausing.** If a peer's video tile is off-screen or the peer's video is toggled off, stop decoding their video frames. Send a control message telling the peer to reduce outbound quality to this node. Resume full quality when the tile is visible again.

**Selective audio decoding at 5+ peers.** Use the existing active speaker detection (300ms interval, RMS-based) to identify the top 2-3 loudest speakers. Only decode Opus for those peers. Others are muted until they exceed the speaking threshold. Reduces Opus decode work by 40-60% at 5+ peers.

### 4. Persistent Identity

**Secret key storage.** Use `tauri-plugin-store` to optionally persist the Iroh secret key.

On app launch:
1. Check store for persistent identity setting.
2. If **persistent**: load saved `SecretKey` → pass to `Endpoint::builder().secret_key(key)`.
3. If **ephemeral** (default): generate fresh key, don't save.
4. Toggle **ephemeral → persistent**: save current session key immediately.
5. Toggle **persistent → ephemeral**: delete saved key. Warn user that contacts using this node ID won't reach them after restart.

**Online presence.** Lightweight peer-to-peer presence probing:
- On startup (persistent identity only), attempt QUIC ping to each favorited contact's node ID via Iroh relay.
- Successful connection = online. Emit presence event to frontend.
- Re-probe unreachable contacts every 30 seconds.
- No central server.

### 5. Contacts System

**Data model.** Stored via `tauri-plugin-store`:

```rust
struct Contact {
    node_id: String,         // Iroh public key
    display_name: String,    // Name shared during call or DM
    added_at: u64,           // Unix timestamp
    last_seen: u64,          // Last successful connection
    source: ContactSource,   // Call or Manual
}
```

**Adding contacts:**
- **From a call:** Star button on peer's video tile or peer list. Saves node ID + display name.
- **Manually:** "+ ADD" in Contacts tab. Paste node ID or scan QR code, enter display name.

**Removing contacts:** Swipe-to-delete or long-press on mobile, hover-reveal delete on desktop.

### 6. Direct Messaging + File Transfer

**New QUIC stream.** Add DM stream (0x05) alongside existing streams:

| Stream | ID | Purpose |
|--------|----|---------|
| Audio | 0x01 | Existing |
| Video | 0x02 | Existing |
| Chat | 0x03 | Existing in-call chat |
| Control | 0x04 | Existing |
| **DM** | **0x05** | **Direct messages + file transfer** |

DMs operate independently from calls — no audio/video/control streams needed.

**Connection lifecycle:**
1. User opens DM thread → Rust connects to contact's node ID via `endpoint.connect(addr, NAFAQ_ALPN)`.
2. Opens only the DM stream (0x05).
3. Connection stays alive while DM view is open or a call is active (heartbeat on stream).
4. Closes when user navigates away AND no call is active, after 60s inactivity.

**Message format:**
```
{ type: "text", content: string, timestamp: u64 }
{ type: "file_start", name: string, size: u64, id: uuid }
{ type: "file_chunk", id: uuid, offset: u64, data: bytes }
{ type: "file_end", id: uuid }
```

Files chunked at 64KB, streamed over QUIC. Progress tracked by offset/size. Receiver writes to temp file, moves to final location on `file_end`.

**Delivery model:** Online-only. Both peers must be connected. If recipient is offline, show "user offline" — no queuing.

**Local storage:** DM history persisted via `tauri-plugin-store`, keyed by contact node ID. File metadata (name, size, local path) in message history. Actual files saved to downloads directory.

**Call escalation.** "CALL" button in DM header sends a `call_invite` control message on the DM stream. The peer's UI shows an incoming call prompt. On accept, both sides open audio/video/control streams (0x01-0x04) on the existing QUIC connection — no ticket exchange needed. The DM stream stays open alongside the call streams. When the call ends, only the call streams close; the DM connection persists.

### 7. Navigation: Tab Bar

Bottom tab bar with 3 tabs:

| Tab | Content |
|-----|---------|
| **Calls** | Current home page (create/join). Minimal changes. |
| **Contacts** | Favorite list with online status, per-contact message/call buttons, simplified identity card (name + node ID + QR/copy, no settings). "+ ADD" button. |
| **Messages** | Conversation list with last message preview and timestamps. Unread count badge on tab. Tap thread → DM conversation view. |

Gear icon in header (visible from any tab) opens Settings page.

### 8. Settings Page

Accessed via gear icon in the top-right header of any tab.

**Identity section:**
- Display name (editable, persisted via store)
- Node ID (read-only, truncated with QR/copy buttons)
- Persistent identity toggle with description

**Devices section:**
- Microphone dropdown
- Camera dropdown
- Speaker/output dropdown

**Call Quality section:**
- Video quality: Auto (default) / manual override (Low/Medium/High)
- Data saver toggle (forces lowest quality profile)

**About section:**
- App version, Iroh version

### 9. UX Polish

**In-call:**
- Mic level VU meter in pre-call overlay (reuse existing `AnalyserNode` from `useMedia`)
- Message delivery confirmation via "sent" indicator (QUIC stream ACK)
- 24-hour timestamps (locale-independent)

**Mobile:**
- Device selection accessible via Settings (no longer desktop-only)
- Fullscreen button visible on mobile
- Chat overlay respects soft keyboard via `visualViewport` API resize events

## Files Affected

### Rust (`src-tauri/src/`)

| File | Changes |
|------|---------|
| `node.rs` | Accept optional `SecretKey` parameter; load/save from store |
| `lib.rs` | Spawn dedicated video runtime (fallback); conditionally skip video decode when WebCodecs active; add DM stream handling; presence probing task |
| `codec.rs` | Split into `AudioCodecState` + `VideoCodecState`; add raw NALU forwarding mode |
| `connection.rs` | Add DM stream (0x05) open/read/write; per-peer quality adaptation; file chunking protocol; presence ping |
| `commands.rs` | New commands: `add_contact`, `remove_contact`, `get_contacts`, `send_dm`, `send_file`, `toggle_persistent_identity`, `get_settings`, `update_settings`, `check_presence` |
| `state.rs` | Add contacts store, DM state, settings state, video runtime handle |
| `messages.rs` | Add DM message types, file transfer types, presence events, settings events |
| `Cargo.toml` | Add `uuid` for file transfer IDs |

### Frontend (`app/`)

| File | Changes |
|------|---------|
| `app.vue` | Add tab bar layout wrapper |
| `pages/index.vue` | Becomes the Calls tab content (minimal changes) |
| `pages/contacts.vue` | **New** — contacts list with identity card, online status, per-contact actions |
| `pages/messages.vue` | **New** — DM thread list with previews |
| `pages/dm/[nodeId].vue` | **New** — DM conversation view with file transfer UI |
| `pages/settings.vue` | **New** — identity, devices, quality, about |
| `pages/call.vue` | Add star-contact button on peer tiles; receive-side video pause |
| `components/TabBar.vue` | **New** — bottom tab bar with unread badge |
| `components/AddContactModal.vue` | **New** — node ID paste / QR scan form |
| `components/FileMessage.vue` | **New** — file attachment display with progress and save |
| `composables/useContacts.ts` | **New** — contacts CRUD, persistence |
| `composables/useDM.ts` | **New** — DM state, send/receive, file transfer progress |
| `composables/useSettings.ts` | **New** — settings state, persistence |
| `composables/usePresence.ts` | **New** — online/offline status for contacts |
| `composables/useMediaTransport.ts` | Add WebCodecs decode path with feature detection; pre-allocated ring buffers; per-peer video pause; selective audio decode |
| `composables/useMedia.ts` | Expose VU meter data for pre-call overlay |

## Implementation Phases

**Phase 1 — Performance** (no UI changes needed, standalone):
- WebCodecs video decode path + dedicated video runtime fallback
- Ring buffers and base64 elimination
- Per-peer adaptive quality, receive-side video pause, selective audio decode

**Phase 2 — Identity & Settings** (foundation for social features):
- Secret key persistence toggle
- Settings page (identity, devices, quality)
- Gear icon in header

**Phase 3 — Contacts & Presence**:
- Contact data model and store
- Contacts tab UI
- Star-from-call flow
- Manual add (node ID / QR)
- Online presence probing

**Phase 4 — DMs & File Transfer**:
- DM QUIC stream (0x05)
- Messages tab and conversation view
- Text messaging
- File chunking and transfer
- Call escalation from DM

**Phase 5 — Navigation & UX Polish**:
- Tab bar component
- VU meter in pre-call
- Delivery confirmations
- Mobile fixes (fullscreen, keyboard, device access)
- 24-hour timestamps

## Out of Scope

- SFU/relay topology (contradicts P2P philosophy)
- Offline message queuing (requires store-and-forward infrastructure)
- Group DMs (1-on-1 only for now)
- End-to-end encryption changes (Iroh already provides TLS 1.3)
- Cross-device message sync
- File preview/inline rendering (files are download-only)

## Verification

1. **WebCodecs path:** Open a 2-peer call on Windows/Android. Confirm Rust CPU drops to near-zero for video decode. Verify canvas rendering is smooth.
2. **Fallback path:** Open a 2-peer call on older macOS. Confirm dedicated video runtime is active. Verify audio doesn't stutter when video is decoding.
3. **Group scaling:** 4-peer call. Confirm per-peer quality adaptation activates for the weakest connection. Minimize one peer's tile — verify their video decoding stops.
4. **Persistent identity:** Enable toggle, restart app. Confirm node ID is the same. Disable toggle, restart. Confirm node ID changes.
5. **Contacts:** Star a peer from a call. Verify they appear in Contacts tab. Add a contact manually via node ID. Verify online status probing works.
6. **DMs:** Open DM with an online contact. Send text messages both directions. Send a file — verify chunked transfer completes and file is saveable.
7. **Call escalation:** From a DM, click CALL. Verify both sides enter a call without manual ticket exchange.
8. **Settings:** Change default mic/camera/speaker. Start a call — verify the selected devices are used. Toggle data saver — verify quality profile drops.
9. **Mobile:** Open on Android. Verify tab bar renders, settings accessible, fullscreen works, chat overlay handles keyboard.
