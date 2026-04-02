use serde::{Deserialize, Serialize};

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

/// Stream type identifiers for binary frame protocol
pub const STREAM_AUDIO: u8 = 0x01;
pub const STREAM_VIDEO: u8 = 0x02;
pub const STREAM_CHAT: u8 = 0x03;
pub const STREAM_CONTROL: u8 = 0x04;

#[derive(Debug, Clone)]
pub struct AudioPacket {
    pub peer_id: String,
    pub timestamp_ms: u64,
    pub sequence: u16,
    pub payload: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct VideoPacket {
    pub peer_id: String,
    pub timestamp_ms: u64,
    pub payload: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaBridgeMode {
    ChannelBinary,
    EventBase64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaReceiveAudioMode {
    DecodedPcm,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaReceiveVideoMode {
    DecodedJpeg,
    RawH264Nalu,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaSendIngressMode {
    InvokeRaw,
    InvokeJsonFallback,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaBridgeRegistration {
    pub session_id: String,
    pub preferred_bridge_modes: Vec<MediaBridgeMode>,
    pub playback_ready: bool,
    #[serde(default)]
    pub webcodecs_active: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaSessionProfile {
    pub session_id: String,
    pub receive_bridge_mode: MediaBridgeMode,
    pub receive_video_mode: MediaReceiveVideoMode,
    pub receive_audio_mode: MediaReceiveAudioMode,
    pub send_ingress_mode: MediaSendIngressMode,
    pub playback_ready: bool,
    pub bridge_ready: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaPlaybackStatus {
    pub session_id: String,
    pub audio_ready: bool,
    pub video_ready: bool,
    pub last_failure: Option<String>,
}

/// Commands from frontend → Rust backend (via Tauri invoke)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Command {
    GetNodeInfo,
    CreateCall,
    JoinCall {
        ticket: String,
    },
    EndCall {
        peer_id: String,
    },
    SendChat {
        peer_id: String,
        message: String,
    },
    SendControl {
        peer_id: String,
        action: ControlAction,
    },
}

/// Control actions sent between peers
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum ControlAction {
    Heartbeat,
    Mute { muted: bool },
    VideoOff { off: bool },
    PeerAnnounce { peer_id: String, ticket: String },
    VideoQualityRequest { layer: VideoLayerRequest },
    KeyframeRequest { layer: VideoLayerRequest },
    SetDisplayName { name: String },
    PerPeerQualityBps { bitrate_bps: u32 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VideoLayerRequest {
    High,
    Low,
    None,
}

impl VideoLayerRequest {
    pub fn to_u8(self) -> u8 {
        match self {
            Self::High => 0,
            Self::Low => 1,
            Self::None => 2,
        }
    }
}

/// Events from Rust backend → frontend (via Tauri events)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    NodeInfo {
        id: String,
        ticket: String,
    },
    CallCreated {
        ticket: String,
    },
    PeerConnected {
        peer_id: String,
    },
    PeerDisconnected {
        peer_id: String,
    },
    ChatReceived {
        peer_id: String,
        message: String,
    },
    ControlReceived {
        peer_id: String,
        action: ControlAction,
    },
    ConnectionStatus {
        peer_id: String,
        status: ConnectionStatusKind,
    },
    Error {
        message: String,
    },
    QualityProfileChanged {
        peer_count: usize,
        bitrate_bps: u32,
        fps: u32,
        max_width: u32,
        max_height: u32,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionStatusKind {
    Direct,
    Relayed,
    Connecting,
}

#[derive(Debug, Clone)]
pub struct AudioDatagram {
    pub sequence: u16,
    pub timestamp_ms: u64,
    pub payload: Vec<u8>,
}

impl AudioDatagram {
    pub const HEADER_SIZE: usize = 2 + 8;

    pub fn encode(sequence: u16, timestamp_ms: u64, payload: &[u8]) -> Vec<u8> {
        let mut buf = Vec::with_capacity(Self::HEADER_SIZE + payload.len());
        buf.extend_from_slice(&sequence.to_be_bytes());
        buf.extend_from_slice(&timestamp_ms.to_be_bytes());
        buf.extend_from_slice(payload);
        buf
    }

    pub fn decode(data: &[u8]) -> Option<Self> {
        if data.len() < Self::HEADER_SIZE {
            return None;
        }
        let sequence = u16::from_be_bytes(data[..2].try_into().ok()?);
        let timestamp_ms = u64::from_be_bytes(data[2..10].try_into().ok()?);
        Some(Self {
            sequence,
            timestamp_ms,
            payload: data[10..].to_vec(),
        })
    }
}

/// Write a length-prefixed message to a QUIC stream.
/// Format: [len: u32 big-endian][payload: len bytes]
pub async fn write_framed(
    send: &mut iroh::endpoint::SendStream,
    data: &[u8],
) -> Result<(), iroh::endpoint::WriteError> {
    let len = (data.len() as u32).to_be_bytes();
    send.write_all(&len).await?;
    send.write_all(data).await?;
    Ok(())
}

/// Maximum frame size (10 MB) — prevents OOM from malicious length prefixes.
const MAX_FRAME_SIZE: usize = 10 * 1024 * 1024;

/// Read a length-prefixed message from a QUIC stream.
/// Returns None if the stream is finished.
pub async fn read_framed(
    recv: &mut iroh::endpoint::RecvStream,
) -> Result<Option<Vec<u8>>, iroh::endpoint::ReadExactError> {
    let mut len_buf = [0u8; 4];
    match recv.read_exact(&mut len_buf).await {
        Ok(()) => {}
        Err(e) => {
            return Err(e);
        }
    }
    let len = u32::from_be_bytes(len_buf) as usize;
    if len > MAX_FRAME_SIZE {
        tracing::warn!("Frame too large ({len} bytes), dropping connection");
        return Ok(None);
    }
    let mut buf = vec![0u8; len];
    recv.read_exact(&mut buf).await?;
    Ok(Some(buf))
}

/// Binary media frame header (for WebSocket binary messages)
/// Format: [stream_type: u8][peer_id: 32 bytes][timestamp_ms: u64][payload: ...]
/// Note: peer_id is the raw 32-byte public key, NOT the hex string.
#[cfg(test)]
pub struct MediaFrame {
    pub stream_type: u8,
    pub peer_id: [u8; 32],
    pub timestamp_ms: u64,
    pub payload: Vec<u8>,
}

#[cfg(test)]
impl MediaFrame {
    pub const HEADER_SIZE: usize = 1 + 32 + 8; // 41 bytes

    pub fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(Self::HEADER_SIZE + self.payload.len());
        buf.push(self.stream_type);
        buf.extend_from_slice(&self.peer_id);
        buf.extend_from_slice(&self.timestamp_ms.to_be_bytes());
        buf.extend_from_slice(&self.payload);
        buf
    }

    pub fn decode(data: &[u8]) -> Option<Self> {
        if data.len() < Self::HEADER_SIZE {
            return None;
        }
        let stream_type = data[0];
        let mut peer_id = [0u8; 32];
        peer_id.copy_from_slice(&data[1..33]);
        let timestamp_ms = u64::from_be_bytes(data[33..41].try_into().ok()?);
        let payload = data[41..].to_vec();
        Some(Self {
            stream_type,
            peer_id,
            timestamp_ms,
            payload,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_command_serialize_get_node_info() {
        let cmd = Command::GetNodeInfo;
        let json = serde_json::to_string(&cmd).unwrap();
        assert_eq!(json, r#"{"type":"get_node_info"}"#);
    }

    #[test]
    fn test_command_serialize_join_call() {
        let cmd = Command::JoinCall {
            ticket: "abc123".into(),
        };
        let json = serde_json::to_string(&cmd).unwrap();
        let parsed: Command = serde_json::from_str(&json).unwrap();
        match parsed {
            Command::JoinCall { ticket } => assert_eq!(ticket, "abc123"),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn test_event_serialize_peer_connected() {
        let evt = Event::PeerConnected {
            peer_id: "deadbeef".into(),
        };
        let json = serde_json::to_string(&evt).unwrap();
        assert!(json.contains("peer_connected"));
        assert!(json.contains("deadbeef"));
    }

    #[test]
    fn test_media_frame_roundtrip() {
        let frame = MediaFrame {
            stream_type: STREAM_AUDIO,
            peer_id: [0xAB; 32],
            timestamp_ms: 1234567890,
            payload: vec![1, 2, 3, 4, 5],
        };
        let encoded = frame.encode();
        assert_eq!(encoded.len(), MediaFrame::HEADER_SIZE + 5);

        let decoded = MediaFrame::decode(&encoded).unwrap();
        assert_eq!(decoded.stream_type, STREAM_AUDIO);
        assert_eq!(decoded.peer_id, [0xAB; 32]);
        assert_eq!(decoded.timestamp_ms, 1234567890);
        assert_eq!(decoded.payload, vec![1, 2, 3, 4, 5]);
    }

    #[test]
    fn test_media_frame_decode_too_short() {
        let data = vec![0; 10];
        assert!(MediaFrame::decode(&data).is_none());
    }

    #[test]
    fn test_control_action_serialize() {
        let action = ControlAction::PeerAnnounce {
            peer_id: "abc".into(),
            ticket: "ticket123".into(),
        };
        let json = serde_json::to_string(&action).unwrap();
        assert!(json.contains("peer_announce"));
    }

    #[test]
    fn test_audio_datagram_roundtrip() {
        let encoded = AudioDatagram::encode(42, 1234, &[1, 2, 3, 4]);
        let decoded = AudioDatagram::decode(&encoded).unwrap();
        assert_eq!(decoded.sequence, 42);
        assert_eq!(decoded.timestamp_ms, 1234);
        assert_eq!(decoded.payload, vec![1, 2, 3, 4]);
    }

    #[test]
    fn test_audio_datagram_decode_too_short() {
        assert!(AudioDatagram::decode(&[1, 2, 3]).is_none());
    }
}
