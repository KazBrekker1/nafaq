# Android Support + Rust Codec Pipeline — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make Nafaq run on Android by moving audio/video encoding from WebCodecs (JS) to Rust (libopus + libvpx), and setting up Android Tauri.

**Architecture:** A new `codec.rs` module handles all encoding/decoding in Rust. The frontend sends raw PCM/RGBA via invoke, Rust encodes and transmits over Iroh. On receive, Rust decodes and emits PCM/JPEG to JS. Android support follows the Meeqat project pattern (`tauri android init` + config overrides + permissions).

**Tech Stack:** Tauri 2, Rust (opus crate, libvpx-sys, image crate), Nuxt 4/Vue 3, Iroh 0.97, Android NDK (arm64-v8a)

**Spec:** `docs/superpowers/specs/2026-03-25-android-rust-codecs-design.md`

---

## File Map

| Action | Path | Responsibility |
|--------|------|----------------|
| Create | `src-tauri/src/codec.rs` | Opus + VP8 encode/decode, RGBA↔YUV420 conversion, JPEG re-encode |
| Modify | `src-tauri/Cargo.toml` | Add `opus`, `libvpx-sys`, `image` deps; move `tauri-plugin-shell` to desktop-only |
| Modify | `src-tauri/src/state.rs` | Add `CodecState` (Arc<Mutex<Option<AudioCodec/VideoCodec>>>) to `AppState` |
| Modify | `src-tauri/src/commands.rs` | Encode in `send_audio`/`send_video`; add `init_codecs`/`destroy_codecs` |
| Modify | `src-tauri/src/connection.rs` | Use `write_framed`/`read_framed` for media uni-streams |
| Modify | `src-tauri/src/lib.rs` | Decode in media forwarder; `#[cfg(desktop)]` for shell plugin; update `MediaEvent` |
| Rewrite | `app/composables/useMediaTransport.ts` | Remove WebCodecs; raw PCM + RGBA capture; JPEG video receive |
| Modify | `package.json` | Add `tauri:android:dev` and `tauri:android:build` scripts |
| Create | `src-tauri/tauri.android.conf.json` | Android window override |
| Create | `src-tauri/capabilities/mobile.json` | Android platform capabilities |
| Modify | `src-tauri/capabilities/main.json` | Add `platforms` filter for desktop |

---

## Task 1: Add Codec Dependencies + Validate Cross-Compilation

**Files:**
- Modify: `src-tauri/Cargo.toml`

This is Step 0 from the spec — validate that codec C libraries cross-compile for Android before writing any pipeline code.

- [ ] **Step 1: Add codec dependencies to Cargo.toml**

Add `opus`, `libvpx-sys`, and `image` to `[dependencies]`. Move `tauri-plugin-shell` to desktop-only section.

```toml
[dependencies]
tauri = { version = "2", features = [] }
tauri-plugin-shell = "2"
iroh = "0.97"
iroh-tickets = "0.4"
tokio = { version = "1", features = ["full"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
bytes = "1"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
anyhow = "1"
opus = "0.3"
libvpx-sys = "0.4"
image = { version = "0.25", default-features = false, features = ["jpeg"] }
```

Note: `tauri-plugin-shell` stays as an unconditional dep for now. It will be moved to desktop-only in Task 8 alongside the `#[cfg(desktop)]` wrapper in `lib.rs`, to avoid build breakage in intermediate tasks.

- [ ] **Step 2: Verify desktop build still compiles**

Run: `cd /Users/yousseifelshahawy/coding/personal/nafaq && cargo build --manifest-path src-tauri/Cargo.toml`

Expected: Build succeeds (may take a while to compile libopus and libvpx from vendored source).

If `libvpx-sys` fails to build, check error output. If it's a build-system issue (configure script fails), try setting `LIBVPX_NO_PKG_CONFIG=1`. If the crate fundamentally doesn't support the build, switch to `openh264` crate as the spec's fallback.

- [ ] **Step 3: Commit**

```bash
git add src-tauri/Cargo.toml
git commit -m "chore: add opus, libvpx-sys, image codec dependencies"
```

---

## Task 2: Rust AudioCodec — Encode + Decode + Tests

**Files:**
- Create: `src-tauri/src/codec.rs`

- [ ] **Step 1: Write failing audio codec tests**

Create `src-tauri/src/codec.rs` with test stubs and empty struct:

```rust
pub struct AudioCodec;

impl AudioCodec {
    pub fn new() -> Self { todo!() }
    pub fn encode(&mut self, _pcm: &[i16]) -> Option<Vec<u8>> { todo!() }
    pub fn decode(&mut self, _opus_data: &[u8]) -> Option<Vec<i16>> { todo!() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_audio_roundtrip() {
        let mut codec = AudioCodec::new();
        // 960 samples of a 440Hz sine wave at 48kHz
        let pcm: Vec<i16> = (0..960)
            .map(|i| (f64::sin(2.0 * std::f64::consts::PI * 440.0 * i as f64 / 48000.0) * 16000.0) as i16)
            .collect();

        let encoded = codec.encode(&pcm).expect("encode failed");
        assert!(!encoded.is_empty());
        assert!(encoded.len() < pcm.len() * 2, "Opus should compress audio");

        let decoded = codec.decode(&encoded).expect("decode failed");
        assert_eq!(decoded.len(), 960);

        // Lossy codec — check correlation, not exact match
        let correlation: f64 = pcm.iter().zip(decoded.iter())
            .map(|(&a, &b)| (a as f64) * (b as f64))
            .sum::<f64>() / (960.0 * 16000.0 * 16000.0);
        assert!(correlation > 0.5, "Decoded audio should correlate with input, got {correlation}");
    }

    #[test]
    #[should_panic(expected = "exactly 960 samples")]
    fn test_audio_rejects_wrong_frame_size() {
        let mut codec = AudioCodec::new();
        let pcm = vec![0i16; 128]; // Wrong size — should be 960
        codec.encode(&pcm);
    }
}
```

- [ ] **Step 2: Register module and run tests to verify they fail**

Add `mod codec;` to `src-tauri/src/lib.rs` (at top, with other mod declarations).

Run: `cd /Users/yousseifelshahawy/coding/personal/nafaq/src-tauri && cargo test --lib codec::tests`

