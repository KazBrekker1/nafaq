# Nafaq Sidecar Implementation Plan (Plan 1 of 3)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the Iroh-based Rust sidecar that handles all P2P networking, QUIC stream management, and WebSocket IPC with the Electrobun app.

**Architecture:** Standalone Rust binary running an Iroh Endpoint with a Router for incoming connections. A WebSocket server on localhost accepts commands from the Bun process and forwards media/chat data bidirectionally between the WebSocket and QUIC streams. Uses tokio channels for internal coordination.

**Tech Stack:** Rust 1.89, iroh 0.97, iroh-tickets, tokio, tokio-tungstenite, serde/serde_json, tracing

**Related plans:**
- Plan 2: `2026-03-25-nafaq-electrobun-bridge.md` (Electrobun app + Bun bridge)
- Plan 3: `2026-03-25-nafaq-frontend.md` (Vue/NuxtUI frontend)

---

## File Structure

```
nafaq/
└── sidecar/
    ├── Cargo.toml
    └── src/
        ├── main.rs           # Entry point: CLI args, start endpoint + WS server
        ├── ipc.rs            # WebSocket IPC server, message routing
        ├── messages.rs       # IPC message types (JSON + binary frame format)
        ├── node.rs           # Iroh endpoint lifecycle, ticket generation
        ├── connection.rs     # Peer connection manager, stream lifecycle
        ├── protocol.rs       # ProtocolHandler impl, stream type routing
        └── tests/
            └── integration.rs  # Two-node integration tests
```

## Key Design Decisions

**Single ALPN, multiplexed streams:** Instead of one ALPN per media type, use a single `nafaq/call/1` ALPN. Within each connection, streams are differentiated by a type byte prefix:
- `0x01` = Audio (unidirectional send stream)
- `0x02` = Video (unidirectional send stream)
- `0x03` = Chat (bidirectional stream)
- `0x04` = Control (bidirectional stream)

**WebSocket protocol:** JSON for control messages, binary for media frames. Binary format: `[type: u8][peer_id: 32 bytes][timestamp_ms: u64][payload]`.

**Concurrency model:** `tokio::sync::broadcast` channel for sending events to the WebSocket client. `tokio::sync::mpsc` for receiving commands. Each peer connection spawns tasks for reading each incoming stream.

---

### Task 1: Project Scaffolding

**Files:**
- Create: `sidecar/Cargo.toml`
- Create: `sidecar/src/main.rs`
- Create: `sidecar/src/messages.rs`
- Create: `sidecar/src/node.rs`
- Create: `sidecar/src/connection.rs`
- Create: `sidecar/src/protocol.rs`
- Create: `sidecar/src/ipc.rs`

- [ ] **Step 1: Create Cargo.toml**

```toml
[package]
name = "nafaq-sidecar"
version = "0.1.0"
edition = "2021"

[dependencies]
iroh = "0.97"
iroh-tickets = "0.4"
tokio = { version = "1", features = ["full"] }
tokio-tungstenite = "0.26"
futures-util = "0.3"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
bytes = "1"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
clap = { version = "4", features = ["derive"] }
anyhow = "1"

[dev-dependencies]
tokio-test = "0.4"
```

- [ ] **Step 2: Create stub files**

`sidecar/src/main.rs`:
```rust
mod connection;
mod ipc;
mod messages;
mod node;
mod protocol;

use clap::Parser;

#[derive(Parser)]
#[command(name = "nafaq-sidecar")]
struct Cli {
    /// WebSocket port for IPC with Electrobun
    #[arg(short, long, default_value = "9320")]
    port: u16,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("nafaq_sidecar=info".parse()?),
        )
        .init();

    let cli = Cli::parse();
    tracing::info!("nafaq-sidecar starting on port {}", cli.port);

    // TODO: wire up endpoint + ws server
    Ok(())
}
```

`sidecar/src/messages.rs`:
```rust
// IPC message types — implemented in Task 2
```

`sidecar/src/node.rs`:
```rust
// Iroh endpoint lifecycle — implemented in Task 3
```

`sidecar/src/connection.rs`:
```rust
// Peer connection manager — implemented in Task 5
```

`sidecar/src/protocol.rs`:
```rust
// Protocol handler — implemented in Task 4
```

`sidecar/src/ipc.rs`:
```rust
// WebSocket IPC server — implemented in Task 6
```

- [ ] **Step 3: Verify it compiles**

Run: `cd sidecar && cargo build`
Expected: Compiles with warnings about unused modules

- [ ] **Step 4: Commit**

```bash
git add sidecar/
git commit -m "feat(sidecar): scaffold Rust project with dependencies"
```

---

### Task 2: IPC Message Types

**Files:**
- Modify: `sidecar/src/messages.rs`

- [ ] **Step 1: Write tests for JSON message serialization**

