# Media Latency Optimization Design

**Date:** 2026-03-26
**Status:** Draft (v2 — updated with research findings)
**Branch:** `feat/tauri-migration`

## Problem

The current media pipeline has ~78-225ms of one-way latency. Profiling identified 12 bottlenecks across the Rust codec layer, Tauri IPC, and frontend capture/playback. Additional research revealed 3 transport-layer improvements. This spec addresses all 15 to bring estimated one-way latency down to ~16-49ms (excluding jitter buffer).

## Architecture Overview

Current pipeline:
```
Capture → JSON serialize (RGBA) → Tauri IPC → Rust encode → QUIC (single long stream)
  → QUIC receive → broadcast(1024) → Rust decode → JPEG re-encode
  → JSON serialize → Tauri event → JPEG decode → Canvas
```

Optimized pipeline:
```
Capture (rVFC) → raw binary IPC → Rust encode → QUIC (stream-per-frame, BBR)
  → QUIC receive → watch channel → Tauri event (base64 H.264 NALUs)
  → WebCodecs VideoDecoder (HW accel) → Canvas
```

Key architecture decisions:
- **Video encode** stays in Rust (canvas capture → raw binary IPC → Rust H.264). `MediaStreamTrackProcessor` is not available on macOS/iOS/Linux WebViews, so canvas capture remains the cross-platform path.
- **Video decode** moves to WebCodecs `VideoDecoder` in the browser (hardware-accelerated, available on all Tauri platforms).
- **Audio encode/decode** stays in Rust (WebCodecs `AudioEncoder`/`AudioDecoder` not available on Safari < 26 / iOS < 26).
- **IPC** uses Tauri v2 raw binary for the send path on desktop (large payloads), with base64 fallback on Android (where `InvokeBody::Raw` is not supported). Events with base64 for the receive path (small payloads).
- **QUIC transport** uses stream-per-frame, BBR congestion control, and stream priorities.

---

## Section 1: Video Receive — WebCodecs Decode

**Issues addressed:** #2 (JPEG re-encoding), #3 (single-threaded decode), #5 (video mutex contention)

### Rust Changes

**`lib.rs` video forwarder** — Stop decoding H.264 on the Rust side. Forward raw H.264 NALUs as base64 to the frontend via Tauri event. Base64 is used for events because Tauri v2 event payloads are JSON-serialized (`Vec<u8>` becomes a JSON number array). H.264 NALUs are small (5-50KB), so base64 overhead is negligible (~0.5ms).

```rust
use base64::Engine;
const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

// Video forwarder task (separate from audio forwarder)
STREAM_VIDEO => {
    let b64 = B64.encode(&payload);
    let _ = app_handle.emit("video-received", VideoEvent {
        data: b64,
        timestamp: timestamp_ms,
    });
}
```

```rust
#[derive(Clone, serde::Serialize)]
struct VideoEvent {
    data: String,       // base64-encoded H.264 NALUs
    timestamp: u64,     // capture timestamp from sender
}
```

Remove `decoded_to_jpeg()`, the `test_jpeg_encode` test, and the `image` crate dependency.

### Frontend Changes

**`useMediaTransport.ts`** — Use WebCodecs `VideoDecoder` for hardware-accelerated H.264 decode:

```typescript
let videoDecoder: VideoDecoder | null = null;

function initVideoDecoder(canvas: HTMLCanvasElement) {
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
    codec: "avc1.42001e",  // Constrained Baseline, Level 3.0
    optimizeForLatency: true,
  });
}
```

Updated payload type and receive listener:

```typescript
type VideoPayload = {
  data: string;      // base64-encoded H.264 NALUs
  timestamp: number;
};

unlistenVideo = await listen<VideoPayload>("video-received", (event) => {
  if (!videoDecoder || !remoteCanvas) return;
  videoFrameTimestamps.push(Date.now());
  if (videoFrameTimestamps.length > 60)
    videoFrameTimestamps = videoFrameTimestamps.slice(-30);

  const bytes = fromBase64(event.payload.data);
  const isKeyframe = detectKeyframe(bytes);

  // Drop frames if decoder queue is backing up
  if (videoDecoder.decodeQueueSize > 2) return;

  const chunk = new EncodedVideoChunk({
    type: isKeyframe ? "key" : "delta",
    timestamp: event.payload.timestamp * 1000, // WebCodecs uses microseconds
    data: bytes,
  });
  videoDecoder.decode(chunk);
});
```

