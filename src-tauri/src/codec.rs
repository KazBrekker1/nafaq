use opus::{Encoder as OpusEncoder, Decoder as OpusDecoder, Channels, Application};
use openh264::encoder::{Encoder as H264Encoder, EncoderConfig};
use openh264::decoder::Decoder as H264Decoder;
use openh264::formats::{RgbaSliceU8, YUVBuffer, YUVSource};
use openh264::OpenH264API;

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

// ── VideoCodec (H.264 via OpenH264) ─────────────────────────────────────

pub struct VideoCodec {
    encoder: H264Encoder,
    decoder: H264Decoder,
    width: u32,
    height: u32,
    force_keyframe: bool,
}

impl VideoCodec {
    pub fn new(width: u32, height: u32) -> Self {
        let api = OpenH264API::from_source();
        let config = EncoderConfig::new();
        let encoder = H264Encoder::with_api_config(api, config)
            .expect("failed to create H264 encoder");
        let decoder = H264Decoder::new()
            .expect("failed to create H264 decoder");
        Self { encoder, decoder, width, height, force_keyframe: false }
    }

    pub fn encode(&mut self, rgba: &[u8], width: u32, height: u32, keyframe: bool) -> Option<Vec<u8>> {
        assert_eq!(rgba.len(), (width * height * 4) as usize, "RGBA buffer size mismatch");

        // Handle resolution change
        if width != self.width || height != self.height {
            self.width = width;
            self.height = height;
            // OpenH264 encoder auto-reinitializes on dimension change
        }

        if keyframe {
            self.encoder.force_intra_frame();
        }

        // Convert RGBA to YUV via openh264's built-in conversion
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

    pub fn decode(&mut self, h264_data: &[u8]) -> Option<(Vec<u8>, u32, u32)> {
        match self.decoder.decode(h264_data) {
            Ok(Some(yuv)) => {
                let (w, h) = yuv.dimensions();
                let mut rgba = vec![0u8; w * h * 4];
                yuv.write_rgba8(&mut rgba);
                Some((rgba, w as u32, h as u32))
            }
            Ok(None) => None, // no frame produced yet (buffering)
            Err(e) => {
                tracing::warn!("H264 decode error: {e}");
                None
            }
        }
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

        let encoded = codec.encode(&rgba, width, height, true).expect("H264 encode failed");
        assert!(!encoded.is_empty());
        assert!(encoded.len() < rgba.len(), "H264 should compress video");

        let (decoded_rgba, dec_w, dec_h) = codec.decode(&encoded).expect("H264 decode failed");
        assert_eq!(dec_w, width);
        assert_eq!(dec_h, height);
        assert_eq!(decoded_rgba.len(), (width * height * 4) as usize);
        assert!(decoded_rgba.iter().any(|&b| b != 0), "Decoded frame should not be all zeros");
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
