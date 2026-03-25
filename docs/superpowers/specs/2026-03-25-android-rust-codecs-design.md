# Android Support + Rust Codec Pipeline

**Date:** 2026-03-25
**Branch:** feat/tauri-migration
**Status:** Approved

## Summary

Make Nafaq usable on Android by:
1. Moving audio/video encoding and decoding from WebCodecs (JS) to Rust (libopus + libvpx)
2. Adding Android Tauri support following the Meeqat reference project pattern

This eliminates the WebCodecs dependency (unavailable on Android WebView) and provides a platform-consistent codec pipeline for desktop and mobile.

## Motivation

Nafaq's media pipeline relies on the WebCodecs API (`AudioEncoder`, `VideoEncoder`, `AudioDecoder`, `VideoDecoder`) for Opus audio and VP8 video. Android's WebView has limited/no WebCodecs support. Moving codecs to Rust makes the pipeline platform-agnostic and provides deterministic latency without JS event loop or GC interference.

## Architecture

### Data Flow

**Send path:**
```
Mic -> AudioWorklet (raw Float32) -> buffer 960 samples -> Int16 PCM -> invoke("send_audio") -> Rust Opus encode -> Iroh
Camera -> Canvas capture 15fps -> getImageData (RGBA) -> invoke("send_video") -> Rust VP8 encode -> Iroh
```

**Receive path:**
```
Iroh -> Rust Opus decode -> raw PCM Int16 -> Tauri event "audio-received" -> JS AudioContext playback
Iroh -> Rust VP8 decode -> RGBA -> JPEG re-encode -> Tauri event "video-received" -> JS createImageBitmap -> Canvas drawImage
```

### IPC Bandwidth

| Stream | Direction | Format | Rate | Bandwidth |
|--------|-----------|--------|------|-----------|
| Audio send | JS → Rust (invoke) | Int16 PCM, 960 samples/frame | ~50 frames/s | ~96 KB/s |
| Audio receive | Rust → JS (event) | Int16 PCM, 960 samples/frame | ~50 frames/s | ~96 KB/s |
| Video send (desktop) | JS → Rust (invoke) | RGBA 640x480 | 15 fps | ~18 MB/s |
| Video send (mobile) | JS → Rust (invoke) | RGBA 320x240 | 15 fps | ~4.5 MB/s |
| Video receive | Rust → JS (event) | JPEG quality 80 | 15 fps | ~0.5 MB/s |

**IPC serialization notes:**
- **JS → Rust (invoke):** Tauri v2 maps `Uint8Array` in JS to `Vec<u8>` in Rust efficiently via the IPC bridge. Raw RGBA frames are transferred as `Uint8Array`, not serialized as JSON number arrays. This is the existing pattern used by `send_audio`/`send_video` today.
- **Rust → JS (event):** Tauri's `emit()` serializes `Vec<u8>` via serde, which produces a JSON array of numbers. A 1.2MB RGBA frame would become ~5MB of JSON at 15fps = ~75MB/s — **not feasible**. To solve this, decoded video is re-encoded as JPEG in Rust (~30-50KB/frame) before emitting to JS. Audio PCM at ~1.9KB/frame serialized as JSON numbers is ~10KB/frame — acceptable at 50 frames/s (~500KB/s).

## Component Design

### 1. Rust Codec Module (`src-tauri/src/codec.rs`)

**AudioCodec:**
- Wraps `opus::Encoder` (48kHz, mono, 32kbps) and `opus::Decoder`
- `encode_audio(raw_pcm: &[i16]) -> Vec<u8>` — takes exactly 960 samples (20ms), returns Opus packet
- `decode_audio(opus_data: &[u8]) -> Vec<i16>` — returns 960 PCM samples
- Frame size: 960 samples (20ms at 48kHz, standard Opus frame)

**VideoCodec:**
- Wraps `libvpx-sys` encoder and decoder contexts (safe wrapper with `unsafe` blocks isolated to encode/decode methods)
- `encode_video(rgba: &[u8], width: u32, height: u32, keyframe: bool) -> Vec<u8>` — RGBA→YUV420 conversion internally, returns VP8 packet
- `decode_video(vp8_data: &[u8]) -> (Vec<u8>, u32, u32)` — decodes VP8 packet, extracts dimensions from the VP8 bitstream header, converts YUV420→RGBA, returns RGBA pixels + width + height. Dimensions are intrinsic to the VP8 bitstream — no out-of-band signaling needed.
- `decoded_to_jpeg(rgba: &[u8], width: u32, height: u32) -> Vec<u8>` — re-encodes decoded RGBA as JPEG quality 80 for efficient IPC to JS
- Config: 500kbps bitrate, 15fps, realtime deadline, keyframe every 30 frames (2s)

**RGBA↔YUV420 conversion:** Pure Rust inline functions, no external dependency.