**Keyframe detection — Annex B parsing:** OpenH264 outputs Annex B format with start code prefixes (`00 00 00 01` or `00 00 01`). Scan for NAL unit type 5 (IDR):

```typescript
function detectKeyframe(nalus: Uint8Array): boolean {
  let i = 0;
  while (i < nalus.length - 4) {
    if (nalus[i] === 0 && nalus[i + 1] === 0) {
      let headerIdx: number;
      if (nalus[i + 2] === 1) {
        headerIdx = i + 3;
      } else if (nalus[i + 2] === 0 && nalus[i + 3] === 1) {
        headerIdx = i + 4;
      } else {
        i++;
        continue;
      }
      if (headerIdx < nalus.length) {
        const nalType = nalus[headerIdx]! & 0x1f;
        if (nalType === 5) return true; // IDR slice
      }
      i = headerIdx;
    } else {
      i++;
    }
  }
  return false;
}
```

Note: NAL types 7 (SPS) and 8 (PPS) precede IDR frames but are not themselves keyframes.

**WebCodecs codec string:** `"avc1.42001e"` = Baseline profile, Level 3.0. OpenH264 outputs Constrained Baseline (a subset) — the decoder handles both. Annex B bitstreams do not require a `description` field in the config; SPS/PPS must be included in-band with keyframe chunks.

**Fallback:** If `typeof VideoDecoder === 'undefined'` (older WebKitGTK), fall back to Rust-side decode → raw RGBA via `tauri::ipc::Response` → `ImageData` → `putImageData()`.

### Impact

Removes ~15-30ms per frame (Rust H.264 decode + JPEG encode + browser JPEG decode). Adds ~1-3ms hardware-accelerated decode. Eliminates video decode blocking audio decode and video mutex contention.

---

## Section 2: Raw Binary IPC

**Issues addressed:** #1 (video send JSON), #4 (audio send/receive JSON)

### Problem

`Array.from(new Uint8Array(...))` converts binary data to a JSON number array. For 640x480 RGBA (1,228,800 bytes), this produces ~4MB of JSON text per frame at 15fps.

### Solution

Tauri v2 supports raw binary transfer via `tauri::ipc::Request` (frontend→Rust) when sending a `Uint8Array` as the top-level `invoke()` payload with `Content-Type: application/octet-stream` header. This bypasses JSON serialization entirely.

**Caveat:** Raw binary mode sends the entire payload as one opaque blob — no automatic parameter deserialization. Metadata (peer ID, width, height, etc.) must be packed into a binary header prepended to the payload.

**Android limitation:** `InvokeBody::Raw` is not supported on Android (Tauri always deserializes as `InvokeBody::Json` on Android). On Android, fall back to base64 encoding the binary data in a JSON payload. Detect at runtime:

```typescript
const isAndroid = /android/i.test(navigator.userAgent);

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

The Rust command accepts both modes via `tauri::ipc::Request`:

```rust
#[tauri::command]
async fn send_video(request: tauri::ipc::Request<'_>, state: State<'_, AppState>) -> Result<(), String> {
    match request.body() {
        tauri::ipc::InvokeBody::Raw(data) => parse_binary_video(data, &state).await,
        tauri::ipc::InvokeBody::Json(value) => parse_json_video(value, &state).await,
    }
}
```

Base64 fallback helper (also used for Android audio):

```typescript
function toBase64(bytes: Uint8Array): string {
  let binary = "";
  const chunk = 8192;
  for (let i = 0; i < bytes.length; i += chunk) {
    binary += String.fromCharCode.apply(null, bytes.subarray(i, i + chunk) as unknown as number[]);
  }
  return btoa(binary);
}
```

### Video Send — Binary Header Format

```
[peer_id_len: u8][peer_id: N bytes][width: u32 LE][height: u32 LE][keyframe: u8][timestamp: u64 LE][rgba_data: remaining]
```

**Frontend (`useMediaTransport.ts`):**

```typescript
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

