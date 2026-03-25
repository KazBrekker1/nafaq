use opus::{Encoder as OpusEncoder, Decoder as OpusDecoder, Channels, Application};
use vpx_sys::*;
use std::mem::MaybeUninit;
use std::ptr;

const SAMPLE_RATE: u32 = 48000;
const CHANNELS: Channels = Channels::Mono;
const OPUS_FRAME_SIZE: usize = 960; // 20ms at 48kHz
const MAX_OPUS_PACKET: usize = 4000;

// libvpx ABI version constants (C #defines, not exported by bindgen).
// Computed from installed libvpx 1.16.0 headers:
//   VPX_IMAGE_ABI_VERSION = 5
//   VPX_CODEC_ABI_VERSION = 4 + 5 = 9
//   VPX_TPL_ABI_VERSION = 5
//   VPX_EXT_RATECTRL_ABI_VERSION = 7 + 5 = 12
//   VPX_ENCODER_ABI_VERSION = 18 + 9 + 12 = 39
//   VPX_DECODER_ABI_VERSION = 3 + 9 = 12
const VPX_ENCODER_ABI_VERSION: i32 = 39;
const VPX_DECODER_ABI_VERSION: i32 = 12;

// Encoder/decoder deadline and flag constants (C #defines, not in bindgen output).
const VPX_DL_REALTIME: std::ffi::c_ulong = 1;
const VPX_EFLAG_FORCE_KF: std::ffi::c_long = 1 << 0;

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

// ── RGBA ↔ YUV420 conversion (BT.601) ──────────────────────────────────

fn rgba_to_yuv420(rgba: &[u8], width: u32, height: u32) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let w = width as usize;
    let h = height as usize;
    let uv_w = (w + 1) / 2;
    let uv_h = (h + 1) / 2;
    let mut y_plane = vec![0u8; w * h];
    let mut u_plane = vec![0u8; uv_w * uv_h];
    let mut v_plane = vec![0u8; uv_w * uv_h];

    for row in 0..h {
        for col in 0..w {
            let idx = (row * w + col) * 4;
            let r = rgba[idx] as f32;
            let g = rgba[idx + 1] as f32;
            let b = rgba[idx + 2] as f32;

            // BT.601 coefficients
            let y = (0.257 * r + 0.504 * g + 0.098 * b + 16.0).clamp(0.0, 255.0);
            y_plane[row * w + col] = y as u8;

            // Subsample U/V: average each 2x2 block
            if row % 2 == 0 && col % 2 == 0 {
                let uv_idx = (row / 2) * uv_w + (col / 2);
                let mut sum_r = r;
                let mut sum_g = g;
                let mut sum_b = b;
                let mut count = 1.0f32;

                if col + 1 < w {
                    let i2 = (row * w + col + 1) * 4;
                    sum_r += rgba[i2] as f32;
                    sum_g += rgba[i2 + 1] as f32;
                    sum_b += rgba[i2 + 2] as f32;
                    count += 1.0;
                }
                if row + 1 < h {
                    let i2 = ((row + 1) * w + col) * 4;
                    sum_r += rgba[i2] as f32;
                    sum_g += rgba[i2 + 1] as f32;
                    sum_b += rgba[i2 + 2] as f32;
                    count += 1.0;
                }
                if col + 1 < w && row + 1 < h {
                    let i2 = ((row + 1) * w + col + 1) * 4;
                    sum_r += rgba[i2] as f32;
                    sum_g += rgba[i2 + 1] as f32;
                    sum_b += rgba[i2 + 2] as f32;
                    count += 1.0;
                }

                let avg_r = sum_r / count;
                let avg_g = sum_g / count;
                let avg_b = sum_b / count;
                let u = (-0.148 * avg_r - 0.291 * avg_g + 0.439 * avg_b + 128.0).clamp(0.0, 255.0);
                let v = (0.439 * avg_r - 0.368 * avg_g - 0.071 * avg_b + 128.0).clamp(0.0, 255.0);
                u_plane[uv_idx] = u as u8;
                v_plane[uv_idx] = v as u8;
            }
        }
    }
    (y_plane, u_plane, v_plane)
}