Expected: FAIL — `todo!()` panics.

- [ ] **Step 3: Implement AudioCodec**

Replace the stub in `src-tauri/src/codec.rs`:

```rust
use opus::{Encoder as OpusEncoder, Decoder as OpusDecoder, Channels, Application};

const SAMPLE_RATE: u32 = 48000;
const CHANNELS: Channels = Channels::Mono;
const OPUS_FRAME_SIZE: usize = 960; // 20ms at 48kHz
const MAX_OPUS_PACKET: usize = 4000;

pub struct AudioCodec {
    encoder: OpusEncoder,
    decoder: OpusDecoder,
}

impl AudioCodec {
    pub fn new() -> Self {
        let encoder = OpusEncoder::new(SAMPLE_RATE, CHANNELS, Application::Voip)
            .expect("failed to create Opus encoder");
        let decoder = OpusDecoder::new(SAMPLE_RATE, CHANNELS)
            .expect("failed to create Opus decoder");
        Self { encoder, decoder }
    }

    pub fn encode(&mut self, pcm: &[i16]) -> Option<Vec<u8>> {
        assert!(pcm.len() == OPUS_FRAME_SIZE, "AudioCodec::encode requires exactly 960 samples, got {}", pcm.len());
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

    pub fn decode(&mut self, opus_data: &[u8]) -> Option<Vec<i16>> {
        let mut pcm = vec![0i16; OPUS_FRAME_SIZE];
        match self.decoder.decode(opus_data, &mut pcm, false) {
            Ok(n) => {
                pcm.truncate(n);
                Some(pcm)
            }
            Err(e) => {
                tracing::warn!("Opus decode error: {e}");
                None
            }
        }
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd /Users/yousseifelshahawy/coding/personal/nafaq/src-tauri && cargo test --lib codec::tests`

Expected: Both tests PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/codec.rs src-tauri/src/lib.rs
git commit -m "feat: add AudioCodec with Opus encode/decode"
```

---

## Task 3: Rust VideoCodec — RGBA↔YUV420 + VP8 Encode/Decode + Tests

**Files:**
- Modify: `src-tauri/src/codec.rs`

- [ ] **Step 1: Write failing video codec tests**

Add to `src-tauri/src/codec.rs`, below AudioCodec:

```rust
pub struct VideoCodec;

impl VideoCodec {
    pub fn new(_width: u32, _height: u32) -> Self { todo!() }
    pub fn encode(&mut self, _rgba: &[u8], _width: u32, _height: u32, _keyframe: bool) -> Option<Vec<u8>> { todo!() }
    pub fn decode(&mut self, _vp8_data: &[u8]) -> Option<(Vec<u8>, u32, u32)> { todo!() }
    pub fn update_dimensions(&mut self, _width: u32, _height: u32) { todo!() }
}
```

Add tests:

```rust
#[test]
fn test_video_roundtrip() {
    let width = 320u32;
    let height = 240u32;
    let mut codec = VideoCodec::new(width, height);

    // Create a test RGBA frame — red/blue gradient
    let mut rgba = vec![0u8; (width * height * 4) as usize];
    for y in 0..height {
        for x in 0..width {
            let idx = ((y * width + x) * 4) as usize;
            rgba[idx] = (x * 255 / width) as u8;     // R
            rgba[idx + 1] = 0;                         // G
            rgba[idx + 2] = (y * 255 / height) as u8;  // B
            rgba[idx + 3] = 255;                        // A
        }
    }

    let encoded = codec.encode(&rgba, width, height, true).expect("VP8 encode failed");
    assert!(!encoded.is_empty());
    assert!(encoded.len() < rgba.len(), "VP8 should compress video");

    let (decoded_rgba, dec_w, dec_h) = codec.decode(&encoded).expect("VP8 decode failed");
    assert_eq!(dec_w, width);
    assert_eq!(dec_h, height);
    assert_eq!(decoded_rgba.len(), (width * height * 4) as usize);
    // VP8 is lossy — just verify non-zero output
    assert!(decoded_rgba.iter().any(|&b| b != 0), "Decoded frame should not be all zeros");
}

#[test]
fn test_video_keyframe_marker() {
    let mut codec = VideoCodec::new(160, 120);
    let rgba = vec![128u8; (160 * 120 * 4) as usize];
    let encoded = codec.encode(&rgba, 160, 120, true).expect("encode failed");
    // VP8 keyframe: bit 0 of first byte is 0
    assert_eq!(encoded[0] & 0x01, 0, "Keyframe should have bit 0 = 0");
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd /Users/yousseifelshahawy/coding/personal/nafaq/src-tauri && cargo test --lib codec::tests::test_video`

Expected: FAIL — `todo!()` panics.

- [ ] **Step 3: Implement RGBA↔YUV420 conversion**

Add to `src-tauri/src/codec.rs` (pure Rust, no deps):

```rust
fn rgba_to_yuv420(rgba: &[u8], width: u32, height: u32) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let w = width as usize;
    let h = height as usize;
    let mut y_plane = vec![0u8; w * h];
    let mut u_plane = vec![0u8; (w / 2) * (h / 2)];
    let mut v_plane = vec![0u8; (w / 2) * (h / 2)];

    for row in 0..h {
        for col in 0..w {
            let idx = (row * w + col) * 4;
            let r = rgba[idx] as f32;
            let g = rgba[idx + 1] as f32;
            let b = rgba[idx + 2] as f32;

            let y = (0.299 * r + 0.587 * g + 0.114 * b).clamp(0.0, 255.0) as u8;
            y_plane[row * w + col] = y;

            if row % 2 == 0 && col % 2 == 0 {
                let u = (-0.169 * r - 0.331 * g + 0.500 * b + 128.0).clamp(0.0, 255.0) as u8;
                let v = (0.500 * r - 0.419 * g - 0.081 * b + 128.0).clamp(0.0, 255.0) as u8;
                let uv_idx = (row / 2) * (w / 2) + (col / 2);
                u_plane[uv_idx] = u;
                v_plane[uv_idx] = v;
            }
        }
    }
    (y_plane, u_plane, v_plane)
}

