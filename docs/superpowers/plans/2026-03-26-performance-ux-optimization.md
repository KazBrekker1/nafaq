# Performance & UX Optimization Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix call quality degradation at 3+ peers via runtime isolation and adaptive quality, and redesign the connection flow with inline lobby and rich feedback.

**Architecture:** Separate the tokio runtime for video decode from the main Tauri runtime so audio is never starved. Add adaptive quality profiles that scale bitrate/FPS/resolution based on peer count. Replace the full-page lobby with an inline pre-call overlay modal, add connection progress feedback, and make display names ephemeral with an optional pin-to-persist feature.

**Tech Stack:** Rust (tokio, openh264, opus, tauri, tauri-plugin-store), Vue 3 / Nuxt 4, Nuxt UI, TypeScript

---

## File Map

### Rust (`src-tauri/src/`)

| File | Action | Responsibility |
|------|--------|---------------|
| `codec.rs` | Modify | Split `CodecState` into `AudioCodecState` + `VideoCodecState`; add `reinit_video_encoder_with_config` for adaptive bitrate |
| `state.rs` | Modify | Update `AppState` to hold separate codec states and video runtime handle |
| `lib.rs` | Modify | Create dedicated video runtime; move video forwarder to it; increase audio channel buffer; emit quality-profile events |
| `commands.rs` | Modify | Update codec command signatures for split state; add `reinit_video_encoder_with_config`; add `get_pinned_name`/`set_pinned_name` commands |
| `connection.rs` | Modify | Add `peer_count()` method; emit `quality-profile-changed` event on peer count change |
| `messages.rs` | Modify | Add `QualityProfileChanged` event variant |
| `Cargo.toml` | Modify | Add `tauri-plugin-store` dependency |

### Frontend (`app/`)

| File | Action | Responsibility |
|------|--------|---------------|
| `composables/useCall.ts` | Modify | Add connection progress states; remove auto-nav to lobby/call; add pre-call overlay flow; handle last-peer-left prompt instead of auto-redirect |
| `composables/useMediaTransport.ts` | Modify | Listen for `quality-profile-changed`; update `targetFps` and capture dimensions based on profile |
| `composables/useMedia.ts` | Modify | Add method to update capture constraints from quality profile |
| `pages/index.vue` | Modify | Add pre-call overlay; connection progress UI; name pin toggle; remove lobby navigation |
| `pages/lobby.vue` | Delete | Replaced by inline pre-call overlay |
| `pages/call.vue` | Modify | Add disconnect toast; last-peer-left prompt; remove auto-redirect to home on disconnect |
| `components/PreCallOverlay.vue` | Create | Compact modal: camera preview, mic/cam toggles, "Join Call" button |
| `components/NameInput.vue` | Create | Name field with pin toggle backed by `tauri-plugin-store` |
| `components/DisconnectToast.vue` | Create | Toast notification for peer disconnect |

---

## Task 1: Split CodecState into AudioCodecState + VideoCodecState

**Files:**
- Modify: `src-tauri/src/codec.rs:253-276`
- Modify: `src-tauri/src/state.rs:1-33`
- Modify: `src-tauri/src/lib.rs:13,117-127`
- Modify: `src-tauri/src/commands.rs:298-331,335-481`

This task splits the codec state struct so video state can later be moved to a dedicated runtime. Pure refactor — no behavior change.

- [ ] **Step 1: Replace `CodecState` with `AudioCodecState` and `VideoCodecState` in `codec.rs`**

Replace lines 253-276 of `src-tauri/src/codec.rs`:

```rust
// ── AudioCodecState ─────────────────────────────────────────────────

pub struct AudioCodecState {
    pub encoder: tokio::sync::Mutex<Option<AudioEncoder>>,
    pub decoders: tokio::sync::Mutex<HashMap<String, AudioDecoder>>,
}

impl AudioCodecState {
    pub fn new() -> Self {
        Self {
            encoder: tokio::sync::Mutex::new(None),
            decoders: tokio::sync::Mutex::new(HashMap::new()),
        }
    }

    pub async fn remove_peer_decoders(&self, peer_id: &str) {
        self.decoders.lock().await.remove(peer_id);
    }
}

// ── VideoCodecState ─────────────────────────────────────────────────

pub struct VideoCodecState {
    pub encoder: tokio::sync::Mutex<Option<VideoEncoder>>,
    pub decoders: tokio::sync::Mutex<HashMap<String, VideoDecoder>>,
}

impl VideoCodecState {
    pub fn new() -> Self {
        Self {
            encoder: tokio::sync::Mutex::new(None),
            decoders: tokio::sync::Mutex::new(HashMap::new()),
        }
    }

    pub async fn remove_peer_decoders(&self, peer_id: &str) {
        self.decoders.lock().await.remove(peer_id);
    }
}
```

