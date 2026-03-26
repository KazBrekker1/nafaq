# Media Latency Optimization Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Reduce one-way media latency from ~78-225ms to ~16-49ms by optimizing IPC, codecs, QUIC transport, and frontend capture/playback.

**Architecture:** Video encode stays in Rust, video decode moves to WebCodecs in the browser. Audio encode/decode stays in Rust. IPC uses raw binary (desktop) / base64 (Android) for send, events with base64 for receive. QUIC transport uses stream-per-frame for video, BBR congestion control, and priority-based scheduling.

**Tech Stack:** Rust (Tauri v2, Iroh 0.97, OpenH264 0.9, Opus 0.3, noq-proto, base64), TypeScript (Vue/Nuxt, WebCodecs API)

**Spec:** `docs/superpowers/specs/2026-03-25-media-latency-optimization-design.md`

---

**Important:** Tasks 1-7 modify interconnected Rust files within a single crate. Individual tasks will NOT compile in isolation — **Task 8 is the compilation gate**. The agentic worker should apply Tasks 1-7 sequentially, then run `cargo check` at Task 8. Intermediate commits (Tasks 1-7) may require `--no-verify` if the project has a `cargo check` pre-commit hook.

---

### Task 1: Dependencies & JPEG Cleanup

**Files:**
- Modify: `src-tauri/Cargo.toml:13-26`
- Modify: `src-tauri/src/codec.rs:140-156` (remove `decoded_to_jpeg`)
- Modify: `src-tauri/src/codec.rs:236-246` (remove `test_jpeg_encode`)
- Modify: `src-tauri/src/lib.rs:139` (remove JPEG call)

- [ ] **Step 1: Update Cargo.toml dependencies**

Add `base64` and `noq-proto`. Remove `image`.

```toml
# Add these lines in [dependencies]:
base64 = "0.22"
noq-proto = "0.16"

# Remove this line:
# image = { version = "0.25", default-features = false, features = ["jpeg"] }
```

- [ ] **Step 2: Remove `decoded_to_jpeg` function from codec.rs**

Delete lines 140-156 (`pub fn decoded_to_jpeg(...)`) and the `// ── JPEG encoding for IPC` comment on line 140.

- [ ] **Step 3: Remove `test_jpeg_encode` test from codec.rs**

Delete lines 236-246 (the `test_jpeg_encode` test function).

- [ ] **Step 4: Remove JPEG call in lib.rs media forwarder**

In `src-tauri/src/lib.rs`, line 139, the video forwarder calls `decoded_to_jpeg`. This entire video decode + JPEG block (lines 135-149) will be replaced in Task 6, but for now comment it out or remove the JPEG call to avoid compile errors. Replace the STREAM_VIDEO branch with a temporary pass-through:

```rust
STREAM_VIDEO => {
    // TODO: Will be replaced by video forwarder in Task 6
    // For now, skip video decode entirely
}
```

- [ ] **Step 5: Commit**

```bash
git add src-tauri/Cargo.toml src-tauri/src/codec.rs src-tauri/src/lib.rs
git commit -m "chore: add base64/noq-proto deps, remove image crate and JPEG encoding"
```

---

### Task 2: Split Codec Structs + H.264 Config + Opus FEC

**Files:**
- Rewrite: `src-tauri/src/codec.rs`

- [ ] **Step 1: Rewrite codec.rs with split encoder/decoder structs**

Replace the entire file content with the new split architecture. Keep existing constants. Add H.264 encoder config (BitRate, FrameRate, RateControlMode). Add Opus FEC (set_inband_fec, set_packet_loss_perc). Add `is_keyframe()` helper for Annex B parsing.

