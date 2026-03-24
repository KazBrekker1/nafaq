# Nafaq — P2P Voice & Video Calling App Design Spec

**Date**: 2026-03-24
**Status**: Approved

## Context

Nafaq is a peer-to-peer voice and video calling application built on Iroh (Rust QUIC networking library). The core motivation is fully decentralized, serverless communication — no central infrastructure, no TURN servers, no accounts. Users connect directly via Iroh's NAT holepunching over QUIC, with TLS 1.3 encryption built in.

The app ships as an Electrobun desktop application (Bun runtime + native webview) with a Vue + Nuxt UI v4 frontend and a brutalist design aesthetic (violet #8B5CF6 accent, monospace typography, sharp edges, high contrast).

## Architecture

### Three-Layer Desktop App

```
┌─────────────────────────────────────────────┐     WebSocket      ┌──────────────────────┐
│ ELECTROBUN APP                              │    (JSON+Binary)   │ IROH SIDECAR (Rust)  │
│                                             │                    │                      │
│  ┌───────────────────────────────────────┐  │                    │  ● Iroh Node         │
│  │ WEBVIEW (Vue + Nuxt UI v4)            │  │                    │  ● Connection Mgmt   │
│  │  ● Media Capture (getUserMedia)       │  │                    │  ● NAT Holepunching  │
│  │  ● WebCodecs (encode/decode)          │  │                    │  ● QUIC Streams      │
│  │  ● Call UI + Chat UI                  │  │        ⟷           │  ● TLS 1.3           │
│  └──────────┬────────────────────────────┘  │                    │                      │
│             │ Named Pipes (Electrobun IPC)   │                    │                      │
│  ┌──────────┴────────────────────────────┐  │                    │                      │
│  │ BUN PROCESS                           │  │                    │                      │
│  │  ● IPC Bridge (webview ↔ sidecar)     │──┼────────────────────┤                      │
│  │  ● Sidecar Process Manager            │  │                    │                      │
│  │  ● App State                          │  │                    │                      │
│  └───────────────────────────────────────┘  │                    │                      │
└─────────────────────────────────────────────┘                    └──────────────────────┘
                                                                            │
                                                                    P2P over Internet
                                                                   QUIC/UDP ● Holepunched
                                                                            │
                                                                   [Same arch on peer]
```

### Why This Architecture

- **Sidecar (not FFI/NAPI)**: Crash isolation — Iroh crash doesn't kill the app. Clean separation of concerns. The Rust sidecar can be reused for future native apps without the Electrobun wrapper.
- **WebSocket IPC**: Simple, well-supported, handles both JSON control messages and binary media frames. Runs over localhost.
- **WebCodecs in webview**: Leverages the browser engine's hardware-accelerated codecs. No need to ship ffmpeg or custom codec implementations.

### Bundling & Distribution

- Iroh sidecar compiled as a standalone Rust binary per platform (macOS arm64/x86_64, Windows, Linux)
- Included in Electrobun app bundle's resources directory (e.g., `Contents/Resources/bin/nafaq-sidecar`)
- Electrobun's zlib compression applies to the whole bundle (~4-6MB overhead for sidecar)
- Differential updates via Electrobun's BSDIFF patching
- Bun spawns sidecar at app launch, resolving path relative to app bundle

## Connection & Call Flow

### 1-on-1 Calls

1. Caller clicks "New Call" → sidecar generates a call ticket (Iroh node ID + ALPN + relay info)
2. Ticket displayed as copyable text + QR code
3. Caller shares ticket out-of-band (any messenger, email, etc.)
4. Callee clicks "Join Call" → pastes ticket or scans QR
5. Sidecar connects to peer via Iroh → holepunching attempts direct UDP path
6. QUIC connection established → streams opened → call begins

### Group Calls (Mesh, 3-8 people)

1. Host creates call → gets ticket (same as 1-on-1)
2. Each joiner connects to the host via ticket
3. Host broadcasts each participant's node ID to all others via control stream
4. Each peer establishes direct connections to every other peer (full mesh)
5. n*(n-1)/2 total connections (e.g., 6 connections for 4 peers, 28 for 8 peers)

### QUIC Stream Protocol

Each peer connection uses 4 dedicated QUIC streams via ALPN:

| Stream | ALPN | Direction | Purpose |
|--------|------|-----------|---------|
| Audio | `nafaq/audio/1` | Unidirectional (each direction) | Opus-encoded audio frames |
| Video | `nafaq/video/1` | Unidirectional (each direction) | VP8/VP9 encoded video frames |
| Chat | `nafaq/chat/1` | Bidirectional | JSON text messages |
| Control | `nafaq/control/1` | Bidirectional | Join/leave/mute/peer discovery |

## Media Pipeline

### Encoding (Webview via WebCodecs)

- **Audio**: Opus codec, 48kHz sample rate, 20ms frames (~100-300 bytes/frame)
- **Video**: VP8 (no licensing issues), target 720p@30fps, adaptive bitrate
- WebCodecs provides hardware-accelerated encoding/decoding in the webview

### Frame Format (IPC Binary Messages)

```
[stream_type: u8][peer_id: 32 bytes][timestamp: u64][payload: variable]
```

- Audio frames: ~100-300 bytes each
- Video frames: ~5-50KB each (I-frames larger, P-frames smaller)
- Total bandwidth per peer: ~500kbps-2Mbps depending on video quality

### Adaptive Quality

- Monitor QUIC congestion signals from Iroh
- Reduce video resolution/framerate when bandwidth constrained
- Audio always prioritized over video
- Graceful degradation: 720p → 480p → 360p → audio-only

## UI Design

### Aesthetic: Brutalist Modern

- **Typography**: JetBrains Mono / SF Mono (monospace throughout)
- **Edges**: Sharp — no border-radius
- **Colors**: Black (#000) backgrounds, white (#e2e8f0) borders/text, violet (#8B5CF6) accent
- **Labels**: Uppercase with wide letter-spacing (CAMERA, MICROPHONE, MESSAGES)
- **Buttons/content**: Normal case (New Call, Join Call, Mic, Cam, End)
- **Borders**: 2px solid, exposed structure
- **No**: gradients, shadows, emoji icons, rounded corners

### Screens

1. **Home** — App name (NAFAQ), "New Call" + "Join Call" buttons, node ID display
2. **Ticket Exchange** — Share: copyable ticket + QR code + waiting indicator. Join: paste field + QR scanner + connect button
3. **Pre-Call Lobby** — Camera preview with "Live" indicator, device dropdowns (camera, mic), mic level visualizer, Mic Off / Cam Off toggles, "Join Call →" button
4. **In-Call (1-on-1)** — Remote video full-screen, self PiP (bottom-right, white border), top bar (timer + "P2P Direct" indicator), bottom controls (Mic | Cam | Chat | End), collapsible chat sidebar
5. **Group Call Grid** — Adaptive grid (2x2 for 4, 2x3 for 5-6, etc.), 2px white grid lines, participant name labels, violet connection indicators

## Error Handling

| Scenario | Behavior |
|----------|----------|
| Holepunching fails | Iroh auto-falls back to relay. UI shows "Relayed" instead of "P2P Direct" |
| Peer disconnects | Detect via QUIC close/timeout. Show "Reconnecting..." with auto-reconnect (exponential backoff) |
| Sidecar crashes | Bun detects child process exit, restarts sidecar, re-establishes connections. Brief "Reconnecting..." UI |
| Media device lost | `devicechange` event. Prompt user to select alternative or continue with remaining media |
| Group mesh partial failure | Control messages can gossip through connected peers. Media requires direct connection |

## Project Structure

```
nafaq/
├── sidecar/                        # Rust crate (Iroh sidecar binary)
│   ├── Cargo.toml
│   └── src/
│       ├── main.rs                 # Entry point, WebSocket IPC server
│       ├── node.rs                 # Iroh node lifecycle
│       ├── connection.rs           # Peer connection management
│       ├── protocol.rs             # QUIC stream protocols (ALPN definitions)
│       └── ipc.rs                  # WebSocket server for Bun communication
├── src/                            # Electrobun app (Bun + Vue)
│   ├── main/                       # Bun main process
│   │   ├── index.ts                # App entry, window creation
│   │   ├── sidecar.ts              # Sidecar process lifecycle manager
│   │   └── bridge.ts               # IPC bridge (webview ↔ sidecar)
│   └── renderer/                   # Vue webview
│       ├── app.vue
│       ├── pages/
│       │   ├── home.vue            # Home screen
│       │   ├── lobby.vue           # Pre-call lobby
│       │   └── call.vue            # In-call view
│       ├── components/
│       │   ├── VideoGrid.vue       # Adaptive video grid
│       │   ├── ChatSidebar.vue     # In-call chat
│       │   ├── CallControls.vue    # Mic/Cam/Chat/End buttons
│       │   └── TicketExchange.vue  # Create/join ticket UI
│       ├── composables/
│       │   ├── useMedia.ts         # getUserMedia + WebCodecs
│       │   ├── useCall.ts          # Call state machine
│       │   └── useChat.ts          # Chat message handling
│       └── lib/
│           └── ipc.ts              # Typed IPC bindings to Bun process
├── electrobun.config.ts
└── package.json
```

## Verification

### How to test end-to-end

1. **Sidecar standalone**: Build and run `cargo run` in `sidecar/`. Verify it starts an Iroh node and listens on WebSocket. Test with a simple WebSocket client sending connect/disconnect commands.

2. **Two-node local test**: Run two sidecar instances on localhost. Generate a ticket from node A, connect from node B. Verify bidirectional QUIC streams work for audio/video/chat data.

3. **Electrobun integration**: Run `electrobun dev`. Verify sidecar spawns automatically, camera/mic permissions prompt, camera preview in lobby, ticket generation and exchange, audio/video streaming, chat messages, clean disconnect.

4. **Group call test**: Start 3+ instances. Create call on one, join from others. Verify mesh forms and all participants see/hear each other.

5. **Error scenarios**: Kill sidecar mid-call → verify restart and reconnection. Disable network → verify reconnection. Disconnect camera → verify graceful degradation.