- [ ] **Step 2: Update `AppState` in `state.rs` to hold both codec states**

Replace the `codec` field in `state.rs`:

```rust
use crate::codec::{AudioCodecState, VideoCodecState};
// ... (remove CodecState import)

pub struct AppState {
    pub endpoint: iroh::Endpoint,
    pub router: Router,
    pub conn_manager: Arc<ConnectionManager>,
    pub event_tx: broadcast::Sender<Event>,
    pub audio_media_tx: broadcast::Sender<AudioPacket>,
    pub video_media_tx: broadcast::Sender<VideoPacket>,
    pub audio_codec: Arc<AudioCodecState>,
    pub video_codec: Arc<VideoCodecState>,
}
```

- [ ] **Step 3: Update `lib.rs` initialization to create both codec states**

Replace `lib.rs:117` (`let codec = Arc::new(CodecState::new());`) and the `AppState` construction (lines 120-128):

```rust
let audio_codec = Arc::new(AudioCodecState::new());
let video_codec = Arc::new(VideoCodecState::new());

let app_state = AppState {
    endpoint,
    router,
    conn_manager: conn_manager.clone(),
    event_tx: event_tx.clone(),
    audio_media_tx: audio_media_tx.clone(),
    video_media_tx: video_media_tx.clone(),
    audio_codec: audio_codec.clone(),
    video_codec: video_codec.clone(),
};
```

Update the audio forwarder (line 218, `let codec_audio = codec.clone();`) to use `audio_codec.clone()` and change `codec_audio.audio_decoders` to `codec_audio.decoders`.

Update the video forwarder (line 307, `let codec_video = codec.clone();`) to use `video_codec.clone()` and change `codec_video.video_decoders` to `codec_video.decoders`.

Update the disconnect cleanup (line 371, `let codec_cleanup = codec.clone();`) — it now needs both:

```rust
let audio_cleanup = audio_codec.clone();
let video_cleanup = video_codec.clone();
tauri::async_runtime::spawn(async move {
    loop {
        match disconnect_rx.recv().await {
            Ok(Event::PeerDisconnected { peer_id }) => {
                audio_cleanup.remove_peer_decoders(&peer_id).await;
                video_cleanup.remove_peer_decoders(&peer_id).await;
            }
            // ... rest unchanged
        }
    }
});
```

- [ ] **Step 4: Update all commands in `commands.rs` to use split state**

Replace `state.codec.audio_encoder` with `state.audio_codec.encoder`, `state.codec.video_encoder` with `state.video_codec.encoder`, etc. Specifically:

- `init_codecs` (line 298): `state.audio_codec.encoder` and `state.video_codec.encoder`
- `destroy_codecs` (line 312): all four fields via their respective states
- `reinit_video_encoder` (line 322): `state.video_codec.encoder`
- `encode_and_send_audio_all` (line 335): `state.audio_codec.encoder`
- `encode_and_send_video_all` (line 453): `state.video_codec.encoder`

Update imports at top of `commands.rs` to use `AudioCodecState` / `VideoCodecState` instead of `CodecState`.

- [ ] **Step 5: Update import in `lib.rs`**

Change line 13 from `use codec::{AudioDecoder, CodecState};` to `use codec::{AudioDecoder, AudioCodecState, VideoCodecState};`.

- [ ] **Step 6: Verify it compiles**

Run: `cd /Users/yousseifelshahawy/coding/personal/nafaq/src-tauri && cargo check`
Expected: No errors. This is a pure refactor — behavior is identical.

- [ ] **Step 7: Run existing tests**

Run: `cd /Users/yousseifelshahawy/coding/personal/nafaq/src-tauri && cargo test`
Expected: All tests pass.

- [ ] **Step 8: Commit**

```bash
git add src-tauri/src/codec.rs src-tauri/src/state.rs src-tauri/src/lib.rs src-tauri/src/commands.rs
git commit -m "refactor: split CodecState into AudioCodecState + VideoCodecState"
```

---

## Task 2: Dedicated Video Runtime + Audio Channel Buffer Increase

**Files:**
- Modify: `src-tauri/src/lib.rs:76-369`

Move the video forwarder task onto a dedicated `tokio::Runtime` so video decode never starves audio. Increase the audio broadcast channel from 64 to 256.

- [ ] **Step 1: Create dedicated video runtime in `lib.rs`**

