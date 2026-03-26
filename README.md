<p align="center">
  <img src="src-tauri/icons/128x128@2x.png" width="128" height="128" alt="Nafaq">
</p>

<h1 align="center">Nafaq</h1>

<p align="center">
  Peer-to-peer encrypted video calls — no servers, no middlemen.
</p>

<p align="center">
  <a href="https://github.com/KazBrekker1/nafaq/actions/workflows/ci.yml"><img src="https://github.com/KazBrekker1/nafaq/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="https://github.com/KazBrekker1/nafaq/releases/latest"><img src="https://img.shields.io/github/v/release/KazBrekker1/nafaq?color=8B5CF6&label=release" alt="Release"></a>
  <img src="https://img.shields.io/badge/platforms-macOS%20%7C%20Windows%20%7C%20Linux%20%7C%20Android-8B5CF6" alt="Platforms">
</p>

---

Nafaq connects you directly to whoever you're calling. Audio and video are encoded in Rust (Opus + H.264), transported over QUIC, and routed peer-to-peer using [Iroh](https://iroh.computer)'s relay-assisted NAT traversal. No data ever touches a central server.

## Features

- **Direct P2P calls** — encrypted end-to-end, relayed only for NAT traversal
- **Multi-peer mesh** — automatic peer discovery, call with multiple people
- **Real-time chat** — with display names and unread indicators
- **Active speaker detection** — highlights who's talking
- **Adaptive quality** — bitrate and resolution adjust to your connection
- **Cross-platform** — macOS, Windows, Linux, Android

## How It Works

```
You ──[Opus/H.264]──> QUIC (Iroh) ──> Peer
         encode          transport       decode + play
```

Audio is captured via Web Audio worklets, video via canvas frame capture. Both are sent to the Rust backend for encoding (Opus for audio, H.264 for video), then transmitted as QUIC datagrams (audio) and streams (video) directly to connected peers. The receiving side decodes in Rust and pushes frames back to the frontend for playback.

## Stack

| | |
|---|---|
| **Frontend** | Nuxt 4 · Vue 3 · Nuxt UI |
| **Desktop** | Tauri 2 |
| **Mobile** | Tauri 2 (Android) |
| **Transport** | Iroh 0.97 (QUIC) |
| **Audio** | Opus |
| **Video** | H.264 (OpenH264) |

## Getting Started

### Prerequisites

- [Bun](https://bun.sh)
- [Rust](https://rustup.rs) (stable)
- For Android: Java 17, Android SDK + NDK 26.1

### Install & Run

```bash
bun install

# Desktop
bun run tauri:dev

# Android (with device connected)
bun run tauri:android:dev
```

### Build for Production

```bash
# Desktop
bun run tauri:build

# Android
bun run tauri:android:build
```

### Version Bump

```bash
bun run bump
```

Updates version across `package.json`, `tauri.conf.json`, and `Cargo.toml`.

## Downloads

Grab the latest build from [GitHub Releases](https://github.com/KazBrekker1/nafaq/releases/latest).

| Platform | Format |
|----------|--------|
| macOS (Apple Silicon) | `.dmg` |
| macOS (Intel) | `.dmg` |
| Windows | `.msi` |
| Linux | `.deb` / `.AppImage` |
| Android | `.apk` |