// In capture loop:
const imageData = ctx.getImageData(0, 0, width, height);
const payload = packVideoPayload(
  peerId, width, height, keyframe, Date.now(),
  new Uint8Array(imageData.data.buffer),
);
invoke("send_video", payload, {
  headers: { "Content-Type": "application/octet-stream" },
}).catch(() => {});
```

**Rust (`commands.rs`):**

```rust
#[tauri::command]
async fn send_video(
    request: tauri::ipc::Request<'_>,
    state: State<'_, AppState>,
) -> Result<(), String> {
    match request.body() {
        tauri::ipc::InvokeBody::Raw(data) => {
            // Desktop path: binary header + RGBA payload
            if data.is_empty() { return Err("Empty payload".into()); }
            let mut offset = 0usize;
            let peer_id_len = data[offset] as usize; offset += 1;
            if data.len() < offset + peer_id_len + 17 {
                return Err("Payload too short".into());
            }
            let peer_id = String::from_utf8_lossy(&data[offset..offset + peer_id_len]).to_string();
            offset += peer_id_len;
            let width = u32::from_le_bytes(data[offset..offset+4].try_into().unwrap()); offset += 4;
            let height = u32::from_le_bytes(data[offset..offset+4].try_into().unwrap()); offset += 4;
            let keyframe = data[offset] != 0; offset += 1;
            let timestamp = u64::from_le_bytes(data[offset..offset+8].try_into().unwrap()); offset += 8;
            let rgba = &data[offset..];
            validate_peer_id(&peer_id)?;
            // ... encode with video_encoder, send with timestamp
            Ok(())
        }
        tauri::ipc::InvokeBody::Json(value) => {
            // Android fallback: JSON with base64-encoded RGBA
            parse_json_video(value, &state).await
        }
    }
}
```

### Audio Send — Binary Header Format

```
[peer_id_len: u8][peer_id: N bytes][timestamp: u64 LE][pcm_data: remaining]
```

Same pattern — frontend packs a small header + 1920 bytes PCM, sends as raw binary. Rust parses header and encodes with Opus.

### Audio/Video Receive — Events with Base64

H.264 NALUs (5-50KB) and decoded PCM (1920 bytes) are small enough that base64 encoding in Tauri events is negligible overhead (~0.5ms). Events remain the receive mechanism:

```rust
// Audio receive — base64 PCM
let raw: Vec<u8> = pcm.iter().flat_map(|s| s.to_le_bytes()).collect();
let _ = app_handle.emit("audio-received", AudioEvent {
    data: B64.encode(&raw),
    timestamp: timestamp_ms,
});
```

```rust
#[derive(Clone, serde::Serialize)]
struct AudioEvent {
    data: String,       // base64-encoded PCM Int16 LE
    timestamp: u64,     // capture timestamp from sender
}
```

Frontend base64 decode helper:

```typescript
function fromBase64(b64: string): Uint8Array {
  const binary = atob(b64);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) {
    bytes[i] = binary.charCodeAt(i);
  }
  return bytes;
}
```

Updated audio payload type:

```typescript
type AudioPayload = {
  data: string;      // base64-encoded PCM Int16 LE
  timestamp: number;
};
```

### Dependencies

Add to `Cargo.toml`:
```toml
base64 = "0.22"
```

Remove from `Cargo.toml`:
```toml
image = { ... }  # no longer needed
```

### Impact

Video send: 1.2MB raw binary instead of ~4MB JSON or ~1.6MB base64. Zero serialization overhead on the send path. Audio/video receive: base64 on small payloads (~0.5ms overhead).

---

## Section 3: Rust Codec Restructuring

**Issues addressed:** #5 (shared mutex), #7 (H.264 encoder config), #8 (Opus FEC)

### Split Encoder/Decoder

```rust
pub struct AudioEncoder {
    encoder: OpusEncoder,
}

pub struct AudioDecoder {
    decoder: OpusDecoder,
    prev_packet_lost: bool,
}

pub struct VideoEncoder {
    encoder: H264Encoder,
    width: u32,
    height: u32,
}

pub struct VideoDecoder {
    decoder: H264Decoder,
}

