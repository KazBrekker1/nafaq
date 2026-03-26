# Performance & UX Optimization Design

**Date:** 2026-03-26
**Status:** Draft
**Scope:** Runtime isolation, adaptive media pipeline, connection flow redesign, error handling

## Problem

Nafaq's call quality degrades significantly when a 3rd peer joins. Audio and video both suffer — choppy audio, frame drops, pixelation. The connection flow also has UX issues: an oversized lobby page with a permanently disabled button, silent retry loops during node init, and no feedback during connection handshake.

## Root Cause Analysis

### Performance: Tokio Runtime Starvation

Audio and video packet processing share the same Tauri async runtime thread pool. H.264 decode + JPEG re-encode is CPU-intensive (~50-100ms per frame per peer). At 3 peers × 12 FPS, video work consumes 450-1200ms of CPU per second, starving audio packet processing.

**Contributing factors:**
- Audio broadcast channel is undersized (64 slots). At 150 packets/sec from 3 peers, buffer exhausts in ~400ms when the forwarder is blocked, causing `RecvError::Lagged` drops.
- `CodecState` holds all encoder/decoder instances. While the mutexes are already separate per field, all state lives in one struct managed on the shared runtime — video decode work on those fields still occupies runtime threads that audio needs.
- Fixed 400kbps bitrate with no adaptation for peer count. Three outbound video streams = 1.2 Mbps minimum.

### UX: Lobby and Feedback Gaps

- Full-page lobby (`lobby.vue`) shows camera preview and device selectors, but the "Ready" button is permanently disabled. User auto-navigates to call on peer connect with no warning.
- Node initialization has a silent 30-second retry loop (15 retries × 2s). User sees "Connecting..." with no progress.
- Join path shows "Connecting..." on the button with no detail about what's happening.
- Peer disconnect silently redirects to home if no peers remain.

## Design

### 1. Runtime Isolation

**Dedicated video runtime.** Spawn a separate `tokio::Runtime` for all video decode and encode work. The main Tauri async runtime handles audio, connection management, and IPC exclusively.

- Video forwarder task (H.264 decode → JPEG encode → emit to frontend) moves to the video runtime.
- Audio forwarder stays on the main runtime, uncontested.
- Video encode (`encode_and_send_video_all`) stays on main runtime since encoding is single-pass and lightweight compared to N-peer decoding.

**Split codec state.** Replace the single `CodecState` struct with separate `AudioCodecState` and `VideoCodecState`. The current struct already uses separate mutexes per field, so there is no cross-lock contention. The split is needed so that `VideoCodecState` (decoders + encoder) can be owned by the dedicated video runtime, while `AudioCodecState` stays on the main runtime. This is a clean ownership boundary, not a lock-contention fix.

```rust
// Before
pub struct CodecState {
    pub audio_encoder: Mutex<Option<AudioEncoder>>,
    pub audio_decoders: Mutex<HashMap<String, AudioDecoder>>,
    pub video_encoder: Mutex<Option<VideoEncoder>>,
    pub video_decoders: Mutex<HashMap<String, VideoDecoder>>,
}

// After
pub struct AudioCodecState {
    pub encoder: Mutex<Option<AudioEncoder>>,
    pub decoders: Mutex<HashMap<String, AudioDecoder>>,
}

pub struct VideoCodecState {
    pub encoder: Mutex<Option<VideoEncoder>>,
    pub decoders: Mutex<HashMap<String, VideoDecoder>>,
}
```

**Audio channel buffer increase.** Increase `broadcast::channel<AudioPacket>` capacity from 64 to 256. At 150 packets/sec from 3 peers, this provides ~1.7 seconds of buffer headroom. On `RecvError::Lagged`, skip to the latest packet rather than replaying stale frames.

### 2. Adaptive Group-Call Mode

Automatic quality scaling based on active peer count, triggered on `peer-connected` and `peer-disconnected` events.

| Peers | Bitrate | FPS | Max Resolution | Rationale |
|-------|---------|-----|----------------|-----------|
| 1-2   | 400 kbps | 12 | 640×360 | Current defaults, no change |
| 3     | 250 kbps | 10 | 480×270 | ~40% bandwidth reduction |
| 4+    | 150 kbps | 8  | 320×180 | Aggressive but smooth |

**Implementation:**
- `ConnectionManager` emits a `quality-profile-changed` event when peer count crosses a threshold.
- Rust side: reconfigure OpenH264 encoder bitrate and frame rate at runtime (supported by the API).
- JS side: update `VideoTrack` constraints to match the new resolution cap.
- Transitions are immediate on peer join/leave — no gradual ramp. The encoder handles bitrate changes cleanly between keyframes.

### 3. Connection Flow Redesign

**Remove `lobby.vue` page entirely.** Replace with an inline pre-call overlay (modal) that appears over the home page.

#### Joiner Flow (new)