Add to `sidecar/src/messages.rs`:
```rust
use serde::{Deserialize, Serialize};

/// Stream type identifiers for binary frame protocol
pub const STREAM_AUDIO: u8 = 0x01;
pub const STREAM_VIDEO: u8 = 0x02;
pub const STREAM_CHAT: u8 = 0x03;
pub const STREAM_CONTROL: u8 = 0x04;

/// Messages from Bun → Sidecar (JSON over WebSocket text frames)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Command {
    /// Request node info (ID, address, ticket)
    GetNodeInfo,
    /// Create a new call (generates a ticket)
    CreateCall,
    /// Join an existing call using a ticket
    JoinCall { ticket: String },
    /// End connection with a specific peer
    EndCall { peer_id: String },
    /// Send a chat message to a peer
    SendChat { peer_id: String, message: String },
    /// Send a control action to a peer
    SendControl { peer_id: String, action: ControlAction },
}

/// Control actions sent between peers
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum ControlAction {
    Mute { muted: bool },
    VideoOff { off: bool },
    /// Host announces a new peer to all participants (for mesh formation)
    PeerAnnounce { peer_id: String, ticket: String },
}

/// Events from Sidecar → Bun (JSON over WebSocket text frames)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    /// Response to GetNodeInfo
    NodeInfo { id: String, ticket: String },
    /// A call was created, here's the ticket to share
    CallCreated { ticket: String },
    /// A peer connected
    PeerConnected { peer_id: String },
    /// A peer disconnected
    PeerDisconnected { peer_id: String },
    /// Chat message received from a peer
    ChatReceived { peer_id: String, message: String },
    /// Control action received from a peer
    ControlReceived { peer_id: String, action: ControlAction },
    /// Connection status change
    ConnectionStatus { peer_id: String, status: ConnectionStatusKind },
    /// Error occurred
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
            // Stream finished = no more messages
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
        let data = vec![0; 10]; // less than HEADER_SIZE
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
```

- [ ] **Step 2: Run tests**

Run: `cd sidecar && cargo test -- messages`
Expected: All 6 tests pass

- [ ] **Step 3: Commit**

```bash
git add sidecar/src/messages.rs
git commit -m "feat(sidecar): define IPC message types with JSON and binary frame protocol"
```

---

### Task 3: Iroh Endpoint Lifecycle

**Files:**
- Modify: `sidecar/src/node.rs`

- [ ] **Step 1: Write tests for node creation and ticket generation**

```rust
use anyhow::Result;
use iroh::{Endpoint, endpoint::presets};
use iroh_tickets::endpoint::EndpointTicket;

/// Creates and configures an Iroh endpoint for the nafaq protocol.
pub const NAFAQ_ALPN: &[u8] = b"nafaq/call/1";

pub async fn create_endpoint() -> Result<Endpoint> {
    let endpoint = Endpoint::builder(presets::N0)
        .alpns(vec![NAFAQ_ALPN.to_vec()])
        .bind()
        .await?;

    // Wait until we're connected to a relay so our address is reachable
    endpoint.online().await;

    tracing::info!("Iroh endpoint started with ID: {}", endpoint.id());
    Ok(endpoint)
}

/// Generate a shareable ticket string from the endpoint's current address.
pub fn generate_ticket(endpoint: &Endpoint) -> String {
    let ticket = EndpointTicket::new(endpoint.addr());
    ticket.serialize()
}

/// Parse a ticket string back into an EndpointTicket.
pub fn parse_ticket(ticket_str: &str) -> Result<EndpointTicket> {
    let ticket = EndpointTicket::deserialize(ticket_str)?;
    Ok(ticket)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_create_endpoint() {
        let endpoint = create_endpoint().await.unwrap();
        // Should have a valid ID (32-byte public key)
        let id = endpoint.id();
        assert!(!id.to_string().is_empty());
        endpoint.close().await;
    }

    #[tokio::test]
    async fn test_ticket_roundtrip() {
        let endpoint = create_endpoint().await.unwrap();
        let ticket_str = generate_ticket(&endpoint);

        // Should be a non-empty base32 string
        assert!(!ticket_str.is_empty());

        // Should parse back successfully
        let ticket = parse_ticket(&ticket_str).unwrap();
        assert_eq!(ticket.endpoint_addr().id, endpoint.id().into());

        endpoint.close().await;
    }
}
```

- [ ] **Step 2: Run tests**

Run: `cd sidecar && cargo test -- node`
Expected: Both tests pass (may take a few seconds for relay connection)

- [ ] **Step 3: Commit**

```bash
git add sidecar/src/node.rs
git commit -m "feat(sidecar): Iroh endpoint lifecycle with ticket generation"
```

---

### Task 4: Protocol Handler

**Files:**
- Modify: `sidecar/src/protocol.rs`

- [ ] **Step 1: Implement ProtocolHandler for incoming connections**