pub struct CodecState {
    pub audio_encoder: Mutex<Option<AudioEncoder>>,
    pub audio_decoder: Mutex<Option<AudioDecoder>>,
    pub video_encoder: Mutex<Option<VideoEncoder>>,
    pub video_decoder: Mutex<Option<VideoDecoder>>, // WebCodecs fallback only
}
```

### Command Updates

**`init_codecs`** — Create all four:

```rust
#[tauri::command]
pub async fn init_codecs(
    width: u32, height: u32, state: State<'_, AppState>,
) -> Result<(), String> {
    if width == 0 || width > MAX_RESOLUTION || height == 0 || height > MAX_RESOLUTION {
        return Err("Invalid resolution".into());
    }
    *state.codec.audio_encoder.lock().await = Some(AudioEncoder::new());
    *state.codec.audio_decoder.lock().await = Some(AudioDecoder::new());
    *state.codec.video_encoder.lock().await = Some(VideoEncoder::new(width, height));
    *state.codec.video_decoder.lock().await = Some(VideoDecoder::new());
    tracing::info!("Codecs initialized: {width}x{height}");
    Ok(())
}
```

**`destroy_codecs`** — Clear all four:

```rust
#[tauri::command]
pub async fn destroy_codecs(state: State<'_, AppState>) -> Result<(), String> {
    *state.codec.audio_encoder.lock().await = None;
    *state.codec.audio_decoder.lock().await = None;
    *state.codec.video_encoder.lock().await = None;
    *state.codec.video_decoder.lock().await = None;
    Ok(())
}
```

**`send_audio` / `send_video`** — Lock `audio_encoder` / `video_encoder` (not shared with decode).

**New `reinit_video_encoder`** — For adaptive quality resolution changes without resetting audio state:

```rust
#[tauri::command]
pub async fn reinit_video_encoder(
    width: u32, height: u32, state: State<'_, AppState>,
) -> Result<(), String> {
    if width == 0 || width > MAX_RESOLUTION || height == 0 || height > MAX_RESOLUTION {
        return Err("Invalid resolution".into());
    }
    *state.codec.video_encoder.lock().await = Some(VideoEncoder::new(width, height));
    Ok(())
}
```

Register in `invoke_handler` in `lib.rs`.

### H.264 Encoder Configuration

The `openh264` 0.9 crate uses newtype wrappers:

```rust
use openh264::encoder::{EncoderConfig, BitRate, FrameRate, RateControlMode};

impl VideoEncoder {
    pub fn new(width: u32, height: u32) -> Self {
        let api = OpenH264API::from_source();
        let config = EncoderConfig::new()
            .bitrate(BitRate::from_bps(500_000))
            .max_frame_rate(FrameRate::from_hz(15.0))
            .rate_control_mode(RateControlMode::Bitrate);
        let encoder = H264Encoder::with_api_config(api, config)
            .expect("failed to create H264 encoder");
        Self { encoder, width, height }
    }
}
```

### Opus FEC

The `opus` 0.3 crate exposes typed methods:

```rust
impl AudioEncoder {
    pub fn new() -> Self {
        let mut encoder = OpusEncoder::new(SAMPLE_RATE, CHANNELS, Application::Voip)
            .expect("failed to create Opus encoder");
        encoder.set_inband_fec(true).expect("failed to enable FEC");
        encoder.set_packet_loss_perc(5).expect("failed to set loss %");
        Self { encoder }
    }
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
            Ok(n) => { pcm.truncate(n); Some(pcm) }
            Err(e) => {
                self.prev_packet_lost = true;
                tracing::warn!("Opus decode error: {e}");
                None
            }
        }
    }
}
```

### Impact

Eliminates 0-20ms lock contention. H.264 config produces consistent frame sizes. Opus FEC reconstructs audio during packet loss.

---

## Section 4: Transport & Timing

**Issues addressed:** #6 (unbounded broadcast channel), #9 (receive-side timestamps)

### Separate Channels with Frame Dropping

```rust
/// (peer_id, timestamp_ms, encoded_payload)
type MediaPacket = (String, u64, Vec<u8>);

// Audio: bounded broadcast, 16 slots (~320ms at 50fps)
let (audio_media_tx, _) = broadcast::channel::<MediaPacket>(16);