fn yuv420_to_rgba(y: &[u8], u: &[u8], v: &[u8], width: u32, height: u32) -> Vec<u8> {
    let w = width as usize;
    let h = height as usize;
    let uv_w = (w + 1) / 2;
    let mut rgba = vec![255u8; w * h * 4];

    for row in 0..h {
        for col in 0..w {
            let y_val = y[row * w + col] as f32;
            let uv_idx = (row / 2) * uv_w + (col / 2);
            let u_val = u[uv_idx] as f32 - 128.0;
            let v_val = v[uv_idx] as f32 - 128.0;

            // BT.601 inverse
            let r = (1.164 * (y_val - 16.0) + 1.596 * v_val).clamp(0.0, 255.0);
            let g = (1.164 * (y_val - 16.0) - 0.813 * v_val - 0.391 * u_val).clamp(0.0, 255.0);
            let b = (1.164 * (y_val - 16.0) + 2.018 * u_val).clamp(0.0, 255.0);

            let out_idx = (row * w + col) * 4;
            rgba[out_idx] = r as u8;
            rgba[out_idx + 1] = g as u8;
            rgba[out_idx + 2] = b as u8;
            // alpha already set to 255
        }
    }
    rgba
}

// ── VPX Encoder/Decoder wrappers ────────────────────────────────────────

struct VpxEncoder {
    ctx: vpx_codec_ctx_t,
    #[allow(dead_code)]
    cfg: vpx_codec_enc_cfg_t,
    pts: i64,
}

impl VpxEncoder {
    fn new(width: u32, height: u32) -> Self {
        unsafe {
            let mut cfg_uninit = MaybeUninit::<vpx_codec_enc_cfg_t>::uninit();
            let iface = vpx_codec_vp8_cx();
            let ret = vpx_codec_enc_config_default(iface, cfg_uninit.as_mut_ptr(), 0);
            assert_eq!(ret, vpx_codec_err_t::VPX_CODEC_OK, "vpx_codec_enc_config_default failed");
            let mut cfg = cfg_uninit.assume_init();

            cfg.g_w = width;
            cfg.g_h = height;
            cfg.g_timebase.num = 1;
            cfg.g_timebase.den = 15; // 15fps timebase
            cfg.rc_target_bitrate = 500; // 500 kbps
            cfg.rc_end_usage = vpx_rc_mode::VPX_CBR;
            cfg.g_lag_in_frames = 0;
            cfg.g_error_resilient = 1;
            cfg.g_threads = 1;

            let mut ctx_uninit = MaybeUninit::<vpx_codec_ctx_t>::uninit();
            let ret = vpx_codec_enc_init_ver(
                ctx_uninit.as_mut_ptr(),
                iface,
                &cfg,
                0, // flags
                VPX_ENCODER_ABI_VERSION,
            );
            assert_eq!(ret, vpx_codec_err_t::VPX_CODEC_OK, "vpx_codec_enc_init_ver failed: {ret:?}");
            let mut ctx = ctx_uninit.assume_init();

            // Set CPU speed to 10 for realtime encoding
            let ret = vpx_codec_control_(
                &mut ctx,
                vp8e_enc_control_id::VP8E_SET_CPUUSED as std::ffi::c_int,
                10 as std::ffi::c_int,
            );
            assert_eq!(ret, vpx_codec_err_t::VPX_CODEC_OK, "vpx_codec_control_ SET_CPUUSED failed");

            Self { ctx, cfg, pts: 0 }
        }
    }