After line 85 (`let rt = tauri::async_runtime::handle();`), add:

```rust
let video_runtime = tokio::runtime::Builder::new_multi_thread()
    .worker_threads(2)
    .thread_name("nafaq-video")
    .enable_all()
    .build()
    .expect("Failed to create video runtime");
```

- [ ] **Step 2: Increase audio broadcast channel capacity**

Change line 88 from:
```rust
let (audio_media_tx, _) = broadcast::channel::<AudioPacket>(64);
```
to:
```rust
let (audio_media_tx, _) = broadcast::channel::<AudioPacket>(256);
```

- [ ] **Step 3: Move video forwarder to dedicated runtime**

Replace the video forwarder spawn (lines 310-369) to use the video runtime instead of `tauri::async_runtime::spawn`:

```rust
// Spawn video forwarder on DEDICATED runtime (isolate from audio)
let app_handle_video = app.handle().clone();
let codec_video = video_codec.clone();
let video_bridge = media_bridge_ref.clone();

video_runtime.spawn(async move {
    let mut video_rx = video_media_tx_for_setup.subscribe();
    loop {
        match video_rx.recv().await {
            Ok(packet) => {
                let mut decoders = codec_video.decoders.lock().await;
                let decoder = decoders
                    .entry(packet.peer_id.clone())
                    .or_insert_with(codec::VideoDecoder::new);
                if let Some((rgba, width, height)) =
                    decoder.decode_rgba(&packet.payload)
                {
                    if let Some(jpeg) = codec::encode_jpeg(&rgba, width, height, 70) {
                        let registration = video_bridge.lock().await.clone();
                        if let Some(registration) = registration {
                            if let Some(channel) = registration.video_channel {
                                let Some(channel_payload) = pack_video_channel_packet(
                                    &packet.peer_id,
                                    packet.timestamp_ms,
                                    width,
                                    height,
                                    &jpeg,
                                ) else {
                                    continue;
                                };
                                let _ = channel.send(channel_payload);
                            } else {
                                let _ = app_handle_video.emit(
                                    "video-received",
                                    VideoEvent {
                                        peer_id: packet.peer_id.clone(),
                                        data: B64.encode(jpeg),
                                        width,
                                        height,
                                        timestamp: packet.timestamp_ms,
                                    },
                                );
                            }
                        } else {
                            let _ = app_handle_video.emit(
                                "video-received",
                                VideoEvent {
                                    peer_id: packet.peer_id.clone(),
                                    data: B64.encode(jpeg),
                                    width,
                                    height,
                                    timestamp: packet.timestamp_ms,
                                },
                            );
                        }
                    }
                }
            }
            Err(broadcast::error::RecvError::Lagged(n)) => {
                tracing::warn!("Video forwarder lagged by {n} frames");
            }
            Err(broadcast::error::RecvError::Closed) => break,
        }
    }
});
```

Note: `video_runtime` needs to be moved into the `.setup()` closure. Move the `let video_runtime = ...` line before `builder.setup(move |app| { ... })` and wrap it so it's captured by the closure.

- [ ] **Step 4: Verify it compiles**

Run: `cd /Users/yousseifelshahawy/coding/personal/nafaq/src-tauri && cargo check`
Expected: No errors.

- [ ] **Step 5: Run existing tests**