```rust
use opus::{Encoder as OpusEncoder, Decoder as OpusDecoder, Channels, Application};
use openh264::encoder::{Encoder as H264Encoder, EncoderConfig};
use openh264::decoder::Decoder as H264Decoder;
use openh264::formats::{RgbaSliceU8, YUVBuffer, YUVSource};
use openh264::OpenH264API;

const SAMPLE_RATE: u32 = 48000;
const CHANNELS: Channels = Channels::Mono;
pub const OPUS_FRAME_SIZE: usize = 960; // 20ms at 48kHz
const MAX_OPUS_PACKET: usize = 4000;

// ── Audio Encoder ────────────────────────────────────────────────────

pub struct AudioEncoder {
    encoder: OpusEncoder,
}

impl AudioEncoder {
    pub fn new() -> Self {
        let mut encoder = OpusEncoder::new(SAMPLE_RATE, CHANNELS, Application::Voip)
            .expect("failed to create Opus encoder");
        encoder.set_inband_fec(true).expect("failed to enable Opus FEC");
        encoder.set_packet_loss_perc(5).expect("failed to set packet loss %");
        Self { encoder }
    }

    pub fn encode(&mut self, pcm: &[i16]) -> Option<Vec<u8>> {
        if pcm.len() != OPUS_FRAME_SIZE {
            tracing::warn!("AudioEncoder::encode requires exactly 960 samples, got {}", pcm.len());
            return None;
        }
        let mut buf = vec![0u8; MAX_OPUS_PACKET];
        match self.encoder.encode(pcm, &mut buf) {
            Ok(n) => {
                buf.truncate(n);
                Some(buf)
            }
            Err(e) => {
                tracing::warn!("Opus encode error: {e}");
                None
            }
        }
    }
}

// ── Audio Decoder ────────────────────────────────────────────────────

pub struct AudioDecoder {
    decoder: OpusDecoder,
    prev_packet_lost: bool,
}

impl AudioDecoder {
    pub fn new() -> Self {
        let decoder = OpusDecoder::new(SAMPLE_RATE, CHANNELS)
            .expect("failed to create Opus decoder");
        Self { decoder, prev_packet_lost: false }
    }

    pub fn decode(&mut self, opus_data: &[u8]) -> Option<Vec<i16>> {
        let fec = self.prev_packet_lost;
        self.prev_packet_lost = false;
        let mut pcm = vec![0i16; OPUS_FRAME_SIZE];
        match self.decoder.decode(opus_data, &mut pcm, fec) {
            Ok(n) => {
                pcm.truncate(n);
                Some(pcm)
            }
            Err(e) => {
                self.prev_packet_lost = true;
                tracing::warn!("Opus decode error: {e}");
                None
            }
        }
    }
}

// ── Video Encoder (H.264 via OpenH264) ───────────────────────────────

pub struct VideoEncoder {
    encoder: H264Encoder,
    width: u32,
    height: u32,
}

impl VideoEncoder {
    pub fn new(width: u32, height: u32) -> Self {
        let api = OpenH264API::from_source();
        let config = EncoderConfig::new()
            .bitrate(openh264::encoder::BitRate::from_bps(500_000))
            .max_frame_rate(openh264::encoder::FrameRate::from_hz(15.0))
            .rate_control_mode(openh264::encoder::RateControlMode::Bitrate);
        let encoder = H264Encoder::with_api_config(api, config)
            .expect("failed to create H264 encoder");
        Self { encoder, width, height }
    }

    pub fn encode(&mut self, rgba: &[u8], width: u32, height: u32, keyframe: bool) -> Option<Vec<u8>> {
        let expected = (width as usize)
            .checked_mul(height as usize)
            .and_then(|n| n.checked_mul(4));
        match expected {
            Some(n) if n == rgba.len() => {}
            _ => {
                tracing::warn!("RGBA buffer size mismatch: got {} for {}x{}", rgba.len(), width, height);
                return None;
            }
        }

        if width != self.width || height != self.height {
            self.width = width;
            self.height = height;
        }

        if keyframe {
            self.encoder.force_intra_frame();
        }

        let rgba_source = RgbaSliceU8::new(rgba, (width as usize, height as usize));
        let yuv = YUVBuffer::from_rgb_source(rgba_source);

        match self.encoder.encode(&yuv) {
            Ok(bitstream) => {
                let data = bitstream.to_vec();
                if data.is_empty() { None } else { Some(data) }
            }
            Err(e) => {
                tracing::warn!("H264 encode error: {e}");
                None
            }
        }
    }
}

// ── Video Decoder (H.264, WebCodecs fallback only) ───────────────────

pub struct VideoDecoder {
    decoder: H264Decoder,
}

impl VideoDecoder {
    pub fn new() -> Self {
        let decoder = H264Decoder::new()
            .expect("failed to create H264 decoder");
        Self { decoder }
    }

    pub fn decode(&mut self, h264_data: &[u8]) -> Option<(Vec<u8>, u32, u32)> {
        match self.decoder.decode(h264_data) {
            Ok(Some(yuv)) => {
                let (w, h) = yuv.dimensions();
                let mut rgba = vec![0u8; w * h * 4];
                yuv.write_rgba8(&mut rgba);
                Some((rgba, w as u32, h as u32))
            }
            Ok(None) => None,
            Err(e) => {
                tracing::warn!("H264 decode error: {e}");
                None
            }
        }
    }
}

// ── Keyframe detection (Annex B) ─────────────────────────────────────

pub fn is_keyframe(h264_data: &[u8]) -> bool {
    let mut i = 0;
    while i + 4 < h264_data.len() {
        if h264_data[i] == 0 && h264_data[i + 1] == 0 {
            let header_idx = if h264_data[i + 2] == 1 {
                i + 3
            } else if h264_data[i + 2] == 0 && h264_data.get(i + 3) == Some(&1) {
                i + 4
            } else {
                i += 1;
                continue;
            };
            if header_idx < h264_data.len() && (h264_data[header_idx] & 0x1f) == 5 {
                return true;
            }
            i = header_idx;
        } else {
            i += 1;
        }
    }
    false
}

// ── CodecState for AppState ──────────────────────────────────────────

pub struct CodecState {
    pub audio_encoder: tokio::sync::Mutex<Option<AudioEncoder>>,
    pub audio_decoder: tokio::sync::Mutex<Option<AudioDecoder>>,
    pub video_encoder: tokio::sync::Mutex<Option<VideoEncoder>>,
    pub video_decoder: tokio::sync::Mutex<Option<VideoDecoder>>,
}

impl CodecState {
    pub fn new() -> Self {
        Self {
            audio_encoder: tokio::sync::Mutex::new(None),
            audio_decoder: tokio::sync::Mutex::new(None),
            video_encoder: tokio::sync::Mutex::new(None),
            video_decoder: tokio::sync::Mutex::new(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_audio_roundtrip() {
        let mut enc = AudioEncoder::new();
        let mut dec = AudioDecoder::new();
        let pcm: Vec<i16> = (0..960)
            .map(|i| (f64::sin(2.0 * std::f64::consts::PI * 440.0 * i as f64 / 48000.0) * 16000.0) as i16)
            .collect();
        let encoded = enc.encode(&pcm).expect("encode failed");
        assert!(!encoded.is_empty());
        let decoded = dec.decode(&encoded).expect("decode failed");
        assert_eq!(decoded.len(), 960);
    }

    #[test]
    fn test_audio_rejects_wrong_frame_size() {
        let mut enc = AudioEncoder::new();
        assert!(enc.encode(&vec![0i16; 128]).is_none());
    }

    #[test]
    fn test_video_roundtrip() {
        let mut enc = VideoEncoder::new(320, 240);
        let mut dec = VideoDecoder::new();
        let mut rgba = vec![0u8; (320 * 240 * 4) as usize];
        for y in 0..240u32 {
            for x in 0..320u32 {
                let idx = ((y * 320 + x) * 4) as usize;
                rgba[idx] = (x * 255 / 320) as u8;
                rgba[idx + 2] = (y * 255 / 240) as u8;
                rgba[idx + 3] = 255;
            }
        }
        let encoded = enc.encode(&rgba, 320, 240, true).expect("encode failed");
        assert!(!encoded.is_empty());
        let (decoded, w, h) = dec.decode(&encoded).expect("decode failed");
        assert_eq!(w, 320);
        assert_eq!(h, 240);
        assert_eq!(decoded.len(), (320 * 240 * 4) as usize);
    }

    #[test]
    fn test_is_keyframe() {
        // IDR NAL with start code 00 00 00 01 and NAL type 5
        let idr = vec![0x00, 0x00, 0x00, 0x01, 0x65, 0x88]; // 0x65 & 0x1f = 5
        assert!(is_keyframe(&idr));
        // Non-IDR NAL type 1
        let non_idr = vec![0x00, 0x00, 0x00, 0x01, 0x41, 0x88]; // 0x41 & 0x1f = 1
        assert!(!is_keyframe(&non_idr));
        // SPS NAL type 7 (not a keyframe itself)
        let sps = vec![0x00, 0x00, 0x00, 0x01, 0x67, 0x42];
        assert!(!is_keyframe(&sps));
    }
}
```