fn yuv420_to_rgba(y_plane: &[u8], u_plane: &[u8], v_plane: &[u8], width: u32, height: u32) -> Vec<u8> {
    let w = width as usize;
    let h = height as usize;
    let mut rgba = vec![255u8; w * h * 4];

    for row in 0..h {
        for col in 0..w {
            let y = y_plane[row * w + col] as f32;
            let uv_idx = (row / 2) * (w / 2) + (col / 2);
            let u = u_plane[uv_idx] as f32 - 128.0;
            let v = v_plane[uv_idx] as f32 - 128.0;

            let r = (y + 1.402 * v).clamp(0.0, 255.0) as u8;
            let g = (y - 0.344 * u - 0.714 * v).clamp(0.0, 255.0) as u8;
            let b = (y + 1.772 * u).clamp(0.0, 255.0) as u8;

            let idx = (row * w + col) * 4;
            rgba[idx] = r;
            rgba[idx + 1] = g;
            rgba[idx + 2] = b;
            // rgba[idx + 3] already 255 (alpha)
        }
    }
    rgba
}
```

- [ ] **Step 4: Implement VideoCodec using libvpx-sys**

Add to `src-tauri/src/codec.rs`:

```rust
use libvpx_sys::*;
use std::ptr;
use std::mem;

pub struct VideoCodec {
    width: u32,
    height: u32,
    encoder: VpxEncoder,
    decoder: VpxDecoder,
}

struct VpxEncoder {
    ctx: vpx_codec_ctx_t,
    cfg: vpx_codec_enc_cfg_t,
}

struct VpxDecoder {
    ctx: vpx_codec_ctx_t,
}

impl VideoCodec {
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            encoder: VpxEncoder::new(width, height),
            decoder: VpxDecoder::new(),
        }
    }

    pub fn encode(&mut self, rgba: &[u8], width: u32, height: u32, keyframe: bool) -> Option<Vec<u8>> {
        if width != self.width || height != self.height {
            self.update_dimensions(width, height);
        }
        let (y, u, v) = rgba_to_yuv420(rgba, width, height);
        self.encoder.encode(&y, &u, &v, width, height, keyframe)
    }

    pub fn decode(&mut self, vp8_data: &[u8]) -> Option<(Vec<u8>, u32, u32)> {
        self.decoder.decode(vp8_data)
    }

    pub fn update_dimensions(&mut self, width: u32, height: u32) {
        self.width = width;
        self.height = height;
        self.encoder = VpxEncoder::new(width, height);
    }
}

impl VpxEncoder {
    fn new(width: u32, height: u32) -> Self {
        unsafe {
            let mut cfg: vpx_codec_enc_cfg_t = mem::zeroed();
            vpx_codec_enc_config_default(vpx_codec_vp8_cx(), &mut cfg, 0);

            cfg.g_w = width;
            cfg.g_h = height;
            cfg.rc_target_bitrate = 500; // kbps
            cfg.g_timebase.num = 1;
            cfg.g_timebase.den = 15;
            cfg.g_error_resilient = VPX_ERROR_RESILIENT_DEFAULT;
            cfg.g_lag_in_frames = 0; // realtime — no look-ahead
            cfg.rc_end_usage = vpx_rc_mode::VPX_CBR;

            let mut ctx: vpx_codec_ctx_t = mem::zeroed();
            let res = vpx_codec_enc_init_ver(
                &mut ctx,
                vpx_codec_vp8_cx(),
                &cfg,
                0,
                VPX_ENCODER_ABI_VERSION as i32,
            );
            assert_eq!(res, VPX_CODEC_OK, "VP8 encoder init failed");

            // Realtime speed
            vpx_codec_control_(&mut ctx, vp8e_enc_control_id::VP8E_SET_CPUUSED as i32, 10);

            Self { ctx, cfg }
        }
    }

    fn encode(&mut self, y: &[u8], u: &[u8], v: &[u8], width: u32, height: u32, keyframe: bool) -> Option<Vec<u8>> {
        unsafe {
            let mut raw: vpx_image_t = mem::zeroed();
            vpx_img_wrap(
                &mut raw,
                vpx_img_fmt::VPX_IMG_FMT_I420,
                width,
                height,
                1,
                y.as_ptr() as *mut _,
            );
            raw.planes[0] = y.as_ptr() as *mut _;
            raw.planes[1] = u.as_ptr() as *mut _;
            raw.planes[2] = v.as_ptr() as *mut _;
            raw.stride[0] = width as i32;
            raw.stride[1] = (width / 2) as i32;
            raw.stride[2] = (width / 2) as i32;

            let flags = if keyframe { VPX_EFLAG_FORCE_KF } else { 0 };
            let res = vpx_codec_encode(
                &mut self.ctx,
                &raw,
                0, // pts
                1, // duration
                flags as i64,
                VPX_DL_REALTIME as u64,
            );
            if res != VPX_CODEC_OK {
                tracing::warn!("VP8 encode failed: {res}");
                return None;
            }

            let mut iter: vpx_codec_iter_t = ptr::null();
            let mut output = Vec::new();
            loop {
                let pkt = vpx_codec_get_cx_data(&mut self.ctx, &mut iter);
                if pkt.is_null() { break; }
                if (*pkt).kind == vpx_codec_cx_pkt_kind::VPX_CODEC_CX_FRAME_PKT {
                    let frame = &(*pkt).data.frame;
                    let data = std::slice::from_raw_parts(frame.buf as *const u8, frame.sz);
                    output.extend_from_slice(data);
                }
            }
            if output.is_empty() { None } else { Some(output) }
        }
    }
}

impl Drop for VpxEncoder {
    fn drop(&mut self) {
        unsafe { vpx_codec_destroy(&mut self.ctx); }
    }
}

impl VpxDecoder {
    fn new() -> Self {
        unsafe {
            let mut ctx: vpx_codec_ctx_t = mem::zeroed();
            let res = vpx_codec_dec_init_ver(
                &mut ctx,
                vpx_codec_vp8_dx(),
                ptr::null(),
                0,
                VPX_DECODER_ABI_VERSION as i32,
            );
            assert_eq!(res, VPX_CODEC_OK, "VP8 decoder init failed");
            Self { ctx }
        }
    }