**CodecState:** Added to `AppState`, holds `Arc<Mutex<Option<AudioCodec>>>` and `Arc<Mutex<Option<VideoCodec>>>`. Initialized on `init_codecs`, dropped on `destroy_codecs`.

**Lifecycle safety:** When codecs are `None` (before `init_codecs` or after `destroy_codecs`), all encode/decode operations silently drop frames. The `Arc<Mutex<Option<...>>>` pattern handles this — callers check for `Some` before processing. Media arriving before codec init is dropped; this is expected since the remote peer may start sending before the local codec is ready.

**Resolution changes:** If the user rotates their phone or the camera resolution changes mid-call, the JS side sends the new `width`/`height` with each `send_video` invoke. The Rust VP8 encoder is reinitialized when incoming dimensions differ from the current config. This adds a one-frame delay on resolution change.

### 2. Tauri Commands (Modified)

**Changed:**
- `send_audio(peer_id, data)` — `data` is raw PCM Int16 LE bytes (exactly 960 samples = 1920 bytes per call). Encodes via `CodecState.audio` before sending over Iroh. If codecs are `None`, returns `Ok(())` (no-op).
- `send_video(peer_id, data, width, height, keyframe)` — `data` is raw RGBA bytes. Encodes via `CodecState.video` before sending over Iroh. If codecs are `None`, returns `Ok(())` (no-op).

**New:**
- `init_codecs(width, height)` — initializes Opus encoder/decoder and VP8 encoder/decoder with given dimensions. Called when a call starts.
- `destroy_codecs()` — drops codec state, frees memory. Called when a call ends.

### 3. Media Receive Path (Modified `lib.rs`)

The media forwarder currently emits encoded bytes to JS. Changes to:
- Decode received audio (Opus → PCM Int16) before emitting `audio-received`
- Decode received video (VP8 → RGBA → JPEG) before emitting `video-received`

`MediaEvent` struct changes for video:
```rust
struct MediaEvent {
    stream_type: u8,
    data: Vec<u8>,     // PCM Int16 for audio, JPEG bytes for video
    timestamp: u64,
    width: Option<u32>,  // Only set for video
    height: Option<u32>, // Only set for video
}
```

If codecs are `None` when media arrives, frames are silently dropped.

### 4. Media Stream Framing (Modified `connection.rs`)

**Current issue:** `send_audio`/`send_video` write raw bytes to unidirectional QUIC streams via `send.write_all(data)` without length framing. The receiver reads into a 64KB buffer via `recv.read()`, which may return partial or concatenated packets. This works by accident with the current small encoded packets but will be fragile with Rust-encoded data.

**Fix:** Use the existing `write_framed`/`read_framed` functions (already in `messages.rs`) for media streams too. Each encoded Opus/VP8 packet is length-prefixed (4-byte big-endian length + payload). The receiver reads complete framed packets.

Changes:
- `ConnectionManager::send_on_stream()` → use `write_framed()` instead of raw `write_all()`
- `ConnectionManager::handle_uni_stream()` → use `read_framed()` loop instead of raw `recv.read()`

This is a wire format change from the current implementation, but there are no deployed users.

### 5. Frontend (`useMediaTransport.ts`)

**Removed:** All WebCodecs code — `AudioEncoder`, `AudioDecoder`, `VideoEncoder`, `VideoDecoder`, Opus config probe, `useRawPcm` flag.

**Send audio:** AudioWorklet captures Float32 128-sample chunks. Buffer chunks in JS until 960 samples accumulated (approximately 7-8 worklet callbacks = 20ms). Convert buffered Float32 → Int16, invoke `send_audio` with exactly 1920 bytes. This ensures each invoke corresponds to one Opus frame.

**Send video:** Canvas capture at 15fps → `ctx.getImageData()` for raw RGBA → `invoke("send_video", { peerId, data: rgba_uint8array, width, height, keyframe: frameCount % 30 === 0 })`. Resolution capped at 320x240 on mobile, 640x480 on desktop.

**Receive audio:** `audio-received` event delivers raw PCM Int16 bytes → convert Int16→Float32 → `AudioBuffer` → schedule via `AudioContext`. Existing code, just becomes the only path.

**Receive video:** `video-received` event delivers JPEG bytes + width + height → `createImageBitmap(new Blob([data], { type: 'image/jpeg' }))` → `ctx.drawImage(bitmap, 0, 0)`. Replaces the `VideoDecoder` path.

**Lifecycle:**
- `startSending()` calls `invoke("init_codecs", { width, height })` before starting capture
- `stop()` calls `invoke("destroy_codecs")` to clean up Rust state

### 6. Android Tauri Setup

**Generated structure** (via `tauri android init`):
- `src-tauri/gen/android/` — Gradle project, `MainActivity.kt` extending `TauriActivity`
- `src-tauri/tauri.android.conf.json` — override that removes desktop window dimensions, resizable, and title fields; single fullscreen window
- `src-tauri/capabilities/mobile.json` — Android capability file