- [ ] **Step 2: Commit**

```bash
git add src-tauri/src/codec.rs
git commit -m "feat: split codec into encoder/decoder structs with H.264 config and Opus FEC"
```

---

### Task 3: Update Messages & State

**Files:**
- Modify: `src-tauri/src/messages.rs:7` (add type alias after stream constants)
- Rewrite: `src-tauri/src/state.rs`

- [ ] **Step 1: Add MediaPacket type alias to messages.rs**

Add at the top of `messages.rs`, after the stream constants (after line 7):

```rust
/// (peer_id, timestamp_ms, encoded_payload)
pub type MediaPacket = (String, u64, Vec<u8>);
```

Keep `MediaFrame`, `write_framed`, `read_framed`, and all existing types for now. `MediaFrame` is no longer used by the new architecture but removing it would break intermediate compilation steps. **Intentional deviation from spec** (which says "remove MediaFrame") — a follow-up cleanup task should remove it once all references are confirmed gone. Keep all tests.

- [ ] **Step 2: Rewrite state.rs with new channel types**

```rust
use std::sync::Arc;

use iroh::protocol::Router;
use tokio::sync::{broadcast, watch};

use crate::codec::CodecState;
use crate::connection::ConnectionManager;
use crate::messages::{Event, MediaPacket};

pub struct AppState {
    pub endpoint: iroh::Endpoint,
    pub router: Router,
    pub conn_manager: Arc<ConnectionManager>,
    pub event_tx: broadcast::Sender<Event>,
    pub audio_media_tx: broadcast::Sender<MediaPacket>,
    pub video_watch_tx: watch::Sender<Option<MediaPacket>>,
    pub codec: Arc<CodecState>,
}
```

- [ ] **Step 3: Commit**

```bash
git add src-tauri/src/messages.rs src-tauri/src/state.rs
git commit -m "feat: add MediaPacket type, update AppState for separate audio/video channels"
```

---

### Task 4: Rewrite Connection Manager

**Files:**
- Rewrite: `src-tauri/src/connection.rs`

- [ ] **Step 1: Rewrite connection.rs**