// Video: watch channel — only latest frame matters
let (video_watch_tx, _) = tokio::sync::watch::channel::<Option<MediaPacket>>(None);
```

`watch` for video: the forwarder always gets the most recent frame. Intermediate frames are silently dropped — no latency spiral. `watch::Sender::send()` always notifies waiters regardless of value equality. The `MediaPacket` tuple carries peer_id for multi-peer support.

Video forwarder must clone before awaiting:
```rust
loop {
    if video_watch_rx.changed().await.is_err() { break; }
    let frame = video_watch_rx.borrow_and_update().clone();
    if let Some((_peer_id, timestamp, payload)) = frame {
        let b64 = B64.encode(&payload);
        let _ = app_handle.emit("video-received", VideoEvent { data: b64, timestamp });
    }
}
```

Spawn **two separate forwarder tasks** in `lib.rs`:
- **Audio forwarder:** subscribes to `audio_media_tx`, locks `audio_decoder`, decodes Opus, emits `"audio-received"`
- **Video forwarder:** subscribes to `video_watch_tx`, base64-encodes H.264 NALUs, emits `"video-received"`

### Send-Side Timestamps

Prepend 8-byte big-endian timestamp to each framed payload:

```
[len: u32 BE][timestamp_ms: u64 BE][encoded_payload: (len - 8) bytes]
```

Backward-incompatible (acceptable for pre-release).

**Send path:** `send_audio`/`send_video` commands accept timestamp from frontend, prepend to QUIC payload:

```rust
let mut payload = Vec::with_capacity(8 + data.len());
payload.extend_from_slice(&timestamp.to_be_bytes());
payload.extend_from_slice(data);
write_framed(send, &payload).await?;
```

**Receive path:** Extract timestamp from payload instead of `SystemTime::now()`:

```rust
let timestamp_ms = u64::from_be_bytes(data[..8].try_into().expect("timestamp bytes"));
let payload = data[8..].to_vec();
```

### Impact

Eliminates latency spiral. Enables jitter measurement for the adaptive buffer.

---

## Section 5: Frontend Capture & Playback

**Issues addressed:** #10 (setInterval), #11 (no adaptive quality), #12 (no jitter buffer)

### Replace setInterval with requestVideoFrameCallback

```typescript
let targetFps = 15;
let lastCaptureTime = 0;

function captureLoop(_now: DOMHighResTimeStamp, metadata: VideoFrameCallbackMetadata) {
  if (!encoding.value || !captureVideoEl) return;
  const elapsed = metadata.mediaTime * 1000 - lastCaptureTime;
  if (elapsed >= 1000 / targetFps) {
    lastCaptureTime = metadata.mediaTime * 1000;
    ctx.drawImage(captureVideoEl, 0, 0, width, height);
    const imageData = ctx.getImageData(0, 0, width, height);
    // ... pack and send via raw binary IPC
  }
  captureVideoEl.requestVideoFrameCallback(captureLoop);
}
captureVideoEl.requestVideoFrameCallback(captureLoop);
```

**Fallback:** `requestAnimationFrame` with `performance.now()` timing if `requestVideoFrameCallback` unavailable.

**TypeScript types:** Add `VideoFrameCallbackMetadata` declaration if not in project's `lib.dom` types.

### Adaptive Quality

```typescript
let currentWidth = 640;
let currentHeight = 480;

watch(connectionQuality, async (q) => {
  if (q === "poor") {
    pauseVideoCapture(); // audio-only
  } else if (q === "degraded") {
    await updateCaptureDimensions(320, 240);
    targetFps = 10;
  } else {
    await updateCaptureDimensions(640, 480);
    targetFps = 15;
  }
});

