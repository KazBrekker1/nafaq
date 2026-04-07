use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;
use bytes::Bytes;
use iroh::{
    endpoint::{Connection, PathId, RecvStream, SendStream},
    Watcher,
};
use tokio::sync::{broadcast, Mutex, Notify};

#[derive(Clone)]
struct PendingVideoFrame {
    timestamp_ms: u64,
    payload: Vec<u8>,
    is_keyframe: bool,
}

#[derive(Clone)]
struct PeerVideoWriter {
    pending: Arc<Mutex<Option<PendingVideoFrame>>>,
    notify: Arc<Notify>,
}

impl PeerVideoWriter {
    fn new() -> Self {
        Self {
            pending: Arc::new(Mutex::new(None)),
            notify: Arc::new(Notify::new()),
        }
    }

    async fn enqueue_latest(&self, frame: PendingVideoFrame) {
        let mut pending = self.pending.lock().await;
        let should_replace = match pending.as_ref() {
            Some(existing) if existing.is_keyframe && !frame.is_keyframe => false,
            _ => true,
        };
        if should_replace {
            *pending = Some(frame);
        }
        self.notify.notify_one();
    }
}

use crate::codec::is_keyframe;
use crate::messages::{
    AudioDatagram, AudioPacket, ControlAction, DmMessage, Event, VideoLayerRequest, VideoPacket,
    STREAM_AUDIO, STREAM_CHAT, STREAM_CONTROL, STREAM_DM, STREAM_VIDEO,
};

struct ActiveFileReceive {
    file: tokio::fs::File,
    temp_path: std::path::PathBuf,
    final_name: String,
    #[allow(dead_code)]
    expected_size: u64,
    received_bytes: u64,
}

/// Resolve a unique file path in the target directory, appending `(N)` if needed.
fn unique_file_path(dir: &std::path::Path, name: &str) -> std::path::PathBuf {
    let candidate = dir.join(name);
    if !candidate.exists() {
        return candidate;
    }
    let stem = std::path::Path::new(name)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(name);
    let ext = std::path::Path::new(name)
        .extension()
        .and_then(|s| s.to_str());
    for i in 1u32.. {
        let new_name = match ext {
            Some(e) => format!("{stem} ({i}).{e}"),
            None => format!("{stem} ({i})"),
        };
        let p = dir.join(&new_name);
        if !p.exists() {
            return p;
        }
    }
    candidate // unreachable in practice
}

/// Process a single DM message, handling file reconstruction when appropriate.
/// Returns `true` if the caller should `continue` (i.e. skip emitting DmReceived).
async fn handle_dm_file_message(
    dm_msg: &DmMessage,
    peer_id: &str,
    active_files: &mut HashMap<String, ActiveFileReceive>,
    event_tx: &broadcast::Sender<Event>,
) -> bool {
    use tokio::io::{AsyncSeekExt, AsyncWriteExt};

    match dm_msg {
        DmMessage::FileStart { name, size, id } => {
            let temp_dir = std::env::temp_dir();
            let temp_path = temp_dir.join(format!("nafaq_recv_{id}"));
            match tokio::fs::File::create(&temp_path).await {
                Ok(file) => {
                    active_files.insert(
                        id.clone(),
                        ActiveFileReceive {
                            file,
                            temp_path,
                            final_name: name.clone(),
                            expected_size: *size,
                            received_bytes: 0,
                        },
                    );
                }
                Err(e) => {
                    tracing::warn!("Failed to create temp file for transfer {id}: {e}");
                }
            }
            false // still emit DmReceived so frontend shows the file
        }
        DmMessage::FileChunk { id, offset, data } => {
            if let Some(recv) = active_files.get_mut(id) {
                // Seek to the correct offset and write
                if recv
                    .file
                    .seek(std::io::SeekFrom::Start(*offset))
                    .await
                    .is_ok()
                {
                    if let Err(e) = recv.file.write_all(data).await {
                        tracing::warn!("Failed to write chunk for transfer {id}: {e}");
                    } else {
                        recv.received_bytes =
                            (*offset + data.len() as u64).max(recv.received_bytes);
                    }
                }
            } else {
                tracing::debug!("FileChunk for unknown transfer {id}, ignoring");
            }
            false
        }
        DmMessage::FileEnd { id } => {
            if let Some(mut recv) = active_files.remove(id) {
                // Flush and close the temp file
                let _ = recv.file.flush().await;
                drop(recv.file);

                // Determine downloads directory
                let downloads_dir = std::env::var("HOME")
                    .or_else(|_| std::env::var("USERPROFILE"))
                    .map(|h| std::path::PathBuf::from(h).join("Downloads"))
                    .unwrap_or_else(|_| std::env::temp_dir());

                // Ensure the directory exists
                let _ = tokio::fs::create_dir_all(&downloads_dir).await;

                let final_path = unique_file_path(&downloads_dir, &recv.final_name);

                match tokio::fs::rename(&recv.temp_path, &final_path).await {
                    Ok(()) => {
                        tracing::info!(
                            "File transfer {id} complete: {} ({} bytes) -> {}",
                            recv.final_name,
                            recv.received_bytes,
                            final_path.display()
                        );
                        let _ = event_tx.send(Event::DmFileSaved {
                            peer_id: peer_id.to_string(),
                            file_id: id.clone(),
                            local_path: final_path.to_string_lossy().to_string(),
                        });
                    }
                    Err(e) => {
                        // rename can fail across filesystems; fall back to copy + remove
                        tracing::debug!("rename failed ({e}), trying copy fallback");
                        match tokio::fs::copy(&recv.temp_path, &final_path).await {
                            Ok(_) => {
                                let _ = tokio::fs::remove_file(&recv.temp_path).await;
                                tracing::info!(
                                    "File transfer {id} complete (copy): {} -> {}",
                                    recv.final_name,
                                    final_path.display()
                                );
                                let _ = event_tx.send(Event::DmFileSaved {
                                    peer_id: peer_id.to_string(),
                                    file_id: id.clone(),
                                    local_path: final_path.to_string_lossy().to_string(),
                                });
                            }
                            Err(e2) => {
                                tracing::warn!(
                                    "Failed to save file for transfer {id}: rename={e}, copy={e2}"
                                );
                            }
                        }
                    }
                }
            } else {
                tracing::debug!("FileEnd for unknown transfer {id}, ignoring");
            }
            false
        }
        _ => false,
    }
}