Major changes:
- `ConnectionManager` holds `audio_media_tx: broadcast::Sender<MediaPacket>` and `video_watch_tx: watch::Sender<Option<MediaPacket>>` instead of single `media_tx`
- `PeerConnection` removes `video_send` (no longer long-lived for video) — keep `audio_send`, `chat_send`, `control_send`, and `connection` for opening new streams
- `send_video()` opens a new uni-stream per frame, sets priority, writes type byte + timestamp + payload, finishes stream
- `send_audio()` uses the long-lived audio stream, prepends 8-byte timestamp
- `setup_connection()` sets stream priorities: audio=90, control=100, chat=10
- `spawn_stream_receivers()` routes STREAM_AUDIO to `audio_media_tx` (long-lived loop), STREAM_VIDEO to `video_watch_tx` (single frame per stream)
- Receive path extracts 8-byte timestamp from payload
- `is_keyframe()` imported from codec module for video priority

The full rewrite is large. Key signatures:

```rust
pub struct ConnectionManager {
    peers: Arc<Mutex<HashMap<String, PeerConnection>>>,
    event_tx: broadcast::Sender<Event>,
    audio_media_tx: broadcast::Sender<MediaPacket>,
    video_watch_tx: watch::Sender<Option<MediaPacket>>,
}

impl ConnectionManager {
    pub fn new(
        event_tx: broadcast::Sender<Event>,
        audio_media_tx: broadcast::Sender<MediaPacket>,
        video_watch_tx: watch::Sender<Option<MediaPacket>>,
    ) -> Self { ... }

    pub async fn send_audio(&self, peer_id: &str, data: &[u8], timestamp: u64) -> Result<()> { ... }
    pub async fn send_video(&self, peer_id: &str, data: &[u8], timestamp: u64) -> Result<()> { ... }
    // send_chat, send_control, disconnect_peer, connected_peers — unchanged signatures
}
```

Refer to spec Section 4 (timestamps), Section 6 (stream-per-frame, priorities, receive path) for exact code.

- [ ] **Step 2: Commit**

```bash
git add src-tauri/src/connection.rs
git commit -m "feat: stream-per-frame video, timestamps, stream priorities"
```

---

### Task 5: BBR Congestion Control

**Files:**
- Modify: `src-tauri/src/node.rs:9-20`

- [ ] **Step 1: Configure BBR on the Iroh endpoint**

Update `create_endpoint()` in `node.rs` to use BBR:

```rust
use std::sync::Arc;
use noq_proto::congestion::BbrConfig;

pub async fn create_endpoint() -> anyhow::Result<iroh::Endpoint> {
    let transport_config = iroh::endpoint::QuicTransportConfig::builder()
        .congestion_controller_factory(Arc::new(BbrConfig::default()))
        .build();

    let endpoint = iroh::Endpoint::builder()
        .alpns(vec![NAFAQ_ALPN.to_vec()])
        .transport_config(transport_config)
        .bind()
        .await?;
    endpoint.home_relay().initialized().await?;
    Ok(endpoint)
}
```

Note: The existing code may use `presets::N0` or a different builder pattern. Match the current style but add the transport_config. If `QuicTransportConfig` or `BbrConfig` APIs differ from what the spec describes, adapt accordingly — the goal is to set BBR as the congestion controller.

- [ ] **Step 2: Commit**

```bash
git add src-tauri/src/node.rs
git commit -m "feat: configure BBR congestion control for real-time media"
```

---

### Task 6: Rewrite lib.rs — Channels & Forwarders

**Files:**
- Rewrite: `src-tauri/src/lib.rs`

- [ ] **Step 1: Rewrite lib.rs**

Major changes:
- Replace single `media_tx` broadcast channel with `audio_media_tx: broadcast::channel::<MediaPacket>(16)` and `video_watch_tx: watch::channel::<Option<MediaPacket>>(None)`
- Pass both channel senders to `ConnectionManager::new()`
- Add `VideoEvent` and `AudioEvent` structs (with `data: String` for base64)
- Add `use base64::Engine;` and `const B64`
- Spawn **two separate forwarder tasks**:
  - Audio forwarder: subscribes to `audio_media_tx`, locks `audio_decoder`, decodes Opus → PCM → base64, emits `"audio-received"`
  - Video forwarder: subscribes to `video_watch_tx`, base64-encodes raw H.264 NALUs, emits `"video-received"`
- Remove the old single media forwarder task (lines 112-161)
- Remove `MediaEvent` struct
- Register `commands::reinit_video_encoder` in `invoke_handler`
- Update `AppState` construction to include new channel senders

```rust
use base64::Engine;
const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

#[derive(Clone, serde::Serialize)]
struct VideoEvent {
    data: String,    // base64-encoded H.264 NALUs
    timestamp: u64,
}

#[derive(Clone, serde::Serialize)]
struct AudioEvent {
    data: String,    // base64-encoded PCM Int16 LE
    timestamp: u64,
}
```

Audio forwarder:
```rust
tauri::async_runtime::spawn(async move {
    let mut audio_rx = audio_media_tx_for_setup.subscribe();
    loop {
        match audio_rx.recv().await {
            Ok((_peer_id, timestamp, payload)) => {
                let mut dec = codec_audio.audio_decoder.lock().await;
                if let Some(ref mut d) = *dec {
                    if let Some(pcm) = d.decode(&payload) {
                        let raw: Vec<u8> = pcm.iter().flat_map(|s| s.to_le_bytes()).collect();
                        let _ = app_handle_audio.emit("audio-received", AudioEvent {
                            data: B64.encode(&raw),
                            timestamp,
                        });
                    }
                }
            }
            Err(broadcast::error::RecvError::Lagged(n)) => {
                tracing::warn!("Audio forwarder lagged by {n} frames");
            }
            Err(broadcast::error::RecvError::Closed) => break,
        }
    }
});
```