async function updateCaptureDimensions(w: number, h: number) {
  if (w === currentWidth && h === currentHeight) return;
  currentWidth = w;
  currentHeight = h;
  canvas.width = w;
  canvas.height = h;
  const { invoke } = await import("@tauri-apps/api/core");
  await invoke("reinit_video_encoder", { width: w, height: h });
}
```

Uses `reinit_video_encoder` to avoid resetting audio state.

### Adaptive Audio Jitter Buffer

EWMA-based, depends on send-side timestamps:

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

Range: 40-120ms. LAN: ~40ms. Relay: ~120ms.

---

## Section 6: QUIC Transport Optimization

**New section — from MoQ/IETF research**

### Stream-Per-Frame

**Problem:** The current architecture opens one long-lived uni-stream per media type (audio, video). All video frames are sent sequentially on a single stream. If one packet is lost, QUIC retransmits it, blocking ALL subsequent frames on that stream (intra-stream head-of-line blocking). The receiver cannot skip the lost frame.

**Solution:** Open a new QUIC uni-stream per video frame. Per the MoQ consensus ([moq.dev](https://moq.dev/blog/never-use-datagrams/), [IETF MoQ Transport draft](https://moq-wg.github.io/moq-transport/draft-ietf-moq-transport.html)):

- Each `send_video` call opens a new `connection.open_uni()`, writes the type byte + timestamp + encoded frame, then finishes the stream
- If a frame becomes stale before it finishes sending (newer frame ready), the old stream can be reset/cancelled
- QUIC handles fragmentation, reliability, and flow control per-stream

```rust
pub async fn send_video(&self, peer_id: &str, data: &[u8], timestamp: u64) -> Result<()> {
    let connection = {
        let peers = self.peers.lock().await;
        peers.get(peer_id).map(|p| p.connection.clone())
    };
    if let Some(conn) = connection {
        let mut send = conn.open_uni().await?;
        let priority = if is_keyframe(data) { 50 } else { 30 };
        send.set_priority(priority)?;
        send.write_all(&[STREAM_VIDEO]).await?;
        let mut payload = Vec::with_capacity(8 + data.len());
        payload.extend_from_slice(&timestamp.to_be_bytes());
        payload.extend_from_slice(data);
        write_framed(&mut send, &payload).await?;
        send.finish()?; // signal end of this frame's stream
    }
    Ok(())
}
```

Each frame opens a fresh uni-stream, writes the data, and finishes immediately. QUIC handles per-stream independence — if the previous frame's packets are still in flight when a new frame opens a new stream, the priority system ensures the newer/higher-priority frame is transmitted first. No explicit stale frame cancellation is needed; stream-per-frame with priorities achieves the same goal naturally.

**Audio:** Keep a single long-lived uni-stream for audio. Audio frames are tiny (~60-200 bytes) and latency-critical — the overhead of opening a new stream per 20ms frame is not worth it. Audio's single stream gets the highest media priority (90), ensuring it is never blocked by video.

### BBR Congestion Control

**Problem:** Quinn's default congestion controller (NewReno) is loss-based — it fills network buffers before reacting, causing latency spikes.

**Solution:** Switch to BBR, which is delay-based and probes for bandwidth without filling buffers. Iroh 0.97 uses `noq-proto` (a fork of Quinn) which exposes BBR:

```rust
use iroh::endpoint::QuicTransportConfig;
use noq_proto::congestion::BbrConfig;
use std::sync::Arc;

let transport_config = QuicTransportConfig::builder()
    .congestion_controller_factory(Arc::new(BbrConfig::default()))
    .build();

let endpoint = Endpoint::builder(presets::N0)
    .transport_config(transport_config)
    // ...
```

**Dependency:** Add `noq-proto` as a direct dependency in `Cargo.toml`. Iroh 0.97 re-exports `Controller`, `ControllerFactory`, and `ControllerMetrics` from `noq_proto::congestion`, but does NOT re-export `BbrConfig` — it must be imported directly:

```toml
noq-proto = "0.16"  # match version used by iroh 0.97
```

Note: Verify the exact `noq-proto` version constraint by checking `iroh`'s `Cargo.toml` to avoid version conflicts.

### Stream Priorities

Quinn exposes per-stream priority via `SendStream::set_priority(i32)`. Higher values = transmitted first.

```
Priority    Stream Type
────────────────────────────
100         Control (mute, video off)
 90         Audio frames
 50         Video keyframes (I-frames)
 30         Video delta frames (P-frames)
 10         Chat messages
```

Audio > Video because audio glitches are far more perceptible than video stutters. Keyframes > P-frames because a lost keyframe makes all subsequent P-frames undecodable.

```rust
// Audio — set once on the long-lived stream
audio_send.set_priority(90)?;

// Video — set per-frame stream
let priority = if is_keyframe { 50 } else { 30 };
send.set_priority(priority)?;

// Control
control_send.set_priority(100)?;