/// Shared DM stream reader loop — reads framed messages, handles files,
/// emits events. Used by both connect_dm and setup_dm_connection.
async fn run_dm_reader(
    recv: &mut iroh::endpoint::RecvStream,
    peer_id: &str,
    event_tx: &broadcast::Sender<Event>,
) {
    let mut active_files: HashMap<String, ActiveFileReceive> = HashMap::new();
    loop {
        match crate::messages::read_framed(recv).await {
            Ok(Some(data)) => {
                if let Ok(dm_msg) = serde_json::from_slice::<DmMessage>(&data) {
                    if matches!(dm_msg, DmMessage::Heartbeat) {
                        continue;
                    }
                    if let DmMessage::CallInvite { ref ticket } = dm_msg {
                        let _ = event_tx.send(Event::CallInviteReceived {
                            peer_id: peer_id.to_string(),
                            ticket: ticket.clone(),
                        });
                    }
                    handle_dm_file_message(&dm_msg, peer_id, &mut active_files, event_tx).await;
                    let _ = event_tx.send(Event::DmReceived {
                        peer_id: peer_id.to_string(),
                        message: dm_msg,
                    });
                }
            }
            Ok(None) => break,
            Err(_) => break,
        }
    }
    for (id, recv) in active_files {
        tracing::debug!("Cleaning up incomplete file transfer {id}");
        drop(recv.file);
        let _ = tokio::fs::remove_file(&recv.temp_path).await;
    }
}

struct PeerConnection {
    connection: Connection,
    chat_send: Arc<Mutex<Option<SendStream>>>,
    control_send: Arc<Mutex<Option<SendStream>>>,
    video_writer: PeerVideoWriter,
    /// Requested video layer: 0=High, 1=Low, 2=None
    requested_video_layer: Arc<AtomicU8>,
    pending_keyframe: Arc<AtomicBool>,
    last_activity_ms: Arc<AtomicU64>,
    /// Per-peer outbound bitrate override (0 = use global profile)
    outbound_bitrate_bps: Arc<AtomicU32>,
}

struct DmPeerConnection {
    connection: Connection,
    dm_send: Arc<Mutex<Option<SendStream>>>,
}

#[derive(Clone)]
struct VideoReceiveState {
    last_received: std::time::Instant,
}

impl Default for VideoReceiveState {
    fn default() -> Self {
        Self {
            last_received: std::time::Instant::now(),
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct NetworkPeerStats {
    pub peer_id: String,
    pub rtt_ms: u64,
    pub lost_packets: u64,
    pub lost_bytes: u64,
    pub datagram_send_buffer_space: usize,
    pub latest_video_age_ms: u64,
}

pub struct ConnectionManager {
    peers: Arc<Mutex<HashMap<String, PeerConnection>>>,
    dm_peers: Arc<Mutex<HashMap<String, DmPeerConnection>>>,
    endpoint: Arc<Mutex<Option<iroh::Endpoint>>>,
    peer_tickets: Arc<Mutex<HashMap<String, String>>>,
    audio_sequences: Arc<Mutex<HashMap<String, u16>>>,
    video_receive_state: Arc<Mutex<HashMap<String, VideoReceiveState>>>,
    event_tx: broadcast::Sender<Event>,
    audio_media_tx: broadcast::Sender<AudioPacket>,
    video_media_tx: broadcast::Sender<VideoPacket>,
}

impl std::fmt::Debug for ConnectionManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConnectionManager").finish()
    }
}

impl ConnectionManager {
    fn current_timestamp_ms() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }

    async fn mark_peer_active_internal(
        peers: &Arc<Mutex<HashMap<String, PeerConnection>>>,
        peer_id: &str,
    ) {
        let last_activity = {
            let peers = peers.lock().await;
            peers.get(peer_id).map(|p| p.last_activity_ms.clone())
        };
        if let Some(last_activity) = last_activity {
            last_activity.store(Self::current_timestamp_ms(), Ordering::Relaxed);
        }
    }

    pub fn new(
        event_tx: broadcast::Sender<Event>,
        audio_media_tx: broadcast::Sender<AudioPacket>,
        video_media_tx: broadcast::Sender<VideoPacket>,
    ) -> Self {
        Self {
            peers: Arc::new(Mutex::new(HashMap::new())),
            dm_peers: Arc::new(Mutex::new(HashMap::new())),
            endpoint: Arc::new(Mutex::new(None)),
            peer_tickets: Arc::new(Mutex::new(HashMap::new())),
            audio_sequences: Arc::new(Mutex::new(HashMap::new())),
            video_receive_state: Arc::new(Mutex::new(HashMap::new())),
            event_tx,
            audio_media_tx,
            video_media_tx,
        }
    }

    pub async fn set_endpoint(&self, endpoint: iroh::Endpoint) {
        *self.endpoint.lock().await = Some(endpoint);
    }

    #[allow(dead_code)]
    pub async fn peer_count(&self) -> usize {
        self.peers.lock().await.len()
    }

    pub fn quality_profile_for_peers(count: usize) -> (u32, u32, u32, u32) {
        // Returns (bitrate_bps, fps, max_width, max_height)
        match count {
            0..=2 => (400_000, 12, 640, 360),
            3 => (250_000, 10, 480, 270),
            _ => (150_000, 8, 320, 180),
        }
    }

    fn emit_quality_profile_if_changed(
        old_count: usize,
        new_count: usize,
        event_tx: &broadcast::Sender<Event>,
    ) {
        let old_profile = Self::quality_profile_for_peers(old_count);
        let new_profile = Self::quality_profile_for_peers(new_count);
        if old_profile == new_profile {
            return;
        }
        let (bitrate, fps, w, h) = new_profile;
        let _ = event_tx.send(Event::QualityProfileChanged {
            peer_count: new_count,
            bitrate_bps: bitrate,
            fps,
            max_width: w,
            max_height: h,
        });
    }

    pub async fn handle_incoming(&self, connection: Connection) -> Result<()> {
        let peer_id = connection.remote_id().to_string();
        tracing::info!("Setting up incoming connection from {peer_id}");
        self.setup_connection(peer_id, connection).await
    }

    pub async fn handle_incoming_dm(&self, connection: Connection) -> Result<()> {
        let peer_id = connection.remote_id().to_string();
        tracing::info!("Setting up incoming DM connection from {peer_id}");
        self.setup_dm_connection(peer_id, connection).await
    }