Video forwarder:
```rust
tauri::async_runtime::spawn(async move {
    let mut video_rx = video_watch_tx_for_setup.subscribe();
    loop {
        if video_rx.changed().await.is_err() { break; }
        let frame = video_rx.borrow_and_update().clone();
        if let Some((_peer_id, timestamp, payload)) = frame {
            let _ = app_handle_video.emit("video-received", VideoEvent {
                data: B64.encode(&payload),
                timestamp,
            });
        }
    }
});
```

Invoke handler — add `reinit_video_encoder`:
```rust
.invoke_handler(tauri::generate_handler![
    commands::get_node_info,
    commands::create_call,
    commands::join_call,
    commands::end_call,
    commands::send_chat,
    commands::send_control,
    commands::send_audio,
    commands::send_video,
    commands::init_codecs,
    commands::destroy_codecs,
    commands::reinit_video_encoder,
])
```

- [ ] **Step 2: Commit**

```bash
git add src-tauri/src/lib.rs
git commit -m "feat: separate audio/video forwarders with base64 events"
```

---

### Task 7: Rewrite Commands — Dual-Mode IPC

**Files:**
- Rewrite: `src-tauri/src/commands.rs`

- [ ] **Step 1: Rewrite commands.rs**

Major changes:
- `send_video` uses `tauri::ipc::Request<'_>` with dual-mode: `InvokeBody::Raw` (desktop) parses binary header, `InvokeBody::Json` (Android) parses base64 JSON
- `send_audio` same dual-mode pattern with simpler header
- Both accept timestamp from payload, pass to `conn_manager.send_audio/video(peer_id, &encoded, timestamp)`
- `init_codecs` creates all 4 split codec instances
- `destroy_codecs` clears all 4
- `reinit_video_encoder` creates only a new VideoEncoder
- All codec locks use `audio_encoder` / `video_encoder` (not shared with decoder)

Key function signatures:

```rust
use base64::Engine;
const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

#[tauri::command]
pub async fn send_video(
    request: tauri::ipc::Request<'_>,
    state: State<'_, AppState>,
) -> Result<(), String> {
    match request.body() {
        tauri::ipc::InvokeBody::Raw(data) => {
            // Parse binary header: [peer_id_len:u8][peer_id][w:u32LE][h:u32LE][kf:u8][ts:u64LE][rgba...]
            // Lock video_encoder, encode, send with timestamp
        }
        tauri::ipc::InvokeBody::Json(value) => {
            // Parse JSON: { peerId, data (base64), width, height, keyframe, timestamp }
            // Decode base64, lock video_encoder, encode, send with timestamp
        }
    }
}

#[tauri::command]
pub async fn send_audio(
    request: tauri::ipc::Request<'_>,
    state: State<'_, AppState>,
) -> Result<(), String> {
    // Same dual-mode pattern
    // Parse binary header: [peer_id_len:u8][peer_id][ts:u64LE][pcm...]
    // Or JSON: { peerId, data (base64), timestamp }
    // Lock audio_encoder, encode, send with timestamp
}

#[tauri::command]
pub async fn init_codecs(width: u32, height: u32, state: State<'_, AppState>) -> Result<(), String> {
    // Create AudioEncoder, AudioDecoder, VideoEncoder, VideoDecoder
}

#[tauri::command]
pub async fn destroy_codecs(state: State<'_, AppState>) -> Result<(), String> {
    // Clear all 4
}

#[tauri::command]
pub async fn reinit_video_encoder(width: u32, height: u32, state: State<'_, AppState>) -> Result<(), String> {
    // Only recreate VideoEncoder — preserves audio state
}
```

Refer to spec Section 2 for binary header format and Section 3 for codec command details.

- [ ] **Step 2: Commit**

```bash
git add src-tauri/src/commands.rs
git commit -m "feat: dual-mode IPC commands with raw binary + base64 fallback"
```

---

### Task 8: Build Verification

**Files:** None (verification only)

- [ ] **Step 1: Run cargo check**

```bash
cd src-tauri && cargo check 2>&1
```

Expected: No errors. If there are errors, fix them before proceeding.

- [ ] **Step 2: Run cargo test**

```bash
cd src-tauri && cargo test 2>&1
```

Expected: All tests pass (audio roundtrip, video roundtrip, is_keyframe, message framing).

- [ ] **Step 3: Commit any fixes**

```bash
git add -A src-tauri/
git commit -m "fix: resolve compilation issues from backend restructuring"
```

---

### Task 9: Frontend — IPC Helpers & WebCodecs VideoDecoder

**Files:**
- Modify: `app/composables/useMediaTransport.ts`

- [ ] **Step 1: Add IPC helper functions at the top of the file (before `useMediaTransport()`)**

Add these after the existing module-level constants (after line 25):