// Chat
chat_send.set_priority(10)?;
```

**Keyframe detection in Rust:** Parse the first NALU type from the Annex B bitstream:

```rust
fn is_keyframe(h264_data: &[u8]) -> bool {
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
                return true; // IDR
            }
            i = header_idx;
        } else {
            i += 1;
        }
    }
    false
}
```

### Receive Path Changes

The receive side now accepts many short-lived uni-streams instead of one long-lived stream per type:

```rust
// In spawn_stream_receivers — accept_uni loop
let peers_ref = self.peers.clone();
let event_tx = self.event_tx.clone();
tokio::spawn(async move {
    loop {
        match connection.accept_uni().await {
            Ok(mut recv) => {
                let peer_id = peer_id.clone();
                let audio_tx = audio_media_tx.clone();
                let video_tx = video_watch_tx.clone();
                tokio::spawn(async move {
                    let mut type_buf = [0u8; 1];
                    if recv.read_exact(&mut type_buf).await.is_err() { return; }
                    match type_buf[0] {
                        STREAM_AUDIO => {
                            // Audio: read all frames from long-lived stream
                            loop {
                                match read_framed(&mut recv).await {
                                    Ok(Some(data)) if data.len() >= 8 => {
                                        let ts = u64::from_be_bytes(data[..8].try_into().unwrap());
                                        let payload = data[8..].to_vec();
                                        let _ = audio_tx.send((peer_id.clone(), ts, payload));
                                    }
                                    _ => break,
                                }
                            }
                        }
                        STREAM_VIDEO => {
                            // Video: single frame per stream (stream-per-frame)
                            if let Ok(Some(data)) = read_framed(&mut recv).await {
                                if data.len() >= 8 {
                                    let ts = u64::from_be_bytes(data[..8].try_into().unwrap());
                                    let payload = data[8..].to_vec();
                                    let _ = video_tx.send(Some((peer_id.clone(), ts, payload)));
                                }
                            }
                        }
                        _ => {}
                    }
                });
            }
            Err(_) => {
                // Connection lost — clean up peer and emit disconnect
                peers_ref.lock().await.remove(&peer_id);
                let _ = event_tx.send(Event::PeerDisconnected { peer_id: peer_id.clone() });
                break;
            }
        }
    }
});
```

### Impact

Stream-per-frame eliminates intra-stream HOL blocking (potential 50-200ms savings under packet loss). BBR prevents buffer-bloat latency spikes. Priorities ensure audio is never starved by video.

---

## Files Modified

| File | Changes |
|------|---------|
| `src-tauri/Cargo.toml` | Add `base64 = "0.22"`, `noq-proto = "0.16"`; remove `image` |
| `src-tauri/src/codec.rs` | Split into `AudioEncoder`/`AudioDecoder`/`VideoEncoder`/`VideoDecoder`; H.264 config; Opus FEC; remove `decoded_to_jpeg()` and tests |
| `src-tauri/src/commands.rs` | `send_video`/`send_audio` use `tauri::ipc::Request<'_>` with dual-mode parsing (raw binary on desktop, JSON/base64 fallback on Android); accept `timestamp`; use split codec state; add `reinit_video_encoder` |
| `src-tauri/src/lib.rs` | Separate audio/video channels + forwarder tasks; `VideoEvent`/`AudioEvent` structs; register `reinit_video_encoder`; remove `MediaEvent` |
| `src-tauri/src/connection.rs` | Stream-per-frame for video (open_uni + finish per frame); long-lived stream for audio; stream priorities; prepend timestamps; accept per-type channel senders; `is_keyframe()` helper; peer disconnect handling |
| `src-tauri/src/messages.rs` | Remove `MediaFrame` struct; keep `write_framed`/`read_framed` |
| `src-tauri/src/state.rs` | Update `AppState` for `broadcast::Sender<MediaPacket>` + `watch::Sender<Option<MediaPacket>>` (where `MediaPacket = (String, u64, Vec<u8>)`) and split `CodecState` |
| `src-tauri/src/node.rs` | Configure BBR via `QuicTransportConfig::builder()` on endpoint |
| `app/composables/useMediaTransport.ts` | Raw binary `invoke` for send; WebCodecs `VideoDecoder` + `decodeQueueSize` guard; `detectKeyframe`; `fromBase64`; `packVideoPayload`/`packAudioPayload`; rVFC + rAF fallback; adaptive quality; EWMA jitter buffer; updated payload types |
| `app/pages/call.vue` | No API changes needed |

## Estimated Latency Budget

| Segment | Current (ms) | Optimized (ms) |
|---------|-------------|----------------|
| Video capture + serialize | 35-85 | 2-5 (raw binary) |
| Audio capture + serialize | 5-10 | 1-2 (raw binary) |
| Encode (audio) | <5 | <5 |
| Encode (video) | 5-20 | 5-15 |
| Network (QUIC) | 1-5 | 1-3 (BBR, priorities) |
| HOL blocking (video) | 0-50 (loss) | 0 (stream-per-frame) |
| Decode (video) | 10-30 + JPEG 10-25 | 1-3 (HW WebCodecs) |
| Decode (audio, blocked) | <5 + 10-30 blocked | <5 (separate task) |
| Receive serialize | 5-15 | <1 (base64, small payloads) |
| Render | 2-5 | 2-5 |
| **Pipeline total** | **78-225** | **16-49** |
| Jitter buffer | 0-200 (hard reset) | 40-120 (adaptive) |
| **End-to-end total** | **78-425** | **56-169** |