Run: `cd /Users/yousseifelshahawy/coding/personal/nafaq/src-tauri && cargo test`
Expected: All tests pass.

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/lib.rs
git commit -m "perf: dedicate video runtime + increase audio channel buffer to 256"
```

---

## Task 3: Adaptive Quality Profiles

**Files:**
- Modify: `src-tauri/src/messages.rs:133-164`
- Modify: `src-tauri/src/connection.rs:88-97`
- Modify: `src-tauri/src/lib.rs` (event forwarder)
- Modify: `src-tauri/src/commands.rs` (add `reinit_video_encoder_with_config`)
- Modify: `src-tauri/src/codec.rs:96-98` (already has `new_with_config`)
- Modify: `app/composables/useMediaTransport.ts:228-231,1063-1067`

When peer count crosses a threshold, emit a quality profile event. The frontend listens and adjusts capture dimensions, FPS, and re-initializes the video encoder with the new bitrate.

- [ ] **Step 1: Add `QualityProfileChanged` event variant in `messages.rs`**

Add to the `Event` enum (after `Error` variant, around line 162):

```rust
QualityProfileChanged {
    peer_count: usize,
    bitrate_bps: u32,
    fps: u32,
    max_width: u32,
    max_height: u32,
},
```

- [ ] **Step 2: Add `peer_count()` and quality profile emission to `ConnectionManager`**

In `connection.rs`, add a public method to `ConnectionManager`:

```rust
pub async fn peer_count(&self) -> usize {
    self.peers.lock().await.len()
}
```

Add a helper method that computes the quality profile from peer count:

```rust
pub fn quality_profile_for_peers(count: usize) -> (u32, u32, u32, u32) {
    // Returns (bitrate_bps, fps, max_width, max_height)
    match count {
        0..=2 => (400_000, 12, 640, 360),
        3 => (250_000, 10, 480, 270),
        _ => (150_000, 8, 320, 180),
    }
}
```

In the existing methods where peers are added/removed (`connect_to_peer` completion and disconnect handling), after modifying the peers map, emit the quality profile event via `event_tx`:

```rust
let count = self.peers.lock().await.len();
let (bitrate, fps, w, h) = Self::quality_profile_for_peers(count);
let _ = self.event_tx.send(Event::QualityProfileChanged {
    peer_count: count,
    bitrate_bps: bitrate,
    fps,
    max_width: w,
    max_height: h,
});
```

- [ ] **Step 3: Forward `QualityProfileChanged` event to frontend in `lib.rs`**

In the event forwarder (around line 149-157), add to the match:

```rust
Event::QualityProfileChanged { .. } => "quality-profile-changed",
```

- [ ] **Step 4: Add `reinit_video_encoder_with_config` command in `commands.rs`**

```rust
#[tauri::command]
pub async fn reinit_video_encoder_with_config(
    width: u32,
    height: u32,
    bitrate_bps: u32,
    fps: f32,
    state: State<'_, AppState>,
) -> Result<(), String> {
    validate_resolution(width, height)?;
    *state.video_codec.encoder.lock().await =
        Some(VideoEncoder::new_with_config(width, height, bitrate_bps, fps));
    tracing::info!(
        "Video encoder reinitialized: {width}x{height} @ {bitrate_bps}bps {fps}fps"
    );
    Ok(())
}
```

Register it in `lib.rs` invoke_handler list (around line 431).

- [ ] **Step 5: Listen for quality profile changes in `useMediaTransport.ts`**

In the `startReceiving` function (where `unlistenStats` is set up, around line 1055-1067), add a new listener:

```typescript
const { listen } = await import("@tauri-apps/api/event");
const unlistenQuality = await listen<{
  peer_count: number;
  bitrate_bps: number;
  fps: number;
  max_width: number;
  max_height: number;
}>("quality-profile-changed", async (event) => {
  const { bitrate_bps, fps, max_width, max_height } = event.payload;
  targetFps = fps;
  const { width, height } = resolveCaptureDimensions(activeCaptureStream, {
    maxWidth: max_width,
    maxHeight: max_height,
  });
  currentWidth = width;
  currentHeight = height;
  clearCaptureSurface();
  const invoke = await invokePromise;
  await invoke("reinit_video_encoder_with_config", {
    width,
    height,
    bitrateBps: bitrate_bps,
    fps: fps as number,
  });
});
```

Store `unlistenQuality` and call it in the cleanup/stop function alongside the other unlisten calls.

- [ ] **Step 6: Verify it compiles (Rust)**

Run: `cd /Users/yousseifelshahawy/coding/personal/nafaq/src-tauri && cargo check`
Expected: No errors.

- [ ] **Step 7: Run existing tests**

Run: `cd /Users/yousseifelshahawy/coding/personal/nafaq/src-tauri && cargo test`
Expected: All tests pass.

- [ ] **Step 8: Commit**

```bash
git add src-tauri/src/messages.rs src-tauri/src/connection.rs src-tauri/src/lib.rs src-tauri/src/commands.rs app/composables/useMediaTransport.ts
git commit -m "feat: adaptive quality profiles based on peer count"
```

---

## Task 4: Name Input with Pin Toggle

**Files:**
- Modify: `src-tauri/Cargo.toml`
- Modify: `src-tauri/src/lib.rs` (register plugin)
- Modify: `src-tauri/src/commands.rs` (add name store commands)
- Create: `app/components/NameInput.vue`
- Modify: `app/pages/index.vue`
- Modify: `app/composables/useCall.ts` (clear displayName default)

- [ ] **Step 1: Add `tauri-plugin-store` to `Cargo.toml`**

Add to `[dependencies]`:

```toml
tauri-plugin-store = "2"
```

- [ ] **Step 2: Register the store plugin in `lib.rs`**

After line 136 (`builder = builder.plugin(tauri_plugin_shell::init());`), add (outside the `#[cfg(desktop)]` block):

