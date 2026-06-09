use std::collections::HashMap;
use std::io::Cursor;

use image::codecs::jpeg::JpegEncoder;
use image::ExtendedColorType;
use image::ImageEncoder;
use openh264::decoder::Decoder as H264Decoder;
use openh264::encoder::{Encoder as H264Encoder, EncoderConfig};
use openh264::formats::{RgbaSliceU8, YUVBuffer, YUVSource};
use openh264::OpenH264API;
use opus::{Application, Channels, Decoder as OpusDecoder, Encoder as OpusEncoder};

const SAMPLE_RATE: u32 = 48000;
const CHANNELS: Channels = Channels::Mono;
pub const OPUS_FRAME_SIZE: usize = 960; // 20ms at 48kHz
const MAX_OPUS_PACKET: usize = 4000;

// ── Audio Encoder ────────────────────────────────────────────────────

pub struct AudioEncoder {
    encoder: OpusEncoder,
    /// Reused per-call scratch; encode() returns a right-sized copy of the
    /// encoded prefix instead of a 4 KB allocation per 20 ms frame.
    scratch: Vec<u8>,
}

impl AudioEncoder {
    pub fn new() -> anyhow::Result<Self> {
        let mut encoder = OpusEncoder::new(SAMPLE_RATE, CHANNELS, Application::Voip)?;
        encoder.set_inband_fec(true)?;
        encoder.set_packet_loss_perc(5)?;
        Ok(Self {
            encoder,
            scratch: vec![0u8; MAX_OPUS_PACKET],
        })
    }