```
Home page → Enter name + paste ticket → Click "Connect"
  → Progress feedback: "Connecting..." → "Establishing secure channel..." → "Connected"
  → Pre-call overlay appears (compact modal over home page):
      - Small camera preview thumbnail
      - Mic toggle, camera toggle (simple on/off, not device picker)
      - "Join Call" button
  → User clicks "Join Call" → navigates to call page
```

#### Creator Flow (new)

```
Home page → Enter name → Click "New Call"
  → Ticket displayed with copy/QR options
  → "Waiting for peer..." with animated indicator
  → Peer connects → same pre-call overlay appears
  → User clicks "Join Call" → navigates to call page
```

Both sides get the same confirmation step. Nobody is auto-navigated into a call.

#### Connection Progress Feedback

Replace silent operations with visible state at every stage:

1. **Node init:** Pulsing dot animation + "Starting node..." text. On success: green dot + "Node ready". On failure after retries: "Could not start — check your network" with retry button.
2. **Connecting to peer:** Step-by-step text updates — "Connecting..." → "Establishing secure channel..." → "Connected". Driven by Tauri events from the Rust connection manager.
3. **Pre-call overlay:** Appears only after connection is established. User controls when to enter the call.

### 4. Name Input Behavior

**Ephemeral by default.** The display name field starts empty on every app launch. No localStorage, no cookies, no auto-fill from previous sessions.

**Pin option.** A pin/lock toggle icon next to the name input:
- **Unpinned (default):** Name clears on app restart. Entered fresh per call.
- **Pinned:** Name persists via `tauri-plugin-store` (native key-value storage). Stays pinned across app restarts until the user unpins it.
- The pin state itself is stored in `tauri-plugin-store` alongside the name value.

This preserves the ephemeral, no-account feel of the app while letting repeat users opt into convenience.

### 5. Disconnect & Error Handling

**Peer disconnect toast.** When a peer leaves, show a brief toast notification: "[Name] left the call". Their video tile fades out. If other peers remain, the call continues. Adaptive quality recalculates for the new peer count.

**Last-peer-left prompt.** Instead of auto-redirecting to home when the last peer disconnects, show an in-call message: "Everyone has left" with a "Leave Call" button. The Iroh endpoint and ticket remain active — new peers can still join using the same ticket. If a peer joins, the call resumes normally with adaptive quality recalculation. The user decides when to exit; only clicking "Leave Call" tears down the session.

**Connection error recovery.** If the Iroh connection drops unexpectedly, show: "Connection lost — attempting to reconnect..." with a retry indicator. Only redirect to home after retries are exhausted, with a clear message: "Call ended — connection lost."

**Node init failure.** Replace the silent retry loop with visible progress. If init fails after all retries, show: "Could not start — check your network" with a manual retry button instead of hanging on "Connecting..." indefinitely.

## Files Affected

### Rust (`src-tauri/src/`)

| File | Changes |
|------|---------|
| `lib.rs` | Spawn dedicated video runtime; split codec state init; increase audio channel to 256; emit connection progress events |
| `codec.rs` | Split `CodecState` into `AudioCodecState` + `VideoCodecState`; add runtime bitrate/FPS reconfiguration methods |
| `connection.rs` | Emit `quality-profile-changed` event on peer count change; emit granular connection progress events; add reconnection logic |
| `state.rs` | Update `AppState` to hold separate codec states and video runtime handle |
| `commands.rs` | Update command signatures for split codec state; add name-pin store commands |
| `Cargo.toml` | Add `tauri-plugin-store` dependency |

### Frontend (`app/`)

| File | Changes |
|------|---------|
| `pages/lobby.vue` | **Delete** |
| `pages/index.vue` | Add pre-call overlay modal; connection progress UI; name pin toggle |
| `pages/call.vue` | Remove auto-redirect guards; add disconnect toast; add last-peer-left prompt; add connection-error overlay |
| `composables/useCall.ts` | Add connection progress states; remove auto-nav to lobby; handle overlay flow; add reconnection state |
| `composables/useMedia.ts` | Update capture constraints on quality profile change |
| `composables/useMediaTransport.ts` | Listen for `quality-profile-changed`; update encoder settings; adapt FPS target |
| `components/CallControls.vue` | Ensure full device picker is accessible here (already partially exists) |
| `components/PreCallOverlay.vue` | **New** — compact modal with camera preview, mic/cam toggles, "Join Call" button |
| `components/ConnectionProgress.vue` | **New** — step-by-step connection status indicator |
| `components/DisconnectToast.vue` | **New** — peer disconnect notification |
| `components/NameInput.vue` | **New** — name field with pin toggle, backed by `tauri-plugin-store` |

## Out of Scope

- SFU/relay topology for large groups (contradicts P2P philosophy)
- Simulcast / SVC encoding (complexity not justified for 3-5 peer target)
- WebCodecs migration for decode (runtime isolation solves the starvation without rearchitecting the bridge)
- Screen sharing
- End-to-end encryption changes (Iroh already provides this)