```rust
builder = builder.plugin(tauri_plugin_store::Builder::new().build());
```

- [ ] **Step 3: Add name store commands in `commands.rs`**

```rust
#[tauri::command]
pub async fn get_pinned_name(app: tauri::AppHandle) -> Result<Option<String>, String> {
    let store = app
        .store("settings.json")
        .map_err(|e| e.to_string())?;
    let pinned = store
        .get("name_pinned")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if !pinned {
        return Ok(None);
    }
    Ok(store
        .get("display_name")
        .and_then(|v| v.as_str().map(String::from)))
}

#[tauri::command]
pub async fn set_pinned_name(
    app: tauri::AppHandle,
    name: Option<String>,
    pinned: bool,
) -> Result<(), String> {
    let store = app
        .store("settings.json")
        .map_err(|e| e.to_string())?;
    store.set("name_pinned", serde_json::json!(pinned));
    if let Some(n) = name {
        store.set("display_name", serde_json::json!(n));
    }
    store.save().map_err(|e| e.to_string())
}
```

Register both in `lib.rs` invoke_handler list.

- [ ] **Step 4: Create `app/components/NameInput.vue`**

```vue
<script setup lang="ts">
const { modelValue } = defineProps<{ modelValue: string }>();
const emit = defineEmits<{
  "update:modelValue": [value: string];
}>();

const pinned = ref(false);
const loaded = ref(false);

onMounted(async () => {
  try {
    const { invoke } = await import("@tauri-apps/api/core");
    const savedName = await invoke<string | null>("get_pinned_name");
    if (savedName) {
      emit("update:modelValue", savedName);
      pinned.value = true;
    }
  } catch {}
  loaded.value = true;
});

async function togglePin() {
  pinned.value = !pinned.value;
  try {
    const { invoke } = await import("@tauri-apps/api/core");
    await invoke("set_pinned_name", {
      name: pinned.value ? modelValue : null,
      pinned: pinned.value,
    });
  } catch {}
}

function onInput(value: string) {
  emit("update:modelValue", value);
}

// Persist name when it changes and pin is active
watch(() => modelValue, async (name) => {
  if (!pinned.value || !loaded.value) return;
  try {
    const { invoke } = await import("@tauri-apps/api/core");
    await invoke("set_pinned_name", { name, pinned: true });
  } catch {}
});
</script>

<template>
  <div class="flex items-center gap-2">
    <UInput
      :model-value="modelValue"
      placeholder="Your name"
      class="flex-1 rounded-none text-sm text-center"
      @update:model-value="onInput"
    />
    <button
      class="w-8 h-8 flex items-center justify-center transition-colors"
      :class="pinned ? 'text-[var(--color-accent)]' : 'text-[var(--color-muted)] hover:text-[var(--color-border)]'"
      :title="pinned ? 'Name pinned — persists across sessions' : 'Pin name to remember it'"
      @click="togglePin"
    >
      <UIcon :name="pinned ? 'i-heroicons-lock-closed' : 'i-heroicons-lock-open'" class="text-sm" />
    </button>
  </div>
</template>
```

- [ ] **Step 5: Replace the name input in `pages/index.vue`**

Replace the name input section (lines 25-31):

```vue
<div class="mb-6 sm:mb-8">
  <NameInput v-model="displayName" />
</div>
```

- [ ] **Step 6: Clear displayName on app init in `useCall.ts`**

The `displayName` ref (line 12) is already initialized as `""`. No change needed — the `NameInput` component handles loading the pinned name on mount.

- [ ] **Step 7: Verify Rust compiles**

Run: `cd /Users/yousseifelshahawy/coding/personal/nafaq/src-tauri && cargo check`
Expected: No errors.

- [ ] **Step 8: Commit**

```bash
git add src-tauri/Cargo.toml src-tauri/src/lib.rs src-tauri/src/commands.rs app/components/NameInput.vue app/pages/index.vue
git commit -m "feat: ephemeral name input with optional pin-to-persist"
```

---

## Task 5: Connection Progress Feedback

**Files:**
- Modify: `app/composables/useCall.ts:85-154`
- Create: `app/components/ConnectionProgress.vue`
- Modify: `app/pages/index.vue`

Add visible progress at every stage: node init, connecting to peer, and connection established.

- [ ] **Step 1: Add connection progress state to `useCall.ts`**

Add new refs after line 11:

```typescript
const connectionProgress = ref<"idle" | "starting-node" | "node-ready" | "connecting" | "securing" | "connected">("idle");
```