    fn decode(&mut self, vp8_data: &[u8]) -> Option<(Vec<u8>, u32, u32)> {
        unsafe {
            let res = vpx_codec_decode(
                &mut self.ctx,
                vp8_data.as_ptr(),
                vp8_data.len() as u32,
                ptr::null_mut(),
                0,
            );
            if res != VPX_CODEC_OK {
                tracing::warn!("VP8 decode failed: {res}");
                return None;
            }

            let mut iter: vpx_codec_iter_t = ptr::null();
            let img = vpx_codec_get_frame(&mut self.ctx, &mut iter);
            if img.is_null() { return None; }

            let width = (*img).d_w;
            let height = (*img).d_h;

            let y_stride = (*img).stride[0] as usize;
            let u_stride = (*img).stride[1] as usize;
            let v_stride = (*img).stride[2] as usize;
            let w = width as usize;
            let h = height as usize;

            // Copy planes — stride may differ from width
            let mut y_plane = vec![0u8; w * h];
            for row in 0..h {
                let src = std::slice::from_raw_parts((*img).planes[0].add(row * y_stride), w);
                y_plane[row * w..row * w + w].copy_from_slice(src);
            }

            let uw = w / 2;
            let uh = h / 2;
            let mut u_plane = vec![0u8; uw * uh];
            for row in 0..uh {
                let src = std::slice::from_raw_parts((*img).planes[1].add(row * u_stride), uw);
                u_plane[row * uw..row * uw + uw].copy_from_slice(src);
            }

            let mut v_plane = vec![0u8; uw * uh];
            for row in 0..uh {
                let src = std::slice::from_raw_parts((*img).planes[2].add(row * v_stride), uw);
                v_plane[row * uw..row * uw + uw].copy_from_slice(src);
            }

            let rgba = yuv420_to_rgba(&y_plane, &u_plane, &v_plane, width, height);
            Some((rgba, width, height))
        }
    }
}

impl Drop for VpxDecoder {
    fn drop(&mut self) {
        unsafe { vpx_codec_destroy(&mut self.ctx); }
    }
}

// Safety: VPX contexts are not accessed concurrently — they're behind Mutex<Option<>>
unsafe impl Send for VpxEncoder {}
unsafe impl Send for VpxDecoder {}
unsafe impl Send for VideoCodec {}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cd /Users/yousseifelshahawy/coding/personal/nafaq/src-tauri && cargo test --lib codec::tests`

Expected: All 4 tests PASS (2 audio + 2 video).

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/codec.rs
git commit -m "feat: add VideoCodec with VP8 encode/decode via libvpx"
```

---

## Task 4: JPEG Re-encode for Video Receive + Tests

**Files:**
- Modify: `src-tauri/src/codec.rs`

- [ ] **Step 1: Write failing JPEG test**

Add to tests in `codec.rs`:

```rust
#[test]
fn test_jpeg_encode() {
    let width = 160u32;
    let height = 120u32;
    let rgba = vec![128u8; (width * height * 4) as usize];
    let jpeg = decoded_to_jpeg(&rgba, width, height);
    // Valid JPEG starts with FF D8 FF
    assert!(jpeg.len() > 2);
    assert_eq!(jpeg[0], 0xFF);
    assert_eq!(jpeg[1], 0xD8);
    // Should be much smaller than raw RGBA
    assert!(jpeg.len() < rgba.len() / 2, "JPEG should compress significantly");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd /Users/yousseifelshahawy/coding/personal/nafaq/src-tauri && cargo test --lib codec::tests::test_jpeg`

Expected: FAIL — function not defined.

- [ ] **Step 3: Implement decoded_to_jpeg**

Add to `src-tauri/src/codec.rs`:

```rust
use image::{ImageBuffer, Rgba, ImageFormat};
use std::io::Cursor;

pub fn decoded_to_jpeg(rgba: &[u8], width: u32, height: u32) -> Vec<u8> {
    let img = ImageBuffer::<Rgba<u8>, _>::from_raw(width, height, rgba.to_vec())
        .expect("RGBA buffer size mismatch");
    let mut buf = Cursor::new(Vec::new());
    img.write_to(&mut buf, ImageFormat::Jpeg)
        .expect("JPEG encoding failed");
    buf.into_inner()
}
```

Note: `image` v0.25 uses `ImageFormat::Jpeg` (not `ImageOutputFormat`). JPEG quality defaults to 80. If a different quality is needed, use the lower-level `image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, 80)` and call `encoder.encode(...)` directly.

- [ ] **Step 4: Run test to verify it passes**

Run: `cd /Users/yousseifelshahawy/coding/personal/nafaq/src-tauri && cargo test --lib codec::tests::test_jpeg`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/codec.rs
git commit -m "feat: add JPEG re-encode for decoded video frames"
```

---

## Task 5: CodecState + init_codecs/destroy_codecs Commands

**Files:**
- Modify: `src-tauri/src/state.rs`
- Modify: `src-tauri/src/commands.rs`
- Modify: `src-tauri/src/lib.rs` (register new commands)

- [ ] **Step 1: Add CodecState to AppState**

Modify `src-tauri/src/state.rs`:

```rust
use std::sync::Arc;

use iroh::Endpoint;
use iroh::protocol::Router;
use tokio::sync::broadcast;

use crate::codec::{AudioCodec, VideoCodec};
use crate::connection::ConnectionManager;
use crate::messages::Event;

pub struct CodecState {
    pub audio: tokio::sync::Mutex<Option<AudioCodec>>,
    pub video: tokio::sync::Mutex<Option<VideoCodec>>,
}

impl CodecState {
    pub fn new() -> Self {
        Self {
            audio: tokio::sync::Mutex::new(None),
            video: tokio::sync::Mutex::new(None),
        }
    }
}

pub struct AppState {
    pub endpoint: Endpoint,
    pub router: Router,
    pub conn_manager: Arc<ConnectionManager>,
    pub event_tx: broadcast::Sender<Event>,
    pub codec: Arc<CodecState>,
}
```

- [ ] **Step 2: Add init_codecs and destroy_codecs commands**

Add to `src-tauri/src/commands.rs`:

```rust
use crate::codec::{AudioCodec, VideoCodec};
use crate::state::CodecState;

