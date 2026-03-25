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
}