Update `fetchNodeInfo` (lines 92-105) to emit progress:

```typescript
async function fetchNodeInfo() {
  connectionProgress.value = "starting-node";
  try {
    const info = await invoke<{ id: string; ticket: string }>("get_node_info");
    nodeId.value = info.id;
    shareTicket.value = info.ticket;
    nodeReady.value = true;
    connectionProgress.value = "node-ready";
  } catch {
    if (++retries < 15) {
      setTimeout(fetchNodeInfo, 2000);
    } else {
      error.value = "Could not start — check your network";
      connectionProgress.value = "idle";
    }
  }
}
```

Update `joinCall` (lines 39-51) to show connecting progress:

```typescript
async function joinCall(t: string) {
  error.value = null;
  state.value = "joining";
  ticket.value = t;
  connectionProgress.value = "connecting";
  try {
    const { invoke } = await import("@tauri-apps/api/core");
    connectionProgress.value = "securing";
    await invoke("join_call", { ticket: t });
    connectionProgress.value = "connected";
    // Don't navigate — show pre-call overlay on index page
  } catch (e) {
    error.value = `Failed to join: ${e}`;
    state.value = "idle";
    connectionProgress.value = "node-ready";
  }
}
```

Export `connectionProgress` in the return object.

- [ ] **Step 2: Create `app/components/ConnectionProgress.vue`**

```vue
<script setup lang="ts">
const { step } = defineProps<{
  step: "idle" | "starting-node" | "node-ready" | "connecting" | "securing" | "connected";
}>();

const steps = [
  { key: "starting-node", label: "Starting node..." },
  { key: "node-ready", label: "Node ready" },
  { key: "connecting", label: "Connecting..." },
  { key: "securing", label: "Establishing secure channel..." },
  { key: "connected", label: "Connected" },
] as const;

const activeIndex = computed(() =>
  steps.findIndex((s) => s.key === step)
);
</script>

<template>
  <div v-if="step !== 'idle'" class="flex items-center gap-2 text-xs">
    <div
      class="w-2 h-2 rounded-full"
      :class="step === 'starting-node' || step === 'connecting' || step === 'securing'
        ? 'bg-[var(--color-accent)] animate-pulse'
        : step === 'node-ready' || step === 'connected'
          ? 'bg-[var(--color-accent)]'
          : 'bg-[var(--color-muted)]'"
    />
    <span class="text-[var(--color-muted)] tracking-wider">
      {{ steps[activeIndex]?.label || "" }}
    </span>
  </div>
</template>
```

- [ ] **Step 3: Replace the node status indicator in `pages/index.vue`**

Replace lines 15-23 (the node status dot section):

```vue
<div class="flex items-center justify-center gap-2 mb-6 sm:mb-8">
  <ConnectionProgress :step="connectionProgress" />
  <span v-if="nodeId && connectionProgress === 'node-ready'" class="text-xs text-[var(--color-muted)]">
    · {{ nodeId.slice(0, 12) }}...
  </span>
</div>
```

Add `connectionProgress` to the destructured imports at the top of the `<script setup>`.

- [ ] **Step 4: Commit**

```bash
git add app/composables/useCall.ts app/components/ConnectionProgress.vue app/pages/index.vue
git commit -m "feat: connection progress feedback at every stage"
```

---

## Task 6: Inline Pre-Call Overlay (Replace Lobby Page)

**Files:**
- Create: `app/components/PreCallOverlay.vue`
- Modify: `app/composables/useCall.ts`
- Modify: `app/pages/index.vue`
- Delete: `app/pages/lobby.vue`

Replace the full-page lobby with a compact modal overlay. Both creator and joiner see it before entering the call.

- [ ] **Step 1: Add `showPreCallOverlay` state to `useCall.ts`**

Add ref:

```typescript
const showPreCallOverlay = ref(false);
```

Modify the `peer-connected` listener (lines 108-124). Remove `navigateTo("/call")` and instead show the overlay:

```typescript
listen<any>("peer-connected", async (event) => {
  const data = event.payload;
  const pid = typeof data === "string" ? data : data?.peer_id;
  if (pid && !peers.value.includes(pid)) {
    peers.value.push(pid);
  }
  peerId.value = pid;
  state.value = "connected";
  // Send our display name to the new peer
  if (displayName.value && pid) {
    invoke("send_control", {
      peerId: pid,
      action: { action: "set_display_name", name: displayName.value },
    }).catch(() => {});
  }
  // Show pre-call overlay instead of auto-navigating
  showPreCallOverlay.value = true;
});
```