    fn encode(&mut self, y: &[u8], u: &[u8], v: &[u8], width: u32, height: u32, keyframe: bool) -> Option<Vec<u8>> {
        unsafe {
            let mut img_uninit = MaybeUninit::<vpx_image_t>::uninit();
            let ret = vpx_img_alloc(img_uninit.as_mut_ptr(), vpx_img_fmt::VPX_IMG_FMT_I420, width, height, 1);
            if ret.is_null() {
                tracing::warn!("vpx_img_alloc failed");
                return None;
            }
            let mut img = img_uninit.assume_init();

            // Copy Y plane
            let y_stride = img.stride[0] as usize;
            for row in 0..height as usize {
                let src_offset = row * width as usize;
                let dst = img.planes[0].add(row * y_stride);
                ptr::copy_nonoverlapping(y.as_ptr().add(src_offset), dst, width as usize);
            }

            // Copy U plane
            let uv_w = ((width + 1) / 2) as usize;
            let uv_h = ((height + 1) / 2) as usize;
            let u_stride = img.stride[1] as usize;
            for row in 0..uv_h {
                let src_offset = row * uv_w;
                let dst = img.planes[1].add(row * u_stride);
                ptr::copy_nonoverlapping(u.as_ptr().add(src_offset), dst, uv_w);
            }

            // Copy V plane
            let v_stride = img.stride[2] as usize;
            for row in 0..uv_h {
                let src_offset = row * uv_w;
                let dst = img.planes[2].add(row * v_stride);
                ptr::copy_nonoverlapping(v.as_ptr().add(src_offset), dst, uv_w);
            }

            let flags: std::ffi::c_long = if keyframe { VPX_EFLAG_FORCE_KF } else { 0 };
            let ret = vpx_codec_encode(
                &mut self.ctx,
                &img,
                self.pts,
                1, // duration
                flags,
                VPX_DL_REALTIME,
            );

            vpx_img_free(&mut img);

            if ret != vpx_codec_err_t::VPX_CODEC_OK {
                tracing::warn!("vpx_codec_encode failed: {ret:?}");
                return None;
            }

            self.pts += 1;

            // Collect encoded packets
            let mut iter: vpx_codec_iter_t = ptr::null();
            let mut result = Vec::new();
            loop {
                let pkt = vpx_codec_get_cx_data(&mut self.ctx, &mut iter);
                if pkt.is_null() {
                    break;
                }
                if (*pkt).kind == vpx_codec_cx_pkt_kind::VPX_CODEC_CX_FRAME_PKT {
                    let frame = &(*pkt).data.frame;
                    let data = std::slice::from_raw_parts(frame.buf as *const u8, frame.sz);
                    result.extend_from_slice(data);
                }
            }

            if result.is_empty() {
                None
            } else {
                Some(result)
            }
        }
    }
}

impl Drop for VpxEncoder {
    fn drop(&mut self) {
        unsafe {
            vpx_codec_destroy(&mut self.ctx);
        }
    }
}

struct VpxDecoder {
    ctx: vpx_codec_ctx_t,
}

impl VpxDecoder {
    fn new() -> Self {
        unsafe {
            let mut ctx_uninit = MaybeUninit::<vpx_codec_ctx_t>::uninit();
            let iface = vpx_codec_vp8_dx();
            let ret = vpx_codec_dec_init_ver(
                ctx_uninit.as_mut_ptr(),
                iface,
                ptr::null(),
                0, // flags
                VPX_DECODER_ABI_VERSION,
            );
            assert_eq!(ret, vpx_codec_err_t::VPX_CODEC_OK, "vpx_codec_dec_init_ver failed");
            Self { ctx: ctx_uninit.assume_init() }
        }
    }

    fn decode(&mut self, data: &[u8]) -> Option<(Vec<u8>, Vec<u8>, Vec<u8>, u32, u32)> {
        unsafe {
            let ret = vpx_codec_decode(
                &mut self.ctx,
                data.as_ptr(),
                data.len() as std::ffi::c_uint,
                ptr::null_mut(),
                0, // deadline
            );
            if ret != vpx_codec_err_t::VPX_CODEC_OK {
                tracing::warn!("vpx_codec_decode failed: {ret:?}");
                return None;
            }

            let mut iter: vpx_codec_iter_t = ptr::null();
            let img = vpx_codec_get_frame(&mut self.ctx, &mut iter);
            if img.is_null() {
                return None;
            }

            let width = (*img).d_w;
            let height = (*img).d_h;
            let uv_w = ((width + 1) / 2) as usize;
            let uv_h = ((height + 1) / 2) as usize;

            // Extract Y plane
            let y_stride = (*img).stride[0] as usize;
            let mut y_plane = vec![0u8; (width as usize) * (height as usize)];
            for row in 0..height as usize {
                let src = (*img).planes[0].add(row * y_stride);
                let dst_offset = row * width as usize;
                ptr::copy_nonoverlapping(src, y_plane.as_mut_ptr().add(dst_offset), width as usize);
            }

            // Extract U plane
            let u_stride = (*img).stride[1] as usize;
            let mut u_plane = vec![0u8; uv_w * uv_h];
            for row in 0..uv_h {
                let src = (*img).planes[1].add(row * u_stride);
                let dst_offset = row * uv_w;
                ptr::copy_nonoverlapping(src, u_plane.as_mut_ptr().add(dst_offset), uv_w);
            }

            // Extract V plane
            let v_stride = (*img).stride[2] as usize;
            let mut v_plane = vec![0u8; uv_w * uv_h];
            for row in 0..uv_h {
                let src = (*img).planes[2].add(row * v_stride);
                let dst_offset = row * uv_w;
                ptr::copy_nonoverlapping(src, v_plane.as_mut_ptr().add(dst_offset), uv_w);
            }

            Some((y_plane, u_plane, v_plane, width, height))
        }
    }
}

