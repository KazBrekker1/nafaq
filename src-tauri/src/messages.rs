use serde::{Deserialize, Serialize};

/// Stream type identifiers for binary frame protocol
pub const STREAM_AUDIO: u8 = 0x01;
pub const STREAM_VIDEO: u8 = 0x02;
pub const STREAM_CHAT: u8 = 0x03;
pub const STREAM_CONTROL: u8 = 0x04;

/// Commands from frontend → Rust backend (via Tauri invoke)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Command {
    GetNodeInfo,
    CreateCall,
    JoinCall { ticket: String },
    EndCall { peer_id: String },
    SendChat { peer_id: String, message: String },
    SendControl { peer_id: String, action: ControlAction },
}

/// Control actions sent between peers
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum ControlAction {
    Mute { muted: bool },
    VideoOff { off: bool },
    PeerAnnounce { peer_id: String, ticket: String },
}

/// Events from Rust backend → frontend (via Tauri events)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    NodeInfo { id: String, ticket: String },
    CallCreated { ticket: String },
    PeerConnected { peer_id: String },
    PeerDisconnected { peer_id: String },
    ChatReceived { peer_id: String, message: String },
    ControlReceived { peer_id: String, action: ControlAction },
    ConnectionStatus { peer_id: String, status: ConnectionStatusKind },
    Error { message: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionStatusKind {
    Direct,
    Relayed,
    Connecting,
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
    let mut buf = vec![0u8; len];
    recv.read_exact(&mut buf).await?;
    Ok(Some(buf))
}

/// Binary media frame header (for WebSocket binary messages)
/// Format: [stream_type: u8][peer_id: 32 bytes][timestamp_ms: u64][payload: ...]
/// Note: peer_id is the raw 32-byte public key, NOT the hex string.
pub struct MediaFrame {
    pub stream_type: u8,
    pub peer_id: [u8; 32],
    pub timestamp_ms: u64,
    pub payload: Vec<u8>,
}

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
}