Remove `navigateTo("/lobby")` from `joinCall` — the user stays on index with the overlay.

Add a `joinCallFromOverlay` method:

```typescript
function joinCallFromOverlay() {
  showPreCallOverlay.value = false;
  navigateTo("/call");
}
```

Export `showPreCallOverlay` and `joinCallFromOverlay`.

- [ ] **Step 2: Create `app/components/PreCallOverlay.vue`**

```vue
<script setup lang="ts">
const { open } = defineProps<{ open: boolean }>();
const emit = defineEmits<{ join: []; cancel: [] }>();

const media = useMedia();
const videoEl = ref<HTMLVideoElement | null>(null);

watch(
  [() => open, () => media.localStream.value],
  async ([isOpen, stream]) => {
    if (isOpen && !stream) {
      await media.startPreview();
    }
    if (videoEl.value) {
      videoEl.value.srcObject = media.localStream.value || null;
    }
  },
  { immediate: true },
);
</script>

<template>
  <UModal v-model:open="open" :closeable="false">
    <template #content>
      <div class="p-4 sm:p-6 space-y-4">
        <p class="label text-center">READY TO JOIN?</p>

        <!-- Camera preview thumbnail -->
        <div class="relative aspect-video bg-[#111] border border-[var(--color-border)] max-w-[320px] mx-auto overflow-hidden">
          <video ref="videoEl" autoplay muted playsinline class="w-full h-full object-contain bg-black" />
          <p v-if="!media.localStream.value" class="absolute inset-0 flex items-center justify-center text-[var(--color-muted)] text-xs">
            {{ media.error.value || "Starting camera..." }}
          </p>
        </div>

        <!-- Quick toggles -->
        <div class="flex justify-center gap-3">
          <button
            class="w-10 h-10 flex items-center justify-center border-2 transition-colors"
            :class="media.audioMuted.value
              ? 'border-[var(--color-danger)] bg-[var(--color-danger)] text-white'
              : 'border-[var(--color-border-muted)] text-[var(--color-border)] hover:bg-white/5'"
            @click="media.toggleAudio()"
          >
            <UIcon :name="media.audioMuted.value ? 'i-lucide-mic-off' : 'i-heroicons-microphone'" class="text-base" />
          </button>
          <button
            class="w-10 h-10 flex items-center justify-center border-2 transition-colors"
            :class="media.videoMuted.value
              ? 'border-[var(--color-danger)] bg-[var(--color-danger)] text-white'
              : 'border-[var(--color-border-muted)] text-[var(--color-border)] hover:bg-white/5'"
            @click="media.toggleVideo()"
          >
            <UIcon :name="media.videoMuted.value ? 'i-heroicons-video-camera-slash' : 'i-heroicons-video-camera'" class="text-base" />
          </button>
        </div>

        <!-- Actions -->
        <div class="flex gap-0">
          <UButton variant="outline" class="flex-1 rounded-none border-r-0" @click="emit('cancel')">Cancel</UButton>
          <UButton class="flex-1 rounded-none" @click="emit('join')">Join Call</UButton>
        </div>
      </div>
    </template>
  </UModal>
</template>
```

- [ ] **Step 3: Add PreCallOverlay to `pages/index.vue`**

Add to the template, after the closing `</div>` of the main container but before the final `</div>`:

```vue
<PreCallOverlay
  :open="showPreCallOverlay"
  @join="joinCallFromOverlay"
  @cancel="endCall"
/>
```

Destructure `showPreCallOverlay` and `joinCallFromOverlay` from `useCall()`.

- [ ] **Step 4: Delete `app/pages/lobby.vue`**

```bash
rm app/pages/lobby.vue
```

- [ ] **Step 5: Remove lobby guard from `call.vue`**

Update `call.vue` `onMounted` (line 73) — remove `if (call.state.value !== "connected") { navigateTo("/"); return; }` and replace with a softer guard that still allows the page to load if navigated to directly:

```typescript
onMounted(async () => {
  if (call.state.value !== "connected" || call.peers.value.length === 0) {
    navigateTo("/");
    return;
  }
  // ... rest unchanged
});
```

- [ ] **Step 6: Commit**

```bash
git add app/components/PreCallOverlay.vue app/composables/useCall.ts app/pages/index.vue app/pages/call.vue
git rm app/pages/lobby.vue
git commit -m "feat: replace lobby page with inline pre-call overlay"
```

---

## Task 7: Disconnect & Error Handling

**Files:**
- Create: `app/components/DisconnectToast.vue`
- Modify: `app/pages/call.vue`
- Modify: `app/composables/useCall.ts:126-137`

