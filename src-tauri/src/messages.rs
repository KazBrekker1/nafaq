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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DmMessage {
    Text {
        content: String,
        timestamp: u64,
        /// Client-generated message id, used for delivery ACKs and dedup.
        /// Optional + defaulted so frames from an older peer (which never
        /// sent this field) still deserialize cleanly.
        #[serde(default)]
        id: Option<String>,
    },
    FileStart {
        name: String,
        size: u64,
        id: String,
    },
    FileChunk {
        id: String,
        offset: u64,
        #[serde(with = "file_chunk_data")]
        data: Vec<u8>,
    },
    FileEnd {
        id: String,
    },
    CallInvite {
        ticket: String,
    },
    CallDecline,
    /// Caller gives up on a pending invite (explicit cancel or ring timeout)
    /// before the callee answered. New variant — see wire-compat note below.
    CallCancel,
    /// Delivery acknowledgement for a `Text` message carrying an `id`. New
    /// variant — see wire-compat note below.
    Ack {
        id: String,
    },
    Heartbeat,
}

// Wire-compat note (additive enum variants over serde_json):
// DmMessage frames are encoded with `serde_json::to_vec` and decoded with
// `serde_json::from_slice::<DmMessage>` (see `write_framed`/`read_framed`
// below). Every call site that parses an inbound frame does so as
// `if let Ok(dm_msg) = serde_json::from_slice::<DmMessage>(data) { .. }` (or
// equivalent) and silently drops the frame on a parse error — there is no
// site that propagates an unknown-variant error up to close the DM stream.
// That means an OLDER peer (pre-`CallCancel`/`Ack`) receiving one of these
// new variants gets a JSON deserialize error for that single frame, drops it,
// and keeps reading — the stream itself is unaffected. So `CallCancel` and
// `Ack` are safe additive variants for one-way skew (new -> old). The
// `Text.id` field is `Option` + `#[serde(default)]`, so an old peer's
// id-less `Text` still deserializes on a new receiver, and a new peer's
// `Text{id: Some(_)}` still deserializes on an old receiver (unknown fields
// are ignored by default; no `deny_unknown_fields` is set on this enum).

/// `FileChunk.data` travels as base64 (a JSON number array was ~3.5x
/// larger on the wire and slow to parse).
mod file_chunk_data {
    use base64::Engine;
    use serde::{Deserialize, Deserializer, Serializer};

    const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

    pub fn serialize<S: Serializer>(data: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&B64.encode(data))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<u8>, D::Error> {
        let encoded = <std::borrow::Cow<'de, str>>::deserialize(deserializer)?;
        B64.decode(encoded.as_bytes()).map_err(serde::de::Error::custom)
    }
}

/// Stream type identifiers for binary frame protocol. 0x01 was a legacy
/// uni-stream audio channel (audio travels as datagrams); don't reuse it.
pub const STREAM_VIDEO: u8 = 0x02;
pub const STREAM_CHAT: u8 = 0x03;
pub const STREAM_CONTROL: u8 = 0x04;
pub const STREAM_DM: u8 = 0x05;

