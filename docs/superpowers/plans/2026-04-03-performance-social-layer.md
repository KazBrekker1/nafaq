# Performance, Social Layer & UX Overhaul — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add WebCodecs video decode, persistent identity, contacts, DMs with file transfer, and a tab-based navigation system to Nafaq.

**Architecture:** The Rust backend gains a dedicated video runtime (fallback), secret key persistence, a new DM QUIC stream (0x05), and contact/presence management. The frontend moves video decoding to WebCodecs where available, adds 3-tab navigation (Calls/Contacts/Messages), a Settings page, and composables for contacts, DMs, presence, and settings.

**Tech Stack:** Tauri 2, Nuxt 4/Vue 3, Iroh 0.97, Rust (tokio), WebCodecs API, tauri-plugin-store, OpenH264 (fallback), Opus

**Spec:** `docs/superpowers/specs/2026-04-03-performance-social-layer-design.md`

---

## Phase 1: Performance

### Task 1: WebCodecs Video Decode — Rust Side (raw NALU forwarding)

**Files:**
- Modify: `src-tauri/src/messages.rs`
- Modify: `src-tauri/src/lib.rs`
- Modify: `src-tauri/src/state.rs`

Currently the video forwarder in `lib.rs:315-380` decodes H.264 → RGBA → JPEG for every frame. When WebCodecs is active on the frontend, Rust should forward raw H.264 NALUs instead.

- [ ] **Step 1: Add `MediaReceiveVideoMode::RawH264Nalu` variant**

In `src-tauri/src/messages.rs`, add a new variant to `MediaReceiveVideoMode`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaReceiveVideoMode {
    DecodedJpeg,
    RawH264Nalu,
}
```

- [ ] **Step 2: Add `webcodecs_active` flag to `MediaBridgeState`**

In `src-tauri/src/state.rs`, add an atomic bool to `MediaBridgeRegistration`:

```rust
#[derive(Clone)]
pub struct MediaBridgeRegistration {
    pub profile: MediaSessionProfile,
    pub audio_channel: Option<Channel<Vec<u8>>>,
    pub video_channel: Option<Channel<Vec<u8>>>,
    pub webcodecs_active: bool,
}
```

- [ ] **Step 3: Add raw NALU binary packing function**

In `src-tauri/src/lib.rs`, add a packing function for raw H.264 NALUs (no JPEG, no decode):

```rust
fn pack_video_channel_raw_nalu(
    peer_id: &str,
    timestamp: u64,
    h264_data: &[u8],
    is_keyframe: bool,
) -> Option<Vec<u8>> {
    let peer_id_bytes = peer_id.as_bytes();
    let peer_id_len = u16::try_from(peer_id_bytes.len()).ok()?;
    let data_len = u32::try_from(h264_data.len()).ok()?;
    // Format: [peer_id_len:u16][peer_id][timestamp:u64][is_keyframe:u8][data_len:u32][h264_data]
    let mut packet = Vec::with_capacity(2 + peer_id_bytes.len() + 8 + 1 + 4 + h264_data.len());
    packet.extend_from_slice(&peer_id_len.to_le_bytes());
    packet.extend_from_slice(peer_id_bytes);
    packet.extend_from_slice(&timestamp.to_le_bytes());
    packet.push(if is_keyframe { 1 } else { 0 });
    packet.extend_from_slice(&data_len.to_le_bytes());
    packet.extend_from_slice(h264_data);
    Some(packet)
}
```

- [ ] **Step 4: Branch the video forwarder on `webcodecs_active`**

In `lib.rs`, modify the video forwarder task (currently lines 315-380). When `webcodecs_active` is true, skip decode+JPEG and forward raw NALUs:

```rust
// Inside the video forwarder task, after receiving a packet:
let registration = video_bridge.lock().await.clone();
if let Some(ref reg) = registration {
    if reg.webcodecs_active {
        // Raw NALU path — zero decode work on Rust side
        let kf = codec::is_keyframe(&packet.payload);
        if let Some(channel) = &reg.video_channel {
            if let Some(raw_packet) = pack_video_channel_raw_nalu(
                &packet.peer_id,
                packet.timestamp_ms,
                &packet.payload,
                kf,
            ) {
                let _ = channel.send(raw_packet);
            }
        }
        continue; // Skip the decode+JPEG path
    }
}
// ... existing decode+JPEG path for fallback ...
```

- [ ] **Step 5: Run `cargo test` and `cargo build`**

```bash
cd src-tauri && cargo test && cargo build
```

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/messages.rs src-tauri/src/lib.rs src-tauri/src/state.rs
git commit -m "feat: add raw H.264 NALU forwarding mode for WebCodecs"
```

---

### Task 2: WebCodecs Video Decode — Frontend

**Files:**
- Modify: `app/composables/useMediaTransport.ts`

The frontend needs to detect WebCodecs support, signal to Rust, and decode H.264 NALUs per-peer using `VideoDecoder`.

- [ ] **Step 1: Add WebCodecs feature detection**

Near the top of `useMediaTransport.ts`, after the `isAndroid` const:

```typescript
const hasWebCodecs = typeof VideoDecoder !== "undefined";
```

- [ ] **Step 2: Signal WebCodecs mode during bridge registration**

In the `registerMediaBridge()` function, when building the registration request, include the WebCodecs flag. Modify the `register_media_bridge` Tauri command invocation to pass `webcodecs_active: hasWebCodecs` as an extra field.

Update `MediaSessionProfile` interface to include:
```typescript
interface MediaSessionProfile {
  // ... existing fields ...
  receiveVideoMode: "decoded_jpeg" | "raw_h264_nalu";
}
```

- [ ] **Step 3: Add per-peer `VideoDecoder` management**

Add a `Map<string, VideoDecoder>` for peer decoders. Create/destroy decoders on peer connect/disconnect:

```typescript
const peerVideoDecoders = new Map<string, VideoDecoder>();

function getOrCreateVideoDecoder(peerId: string, canvas: HTMLCanvasElement): VideoDecoder {
  let decoder = peerVideoDecoders.get(peerId);
  if (decoder) return decoder;

  const ctx = canvas.getContext("2d")!;
  decoder = new VideoDecoder({
    output(frame: VideoFrame) {
      ctx.drawImage(frame, 0, 0, canvas.width, canvas.height);
      frame.close();
    },
    error(e: DOMException) {
      console.warn(`VideoDecoder error for ${peerId}:`, e);
    },
  });
  decoder.configure({
    codec: "avc1.42001E", // H.264 Constrained Baseline Level 3.0
    optimizeForLatency: true,
  });
  peerVideoDecoders.set(peerId, decoder);
  return decoder;
}

function destroyVideoDecoder(peerId: string) {
  const decoder = peerVideoDecoders.get(peerId);
  if (decoder && decoder.state !== "closed") {
    decoder.close();
  }
  peerVideoDecoders.delete(peerId);
}
```