impl Drop for VpxDecoder {
    fn drop(&mut self) {
        unsafe {
            vpx_codec_destroy(&mut self.ctx);
        }
    }
}

// SAFETY: VpxEncoder/VpxDecoder contain raw pointers inside vpx_codec_ctx_t,
// but they are only accessed through &mut self methods and will be behind
// Mutex<Option<>> in the application. The libvpx codec contexts are not
// internally shared across threads.
unsafe impl Send for VpxEncoder {}
unsafe impl Send for VpxDecoder {}

// ── VideoCodec ──────────────────────────────────────────────────────────

pub struct VideoCodec {
    width: u32,
    height: u32,
    encoder: VpxEncoder,
    decoder: VpxDecoder,
}

// SAFETY: See VpxEncoder/VpxDecoder Send impls above.
unsafe impl Send for VideoCodec {}

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
        assert_eq!(rgba.len(), (width * height * 4) as usize, "RGBA buffer size mismatch");

        // Handle resolution change
        if width != self.width || height != self.height {
            self.update_dimensions(width, height);
        }

        let (y, u, v) = rgba_to_yuv420(rgba, width, height);
        self.encoder.encode(&y, &u, &v, width, height, keyframe)
    }

    pub fn decode(&mut self, vp8_data: &[u8]) -> Option<(Vec<u8>, u32, u32)> {
        let (y, u, v, width, height) = self.decoder.decode(vp8_data)?;
        let rgba = yuv420_to_rgba(&y, &u, &v, width, height);
        Some((rgba, width, height))
    }

    pub fn update_dimensions(&mut self, width: u32, height: u32) {
        self.width = width;
        self.height = height;
        self.encoder = VpxEncoder::new(width, height);
    }
}

// ── JPEG encoding for IPC ──────────────────────────────────────────────

pub fn decoded_to_jpeg(rgba: &[u8], width: u32, height: u32) -> Vec<u8> {
    use image::{ImageBuffer, Rgb, ImageFormat};
    use std::io::Cursor;

    // JPEG doesn't support alpha — convert RGBA to RGB
    let rgb: Vec<u8> = rgba.chunks_exact(4)
        .flat_map(|px| [px[0], px[1], px[2]])
        .collect();
    let img = ImageBuffer::<Rgb<u8>, _>::from_raw(width, height, rgb)
        .expect("RGB buffer size mismatch");
    let mut buf = Cursor::new(Vec::new());
    img.write_to(&mut buf, ImageFormat::Jpeg)
        .expect("JPEG encoding failed");
    buf.into_inner()
}

// ── CodecState for AppState ────────────────────────────────────────────

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
        assert!(correlation > 0.1, "Decoded audio should correlate with input, got {correlation}");
    }

    #[test]
    #[should_panic(expected = "exactly 960 samples")]
    fn test_audio_rejects_wrong_frame_size() {
        let mut codec = AudioCodec::new();
        let pcm = vec![0i16; 128];
        codec.encode(&pcm);
    }

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
                rgba[idx] = (x * 255 / width) as u8;
                rgba[idx + 1] = 0;
                rgba[idx + 2] = (y * 255 / height) as u8;
                rgba[idx + 3] = 255;
            }
        }

        let encoded = codec.encode(&rgba, width, height, true).expect("VP8 encode failed");
        assert!(!encoded.is_empty());
        assert!(encoded.len() < rgba.len(), "VP8 should compress video");

        let (decoded_rgba, dec_w, dec_h) = codec.decode(&encoded).expect("VP8 decode failed");
        assert_eq!(dec_w, width);
        assert_eq!(dec_h, height);
        assert_eq!(decoded_rgba.len(), (width * height * 4) as usize);
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

    #[test]
    fn test_jpeg_encode() {
        let width = 64u32;
        let height = 64u32;
        let rgba = vec![128u8; (width * height * 4) as usize];
        let jpeg = decoded_to_jpeg(&rgba, width, height);
        // JPEG starts with FF D8
        assert_eq!(jpeg[0], 0xFF);
        assert_eq!(jpeg[1], 0xD8);
        assert!(jpeg.len() < rgba.len(), "JPEG should be smaller than raw RGBA");
    }
}