#[derive(Debug, Clone)]
pub struct AudioPacket {
    pub peer_id: String,
    /// `stable_id` of the QUIC connection the packet arrived on. The sender's
    /// sequence counter restarts with every connection, so receivers reset
    /// their per-peer sequence/decoder state when this changes.
    pub connection_id: usize,
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
pub enum MediaReceiveVideoMode {
    DecodedJpeg,
    RawH264Nalu,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaBridgeRegistration {
    pub session_id: String,
    pub preferred_bridge_modes: Vec<MediaBridgeMode>,
    #[serde(default)]
    pub webcodecs_active: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaSessionProfile {
    pub session_id: String,
    pub receive_bridge_mode: MediaBridgeMode,
    pub receive_video_mode: MediaReceiveVideoMode,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelayStatusKind {
    Starting,
    Connecting,
    Online,
    Degraded,
    Offline,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PeerConnectionKind {
    Idle,
    Connecting,
    Connected,
    Suspect,
    Reconnecting,
    Disconnected,
    Failed,
}

/// Events from Rust backend → frontend (via Tauri events)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
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
    PeerConnectionStatusChanged {
        peer_id: String,
        status: PeerConnectionKind,
        reason: Option<String>,
    },
    QualityProfileChanged {
        peer_count: usize,
        bitrate_bps: u32,
        fps: u32,
        max_width: u32,
        max_height: u32,
    },
    RelayStatusChanged {
        status: RelayStatusKind,
        relay_url: String,
        node_id: String,
        ticket_available: bool,
        message: Option<String>,
    },
    TicketRefreshed {
        ticket: String,
    },
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
        ticket: String,
    },
    CallDeclineReceived {
        peer_id: String,
    },
    CallCancelReceived {
        peer_id: String,
    },
    DmAckReceived {
        peer_id: String,
        id: String,
    },
    DmFileSaved {
        peer_id: String,
        file_id: String,
        local_path: String,
    },
    DmFileTransferFailed {
        peer_id: String,
        file_id: String,
        /// Human-readable cause, for logs/UI. Additive field.
        reason: Option<String>,
    },
    DmFileProgress {
        peer_id: String,
        file_id: String,
        received: u64,
    },
    PresenceChanged {
        peer_id: String,
        online: bool,
    },
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

/// Frame size cap for chat streams: chat messages are raw UTF-8 and the
/// sender rejects anything over 64 KiB (`commands::MAX_CHAT_LEN`).
pub const MAX_CHAT_FRAME_BYTES: usize = 64 * 1024;
/// Frame size cap for control streams: small JSON `ControlAction`s (the
/// largest is a `PeerAnnounce` carrying a ticket of at most a few KiB).
pub const MAX_CONTROL_FRAME_BYTES: usize = 64 * 1024;
/// Frame size cap for DM streams. The largest legitimate frames:
/// - a 64 KiB `Text` whose JSON escaping expands control chars 6x
///   (`\u0001`) = 384 KiB, plus envelope;
/// - a 64 KiB `FileChunk` as base64 = ~88 KiB.
///
/// 512 KiB covers both with headroom (was a blanket 10 MiB).
pub const MAX_DM_FRAME_BYTES: usize = 512 * 1024;

/// Frames are read in slices of this size, so a peer declaring a large
/// length only costs memory for bytes it actually sends.
const FRAME_READ_SLICE: usize = 16 * 1024;

/// Read a length-prefixed message from a QUIC stream.
/// Returns None if the stream is finished, or if the peer declared a frame
/// larger than `max_len` (the caller stops reading the stream).
pub async fn read_framed(
    recv: &mut iroh::endpoint::RecvStream,
    max_len: usize,
) -> Result<Option<Vec<u8>>, iroh::endpoint::ReadExactError> {
    let mut len_buf = [0u8; 4];
    recv.read_exact(&mut len_buf).await?;
    let len = u32::from_be_bytes(len_buf) as usize;
    if len > max_len {
        tracing::warn!("Frame too large ({len} > {max_len} bytes), dropping stream");
        return Ok(None);
    }
    // Grow the buffer as data arrives instead of allocating `len` up front.
    let mut buf = Vec::with_capacity(len.min(FRAME_READ_SLICE));
    while buf.len() < len {
        let start = buf.len();
        let slice = (len - start).min(FRAME_READ_SLICE);
        buf.resize(start + slice, 0);
        recv.read_exact(&mut buf[start..]).await?;
    }
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
            stream_type: STREAM_VIDEO,
            peer_id: [0xAB; 32],
            timestamp_ms: 1234567890,
            payload: vec![1, 2, 3, 4, 5],
        };
        let encoded = frame.encode();
        assert_eq!(encoded.len(), MediaFrame::HEADER_SIZE + 5);

        let decoded = MediaFrame::decode(&encoded).unwrap();
        assert_eq!(decoded.stream_type, STREAM_VIDEO);
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

    #[test]
    fn test_dm_text_id_roundtrip() {
        let msg = DmMessage::Text {
            content: "hi".into(),
            timestamp: 42,
            id: Some("client-id-1".into()),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: DmMessage = serde_json::from_str(&json).unwrap();
        match parsed {
            DmMessage::Text { id, .. } => assert_eq!(id.as_deref(), Some("client-id-1")),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn test_dm_text_without_id_defaults_to_none() {
        // Simulates a frame from an older peer that never had the `id` field.
        let json = r#"{"type":"text","content":"hi","timestamp":42}"#;
        let parsed: DmMessage = serde_json::from_str(json).unwrap();
        match parsed {
            DmMessage::Text { id, .. } => assert_eq!(id, None),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn file_chunk_data_round_trips_as_base64() {
        let msg = DmMessage::FileChunk {
            id: "t".into(),
            offset: 0,
            data: vec![1, 2, 255],
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""data":"AQL/""#), "{json}");
        match serde_json::from_str::<DmMessage>(&json).unwrap() {
            DmMessage::FileChunk { data, .. } => assert_eq!(data, vec![1, 2, 255]),
            other => panic!("wrong variant {other:?}"),
        }
        let bad = r#"{"type":"file_chunk","id":"t","offset":0,"data":"not base64!"}"#;
        assert!(serde_json::from_str::<DmMessage>(bad).is_err());
    }

    #[test]
    fn test_dm_ack_roundtrip() {
        let msg = DmMessage::Ack {
            id: "client-id-1".into(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"ack\""));
        let parsed: DmMessage = serde_json::from_str(&json).unwrap();
        match parsed {
            DmMessage::Ack { id } => assert_eq!(id, "client-id-1"),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn test_dm_call_cancel_roundtrip() {
        let json = serde_json::to_string(&DmMessage::CallCancel).unwrap();
        assert_eq!(json, r#"{"type":"call_cancel"}"#);
        assert!(matches!(
            serde_json::from_str::<DmMessage>(&json).unwrap(),
            DmMessage::CallCancel
        ));
    }

    #[test]
    fn test_dm_message_unknown_variant_is_tolerated_not_propagated() {
        // An unrecognized `type` tag (e.g. a future variant an older build
        // doesn't know about) must fail to parse as a single bad frame, not
        // panic or produce something callers mistake for a known variant.
        // Every read site in connection.rs treats this Err as "drop the
        // frame, keep reading" rather than closing the stream.
        let json = r#"{"type":"some_future_variant","stuff":1}"#;
        assert!(serde_json::from_str::<DmMessage>(json).is_err());
    }
}