- [ ] **Step 4: Parse raw NALU packets from the binary channel**

Add a parser for the raw NALU binary format sent by Rust:

```typescript
function parseRawNaluPacket(buf: ArrayBuffer) {
  const view = new DataView(buf);
  let offset = 0;
  const peerIdLen = view.getUint16(offset, true); offset += 2;
  const peerId = sharedTextDecoder.decode(new Uint8Array(buf, offset, peerIdLen)); offset += peerIdLen;
  const timestamp = Number(view.getBigUint64(offset, true)); offset += 8;
  const isKeyframe = view.getUint8(offset) === 1; offset += 1;
  const dataLen = view.getUint32(offset, true); offset += 4;
  const h264Data = new Uint8Array(buf, offset, dataLen);
  return { peerId, timestamp, isKeyframe, h264Data };
}
```

- [ ] **Step 5: Handle raw NALU frames in the video channel callback**

In the video channel's `onmessage` handler, branch on `hasWebCodecs`:

```typescript
if (hasWebCodecs) {
  const { peerId, timestamp, isKeyframe, h264Data } = parseRawNaluPacket(toArrayBuffer(data));
  const peerState = peerMediaStates.get(peerId);
  if (!peerState?.canvas) return;
  const decoder = getOrCreateVideoDecoder(peerId, peerState.canvas);
  if (decoder.state === "closed") return;
  decoder.decode(new EncodedVideoChunk({
    type: isKeyframe ? "key" : "delta",
    timestamp,
    data: h264Data,
  }));
} else {
  // Existing JPEG decode path
}
```

- [ ] **Step 6: Clean up decoders on peer disconnect**

In the disconnect handler, call `destroyVideoDecoder(peerId)`.

- [ ] **Step 7: Run `bun run tauri:dev` and test with 2 peers**

Verify video renders on both sides. On Windows/Android, confirm WebCodecs path is used. On older macOS, confirm JPEG fallback.

- [ ] **Step 8: Commit**

```bash
git add app/composables/useMediaTransport.ts
git commit -m "feat: WebCodecs H.264 decode on frontend, bypass Rust decode"
```

---

### Task 3: Dedicated Video Runtime (Fallback Path)

**Files:**
- Modify: `src-tauri/src/lib.rs`
- Modify: `src-tauri/src/state.rs`

For platforms without WebCodecs, move video decode+JPEG work to a separate tokio runtime.

- [ ] **Step 1: Add video runtime handle to `AppState`**

In `src-tauri/src/state.rs`:

```rust
pub struct AppState {
    // ... existing fields ...
    pub video_runtime: tokio::runtime::Handle,
}
```

- [ ] **Step 2: Create dedicated video runtime in `lib.rs`**

In `run()`, before the app builder setup:

```rust
let video_runtime = tokio::runtime::Builder::new_multi_thread()
    .worker_threads(2)
    .thread_name("nafaq-video")
    .enable_all()
    .build()
    .expect("Failed to create video runtime");
let video_runtime_handle = video_runtime.handle().clone();
// Keep runtime alive by leaking — it lives for the app's lifetime
std::mem::forget(video_runtime);
```

- [ ] **Step 3: Move video decode work to the video runtime**

In the video forwarder task, replace `tokio::task::block_in_place` with a spawn on the video runtime for the fallback (non-WebCodecs) path:

```rust
// In the JPEG fallback branch of the video forwarder:
let codec_video_clone = codec_video.clone();
let payload = packet.payload.clone();
let peer_id = packet.peer_id.clone();
let jpeg_result = video_runtime_handle.spawn(async move {
    let mut decoders = codec_video_clone.decoders.lock().await;
    let decoder = decoders
        .entry(peer_id)
        .or_insert_with(codec::VideoDecoder::new);
    decoder.decode_rgba(&payload).and_then(|(rgba, w, h)| {
        codec::encode_jpeg(&rgba, w, h, 70).map(|j| (j, w, h))
    })
}).await.ok().flatten();
```

- [ ] **Step 4: Run `cargo test` and `cargo build`**

```bash
cd src-tauri && cargo test && cargo build
```

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/lib.rs src-tauri/src/state.rs
git commit -m "feat: dedicated video runtime for fallback decode path"
```

---

### Task 4: Memory & GC Pressure — Ring Buffers

**Files:**
- Modify: `app/composables/useMediaTransport.ts`

Replace per-frame allocations with pre-allocated buffer pools.

- [ ] **Step 1: Create a simple buffer pool class**

Add near the top of `useMediaTransport.ts`:

```typescript
class BufferPool<T extends Int16Array | Uint8Array> {
  private pool: T[] = [];
  private factory: () => T;

  constructor(size: number, factory: () => T) {
    this.factory = factory;
    for (let i = 0; i < size; i++) {
      this.pool.push(factory());
    }
  }

  acquire(): T {
    return this.pool.pop() ?? this.factory();
  }

  release(buf: T) {
    if (this.pool.length < 16) {
      this.pool.push(buf);
    }
  }
}