    pub fn encode(&mut self, pcm: &[i16]) -> Option<Vec<u8>> {
        if pcm.len() != OPUS_FRAME_SIZE {
            tracing::warn!(
                "AudioEncoder::encode requires exactly 960 samples, got {}",
                pcm.len()
            );
            return None;
        }
        match self.encoder.encode(pcm, &mut self.scratch) {
            Ok(n) => Some(self.scratch[..n].to_vec()),
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
    /// Reused per-call scratch; decode() returns a right-sized copy so callers
    /// keep an owned frame without a full-frame zeroed allocation per packet.
    scratch: Vec<i16>,
}

impl AudioDecoder {
    pub fn new() -> anyhow::Result<Self> {
        let decoder = OpusDecoder::new(SAMPLE_RATE, CHANNELS)?;
        Ok(Self {
            decoder,
            scratch: vec![0i16; OPUS_FRAME_SIZE],
        })
    }

    pub fn decode(&mut self, opus_data: &[u8], packet_lost: bool) -> Option<Vec<i16>> {
        match self.decoder.decode(opus_data, &mut self.scratch, packet_lost) {
            Ok(n) => Some(self.scratch[..n].to_vec()),
            Err(e) => {
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
    /// Bitrate the encoder was configured with (the frontend's quality profile).
    base_bitrate_bps: u32,
    /// Bitrate the inner encoder is currently running at (base, or an override).
    active_bitrate_bps: u32,
    fps: f32,
}

impl VideoEncoder {
    pub fn new(width: u32, height: u32) -> anyhow::Result<Self> {
        Self::new_with_config(width, height, 400_000, 12.0)
    }

    pub fn new_with_config(
        width: u32,
        height: u32,
        bitrate_bps: u32,
        fps: f32,
    ) -> anyhow::Result<Self> {
        let encoder = Self::build_encoder(bitrate_bps, fps)?;
        Ok(Self {
            encoder,
            width,
            height,
            base_bitrate_bps: bitrate_bps,
            active_bitrate_bps: bitrate_bps,
            fps,
        })
    }

    fn build_encoder(bitrate_bps: u32, fps: f32) -> anyhow::Result<H264Encoder> {
        let api = OpenH264API::from_source();
        let config = EncoderConfig::new()
            .bitrate(openh264::encoder::BitRate::from_bps(bitrate_bps))
            .max_frame_rate(openh264::encoder::FrameRate::from_hz(fps))
            .rate_control_mode(openh264::encoder::RateControlMode::Bitrate);
        H264Encoder::with_api_config(api, config)
            .map_err(|e| anyhow::anyhow!("failed to create H264 encoder: {e}"))
    }

    /// Apply a temporary bitrate cap (0 clears it, restoring the base bitrate).
    /// openh264 has no safe runtime bitrate setter, so this rebuilds the inner
    /// encoder; the next encoded frame starts a fresh IDR/SPS sequence, which
    /// receivers handle the same way as any keyframe.
    pub fn apply_bitrate_override(&mut self, override_bps: u32) {
        let target = if override_bps == 0 {
            self.base_bitrate_bps
        } else {
            override_bps.min(self.base_bitrate_bps)
        };
        if target == self.active_bitrate_bps {
            return;
        }
        match Self::build_encoder(target, self.fps) {
            Ok(encoder) => {
                tracing::info!(
                    "Video encoder bitrate {} -> {target} bps",
                    self.active_bitrate_bps
                );
                self.encoder = encoder;
                self.active_bitrate_bps = target;
            }
            Err(e) => tracing::warn!("Failed to apply bitrate override: {e}"),
        }
    }

    pub fn encode(
        &mut self,
        rgba: &[u8],
        width: u32,
        height: u32,
        keyframe: bool,
    ) -> Option<Vec<u8>> {
        let expected = (width as usize)
            .checked_mul(height as usize)
            .and_then(|n| n.checked_mul(4));
        match expected {
            Some(n) if n == rgba.len() => {}
            _ => {
                tracing::warn!(
                    "RGBA buffer size mismatch: got {} for {}x{}",
                    rgba.len(),
                    width,
                    height
                );
                return None;
            }
        }

        // Bookkeeping only: openh264's Encoder re-initializes itself when the
        // incoming frame dimensions change (see openh264 encoder.rs reinit).
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
                if data.is_empty() {
                    None
                } else {
                    Some(data)
                }
            }
            Err(e) => {
                tracing::warn!("H264 encode error: {e}");
                None
            }
        }
    }
}

// ── Video Decoder (H.264 -> RGBA/JPEG) ───────────────────────────────

pub struct VideoDecoder {
    decoder: H264Decoder,
}

impl VideoDecoder {
    pub fn new() -> anyhow::Result<Self> {
        let decoder = H264Decoder::new()
            .map_err(|e| anyhow::anyhow!("failed to create H264 decoder: {e}"))?;
        Ok(Self { decoder })
    }

    pub fn decode_rgba(&mut self, h264_data: &[u8]) -> Option<(Vec<u8>, u32, u32)> {
        match self.decoder.decode(h264_data) {
            Ok(Some(frame)) => {
                let (width, height) = frame.dimensions();
                let mut rgba = vec![0u8; frame.rgba8_len()];
                frame.write_rgba8(&mut rgba);
                Some((rgba, width as u32, height as u32))
            }
            Ok(None) => None,
            Err(e) => {
                tracing::warn!("H264 decode error: {e}");
                None
            }
        }
    }
}

pub fn encode_jpeg(rgba: &[u8], width: u32, height: u32, quality: u8) -> Option<Vec<u8>> {
    let expected = (width as usize)
        .checked_mul(height as usize)
        .and_then(|n| n.checked_mul(4));
    match expected {
        Some(n) if n == rgba.len() => {}
        _ => {
            tracing::warn!(
                "JPEG encode buffer size mismatch: got {} for {}x{}",
                rgba.len(),
                width,
                height
            );
            return None;
        }
    }

    let rgb: Vec<u8> = rgba
        .chunks_exact(4)
        .flat_map(|px| [px[0], px[1], px[2]])
        .collect();

    let mut out = Vec::new();
    let encoder = JpegEncoder::new_with_quality(Cursor::new(&mut out), quality);
    if let Err(e) = encoder.write_image(&rgb, width, height, ExtendedColorType::Rgb8) {
        tracing::warn!("JPEG encode error: {e}");
        return None;
    }

    Some(out)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_audio_roundtrip() {
        let mut enc = AudioEncoder::new().expect("encoder init");
        let mut dec = AudioDecoder::new().expect("decoder init");
        let pcm: Vec<i16> = (0..960)
            .map(|i| {
                (f64::sin(2.0 * std::f64::consts::PI * 440.0 * i as f64 / 48000.0) * 16000.0) as i16
            })
            .collect();
        let encoded = enc.encode(&pcm).expect("encode failed");
        assert!(!encoded.is_empty());
        let decoded = dec.decode(&encoded, false).expect("decode failed");
        assert_eq!(decoded.len(), 960);
    }

    #[test]
    fn test_audio_rejects_wrong_frame_size() {
        let mut enc = AudioEncoder::new().expect("encoder init");
        assert!(enc.encode(&vec![0i16; 128]).is_none());
    }

    #[test]
    fn test_video_encode() {
        let mut enc = VideoEncoder::new(320, 240).expect("encoder init");
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
        assert!(is_keyframe(&encoded));
    }

    #[test]
    fn test_video_decode_and_jpeg_encode() {
        let mut enc = VideoEncoder::new(320, 240).expect("encoder init");
        let mut dec = VideoDecoder::new().expect("decoder init");
        let mut rgba = vec![0u8; (320 * 240 * 4) as usize];
        for y in 0..240u32 {
            for x in 0..320u32 {
                let idx = ((y * 320 + x) * 4) as usize;
                rgba[idx] = (x * 255 / 320) as u8;
                rgba[idx + 1] = 24;
                rgba[idx + 2] = (y * 255 / 240) as u8;
                rgba[idx + 3] = 255;
            }
        }

        let encoded = enc.encode(&rgba, 320, 240, true).expect("encode failed");
        let (decoded_rgba, width, height) = dec.decode_rgba(&encoded).expect("decode failed");
        assert_eq!((width, height), (320, 240));
        assert_eq!(decoded_rgba.len(), rgba.len());

        let jpeg = encode_jpeg(&decoded_rgba, width, height, 80).expect("jpeg encode failed");
        assert!(jpeg.starts_with(&[0xFF, 0xD8]));
        assert!(jpeg.len() > 256);
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