Replace silent auto-redirect with disconnect toasts and a last-peer-left prompt.

- [ ] **Step 1: Create `app/components/DisconnectToast.vue`**

```vue
<script setup lang="ts">
const { name } = defineProps<{ name: string }>();
const visible = ref(true);

onMounted(() => {
  setTimeout(() => { visible.value = false; }, 3000);
});
</script>

<template>
  <Transition name="fade">
    <div
      v-if="visible"
      class="fixed top-4 left-1/2 -translate-x-1/2 z-50 bg-black/90 border border-[var(--color-border-muted)] px-4 py-2 text-xs text-[var(--color-muted)] tracking-wider"
    >
      {{ name }} left the call
    </div>
  </Transition>
</template>

<style scoped>
.fade-enter-active, .fade-leave-active { transition: opacity 0.3s; }
.fade-enter-from, .fade-leave-to { opacity: 0; }
</style>
```

- [ ] **Step 2: Update `peer-disconnected` handler in `useCall.ts`**

Replace lines 126-137:

```typescript
const lastDisconnectedPeer = ref<{ id: string; name: string } | null>(null);
const allPeersLeft = ref(false);

listen<any>("peer-disconnected", (event) => {
  const data = event.payload;
  const pid = typeof data === "string" ? data : data?.peer_id;
  const peerName = peerNames.value[pid] || pid?.slice(0, 12) || "Peer";
  const idx = peers.value.indexOf(pid);
  if (idx >= 0) peers.value.splice(idx, 1);

  lastDisconnectedPeer.value = { id: pid, name: peerName };
  setTimeout(() => {
    if (lastDisconnectedPeer.value?.id === pid) {
      lastDisconnectedPeer.value = null;
    }
  }, 3500);

  if (peers.value.length === 0) {
    allPeersLeft.value = true;
    // Don't auto-redirect — show "everyone left" prompt
  }
});
```

Export `lastDisconnectedPeer` and `allPeersLeft`. Reset `allPeersLeft` to `false` in `peer-connected` handler and in `endCall`.

- [ ] **Step 3: Add disconnect toast and last-peer-left prompt to `call.vue`**

Add to the template, inside the main container:

```vue
<!-- Disconnect toast -->
<DisconnectToast
  v-if="call.lastDisconnectedPeer.value"
  :key="call.lastDisconnectedPeer.value.id"
  :name="call.lastDisconnectedPeer.value.name"
/>

<!-- Last peer left prompt -->
<div
  v-if="call.allPeersLeft.value"
  class="absolute inset-0 z-30 flex items-center justify-center bg-black/80"
>
  <div class="text-center space-y-4">
    <p class="text-sm text-[var(--color-muted)] tracking-wider">Everyone has left</p>
    <UButton class="rounded-none" @click="handleEndCall">Leave Call</UButton>
  </div>
</div>
```

Remove the existing auto-redirect in the `peers` watcher in `call.vue` — it should no longer navigate home when peers drop to 0 (that's handled by `useCall.ts` setting `allPeersLeft`).

- [ ] **Step 4: Handle reconnection when `allPeersLeft` is true**

In the `peer-connected` listener in `useCall.ts`, add:

```typescript
allPeersLeft.value = false;
```

This way if a new peer joins while the "everyone left" prompt is showing, the call resumes.

- [ ] **Step 5: Commit**

```bash
git add app/components/DisconnectToast.vue app/composables/useCall.ts app/pages/call.vue
git commit -m "feat: disconnect toasts and last-peer-left prompt"
```

---

## Task 8: Final Integration Verification

**Files:** All modified files

End-to-end verification that everything compiles and the existing test suite passes.

- [ ] **Step 1: Verify Rust compiles**

Run: `cd /Users/yousseifelshahawy/coding/personal/nafaq/src-tauri && cargo check`
Expected: No errors.

- [ ] **Step 2: Run Rust tests**

Run: `cd /Users/yousseifelshahawy/coding/personal/nafaq/src-tauri && cargo test`
Expected: All tests pass.

- [ ] **Step 3: Verify frontend builds**

Run: `cd /Users/yousseifelshahawy/coding/personal/nafaq && bun run build`
Expected: No build errors. No TypeScript errors.

- [ ] **Step 4: Verify no references to deleted lobby page**

Run: `grep -r "lobby" app/ --include="*.vue" --include="*.ts"`
Expected: No navigation to `/lobby` remains. Only historical references (if any) in comments.

- [ ] **Step 5: Final commit if any cleanup needed**

```bash
git add -A
git commit -m "chore: final integration cleanup"
```