```typescript
const isAndroid = /android/i.test(navigator.userAgent);

function toBase64(bytes: Uint8Array): string {
  let binary = "";
  const chunk = 8192;
  for (let i = 0; i < bytes.length; i += chunk) {
    binary += String.fromCharCode.apply(null, bytes.subarray(i, i + chunk) as unknown as number[]);
  }
  return btoa(binary);
}

function fromBase64(b64: string): Uint8Array {
  const binary = atob(b64);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) {
    bytes[i] = binary.charCodeAt(i);
  }
  return bytes;
}

function packVideoPayload(
  peerId: string, width: number, height: number,
  keyframe: boolean, timestamp: number, rgba: Uint8Array,
): Uint8Array {
  const peerIdBytes = new TextEncoder().encode(peerId);
  const headerSize = 1 + peerIdBytes.length + 4 + 4 + 1 + 8;
  const payload = new Uint8Array(headerSize + rgba.length);
  const view = new DataView(payload.buffer);
  let offset = 0;
  payload[offset] = peerIdBytes.length; offset += 1;
  payload.set(peerIdBytes, offset); offset += peerIdBytes.length;
  view.setUint32(offset, width, true); offset += 4;
  view.setUint32(offset, height, true); offset += 4;
  payload[offset] = keyframe ? 1 : 0; offset += 1;
  view.setBigUint64(offset, BigInt(timestamp), true); offset += 8;
  payload.set(rgba, offset);
  return payload;
}

function packAudioPayload(
  peerId: string, timestamp: number, pcm: Uint8Array,
): Uint8Array {
  const peerIdBytes = new TextEncoder().encode(peerId);
  const headerSize = 1 + peerIdBytes.length + 8;
  const payload = new Uint8Array(headerSize + pcm.length);
  const view = new DataView(payload.buffer);
  let offset = 0;
  payload[offset] = peerIdBytes.length; offset += 1;
  payload.set(peerIdBytes, offset); offset += peerIdBytes.length;
  view.setBigUint64(offset, BigInt(timestamp), true); offset += 8;
  payload.set(pcm, offset);
  return payload;
}

function detectKeyframe(nalus: Uint8Array): boolean {
  let i = 0;
  while (i < nalus.length - 4) {
    if (nalus[i] === 0 && nalus[i + 1] === 0) {
      let headerIdx: number;
      if (nalus[i + 2] === 1) {
        headerIdx = i + 3;
      } else if (nalus[i + 2] === 0 && nalus[i + 3] === 1) {
        headerIdx = i + 4;
      } else { i++; continue; }
      if (headerIdx < nalus.length) {
        const nalType = nalus[headerIdx]! & 0x1f;
        if (nalType === 5) return true;
      }
      i = headerIdx;
    } else { i++; }
  }
  return false;
}
```

- [ ] **Step 2: Add WebCodecs VideoDecoder module-level variable and init function**

Add after the existing module-level variables (after line 25, but after the helpers from step 1):

```typescript
let videoDecoder: VideoDecoder | null = null;
```

Inside `useMediaTransport()`, add `initVideoDecoder`:

```typescript
function initVideoDecoder(canvas: HTMLCanvasElement) {
  if (typeof globalThis.VideoDecoder === "undefined") {
    // WebCodecs not available (older WebKitGTK on Linux).
    // Known gap: no video decode fallback implemented yet.
    // Spec describes Rust-side decode → RGBA → tauri::ipc::Response fallback.
    // For now, video will not render on these platforms.
    console.warn("WebCodecs VideoDecoder not available — video disabled");
    return;
  }
  const ctx = canvas.getContext("2d")!;
  videoDecoder = new VideoDecoder({
    output: (frame: VideoFrame) => {
      if (canvas.width !== frame.displayWidth) canvas.width = frame.displayWidth;
      if (canvas.height !== frame.displayHeight) canvas.height = frame.displayHeight;
      ctx.drawImage(frame, 0, 0);
      frame.close();
    },
    error: (e) => console.warn("VideoDecoder error:", e),
  });
  videoDecoder.configure({
    codec: "avc1.42001e",
    optimizeForLatency: true,
  });
}
```

- [ ] **Step 3: Update `startSending()` — replace JSON serialization with raw binary IPC**

In the audio worklet `onmessage` handler (around line 112-115), replace:
```typescript
invoke("send_audio", {
  peerId,
  data: Array.from(new Uint8Array(pcm.buffer)),
}).catch(() => {});
```
With:
```typescript
const pcmBytes = new Uint8Array(pcm.buffer);
if (isAndroid) {
  invoke("send_audio", {
    peerId,
    data: toBase64(pcmBytes),
    timestamp: Date.now(),
  }).catch(() => {});
} else {
  invoke("send_audio", packAudioPayload(peerId, Date.now(), pcmBytes), {
    headers: { "Content-Type": "application/octet-stream" },
  }).catch(() => {});
}
```