let audioBufferPool: BufferPool<Int16Array> | null = null;
let videoFrameBufferPool: BufferPool<Uint8Array> | null = null;
```

- [ ] **Step 2: Initialize pools during codec init**

In `initCodecs()`, create the pools:

```typescript
audioBufferPool = new BufferPool(8, () => new Int16Array(OPUS_FRAME_SAMPLES));
videoFrameBufferPool = new BufferPool(4, () => new Uint8Array(currentWidth * currentHeight * 4));
```

- [ ] **Step 3: Use pooled buffers in the audio worklet message handler**

Replace `new Int16Array(OPUS_FRAME_SAMPLES)` allocations in the audio capture path with `audioBufferPool.acquire()`. After sending the audio, call `audioBufferPool.release(buf)`.

- [ ] **Step 4: Use pooled buffers in the video capture path**

In the `requestAnimationFrame` video capture callback, replace `new Uint8Array(imageData.data.buffer)` with a pooled buffer. After sending, release it.

- [ ] **Step 5: Eliminate base64 on event fallback path**

In `useMediaTransport.ts`, in the legacy event handlers for `audio-received` and `video-received`, replace `fromBase64(event.data)` with `toArrayBuffer(event.data)` where possible. For the Rust side, when no binary channel is registered, use `app_handle.emit_bytes()` (if available in Tauri 2) or `emit` with a raw `Vec<u8>` payload instead of base64-encoding to a `String`. This eliminates the 33% size bloat and allocation overhead on every frame in the fallback path.

- [ ] **Step 6: Clean up pools in `stop()`**

Set pools to `null` in the `stop()` function.

- [ ] **Step 7: Test with `bun run tauri:dev`**

Verify audio/video still work. Monitor memory in devtools — allocation rate should drop.

- [ ] **Step 8: Commit**

```bash
git add app/composables/useMediaTransport.ts
git commit -m "perf: pre-allocated ring buffers, eliminate base64 fallback"
```

---

### Task 5: Per-Peer Adaptive Quality

**Files:**
- Modify: `src-tauri/src/connection.rs`
- Modify: `src-tauri/src/messages.rs`

Currently quality changes are global (all peers get the same bitrate). Add per-peer quality based on network stats.

- [ ] **Step 1: Add `PerPeerQualityRequest` control action**

In `messages.rs`, add a new control action:

```rust
// In ControlAction enum:
PerPeerQualityBps { bitrate_bps: u32 },
```

- [ ] **Step 2: Track per-peer outbound quality in `PeerConnection`**

In `connection.rs`, add a field to `PeerConnection`:

```rust
struct PeerConnection {
    // ... existing fields ...
    /// Per-peer outbound bitrate override (0 = use global profile)
    outbound_bitrate_bps: Arc<AtomicU32>,
}
```

Initialize to `0` in `setup_connection`.

- [ ] **Step 3: Add method to get per-peer bitrate**

In `ConnectionManager`:

```rust
pub async fn get_peer_outbound_bitrate(&self, peer_id: &str) -> u32 {
    let peers = self.peers.lock().await;
    peers.get(peer_id)
        .map(|p| p.outbound_bitrate_bps.load(Ordering::Relaxed))
        .unwrap_or(0)
}

pub async fn set_peer_outbound_bitrate(&self, peer_id: &str, bitrate_bps: u32) {
    let peers = self.peers.lock().await;
    if let Some(peer) = peers.get(peer_id) {
        peer.outbound_bitrate_bps.store(bitrate_bps, Ordering::Relaxed);
    }
}
```

- [ ] **Step 4: Evaluate per-peer quality in the network stats reporter**

In `lib.rs`, in the network stats task (lines 401-412), after snapshotting stats, check each peer:

```rust
for stats in conn_manager_stats.snapshot_network_stats().await {
    // If RTT > 200ms or loss > 5%, reduce outbound bitrate for this peer
    let current = conn_manager_stats.get_peer_outbound_bitrate(&stats.peer_id).await;
    let target = if stats.rtt_ms > 200 || stats.lost_packets > 50 {
        100_000 // 100kbps for degraded peers
    } else {
        0 // Use global profile
    };
    if current != target {
        conn_manager_stats.set_peer_outbound_bitrate(&stats.peer_id, target).await;
    }
    let _ = app_handle_stats.emit("network-stats", &stats);
}
```

- [ ] **Step 5: Run `cargo test`**

```bash
cd src-tauri && cargo test
```

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/connection.rs src-tauri/src/messages.rs src-tauri/src/lib.rs
git commit -m "feat: per-peer adaptive quality based on network stats"
```

---

### Task 6: Receive-Side Video Pausing

**Files:**
- Modify: `app/composables/useMediaTransport.ts`
- Modify: `src-tauri/src/messages.rs`

When a peer's video tile is off-screen, stop decoding their frames and tell them to reduce quality.

- [ ] **Step 1: Add `videoPaused` to `PeerMediaState`**

```typescript
interface PeerMediaState {
  // ... existing fields ...
  videoPaused: boolean;
}
```

Initialize to `false` when creating a new `PeerMediaState`.

- [ ] **Step 2: Add `setPeerVideoPaused` function**

```typescript
async function setPeerVideoPaused(peerId: string, paused: boolean) {
  const state = peerMediaStates.get(peerId);
  if (!state || state.videoPaused === paused) return;
  state.videoPaused = paused;
  const invoke = await invokePromise;
  await invoke("send_control", {
    peerId,
    action: {
      action: "video_quality_request",
      layer: paused ? "none" : "high",
    },
  }).catch(() => {});
}
```

- [ ] **Step 3: Skip decode for paused peers in the video channel handler**

In the video channel callback, before decoding:

```typescript
const peerState = peerMediaStates.get(peerId);
if (!peerState || peerState.videoPaused) return;
```

- [ ] **Step 4: Expose `setPeerVideoPaused` from the composable**

Add it to the return object of `useMediaTransport()`.

- [ ] **Step 5: Use IntersectionObserver in `call.vue`**

In `pages/call.vue`, observe each peer's video canvas. When a canvas leaves the viewport, call `setPeerVideoPaused(peerId, true)`. When it enters, call `setPeerVideoPaused(peerId, false)`.

- [ ] **Step 6: Test with 3+ peers, scroll the grid**

Verify that off-screen peers' video stops and resumes when scrolled back.

- [ ] **Step 7: Commit**

```bash
git add app/composables/useMediaTransport.ts app/pages/call.vue
git commit -m "feat: pause video decode for off-screen peers"
```

---

### Task 7: Selective Audio Decoding at 5+ Peers

**Files:**
- Modify: `src-tauri/src/lib.rs`

At 5+ peers, only decode Opus for the top 2-3 loudest speakers.

- [ ] **Step 1: Track per-peer audio energy in the audio forwarder**

In the audio forwarder task in `lib.rs`, maintain a `HashMap<String, f32>` of recent RMS energy per peer. Compute RMS from the Opus-decoded PCM:

```rust
let mut peer_energy: HashMap<String, f32> = HashMap::new();

// After decoding PCM:
let rms = (pcm.iter().map(|&s| (s as f32).powi(2)).sum::<f32>() / pcm.len() as f32).sqrt();
peer_energy.insert(peer_id.clone(), rms);
```

- [ ] **Step 2: At 5+ peers, skip quiet speakers**

Before decoding, check peer count. If >= 5, sort peers by energy, only decode top 3:

```rust
let peer_count = last_active.len();
if peer_count >= 5 {
    let mut energies: Vec<(&String, &f32)> = peer_energy.iter().collect();
    energies.sort_by(|a, b| b.1.partial_cmp(a.1).unwrap_or(std::cmp::Ordering::Equal));
    let top_speakers: HashSet<&String> = energies.iter().take(3).map(|(id, _)| *id).collect();
    if !top_speakers.contains(&peer_id) {
        continue; // Skip decode for quiet peers
    }
}
```

- [ ] **Step 3: Run `cargo test`**

```bash
cd src-tauri && cargo test
```

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/lib.rs
git commit -m "perf: selective audio decode — top 3 speakers at 5+ peers"
```

---

## Phase 2: Identity & Settings

### Task 8: Persistent Secret Key

**Files:**
- Modify: `src-tauri/src/node.rs`
- Modify: `src-tauri/src/lib.rs`
- Modify: `src-tauri/src/commands.rs`

- [ ] **Step 1: Add key persistence to `node.rs`**

Modify `create_endpoint` to accept an optional secret key:

```rust
use iroh::SecretKey;

pub async fn create_endpoint_with_key(secret_key: Option<SecretKey>) -> Result<Endpoint> {
    // ... existing transport config ...

    let mut builder = Endpoint::builder(presets::N0)
        .alpns(vec![NAFAQ_ALPN.to_vec()])
        .transport_config(transport_config);

    if let Some(key) = secret_key {
        builder = builder.secret_key(key);
    }

    let endpoint = builder.bind().await?;
    endpoint.online().await;
    tracing::info!("Iroh endpoint started with ID: {}", endpoint.id());
    Ok(endpoint)
}
```

- [ ] **Step 2: Load/save key from store in `lib.rs`**

In `run()`, before creating the endpoint, check the store for a persisted key:

```rust
// In the rt.block_on async block:
let store = app_handle.store("settings.json").map_err(|e| e.to_string())?;
let persistent_identity = store.get("persistent_identity").and_then(|v| v.as_bool()).unwrap_or(false);
let secret_key = if persistent_identity {
    store.get("secret_key")
        .and_then(|v| v.as_str().map(String::from))
        .and_then(|hex| SecretKey::from_str(&hex).ok())
} else {
    None
};
let endpoint = node::create_endpoint_with_key(secret_key).await?;
```

Note: The store requires an `AppHandle`, so the endpoint creation must move into the `setup()` closure. Refactor accordingly.

- [ ] **Step 3: Add `toggle_persistent_identity` command**

In `commands.rs`:

```rust
#[tauri::command]
pub async fn toggle_persistent_identity(
    enabled: bool,
    state: State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    let store = app.store("settings.json").map_err(|e| e.to_string())?;
    if enabled {
        let key = state.endpoint.secret_key();
        store.set("secret_key", serde_json::Value::String(key.to_string()));
        store.set("persistent_identity", serde_json::Value::Bool(true));
    } else {
        store.delete("secret_key");
        store.set("persistent_identity", serde_json::Value::Bool(false));
    }
    store.save().map_err(|e| e.to_string())?;
    Ok(())
}
```

- [ ] **Step 4: Register the command in `lib.rs`**

Add `commands::toggle_persistent_identity` to the `invoke_handler` list.

- [ ] **Step 5: Run `cargo test` and `cargo build`**

```bash
cd src-tauri && cargo test && cargo build
```

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/node.rs src-tauri/src/lib.rs src-tauri/src/commands.rs
git commit -m "feat: persistent Iroh identity via secret key storage"
```

---

### Task 9: Settings Composable

**Files:**
- Create: `app/composables/useSettings.ts`

- [ ] **Step 1: Create the composable**

```typescript
export interface AppSettings {
  displayName: string;
  persistentIdentity: boolean;
  nodeId: string | null;
  preferredMic: string | null;
  preferredCamera: string | null;
  preferredSpeaker: string | null;
  videoQuality: "auto" | "low" | "medium" | "high";
  dataSaver: boolean;
}

const settings = ref<AppSettings>({
  displayName: "",
  persistentIdentity: false,
  nodeId: null,
  preferredMic: null,
  preferredCamera: null,
  preferredSpeaker: null,
  videoQuality: "auto",
  dataSaver: false,
});

const loaded = ref(false);

export function useSettings() {
  async function load() {
    const { invoke } = await import("@tauri-apps/api/core");
    const stored = await invoke<Partial<AppSettings>>("get_settings").catch(() => ({}));
    Object.assign(settings.value, stored);
    loaded.value = true;
  }

  async function save(patch: Partial<AppSettings>) {
    Object.assign(settings.value, patch);
    const { invoke } = await import("@tauri-apps/api/core");
    await invoke("update_settings", { settings: patch }).catch(() => {});
  }

  async function togglePersistentIdentity(enabled: boolean) {
    const { invoke } = await import("@tauri-apps/api/core");
    await invoke("toggle_persistent_identity", { enabled });
    settings.value.persistentIdentity = enabled;
  }

  if (!loaded.value) load();

  return { settings, loaded, save, togglePersistentIdentity };
}
```

- [ ] **Step 2: Add `get_settings` and `update_settings` commands to Rust**

In `commands.rs`, read/write settings from the store:

```rust
#[tauri::command]
pub async fn get_settings(app: tauri::AppHandle) -> Result<serde_json::Value, String> {
    let store = app.store("settings.json").map_err(|e| e.to_string())?;
    let settings = store.get("app_settings").cloned().unwrap_or(serde_json::json!({}));
    Ok(settings)
}

#[tauri::command]
pub async fn update_settings(
    settings: serde_json::Value,
    app: tauri::AppHandle,
) -> Result<(), String> {
    let store = app.store("settings.json").map_err(|e| e.to_string())?;
    let mut current = store.get("app_settings").cloned().unwrap_or(serde_json::json!({}));
    if let (Some(current_obj), Some(patch)) = (current.as_object_mut(), settings.as_object()) {
        for (k, v) in patch {
            current_obj.insert(k.clone(), v.clone());
        }
    }
    store.set("app_settings", current);
    store.save().map_err(|e| e.to_string())?;
    Ok(())
}
```

Register both in `invoke_handler`.

- [ ] **Step 3: Commit**

```bash
git add app/composables/useSettings.ts src-tauri/src/commands.rs src-tauri/src/lib.rs
git commit -m "feat: settings composable with Rust store backend"
```

---

### Task 10: Settings Page