**AndroidManifest.xml permissions:**
```xml
<uses-permission android:name="android.permission.INTERNET" />
<uses-permission android:name="android.permission.CAMERA" />
<uses-permission android:name="android.permission.RECORD_AUDIO" />
<uses-permission android:name="android.permission.MODIFY_AUDIO_SETTINGS" />
```

**Android runtime permissions (API 23+):** `getUserMedia()` in the WebView triggers Android's runtime permission dialog for camera and microphone. Tauri's Android WebView should handle this via its `WebChromeClient` implementation, which delegates `onPermissionRequest` to the system dialog. Verify during Android validation that permissions are correctly prompted.

**Gradle config:**
- `minSdk: 24` (Android 7.0)
- `targetSdk: 36`
- ABI filter: `arm64-v8a`

**package.json scripts:**
```json
"tauri:android:dev": "tauri android dev",
"tauri:android:build": "tauri android build"
```

**Cargo.toml changes:**
- Add `opus` and `libvpx-sys` (or safe `vpx` wrapper if available) with vendored source builds
- Add `image` crate (for JPEG encoding of decoded video frames)
- Move `tauri-plugin-shell` to desktop-only: `[target.'cfg(not(any(target_os = "android", target_os = "ios")))'.dependencies]`

**lib.rs changes:**
- Wrap `tauri_plugin_shell::init()` with `#[cfg(desktop)]` conditional compilation, matching the Meeqat pattern for platform-specific plugins:
```rust
let builder = tauri::Builder::default().manage(app_state);
#[cfg(desktop)]
let builder = builder.plugin(tauri_plugin_shell::init());
```

## Cross-Compilation

**Step 0 (before writing pipeline code):** Add codec dependencies to Cargo.toml and run `cargo build --target aarch64-linux-android` to validate cross-compilation. This is the first implementation step, not an afterthought.

**opus crate:** Builds libopus from vendored C source via `cc` crate. Respects NDK toolchain env vars (`CC`, `AR`, `CFLAGS`) set by Tauri's Android build. Expected to work without issues.

**libvpx-sys:** Builds libvpx from source using `configure + make`. Needs to target `arm64-android-gcc`. This is the riskiest dependency — NDK r25+ dropped standalone toolchains in favor of the unified toolchain, which can break libvpx's configure script. May need `LIBVPX_NO_PKG_CONFIG=1` env var. If the build script needs patching for modern NDK, evaluate effort before proceeding.

**Fallback:** If `libvpx-sys` fails to cross-compile, switch to `openh264` crate (H.264 instead of VP8). Builds via `cc`, known to work on Android. Wire format unchanged — encoded bytes in MediaFrame payload. Both peers must use the same codec, but we control both sides.

**iroh crate (v0.97):** Built on `quinn` (QUIC implementation), which uses standard Rust networking primitives. Should work on Android's network stack. Validated implicitly when the app builds and connects — if Iroh doesn't work on Android, P2P connectivity fails visibly.

**image crate:** Pure Rust, no C dependencies. No cross-compilation concerns.

## Wire Compatibility

- Encoded bytes are produced by the same underlying C libraries (libopus, libvpx) as WebCodecs — bitstreams are identical
- `MediaFrame` binary wire format unchanged (41-byte header + encoded payload)
- QUIC stream types unchanged (0x01 audio, 0x02 video)
- Media streams gain length-prefix framing (Section 4) — this is a wire change from the current raw-bytes approach, acceptable since there are no deployed users
- No protocol version bump needed
- Old WebCodecs-based builds are not wire-compatible, but there are no deployed users
- `send_audio` and `send_video` invoke signatures change — frontend and Rust must be updated together
- VP8 keyframe detection on receive side (`bytes[0] & 0x01 === 0`) still works — property of the VP8 bitstream
- Video dimensions are extracted from the VP8 bitstream header during decode — not transmitted out-of-band

## Testing

**Unit tests (Rust):**
- Audio roundtrip: encode 960-sample PCM → decode → compare (lossy tolerance for Opus)
- Video roundtrip: encode RGBA frame → decode → verify dimensions match + non-zero output
- Keyframe flag: encode with keyframe=true, verify VP8 bitstream has keyframe marker
- JPEG encode: decode VP8 → JPEG, verify valid JPEG header + reasonable size
- Frame alignment: verify encoder rejects non-960-sample audio input

**Integration (desktop):**
1. Build `tauri:dev`, open two instances
2. Create call, join with ticket
3. Verify audio/video flow with Rust codecs
4. Verify latency within 200ms target

**Android validation:**
1. `tauri android init` + build succeeds
2. Deploy to emulator via `adb`
3. App launches, UI renders correctly
4. Camera/microphone runtime permissions prompted and granted
5. Cross-test: desktop creates call, Android joins (or vice versa)
6. Audio and video flow bidirectionally