    async fn setup_dm_connection(&self, peer_id: String, connection: Connection) -> Result<()> {
        let dm_peers_ref = self.dm_peers.clone();
        let event_tx = self.event_tx.clone();
        let peer_id_reader = peer_id.clone();
        let connection_reader = connection.clone();

        // Spawn bi-stream reader for incoming DM streams
        tokio::spawn(async move {
            loop {
                match connection_reader.accept_bi().await {
                    Ok((send, mut recv)) => {
                        let mut type_buf = [0u8; 1];
                        if recv.read_exact(&mut type_buf).await.is_err() {
                            continue;
                        }
                        if type_buf[0] == STREAM_DM {
                            let mut dm_peers = dm_peers_ref.lock().await;
                            if !dm_peers.contains_key(&peer_id_reader) {
                                dm_peers.insert(
                                    peer_id_reader.clone(),
                                    DmPeerConnection {
                                        connection: connection_reader.clone(),
                                        dm_send: Arc::new(Mutex::new(Some(send))),
                                    },
                                );
                                let _ = event_tx.send(Event::DmConnected {
                                    peer_id: peer_id_reader.clone(),
                                });
                            }
                            drop(dm_peers);

                            // Spawn reader for this DM stream
                            let event_tx = event_tx.clone();
                            let peer_id = peer_id_reader.clone();
                            tokio::spawn(async move {
                                run_dm_reader(&mut recv, &peer_id, &event_tx).await;
                            });
                        }
                        // Ignore non-DM streams on a DM connection
                    }
                    Err(_) => break,
                }
            }

            // Connection closed — clean up
            let removed = dm_peers_ref.lock().await.remove(&peer_id_reader);
            if removed.is_some() {
                let _ = event_tx.send(Event::DmDisconnected {
                    peer_id: peer_id_reader,
                });
            }
        });

        // Spawn a task to detect connection closure as a backstop
        let event_tx_closed = self.event_tx.clone();
        let dm_peers_closed = self.dm_peers.clone();
        let peer_id_closed = peer_id;
        tokio::spawn(async move {
            connection.closed().await;
            if dm_peers_closed
                .lock()
                .await
                .remove(&peer_id_closed)
                .is_some()
            {
                let _ = event_tx_closed.send(Event::DmDisconnected {
                    peer_id: peer_id_closed,
                });
            }
        });

        Ok(())
    }

    pub async fn connect_to_peer(
        &self,
        endpoint: &iroh::Endpoint,
        addr: iroh::EndpointAddr,
    ) -> Result<String> {
        let connection = endpoint.connect(addr, crate::node::NAFAQ_ALPN).await?;
        let peer_id = connection.remote_id().to_string();
        tracing::info!("Connected to peer {peer_id}");
        self.setup_connection(peer_id.clone(), connection).await?;
        Ok(peer_id)
    }

    async fn setup_connection(&self, peer_id: String, connection: Connection) -> Result<()> {
        connection.set_max_concurrent_uni_streams(2048_u32.into());

        let (mut chat_send, _) = connection.open_bi().await?;
        chat_send.write_all(&[STREAM_CHAT]).await?;
        chat_send.set_priority(10)?;

        let (mut control_send, _) = connection.open_bi().await?;
        control_send.write_all(&[STREAM_CONTROL]).await?;
        control_send.set_priority(100)?;

        let peer_conn = PeerConnection {
            connection: connection.clone(),
            chat_send: Arc::new(Mutex::new(Some(chat_send))),
            control_send: Arc::new(Mutex::new(Some(control_send))),
            video_writer: PeerVideoWriter::new(),
            requested_video_layer: Arc::new(AtomicU8::new(0)),
            pending_keyframe: Arc::new(AtomicBool::new(false)),
            last_activity_ms: Arc::new(AtomicU64::new(Self::current_timestamp_ms())),
            outbound_bitrate_bps: Arc::new(AtomicU32::new(0)),
        };

        let video_writer = peer_conn.video_writer.clone();

        let (old_count, new_count) = {
            let mut peers = self.peers.lock().await;
            let old = peers.len();
            peers.insert(peer_id.clone(), peer_conn);
            (old, peers.len())
        };

        Self::spawn_video_writer(peer_id.clone(), connection.clone(), video_writer);

        let _ = self.event_tx.send(Event::PeerConnected {
            peer_id: peer_id.clone(),
        });

        Self::emit_quality_profile_if_changed(old_count, new_count, &self.event_tx);

        self.spawn_stream_receivers(peer_id.clone(), connection);

        let endpoint_guard = self.endpoint.lock().await;
        if let Some(endpoint) = endpoint_guard.as_ref() {
            let own_ticket = crate::node::generate_ticket(endpoint);
            let own_id = endpoint.id().to_string();
            drop(endpoint_guard);

            let announce_self = ControlAction::PeerAnnounce {
                peer_id: own_id,
                ticket: own_ticket,
            };
            let _ = self.send_control(&peer_id, &announce_self).await;

            let stored_tickets: Vec<(String, String)> = {
                let tickets = self.peer_tickets.lock().await;
                tickets
                    .iter()
                    .filter(|(id, _)| **id != peer_id)
                    .map(|(id, t)| (id.clone(), t.clone()))
                    .collect()
            };
            for (stored_id, stored_ticket) in stored_tickets {
                let announce = ControlAction::PeerAnnounce {
                    peer_id: stored_id,
                    ticket: stored_ticket,
                };
                let _ = self.send_control(&peer_id, &announce).await;
            }
        } else {
            drop(endpoint_guard);
        }

        Ok(())
    }

    async fn cleanup_peer_internal(
        peer_id: &str,
        peers: &Arc<Mutex<HashMap<String, PeerConnection>>>,
        peer_tickets: &Arc<Mutex<HashMap<String, String>>>,
        audio_sequences: &Arc<Mutex<HashMap<String, u16>>>,
        video_receive_state: &Arc<Mutex<HashMap<String, VideoReceiveState>>>,
        event_tx: &broadcast::Sender<Event>,
        close_reason: Option<&'static [u8]>,
    ) -> bool {
        let (removed, old_count, new_count) = {
            let mut peers = peers.lock().await;
            let old = peers.len();
            let removed = peers.remove(peer_id);
            (removed, old, peers.len())
        };

        let Some(peer) = removed else {
            return false;
        };

        if let Some(reason) = close_reason {
            peer.connection.close(0u32.into(), reason);
        }

        peer_tickets.lock().await.remove(peer_id);
        audio_sequences.lock().await.remove(peer_id);
        video_receive_state.lock().await.remove(peer_id);
        let _ = event_tx.send(Event::PeerDisconnected {
            peer_id: peer_id.to_string(),
        });

        ConnectionManager::emit_quality_profile_if_changed(old_count, new_count, event_tx);

        true
    }