**Files:**
- Create: `app/pages/settings.vue`

- [ ] **Step 1: Create the Settings page**

Build the page with 4 sections matching the mockup: Identity, Devices, Call Quality, About. Use `useSettings()` for state and `useMedia()` for device enumeration.

Key sections:
- **Identity:** Display name input, node ID display with copy/QR buttons, persistent identity toggle
- **Devices:** Microphone/camera/speaker dropdowns populated from `useMedia().devices`
- **Call Quality:** Video quality dropdown (Auto/Low/Medium/High), data saver toggle
- **About:** Version from `useRuntimeConfig().public.appVersion`, Iroh version hardcoded

Style with the existing brutalist design system: monospace font, no rounded corners, uppercase labels, border-based layout.

- [ ] **Step 2: Add back navigation**

The settings page is accessed via gear icon and returns via a back arrow. Use `navigateTo("/")` or `router.back()`.

- [ ] **Step 3: Test navigation to/from settings**

```bash
bun run tauri:dev
```

Navigate to settings, change a value, go back, verify it persists.

- [ ] **Step 4: Commit**

```bash
git add app/pages/settings.vue
git commit -m "feat: settings page — identity, devices, quality, about"
```

---

## Phase 3: Contacts & Presence

### Task 11: Contacts Backend (Rust)

**Files:**
- Modify: `src-tauri/src/commands.rs`
- Modify: `src-tauri/src/messages.rs`
- Modify: `src-tauri/src/lib.rs`