```rust
use std::sync::Arc;

use anyhow::Result;
use iroh::{
    endpoint::Connection,
    protocol::{AcceptError, ProtocolHandler},
};
use tokio::sync::mpsc;
use tracing;

use crate::connection::ConnectionManager;
use crate::messages::Event;

/// Protocol handler that the Router dispatches incoming connections to.
#[derive(Debug, Clone)]
pub struct NafaqProtocol {
    conn_manager: Arc<ConnectionManager>,
}

impl NafaqProtocol {
    pub fn new(conn_manager: Arc<ConnectionManager>) -> Self {
        Self { conn_manager }
    }
}

impl ProtocolHandler for NafaqProtocol {
    async fn accept(&self, connection: Connection) -> std::result::Result<(), AcceptError> {
        let peer_id = connection.remote_id();
        tracing::info!("Accepted incoming connection from {peer_id}");

        self.conn_manager
            .handle_incoming(connection)
            .await
            .map_err(|e| AcceptError::from(e))?;

        Ok(())
    }
}
```

- [ ] **Step 2: Verify it compiles**

Run: `cd sidecar && cargo check`
Expected: Compiles (ConnectionManager not yet implemented, so this will fail — that's expected, move to Task 5)

- [ ] **Step 3: Commit**

```bash
git add sidecar/src/protocol.rs
git commit -m "feat(sidecar): protocol handler for incoming connections"
```

---

### Task 5: Connection Manager

**Files:**
- Modify: `sidecar/src/connection.rs`

This is the core component. It manages peer connections, opens/accepts QUIC streams, and bridges data between streams and the WebSocket IPC.

- [ ] **Step 1: Implement ConnectionManager**

```rust
use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use iroh::{
    endpoint::{Connection, RecvStream, SendStream},
    EndpointId,
};
use tokio::sync::{broadcast, mpsc, Mutex};
use tracing;

use crate::messages::{
    ControlAction, Event, MediaFrame, STREAM_AUDIO, STREAM_CHAT, STREAM_CONTROL, STREAM_VIDEO,
};

/// Tracks the state of a single peer connection.
struct PeerConnection {
    connection: Connection,
    audio_send: Option<SendStream>,
    video_send: Option<SendStream>,
    chat_send: Option<SendStream>,
    control_send: Option<SendStream>,
}

/// Manages all peer connections and routes data between QUIC streams and IPC.
pub struct ConnectionManager {
    peers: Mutex<HashMap<String, PeerConnection>>,
    event_tx: broadcast::Sender<Event>,
    media_tx: broadcast::Sender<Vec<u8>>,
}

impl std::fmt::Debug for ConnectionManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConnectionManager").finish()
    }
}

impl ConnectionManager {
    pub fn new(
        event_tx: broadcast::Sender<Event>,
        media_tx: broadcast::Sender<Vec<u8>>,
    ) -> Self {
        Self {
            peers: Mutex::new(HashMap::new()),
            event_tx,
            media_tx,
        }
    }

    /// Handle an incoming connection (called by NafaqProtocol).
    pub async fn handle_incoming(&self, connection: Connection) -> Result<()> {
        let peer_id = connection.remote_id().to_string();
        tracing::info!("Setting up incoming connection from {peer_id}");

        self.setup_connection(peer_id, connection).await
    }

    /// Initiate an outgoing connection to a peer.
    pub async fn connect_to_peer(
        &self,
        endpoint: &iroh::Endpoint,
        addr: iroh::EndpointAddr,
    ) -> Result<String> {
        let connection = endpoint
            .connect(addr, crate::node::NAFAQ_ALPN)
            .await?;
        let peer_id = connection.remote_id().to_string();
        tracing::info!("Connected to peer {peer_id}");

        self.setup_connection(peer_id.clone(), connection).await?;
        Ok(peer_id)
    }

    /// Set up stream handling for a connection (both incoming and outgoing).
    async fn setup_connection(&self, peer_id: String, connection: Connection) -> Result<()> {
        // Open outgoing streams with type byte prefix
        let mut audio_send = connection.open_uni().await?;
        audio_send.write_all(&[STREAM_AUDIO]).await?;

        let mut video_send = connection.open_uni().await?;
        video_send.write_all(&[STREAM_VIDEO]).await?;

        let (mut chat_send, _chat_recv_initial) = connection.open_bi().await?;
        chat_send.write_all(&[STREAM_CHAT]).await?;

        let (mut control_send, _control_recv_initial) = connection.open_bi().await?;
        control_send.write_all(&[STREAM_CONTROL]).await?;

        let peer_conn = PeerConnection {
            connection: connection.clone(),
            audio_send: Some(audio_send),
            video_send: Some(video_send),
            chat_send: Some(chat_send),
            control_send: Some(control_send),
        };

        {
            let mut peers = self.peers.lock().await;
            peers.insert(peer_id.clone(), peer_conn);
        }

        // Notify Bun that a peer connected
        let _ = self.event_tx.send(Event::PeerConnected {
            peer_id: peer_id.clone(),
        });

        // Spawn tasks to accept and handle incoming streams from this peer
        self.spawn_stream_receivers(peer_id.clone(), connection);

        Ok(())
    }

    /// Spawn tasks that accept incoming streams from a peer and forward data to IPC.
    fn spawn_stream_receivers(&self, peer_id: String, connection: Connection) {
        let event_tx = self.event_tx.clone();
        let media_tx = self.media_tx.clone();
        let peer_id_clone = peer_id.clone();

        // Accept unidirectional streams (audio/video from peer)
        tokio::spawn(async move {
            loop {
                match connection.accept_uni().await {
                    Ok(mut recv) => {
                        let peer_id = peer_id_clone.clone();
                        let media_tx = media_tx.clone();

                        // Read type byte
                        let mut type_buf = [0u8; 1];
                        if recv.read_exact(&mut type_buf).await.is_err() {
                            continue;
                        }

                        tokio::spawn(async move {
                            Self::handle_uni_stream(type_buf[0], &peer_id, recv, media_tx).await;
                        });
                    }
                    Err(_) => {
                        tracing::info!("Uni stream accept ended for peer {peer_id_clone}");
                        break;
                    }
                }
            }
        });

        let event_tx_bi = event_tx.clone();
        let peer_id_bi = peer_id.clone();
        let connection_bi = connection.clone();

        // Accept bidirectional streams (chat/control from peer)
        tokio::spawn(async move {
            loop {
                match connection_bi.accept_bi().await {
                    Ok((_, mut recv)) => {
                        let peer_id = peer_id_bi.clone();
                        let event_tx = event_tx_bi.clone();

                        // Read type byte
                        let mut type_buf = [0u8; 1];
                        if recv.read_exact(&mut type_buf).await.is_err() {
                            continue;
                        }

                        tokio::spawn(async move {
                            Self::handle_bi_stream(type_buf[0], &peer_id, recv, event_tx).await;
                        });
                    }
                    Err(_) => {
                        tracing::info!("Bi stream accept ended for peer {peer_id_bi}");
                        break;
                    }
                }
            }
        });
    }

    /// Handle an incoming unidirectional stream (audio or video).
    async fn handle_uni_stream(
        stream_type: u8,
        peer_id: &str,
        mut recv: RecvStream,
        media_tx: broadcast::Sender<Vec<u8>>,
    ) {
        let mut buf = vec![0u8; 65536]; // 64KB buffer
        loop {
            match recv.read(&mut buf).await {
                Ok(Some(n)) => {
                    // Reconstruct media frame for IPC using raw public key bytes
                    let peer_id_bytes: [u8; 32] = peer_id
                        .parse::<iroh::EndpointId>()
                        .map(|id| *id.as_bytes())
                        .unwrap_or([0u8; 32]);

                    let frame = MediaFrame {
                        stream_type,
                        peer_id: peer_id_bytes,
                        timestamp_ms: std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_millis() as u64,
                        payload: buf[..n].to_vec(),
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

    /// Handle an incoming bidirectional stream (chat or control) with length-prefixed framing.
    async fn handle_bi_stream(
        stream_type: u8,
        peer_id: &str,
        mut recv: RecvStream,
        event_tx: broadcast::Sender<Event>,
    ) {
        loop {
            match crate::messages::read_framed(&mut recv).await {
                Ok(Some(data)) => {
                    match stream_type {
                        STREAM_CHAT => {
                            if let Ok(message) = String::from_utf8(data) {
                                let _ = event_tx.send(Event::ChatReceived {
                                    peer_id: peer_id.to_string(),
                                    message,
                                });
                            }
                        }
                        STREAM_CONTROL => {
                            if let Ok(action) = serde_json::from_slice::<ControlAction>(&data) {
                                let _ = event_tx.send(Event::ControlReceived {
                                    peer_id: peer_id.to_string(),
                                    action,
                                });
                            }
                        }
                        _ => {
                            tracing::warn!("Unknown bi stream type: {stream_type}");
                        }
                    }
                }
                Ok(None) => break,
                Err(e) => {
                    tracing::warn!("Error reading bi stream from {peer_id}: {e}");
                    break;
                }
            }
        }
    }

    /// Send audio data to a specific peer.
    pub async fn send_audio(&self, peer_id: &str, data: &[u8]) -> Result<()> {
        let mut peers = self.peers.lock().await;
        if let Some(peer) = peers.get_mut(peer_id) {
            if let Some(ref mut send) = peer.audio_send {
                send.write_all(data).await?;
            }
        }
        Ok(())
    }

    /// Send video data to a specific peer.
    pub async fn send_video(&self, peer_id: &str, data: &[u8]) -> Result<()> {
        let mut peers = self.peers.lock().await;
        if let Some(peer) = peers.get_mut(peer_id) {
            if let Some(ref mut send) = peer.video_send {
                send.write_all(data).await?;
            }
        }
        Ok(())
    }

    /// Send a chat message to a specific peer (length-prefixed framing).
    pub async fn send_chat(&self, peer_id: &str, message: &str) -> Result<()> {
        let mut peers = self.peers.lock().await;
        if let Some(peer) = peers.get_mut(peer_id) {
            if let Some(ref mut send) = peer.chat_send {
                crate::messages::write_framed(send, message.as_bytes()).await?;
            }
        }
        Ok(())
    }

    /// Send a control action to a specific peer (length-prefixed framing).
    pub async fn send_control(&self, peer_id: &str, action: &ControlAction) -> Result<()> {
        let data = serde_json::to_vec(action)?;
        let mut peers = self.peers.lock().await;
        if let Some(peer) = peers.get_mut(peer_id) {
            if let Some(ref mut send) = peer.control_send {
                crate::messages::write_framed(send, &data).await?;
            }
        }
        Ok(())
    }

    /// Disconnect from a specific peer.
    pub async fn disconnect_peer(&self, peer_id: &str) -> Result<()> {
        let mut peers = self.peers.lock().await;
        if let Some(peer) = peers.remove(peer_id) {
            peer.connection.close(0u32.into(), b"call ended");
            let _ = self.event_tx.send(Event::PeerDisconnected {
                peer_id: peer_id.to_string(),
            });
        }
        Ok(())
    }

    /// Get list of connected peer IDs.
    pub async fn connected_peers(&self) -> Vec<String> {
        let peers = self.peers.lock().await;
        peers.keys().cloned().collect()
    }
}
```

- [ ] **Step 2: Verify it compiles**

Run: `cd sidecar && cargo check`
Expected: Compiles successfully

- [ ] **Step 3: Commit**

```bash
git add sidecar/src/connection.rs sidecar/src/protocol.rs
git commit -m "feat(sidecar): connection manager with QUIC stream multiplexing"
```

---

### Task 6: WebSocket IPC Server

**Files:**
- Modify: `sidecar/src/ipc.rs`

- [ ] **Step 1: Implement WebSocket server**

```rust
use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Result;
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpListener;
use tokio::sync::broadcast;
use tokio_tungstenite::accept_async;
use tokio_tungstenite::tungstenite::Message;
use tracing;

use crate::connection::ConnectionManager;
use crate::messages::{Command, Event, MediaFrame, STREAM_AUDIO, STREAM_VIDEO};
use crate::node;

/// Start the WebSocket IPC server.
pub async fn start_ws_server(
    port: u16,
    endpoint: iroh::Endpoint,
    conn_manager: Arc<ConnectionManager>,
    event_tx: broadcast::Sender<Event>,
    media_tx: broadcast::Sender<Vec<u8>>,
) -> Result<()> {
    let addr: SocketAddr = ([127, 0, 0, 1], port).into();
    let listener = TcpListener::bind(addr).await?;
    tracing::info!("WebSocket IPC server listening on ws://{addr}");

    while let Ok((stream, client_addr)) = listener.accept().await {
        tracing::info!("IPC client connected from {client_addr}");

        let endpoint = endpoint.clone();
        let conn_manager = conn_manager.clone();
        let mut event_rx = event_tx.subscribe();
        let mut media_rx = media_tx.subscribe();

        tokio::spawn(async move {
            let ws_stream = match accept_async(stream).await {
                Ok(ws) => ws,
                Err(e) => {
                    tracing::error!("WebSocket handshake failed: {e}");
                    return;
                }
            };

            let (mut ws_write, mut ws_read) = ws_stream.split();

            // Task 1: Forward events from sidecar to WebSocket client
            let mut write_clone = None; // We'll use channels instead
            let (ws_out_tx, mut ws_out_rx) = tokio::sync::mpsc::channel::<Message>(256);

            let ws_out_tx_events = ws_out_tx.clone();
            let ws_out_tx_media = ws_out_tx.clone();

            // Forward JSON events to WS
            tokio::spawn(async move {
                while let Ok(event) = event_rx.recv().await {
                    if let Ok(json) = serde_json::to_string(&event) {
                        let _ = ws_out_tx_events.send(Message::Text(json)).await;
                    }
                }
            });

            // Forward binary media frames to WS
            tokio::spawn(async move {
                while let Ok(data) = media_rx.recv().await {
                    let _ = ws_out_tx_media.send(Message::Binary(data)).await;
                }
            });

            // Write outgoing messages to WebSocket
            let write_task = tokio::spawn(async move {
                while let Some(msg) = ws_out_rx.recv().await {
                    if ws_write.send(msg).await.is_err() {
                        break;
                    }
                }
            });

            // Read incoming messages from WebSocket client
            let endpoint_ref = endpoint.clone();
            let conn_manager_ref = conn_manager.clone();
            let ws_out_tx_cmd = ws_out_tx.clone();

            while let Some(msg_result) = ws_read.next().await {
                let msg = match msg_result {
                    Ok(m) => m,
                    Err(e) => {
                        tracing::warn!("WebSocket read error: {e}");
                        break;
                    }
                };

                match msg {
                    Message::Text(text) => {
                        handle_text_command(
                            &text,
                            &endpoint_ref,
                            &conn_manager_ref,
                            &ws_out_tx_cmd,
                        )
                        .await;
                    }
                    Message::Binary(data) => {
                        handle_binary_frame(&data, &conn_manager_ref).await;
                    }
                    Message::Close(_) => {
                        tracing::info!("IPC client disconnected");
                        break;
                    }
                    _ => {}
                }
            }

            write_task.abort();
            tracing::info!("IPC client session ended");
        });
    }

    Ok(())
}

async fn handle_text_command(
    text: &str,
    endpoint: &iroh::Endpoint,
    conn_manager: &Arc<ConnectionManager>,
    ws_tx: &tokio::sync::mpsc::Sender<Message>,
) {
    let command: Command = match serde_json::from_str(text) {
        Ok(cmd) => cmd,
        Err(e) => {
            tracing::warn!("Invalid command JSON: {e}");
            let err_event = Event::Error {
                message: format!("Invalid command: {e}"),
            };
            let _ = ws_tx
                .send(Message::Text(serde_json::to_string(&err_event).unwrap()))
                .await;
            return;
        }
    };

    match command {
        Command::GetNodeInfo => {
            let ticket = node::generate_ticket(endpoint);
            let event = Event::NodeInfo {
                id: endpoint.id().to_string(),
                ticket,
            };
            let _ = ws_tx
                .send(Message::Text(serde_json::to_string(&event).unwrap()))
                .await;
        }
        Command::CreateCall => {
            let ticket = node::generate_ticket(endpoint);
            let event = Event::CallCreated { ticket };
            let _ = ws_tx
                .send(Message::Text(serde_json::to_string(&event).unwrap()))
                .await;
        }
        Command::JoinCall { ticket } => {
            match node::parse_ticket(&ticket) {
                Ok(endpoint_ticket) => {
                    let addr = endpoint_ticket.endpoint_addr().clone();
                    match conn_manager.connect_to_peer(endpoint, addr).await {
                        Ok(peer_id) => {
                            tracing::info!("Successfully connected to peer {peer_id}");
                        }
                        Err(e) => {
                            let _ = ws_tx
                                .send(Message::Text(
                                    serde_json::to_string(&Event::Error {
                                        message: format!("Failed to connect: {e}"),
                                    })
                                    .unwrap(),
                                ))
                                .await;
                        }
                    }
                }
                Err(e) => {
                    let _ = ws_tx
                        .send(Message::Text(
                            serde_json::to_string(&Event::Error {
                                message: format!("Invalid ticket: {e}"),
                            })
                            .unwrap(),
                        ))
                        .await;
                }
            }
        }
        Command::EndCall { peer_id } => {
            if let Err(e) = conn_manager.disconnect_peer(&peer_id).await {
                tracing::warn!("Error disconnecting peer {peer_id}: {e}");
            }
        }
        Command::SendChat { peer_id, message } => {
            if let Err(e) = conn_manager.send_chat(&peer_id, &message).await {
                tracing::warn!("Error sending chat to {peer_id}: {e}");
            }
        }
        Command::SendControl { peer_id, action } => {
            if let Err(e) = conn_manager.send_control(&peer_id, &action).await {
                tracing::warn!("Error sending control to {peer_id}: {e}");
            }
        }
    }
}

async fn handle_binary_frame(data: &[u8], conn_manager: &Arc<ConnectionManager>) {
    if let Some(frame) = MediaFrame::decode(data) {
        // Convert raw 32-byte public key to EndpointId string
        let peer_id = iroh::EndpointId::from_bytes(&frame.peer_id)
            .map(|id| id.to_string())
            .unwrap_or_default();

        let result = match frame.stream_type {
            STREAM_AUDIO => conn_manager.send_audio(&peer_id, &frame.payload).await,
            STREAM_VIDEO => conn_manager.send_video(&peer_id, &frame.payload).await,
            _ => {
                tracing::warn!("Unknown binary frame type: {}", frame.stream_type);
                Ok(())
            }
        };

        if let Err(e) = result {
            tracing::warn!("Error forwarding media to {peer_id}: {e}");
        }
    }
}
```

- [ ] **Step 2: Verify it compiles**

Run: `cd sidecar && cargo check`
Expected: Compiles successfully

- [ ] **Step 3: Commit**

```bash
git add sidecar/src/ipc.rs
git commit -m "feat(sidecar): WebSocket IPC server with command routing"
```

---

### Task 7: Main Entry Point

**Files:**
- Modify: `sidecar/src/main.rs`

- [ ] **Step 1: Wire everything together**

```rust
mod connection;
mod ipc;
mod messages;
mod node;
mod protocol;

use std::sync::Arc;

use clap::Parser;
use tokio::sync::broadcast;

use connection::ConnectionManager;
use iroh::protocol::Router;
use messages::Event;
use protocol::NafaqProtocol;

#[derive(Parser)]
#[command(name = "nafaq-sidecar")]
struct Cli {
    /// WebSocket port for IPC with Electrobun
    #[arg(short, long, default_value = "9320")]
    port: u16,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("nafaq_sidecar=info".parse()?),
        )
        .init();

    let cli = Cli::parse();
    tracing::info!("nafaq-sidecar v{}", env!("CARGO_PKG_VERSION"));

    // Create broadcast channels for events and media
    let (event_tx, _) = broadcast::channel::<Event>(256);
    let (media_tx, _) = broadcast::channel::<Vec<u8>>(1024);

    // Create the connection manager
    let conn_manager = Arc::new(ConnectionManager::new(event_tx.clone(), media_tx.clone()));

    // Create the Iroh endpoint
    let endpoint = node::create_endpoint().await?;
    let ticket = node::generate_ticket(&endpoint);
    tracing::info!("Node ID: {}", endpoint.id());
    tracing::info!("Ticket: {ticket}");

    // Set up the protocol router
    let router = Router::builder(endpoint.clone())
        .accept(node::NAFAQ_ALPN, NafaqProtocol::new(conn_manager.clone()))
        .spawn();

    // Start the WebSocket IPC server (blocks until shutdown)
    let ws_result = ipc::start_ws_server(
        cli.port,
        endpoint.clone(),
        conn_manager,
        event_tx,
        media_tx,
    )
    .await;

    // Cleanup
    tracing::info!("Shutting down...");
    if let Err(e) = router.shutdown().await {
        tracing::warn!("Router shutdown error: {e}");
    }
    endpoint.close().await;

    ws_result
}
```

- [ ] **Step 2: Build the binary**

Run: `cd sidecar && cargo build`
Expected: Compiles successfully, binary at `target/debug/nafaq-sidecar`

- [ ] **Step 3: Smoke test — run the binary**

Run: `cd sidecar && timeout 5 cargo run -- --port 9321 2>&1 || true`
Expected: See log output like "nafaq-sidecar v0.1.0", "Iroh endpoint started", "WebSocket IPC server listening on ws://127.0.0.1:9321"

- [ ] **Step 4: Commit**

```bash
git add sidecar/src/main.rs
git commit -m "feat(sidecar): wire up main entry point with endpoint, router, and WS server"
```

---

### Task 8: Integration Test — Two Nodes

**Files:**
- Create: `sidecar/tests/integration.rs`

- [ ] **Step 1: Write integration test for two nodes connecting**

```rust
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::broadcast;
use tokio::time::timeout;

// Import from the crate
use nafaq_sidecar::connection::ConnectionManager;
use nafaq_sidecar::messages::{Event, STREAM_CHAT};
use nafaq_sidecar::node::{self, NAFAQ_ALPN};
use nafaq_sidecar::protocol::NafaqProtocol;

use iroh::protocol::Router;

#[tokio::test]
async fn test_two_nodes_connect_and_chat() {
    tracing_subscriber::fmt()
        .with_env_filter("nafaq_sidecar=debug,iroh=warn")
        .try_init()
        .ok();

    // --- Node A (acceptor) ---
    let (event_tx_a, mut event_rx_a) = broadcast::channel::<Event>(64);
    let (media_tx_a, _) = broadcast::channel::<Vec<u8>>(64);
    let conn_mgr_a = Arc::new(ConnectionManager::new(event_tx_a.clone(), media_tx_a));

    let endpoint_a = node::create_endpoint().await.unwrap();
    let ticket_a = node::generate_ticket(&endpoint_a);

    let _router_a = Router::builder(endpoint_a.clone())
        .accept(NAFAQ_ALPN, NafaqProtocol::new(conn_mgr_a.clone()))
        .spawn();

    // --- Node B (connector) ---
    let (event_tx_b, mut event_rx_b) = broadcast::channel::<Event>(64);
    let (media_tx_b, _) = broadcast::channel::<Vec<u8>>(64);
    let conn_mgr_b = Arc::new(ConnectionManager::new(event_tx_b.clone(), media_tx_b));

    let endpoint_b = node::create_endpoint().await.unwrap();

    let _router_b = Router::builder(endpoint_b.clone())
        .accept(NAFAQ_ALPN, NafaqProtocol::new(conn_mgr_b.clone()))
        .spawn();

    // --- Connect B to A ---
    let ticket = node::parse_ticket(&ticket_a).unwrap();
    let addr = ticket.endpoint_addr().clone();
    let peer_id = conn_mgr_b
        .connect_to_peer(&endpoint_b, addr)
        .await
        .unwrap();

    // Wait for PeerConnected event on Node A
    let event = timeout(Duration::from_secs(10), event_rx_a.recv())
        .await
        .expect("Timed out waiting for connection event")
        .expect("Channel error");

    match event {
        Event::PeerConnected { peer_id } => {
            println!("Node A saw connection from: {peer_id}");
        }
        other => panic!("Expected PeerConnected, got: {other:?}"),
    }

    // --- Send chat from B to A ---
    conn_mgr_b
        .send_chat(&peer_id, "Hello from B!")
        .await
        .unwrap();

    // Wait for ChatReceived event on Node A
    let event = timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(evt) = event_rx_a.recv().await {
                if matches!(evt, Event::ChatReceived { .. }) {
                    return evt;
                }
            }
        }
    })
    .await
    .expect("Timed out waiting for chat message");

    match event {
        Event::ChatReceived { message, .. } => {
            assert_eq!(message, "Hello from B!");
        }
        other => panic!("Expected ChatReceived, got: {other:?}"),
    }

    // --- Cleanup ---
    conn_mgr_b.disconnect_peer(&peer_id).await.unwrap();
    _router_a.shutdown().await.ok();
    _router_b.shutdown().await.ok();
    endpoint_a.close().await;
    endpoint_b.close().await;
}
```

- [ ] **Step 2: Make modules public for integration tests**

Update `sidecar/src/main.rs` to add a `lib.rs` or make modules `pub`. Create `sidecar/src/lib.rs`:

```rust
pub mod connection;
pub mod ipc;
pub mod messages;
pub mod node;
pub mod protocol;
```

And keep `main.rs` importing from `nafaq_sidecar::*`:

```rust
use nafaq_sidecar::{connection, ipc, messages, node, protocol};
```

Update `main.rs` to remove `mod` declarations and import from the lib crate instead:

```rust
use std::sync::Arc;

use clap::Parser;
use tokio::sync::broadcast;

use nafaq_sidecar::connection::ConnectionManager;
use nafaq_sidecar::messages::Event;
use nafaq_sidecar::node;
use nafaq_sidecar::protocol::NafaqProtocol;
use iroh::protocol::Router;

#[derive(Parser)]
#[command(name = "nafaq-sidecar")]
struct Cli {
    #[arg(short, long, default_value = "9320")]
    port: u16,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("nafaq_sidecar=info".parse()?),
        )
        .init();

    let cli = Cli::parse();
    tracing::info!("nafaq-sidecar v{}", env!("CARGO_PKG_VERSION"));

    let (event_tx, _) = broadcast::channel::<Event>(256);
    let (media_tx, _) = broadcast::channel::<Vec<u8>>(1024);

    let conn_manager = Arc::new(ConnectionManager::new(event_tx.clone(), media_tx.clone()));

    let endpoint = node::create_endpoint().await?;
    tracing::info!("Node ID: {}", endpoint.id());
    tracing::info!("Ticket: {}", node::generate_ticket(&endpoint));

    let router = Router::builder(endpoint.clone())
        .accept(node::NAFAQ_ALPN, NafaqProtocol::new(conn_manager.clone()))
        .spawn();

    let ws_result = nafaq_sidecar::ipc::start_ws_server(
        cli.port,
        endpoint.clone(),
        conn_manager,
        event_tx,
        media_tx,
    )
    .await;

    tracing::info!("Shutting down...");
    if let Err(e) = router.shutdown().await {
        tracing::warn!("Router shutdown error: {e}");
    }
    endpoint.close().await;
    ws_result
}
```

- [ ] **Step 3: Run integration test**

Run: `cd sidecar && cargo test --test integration -- --nocapture`
Expected: Test passes — two nodes connect and exchange a chat message

- [ ] **Step 4: Commit**

```bash
git add sidecar/src/lib.rs sidecar/src/main.rs sidecar/tests/integration.rs
git commit -m "test(sidecar): integration test for two-node connection and chat"
```

---

## Verification Checklist

After completing all tasks:

- [ ] `cargo build --release` in `sidecar/` produces a binary
- [ ] `cargo test` passes all unit and integration tests
- [ ] Running `./target/release/nafaq-sidecar --port 9320` starts the sidecar, prints node ID and ticket, and listens for WebSocket connections
- [ ] A WebSocket client (e.g., `websocat ws://127.0.0.1:9320`) can send `{"type":"get_node_info"}` and receive a response with the node ID and ticket

## Next Plan

After this plan is complete, proceed to **Plan 2: Electrobun + Bridge** (`2026-03-25-nafaq-electrobun-bridge.md`) which sets up the Electrobun app, spawns this sidecar, and bridges IPC between the webview and sidecar.