#[tauri::command]
pub async fn init_codecs(
    width: u32,
    height: u32,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let mut audio = state.codec.audio.lock().await;
    *audio = Some(AudioCodec::new());
    let mut video = state.codec.video.lock().await;
    *video = Some(VideoCodec::new(width, height));
    tracing::info!("Codecs initialized: {width}x{height}");
    Ok(())
}

#[tauri::command]
pub async fn destroy_codecs(state: State<'_, AppState>) -> Result<(), String> {
    let mut audio = state.codec.audio.lock().await;
    *audio = None;
    let mut video = state.codec.video.lock().await;
    *video = None;
    tracing::info!("Codecs destroyed");
    Ok(())
}
```

- [ ] **Step 3: Update AppState initialization in lib.rs**

In `src-tauri/src/lib.rs`, update the `app_state` creation to include `CodecState`:

```rust
use crate::codec::CodecState; // add to imports at top
use std::sync::Arc;

// In run(), after creating conn_manager:
let codec = Arc::new(CodecState::new());

let app_state = AppState {
    endpoint,
    router,
    conn_manager: conn_manager.clone(),
    event_tx: event_tx.clone(),
    codec: codec.clone(), // add this field
};
```

Register the new commands in the `invoke_handler`:

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
])
```

- [ ] **Step 4: Verify it compiles**

Run: `cd /Users/yousseifelshahawy/coding/personal/nafaq/src-tauri && cargo check`