- [ ] **Step 1: Define `Contact` struct in `messages.rs`**

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Contact {
    pub node_id: String,
    pub display_name: String,
    pub added_at: u64,
    pub last_seen: u64,
    pub source: ContactSource,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContactSource {
    Call,
    Manual,
}
```

- [ ] **Step 2: Add contact CRUD commands**

In `commands.rs`:

```rust
#[tauri::command]
pub async fn get_contacts(app: tauri::AppHandle) -> Result<Vec<Contact>, String> {
    let store = app.store("contacts.json").map_err(|e| e.to_string())?;
    let contacts: Vec<Contact> = store.get("contacts")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();
    Ok(contacts)
}

#[tauri::command]
pub async fn add_contact(contact: Contact, app: tauri::AppHandle) -> Result<(), String> {
    let store = app.store("contacts.json").map_err(|e| e.to_string())?;
    let mut contacts: Vec<Contact> = store.get("contacts")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();
    // Upsert by node_id
    if let Some(existing) = contacts.iter_mut().find(|c| c.node_id == contact.node_id) {
        existing.display_name = contact.display_name;
        existing.last_seen = contact.last_seen;
    } else {
        contacts.push(contact);
    }
    store.set("contacts", serde_json::to_value(&contacts).unwrap());
    store.save().map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn remove_contact(node_id: String, app: tauri::AppHandle) -> Result<(), String> {
    let store = app.store("contacts.json").map_err(|e| e.to_string())?;
    let mut contacts: Vec<Contact> = store.get("contacts")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();
    contacts.retain(|c| c.node_id != node_id);
    store.set("contacts", serde_json::to_value(&contacts).unwrap());
    store.save().map_err(|e| e.to_string())?;
    Ok(())
}
```

- [ ] **Step 3: Register commands in `lib.rs`**

Add `commands::get_contacts`, `commands::add_contact`, `commands::remove_contact` to `invoke_handler`.

- [ ] **Step 4: Run `cargo test` and `cargo build`**

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/commands.rs src-tauri/src/messages.rs src-tauri/src/lib.rs
git commit -m "feat: contact CRUD commands with persistent store"
```

---

### Task 12: Contacts Composable

**Files:**
- Create: `app/composables/useContacts.ts`

- [ ] **Step 1: Create the composable**

```typescript
export interface Contact {
  node_id: string;
  display_name: string;
  added_at: number;
  last_seen: number;
  source: "call" | "manual";
}

const contacts = ref<Contact[]>([]);
const loaded = ref(false);

export function useContacts() {
  async function load() {
    const { invoke } = await import("@tauri-apps/api/core");
    contacts.value = await invoke<Contact[]>("get_contacts").catch(() => []);
    loaded.value = true;
  }

  async function add(contact: Contact) {
    const { invoke } = await import("@tauri-apps/api/core");
    await invoke("add_contact", { contact });
    await load();
  }

  async function remove(nodeId: string) {
    const { invoke } = await import("@tauri-apps/api/core");
    await invoke("remove_contact", { nodeId });
    contacts.value = contacts.value.filter(c => c.node_id !== nodeId);
  }

  async function starFromCall(nodeId: string, displayName: string) {
    await add({
      node_id: nodeId,
      display_name: displayName,
      added_at: Date.now(),
      last_seen: Date.now(),
      source: "call",
    });
  }

  if (!loaded.value) load();

  return { contacts, loaded, add, remove, starFromCall };
}
```

- [ ] **Step 2: Commit**

```bash
git add app/composables/useContacts.ts
git commit -m "feat: contacts composable — load, add, remove, starFromCall"
```

---

### Task 13: Presence Probing

**Files:**
- Create: `app/composables/usePresence.ts`
- Modify: `src-tauri/src/commands.rs`
- Modify: `src-tauri/src/lib.rs`

- [ ] **Step 1: Add `check_presence` Rust command**

In `commands.rs`:

```rust
#[tauri::command]
pub async fn check_presence(
    node_id: String,
    state: State<'_, AppState>,
) -> Result<bool, String> {
    let addr = iroh::EndpointAddr::from_node_id(
        node_id.parse().map_err(|e: anyhow::Error| e.to_string())?,
    );
    // Try to connect with a 5-second timeout
    match tokio::time::timeout(
        std::time::Duration::from_secs(5),
        state.endpoint.connect(addr, crate::node::NAFAQ_ALPN),
    ).await {
        Ok(Ok(conn)) => {
            conn.close(0u32.into(), b"presence_probe");
            Ok(true)
        }
        _ => Ok(false),
    }
}
```

Register in `invoke_handler`.

- [ ] **Step 2: Create presence composable**

```typescript
const onlineStatus = ref<Record<string, boolean>>({});

export function usePresence() {
  let probeInterval: ReturnType<typeof setInterval> | null = null;

  async function probeAll(contacts: Contact[]) {
    const { invoke } = await import("@tauri-apps/api/core");
    for (const contact of contacts) {
      const online = await invoke<boolean>("check_presence", { nodeId: contact.node_id }).catch(() => false);
      onlineStatus.value = { ...onlineStatus.value, [contact.node_id]: online };
    }
  }

  function startProbing(contacts: Ref<Contact[]>) {
    probeAll(contacts.value);
    probeInterval = setInterval(() => probeAll(contacts.value), 30_000);
  }

  function stopProbing() {
    if (probeInterval) {
      clearInterval(probeInterval);
      probeInterval = null;
    }
  }

  function isOnline(nodeId: string): boolean {
    return onlineStatus.value[nodeId] ?? false;
  }

  return { onlineStatus, startProbing, stopProbing, isOnline };
}
```

- [ ] **Step 3: Commit**

```bash
git add app/composables/usePresence.ts src-tauri/src/commands.rs src-tauri/src/lib.rs
git commit -m "feat: P2P presence probing for contacts"
```

---

### Task 14: Contacts Page

**Files:**
- Create: `app/pages/contacts.vue`
- Create: `app/components/AddContactModal.vue`

- [ ] **Step 1: Create the Contacts page**

Build the page matching the approved mockup:
- Simplified identity card at top (name, truncated node ID, QR/copy buttons)
- Contact list with online status indicators, per-contact message/call action buttons
- `+ ADD` button that opens `AddContactModal`
- Online contacts' action buttons active, offline contacts' buttons greyed (opacity 0.4)

Use `useContacts()`, `usePresence()`, and `useCall()` composables.

- [ ] **Step 2: Create AddContactModal**

Modal with two entry methods:
- Paste node ID text field
- QR scanner button (reuse existing `QrScanner.vue`)
- Display name input
- Save button that calls `useContacts().add()`

- [ ] **Step 3: Add star button to call.vue peer tiles**

In `pages/call.vue`, add a star icon button on each peer's video tile. On click, call `useContacts().starFromCall(peerId, peerName)`.

- [ ] **Step 4: Test the full contacts flow**

Create a contact manually, verify it appears. Start a call with a peer, star them, verify they appear in contacts. Delete a contact.

- [ ] **Step 5: Commit**

```bash
git add app/pages/contacts.vue app/components/AddContactModal.vue app/pages/call.vue
git commit -m "feat: contacts page with add/remove and star-from-call"
```

---

## Phase 4: DMs & File Transfer

### Task 15: DM Stream — Rust Backend

**Files:**
- Modify: `src-tauri/src/messages.rs`
- Modify: `src-tauri/src/connection.rs`
- Modify: `src-tauri/src/commands.rs`
- Modify: `src-tauri/src/lib.rs`
- Modify: `src-tauri/Cargo.toml`

- [ ] **Step 1: Add DM stream constant and message types**

In `messages.rs`:

```rust
pub const STREAM_DM: u8 = 0x05;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DmMessage {
    Text { content: String, timestamp: u64 },
    FileStart { name: String, size: u64, id: String },
    FileChunk { id: String, offset: u64, #[serde(with = "serde_bytes")] data: Vec<u8> },
    FileEnd { id: String },
    CallInvite,
    CallAccept,
    CallDecline,
    Heartbeat,
}
```

Add `serde_bytes = "0.11"` to `Cargo.toml` if needed, or use base64 encoding for binary data in JSON. Also add `uuid = { version = "1", features = ["v4"] }`.

- [ ] **Step 2: Add DM events**

In `messages.rs`, extend the `Event` enum:

```rust
// Add to Event enum:
DmReceived {
    peer_id: String,
    message: DmMessage,
},
DmConnected {
    peer_id: String,
},
DmDisconnected {
    peer_id: String,
},
CallInviteReceived {
    peer_id: String,
},
```

- [ ] **Step 3: Add DM connection management in `connection.rs`**

Add a separate `dm_connections` map alongside `peers`. DM connections only open the DM stream (0x05), not audio/video/control. Add methods:

```rust
pub async fn connect_dm(&self, node_id: &str) -> Result<()> {
    // Parse node_id, connect via endpoint, open only DM stream
    // Start DM stream reader task
    // Store in dm_connections map
}

pub async fn send_dm(&self, peer_id: &str, message: &DmMessage) -> Result<()> {
    // Serialize message as JSON, write framed to DM stream
}

pub async fn disconnect_dm(&self, peer_id: &str) {
    // Close DM connection
}
```

- [ ] **Step 4: Handle DM stream in stream receivers**

In `spawn_stream_receivers`, add a handler for `STREAM_DM` (0x05) that reads framed JSON messages and broadcasts them as `Event::DmReceived`.

- [ ] **Step 5: Add DM Tauri commands**

In `commands.rs`:

```rust
#[tauri::command]
pub async fn connect_dm(node_id: String, state: State<'_, AppState>) -> Result<(), String> {
    state.conn_manager.connect_dm(&node_id).await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn send_dm(peer_id: String, message: serde_json::Value, state: State<'_, AppState>) -> Result<(), String> {
    let dm_msg: DmMessage = serde_json::from_value(message).map_err(|e| e.to_string())?;
    state.conn_manager.send_dm(&peer_id, &dm_msg).await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn disconnect_dm(peer_id: String, state: State<'_, AppState>) -> Result<(), String> {
    state.conn_manager.disconnect_dm(&peer_id).await;
    Ok(())
}
```

Register all in `invoke_handler`.

- [ ] **Step 6: Forward DM events in `lib.rs`**

In the event forwarder task, add handlers for the new DM events:

```rust
Event::DmReceived { .. } => "dm-received",
Event::DmConnected { .. } => "dm-connected",
Event::DmDisconnected { .. } => "dm-disconnected",
Event::CallInviteReceived { .. } => "call-invite-received",
```

- [ ] **Step 7: Run `cargo test` and `cargo build`**

- [ ] **Step 8: Commit**

```bash
git add src-tauri/src/messages.rs src-tauri/src/connection.rs src-tauri/src/commands.rs src-tauri/src/lib.rs src-tauri/Cargo.toml
git commit -m "feat: DM QUIC stream (0x05) with text and file transfer protocol"
```

---

### Task 16: File Transfer — Rust Backend

**Files:**
- Modify: `src-tauri/src/commands.rs`
- Modify: `src-tauri/src/connection.rs`

- [ ] **Step 1: Add `send_file` command**

```rust
#[tauri::command]
pub async fn send_file(
    peer_id: String,
    file_path: String,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let path = std::path::PathBuf::from(&file_path);
    let name = path.file_name()
        .and_then(|n| n.to_str())
        .ok_or("Invalid file name")?
        .to_string();
    let metadata = tokio::fs::metadata(&path).await.map_err(|e| e.to_string())?;
    let size = metadata.len();
    let id = uuid::Uuid::new_v4().to_string();

    // Send FileStart
    state.conn_manager.send_dm(&peer_id, &DmMessage::FileStart {
        name, size, id: id.clone(),
    }).await.map_err(|e| e.to_string())?;

    // Stream file in 64KB chunks
    let mut file = tokio::fs::File::open(&path).await.map_err(|e| e.to_string())?;
    let mut offset = 0u64;
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = tokio::io::AsyncReadExt::read(&mut file, &mut buf).await.map_err(|e| e.to_string())?;
        if n == 0 { break; }
        state.conn_manager.send_dm(&peer_id, &DmMessage::FileChunk {
            id: id.clone(), offset, data: buf[..n].to_vec(),
        }).await.map_err(|e| e.to_string())?;
        offset += n as u64;
    }

    // Send FileEnd
    state.conn_manager.send_dm(&peer_id, &DmMessage::FileEnd { id: id.clone() })
        .await.map_err(|e| e.to_string())?;

    Ok(id)
}
```

Register in `invoke_handler`.

- [ ] **Step 2: Handle incoming file chunks in the DM receiver**

In the DM stream reader in `connection.rs`, when receiving `FileStart`, create a temp file. On `FileChunk`, write to it. On `FileEnd`, move to downloads directory and emit a completion event.

- [ ] **Step 3: Run `cargo build`**

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/commands.rs src-tauri/src/connection.rs
git commit -m "feat: chunked file transfer over DM stream"
```

---

### Task 17: DM Composable

**Files:**
- Create: `app/composables/useDM.ts`

- [ ] **Step 1: Create the composable**

```typescript
export interface DmTextMessage {
  type: "text";
  content: string;
  timestamp: number;
  from: "self" | "peer";
}

export interface DmFileMessage {
  type: "file";
  name: string;
  size: number;
  id: string;
  progress: number; // 0-1
  localPath: string | null;
  from: "self" | "peer";
  timestamp: number;
}

export type DmMessageItem = DmTextMessage | DmFileMessage;

const conversations = ref<Record<string, DmMessageItem[]>>({});
const activeConversation = ref<string | null>(null);
const unreadCounts = ref<Record<string, number>>({});

export function useDM() {
  async function connect(nodeId: string) {
    const { invoke } = await import("@tauri-apps/api/core");
    await invoke("connect_dm", { nodeId });
    activeConversation.value = nodeId;
  }

  async function disconnect() {
    if (!activeConversation.value) return;
    const { invoke } = await import("@tauri-apps/api/core");
    await invoke("disconnect_dm", { peerId: activeConversation.value }).catch(() => {});
    activeConversation.value = null;
  }

  async function sendText(nodeId: string, content: string) {
    const { invoke } = await import("@tauri-apps/api/core");
    const timestamp = Date.now();
    await invoke("send_dm", {
      peerId: nodeId,
      message: { type: "text", content, timestamp },
    });
    pushMessage(nodeId, { type: "text", content, timestamp, from: "self" });
  }

  async function sendFile(nodeId: string, filePath: string) {
    const { invoke } = await import("@tauri-apps/api/core");
    const name = filePath.split(/[/\\]/).pop() || "file";
    const id = await invoke<string>("send_file", { peerId: nodeId, filePath });
    pushMessage(nodeId, {
      type: "file", name, size: 0, id, progress: 0,
      localPath: filePath, from: "self", timestamp: Date.now(),
    });
  }

  function pushMessage(nodeId: string, msg: DmMessageItem) {
    if (!conversations.value[nodeId]) {
      conversations.value[nodeId] = [];
    }
    conversations.value[nodeId].push(msg);
    if (activeConversation.value !== nodeId) {
      unreadCounts.value[nodeId] = (unreadCounts.value[nodeId] || 0) + 1;
    }
  }

  function markRead(nodeId: string) {
    unreadCounts.value[nodeId] = 0;
  }

  function totalUnread(): number {
    return Object.values(unreadCounts.value).reduce((a, b) => a + b, 0);
  }

  return {
    conversations, activeConversation, unreadCounts,
    connect, disconnect, sendText, sendFile,
    pushMessage, markRead, totalUnread,
  };
}
```

- [ ] **Step 2: Listen for DM events**

Initialize a listener for `dm-received` Tauri events that calls `pushMessage` with `from: "peer"`.

- [ ] **Step 3: Persist DM history to store**

Debounce-save conversations to `tauri-plugin-store` keyed by `dm-history.json`. Load on init.

- [ ] **Step 4: Commit**

```bash
git add app/composables/useDM.ts
git commit -m "feat: DM composable — connect, send text/file, conversation state"
```

---

### Task 18: Messages Page & DM Conversation View

**Files:**
- Create: `app/pages/messages.vue`
- Create: `app/pages/dm/[nodeId].vue`
- Create: `app/components/FileMessage.vue`

- [ ] **Step 1: Create Messages page (conversation list)**

Shows all contacts with DM history, sorted by last message time. Each entry shows contact name, last message preview, timestamp, and unread badge. Tapping navigates to `/dm/[nodeId]`.

- [ ] **Step 2: Create DM conversation view**

Full chat view matching the approved mockup:
- Header: back arrow, contact name, online status, CALL button
- Message list: text bubbles (left for peer, right for self), file messages with `FileMessage` component
- Input bar: paperclip attach button (opens file dialog via `@tauri-apps/plugin-dialog`), text input, send button

Connect to the peer on mount via `useDM().connect(nodeId)`. Disconnect on unmount.

- [ ] **Step 3: Create FileMessage component**

Displays file attachment with name, size, progress bar (during transfer), and SAVE button (after completion). Styled with violet accent border per the mockup.

- [ ] **Step 4: Implement call escalation**

The CALL button in the DM header sends a `call_invite` message via the DM stream. Listen for `call-invite-received` event and show an incoming call modal. On accept, navigate to `/call` — the existing QUIC connection gets upgraded with audio/video/control streams.

- [ ] **Step 5: Test the full DM flow**

Two instances: connect as DM, send text back and forth, send a file, verify it arrives and is saveable. Test call escalation from DM.

- [ ] **Step 6: Commit**

```bash
git add app/pages/messages.vue app/pages/dm/ app/components/FileMessage.vue
git commit -m "feat: messages page, DM conversation view, file transfer UI"
```

---

## Phase 5: Navigation & UX Polish

### Task 19: Tab Bar Navigation

**Files:**
- Create: `app/components/TabBar.vue`
- Modify: `app/app.vue`
- Modify: `app/pages/index.vue`

- [ ] **Step 1: Create TabBar component**

Bottom tab bar with 3 tabs: Calls (phone icon), Contacts (star icon), Messages (envelope icon). Active tab highlighted with `--color-accent`. Messages tab shows unread badge from `useDM().totalUnread()`. Gear icon in the right side of the header for Settings.

```vue
<template>
  <div class="fixed bottom-0 left-0 right-0 border-t-2 border-[var(--color-border)] bg-black z-50 safe-area-inset">
    <div class="grid grid-cols-3 text-center">
      <NuxtLink to="/" class="tab-item" :class="{ active: route.path === '/' }">
        <span class="text-sm">&#9742;</span>
        <span class="label text-[10px]">CALLS</span>
      </NuxtLink>
      <NuxtLink to="/contacts" class="tab-item" :class="{ active: route.path === '/contacts' }">
        <span class="text-sm">&#9733;</span>
        <span class="label text-[10px]">CONTACTS</span>
      </NuxtLink>
      <NuxtLink to="/messages" class="tab-item" :class="{ active: route.path === '/messages' }">
        <span class="text-sm relative">
          &#9993;
          <span v-if="unread > 0" class="unread-badge">{{ unread }}</span>
        </span>
        <span class="label text-[10px]">MESSAGES</span>
      </NuxtLink>
    </div>
  </div>
</template>
```

- [ ] **Step 2: Integrate into `app.vue`**

Wrap `NuxtPage` with a layout that includes the TabBar at the bottom and a header with a gear icon. Hide the TabBar when on `/call` or `/settings` pages.

```vue
<template>
  <UApp>
    <div class="min-h-screen flex flex-col">
      <header v-if="showNav" class="flex items-center justify-between px-4 py-3 border-b-2 border-[var(--color-border)]">
        <span class="font-black tracking-[4px] text-lg">NAFAQ</span>
        <NuxtLink to="/settings" class="text-lg text-[var(--color-muted)]">&#9881;</NuxtLink>
      </header>
      <main class="flex-1" :class="{ 'pb-16': showNav }">
        <NuxtPage />
      </main>
      <TabBar v-if="showNav" />
    </div>
  </UApp>
</template>
```

- [ ] **Step 3: Adjust `index.vue`**

Remove the `NAFAQ` header and version footer from `index.vue` since they now live in `app.vue`. The page becomes just the call creation/join content.

- [ ] **Step 4: Test tab navigation**

Navigate between all 3 tabs. Verify active state, unread badge, gear icon. Verify tab bar hides during calls.

- [ ] **Step 5: Commit**

```bash
git add app/components/TabBar.vue app/app.vue app/pages/index.vue
git commit -m "feat: bottom tab bar navigation — Calls, Contacts, Messages"
```

---

### Task 20: VU Meter in Pre-Call Overlay

**Files:**
- Modify: `app/composables/useMedia.ts`
- Modify: `app/components/PreCallOverlay.vue`

- [ ] **Step 1: Expose mic level from `useMedia`**

The `useMedia` composable already has an `AnalyserNode` for mic level visualization. Expose a reactive `micLevel` ref (0.0 to 1.0) that updates on a `requestAnimationFrame` loop when the stream is active.

- [ ] **Step 2: Add VU meter to PreCallOverlay**

Display a horizontal bar under the camera preview that shows the mic level. Use a `<div>` with width bound to `micLevel * 100%` and a purple background.

- [ ] **Step 3: Commit**

```bash
git add app/composables/useMedia.ts app/components/PreCallOverlay.vue
git commit -m "feat: mic level VU meter in pre-call overlay"
```

---

### Task 21: Mobile UX Fixes

**Files:**
- Modify: `app/pages/call.vue`
- Modify: `app/components/ChatSidebar.vue`

- [ ] **Step 1: Make fullscreen button visible on mobile**

Remove the `hidden sm:block` class from the fullscreen button in `call.vue`. Add a touch-friendly size.

- [ ] **Step 2: Fix chat overlay for soft keyboard**

In `ChatSidebar.vue`, use the `visualViewport` API to adjust the chat overlay height when the soft keyboard appears:

```typescript
onMounted(() => {
  if (window.visualViewport) {
    window.visualViewport.addEventListener("resize", () => {
      const vh = window.visualViewport!.height;
      document.documentElement.style.setProperty("--vh", `${vh}px`);
    });
  }
});
```

Use `height: var(--vh, 100vh)` on the chat overlay.

- [ ] **Step 3: Use 24-hour timestamps**

Replace all `toLocaleTimeString` calls with explicit 24-hour formatting:

```typescript
function formatTime(ts: number): string {
  const d = new Date(ts);
  return `${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}`;
}
```

- [ ] **Step 4: Test on Android emulator**

```bash
bun run tauri:android:dev
```

Verify tab bar, fullscreen, keyboard handling, and timestamps.

- [ ] **Step 5: Commit**

```bash
git add app/pages/call.vue app/components/ChatSidebar.vue
git commit -m "fix: mobile UX — fullscreen, keyboard, 24h timestamps"
```

---

## Verification Checklist

After all tasks are complete, run through these end-to-end tests:

- [ ] **WebCodecs path:** 2-peer call on Windows/Android. Rust CPU near-zero for video decode.
- [ ] **Fallback path:** 2-peer call on older macOS. Dedicated runtime active, audio smooth.
- [ ] **Group scaling:** 4-peer call. Per-peer quality adapts. Off-screen tile stops decoding.
- [ ] **Persistent identity:** Enable toggle, restart app, node ID unchanged. Disable, restart, ID changes.
- [ ] **Contacts:** Star from call + manual add. Online status shows correctly.
- [ ] **DMs:** Text messages both directions. File transfer with progress. Save works.
- [ ] **Call escalation:** Click CALL in DM header. Both sides enter call without ticket exchange.
- [ ] **Settings:** Device changes applied to next call. Data saver reduces quality.
- [ ] **Tab navigation:** All 3 tabs work. Unread badge on Messages. Gear opens Settings.
- [ ] **Mobile:** Android — tab bar, fullscreen, keyboard, 24h timestamps all work.