    pub async fn handle_peer_announce(
        &self,
        sender_id: &str,
        announced_peer_id: String,
        ticket: String,
    ) {
        let is_self = {
            let guard = self.endpoint.lock().await;
            guard
                .as_ref()
                .is_some_and(|ep| announced_peer_id == ep.id().to_string())
        };
        if is_self {
            return;
        }

        let is_new = {
            let mut tickets = self.peer_tickets.lock().await;
            if tickets.contains_key(&announced_peer_id) {
                false
            } else {
                tickets.insert(announced_peer_id.clone(), ticket.clone());
                true
            }
        };
        if !is_new {
            return;
        }

        let already_connected = self.peers.lock().await.contains_key(&announced_peer_id);
        if !already_connected {
            let endpoint = self.endpoint.lock().await.clone();
            if let Some(ep) = endpoint {
                match crate::node::parse_ticket(&ticket) {
                    Ok(endpoint_ticket) => {
                        let addr = endpoint_ticket.endpoint_addr().clone();
                        match self.connect_to_peer(&ep, addr).await {
                            Ok(_) => {
                                tracing::info!(
                                    "Mesh: auto-connected to announced peer {announced_peer_id}"
                                )
                            }
                            Err(e) => tracing::warn!(
                                "Mesh: failed to auto-connect to {announced_peer_id}: {e}"
                            ),
                        }
                    }
                    Err(e) => tracing::warn!("Mesh: invalid ticket for {announced_peer_id}: {e}"),
                }
            }
        }

        let relay_targets: Vec<String> = {
            let peers = self.peers.lock().await;
            peers
                .keys()
                .filter(|id| *id != sender_id && *id != &announced_peer_id)
                .cloned()
                .collect()
        };
        for target_id in relay_targets {
            let announce = ControlAction::PeerAnnounce {
                peer_id: announced_peer_id.clone(),
                ticket: ticket.clone(),
            };
            let _ = self.send_control(&target_id, &announce).await;
        }
    }