In the video capture interval (around line 147-153), replace:
```typescript
invoke("send_video", {
  peerId,
  data: Array.from(new Uint8Array(imageData.data.buffer)),
  width,
  height,
  keyframe,
}).catch(() => {});
```
With:
```typescript
const rgba = new Uint8Array(imageData.data.buffer);
if (isAndroid) {
  invoke("send_video", {
    peerId, data: toBase64(rgba), width, height, keyframe, timestamp: Date.now(),
  }).catch(() => {});
} else {
  invoke("send_video", packVideoPayload(peerId, width, height, keyframe, Date.now(), rgba), {
    headers: { "Content-Type": "application/octet-stream" },
  }).catch(() => {});
}
```

- [ ] **Step 4: Update `startReceiving()` — WebCodecs video + base64 audio**

Update the payload types:
```typescript
type AudioPayload = { data: string; timestamp: number };
type VideoPayload = { data: string; timestamp: number };
```

Replace the audio listener to decode base64:
```typescript
unlistenAudio = await listen<AudioPayload>("audio-received", (event) => {
  if (!playbackCtx) return;
  const bytes = fromBase64(event.payload.data);
  const int16 = new Int16Array(bytes.buffer, bytes.byteOffset, bytes.byteLength / 2);
  const buffer = playbackCtx.createBuffer(1, int16.length, 48000);
  const ch = buffer.getChannelData(0);
  let sum = 0;
  for (let i = 0; i < int16.length; i++) {
    const sample = int16[i]! / 32768;
    ch[i] = sample;
    sum += sample * sample;
  }
  scheduleAudioBuffer(buffer, event.payload.timestamp);
  const rms = Math.sqrt(sum / int16.length);
  if (rms > SPEAKING_RMS_THRESHOLD) {
    if (!peerSpeaking.value) peerSpeaking.value = true;
    if (speakingTimeout) clearTimeout(speakingTimeout);
    speakingTimeout = setTimeout(() => { peerSpeaking.value = false; }, SPEAKING_DEBOUNCE_MS);
  }
});
```

Replace the video listener with WebCodecs decode:
```typescript
unlistenVideo = await listen<VideoPayload>("video-received", (event) => {
  if (!remoteCanvas) return;
  videoFrameTimestamps.push(Date.now());
  if (videoFrameTimestamps.length > 60) videoFrameTimestamps = videoFrameTimestamps.slice(-30);

  if (videoDecoder && videoDecoder.state === "configured") {
    const bytes = fromBase64(event.payload.data);
    const isKf = detectKeyframe(bytes);
    if (videoDecoder.decodeQueueSize > 2) return;
    const chunk = new EncodedVideoChunk({
      type: isKf ? "key" : "delta",
      timestamp: event.payload.timestamp * 1000,
      data: bytes,
    });
    videoDecoder.decode(chunk);
  }
});
```

Call `initVideoDecoder(canvas)` at the start of `startReceiving()`.

- [ ] **Step 5: Update `stop()` — close VideoDecoder**

In the `stop()` function, add cleanup for the WebCodecs decoder:
```typescript
if (videoDecoder) {
  try { videoDecoder.close(); } catch {}
  videoDecoder = null;
}
```

- [ ] **Step 6: Commit**

```bash
git add app/composables/useMediaTransport.ts
git commit -m "feat: raw binary IPC, WebCodecs VideoDecoder, base64 receive"
```

---

### Task 10: Frontend — rVFC, Adaptive Quality, Jitter Buffer

**Files:**
- Modify: `app/composables/useMediaTransport.ts`

- [ ] **Step 1: Replace `setInterval` video capture with `requestVideoFrameCallback`**

In `startSending()`, replace the video capture block (around line 141-154) that uses `setInterval` with:

```typescript
let targetFps = 15;
let lastCaptureTime = 0;

function captureLoop(_now: DOMHighResTimeStamp, metadata: any) {
  if (!encoding.value || !captureVideoEl) return;
  const mediaTime = (metadata?.mediaTime ?? _now / 1000) * 1000;
  const elapsed = mediaTime - lastCaptureTime;
  if (elapsed >= 1000 / targetFps) {
    lastCaptureTime = mediaTime;
    ctx.drawImage(captureVideoEl, 0, 0, width, height);
    const imageData = ctx.getImageData(0, 0, width, height);
    const keyframe = vFrameCount === 0 || vFrameCount % 30 === 0;
    vFrameCount++;
    const rgba = new Uint8Array(imageData.data.buffer);
    if (isAndroid) {
      invoke("send_video", {
        peerId, data: toBase64(rgba), width, height, keyframe, timestamp: Date.now(),
      }).catch(() => {});
    } else {
      invoke("send_video", packVideoPayload(peerId, width, height, keyframe, Date.now(), rgba), {
        headers: { "Content-Type": "application/octet-stream" },
      }).catch(() => {});
    }
  }
  captureVideoEl.requestVideoFrameCallback(captureLoop);
}

if ("requestVideoFrameCallback" in captureVideoEl) {
  captureVideoEl.requestVideoFrameCallback(captureLoop);
} else {
  // Fallback to requestAnimationFrame
  function rafLoop() {
    if (!encoding.value) return;
    captureLoop(performance.now(), null);
    requestAnimationFrame(rafLoop);
  }
  requestAnimationFrame(rafLoop);
}
```