Expected: Compiles (may warn about unused codec state — that's fine, we wire it up in Task 7).

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/state.rs src-tauri/src/commands.rs src-tauri/src/lib.rs
git commit -m "feat: add CodecState + init_codecs/destroy_codecs commands"
```

---

## Task 6: Length-Framed Media Streams

**Files:**
- Modify: `src-tauri/src/connection.rs`

- [ ] **Step 1: Update send_on_stream to use write_framed**

Replace `send_on_stream` in `connection.rs`:

```rust
async fn send_on_stream(stream: &Arc<Mutex<Option<SendStream>>>, data: &[u8]) -> Result<()> {
    let mut guard = stream.lock().await;
    if let Some(ref mut send) = *guard {
        crate::messages::write_framed(send, data).await?;
    }
    Ok(())
}
```

This changes the send path from raw `write_all` to length-prefixed framing. The existing `write_framed` in `messages.rs` writes `[u32 BE length][payload]`.

- [ ] **Step 2: Update handle_uni_stream to use read_framed**

Replace `handle_uni_stream` in `connection.rs`:

```rust
async fn handle_uni_stream(
    stream_type: u8,
    peer_id: &str,
    mut recv: RecvStream,
    media_tx: broadcast::Sender<Vec<u8>>,
) {
    let peer_id_bytes: [u8; 32] = peer_id
        .parse::<iroh::EndpointId>()
        .map(|id| *id.as_bytes())
        .unwrap_or([0u8; 32]);

    loop {
        match crate::messages::read_framed(&mut recv).await {
            Ok(Some(data)) => {
                let frame = MediaFrame {
                    stream_type,
                    peer_id: peer_id_bytes,
                    timestamp_ms: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis() as u64,
                    payload: data,
                };
                let _ = media_tx.send(frame.encode());
            }
            Ok(None) => break,
            Err(e) => {
                tracing::warn!("Error reading {stream_type} stream from {peer_id}: {e}");
                break;
            }
        }
    }
}
```

- [ ] **Step 3: Verify it compiles**

Run: `cd /Users/yousseifelshahawy/coding/personal/nafaq/src-tauri && cargo check`

Expected: Compiles.

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/connection.rs
git commit -m "feat: use length-framed protocol for media uni-streams"
```

---

## Task 7: Wire Codec Encoding into send_audio/send_video Commands

**Files:**
- Modify: `src-tauri/src/commands.rs`

- [ ] **Step 1: Update send_audio to encode via CodecState**

Replace `send_audio` in `commands.rs`:

```rust
#[tauri::command]
pub async fn send_audio(
    peer_id: String,
    data: Vec<u8>,
    state: State<'_, AppState>,
) -> Result<(), String> {
    // data is raw PCM Int16 LE bytes (1920 bytes = 960 i16 samples)
    let pcm: Vec<i16> = data.chunks_exact(2)
        .map(|c| i16::from_le_bytes([c[0], c[1]]))
        .collect();

    let mut codec = state.codec.audio.lock().await;
    let encoded = match codec.as_mut() {
        Some(c) => c.encode(&pcm),
        None => return Ok(()), // codecs not initialized — drop frame
    };
    drop(codec); // release lock before network I/O

    if let Some(encoded) = encoded {
        state.conn_manager
            .send_audio(&peer_id, &encoded)
            .await
            .map_err(|e| e.to_string())
    } else {
        Ok(())
    }
}

- [ ] **Step 2: Update send_video to encode via CodecState**

Replace `send_video` in `commands.rs`:

```rust
#[tauri::command]
pub async fn send_video(
    peer_id: String,
    data: Vec<u8>,
    width: u32,
    height: u32,
    keyframe: bool,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let mut codec = state.codec.video.lock().await;
    let encoded = match codec.as_mut() {
        Some(c) => c.encode(&data, width, height, keyframe),
        None => return Ok(()), // codecs not initialized — drop frame
    };
    drop(codec);

    if let Some(encoded) = encoded {
        state.conn_manager
            .send_video(&peer_id, &encoded)
            .await
            .map_err(|e| e.to_string())
    } else {
        Ok(())
    }
}
```

- [ ] **Step 3: Verify it compiles**

Run: `cd /Users/yousseifelshahawy/coding/personal/nafaq/src-tauri && cargo check`

Expected: Compiles.

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/commands.rs src-tauri/Cargo.toml
git commit -m "feat: encode audio/video via Rust codecs in send commands"
```

---

## Task 8: Decode in Media Forwarder + Conditional Shell Plugin

**Files:**
- Modify: `src-tauri/src/lib.rs`

- [ ] **Step 1: Update MediaEvent struct**

```rust
#[derive(Clone, serde::Serialize)]
struct MediaEvent {
    stream_type: u8,
    data: Vec<u8>,
    timestamp: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    width: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    height: Option<u32>,
}
```

- [ ] **Step 2: Update media forwarder to decode before emitting**

Replace the media forwarder spawn block in `lib.rs` (the `tauri::async_runtime::spawn` that processes `media_rx`):

```rust
// Spawn media forwarder (binary frames → decode → Tauri events)
let app_handle2 = app.handle().clone();
let mut media_rx = media_tx_for_setup.subscribe();
let codec_for_media = codec.clone();

tauri::async_runtime::spawn(async move {
    loop {
        match media_rx.recv().await {
            Ok(raw) => {
                if let Some(frame) = MediaFrame::decode(&raw) {
                    match frame.stream_type {
                        STREAM_AUDIO => {
                            let mut audio = codec_for_media.audio.lock().await;
                            if let Some(ref mut dec) = *audio {
                                if let Some(pcm) = dec.decode(&frame.payload) {
                                    let data: Vec<u8> = pcm.iter()
                                        .flat_map(|s| s.to_le_bytes())
                                        .collect();
                                    let _ = app_handle2.emit("audio-received", MediaEvent {
                                        stream_type: frame.stream_type,
                                        data,
                                        timestamp: frame.timestamp_ms,
                                        width: None,
                                        height: None,
                                    });
                                }
                            }
                        }
                        STREAM_VIDEO => {
                            let mut video = codec_for_media.video.lock().await;
                            if let Some(ref mut dec) = *video {
                                if let Some((rgba, w, h)) = dec.decode(&frame.payload) {
                                    let jpeg = crate::codec::decoded_to_jpeg(&rgba, w, h);
                                    let _ = app_handle2.emit("video-received", MediaEvent {
                                        stream_type: frame.stream_type,
                                        data: jpeg,
                                        timestamp: frame.timestamp_ms,
                                        width: Some(w),
                                        height: Some(h),
                                    });
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            Err(broadcast::error::RecvError::Lagged(n)) => {
                tracing::warn!("Media forwarder lagged by {n} frames");
            }
            Err(broadcast::error::RecvError::Closed) => break,
        }
    }
});
```

- [ ] **Step 3: Move tauri-plugin-shell to desktop-only in Cargo.toml**

In `src-tauri/Cargo.toml`, move `tauri-plugin-shell = "2"` from `[dependencies]` to:

```toml
[target.'cfg(not(any(target_os = "android", target_os = "ios")))'.dependencies]
tauri-plugin-shell = "2"
```

- [ ] **Step 4: Wrap shell plugin with cfg(desktop) in lib.rs**

In `lib.rs`, change the builder chain:

```rust
let mut builder = tauri::Builder::default()
    .manage(app_state);

#[cfg(desktop)]
{
    builder = builder.plugin(tauri_plugin_shell::init());
}

builder
    .setup(move |app| {
        // ... existing setup code ...
    })
    .invoke_handler(tauri::generate_handler![
        // ... existing handlers ...
    ])
    .run(tauri::generate_context!())
    .expect("error running nafaq");
```

- [ ] **Step 5: Verify module imports**

`mod codec;` was already added to `lib.rs` in Task 2. Ensure the `use` statements at the top of `lib.rs` include:

```rust
use codec::CodecState;
```

- [ ] **Step 6: Verify it compiles**

Run: `cd /Users/yousseifelshahawy/coding/personal/nafaq/src-tauri && cargo check`

Expected: Compiles.

- [ ] **Step 7: Run all Rust tests**

Run: `cd /Users/yousseifelshahawy/coding/personal/nafaq/src-tauri && cargo test`

Expected: All tests pass (codec tests + existing message/node tests).

- [ ] **Step 8: Commit**

```bash
git add src-tauri/src/lib.rs src-tauri/Cargo.toml
git commit -m "feat: decode media in Rust forwarder, conditional shell plugin"
```

---

## Task 9: Rewrite Frontend useMediaTransport.ts

**Files:**
- Rewrite: `app/composables/useMediaTransport.ts`

- [ ] **Step 1: Rewrite useMediaTransport.ts**

Replace the entire file with the new implementation that removes all WebCodecs code and sends raw frames to Rust:

```typescript
// Media transport: raw frames → Rust codec pipeline.
// Audio: AudioWorklet → buffer 960 samples → Int16 PCM → invoke("send_audio") → Rust Opus encode
// Video: Canvas capture 15fps → getImageData RGBA → invoke("send_video") → Rust VP8 encode
// Receive: Rust decodes → PCM Int16 / JPEG → JS playback

const encoding = ref(false);
const OPUS_FRAME_SAMPLES = 960; // 20ms at 48kHz

let playbackCtx: AudioContext | null = null;
let captureCtx: AudioContext | null = null;
let captureVideoEl: HTMLVideoElement | null = null;
let captureInterval: ReturnType<typeof setInterval> | null = null;
let workletNode: AudioWorkletNode | null = null;
let sourceNode: MediaStreamAudioSourceNode | null = null;
let nextPlayTime = 0;
let unlistenAudio: (() => void) | null = null;
let unlistenVideo: (() => void) | null = null;
let remoteCanvas: HTMLCanvasElement | null = null;

export function useMediaTransport() {
  async function startSending(stream: MediaStream, peerId: string) {
    if (encoding.value) return;
    encoding.value = true;

    const { invoke } = await import("@tauri-apps/api/core");

    // Determine resolution — cap at 320x240 on mobile
    const isMobile = /android/i.test(navigator.userAgent);
    const videoTrack = stream.getVideoTracks()[0];
    const maxDim = isMobile ? 320 : 640;
    const width = videoTrack
      ? Math.min(videoTrack.getSettings().width || maxDim, maxDim)
      : maxDim;
    const height = videoTrack
      ? Math.min(videoTrack.getSettings().height || (isMobile ? 240 : 480), isMobile ? 240 : 480)
      : (isMobile ? 240 : 480);

    // Initialize Rust codecs BEFORE audio or video setup — needed for both paths
    await invoke("init_codecs", { width, height });

    // --- Audio ---
    const audioTrack = stream.getAudioTracks()[0];
    if (audioTrack) {
      captureCtx = new AudioContext({ sampleRate: 48000 });
      const WORKLET_CODE = `
        class CaptureProcessor extends AudioWorkletProcessor {
          process(inputs) {
            const ch = inputs[0]?.[0];
            if (ch && ch.length > 0) {
              this.port.postMessage({ samples: new Float32Array(ch) });
            }
            return true;
          }
        }
        registerProcessor("capture", CaptureProcessor);
      `;
      const blobUrl = URL.createObjectURL(new Blob([WORKLET_CODE], { type: "application/javascript" }));
      await captureCtx.audioWorklet.addModule(blobUrl);
      URL.revokeObjectURL(blobUrl);

      sourceNode = captureCtx.createMediaStreamSource(new MediaStream([audioTrack]));
      workletNode = new AudioWorkletNode(captureCtx, "capture");

      // Buffer 128-sample worklet chunks into 960-sample Opus frames
      const sampleBuffer = new Float32Array(OPUS_FRAME_SAMPLES);
      let bufferOffset = 0;

      workletNode.port.onmessage = (event) => {
        const { samples } = event.data as { samples: Float32Array };
        let srcOffset = 0;

        while (srcOffset < samples.length) {
          const remaining = OPUS_FRAME_SAMPLES - bufferOffset;
          const toCopy = Math.min(remaining, samples.length - srcOffset);
          sampleBuffer.set(samples.subarray(srcOffset, srcOffset + toCopy), bufferOffset);
          bufferOffset += toCopy;
          srcOffset += toCopy;

          if (bufferOffset === OPUS_FRAME_SAMPLES) {
            // Convert Float32 → Int16 LE
            const pcm = new Int16Array(OPUS_FRAME_SAMPLES);
            for (let i = 0; i < OPUS_FRAME_SAMPLES; i++) {
              pcm[i] = Math.max(-32768, Math.min(32767, Math.round(sampleBuffer[i] * 32767)));
            }
            invoke("send_audio", { peerId, data: new Uint8Array(pcm.buffer) }).catch(() => {});
            bufferOffset = 0;
          }
        }
      };

      sourceNode.connect(workletNode);
      workletNode.connect(captureCtx.destination);
    }

    // --- Video ---
    if (videoTrack) {
      if (captureVideoEl) { captureVideoEl.pause(); captureVideoEl.srcObject = null; }
      captureVideoEl = document.createElement("video");
      captureVideoEl.srcObject = stream;
      captureVideoEl.muted = true;
      captureVideoEl.play();

      const canvas = new OffscreenCanvas(width, height);
      const ctx = canvas.getContext("2d")!;
      let vFrameCount = 0;

      captureInterval = setInterval(() => {
        if (!captureVideoEl || captureVideoEl.readyState < 2) return;
        ctx.drawImage(captureVideoEl, 0, 0, width, height);
        const imageData = ctx.getImageData(0, 0, width, height);
        const keyframe = vFrameCount === 0 || vFrameCount % 30 === 0;
        vFrameCount++;
        invoke("send_video", {
          peerId,
          data: new Uint8Array(imageData.data.buffer),
          width,
          height,
          keyframe,
        }).catch(() => {});
      }, 1000 / 15);
    }
  }

  async function startReceiving(canvas: HTMLCanvasElement) {
    remoteCanvas = canvas;
    const { listen } = await import("@tauri-apps/api/event");

    playbackCtx = new AudioContext({ sampleRate: 48000 });
    nextPlayTime = playbackCtx.currentTime;

    // Cache canvas context
    let canvasCtx: CanvasRenderingContext2D | null = null;

    type AudioPayload = { stream_type: number; data: number[]; timestamp: number };
    type VideoPayload = { stream_type: number; data: number[]; timestamp: number; width: number; height: number };

    // Audio receive — PCM Int16 from Rust Opus decoder
    unlistenAudio = await listen<AudioPayload>("audio-received", (event) => {
      if (!playbackCtx) return;
      const bytes = new Uint8Array(event.payload.data);
      const int16 = new Int16Array(bytes.buffer, bytes.byteOffset, bytes.byteLength / 2);
      const buffer = playbackCtx.createBuffer(1, int16.length, 48000);
      const ch = buffer.getChannelData(0);
      for (let i = 0; i < int16.length; i++) ch[i] = int16[i] / 32768;
      scheduleAudioBuffer(buffer);
    });

    // Video receive — JPEG from Rust VP8 decoder
    unlistenVideo = await listen<VideoPayload>("video-received", (event) => {
      if (!remoteCanvas) return;
      const bytes = new Uint8Array(event.payload.data);
      const blob = new Blob([bytes], { type: "image/jpeg" });
      createImageBitmap(blob).then((bitmap) => {
        if (!remoteCanvas) return;
        if (!canvasCtx) canvasCtx = remoteCanvas.getContext("2d");
        if (canvasCtx) {
          if (remoteCanvas.width !== bitmap.width) remoteCanvas.width = bitmap.width;
          if (remoteCanvas.height !== bitmap.height) remoteCanvas.height = bitmap.height;
          canvasCtx.drawImage(bitmap, 0, 0);
          bitmap.close();
        }
      }).catch(() => {});
    });
  }

  async function stop() {
    encoding.value = false;

    if (workletNode) { workletNode.port.onmessage = null; workletNode.disconnect(); workletNode = null; }
    if (sourceNode) { sourceNode.disconnect(); sourceNode = null; }
    if (captureCtx) { captureCtx.close(); captureCtx = null; }
    if (captureVideoEl) { captureVideoEl.pause(); captureVideoEl.srcObject = null; captureVideoEl = null; }
    if (captureInterval) { clearInterval(captureInterval); captureInterval = null; }
    if (playbackCtx) { playbackCtx.close(); playbackCtx = null; }

    unlistenAudio?.(); unlistenVideo?.();
    unlistenAudio = null; unlistenVideo = null;
    remoteCanvas = null;

    // Clean up Rust codec state
    try {
      const { invoke } = await import("@tauri-apps/api/core");
      await invoke("destroy_codecs");
    } catch {}
  }

  function scheduleAudioBuffer(buffer: AudioBuffer) {
    if (!playbackCtx) return;
    const source = playbackCtx.createBufferSource();
    source.buffer = buffer;
    source.connect(playbackCtx.destination);
    const now = playbackCtx.currentTime;
    if (nextPlayTime < now - 0.2) nextPlayTime = now;
    source.start(nextPlayTime);
    nextPlayTime += buffer.duration;
  }

  return { encoding, startSending, startReceiving, stop };
}
```

**Breaking change:** `stop()` is now `async` (it calls `invoke("destroy_codecs")`). Callers in UI components that call `stop()` must now `await` it. Check `app/pages/call.vue` and any component that calls `useMediaTransport().stop()` — add `await` where needed.

- [ ] **Step 2: Verify TypeScript compiles and fix callers**

Run: `cd /Users/yousseifelshahawy/coding/personal/nafaq && bun run generate`

Expected: Nuxt generates successfully. If there are type errors (e.g., `OffscreenCanvas.getContext("2d")` returning different type), fix them.

- [ ] **Step 3: Commit**

```bash
git add app/composables/useMediaTransport.ts
git commit -m "feat: rewrite media transport to use Rust codec pipeline"
```

---

## Task 10: Android Tauri Setup

**Files:**
- Create: `src-tauri/tauri.android.conf.json`
- Create: `src-tauri/capabilities/mobile.json`
- Modify: `src-tauri/capabilities/main.json`
- Modify: `package.json`

- [ ] **Step 1: Add Android build scripts to package.json**

Add to `scripts` in `package.json`:

```json
"tauri:android:dev": "tauri android dev",
"tauri:android:build": "tauri android build"
```

- [ ] **Step 2: Create Android config override**

Create `src-tauri/tauri.android.conf.json`:

```json
{
  "app": {
    "windows": [
      {
        "label": "main",
        "title": "Nafaq",
        "fullscreen": false
      }
    ]
  }
}
```

- [ ] **Step 3: Create mobile capabilities**

Create `src-tauri/capabilities/mobile.json`:

```json
{
  "$schema": "../gen/schemas/mobile-schema.json",
  "identifier": "mobile",
  "description": "Capabilities for mobile platforms",
  "platforms": ["android", "iOS"],
  "windows": ["main"],
  "permissions": [
    "core:default",
    "core:event:default",
    "core:window:default",
    "core:app:default"
  ]
}
```

- [ ] **Step 4: Add platform filter to desktop capabilities**

Update `src-tauri/capabilities/main.json` to scope it to desktop platforms:

```json
{
  "$schema": "../gen/schemas/desktop-schema.json",
  "identifier": "main-capability",
  "description": "Main window permissions",
  "platforms": ["linux", "macOS", "windows"],
  "windows": ["main"],
  "permissions": [
    "core:default",
    "core:event:default",
    "core:window:default",
    "core:window:allow-close",
    "core:window:allow-set-title",
    "core:app:default",
    "shell:allow-open"
  ]
}
```

- [ ] **Step 5: Run tauri android init**

Run: `cd /Users/yousseifelshahawy/coding/personal/nafaq && bun run tauri android init`

This generates `src-tauri/gen/android/` with the Gradle project, `MainActivity.kt`, and `AndroidManifest.xml`.

Expected: Command succeeds and creates the Android project structure.

- [ ] **Step 6: Add Android permissions to AndroidManifest.xml**

After `tauri android init`, edit `src-tauri/gen/android/app/src/main/AndroidManifest.xml` to add permissions before the `<application>` tag:

```xml
<uses-permission android:name="android.permission.INTERNET" />
<uses-permission android:name="android.permission.CAMERA" />
<uses-permission android:name="android.permission.RECORD_AUDIO" />
<uses-permission android:name="android.permission.MODIFY_AUDIO_SETTINGS" />
```

Note: Some of these may already be present from Tauri's default template. Only add the missing ones.

- [ ] **Step 7: Commit**

```bash
git add package.json src-tauri/tauri.android.conf.json src-tauri/capabilities/ src-tauri/gen/android/
git commit -m "feat: add Android Tauri setup with permissions"
```

---

## Task 11: Desktop Integration Test

**Files:** None (manual verification)

- [ ] **Step 1: Build and run desktop app**

Run: `cd /Users/yousseifelshahawy/coding/personal/nafaq && bun run tauri:dev`

Expected: App launches, shows node ID on home page, sidecar connected indicator.

- [ ] **Step 2: Verify Rust codec pipeline works**

Open browser devtools in the Tauri window. Check console for any errors.

Create a call → copy ticket → open a second instance (or use a second terminal with `bun run tauri:dev`) → join with ticket.

Verify:
- Peer connected event fires
- Audio flows (check console for any codec errors)
- Video shows on remote canvas
- No WebCodecs errors (those APIs should no longer be used)

- [ ] **Step 3: Commit any fixes needed**

If any bugs are found during testing, fix and commit individually.

---

## Task 12: Android Build + Emulator Deploy

**Files:** None (build verification)

- [ ] **Step 1: Build Android APK**

Run: `cd /Users/yousseifelshahawy/coding/personal/nafaq && bun run tauri:android:build --target aarch64`

Expected: Build succeeds. APK generated at `src-tauri/gen/android/app/build/outputs/apk/`.

If `libvpx-sys` fails to cross-compile for Android:
1. Check error output
2. Try `LIBVPX_NO_PKG_CONFIG=1 bun run tauri:android:build --target aarch64`
3. If still fails, swap `libvpx-sys` for `openh264` in `Cargo.toml` and adapt `VideoCodec` in `codec.rs` (spec fallback plan)

- [ ] **Step 2: Start Android emulator**

Run: `adb devices` to check for running emulator.

If no emulator running, start one (check `~/coding/personal/meeqat` for the emulator setup — likely uses `emulator -avd <avd_name>`).

- [ ] **Step 3: Install APK on emulator**

Run: `adb install src-tauri/gen/android/app/build/outputs/apk/universal/release/app-universal-release.apk`

(Path may vary — check actual output directory from Step 1.)

Or use: `bun run tauri:android:dev` which builds and deploys to connected device/emulator in one step.

- [ ] **Step 4: Cross-test desktop ↔ Android**

1. Run desktop app: `bun run tauri:dev`
2. On desktop: create call, copy ticket
3. On Android emulator: open app, paste ticket, join call
4. Verify:
   - App launches on Android without crash
   - Camera/mic permission dialog appears
   - Peer connection established
   - Audio flows bidirectionally
   - Video shows on both sides

- [ ] **Step 5: Commit any Android-specific fixes**

If any fixes are needed for Android (permissions, WebView quirks, etc.), commit individually.