    fn spawn_stream_receivers(&self, peer_id: String, connection: Connection) {
        let audio_media_tx = self.audio_media_tx.clone();
        let video_media_tx = self.video_media_tx.clone();
        let peers_ref = self.peers.clone();
        let peer_tickets_ref = self.peer_tickets.clone();
        let sequences_ref = self.audio_sequences.clone();
        let video_state_ref = self.video_receive_state.clone();
        let video_state_ref_uni = video_state_ref.clone();
        let event_tx_cleanup = self.event_tx.clone();
        let peer_id_uni = peer_id.clone();
        let connection_uni = connection.clone();
        let peers_ref_uni = peers_ref.clone();

        tokio::spawn(async move {
            loop {
                match connection_uni.accept_uni().await {
                    Ok(mut recv) => {
                        let peer_id = peer_id_uni.clone();
                        let audio_tx = audio_media_tx.clone();
                        let video_tx = video_media_tx.clone();
                        let video_state_ref = video_state_ref_uni.clone();
                        let peers_ref = peers_ref_uni.clone();
                        tokio::spawn(async move {
                            let mut type_buf = [0u8; 1];
                            if recv.read_exact(&mut type_buf).await.is_err() {
                                return;
                            }
                            match type_buf[0] {
                                STREAM_AUDIO => loop {
                                    match crate::messages::read_framed(&mut recv).await {
                                        Ok(Some(data)) => {
                                            if let Some(packet) = AudioDatagram::decode(&data) {
                                                Self::mark_peer_active_internal(
                                                    &peers_ref, &peer_id,
                                                )
                                                .await;
                                                let _ = audio_tx.send(AudioPacket {
                                                    peer_id: peer_id.clone(),
                                                    timestamp_ms: packet.timestamp_ms,
                                                    sequence: packet.sequence,
                                                    payload: packet.payload,
                                                });
                                            }
                                        }
                                        _ => break,
                                    }
                                },
                                STREAM_VIDEO => loop {
                                    match crate::messages::read_framed(&mut recv).await {
                                        Ok(Some(data)) => {
                                            if data.len() < 8 {
                                                continue;
                                            }
                                            Self::mark_peer_active_internal(&peers_ref, &peer_id)
                                                .await;
                                            let timestamp_ms =
                                                u64::from_be_bytes(data[..8].try_into().unwrap());
                                            let payload = data[8..].to_vec();
                                            video_state_ref.lock().await.insert(
                                                peer_id.clone(),
                                                VideoReceiveState {
                                                    last_received: std::time::Instant::now(),
                                                },
                                            );
                                            let _ = video_tx.send(VideoPacket {
                                                peer_id: peer_id.clone(),
                                                timestamp_ms,
                                                payload,
                                            });
                                        }
                                        _ => break,
                                    }
                                },
                                _ => {}
                            }
                        });
                    }
                    Err(_) => {
                        tracing::info!("Uni stream accept ended for peer {peer_id_uni}");
                        break;
                    }
                }
            }
        });

        let audio_media_tx = self.audio_media_tx.clone();
        let peer_id_datagram = peer_id.clone();
        let connection_datagram = connection.clone();
        let peers_ref_datagram = peers_ref.clone();
        tokio::spawn(async move {
            loop {
                match connection_datagram.read_datagram().await {
                    Ok(data) => {
                        if let Some(packet) = AudioDatagram::decode(&data) {
                            Self::mark_peer_active_internal(&peers_ref_datagram, &peer_id_datagram)
                                .await;
                            let _ = audio_media_tx.send(AudioPacket {
                                peer_id: peer_id_datagram.clone(),
                                timestamp_ms: packet.timestamp_ms,
                                sequence: packet.sequence,
                                payload: packet.payload,
                            });
                        }
                    }
                    Err(e) => {
                        tracing::debug!("Datagram receive ended for peer {peer_id_datagram}: {e}");
                        break;
                    }
                }
            }
        });

        let peers_ref_closed = peers_ref.clone();
        let peer_tickets_ref = peer_tickets_ref.clone();
        let sequences_ref = sequences_ref.clone();
        let video_state_ref = video_state_ref.clone();
        let event_tx_cleanup_closed = event_tx_cleanup.clone();
        let peer_id_closed = peer_id.clone();
        let connection_closed = connection.clone();
        tokio::spawn(async move {
            let close_reason = connection_closed.closed().await;
            tracing::info!("Connection closed for peer {peer_id_closed}: {close_reason}");
            Self::cleanup_peer_internal(
                &peer_id_closed,
                &peers_ref_closed,
                &peer_tickets_ref,
                &sequences_ref,
                &video_state_ref,
                &event_tx_cleanup_closed,
                None,
            )
            .await;
        });

        let event_tx = self.event_tx.clone();
        let peer_id_bi = peer_id.clone();
        let peers_ref_bi = peers_ref.clone();
        let dm_peers_ref_bi = self.dm_peers.clone();
        let connection_bi = connection.clone();
        tokio::spawn(async move {
            loop {
                match connection.accept_bi().await {
                    Ok((send, mut recv)) => {
                        let peer_id = peer_id_bi.clone();
                        let event_tx = event_tx.clone();
                        let peers_ref = peers_ref_bi.clone();
                        let mut type_buf = [0u8; 1];
                        if recv.read_exact(&mut type_buf).await.is_err() {
                            continue;
                        }
                        // For incoming DM streams, store the send side so we
                        // can reply, and emit DmConnected if this is a new
                        // DM peer.
                        if type_buf[0] == STREAM_DM {
                            let dm_peers_ref = dm_peers_ref_bi.clone();
                            let mut dm_peers = dm_peers_ref.lock().await;
                            if !dm_peers.contains_key(&peer_id) {
                                dm_peers.insert(
                                    peer_id.clone(),
                                    DmPeerConnection {
                                        connection: connection_bi.clone(),
                                        dm_send: Arc::new(Mutex::new(Some(send))),
                                    },
                                );
                                let _ = event_tx.send(Event::DmConnected {
                                    peer_id: peer_id.clone(),
                                });
                            }
                            drop(dm_peers);
                        }
                        tokio::spawn(async move {
                            Self::handle_bi_stream(
                                type_buf[0],
                                &peer_id,
                                recv,
                                event_tx,
                                peers_ref,
                            )
                            .await;
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

    async fn handle_bi_stream(
        stream_type: u8,
        peer_id: &str,
        mut recv: RecvStream,
        event_tx: broadcast::Sender<Event>,
        peers: Arc<Mutex<HashMap<String, PeerConnection>>>,
    ) {
        let mut active_files: HashMap<String, ActiveFileReceive> = HashMap::new();

        loop {
            match crate::messages::read_framed(&mut recv).await {
                Ok(Some(data)) => match stream_type {
                    STREAM_CHAT => {
                        Self::mark_peer_active_internal(&peers, peer_id).await;
                        if let Ok(message) = String::from_utf8(data) {
                            let _ = event_tx.send(Event::ChatReceived {
                                peer_id: peer_id.to_string(),
                                message,
                            });
                        }
                    }
                    STREAM_CONTROL => {
                        Self::mark_peer_active_internal(&peers, peer_id).await;
                        if let Ok(action) = serde_json::from_slice::<ControlAction>(&data) {
                            if matches!(action, ControlAction::Heartbeat) {
                                continue;
                            }
                            let _ = event_tx.send(Event::ControlReceived {
                                peer_id: peer_id.to_string(),
                                action,
                            });
                        }
                    }
                    STREAM_DM => {
                        if let Ok(dm_msg) = serde_json::from_slice::<DmMessage>(&data) {
                            if matches!(dm_msg, DmMessage::Heartbeat) {
                                continue;
                            }
                            if let DmMessage::CallInvite { ref ticket } = dm_msg {
                                let _ = event_tx.send(Event::CallInviteReceived {
                                    peer_id: peer_id.to_string(),
                                    ticket: ticket.clone(),
                                });
                            }
                            handle_dm_file_message(&dm_msg, peer_id, &mut active_files, &event_tx)
                                .await;
                            let _ = event_tx.send(Event::DmReceived {
                                peer_id: peer_id.to_string(),
                                message: dm_msg,
                            });
                        }
                    }
                    _ => tracing::warn!("Unknown bi stream type: {stream_type}"),
                },
                Ok(None) => break,
                Err(e) => {
                    tracing::warn!("Error reading bi stream from {peer_id}: {e}");
                    break;
                }
            }
        }

        // Clean up any incomplete temp files on stream close
        for (id, recv) in active_files {
            tracing::debug!("Cleaning up incomplete file transfer {id}");
            drop(recv.file);
            let _ = tokio::fs::remove_file(&recv.temp_path).await;
        }
    }

    fn timestamped_payload(data: &[u8], timestamp: u64) -> Vec<u8> {
        let mut payload = Vec::with_capacity(8 + data.len());
        payload.extend_from_slice(&timestamp.to_be_bytes());
        payload.extend_from_slice(data);
        payload
    }

    fn spawn_video_writer(peer_id: String, connection: Connection, writer: PeerVideoWriter) {
        tokio::spawn(async move {
            let mut send: Option<SendStream> = None;
            loop {
                tokio::select! {
                    _ = writer.notify.notified() => {}
                    _ = connection.closed() => {
                        tracing::info!("Video writer closed for peer {peer_id}");
                        break;
                    }
                }

                while let Some(frame) = writer.pending.lock().await.take() {
                    let payload = Self::timestamped_payload(&frame.payload, frame.timestamp_ms);
                    let mut attempt = 0usize;

                    loop {
                        if send.is_none() {
                            match connection.open_uni().await {
                                Ok(mut stream) => {
                                    if stream.write_all(&[STREAM_VIDEO]).await.is_ok() {
                                        send = Some(stream);
                                    } else {
                                        tracing::warn!(
                                            "Failed to prime video stream for peer {peer_id}"
                                        );
                                        break;
                                    }
                                }
                                Err(e) => {
                                    tracing::warn!(
                                        "Failed to open video stream for peer {peer_id}: {e}"
                                    );
                                    break;
                                }
                            }
                        }

                        let result = if let Some(stream) = send.as_mut() {
                            let _ = stream.set_priority(if is_keyframe(&frame.payload) {
                                50
                            } else {
                                30
                            });
                            crate::messages::write_framed(stream, &payload).await
                        } else {
                            break;
                        };

                        match result {
                            Ok(()) => break,
                            Err(e) => {
                                tracing::warn!(
                                    "Video stream write failed for peer {peer_id}, attempt {}: {e}",
                                    attempt + 1
                                );
                                send = None;
                                attempt += 1;
                                if attempt >= 2 {
                                    break;
                                }
                            }
                        }
                    }
                }
            }
        });
    }

    fn send_audio_datagram(
        conn: &Connection,
        sequence: u16,
        data: &[u8],
        timestamp: u64,
    ) -> Result<()> {
        let payload = AudioDatagram::encode(sequence, timestamp, data);
        if let Some(max_size) = conn.max_datagram_size() {
            if payload.len() > max_size {
                tracing::warn!(
                    "Audio datagram too large for peer {} > {}",
                    payload.len(),
                    max_size
                );
                return Ok(());
            }
        }
        conn.send_datagram(Bytes::from(payload))?;
        Ok(())
    }

    pub async fn send_audio_to_all(&self, data: &[u8], timestamp: u64) -> Result<()> {
        let peers: Vec<(String, Connection)> = {
            let peers = self.peers.lock().await;
            peers
                .iter()
                .map(|(peer_id, peer)| (peer_id.clone(), peer.connection.clone()))
                .collect()
        };
        let mut sequences = self.audio_sequences.lock().await;
        for (peer_id, conn) in peers {
            let entry = sequences.entry(peer_id.clone()).or_insert(0);
            let sequence = *entry;
            *entry = entry.wrapping_add(1);
            if let Err(e) = Self::send_audio_datagram(&conn, sequence, data, timestamp) {
                tracing::warn!("Audio datagram send failed for {peer_id}: {e}");
            }
        }
        Ok(())
    }

    pub async fn send_video_frame_all(&self, data: &[u8], timestamp: u64) -> Result<()> {
        let peers: Vec<PeerVideoWriter> = {
            let peers_guard = self.peers.lock().await;
            peers_guard
                .values()
                .map(|p| p.video_writer.clone())
                .collect()
        };

        for writer in peers {
            writer
                .enqueue_latest(PendingVideoFrame {
                    timestamp_ms: timestamp,
                    payload: data.to_vec(),
                    is_keyframe: is_keyframe(data),
                })
                .await;
        }
        Ok(())
    }

    pub async fn has_peers(&self) -> bool {
        !self.peers.lock().await.is_empty()
    }

    pub async fn consume_pending_keyframe_requests(&self) -> bool {
        let peers = self.peers.lock().await;
        let mut force_keyframe = false;
        for peer in peers.values() {
            if peer.pending_keyframe.swap(false, Ordering::Relaxed) {
                force_keyframe = true;
            }
        }
        force_keyframe
    }

    pub async fn set_peer_video_layer(&self, peer_id: &str, layer: VideoLayerRequest) {
        let peers = self.peers.lock().await;
        if let Some(peer) = peers.get(peer_id) {
            let new_val = layer.to_u8();
            let current = peer.requested_video_layer.load(Ordering::Relaxed);
            if current != new_val {
                peer.requested_video_layer.store(new_val, Ordering::Relaxed);
                if new_val < 2 {
                    peer.pending_keyframe.store(true, Ordering::Relaxed);
                }
            }
        }
    }

    pub async fn request_peer_keyframe(&self, peer_id: &str, layer: VideoLayerRequest) {
        let peers = self.peers.lock().await;
        if let Some(peer) = peers.get(peer_id) {
            let requested = layer.to_u8();
            if requested < 2 {
                peer.pending_keyframe.store(true, Ordering::Relaxed);
            }
        }
    }

    pub async fn send_chat(&self, peer_id: &str, message: &str) -> Result<()> {
        let stream = {
            let peers = self.peers.lock().await;
            peers.get(peer_id).map(|p| p.chat_send.clone())
        };
        let Some(s) = stream else {
            anyhow::bail!("Peer {peer_id} is not connected");
        };

        let mut guard = s.lock().await;
        if let Some(ref mut send) = *guard {
            crate::messages::write_framed(send, message.as_bytes()).await?;
        } else {
            anyhow::bail!("Chat stream for peer {peer_id} is unavailable");
        }
        Ok(())
    }

    pub async fn send_chat_to_all(&self, message: &str) -> Vec<String> {
        let streams: Vec<(String, Arc<Mutex<Option<SendStream>>>)> = {
            let peers = self.peers.lock().await;
            peers
                .iter()
                .map(|(peer_id, peer)| (peer_id.clone(), peer.chat_send.clone()))
                .collect()
        };

        let mut failed = Vec::new();
        for (peer_id, stream) in streams {
            let mut guard = stream.lock().await;
            let result = if let Some(ref mut send) = *guard {
                crate::messages::write_framed(send, message.as_bytes()).await
            } else {
                failed.push(peer_id);
                continue;
            };

            if result.is_err() {
                failed.push(peer_id);
            }
        }

        failed
    }

    pub async fn send_heartbeat_to_all(&self) {
        let peer_ids: Vec<String> = {
            let peers = self.peers.lock().await;
            peers.keys().cloned().collect()
        };

        for peer_id in peer_ids {
            let _ = self.send_control(&peer_id, &ControlAction::Heartbeat).await;
        }
    }

    pub async fn prune_stale_peers(&self, max_idle_ms: u64) {
        let now = Self::current_timestamp_ms();
        let stale_peer_ids: Vec<String> = {
            let peers = self.peers.lock().await;
            peers
                .iter()
                .filter_map(|(peer_id, peer)| {
                    let idle_ms = now.saturating_sub(peer.last_activity_ms.load(Ordering::Relaxed));
                    if idle_ms > max_idle_ms {
                        Some(peer_id.clone())
                    } else {
                        None
                    }
                })
                .collect()
        };

        for peer_id in stale_peer_ids {
            tracing::info!("Pruning stale peer {peer_id} after inactivity timeout");
            Self::cleanup_peer_internal(
                &peer_id,
                &self.peers,
                &self.peer_tickets,
                &self.audio_sequences,
                &self.video_receive_state,
                &self.event_tx,
                Some(b"peer timeout"),
            )
            .await;
        }
    }

    pub async fn send_control(&self, peer_id: &str, action: &ControlAction) -> Result<()> {
        let data = serde_json::to_vec(action)?;
        let stream = {
            let peers = self.peers.lock().await;
            peers.get(peer_id).map(|p| p.control_send.clone())
        };
        let Some(s) = stream else {
            anyhow::bail!("Peer {peer_id} is not connected");
        };

        let mut guard = s.lock().await;
        if let Some(ref mut send) = *guard {
            crate::messages::write_framed(send, &data).await?;
        } else {
            anyhow::bail!("Control stream for peer {peer_id} is unavailable");
        }
        Ok(())
    }

    pub async fn snapshot_network_stats(&self) -> Vec<NetworkPeerStats> {
        let peers = self.peers.lock().await;
        let video_state = self.video_receive_state.lock().await;
        let _now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);

        peers
            .iter()
            .map(|(peer_id, peer)| {
                let mut paths = peer.connection.paths();
                let _ = paths.update();
                let path_list = paths.peek().clone();
                let path_stats = path_list
                    .iter()
                    .find(|path| path.is_selected() && !path.is_closed())
                    .or_else(|| path_list.iter().find(|path| !path.is_closed()))
                    .and_then(|path| path.stats());

                let rtt_ms = path_stats
                    .map(|path| path.rtt.as_millis() as u64)
                    .or_else(|| {
                        peer.connection
                            .rtt(PathId::ZERO)
                            .map(|rtt| rtt.as_millis() as u64)
                    })
                    .unwrap_or_default();
                let lost_packets = path_stats.map(|path| path.lost_packets).unwrap_or_default();
                let lost_bytes = path_stats.map(|path| path.lost_bytes).unwrap_or_default();
                let latest_video_age_ms = video_state
                    .get(peer_id)
                    .map(|state| state.last_received.elapsed().as_millis() as u64)
                    .unwrap_or_default();

                NetworkPeerStats {
                    peer_id: peer_id.clone(),
                    rtt_ms,
                    lost_packets,
                    lost_bytes,
                    datagram_send_buffer_space: peer.connection.datagram_send_buffer_space(),
                    latest_video_age_ms,
                }
            })
            .collect()
    }

    pub async fn get_peer_outbound_bitrate(&self, peer_id: &str) -> u32 {
        let peers = self.peers.lock().await;
        peers
            .get(peer_id)
            .map(|p| p.outbound_bitrate_bps.load(Ordering::Relaxed))
            .unwrap_or(0)
    }

    pub async fn set_peer_outbound_bitrate(&self, peer_id: &str, bitrate_bps: u32) {
        let peers = self.peers.lock().await;
        if let Some(peer) = peers.get(peer_id) {
            peer.outbound_bitrate_bps
                .store(bitrate_bps, Ordering::Relaxed);
        }
    }

    // ── DM connection management ────────────────────────────────────────

    pub async fn connect_dm(&self, node_id_str: &str) -> Result<()> {
        // Skip if we already have a live connection to this peer
        if self.dm_peers.lock().await.contains_key(node_id_str) {
            return Ok(());
        }

        let node_public_key: iroh::PublicKey = node_id_str
            .parse()
            .map_err(|_| anyhow::anyhow!("Invalid node ID: {node_id_str}"))?;
        let addr = iroh::EndpointAddr::new(node_public_key)
            .with_relay_url(crate::node::RELAY_URL_PARSED.clone());

        let endpoint = {
            let guard = self.endpoint.lock().await;
            guard
                .clone()
                .ok_or_else(|| anyhow::anyhow!("Endpoint not initialized"))?
        };

        let connection: iroh::endpoint::Connection =
            endpoint.connect(addr, crate::node::NAFAQ_DM_ALPN).await?;
        let peer_id = connection.remote_id().to_string();

        let (mut dm_send, mut dm_recv): (iroh::endpoint::SendStream, iroh::endpoint::RecvStream) =
            connection.open_bi().await?;
        dm_send.write_all(&[STREAM_DM]).await?;

        let dm_peer = DmPeerConnection {
            connection: connection.clone(),
            dm_send: Arc::new(Mutex::new(Some(dm_send))),
        };

        self.dm_peers.lock().await.insert(peer_id.clone(), dm_peer);

        // Spawn a reader for the initial bistream's recv side so the remote
        // peer can reply on the same bistream (via accept_bi's send half).
        {
            let event_tx = self.event_tx.clone();
            let peer_id = peer_id.clone();
            tokio::spawn(async move {
                run_dm_reader(&mut dm_recv, &peer_id, &event_tx).await;
            });
        }

        // Spawn a reader task for additional incoming DM bistreams on this connection
        let event_tx = self.event_tx.clone();
        let dm_peers_ref = self.dm_peers.clone();
        let peer_id_reader = peer_id.clone();
        let connection_reader = connection.clone();
        tokio::spawn(async move {
            loop {
                match connection_reader.accept_bi().await {
                    Ok((_, mut recv)) => {
                        let mut type_buf = [0u8; 1];
                        if recv.read_exact(&mut type_buf).await.is_err() {
                            continue;
                        }
                        if type_buf[0] != STREAM_DM {
                            continue;
                        }
                        let event_tx = event_tx.clone();
                        let peer_id = peer_id_reader.clone();
                        tokio::spawn(async move {
                            run_dm_reader(&mut recv, &peer_id, &event_tx).await;
                        });
                    }
                    Err(_) => break,
                }
            }

            // Connection closed — clean up and emit DmDisconnected
            if dm_peers_ref.lock().await.remove(&peer_id_reader).is_some() {
                let _ = event_tx.send(Event::DmDisconnected {
                    peer_id: peer_id_reader,
                });
            }
        });

        // Spawn a task to detect connection closure
        let event_tx_closed = self.event_tx.clone();
        let dm_peers_closed = self.dm_peers.clone();
        let peer_id_closed = peer_id.clone();
        tokio::spawn(async move {
            connection.closed().await;
            if dm_peers_closed
                .lock()
                .await
                .remove(&peer_id_closed)
                .is_some()
            {
                let _ = event_tx_closed.send(Event::DmDisconnected {
                    peer_id: peer_id_closed,
                });
            }
        });

        let _ = self.event_tx.send(Event::DmConnected { peer_id });

        Ok(())
    }

    pub async fn send_dm(&self, peer_id: &str, message: &DmMessage) -> Result<()> {
        let stream = {
            let dm_peers = self.dm_peers.lock().await;
            dm_peers.get(peer_id).map(|p| p.dm_send.clone())
        };
        let Some(s) = stream else {
            anyhow::bail!("DM peer {peer_id} is not connected");
        };

        let data = serde_json::to_vec(message)?;
        let mut guard = s.lock().await;
        if let Some(ref mut send) = *guard {
            crate::messages::write_framed(send, &data).await?;
        } else {
            anyhow::bail!("DM stream for peer {peer_id} is unavailable");
        }
        Ok(())
    }

    pub async fn disconnect_dm(&self, peer_id: &str) {
        let removed = self.dm_peers.lock().await.remove(peer_id);
        if let Some(dm_peer) = removed {
            dm_peer.connection.close(0u32.into(), b"dm_closed");
            let _ = self.event_tx.send(Event::DmDisconnected {
                peer_id: peer_id.to_string(),
            });
        }
    }

    pub async fn disconnect_peer(&self, peer_id: &str) -> Result<()> {
        Self::cleanup_peer_internal(
            peer_id,
            &self.peers,
            &self.peer_tickets,
            &self.audio_sequences,
            &self.video_receive_state,
            &self.event_tx,
            Some(b"call ended"),
        )
        .await;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::Arc;
    use std::time::Duration;

    use iroh::{endpoint::Connection, protocol::Router};
    use tokio::time::timeout;

    use crate::node;
    use crate::protocol::{NafaqDmProtocol, NafaqProtocol};

    #[tokio::test]
    async fn emits_single_disconnect_when_remote_endpoint_closes() {
        let (event_tx_a, _) = broadcast::channel::<Event>(64);
        let (audio_tx_a, _) = broadcast::channel::<AudioPacket>(8);
        let (video_tx_a, _) = broadcast::channel::<VideoPacket>(8);
        let mgr_a = Arc::new(ConnectionManager::new(
            event_tx_a.clone(),
            audio_tx_a,
            video_tx_a,
        ));

        let (event_tx_b, _) = broadcast::channel::<Event>(64);
        let (audio_tx_b, _) = broadcast::channel::<AudioPacket>(8);
        let (video_tx_b, _) = broadcast::channel::<VideoPacket>(8);
        let mgr_b = Arc::new(ConnectionManager::new(event_tx_b, audio_tx_b, video_tx_b));

        let endpoint_a = node::create_endpoint().await.unwrap();
        let endpoint_b = node::create_endpoint().await.unwrap();
        mgr_a.set_endpoint(endpoint_a.clone()).await;
        mgr_b.set_endpoint(endpoint_b.clone()).await;

        let router_a = Router::builder(endpoint_a.clone())
            .accept(node::NAFAQ_ALPN, NafaqProtocol::new(mgr_a.clone()))
            .accept(node::NAFAQ_DM_ALPN, NafaqDmProtocol::new(mgr_a.clone()))
            .spawn();

        let mut rx_a = event_tx_a.subscribe();
        let addr_a = node::parse_ticket(&node::generate_ticket(&endpoint_a))
            .unwrap()
            .endpoint_addr()
            .clone();

        mgr_b.connect_to_peer(&endpoint_b, addr_a).await.unwrap();

        let connected_peer = timeout(Duration::from_secs(10), async {
            loop {
                match rx_a.recv().await {
                    Ok(Event::PeerConnected { peer_id }) => break peer_id,
                    Ok(_) => {}
                    Err(_) => continue,
                }
            }
        })
        .await
        .unwrap();

        endpoint_b.close().await;

        let disconnected_peer = timeout(Duration::from_secs(15), async {
            loop {
                match rx_a.recv().await {
                    Ok(Event::PeerDisconnected { peer_id }) => break peer_id,
                    Ok(_) => {}
                    Err(_) => continue,
                }
            }
        })
        .await
        .unwrap();

        assert_eq!(disconnected_peer, connected_peer);

        let second_disconnect = timeout(Duration::from_millis(750), async {
            loop {
                match rx_a.recv().await {
                    Ok(Event::PeerDisconnected { peer_id }) => break Some(peer_id),
                    Ok(_) => {}
                    Err(_) => break None,
                }
            }
        })
        .await;
        assert!(
            second_disconnect.is_err(),
            "received duplicate disconnect event"
        );

        router_a.shutdown().await.ok();
        endpoint_a.close().await;
    }

    async fn wait_for_selected_relay(conn: &Connection) {
        timeout(Duration::from_secs(10), async {
            loop {
                let paths = conn.paths().get();
                if paths
                    .iter()
                    .any(|path| path.is_selected() && path.is_relay())
                {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .expect("timed out waiting for relay path selection");
    }

    #[tokio::test]
    async fn two_nodes_connect_via_relay_only_addr() {
        let endpoint_a = node::create_endpoint().await.unwrap();
        let endpoint_b = node::create_endpoint().await.unwrap();

        let mut relay_only_addr = endpoint_a.addr();
        relay_only_addr.addrs.retain(|addr| addr.is_relay());
        assert!(
            !relay_only_addr.addrs.is_empty(),
            "endpoint A did not publish any relay address"
        );

        let accept_task = tokio::spawn({
            let endpoint_a = endpoint_a.clone();
            async move {
                timeout(Duration::from_secs(30), async {
                    endpoint_a.accept().await.unwrap().await.unwrap()
                })
                .await
                .expect("timed out accepting relay connection")
            }
        });

        let conn_b = timeout(
            Duration::from_secs(30),
            endpoint_b.connect(relay_only_addr, node::NAFAQ_ALPN),
        )
        .await
        .expect("timed out dialing relay-only address")
        .unwrap();

        let conn_a = accept_task.await.unwrap();

        wait_for_selected_relay(&conn_a).await;
        wait_for_selected_relay(&conn_b).await;

        let mut send = conn_b.open_uni().await.unwrap();
        send.write_all(b"relay-ok").await.unwrap();
        send.finish().unwrap();

        let mut recv = timeout(Duration::from_secs(10), conn_a.accept_uni())
            .await
            .expect("timed out waiting for unidirectional stream")
            .unwrap();
        let mut buf = [0u8; 8];
        recv.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"relay-ok");

        conn_b.close(0u32.into(), b"done");
        conn_a.close(0u32.into(), b"done");
        endpoint_b.close().await;
        endpoint_a.close().await;
    }

    #[tokio::test]
    async fn video_queue_keeps_pending_keyframe_over_newer_delta_frame() {
        let writer = PeerVideoWriter::new();

        writer
            .enqueue_latest(PendingVideoFrame {
                timestamp_ms: 1,
                payload: vec![0x00, 0x00, 0x00, 0x01, 0x65, 0x88],
                is_keyframe: true,
            })
            .await;

        writer
            .enqueue_latest(PendingVideoFrame {
                timestamp_ms: 2,
                payload: vec![0x00, 0x00, 0x00, 0x01, 0x41, 0x88],
                is_keyframe: false,
            })
            .await;

        let pending = writer.pending.lock().await.clone().expect("pending frame");
        assert!(pending.is_keyframe);
        assert_eq!(pending.timestamp_ms, 1);
    }
}