---

## Appendix: Future Tier 1 Optimizations (Chromium WebViews)

The following optimizations require APIs only available on Chromium-based WebViews (Windows WebView2, Android System WebView). They are not feasible on macOS WKWebView, iOS WKWebView, or Linux WebKitGTK today.

**Detection:** `typeof MediaStreamTrackProcessor !== 'undefined'`

### MediaStreamTrackProcessor (Zero-Copy Capture)

Replace canvas capture with `MediaStreamTrackProcessor` for zero-copy `VideoFrame` access:

```typescript
const processor = new MediaStreamTrackProcessor({ track: videoTrack });
const reader = processor.readable.getReader();
while (encoding.value) {
  const { value: frame, done } = await reader.read();
  if (done) break;
  encoder.encode(frame, { keyFrame: isKeyframeNeeded });
  frame.close();
}
```

Eliminates `getImageData()` overhead (10-20ms per frame). GPU-backed `VideoFrame` objects feed directly to `VideoEncoder`.

### WebCodecs VideoEncoder (Browser-Side H.264)

Encode H.264 in the browser using hardware acceleration:

```typescript
const encoder = new VideoEncoder({
  output: (chunk: EncodedVideoChunk) => {
    const data = new Uint8Array(chunk.byteLength);
    chunk.copyTo(data);
    // Send small H.264 NALUs (5-50KB) to Rust via raw binary IPC
    // Rust becomes pure transport — no encode needed
  },
  error: (e) => console.error(e),
});
encoder.configure({
  codec: "avc1.42001e",
  width: 640, height: 480,
  bitrate: 500_000,
  framerate: 15,
  latencyMode: "realtime",
  avc: { format: "annexb" },
});
```

IPC payload shrinks from 1.2MB RGBA to 5-50KB H.264 NALUs. Rust `VideoEncoder` becomes unnecessary on Tier 1.

### WebCodecs AudioEncoder/AudioDecoder

Encode/decode Opus in the browser (available on Chrome 94+, Safari 26+):

```typescript
const audioEncoder = new AudioEncoder({
  output: (chunk) => { /* ~60 bytes Opus, send to Rust */ },
  error: (e) => console.error(e),
});
audioEncoder.configure({
  codec: "opus",
  sampleRate: 48000,
  numberOfChannels: 1,
  bitrate: 24000,
  opus: { application: "voip", frameDuration: 20000, useinbandfec: true },
});
```

IPC payload shrinks from 1920 bytes PCM to ~60 bytes Opus. Rust `AudioEncoder`/`AudioDecoder` become unnecessary on Tier 1.

**Important:** Set `frameDuration: 20000` (20ms) explicitly — Chrome defaults to 60ms which adds 40ms latency. `AudioEncoder` is NOT available inside `AudioWorkletGlobalScope` — must run in a Worker fed via `SharedArrayBuffer` ring buffer from the AudioWorklet.

### Platform Support Matrix

| API | Windows (WebView2) | macOS (WKWebView) | Linux (WebKitGTK) | Android (WebView) | iOS (WKWebView) |
|-----|-------------------|------------------|-------------------|-------------------|----------------|
| VideoDecoder | Yes | Yes (13.3+) | Yes (2.44+) | Yes | Yes (16.4+) |
| VideoEncoder | Yes | Yes (13.3+) | Yes (2.44+) | Yes | Yes (16.4+) |
| AudioEncoder | Yes | Safari 26+ | 2.48+ | Yes | iOS 26+ |
| AudioDecoder | Yes | Safari 26+ | 2.48+ | Yes | iOS 26+ |
| MediaStreamTrackProcessor | Yes | **No** | **No** | Yes | **No** |
| HW H.264 (encode+decode) | Yes (DXVA) | Yes (VideoToolbox) | Via GStreamer | Yes (MediaCodec) | Yes (VideoToolbox) |