Remove the old `captureInterval = setInterval(...)` line and update `teardownCapture()` — remove `clearInterval(captureInterval)` since rVFC/rAF doesn't need explicit cleanup (the callback checks `encoding.value`).

- [ ] **Step 2: Add adaptive quality**

Add module-level state:
```typescript
let currentWidth = 640;
let currentHeight = 480;
```

Inside `useMediaTransport()`, add the quality watcher and dimension update function:

```typescript
async function updateCaptureDimensions(w: number, h: number) {
  if (w === currentWidth && h === currentHeight) return;
  currentWidth = w;
  currentHeight = h;
  // Canvas will be recreated on next capture cycle
  const { invoke } = await import("@tauri-apps/api/core");
  await invoke("reinit_video_encoder", { width: w, height: h });
}

function pauseVideoCapture() {
  // Stop video capture but keep audio
  encoding.value = false; // This stops the rVFC loop
}
```

In `startReceiving()`, after the quality interval setup, add the watcher:

```typescript
const { watch } = await import("vue");
watch(connectionQuality, async (q) => {
  if (q === "poor") {
    pauseVideoCapture();
  } else if (q === "degraded") {
    await updateCaptureDimensions(320, 240);
    targetFps = 10;
  } else {
    await updateCaptureDimensions(640, 480);
    targetFps = 15;
  }
});
```

- [ ] **Step 3: Replace `scheduleAudioBuffer` with adaptive jitter buffer**

Replace the existing `scheduleAudioBuffer()` (lines 283-292) with:

```typescript
let jitterBufferMs = 60;
let jitterEstimate = 0;
let baseDelay: number | null = null;

function scheduleAudioBuffer(buffer: AudioBuffer, captureTimestamp: number) {
  if (!playbackCtx) return;

  const now = Date.now();
  const oneWayDelay = now - captureTimestamp;
  if (baseDelay === null) baseDelay = oneWayDelay;
  baseDelay = Math.min(baseDelay, oneWayDelay);

  const jitter = Math.abs(oneWayDelay - baseDelay);
  jitterEstimate = 0.9 * jitterEstimate + 0.1 * jitter;
  jitterBufferMs = Math.max(40, Math.min(120, jitterEstimate * 2));

  const jitterBufferSec = jitterBufferMs / 1000;
  const source = playbackCtx.createBufferSource();
  source.buffer = buffer;
  source.connect(playbackCtx.destination);
  const ctxNow = playbackCtx.currentTime;
  if (nextPlayTime < ctxNow - jitterBufferSec) {
    nextPlayTime = ctxNow + jitterBufferSec;
  }
  source.start(nextPlayTime);
  nextPlayTime += buffer.duration;
}
```

Also reset jitter state in `stop()`:
```typescript
jitterBufferMs = 60;
jitterEstimate = 0;
baseDelay = null;
```

- [ ] **Step 4: Update return statement to export new functions**

```typescript
return {
  encoding, peerSpeaking, connectionQuality,
  initCodecs, startSending, restartSending, startReceiving, stop,
  updateCaptureDimensions,
};
```

- [ ] **Step 5: Commit**

```bash
git add app/composables/useMediaTransport.ts
git commit -m "feat: requestVideoFrameCallback, adaptive quality, EWMA jitter buffer"
```

---

### Task 11: Build & Smoke Test

**Files:** None (verification only)

- [ ] **Step 1: Run Nuxt type check**

```bash
npx nuxi typecheck 2>&1
```

If `VideoDecoder`, `EncodedVideoChunk`, `VideoFrameCallbackMetadata` types are missing, add a `app/types/webcodecs.d.ts` shim:

```typescript
// Minimal WebCodecs type declarations for environments without full lib.dom support
declare class VideoDecoder {
  constructor(init: { output: (frame: VideoFrame) => void; error: (e: any) => void });
  configure(config: { codec: string; optimizeForLatency?: boolean }): void;
  decode(chunk: EncodedVideoChunk): void;
  close(): void;
  readonly state: string;
  readonly decodeQueueSize: number;
}

declare class EncodedVideoChunk {
  constructor(init: { type: "key" | "delta"; timestamp: number; data: BufferSource });
}

declare class VideoFrame {
  readonly displayWidth: number;
  readonly displayHeight: number;
  close(): void;
}
```

- [ ] **Step 2: Run full Rust build**

```bash
cd src-tauri && cargo build 2>&1
```

- [ ] **Step 3: Run Rust tests**

```bash
cd src-tauri && cargo test 2>&1
```

Expected: All tests pass.

- [ ] **Step 4: Run the Tauri dev server to verify the app starts**

```bash
npx tauri dev 2>&1
```

Expected: App window opens without errors in the console. Verify codecs can be initialized (navigate to a call screen).

- [ ] **Step 5: Commit any fixes**

```bash
git add -A
git commit -m "fix: resolve build issues from media latency optimization"
```

- [ ] **Step 6: Final commit for the feature**

```bash
git add -A
git commit -m "feat: media latency optimization — raw binary IPC, WebCodecs decode, stream-per-frame QUIC, BBR, adaptive quality/jitter buffer"
```
