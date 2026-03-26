# Nafaq

P2P encrypted video calling app built with Tauri, Nuxt, and Iroh.

Peer-to-peer connections with no central server. Audio/video encoded via Opus and H.264 in Rust, transported over QUIC using Iroh's relay-assisted NAT traversal.

## Features

- P2P encrypted audio/video calls via Iroh (QUIC)
- Opus audio codec + H.264 video codec (Rust backend)
- Multi-peer mesh networking with automatic peer discovery
- Real-time chat with display names
- Active speaker detection and connection quality monitoring
- Adaptive bitrate/resolution based on network conditions
- Cross-platform: macOS, Windows, Linux, Android

## Stack

| Layer | Technology |
|-------|-----------|
| Frontend | Nuxt 4, Vue 3, Nuxt UI |
| Desktop | Tauri 2 |
| Mobile | Tauri 2 (Android) |
| P2P Transport | Iroh 0.97 (QUIC) |
| Audio Codec | Opus (via `opus` crate) |
| Video Codec | H.264 (via `openh264` crate) |
| Async Runtime | Tokio |

## Development

### Prerequisites

- [Bun](https://bun.sh) (package manager)
- [Rust](https://rustup.rs) (stable toolchain)
- For Android: Java 17, Android SDK + NDK 26.1

### Setup

```bash
bun install
```

### Desktop

```bash
bun run tauri:dev          # development
bun run tauri:build        # production build
```

### Android

```bash
bun run tauri android init       # first-time setup
bun run tauri:android:dev        # development (connected device)
bun run tauri:android:build      # production build
```

## CI/CD

**CI** (`dev` branch, PRs): Lint, typecheck, build desktop + Android (unsigned).

**Release** (`main` branch): Builds signed desktop + Android artifacts and publishes to GitHub Releases.

### GitHub Secrets for Android Signing

| Secret | Description |
|--------|-------------|
| `ANDROID_SIGNING_KEY_STORE_BASE64` | Base64-encoded JKS keystore |
| `ANDROID_SIGNING_KEY_ALIAS` | Key alias (`nafaq-release`) |
| `ANDROID_SIGNING_KEY_PASSWORD` | Key password |
| `ANDROID_SIGNING_STORE_PASSWORD` | Keystore password |

## License

MIT
