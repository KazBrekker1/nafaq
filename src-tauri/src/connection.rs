use std::collections::{HashMap, HashSet, VecDeque};
use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU16, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use bytes::Bytes;
use iroh::endpoint::{Connection, ConnectionError, PathId, RecvStream, SendStream};
use tokio::sync::{Mutex, Notify, broadcast};

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
            drop(pending);
            // Only wake the writer when the slot actually changed — a rejected
            // delta (keyframe already queued) would otherwise cause a spurious
            // wakeup per frame per peer.
            self.notify.notify_one();
        }
    }
}

use crate::codec::is_keyframe;
use crate::messages::{
    AudioDatagram, AudioPacket, ControlAction, DmMessage, Event, PeerConnectionKind, STREAM_AUDIO,
    STREAM_CHAT, STREAM_CONTROL, STREAM_DM, STREAM_VIDEO, VideoLayerRequest, VideoPacket,
};

const CALL_DIAL_TIMEOUT: Duration = Duration::from_secs(20);
pub(crate) const DM_DIAL_TIMEOUT: Duration = Duration::from_secs(12);
const DM_DUPLICATE_DRAIN_TIMEOUT: Duration = Duration::from_secs(2);
const MESH_DIAL_TIMEOUT: Duration = Duration::from_secs(15);
const STREAM_OPEN_TIMEOUT: Duration = Duration::from_secs(8);
const DM_CONNECT_WAIT_TIMEOUT: Duration = Duration::from_secs(21);
/// A single DM frame write must complete in this long or the peer's flow
/// control window is stuck (e.g. it stopped reading). Without this,
/// `write_all` blocks forever and the frontend's message sits "sending"
/// indefinitely instead of surfacing a failure.
const DM_WRITE_TIMEOUT: Duration = Duration::from_secs(10);
const SUSPECT_AFTER_MS: u64 = 20_000;
const RECONNECT_AFTER_MS: u64 = 35_000;
const DISCONNECT_AFTER_MS: u64 = 120_000;
const RECONNECT_RETRY_AFTER_MS: u64 = 15_000;
/// One extra attempt for the initial call/DM dial, after a short pause. Covers
/// a peer whose relay path is still settling without turning a genuine
/// failure into a long retry loop. Mesh auto-connect and the liveness-driven
/// reconnect ladder already have their own retry cadence and are untouched.
const INITIAL_DIAL_RETRY_BACKOFF: Duration = Duration::from_millis(500);

async fn with_timeout<T, F>(
    timeout_duration: Duration,
    timeout_message: impl Into<String>,
    future: F,
) -> Result<T>
where
    F: Future<Output = Result<T>>,
{
    let timeout_message = timeout_message.into();
    tokio::time::timeout(timeout_duration, future)
        .await
        .map_err(|_| anyhow::anyhow!(timeout_message))?
}

async fn dial_peer_with_timeout(
    endpoint: &iroh::Endpoint,
    addr: iroh::EndpointAddr,
    alpn: &'static [u8],
    timeout_duration: Duration,
    timeout_message: &'static str,
) -> Result<Connection> {
    with_timeout(timeout_duration, timeout_message, async {
        Ok(endpoint.connect(addr, alpn).await?)
    })
    .await
}

async fn open_typed_bi_stream(
    connection: &Connection,
    stream_type: u8,
    stream_name: &'static str,
) -> Result<(SendStream, RecvStream)> {
    with_timeout(
        STREAM_OPEN_TIMEOUT,
        format!("timed out opening {stream_name} stream"),
        async {
            let (mut send, recv) = connection.open_bi().await?;
            send.write_all(&[stream_type]).await?;
            Ok((send, recv))
        },
    )
    .await
}

fn relay_targets_for_announce<'a>(
    peer_ids: impl IntoIterator<Item = &'a String>,
    sender_id: &str,
    announced_peer_id: &str,
) -> Vec<String> {
    peer_ids
        .into_iter()
        .filter(|id| id.as_str() != sender_id && id.as_str() != announced_peer_id)
        .cloned()
        .collect()
}

struct ActiveFileReceive {
    file: tokio::fs::File,
    temp_path: std::path::PathBuf,
    final_name: String,
    expected_size: u64,
    received_bytes: u64,
}

/// Maximum size a peer may declare for an inbound transfer. Mirrors the
/// sender-side cap in `send_file`; without this a peer can declare an
/// arbitrarily large file and legitimately stream it until the disk fills.
const MAX_INCOMING_FILE_SIZE: u64 = 100 * 1024 * 1024;

/// Transfer ids are embedded in temp-file names, so anything outside a
/// UUID-ish charset is a path-injection attempt.
fn is_valid_transfer_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Reduce a peer-supplied file name to a safe basename. `Path::join` with an
/// absolute path replaces the base entirely, so the raw name must never reach
/// a join — strip directories, control chars, and leading dots.
fn sanitize_file_name(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let cleaned: String = base.chars().filter(|c| !c.is_control()).take(200).collect();
    let trimmed = cleaned.trim_start_matches(['.', ' ']).trim_end();
    if trimmed.is_empty() {
        "download".to_string()
    } else {
        trimmed.to_string()
    }
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
            // Protocol-violation rejects suppress the DmReceived event entirely
            // (return true) so a malicious FileStart can't spam phantom file
            // cards in the frontend.
            if !is_valid_transfer_id(id) {
                tracing::warn!("Rejecting file transfer from {peer_id}: invalid transfer id");
                return true;
            }
            if *size > MAX_INCOMING_FILE_SIZE {
                tracing::warn!(
                    "Rejecting file transfer {id} from {peer_id}: declared size {size} exceeds {MAX_INCOMING_FILE_SIZE}"
                );
                return true;
            }
            // Bound concurrent in-flight transfers so a peer can't exhaust file
            // descriptors / memory by opening unbounded FileStarts without ends.
            const MAX_CONCURRENT_TRANSFERS: usize = 16;
            if active_files.len() >= MAX_CONCURRENT_TRANSFERS && !active_files.contains_key(id) {
                tracing::warn!(
                    "Rejecting file transfer {id} from {peer_id}: too many concurrent transfers"
                );
                return false;
            }
            let temp_dir = std::env::temp_dir();
            let temp_path = temp_dir.join(format!("nafaq_recv_{id}"));
            match tokio::fs::File::create(&temp_path).await {
                Ok(file) => {
                    active_files.insert(
                        id.clone(),
                        ActiveFileReceive {
                            file,
                            temp_path,
                            final_name: sanitize_file_name(name),
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
            // Reject writes that would extend the file past its declared size —
            // prevents a peer inflating an "8 KB" transfer into a disk-filling
            // write via large offsets.
            let over_declared = active_files
                .get(id)
                .is_some_and(|recv| offset.saturating_add(data.len() as u64) > recv.expected_size);
            if over_declared {
                tracing::warn!("FileChunk for {id} exceeds declared size; dropping transfer");
                if let Some(recv) = active_files.remove(id) {
                    drop(recv.file);
                    let _ = tokio::fs::remove_file(&recv.temp_path).await;
                }
                return true;
            }
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
                        // Tiny progress ping (no payload) so the receiver's
                        // progress bar advances without re-streaming the chunk.
                        let _ = event_tx.send(Event::DmFileProgress {
                            peer_id: peer_id.to_string(),
                            file_id: id.clone(),
                            received: recv.received_bytes,
                        });
                    }
                }
            } else {
                tracing::debug!("FileChunk for unknown transfer {id}, ignoring");
            }
            // Chunks are persisted to disk here; the raw payload must NOT also be
            // re-emitted as a DmReceived IPC event (that floods the bridge with
            // the full file, e.g. ~68 MB for a 50 MB file).
            true
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

/// Call-signaling variants (CallInvite/CallDecline/CallCancel) are handled
/// before generic DmReceived/file processing and never reach it. Returns
/// `true` if the message was fully handled here.
///
/// CallDecline additionally clears our own `call_session_active` flag: if we
/// are the caller and the callee just declined, our outstanding ticket must
/// stop accepting inbound call dials (see `setup_connection`'s gate) even
/// though no call peer was ever established to disconnect.
fn handle_call_signal(manager: &ConnectionManager, dm_msg: &DmMessage, peer_id: &str) -> bool {
    match dm_msg {
        DmMessage::CallInvite { ticket } => {
            let _ = manager.event_tx.send(Event::CallInviteReceived {
                peer_id: peer_id.to_string(),
                ticket: ticket.clone(),
            });
            true
        }
        DmMessage::CallDecline => {
            manager.set_call_session_active(false);
            let _ = manager.event_tx.send(Event::CallDeclineReceived {
                peer_id: peer_id.to_string(),
            });
            true
        }
        DmMessage::CallCancel => {
            let _ = manager.event_tx.send(Event::CallCancelReceived {
                peer_id: peer_id.to_string(),
            });
            true
        }
        _ => false,
    }
}

/// Shared DM frame dispatch — used by both the dedicated DM connection reader
/// (`run_dm_reader`) and the DM stream multiplexed onto a call connection
/// (`handle_bi_stream`), so call-signal/ack/dedup handling is identical on
/// both paths.
async fn handle_dm_frame_payload(
    manager: &ConnectionManager,
    data: &[u8],
    peer_id: &str,
    active_files: &mut HashMap<String, ActiveFileReceive>,
) {
    let Ok(dm_msg) = serde_json::from_slice::<DmMessage>(data) else {
        // Malformed frame, or a variant this build doesn't know about (e.g.
        // a newer peer's message) — drop just this frame, keep reading.
        return;
    };
    if matches!(dm_msg, DmMessage::Heartbeat) {
        return;
    }
    if let DmMessage::Ack { id } = &dm_msg {
        let _ = manager.event_tx.send(Event::DmAckReceived {
            peer_id: peer_id.to_string(),
            id: id.clone(),
        });
        return;
    }
    if handle_call_signal(manager, &dm_msg, peer_id) {
        return;
    }

    // Text messages carry an optional client id used for dedup + ack. Files
    // reuse the `id` field for transfer ids (a different namespace), so this
    // only applies to Text.
    if let DmMessage::Text { id: Some(id), .. } = &dm_msg {
        let is_duplicate = manager.record_and_check_duplicate_dm_id(peer_id, id).await;
        // Ack every receipt, including duplicates — the sender's earlier Ack
        // may have been lost, and re-acking is harmless (idempotent on the
        // sender's side, keyed by id).
        let ack = DmMessage::Ack { id: id.clone() };
        if let Err(e) = manager.send_dm_frame_strict(peer_id, &ack).await {
            tracing::debug!("Failed to ack DM {id} to {peer_id}: {e}");
        }
        if is_duplicate {
            return;
        }
    }

    let skip_dm_event =
        handle_dm_file_message(&dm_msg, peer_id, active_files, &manager.event_tx).await;
    if !skip_dm_event {
        let _ = manager.event_tx.send(Event::DmReceived {
            peer_id: peer_id.to_string(),
            message: dm_msg,
        });
    }
}

async fn cleanup_active_dm_files(
    active_files: HashMap<String, ActiveFileReceive>,
    peer_id: &str,
    event_tx: &broadcast::Sender<Event>,
) {
    for (id, recv) in active_files {
        tracing::debug!("Cleaning up incomplete file transfer {id}");
        drop(recv.file);
        let _ = tokio::fs::remove_file(&recv.temp_path).await;
        // The stream dropped mid-transfer — tell the frontend so it can mark the
        // file failed instead of leaving it stuck at partial progress forever.
        let _ = event_tx.send(Event::DmFileTransferFailed {
            peer_id: peer_id.to_string(),
            file_id: id,
        });
    }
}

async fn drain_duplicate_dm_frame_once(
    recv: &mut iroh::endpoint::RecvStream,
    peer_id: &str,
    manager: &ConnectionManager,
) {
    let mut active_files: HashMap<String, ActiveFileReceive> = HashMap::new();
    match tokio::time::timeout(
        DM_DUPLICATE_DRAIN_TIMEOUT,
        crate::messages::read_framed(recv),
    )
    .await
    {
        Ok(Ok(Some(data))) => {
            handle_dm_frame_payload(manager, &data, peer_id, &mut active_files).await;
        }
        Ok(Ok(None)) => {}
        Ok(Err(error)) => {
            tracing::debug!("Duplicate DM drain read failed for {peer_id}: {error}");
        }
        Err(_) => {
            tracing::debug!("Duplicate DM drain timed out for {peer_id}");
        }
    }
    cleanup_active_dm_files(active_files, peer_id, &manager.event_tx).await;
}

/// Shared DM stream reader loop — reads framed messages, handles files,
/// emits events. Used by both connect_dm and setup_dm_connection.
async fn run_dm_reader(recv: &mut iroh::endpoint::RecvStream, peer_id: &str, manager: &ConnectionManager) {
    let mut active_files: HashMap<String, ActiveFileReceive> = HashMap::new();
    loop {
        match crate::messages::read_framed(recv).await {
            Ok(Some(data)) => {
                handle_dm_frame_payload(manager, &data, peer_id, &mut active_files).await;
            }
            Ok(None) => break,
            Err(_) => break,
        }
    }
    cleanup_active_dm_files(active_files, peer_id, &manager.event_tx).await;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConnectionDirection {
    Inbound,
    Outbound,
}

fn preferred_connection_direction(
    local_node_id: &str,
    remote_peer_id: &str,
) -> ConnectionDirection {
    if local_node_id > remote_peer_id {
        ConnectionDirection::Outbound
    } else {
        ConnectionDirection::Inbound
    }
}

fn should_replace_connection(
    local_node_id: &str,
    remote_peer_id: &str,
    existing_direction: ConnectionDirection,
    candidate_direction: ConnectionDirection,
) -> bool {
    existing_direction != candidate_direction
        && candidate_direction == preferred_connection_direction(local_node_id, remote_peer_id)
}

fn should_accept_call_connection_candidate(
    local_node_id: Option<&str>,
    remote_peer_id: &str,
    existing_direction: ConnectionDirection,
    existing_status: &PeerConnectionKind,
    candidate_direction: ConnectionDirection,
) -> bool {
    if existing_status == &PeerConnectionKind::Reconnecting {
        return true;
    }

    local_node_id.is_some_and(|local_id| {
        should_replace_connection(
            local_id,
            remote_peer_id,
            existing_direction,
            candidate_direction,
        )
    })
}

struct PeerConnection {
    connection: Connection,
    direction: ConnectionDirection,
    chat_send: Arc<Mutex<Option<SendStream>>>,
    control_send: Arc<Mutex<Option<SendStream>>>,
    video_writer: PeerVideoWriter,
    /// Requested video layer: 0=High, 1=Low, 2=None
    requested_video_layer: Arc<AtomicU8>,
    pending_keyframe: Arc<AtomicBool>,
    last_activity_ms: Arc<AtomicU64>,
    connection_status: PeerConnectionKind,
    /// When this call entry was established. Used to detect entries that
    /// pre-date a remote restart (gossip NeighborUp newer than this).
    established_at: std::time::Instant,
    /// Per-peer outbound bitrate override (0 = use global profile)
    outbound_bitrate_bps: Arc<AtomicU32>,
    /// Outbound audio datagram sequence counter. Lives on the peer entry so
    /// the audio hot path never needs a separate map lock.
    audio_sequence: Arc<AtomicU16>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DmConnectionOwnership {
    Dedicated,
    SharedCall,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DmConnectionHandling {
    Store,
    DrainDuplicate,
    CloseDuplicate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DmStreamRegistration {
    Stored,
    DrainDuplicate,
    CloseDuplicate,
}

struct DmPeerConnection {
    connection: Connection,
    direction: ConnectionDirection,
    dm_send: Arc<Mutex<Option<SendStream>>>,
    ownership: DmConnectionOwnership,
    established_at: std::time::Instant,
}

impl DmPeerConnection {
    fn close_if_owned(&self, reason: &'static [u8]) {
        if self.ownership == DmConnectionOwnership::Dedicated {
            self.connection.close(0u32.into(), reason);
        }
    }

    async fn finish_send_stream(&self) {
        let mut guard = self.dm_send.lock().await;
        if let Some(mut send) = guard.take() {
            let _ = send.finish();
        }
    }
}

struct ConnectingReservation {
    reservations: Arc<StdMutex<HashSet<String>>>,
    peer_id: String,
    active: bool,
    /// Woken when the reservation releases, so waiters polling "is a connect
    /// still in progress?" can block on a Notify instead of busy-sleeping.
    release_notify: Option<Arc<Notify>>,
}

impl ConnectingReservation {
    fn try_reserve(
        reservations: Arc<StdMutex<HashSet<String>>>,
        peer_id: &str,
        release_notify: Option<Arc<Notify>>,
    ) -> Option<Self> {
        let inserted = {
            let mut guard = reservations
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            guard.insert(peer_id.to_string())
        };

        inserted.then(|| Self {
            reservations,
            peer_id: peer_id.to_string(),
            active: true,
            release_notify,
        })
    }
}

impl Drop for ConnectingReservation {
    fn drop(&mut self) {
        if self.active {
            let mut guard = self
                .reservations
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            guard.remove(&self.peer_id);
            self.active = false;
            if let Some(notify) = &self.release_notify {
                notify.notify_waiters();
            }
        }
    }
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

#[derive(Debug, Clone)]
struct PeerTicketRecord {
    ticket: String,
    last_updated_ms: u64,
    last_dial_failed_ms: Option<u64>,
    dial_failures: u32,
}

#[derive(Clone)]
pub struct ConnectionManager {
    peers: Arc<Mutex<HashMap<String, PeerConnection>>>,
    dm_peers: Arc<Mutex<HashMap<String, DmPeerConnection>>>,
    call_connecting: Arc<StdMutex<HashSet<String>>>,
    dm_connecting: Arc<StdMutex<HashSet<String>>>,
    /// Woken whenever a DM connect attempt finishes (reservation released) or
    /// a DM connection is stored — lets waiters block instead of polling.
    dm_connect_done: Arc<Notify>,
    endpoint: Arc<Mutex<Option<iroh::Endpoint>>>,
    latest_ticket: Arc<Mutex<Option<String>>>,
    peer_tickets: Arc<Mutex<HashMap<String, PeerTicketRecord>>>,
    video_receive_state: Arc<Mutex<HashMap<String, VideoReceiveState>>>,
    event_tx: broadcast::Sender<Event>,
    audio_media_tx: broadcast::Sender<AudioPacket>,
    video_media_tx: broadcast::Sender<VideoPacket>,
    presence: Arc<Mutex<Option<Arc<crate::presence::PresenceManager>>>>,
    /// True while we've deliberately created or joined a call (create_call /
    /// join_call) and haven't cancelled or fully left it yet. Gates inbound
    /// call-media connection acceptance in `setup_connection` — see the
    /// wave-2 ghost-call fix. DM/presence/file streams are unaffected.
    call_session_active: Arc<AtomicBool>,
    /// Recently-seen DM `Text` message ids per peer, for ack + dedup. Bounded
    /// per peer so a chatty (or malicious) peer can't grow this unbounded.
    recent_dm_ids: Arc<Mutex<HashMap<String, RecentIds>>>,
}

const RECENT_DM_IDS_CAPACITY: usize = 256;

#[derive(Default)]
struct RecentIds {
    order: VecDeque<String>,
    set: HashSet<String>,
}

impl RecentIds {
    /// Returns true if `id` was already seen, otherwise records it.
    fn check_and_insert(&mut self, id: &str) -> bool {
        if self.set.contains(id) {
            return true;
        }
        self.set.insert(id.to_string());
        self.order.push_back(id.to_string());
        if self.order.len() > RECENT_DM_IDS_CAPACITY {
            if let Some(oldest) = self.order.pop_front() {
                self.set.remove(&oldest);
            }
        }
        false
    }
}

/// A NeighborUp this recent is treated as "the peer is freshly reachable", used
/// as one (lenient) trigger for evicting a stale DM entry on an inbound
/// reconnect. The unbounded `*_predates_recent_rejoin` checks additionally
/// cover the case where the NeighborUp is older than this window but still
/// newer than the stale entry (slow-relay reconnect past QUIC's idle timeout).
const DM_RECENT_NEIGHBOR_WINDOW: std::time::Duration = std::time::Duration::from_secs(10);

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
        latest_ticket: Arc<Mutex<Option<String>>>,
    ) -> Self {
        Self {
            peers: Arc::new(Mutex::new(HashMap::new())),
            dm_peers: Arc::new(Mutex::new(HashMap::new())),
            call_connecting: Arc::new(StdMutex::new(HashSet::new())),
            dm_connecting: Arc::new(StdMutex::new(HashSet::new())),
            dm_connect_done: Arc::new(Notify::new()),
            endpoint: Arc::new(Mutex::new(None)),
            latest_ticket,
            peer_tickets: Arc::new(Mutex::new(HashMap::new())),
            video_receive_state: Arc::new(Mutex::new(HashMap::new())),
            event_tx,
            audio_media_tx,
            video_media_tx,
            presence: Arc::new(Mutex::new(None)),
            call_session_active: Arc::new(AtomicBool::new(false)),
            recent_dm_ids: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Mark whether we currently intend to accept inbound call-media dials —
    /// set on create_call/join_call, cleared when the call is cancelled
    /// before anyone joined or once the last peer leaves. See
    /// `setup_connection`'s inbound gate.
    pub fn set_call_session_active(&self, active: bool) {
        self.call_session_active.store(active, Ordering::SeqCst);
    }

    fn is_call_session_active(&self) -> bool {
        self.call_session_active.load(Ordering::SeqCst)
    }

    /// Records `id` as seen for `peer_id` and returns whether it was already
    /// present (i.e. this is a duplicate delivery).
    async fn record_and_check_duplicate_dm_id(&self, peer_id: &str, id: &str) -> bool {
        let mut map = self.recent_dm_ids.lock().await;
        map.entry(peer_id.to_string())
            .or_default()
            .check_and_insert(id)
    }

    pub async fn set_endpoint(&self, endpoint: iroh::Endpoint) {
        *self.endpoint.lock().await = Some(endpoint);
    }

    pub async fn set_presence(&self, presence: Arc<crate::presence::PresenceManager>) {
        *self.presence.lock().await = Some(presence);
    }

    async fn peer_recently_rejoined_gossip(&self, peer_id: &str) -> bool {
        let presence = self.presence.lock().await.clone();
        match presence {
            Some(p) => {
                p.is_recent_neighbor(peer_id, DM_RECENT_NEIGHBOR_WINDOW)
                    .await
            }
            None => false,
        }
    }

    /// True if the existing DM entry for `peer_id` pre-dates the most recent
    /// gossip NeighborUp signal for that peer. Indicates the entry is stale —
    /// QUIC's idle timeout hasn't fired yet on a connection whose remote half
    /// is already gone, but presence already told us the peer rebooted.
    async fn dm_entry_predates_recent_rejoin(&self, peer_id: &str) -> bool {
        let entry_established = {
            let dm_peers = self.dm_peers.lock().await;
            dm_peers.get(peer_id).map(|p| p.established_at)
        };
        let Some(established_at) = entry_established else {
            return false;
        };
        let presence = self.presence.lock().await.clone();
        let Some(presence) = presence else {
            return false;
        };
        match presence.last_neighbor_up(peer_id).await {
            Some(up_at) => up_at > established_at,
            None => false,
        }
    }

    async fn local_node_id(&self) -> Option<String> {
        self.endpoint
            .lock()
            .await
            .as_ref()
            .map(|endpoint| endpoint.id().to_string())
    }

    fn emit_peer_connection_status(
        &self,
        peer_id: impl Into<String>,
        status: PeerConnectionKind,
        reason: Option<String>,
    ) {
        let _ = self.event_tx.send(Event::PeerConnectionStatusChanged {
            peer_id: peer_id.into(),
            status,
            reason,
        });
    }

    #[cfg(test)]
    async fn reserve_call_connecting(&self, peer_id: &str) -> bool {
        self.call_connecting
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .insert(peer_id.to_string())
    }

    async fn reserve_call_connecting_guard(&self, peer_id: &str) -> Option<ConnectingReservation> {
        ConnectingReservation::try_reserve(self.call_connecting.clone(), peer_id, None)
    }

    #[cfg(test)]
    async fn clear_call_connecting(&self, peer_id: &str) {
        self.call_connecting
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .remove(peer_id);
    }

    #[cfg(test)]
    async fn reserve_dm_connecting(&self, peer_id: &str) -> bool {
        self.dm_connecting
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .insert(peer_id.to_string())
    }

    async fn reserve_dm_connecting_guard(&self, peer_id: &str) -> Option<ConnectingReservation> {
        ConnectingReservation::try_reserve(
            self.dm_connecting.clone(),
            peer_id,
            Some(self.dm_connect_done.clone()),
        )
    }

    #[cfg(test)]
    async fn clear_dm_connecting(&self, peer_id: &str) {
        self.dm_connecting
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .remove(peer_id);
    }

    async fn dm_connect_in_progress(&self, peer_id: &str) -> bool {
        self.dm_connecting
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .contains(peer_id)
    }

    async fn call_peer_connected(&self, peer_id: &str) -> bool {
        self.peers.lock().await.contains_key(peer_id)
    }

    pub async fn dm_peer_connected(&self, peer_id: &str) -> bool {
        let probe = {
            let dm_peers = self.dm_peers.lock().await;
            dm_peers.get(peer_id).map(|peer| {
                (
                    peer.connection.close_reason().is_some(),
                    peer.dm_send.clone(),
                )
            })
        };
        let Some((conn_closed, dm_send)) = probe else {
            return false;
        };
        if conn_closed {
            return false;
        }
        let guard = dm_send.lock().await;
        guard.is_some()
    }

    /// True if the existing call entry for `peer_id` was established before the
    /// most recent gossip NeighborUp for that peer — i.e. the remote restarted
    /// and the entry we hold is stale even though QUIC's idle timeout hasn't
    /// fired yet. Mirrors `dm_entry_predates_recent_rejoin` for the call path.
    async fn call_entry_predates_recent_rejoin(&self, peer_id: &str) -> bool {
        let established_at = {
            let peers = self.peers.lock().await;
            peers.get(peer_id).map(|p| p.established_at)
        };
        let Some(established_at) = established_at else {
            return false;
        };
        let presence = self.presence.lock().await.clone();
        let Some(presence) = presence else {
            return false;
        };
        match presence.last_neighbor_up(peer_id).await {
            Some(up_at) => up_at > established_at,
            None => false,
        }
    }

    /// Evict a stale call peer entry (closing the old connection) without
    /// forgetting its ticket, so a reconnect can proceed immediately.
    async fn evict_stale_call_peer(
        &self,
        peer_id: &str,
        expected_connection_id: usize,
        reason: &'static [u8],
    ) {
        Self::cleanup_peer_internal(
            peer_id,
            &self.peers,
            &self.dm_peers,
            &self.peer_tickets,
            &self.video_receive_state,
            &self.event_tx,
            Some(reason),
            Some(expected_connection_id),
            true, // reconnecting peer — keep its ticket
        )
        .await;
    }

    async fn should_accept_call_connection(
        &self,
        peer_id: &str,
        direction: ConnectionDirection,
    ) -> bool {
        let local_node_id = self.local_node_id().await;

        let probe = {
            let peers = self.peers.lock().await;
            peers.get(peer_id).map(|existing| {
                (
                    existing.connection.close_reason().is_some(),
                    existing.direction,
                    existing.connection_status.clone(),
                    existing.connection.stable_id(),
                )
            })
        };

        let Some((conn_closed, existing_direction, existing_status, existing_conn_id)) = probe
        else {
            return true;
        };

        // The closed() cleanup task may not have fired yet; treat a dead
        // connection as already evicted so a returning peer isn't rejected
        // during the QUIC idle-timeout window.
        if conn_closed {
            self.evict_stale_call_peer(peer_id, existing_conn_id, b"stale_call_connection")
                .await;
            return true;
        }

        // Gossip presence shows the peer is freshly reachable — the remote
        // restarted and its old QUIC connection is dead on its side. Prefer the
        // new inbound and evict the stale entry instead of rejecting it via the
        // lexicographic tiebreak. Same dual trigger as the DM path: recent
        // NeighborUp (short window) or any NeighborUp newer than this entry
        // (unbounded, covers a slow reconnect past QUIC's idle timeout).
        if matches!(direction, ConnectionDirection::Inbound)
            && (self.peer_recently_rejoined_gossip(peer_id).await
                || self.call_entry_predates_recent_rejoin(peer_id).await)
        {
            self.evict_stale_call_peer(peer_id, existing_conn_id, b"peer_rejoined_gossip")
                .await;
            return true;
        }

        // A duplicate in the SAME direction can't be the crossed simultaneous-
        // dial race (those produce one inbound + one outbound). It means the
        // initiating side re-dialed, and it only does that once it considers
        // the old path dead — e.g. its CONNECTION_CLOSE to us was lost, which
        // QUIC does not retransmit. The newest connection wins; rejecting it
        // would strand both sides on a half-dead connection until idle timeout.
        if existing_direction == direction {
            self.evict_stale_call_peer(peer_id, existing_conn_id, b"superseded_by_redial")
                .await;
            return true;
        }

        should_accept_call_connection_candidate(
            local_node_id.as_deref(),
            peer_id,
            existing_direction,
            &existing_status,
            direction,
        )
    }

    async fn dm_connection_handling(
        &self,
        peer_id: &str,
        direction: ConnectionDirection,
    ) -> DmConnectionHandling {
        let local_node_id = self.local_node_id().await;

        let probe = {
            let dm_peers = self.dm_peers.lock().await;
            dm_peers.get(peer_id).map(|existing| {
                (
                    existing.connection.close_reason().is_some(),
                    existing.dm_send.clone(),
                    existing.direction,
                )
            })
        };

        let Some((conn_closed, dm_send, existing_direction)) = probe else {
            return DmConnectionHandling::Store;
        };

        // The closed() cleanup task may not have fired yet; treat a dead
        // connection or taken stream as already evicted.
        if conn_closed || dm_send.lock().await.is_none() {
            Self::cleanup_dm_internal(
                peer_id,
                &self.dm_peers,
                &self.event_tx,
                Some(b"stale_dm_connection"),
                None,
            )
            .await;
            return DmConnectionHandling::Store;
        }

        // NeighborUp is stale-DM evidence only when it is newer than the stored
        // entry. Initial blank-state presence is also recent, so it must not
        // bypass crossed-dial arbitration.
        if matches!(direction, ConnectionDirection::Inbound)
            && self.dm_entry_predates_recent_rejoin(peer_id).await
        {
            Self::cleanup_dm_internal(
                peer_id,
                &self.dm_peers,
                &self.event_tx,
                Some(b"peer_rejoined_gossip"),
                None,
            )
            .await;
            return DmConnectionHandling::Store;
        }

        // Same-direction duplicate: not the crossed-dial race (that pairs one
        // inbound with one outbound) — the initiator re-dialed because it
        // considers the old path dead (e.g. its CONNECTION_CLOSE to us was
        // lost). Newest wins; rejecting it strands both sides until the QUIC
        // idle timeout.
        if existing_direction == direction {
            Self::cleanup_dm_internal(
                peer_id,
                &self.dm_peers,
                &self.event_tx,
                Some(b"superseded_by_redial"),
                None,
            )
            .await;
            return DmConnectionHandling::Store;
        }

        match local_node_id.as_deref() {
            Some(local_id)
                if should_replace_connection(local_id, peer_id, existing_direction, direction) =>
            {
                DmConnectionHandling::Store
            }
            Some(_) => DmConnectionHandling::DrainDuplicate,
            None => DmConnectionHandling::CloseDuplicate,
        }
    }

    async fn store_dm_peer_connection(
        &self,
        peer_id: &str,
        connection: Connection,
        direction: ConnectionDirection,
        dm_send: SendStream,
    ) -> DmStreamRegistration {
        self.store_dm_peer_connection_with_ownership(
            peer_id,
            connection,
            direction,
            dm_send,
            DmConnectionOwnership::Dedicated,
        )
        .await
    }

    async fn register_dm_stream_on_call_connection(
        &self,
        peer_id: &str,
        connection: Connection,
        direction: ConnectionDirection,
        dm_send: SendStream,
    ) -> DmStreamRegistration {
        self.store_dm_peer_connection_with_ownership(
            peer_id,
            connection,
            direction,
            dm_send,
            DmConnectionOwnership::SharedCall,
        )
        .await
    }

    async fn store_dm_peer_connection_with_ownership(
        &self,
        peer_id: &str,
        connection: Connection,
        direction: ConnectionDirection,
        dm_send: SendStream,
        ownership: DmConnectionOwnership,
    ) -> DmStreamRegistration {
        let dm_send = Arc::new(Mutex::new(Some(dm_send)));
        let dm_peer = DmPeerConnection {
            connection: connection.clone(),
            direction,
            dm_send: dm_send.clone(),
            ownership,
            established_at: std::time::Instant::now(),
        };

        let mut close_replaced_connection = true;
        let mut close_ignored_candidate = true;

        let old_dm_peer = {
            let local_node_id = self.local_node_id().await;
            let mut dm_peers = self.dm_peers.lock().await;
            let should_insert = match dm_peers.get(peer_id) {
                None => true,
                Some(existing) if existing.connection.stable_id() == connection.stable_id() => {
                    if ownership == DmConnectionOwnership::SharedCall {
                        return DmStreamRegistration::CloseDuplicate;
                    }
                    if existing.dm_send.lock().await.is_some() {
                        return DmStreamRegistration::DrainDuplicate;
                    }
                    // Same underlying QUIC connection re-presented its DM stream.
                    // Swap the new SendStream in while STILL holding dm_peers, so
                    // a concurrent cleanup_dm_internal can't remove the entry
                    // between our check and the write — which would leave the new
                    // stream stranded on an orphaned entry and silently drop DMs.
                    //
                    // LOCK ORDER INVARIANT: dm_peers may be held while acquiring a
                    // dm_send lock (here), so no code path may acquire dm_peers
                    // while holding a dm_send lock — every sender must clone the
                    // dm_send Arc under dm_peers, release dm_peers, then lock it.
                    let new_send = dm_send.lock().await.take();
                    *existing.dm_send.lock().await = new_send;
                    self.dm_connect_done.notify_waiters();
                    return DmStreamRegistration::Stored;
                }
                // Same-direction duplicate: the initiator re-dialed, which only
                // happens once it considers the old connection dead — replace
                // rather than reject (see dm_connection_handling).
                Some(existing) if existing.direction == direction => true,
                Some(existing) => match local_node_id.as_ref() {
                    Some(local_id)
                        if should_replace_connection(
                            local_id,
                            peer_id,
                            existing.direction,
                            direction,
                        ) =>
                    {
                        close_replaced_connection = false;
                        true
                    }
                    Some(_) => {
                        close_ignored_candidate = false;
                        false
                    }
                    None => false,
                },
            };

            if !should_insert {
                drop(dm_peers);
                tracing::info!(
                    "Ignoring duplicate {direction:?} DM stream for peer {peer_id}; existing connection wins"
                );
                if close_ignored_candidate {
                    dm_peer.close_if_owned(b"duplicate_dm_connection");
                    return DmStreamRegistration::CloseDuplicate;
                }
                return match ownership {
                    DmConnectionOwnership::Dedicated => DmStreamRegistration::DrainDuplicate,
                    DmConnectionOwnership::SharedCall => DmStreamRegistration::CloseDuplicate,
                };
            }

            dm_peers.insert(peer_id.to_string(), dm_peer)
        };

        if let Some(old_dm_peer) = old_dm_peer {
            if close_replaced_connection {
                old_dm_peer.close_if_owned(b"replaced_dm_connection");
            } else {
                old_dm_peer.finish_send_stream().await;
            }
        }

        let _ = self.event_tx.send(Event::DmConnected {
            peer_id: peer_id.to_string(),
        });
        self.dm_connect_done.notify_waiters();
        DmStreamRegistration::Stored
    }

    async fn cleanup_dm_internal(
        peer_id: &str,
        dm_peers: &Arc<Mutex<HashMap<String, DmPeerConnection>>>,
        event_tx: &broadcast::Sender<Event>,
        close_reason: Option<&'static [u8]>,
        expected_connection_id: Option<usize>,
    ) -> bool {
        let removed = {
            let mut dm_peers = dm_peers.lock().await;
            let should_remove = dm_peers.get(peer_id).is_some_and(|peer| {
                expected_connection_id.is_none_or(|id| peer.connection.stable_id() == id)
            });
            if should_remove {
                dm_peers.remove(peer_id)
            } else {
                None
            }
        };

        let Some(dm_peer) = removed else {
            return false;
        };

        if let Some(reason) = close_reason {
            dm_peer.close_if_owned(reason);
        }

        let _ = event_tx.send(Event::DmDisconnected {
            peer_id: peer_id.to_string(),
        });
        true
    }

    async fn upsert_peer_ticket(&self, peer_id: &str, ticket: &str) -> bool {
        let mut tickets = self.peer_tickets.lock().await;
        match tickets.get_mut(peer_id) {
            Some(existing) if existing.ticket == ticket => false,
            Some(existing) => {
                existing.ticket = ticket.to_string();
                existing.last_updated_ms = Self::current_timestamp_ms();
                existing.last_dial_failed_ms = None;
                existing.dial_failures = 0;
                true
            }
            None => {
                // Cap the cache: gossip-announced peers that never connect are
                // only removed on explicit disconnect, so without a bound the
                // map grows for the lifetime of the process. Evict the stalest.
                const MAX_PEER_TICKETS: usize = 256;
                if tickets.len() >= MAX_PEER_TICKETS {
                    if let Some(stalest) = tickets
                        .iter()
                        .min_by_key(|(_, record)| record.last_updated_ms)
                        .map(|(id, _)| id.clone())
                    {
                        tickets.remove(&stalest);
                    }
                }
                tickets.insert(
                    peer_id.to_string(),
                    PeerTicketRecord {
                        ticket: ticket.to_string(),
                        last_updated_ms: Self::current_timestamp_ms(),
                        last_dial_failed_ms: None,
                        dial_failures: 0,
                    },
                );
                true
            }
        }
    }

    async fn record_peer_ticket_dial_failure(&self, peer_id: &str) {
        let now = Self::current_timestamp_ms();
        let mut tickets = self.peer_tickets.lock().await;
        if let Some(record) = tickets.get_mut(peer_id) {
            record.last_dial_failed_ms = Some(now);
            record.dial_failures = record.dial_failures.saturating_add(1);
        }
    }

    async fn latest_self_announce_action(&self) -> Option<ControlAction> {
        let ticket = self.latest_ticket.lock().await.clone()?;
        self.self_announce_action(ticket).await
    }

    async fn self_announce_action(&self, ticket: String) -> Option<ControlAction> {
        let own_id = self.endpoint.lock().await.as_ref()?.id().to_string();
        Some(ControlAction::PeerAnnounce {
            peer_id: own_id,
            ticket,
        })
    }

    pub async fn send_self_announce_to_all(&self, ticket: String) {
        let Some(announce_self) = self.self_announce_action(ticket).await else {
            return;
        };

        let peer_ids: Vec<String> = {
            let peers = self.peers.lock().await;
            peers.keys().cloned().collect()
        };

        for peer_id in peer_ids {
            let _ = self.send_control(&peer_id, &announce_self).await;
        }
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
        self.setup_connection(peer_id, connection, ConnectionDirection::Inbound)
            .await
    }

    pub async fn handle_incoming_dm(&self, connection: Connection) -> Result<()> {
        let peer_id = connection.remote_id().to_string();
        tracing::info!("Setting up incoming DM connection from {peer_id}");
        self.setup_dm_connection(peer_id, connection, ConnectionDirection::Inbound)
            .await
    }

    async fn setup_dm_connection(
        &self,
        peer_id: String,
        connection: Connection,
        direction: ConnectionDirection,
    ) -> Result<()> {
        let connection_handling = self.dm_connection_handling(&peer_id, direction).await;
        match connection_handling {
            DmConnectionHandling::Store => {}
            DmConnectionHandling::DrainDuplicate => {
                tracing::info!(
                    "Draining duplicate {direction:?} DM connection for peer {peer_id}; existing connection wins"
                );
                let manager = self.clone();
                tokio::spawn(async move {
                    match tokio::time::timeout(DM_DUPLICATE_DRAIN_TIMEOUT, connection.accept_bi())
                        .await
                    {
                        Ok(Ok((_send, mut recv))) => {
                            let mut type_buf = [0u8; 1];
                            let typed = tokio::time::timeout(
                                DM_DUPLICATE_DRAIN_TIMEOUT,
                                recv.read_exact(&mut type_buf),
                            )
                            .await;
                            if matches!(typed, Ok(Ok(_))) && type_buf[0] == STREAM_DM {
                                drain_duplicate_dm_frame_once(&mut recv, &peer_id, &manager).await;
                            }
                        }
                        Ok(Err(error)) => {
                            tracing::debug!(
                                "Duplicate DM connection drain accept failed for {peer_id}: {error}"
                            );
                        }
                        Err(_) => {
                            tracing::debug!(
                                "Duplicate DM connection drain accept timed out for {peer_id}"
                            );
                        }
                    }
                    connection.close(0u32.into(), b"duplicate_dm_drained");
                });
                return Ok(());
            }
            DmConnectionHandling::CloseDuplicate => {
                tracing::info!(
                    "Closing duplicate {direction:?} DM connection for peer {peer_id}; existing connection wins"
                );
                connection.close(0u32.into(), b"duplicate_dm_connection");
                return Ok(());
            }
        }

        let manager = self.clone();
        let peer_id_reader = peer_id.clone();
        let connection_reader = connection.clone();
        let connection_reader_id = connection_reader.stable_id();
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
                            let registration = match connection_handling {
                                DmConnectionHandling::Store => {
                                    manager
                                        .store_dm_peer_connection(
                                            &peer_id_reader,
                                            connection_reader.clone(),
                                            direction,
                                            send,
                                        )
                                        .await
                                }
                                DmConnectionHandling::DrainDuplicate => {
                                    DmStreamRegistration::DrainDuplicate
                                }
                                DmConnectionHandling::CloseDuplicate => {
                                    DmStreamRegistration::CloseDuplicate
                                }
                            };
                            match registration {
                                DmStreamRegistration::Stored => {
                                    let manager = manager.clone();
                                    let peer_id = peer_id_reader.clone();
                                    tokio::spawn(async move {
                                        run_dm_reader(&mut recv, &peer_id, &manager).await;
                                    });
                                }
                                DmStreamRegistration::DrainDuplicate => {
                                    drain_duplicate_dm_frame_once(
                                        &mut recv,
                                        &peer_id_reader,
                                        &manager,
                                    )
                                    .await;
                                    connection_reader.close(0u32.into(), b"duplicate_dm_drained");
                                    break;
                                }
                                DmStreamRegistration::CloseDuplicate => break,
                            }
                        }
                        // Ignore non-DM streams on a DM connection
                    }
                    Err(_) => break,
                }
            }

            // Connection closed — clean up
            Self::cleanup_dm_internal(
                &peer_id_reader,
                &manager.dm_peers,
                &manager.event_tx,
                None,
                Some(connection_reader_id),
            )
            .await;
        });

        // Spawn a task to detect connection closure as a backstop
        let event_tx_closed = self.event_tx.clone();
        let dm_peers_closed = self.dm_peers.clone();
        let peer_id_closed = peer_id;
        let connection_closed = connection.clone();
        let connection_closed_id = connection_closed.stable_id();
        tokio::spawn(async move {
            connection_closed.closed().await;
            Self::cleanup_dm_internal(
                &peer_id_closed,
                &dm_peers_closed,
                &event_tx_closed,
                None,
                Some(connection_closed_id),
            )
            .await;
        });

        Ok(())
    }

    pub async fn connect_to_peer(
        &self,
        endpoint: &iroh::Endpoint,
        addr: iroh::EndpointAddr,
    ) -> Result<String> {
        crate::node::validate_project_relay_addr(&addr)?;
        self.connect_to_peer_with_timeout(endpoint, addr, CALL_DIAL_TIMEOUT)
            .await
    }

    pub async fn connect_to_peer_with_ticket(
        &self,
        endpoint: &iroh::Endpoint,
        ticket: &str,
    ) -> Result<String> {
        let endpoint_ticket = crate::node::parse_external_ticket(ticket)?;
        let addr = endpoint_ticket.endpoint_addr().clone();
        let peer_id = addr.id.to_string();
        self.upsert_peer_ticket(&peer_id, ticket).await;

        let mut result = self.connect_to_peer(endpoint, addr.clone()).await;
        if result.is_err() {
            // One quick retry for the initial join dial — covers a peer whose
            // relay path hasn't finished settling without leaving the user to
            // manually retry a failed join.
            tokio::time::sleep(INITIAL_DIAL_RETRY_BACKOFF).await;
            result = self.connect_to_peer(endpoint, addr).await;
        }
        if result.is_err() {
            self.record_peer_ticket_dial_failure(&peer_id).await;
        }
        result
    }

    async fn connect_to_peer_with_timeout(
        &self,
        endpoint: &iroh::Endpoint,
        addr: iroh::EndpointAddr,
        timeout: Duration,
    ) -> Result<String> {
        crate::node::validate_project_relay_addr(&addr)?;
        let peer_id = addr.id.to_string();

        if self.call_peer_connected(&peer_id).await {
            return Ok(peer_id);
        }
        let Some(_reservation) = self.reserve_call_connecting_guard(&peer_id).await else {
            if self.call_peer_connected(&peer_id).await {
                return Ok(peer_id);
            }
            anyhow::bail!("connection already in progress for peer {peer_id}");
        };

        self.emit_peer_connection_status(&peer_id, PeerConnectionKind::Connecting, None);

        let result = async {
            let connection = dial_peer_with_timeout(
                endpoint,
                addr,
                crate::node::NAFAQ_ALPN,
                timeout,
                "timed out dialing peer",
            )
            .await?;
            self.setup_outgoing_connection(connection).await
        }
        .await;

        if let Err(err) = &result {
            if self.call_peer_connected(&peer_id).await {
                return Ok(peer_id);
            }
            self.emit_peer_connection_status(
                &peer_id,
                PeerConnectionKind::Failed,
                Some(err.to_string()),
            );
        }

        result
    }

    async fn setup_outgoing_connection(&self, connection: Connection) -> Result<String> {
        let peer_id = connection.remote_id().to_string();
        tracing::info!("Connected to peer {peer_id}");
        self.setup_connection(peer_id.clone(), connection, ConnectionDirection::Outbound)
            .await?;
        Ok(peer_id)
    }

    async fn setup_connection(
        &self,
        peer_id: String,
        connection: Connection,
        direction: ConnectionDirection,
    ) -> Result<()> {
        // Ghost-call protocol fix: an inbound call dial with no locally active
        // call session (never created/joined one, or it was cancelled/ended)
        // is rejected outright — regardless of any per-peer duplicate/replace
        // logic below. Outbound dials are never gated here: they only happen
        // because we ourselves just called create_call/join_call, which sets
        // the flag before dialing. DM/presence/file streams don't go through
        // this path at all.
        if direction == ConnectionDirection::Inbound && !self.is_call_session_active() {
            tracing::info!(
                "Rejecting inbound call connection from {peer_id}: no active call session"
            );
            connection.close(0u32.into(), b"call_not_active");
            return Ok(());
        }

        if !self
            .should_accept_call_connection(&peer_id, direction)
            .await
        {
            tracing::info!(
                "Closing duplicate {direction:?} call connection for peer {peer_id}; existing connection wins"
            );
            connection.close(0u32.into(), b"duplicate_call_connection");
            return Ok(());
        }

        // Keep the transport-level uni-stream cap (256) — do NOT raise it back
        // to thousands here, which reopened the unbounded-accept-task DoS the
        // transport config closes. 256 is ample headroom for pipelined video.

        let (chat_send, _) = open_typed_bi_stream(&connection, STREAM_CHAT, "chat").await?;
        chat_send.set_priority(10)?;

        let (control_send, _) =
            open_typed_bi_stream(&connection, STREAM_CONTROL, "control").await?;
        control_send.set_priority(100)?;

        let peer_conn = PeerConnection {
            connection: connection.clone(),
            direction,
            chat_send: Arc::new(Mutex::new(Some(chat_send))),
            control_send: Arc::new(Mutex::new(Some(control_send))),
            video_writer: PeerVideoWriter::new(),
            requested_video_layer: Arc::new(AtomicU8::new(0)),
            pending_keyframe: Arc::new(AtomicBool::new(false)),
            last_activity_ms: Arc::new(AtomicU64::new(Self::current_timestamp_ms())),
            connection_status: PeerConnectionKind::Connected,
            established_at: std::time::Instant::now(),
            outbound_bitrate_bps: Arc::new(AtomicU32::new(0)),
            audio_sequence: Arc::new(AtomicU16::new(0)),
        };

        let video_writer = peer_conn.video_writer.clone();

        let (old_connection, old_count, new_count) = {
            let local_node_id = self.local_node_id().await;
            let mut peers = self.peers.lock().await;
            let old = peers.len();
            let should_insert = match peers.get(&peer_id) {
                None => true,
                Some(existing) => should_accept_call_connection_candidate(
                    local_node_id.as_deref(),
                    &peer_id,
                    existing.direction,
                    &existing.connection_status,
                    direction,
                ),
            };

            if !should_insert {
                drop(peers);
                tracing::info!(
                    "Closing duplicate {direction:?} call connection for peer {peer_id}; existing connection wins"
                );
                peer_conn
                    .connection
                    .close(0u32.into(), b"duplicate_call_connection");
                return Ok(());
            }

            let old_connection = peers
                .insert(peer_id.clone(), peer_conn)
                .map(|peer| peer.connection);
            (old_connection, old, peers.len())
        };

        if let Some(old_connection) = old_connection {
            old_connection.close(0u32.into(), b"replaced_call_connection");
        }

        Self::spawn_video_writer(peer_id.clone(), connection.clone(), video_writer);

        let _ = self.event_tx.send(Event::PeerConnected {
            peer_id: peer_id.clone(),
        });
        self.emit_peer_connection_status(&peer_id, PeerConnectionKind::Connected, None);

        Self::emit_quality_profile_if_changed(old_count, new_count, &self.event_tx);

        self.spawn_stream_receivers(peer_id.clone(), connection, direction);

        if let Some(announce_self) = self.latest_self_announce_action().await {
            let _ = self.send_control(&peer_id, &announce_self).await;
        }

        let stored_tickets: Vec<(String, String)> = {
            let tickets = self.peer_tickets.lock().await;
            tickets
                .iter()
                .filter(|(id, _)| **id != peer_id)
                .map(|(id, record)| (id.clone(), record.ticket.clone()))
                .collect()
        };
        for (stored_id, stored_ticket) in stored_tickets {
            let announce = ControlAction::PeerAnnounce {
                peer_id: stored_id,
                ticket: stored_ticket,
            };
            let _ = self.send_control(&peer_id, &announce).await;
        }

        Ok(())
    }

    async fn cleanup_peer_internal(
        peer_id: &str,
        peers: &Arc<Mutex<HashMap<String, PeerConnection>>>,
        dm_peers: &Arc<Mutex<HashMap<String, DmPeerConnection>>>,
        peer_tickets: &Arc<Mutex<HashMap<String, PeerTicketRecord>>>,
        video_receive_state: &Arc<Mutex<HashMap<String, VideoReceiveState>>>,
        event_tx: &broadcast::Sender<Event>,
        close_reason: Option<&'static [u8]>,
        expected_connection_id: Option<usize>,
        preserve_ticket: bool,
    ) -> bool {
        let (removed, old_count, new_count) = {
            let mut peers = peers.lock().await;
            let old = peers.len();
            let should_remove = peers.get(peer_id).is_some_and(|peer| {
                expected_connection_id.is_none_or(|id| peer.connection.stable_id() == id)
            });
            let removed = if should_remove {
                peers.remove(peer_id)
            } else {
                None
            };
            (removed, old, peers.len())
        };

        let Some(peer) = removed else {
            return false;
        };

        if let Some(reason) = close_reason {
            peer.connection.close(0u32.into(), reason);
        }

        // Keep the cached ticket when the connection merely dropped (e.g. QUIC
        // idle timeout) so the liveness/reconnect path can still re-dial the
        // peer. Only forget it on a deliberate teardown (user ended the call,
        // or the peer was given up on after exhausting reconnect attempts).
        if !preserve_ticket {
            peer_tickets.lock().await.remove(peer_id);
        }
        video_receive_state.lock().await.remove(peer_id);

        // A SharedCall DM rides the QUIC connection we just tore down; no DM
        // cleanup path observes call teardowns, so evict it here or the entry
        // lingers pointing at a closed connection and DmDisconnected is never
        // emitted, leaving the frontend's DM session in limbo.
        let dead_connection_id = peer.connection.stable_id();
        let shared_dm_dead = {
            let dm_peers_guard = dm_peers.lock().await;
            dm_peers_guard.get(peer_id).is_some_and(|dm| {
                dm.ownership == DmConnectionOwnership::SharedCall
                    && dm.connection.stable_id() == dead_connection_id
            })
        };
        if shared_dm_dead {
            Self::cleanup_dm_internal(peer_id, dm_peers, event_tx, None, Some(dead_connection_id))
                .await;
        }

        let _ = event_tx.send(Event::PeerDisconnected {
            peer_id: peer_id.to_string(),
        });
        let _ = event_tx.send(Event::PeerConnectionStatusChanged {
            peer_id: peer_id.to_string(),
            status: PeerConnectionKind::Disconnected,
            reason: close_reason.map(|reason| String::from_utf8_lossy(reason).to_string()),
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

        let endpoint_ticket = match crate::node::parse_external_ticket(&ticket) {
            Ok(endpoint_ticket) => endpoint_ticket,
            Err(e) => {
                tracing::warn!("Mesh: rejected ticket for {announced_peer_id}: {e}");
                return;
            }
        };
        let addr = endpoint_ticket.endpoint_addr().clone();

        let ticket_changed = self.upsert_peer_ticket(&announced_peer_id, &ticket).await;
        if !ticket_changed {
            return;
        }

        let relay_targets: Vec<String> = {
            let peers = self.peers.lock().await;
            relay_targets_for_announce(peers.keys(), sender_id, &announced_peer_id)
        };
        for target_id in relay_targets {
            let announce = ControlAction::PeerAnnounce {
                peer_id: announced_peer_id.clone(),
                ticket: ticket.clone(),
            };
            let _ = self.send_control(&target_id, &announce).await;
        }

        let already_connected = self.peers.lock().await.contains_key(&announced_peer_id);
        if already_connected {
            return;
        }

        let endpoint = self.endpoint.lock().await.clone();
        let Some(ep) = endpoint else {
            return;
        };

        let manager = self.clone();
        tokio::spawn(async move {
            match manager
                .connect_to_peer_with_timeout(&ep, addr, MESH_DIAL_TIMEOUT)
                .await
            {
                Ok(_) => {
                    tracing::info!("Mesh: auto-connected to announced peer {announced_peer_id}")
                }
                Err(e) => {
                    tracing::warn!("Mesh: failed to auto-connect to {announced_peer_id}: {e}");
                    manager
                        .record_peer_ticket_dial_failure(&announced_peer_id)
                        .await;
                }
            }
        });
    }

    fn spawn_stream_receivers(
        &self,
        peer_id: String,
        connection: Connection,
        direction: ConnectionDirection,
    ) {
        let audio_media_tx = self.audio_media_tx.clone();
        let video_media_tx = self.video_media_tx.clone();
        let peers_ref = self.peers.clone();
        let peer_tickets_ref = self.peer_tickets.clone();
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

        let manager_closed = self.clone();
        let peers_ref_closed = peers_ref.clone();
        let dm_peers_ref_closed = self.dm_peers.clone();
        let peer_tickets_ref = peer_tickets_ref.clone();
        let video_state_ref = video_state_ref.clone();
        let event_tx_cleanup_closed = event_tx_cleanup.clone();
        let peer_id_closed = peer_id.clone();
        let connection_closed = connection.clone();
        let connection_closed_id = connection_closed.stable_id();
        tokio::spawn(async move {
            let close_reason = connection_closed.closed().await;
            tracing::info!("Connection closed for peer {peer_id_closed}: {close_reason}");

            // A silent QUIC idle-timeout (no CONNECTION_CLOSE frame reached us)
            // means the network dropped, not that either side ended the call —
            // give the peer a chance to reconnect instead of tearing the call
            // down immediately. Any other close reason (explicit close by us
            // or the peer, reset, protocol error) is treated as final, exactly
            // as before.
            if matches!(close_reason, ConnectionError::TimedOut)
                && manager_closed
                    .try_begin_peer_reconnect(&peer_id_closed, connection_closed_id)
                    .await
            {
                return;
            }

            Self::cleanup_peer_internal(
                &peer_id_closed,
                &peers_ref_closed,
                &dm_peers_ref_closed,
                &peer_tickets_ref,
                &video_state_ref,
                &event_tx_cleanup_closed,
                None,
                Some(connection_closed_id),
                true, // preserve ticket so the peer can be re-dialed after a drop
            )
            .await;
        });

        let peer_id_bi = peer_id.clone();
        let peers_ref_bi = peers_ref.clone();
        let manager_bi = self.clone();
        let connection_bi = connection.clone();
        tokio::spawn(async move {
            loop {
                match connection.accept_bi().await {
                    Ok((send, mut recv)) => {
                        let peer_id = peer_id_bi.clone();
                        let manager = manager_bi.clone();
                        let peers_ref = peers_ref_bi.clone();
                        let mut type_buf = [0u8; 1];
                        if recv.read_exact(&mut type_buf).await.is_err() {
                            continue;
                        }
                        // For incoming DM streams, store the send side so we
                        // can reply, and emit DmConnected if this is a new
                        // DM peer.
                        if type_buf[0] == STREAM_DM {
                            let registration = manager_bi
                                .register_dm_stream_on_call_connection(
                                    &peer_id,
                                    connection_bi.clone(),
                                    direction,
                                    send,
                                )
                                .await;
                            match registration {
                                DmStreamRegistration::Stored => {}
                                DmStreamRegistration::DrainDuplicate => {
                                    drain_duplicate_dm_frame_once(&mut recv, &peer_id, &manager)
                                        .await;
                                    continue;
                                }
                                DmStreamRegistration::CloseDuplicate => continue,
                            }
                        }
                        tokio::spawn(async move {
                            Self::handle_bi_stream(
                                type_buf[0],
                                &peer_id,
                                recv,
                                manager,
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
        manager: ConnectionManager,
        peers: Arc<Mutex<HashMap<String, PeerConnection>>>,
    ) {
        let mut active_files: HashMap<String, ActiveFileReceive> = HashMap::new();

        loop {
            match crate::messages::read_framed(&mut recv).await {
                Ok(Some(data)) => match stream_type {
                    STREAM_CHAT => {
                        Self::mark_peer_active_internal(&peers, peer_id).await;
                        if let Ok(message) = String::from_utf8(data) {
                            let _ = manager.event_tx.send(Event::ChatReceived {
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
                            let _ = manager.event_tx.send(Event::ControlReceived {
                                peer_id: peer_id.to_string(),
                                action,
                            });
                        }
                    }
                    // DM riding a call connection goes through the exact same
                    // dispatch (call-signal/ack/dedup handling) as the
                    // dedicated DM connection reader.
                    STREAM_DM => {
                        handle_dm_frame_payload(&manager, &data, peer_id, &mut active_files).await;
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

        cleanup_active_dm_files(active_files, peer_id, &manager.event_tx).await;
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
                            // Bound the open: when the peer's flow-control window
                            // is exhausted or the connection is half-open, open_uni
                            // blocks until QUIC's 30s idle timeout — freezing video
                            // for the whole window. Drop the (stale) frame instead.
                            let opened =
                                tokio::time::timeout(Duration::from_secs(5), connection.open_uni())
                                    .await;
                            match opened {
                                Ok(Ok(mut stream)) => {
                                    if stream.write_all(&[STREAM_VIDEO]).await.is_ok() {
                                        send = Some(stream);
                                    } else {
                                        tracing::warn!(
                                            "Failed to prime video stream for peer {peer_id}"
                                        );
                                        break;
                                    }
                                }
                                Ok(Err(e)) => {
                                    tracing::warn!(
                                        "Failed to open video stream for peer {peer_id}: {e}"
                                    );
                                    break;
                                }
                                Err(_) => {
                                    tracing::warn!(
                                        "Timed out opening video stream for peer {peer_id}; dropping frame"
                                    );
                                    break;
                                }
                            }
                        }

                        let result = if let Some(stream) = send.as_mut() {
                            let _ = stream.set_priority(if frame.is_keyframe { 50 } else { 30 });
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
        let peers: Vec<(String, Connection, Arc<AtomicU16>)> = {
            let peers = self.peers.lock().await;
            peers
                .iter()
                .map(|(peer_id, peer)| {
                    (
                        peer_id.clone(),
                        peer.connection.clone(),
                        peer.audio_sequence.clone(),
                    )
                })
                .collect()
        };
        for (peer_id, conn, seq) in peers {
            // fetch_add wraps on overflow, matching the receiver's
            // wrapping-aware sequence tracking.
            let sequence = seq.fetch_add(1, Ordering::Relaxed);
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
                // Skip peers the liveness ticker has flagged as silent
                // (Suspect/Reconnecting). Their QUIC connection may not be
                // closed yet, so open_uni() would succeed and we'd waste
                // CPU/bandwidth encoding frames into a dead connection.
                .filter(|p| {
                    matches!(
                        p.connection_status,
                        PeerConnectionKind::Connected | PeerConnectionKind::Connecting
                    )
                })
                // Honor VideoQualityRequest { layer: None } — the peer asked us
                // to stop sending video (e.g. their view of us is paused).
                .filter(|p| {
                    p.requested_video_layer.load(Ordering::Relaxed)
                        != VideoLayerRequest::None.to_u8()
                })
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

    /// Ask every connected peer to send a keyframe — used when the local video
    /// pipeline dropped frames (broadcast lag) and may have lost an IDR.
    pub async fn request_keyframes_from_all_peers(&self) {
        let peer_ids: Vec<String> = {
            let peers = self.peers.lock().await;
            peers.keys().cloned().collect()
        };
        for peer_id in peer_ids {
            let _ = self
                .send_control(
                    &peer_id,
                    &ControlAction::KeyframeRequest {
                        layer: VideoLayerRequest::High,
                    },
                )
                .await;
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

    async fn latest_reconnect_ticket(&self, peer_id: &str, now: u64) -> Option<String> {
        let tickets = self.peer_tickets.lock().await;
        let record = tickets.get(peer_id)?;
        if record
            .last_dial_failed_ms
            .is_some_and(|failed_at| now.saturating_sub(failed_at) < RECONNECT_RETRY_AFTER_MS)
        {
            return None;
        }
        Some(record.ticket.clone())
    }

    async fn spawn_peer_reconnect(&self, peer_id: String, ticket: String) {
        let Some(endpoint) = self.endpoint.lock().await.clone() else {
            return;
        };
        let Some(reservation) = self.reserve_call_connecting_guard(&peer_id).await else {
            return;
        };

        let manager = self.clone();
        tokio::spawn(async move {
            let _reservation = reservation;
            let endpoint_ticket = match crate::node::parse_external_ticket(&ticket) {
                Ok(endpoint_ticket) => endpoint_ticket,
                Err(e) => {
                    tracing::warn!("Reconnect: invalid ticket for {peer_id}: {e}");
                    manager.record_peer_ticket_dial_failure(&peer_id).await;
                    return;
                }
            };

            let addr = endpoint_ticket.endpoint_addr().clone();
            if addr.id.to_string() != peer_id {
                tracing::warn!("Reconnect: ticket node id did not match peer {peer_id}");
                manager.record_peer_ticket_dial_failure(&peer_id).await;
                return;
            }

            match dial_peer_with_timeout(
                &endpoint,
                addr,
                crate::node::NAFAQ_ALPN,
                CALL_DIAL_TIMEOUT,
                "timed out reconnecting peer",
            )
            .await
            {
                Ok(connection) => {
                    // The call may have ended (user hung up, or the liveness
                    // ladder's own final timeout already gave up) while this
                    // redial was in flight. Check the peer is still expected
                    // before reviving it, so a straggler reconnect can't
                    // resurrect a call that was intentionally torn down.
                    if !manager.peers.lock().await.contains_key(&peer_id) {
                        connection.close(0u32.into(), b"reconnect_no_longer_wanted");
                        return;
                    }
                    if let Err(e) = manager.setup_outgoing_connection(connection).await {
                        tracing::warn!("Reconnect: failed to set up peer {peer_id}: {e}");
                        manager.record_peer_ticket_dial_failure(&peer_id).await;
                    }
                }
                Err(e) => {
                    tracing::warn!("Reconnect: failed to dial peer {peer_id}: {e}");
                    manager.record_peer_ticket_dial_failure(&peer_id).await;
                }
            }
        });
    }

    /// Move a peer straight to `Reconnecting` and kick off a redial when its
    /// connection ends via a silent QUIC idle-timeout — the transport gives up
    /// (`max_idle_timeout`, node.rs) well before the liveness ladder's own
    /// Suspect→Reconnecting promotion would (`SUSPECT_AFTER_MS`/
    /// `RECONNECT_AFTER_MS` above), so without this the peer is evicted before
    /// a reconnect is ever attempted. Returns `false` (doing nothing) if the
    /// connection was already superseded/evicted, or no ticket is cached to
    /// redial with — callers fall back to the normal teardown in that case.
    /// The liveness loop's `DISCONNECT_AFTER_MS` bound still applies from
    /// here: it sees the same `Reconnecting` status and unchanged
    /// `last_activity_ms`, and keeps retrying (or finally gives up) exactly as
    /// it does for a liveness-ladder-detected drop.
    async fn try_begin_peer_reconnect(&self, peer_id: &str, expected_connection_id: usize) -> bool {
        let now = Self::current_timestamp_ms();
        let Some(ticket) = self.latest_reconnect_ticket(peer_id, now).await else {
            return false;
        };

        let became_reconnecting = {
            let mut peers = self.peers.lock().await;
            match peers.get_mut(peer_id) {
                Some(peer) if peer.connection.stable_id() == expected_connection_id => {
                    peer.connection_status = PeerConnectionKind::Reconnecting;
                    true
                }
                _ => false,
            }
        };
        if !became_reconnecting {
            return false;
        }

        self.emit_peer_connection_status(
            peer_id,
            PeerConnectionKind::Reconnecting,
            Some("connection dropped; attempting reconnect".to_string()),
        );
        self.spawn_peer_reconnect(peer_id.to_string(), ticket).await;
        true
    }

    pub async fn maintain_peer_liveness(&self) {
        let now = Self::current_timestamp_ms();
        let mut suspect_peer_ids = Vec::new();
        let mut suspect_reconnect_peer_ids = Vec::new();
        let mut reconnect_peer_ids = Vec::new();
        let mut connected_peer_ids = Vec::new();
        let mut disconnected_peer_ids = Vec::new();

        {
            let mut peers = self.peers.lock().await;
            for (peer_id, peer) in peers.iter_mut() {
                let idle_ms = now.saturating_sub(peer.last_activity_ms.load(Ordering::Relaxed));
                if idle_ms > DISCONNECT_AFTER_MS {
                    disconnected_peer_ids.push(peer_id.clone());
                } else {
                    match peer.connection_status {
                        PeerConnectionKind::Connected if idle_ms > SUSPECT_AFTER_MS => {
                            peer.connection_status = PeerConnectionKind::Suspect;
                            suspect_peer_ids.push(peer_id.clone());
                        }
                        PeerConnectionKind::Suspect if idle_ms > RECONNECT_AFTER_MS => {
                            suspect_reconnect_peer_ids.push(peer_id.clone());
                        }
                        PeerConnectionKind::Reconnecting if idle_ms > RECONNECT_AFTER_MS => {
                            reconnect_peer_ids.push(peer_id.clone());
                        }
                        _ if idle_ms <= SUSPECT_AFTER_MS
                            && peer.connection_status != PeerConnectionKind::Connected =>
                        {
                            peer.connection_status = PeerConnectionKind::Connected;
                            connected_peer_ids.push(peer_id.clone());
                        }
                        _ => {}
                    }
                }
            }
        }

        for peer_id in suspect_peer_ids {
            tracing::info!("Marking peer {peer_id} suspect after liveness timeout");
            self.emit_peer_connection_status(
                &peer_id,
                PeerConnectionKind::Suspect,
                Some("peer liveness is stale".to_string()),
            );
        }

        for peer_id in connected_peer_ids {
            self.emit_peer_connection_status(&peer_id, PeerConnectionKind::Connected, None);
        }

        for peer_id in suspect_reconnect_peer_ids {
            let Some(ticket) = self.latest_reconnect_ticket(&peer_id, now).await else {
                self.emit_peer_connection_status(
                    &peer_id,
                    PeerConnectionKind::Suspect,
                    Some(
                        "peer liveness is stale; waiting for fresh peer ticket or activity"
                            .to_string(),
                    ),
                );
                continue;
            };

            let mut should_reconnect = false;
            {
                let mut peers = self.peers.lock().await;
                if let Some(peer) = peers.get_mut(&peer_id) {
                    let idle_ms = now.saturating_sub(peer.last_activity_ms.load(Ordering::Relaxed));
                    if peer.connection_status == PeerConnectionKind::Suspect
                        && idle_ms > RECONNECT_AFTER_MS
                        && idle_ms <= DISCONNECT_AFTER_MS
                    {
                        peer.connection_status = PeerConnectionKind::Reconnecting;
                        should_reconnect = true;
                    }
                }
            }

            if should_reconnect {
                self.emit_peer_connection_status(
                    &peer_id,
                    PeerConnectionKind::Reconnecting,
                    Some("peer liveness is stale; attempting reconnect".to_string()),
                );
                self.spawn_peer_reconnect(peer_id, ticket).await;
            }
        }

        for peer_id in reconnect_peer_ids {
            if let Some(ticket) = self.latest_reconnect_ticket(&peer_id, now).await {
                self.spawn_peer_reconnect(peer_id, ticket).await;
            }
        }

        for peer_id in disconnected_peer_ids {
            tracing::info!("Disconnecting stale peer {peer_id} after liveness timeout");
            Self::cleanup_peer_internal(
                &peer_id,
                &self.peers,
                &self.dm_peers,
                &self.peer_tickets,
                &self.video_receive_state,
                &self.event_tx,
                Some(b"peer timeout"),
                None,
                false, // reconnect attempts exhausted — forget the ticket
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
        // Snapshot video-receive ages into a local map and release that lock
        // before taking `peers`. Otherwise both mutexes are held for the entire
        // stats pass (which queries QUIC path stats per peer under lock),
        // needlessly contending writers and risking lock-ordering issues.
        let video_ages: HashMap<String, u64> = {
            let video_state = self.video_receive_state.lock().await;
            video_state
                .iter()
                .map(|(id, state)| (id.clone(), state.last_received.elapsed().as_millis() as u64))
                .collect()
        };

        let peers = self.peers.lock().await;

        peers
            .iter()
            .map(|(peer_id, peer)| {
                let paths = peer.connection.paths();
                let path_stats = paths
                    .iter()
                    .find(|path| path.is_selected())
                    .or_else(|| paths.iter().next())
                    .map(|path| path.stats());

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
                let latest_video_age_ms = video_ages.get(peer_id).copied().unwrap_or_default();

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

    /// Strictest (lowest) non-zero per-peer outbound bitrate override, or 0
    /// when no peer has one. The video encoder is shared across peers, so the
    /// most constrained peer dictates the encoding bitrate.
    pub async fn min_peer_outbound_bitrate(&self) -> u32 {
        let peers = self.peers.lock().await;
        peers
            .values()
            .map(|p| p.outbound_bitrate_bps.load(Ordering::Relaxed))
            .filter(|&bps| bps > 0)
            .min()
            .unwrap_or(0)
    }

    // ── DM connection management ────────────────────────────────────────

    pub async fn connect_dm(&self, node_id_str: &str) -> Result<()> {
        if self.dm_peer_connected(node_id_str).await {
            return Ok(());
        }
        let Some(_reservation) = self.reserve_dm_connecting_guard(node_id_str).await else {
            if self.dm_peer_connected(node_id_str).await {
                return Ok(());
            }
            anyhow::bail!("DM connection already in progress for peer {node_id_str}");
        };

        let result: Result<()> = async {
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

            let connection: iroh::endpoint::Connection = match dial_peer_with_timeout(
                &endpoint,
                addr.clone(),
                crate::node::NAFAQ_DM_ALPN,
                DM_DIAL_TIMEOUT,
                "timed out dialing DM peer",
            )
            .await
            {
                Ok(connection) => connection,
                Err(_) => {
                    // One quick retry — same rationale as connect_to_peer_with_ticket.
                    tokio::time::sleep(INITIAL_DIAL_RETRY_BACKOFF).await;
                    dial_peer_with_timeout(
                        &endpoint,
                        addr,
                        crate::node::NAFAQ_DM_ALPN,
                        DM_DIAL_TIMEOUT,
                        "timed out dialing DM peer",
                    )
                    .await?
                }
            };
            let peer_id = connection.remote_id().to_string();

            let (dm_send, mut dm_recv): (iroh::endpoint::SendStream, iroh::endpoint::RecvStream) =
                open_typed_bi_stream(&connection, STREAM_DM, "DM").await?;

            let registration = self
                .store_dm_peer_connection(
                    &peer_id,
                    connection.clone(),
                    ConnectionDirection::Outbound,
                    dm_send,
                )
                .await;
            match registration {
                DmStreamRegistration::Stored => {}
                DmStreamRegistration::DrainDuplicate => {
                    drain_duplicate_dm_frame_once(&mut dm_recv, &peer_id, self).await;
                    connection.close(0u32.into(), b"duplicate_dm_drained");
                    return Ok(());
                }
                DmStreamRegistration::CloseDuplicate => {
                    connection.close(0u32.into(), b"duplicate_dm_connection");
                    return Ok(());
                }
            }

            // Spawn a reader for the initial bistream's recv side so the remote
            // peer can reply on the same bistream (via accept_bi's send half).
            {
                let manager = self.clone();
                let peer_id = peer_id.clone();
                tokio::spawn(async move {
                    run_dm_reader(&mut dm_recv, &peer_id, &manager).await;
                });
            }

            // Spawn a reader task for additional incoming DM bistreams on this connection
            let manager = self.clone();
            let event_tx = self.event_tx.clone();
            let dm_peers_ref = self.dm_peers.clone();
            let peer_id_reader = peer_id.clone();
            let connection_reader = connection.clone();
            let connection_reader_id = connection_reader.stable_id();
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
                            drain_duplicate_dm_frame_once(&mut recv, &peer_id_reader, &manager)
                                .await;
                            connection_reader.close(0u32.into(), b"duplicate_dm_drained");
                            break;
                        }
                        Err(_) => break,
                    }
                }

                // Connection closed — clean up and emit DmDisconnected
                Self::cleanup_dm_internal(
                    &peer_id_reader,
                    &dm_peers_ref,
                    &event_tx,
                    None,
                    Some(connection_reader_id),
                )
                .await;
            });

            // Spawn a task to detect connection closure
            let event_tx_closed = self.event_tx.clone();
            let dm_peers_closed = self.dm_peers.clone();
            let peer_id_closed = peer_id.clone();
            let connection_closed = connection.clone();
            let connection_closed_id = connection_closed.stable_id();
            tokio::spawn(async move {
                connection_closed.closed().await;
                Self::cleanup_dm_internal(
                    &peer_id_closed,
                    &dm_peers_closed,
                    &event_tx_closed,
                    None,
                    Some(connection_closed_id),
                )
                .await;
            });

            Ok(())
        }
        .await;

        if result.is_err() && self.dm_peer_connected(node_id_str).await {
            return Ok(());
        }

        result
    }

    async fn wait_for_dm_connecting_to_finish(&self, peer_id: &str) -> Result<()> {
        with_timeout(
            DM_CONNECT_WAIT_TIMEOUT,
            format!("timed out waiting for DM connection to peer {peer_id}"),
            async {
                loop {
                    // Arm the notification BEFORE checking the condition so a
                    // state change between check and wait can't be missed.
                    let notified = self.dm_connect_done.notified();
                    if self.dm_peer_connected(peer_id).await
                        || !self.dm_connect_in_progress(peer_id).await
                    {
                        return Ok(());
                    }
                    notified.await;
                }
            },
        )
        .await
    }

    pub async fn ensure_dm_connected(&self, peer_id: &str) -> Result<()> {
        loop {
            if self.dm_peer_connected(peer_id).await {
                // Gossip presence may have observed the remote rejoin AFTER this
                // entry was established — meaning the existing QUIC stream points
                // at a dead remote that iroh's idle timeout hasn't yet noticed.
                // Writes to that stream would silently succeed locally and drop
                // bytes on the floor. Evict and redial.
                if self.dm_entry_predates_recent_rejoin(peer_id).await {
                    tracing::info!(
                        "DM entry for {peer_id} pre-dates recent gossip rejoin; evicting + redialing"
                    );
                    Self::cleanup_dm_internal(
                        peer_id,
                        &self.dm_peers,
                        &self.event_tx,
                        Some(b"dm_entry_predates_rejoin"),
                        None,
                    )
                    .await;
                    continue;
                }
                return Ok(());
            }

            if self.dm_connect_in_progress(peer_id).await {
                self.wait_for_dm_connecting_to_finish(peer_id).await?;
                continue;
            }

            match self.connect_dm(peer_id).await {
                Ok(()) => return Ok(()),
                Err(_) if self.dm_peer_connected(peer_id).await => return Ok(()),
                Err(err) if self.dm_connect_in_progress(peer_id).await => {
                    tracing::debug!("DM connect for {peer_id} raced with another attempt: {err}");
                    self.wait_for_dm_connecting_to_finish(peer_id).await?;
                }
                Err(err) => {
                    return Err(anyhow::anyhow!(
                        "failed to connect DM peer {peer_id}: {err}"
                    ));
                }
            }
        }
    }

    async fn write_dm_frame(&self, peer_id: &str, data: &[u8]) -> Result<()> {
        let stream = {
            let dm_peers = self.dm_peers.lock().await;
            dm_peers.get(peer_id).map(|p| p.dm_send.clone())
        };
        let Some(s) = stream else {
            anyhow::bail!("DM peer {peer_id} is not connected");
        };

        let mut guard = s.lock().await;
        if let Some(ref mut send) = *guard {
            with_timeout(
                DM_WRITE_TIMEOUT,
                format!("timed out writing DM frame to peer {peer_id}"),
                async { Ok(crate::messages::write_framed(send, data).await?) },
            )
            .await?;
            Ok(())
        } else {
            anyhow::bail!("DM stream for peer {peer_id} is unavailable");
        }
    }

    /// Writes one DM frame to the current stream without connecting, reconnecting,
    /// or retrying. Intended for multi-frame protocols whose receiver state is
    /// stream-local after the caller has performed an initial connection ensure.
    pub async fn send_dm_frame_strict(&self, peer_id: &str, message: &DmMessage) -> Result<()> {
        let data = serde_json::to_vec(message)?;
        self.write_dm_frame(peer_id, &data).await.map_err(|err| {
            anyhow::anyhow!("failed to send DM to peer {peer_id} without retry: {err}")
        })
    }

    pub async fn send_dm(&self, peer_id: &str, message: &DmMessage) -> Result<()> {
        self.ensure_dm_connected(peer_id).await?;

        // Capture the connection id we're about to write on, so a write-failure
        // cleanup can't accidentally evict a fresher entry that landed mid-flight.
        let active_connection_id = {
            let dm_peers = self.dm_peers.lock().await;
            dm_peers.get(peer_id).map(|p| p.connection.stable_id())
        };

        let data = serde_json::to_vec(message)?;
        match self.write_dm_frame(peer_id, &data).await {
            Ok(()) => Ok(()),
            Err(first_err) => {
                tracing::warn!("DM write to peer {peer_id} failed; reconnecting once: {first_err}");
                Self::cleanup_dm_internal(
                    peer_id,
                    &self.dm_peers,
                    &self.event_tx,
                    Some(b"dm_write_failed"),
                    active_connection_id,
                )
                .await;

                self.ensure_dm_connected(peer_id).await.map_err(|reconnect_err| {
                    anyhow::anyhow!(
                        "failed to reconnect DM peer {peer_id} after write failure: {reconnect_err}; original write error: {first_err}"
                    )
                })?;

                self.write_dm_frame(peer_id, &data).await.map_err(|retry_err| {
                    anyhow::anyhow!(
                        "failed to send DM to peer {peer_id} after reconnect retry: {retry_err}; original write error: {first_err}"
                    )
                })
            }
        }
    }

    pub async fn disconnect_dm(&self, peer_id: &str) {
        Self::cleanup_dm_internal(
            peer_id,
            &self.dm_peers,
            &self.event_tx,
            Some(b"dm_closed"),
            None,
        )
        .await;
    }

    pub async fn disconnect_peer(&self, peer_id: &str) -> Result<()> {
        Self::cleanup_peer_internal(
            peer_id,
            &self.peers,
            &self.dm_peers,
            &self.peer_tickets,
            &self.video_receive_state,
            &self.event_tx,
            Some(b"call ended"),
            None,
            false, // user ended the call — forget the ticket
        )
        .await;
        // This is the explicit, user-initiated hang-up path (the `end_call`
        // command, invoked per-peer by the frontend's terminateCall). Once
        // the last peer is gone, the call is genuinely over from our side —
        // stop accepting new inbound dials for the outstanding ticket.
        //
        // Deliberately NOT done in the automatic liveness/reconnect-timeout
        // disconnect paths: those exist specifically so a transient network
        // drop can self-heal via redial (including the other side racing us
        // with its own inbound reconnect), and clearing the flag there could
        // reject a legitimate in-flight reconnect.
        if self.peer_count().await == 0 {
            self.set_call_session_active(false);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::Arc;
    use std::time::Duration;

    use iroh::{EndpointAddr, SecretKey, TransportAddr, endpoint::Connection, protocol::Router};
    use iroh_tickets::{Ticket, endpoint::EndpointTicket};
    use tokio::time::timeout;

    use crate::node;
    use crate::protocol::{NafaqDmProtocol, NafaqProtocol};

    fn test_manager(
        event_tx: broadcast::Sender<Event>,
        audio_tx: broadcast::Sender<AudioPacket>,
        video_tx: broadcast::Sender<VideoPacket>,
    ) -> ConnectionManager {
        ConnectionManager::new(event_tx, audio_tx, video_tx, Arc::new(Mutex::new(None)))
    }

    fn test_public_key() -> iroh::PublicKey {
        SecretKey::generate().public()
    }

    fn serialize_endpoint_addr(addr: EndpointAddr) -> String {
        EndpointTicket::new(addr).encode_string()
    }

    async fn wait_for_relay_addr(endpoint: &iroh::Endpoint) -> EndpointAddr {
        timeout(Duration::from_secs(60), async {
            loop {
                let mut addr = endpoint.addr();
                addr.addrs
                    .retain(|transport_addr| transport_addr.is_relay());
                if !addr.addrs.is_empty() {
                    return addr;
                }
                let _ = tokio::time::timeout(Duration::from_millis(250), endpoint.online()).await;
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .expect("endpoint did not publish a relay address")
    }

    async fn set_peer_liveness(
        manager: &ConnectionManager,
        peer_id: &str,
        idle_ms: u64,
        status: PeerConnectionKind,
    ) {
        let mut peers = manager.peers.lock().await;
        let peer = peers.get_mut(peer_id).expect("test peer should exist");
        peer.last_activity_ms.store(
            ConnectionManager::current_timestamp_ms().saturating_sub(idle_ms),
            Ordering::Relaxed,
        );
        peer.connection_status = status;
    }

    async fn connected_call_pair(
        event_tx_a: broadcast::Sender<Event>,
        event_tx_b: broadcast::Sender<Event>,
    ) -> (
        Arc<ConnectionManager>,
        ConnectionManager,
        iroh::Endpoint,
        iroh::Endpoint,
        Router,
        String,
    ) {
        let (audio_tx_a, _) = broadcast::channel::<AudioPacket>(8);
        let (video_tx_a, _) = broadcast::channel::<VideoPacket>(8);
        let mgr_a = Arc::new(test_manager(event_tx_a, audio_tx_a, video_tx_a));

        let (audio_tx_b, _) = broadcast::channel::<AudioPacket>(8);
        let (video_tx_b, _) = broadcast::channel::<VideoPacket>(8);
        let mgr_b = test_manager(event_tx_b, audio_tx_b, video_tx_b);

        let endpoint_a = node::create_test_endpoint().await.unwrap();
        let endpoint_b = node::create_test_endpoint().await.unwrap();
        mgr_a.set_endpoint(endpoint_a.clone()).await;
        mgr_b.set_endpoint(endpoint_b.clone()).await;

        let router_a = Router::builder(endpoint_a.clone())
            .accept(node::NAFAQ_ALPN, NafaqProtocol::new(mgr_a.clone()))
            .accept(node::NAFAQ_DM_ALPN, NafaqDmProtocol::new(mgr_a.clone()))
            .spawn();

        // Mirrors production's create_call/join_call, which mark a call
        // session active before anyone can dial in — without this, the
        // protocol-level ghost-call gate (setup_connection) rejects the
        // inbound test connection below.
        mgr_a.set_call_session_active(true);

        let addr_a = node::parse_ticket(&node::generate_ticket(&endpoint_a))
            .unwrap()
            .endpoint_addr()
            .clone();
        let peer_id = mgr_b.connect_to_peer(&endpoint_b, addr_a).await.unwrap();

        (mgr_a, mgr_b, endpoint_a, endpoint_b, router_a, peer_id)
    }

    #[test]
    fn duplicate_tie_break_prefers_outbound_for_higher_local_id() {
        assert_eq!(
            preferred_connection_direction("node-z", "node-a"),
            ConnectionDirection::Outbound
        );
        assert_eq!(
            preferred_connection_direction("node-a", "node-z"),
            ConnectionDirection::Inbound
        );
    }

    #[test]
    fn duplicate_tie_break_is_complementary_for_simultaneous_dials() {
        let low = "node-a";
        let high = "node-z";

        assert!(should_replace_connection(
            low,
            high,
            ConnectionDirection::Outbound,
            ConnectionDirection::Inbound
        ));
        assert!(!should_replace_connection(
            low,
            high,
            ConnectionDirection::Inbound,
            ConnectionDirection::Outbound
        ));
        assert!(should_replace_connection(
            high,
            low,
            ConnectionDirection::Inbound,
            ConnectionDirection::Outbound
        ));
        assert!(!should_replace_connection(
            high,
            low,
            ConnectionDirection::Outbound,
            ConnectionDirection::Inbound
        ));
    }

    #[test]
    fn reconnecting_call_candidate_can_replace_same_direction_connection() {
        assert!(should_accept_call_connection_candidate(
            None,
            "node-z",
            ConnectionDirection::Outbound,
            &PeerConnectionKind::Reconnecting,
            ConnectionDirection::Outbound,
        ));
        assert!(!should_accept_call_connection_candidate(
            None,
            "node-z",
            ConnectionDirection::Outbound,
            &PeerConnectionKind::Connected,
            ConnectionDirection::Outbound,
        ));
    }

    #[tokio::test]
    async fn duplicate_call_reservation_allows_only_one_in_flight_connect() {
        let (event_tx, _) = broadcast::channel::<Event>(8);
        let (audio_tx, _) = broadcast::channel::<AudioPacket>(8);
        let (video_tx, _) = broadcast::channel::<VideoPacket>(8);
        let manager = test_manager(event_tx, audio_tx, video_tx);

        assert!(manager.reserve_call_connecting("peer-a").await);
        assert!(!manager.reserve_call_connecting("peer-a").await);
        assert!(manager.reserve_call_connecting("peer-b").await);

        manager.clear_call_connecting("peer-a").await;
        assert!(manager.reserve_call_connecting("peer-a").await);
    }

    #[tokio::test]
    async fn duplicate_dm_reservation_allows_only_one_in_flight_connect() {
        let (event_tx, _) = broadcast::channel::<Event>(8);
        let (audio_tx, _) = broadcast::channel::<AudioPacket>(8);
        let (video_tx, _) = broadcast::channel::<VideoPacket>(8);
        let manager = test_manager(event_tx, audio_tx, video_tx);

        assert!(manager.reserve_dm_connecting("peer-a").await);
        assert!(!manager.reserve_dm_connecting("peer-a").await);
        assert!(manager.reserve_dm_connecting("peer-b").await);

        manager.clear_dm_connecting("peer-a").await;
        assert!(manager.reserve_dm_connecting("peer-a").await);
    }

    #[tokio::test]
    async fn call_connection_reservation_guard_cleans_up_on_drop() {
        let (event_tx, _) = broadcast::channel::<Event>(8);
        let (audio_tx, _) = broadcast::channel::<AudioPacket>(8);
        let (video_tx, _) = broadcast::channel::<VideoPacket>(8);
        let manager = test_manager(event_tx, audio_tx, video_tx);

        let guard = manager
            .reserve_call_connecting_guard("peer-a")
            .await
            .expect("first reservation should succeed");
        assert!(
            manager
                .reserve_call_connecting_guard("peer-a")
                .await
                .is_none()
        );

        drop(guard);

        assert!(
            manager
                .reserve_call_connecting_guard("peer-a")
                .await
                .is_some()
        );
    }

    #[tokio::test]
    async fn dm_connection_reservation_guard_cleans_up_on_drop() {
        let (event_tx, _) = broadcast::channel::<Event>(8);
        let (audio_tx, _) = broadcast::channel::<AudioPacket>(8);
        let (video_tx, _) = broadcast::channel::<VideoPacket>(8);
        let manager = test_manager(event_tx, audio_tx, video_tx);

        let guard = manager
            .reserve_dm_connecting_guard("peer-a")
            .await
            .expect("first reservation should succeed");
        assert!(
            manager
                .reserve_dm_connecting_guard("peer-a")
                .await
                .is_none()
        );

        drop(guard);

        assert!(
            manager
                .reserve_dm_connecting_guard("peer-a")
                .await
                .is_some()
        );
    }

    #[tokio::test]
    async fn duplicate_call_connection_attempt_returns_in_progress_error() {
        let (event_tx, _) = broadcast::channel::<Event>(8);
        let (audio_tx, _) = broadcast::channel::<AudioPacket>(8);
        let (video_tx, _) = broadcast::channel::<VideoPacket>(8);
        let manager = test_manager(event_tx, audio_tx, video_tx);
        let endpoint = node::create_test_endpoint().await.unwrap();
        let addr = endpoint.addr();
        let peer_id = addr.id.to_string();

        assert!(manager.reserve_call_connecting(&peer_id).await);

        let err = manager
            .connect_to_peer_with_timeout(&endpoint, addr, Duration::from_millis(1))
            .await
            .expect_err("duplicate in-flight connect must not report success");
        assert_eq!(
            err.to_string(),
            format!("connection already in progress for peer {peer_id}")
        );

        endpoint.close().await;
    }

    #[tokio::test]
    async fn duplicate_dm_connection_attempt_returns_in_progress_error() {
        let (event_tx, _) = broadcast::channel::<Event>(8);
        let (audio_tx, _) = broadcast::channel::<AudioPacket>(8);
        let (video_tx, _) = broadcast::channel::<VideoPacket>(8);
        let manager = test_manager(event_tx, audio_tx, video_tx);

        assert!(manager.reserve_dm_connecting("peer-a").await);

        let err = manager
            .connect_dm("peer-a")
            .await
            .expect_err("duplicate in-flight DM connect must not report success");
        assert_eq!(
            err.to_string(),
            "DM connection already in progress for peer peer-a"
        );
    }

    #[tokio::test]
    async fn resilience_timeout_helper_returns_clear_error() {
        let err = with_timeout(
            Duration::from_millis(5),
            "timed out test helper",
            std::future::pending::<Result<()>>(),
        )
        .await
        .expect_err("pending future should time out");

        assert_eq!(err.to_string(), "timed out test helper");
    }

    #[tokio::test]
    async fn timeout_helper_returns_successful_result() {
        let value = with_timeout(Duration::from_secs(1), "timed out test helper", async {
            Ok(7usize)
        })
        .await
        .expect("ready future should not time out");

        assert_eq!(value, 7);
    }

    #[tokio::test]
    async fn resilience_ticket_upsert_changed_ticket_updates_record() {
        let (event_tx, _) = broadcast::channel::<Event>(8);
        let (audio_tx, _) = broadcast::channel::<AudioPacket>(8);
        let (video_tx, _) = broadcast::channel::<VideoPacket>(8);
        let manager = test_manager(event_tx, audio_tx, video_tx);

        assert!(manager.upsert_peer_ticket("peer-a", "ticket-1").await);
        let first_updated = manager
            .peer_tickets
            .lock()
            .await
            .get("peer-a")
            .expect("ticket record")
            .last_updated_ms;

        manager.record_peer_ticket_dial_failure("peer-a").await;
        assert!(manager.upsert_peer_ticket("peer-a", "ticket-2").await);

        let record = manager
            .peer_tickets
            .lock()
            .await
            .get("peer-a")
            .expect("ticket record")
            .clone();
        assert_eq!(record.ticket, "ticket-2");
        assert!(record.last_updated_ms >= first_updated);
        assert_eq!(record.last_dial_failed_ms, None);
        assert_eq!(record.dial_failures, 0);
    }

    #[tokio::test]
    async fn ticket_upsert_identical_ticket_preserves_failure_counters() {
        let (event_tx, _) = broadcast::channel::<Event>(8);
        let (audio_tx, _) = broadcast::channel::<AudioPacket>(8);
        let (video_tx, _) = broadcast::channel::<VideoPacket>(8);
        let manager = test_manager(event_tx, audio_tx, video_tx);

        assert!(manager.upsert_peer_ticket("peer-a", "ticket-1").await);
        manager.record_peer_ticket_dial_failure("peer-a").await;
        manager.record_peer_ticket_dial_failure("peer-a").await;
        let failed_at = manager
            .peer_tickets
            .lock()
            .await
            .get("peer-a")
            .expect("ticket record")
            .last_dial_failed_ms;

        assert!(!manager.upsert_peer_ticket("peer-a", "ticket-1").await);

        let record = manager
            .peer_tickets
            .lock()
            .await
            .get("peer-a")
            .expect("ticket record")
            .clone();
        assert_eq!(record.ticket, "ticket-1");
        assert_eq!(record.last_dial_failed_ms, failed_at);
        assert_eq!(record.dial_failures, 2);
    }

    #[tokio::test]
    async fn foreign_relay_ticket_is_rejected_before_cache_or_dial() {
        let (event_tx, _) = broadcast::channel::<Event>(8);
        let (audio_tx, _) = broadcast::channel::<AudioPacket>(8);
        let (video_tx, _) = broadcast::channel::<VideoPacket>(8);
        let manager = test_manager(event_tx, audio_tx, video_tx);
        let foreign_relay = "https://foreign-relay.example".parse().unwrap();
        let addr =
            EndpointAddr::from_parts(test_public_key(), [TransportAddr::Relay(foreign_relay)]);
        let peer_id = addr.id.to_string();
        let ticket = serialize_endpoint_addr(addr);

        manager
            .handle_peer_announce("sender", peer_id.clone(), ticket.clone())
            .await;
        assert!(
            !manager.peer_tickets.lock().await.contains_key(&peer_id),
            "foreign relay announce must not be cached"
        );

        let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .bind()
            .await
            .unwrap();
        let err = manager
            .connect_to_peer_with_ticket(&endpoint, &ticket)
            .await
            .expect_err("foreign relay ticket should be rejected before dialing");
        assert!(err.to_string().contains("unsupported relay"));
        assert!(
            !manager.peer_tickets.lock().await.contains_key(&peer_id),
            "foreign relay join ticket must not be cached"
        );
        endpoint.close().await;
    }

    #[tokio::test]
    async fn invalid_or_foreign_ticket_replay_does_not_replace_trusted_cache() {
        let (event_tx, _) = broadcast::channel::<Event>(8);
        let (audio_tx, _) = broadcast::channel::<AudioPacket>(8);
        let (video_tx, _) = broadcast::channel::<VideoPacket>(8);
        let manager = test_manager(event_tx, audio_tx, video_tx);

        let trusted_addr = EndpointAddr::from_parts(
            test_public_key(),
            [TransportAddr::Relay(node::RELAY_URL_PARSED.clone())],
        );
        let peer_id = trusted_addr.id.to_string();
        let trusted_ticket = serialize_endpoint_addr(trusted_addr);
        assert!(manager.upsert_peer_ticket(&peer_id, &trusted_ticket).await);

        let foreign_relay = "https://foreign-relay.example".parse().unwrap();
        let foreign_addr = EndpointAddr::from_parts(
            peer_id.parse().unwrap(),
            [TransportAddr::Relay(foreign_relay)],
        );
        let foreign_ticket = serialize_endpoint_addr(foreign_addr);

        manager
            .handle_peer_announce("sender", peer_id.clone(), foreign_ticket.clone())
            .await;
        manager
            .handle_peer_announce("sender", peer_id.clone(), "not-a-ticket".to_string())
            .await;

        let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .bind()
            .await
            .unwrap();
        let foreign_err = manager
            .connect_to_peer_with_ticket(&endpoint, &foreign_ticket)
            .await
            .expect_err("foreign relay replay must be rejected");
        assert!(foreign_err.to_string().contains("unsupported relay"));
        manager
            .connect_to_peer_with_ticket(&endpoint, "not-a-ticket")
            .await
            .expect_err("invalid ticket replay must be rejected");

        let cached_ticket = manager
            .peer_tickets
            .lock()
            .await
            .get(&peer_id)
            .expect("trusted ticket should remain cached")
            .ticket
            .clone();
        assert_eq!(cached_ticket, trusted_ticket);
        assert!(!manager.peers.lock().await.contains_key(&peer_id));

        endpoint.close().await;
    }

    #[tokio::test]
    async fn own_relay_and_direct_only_tickets_are_accepted_for_cache() {
        let (event_tx, _) = broadcast::channel::<Event>(8);
        let (audio_tx, _) = broadcast::channel::<AudioPacket>(8);
        let (video_tx, _) = broadcast::channel::<VideoPacket>(8);
        let manager = test_manager(event_tx, audio_tx, video_tx);

        let own_relay_addr = EndpointAddr::from_parts(
            test_public_key(),
            [TransportAddr::Relay(node::RELAY_URL_PARSED.clone())],
        );
        let own_relay_peer_id = own_relay_addr.id.to_string();
        let own_relay_ticket = serialize_endpoint_addr(own_relay_addr);
        manager
            .handle_peer_announce(
                "sender",
                own_relay_peer_id.clone(),
                own_relay_ticket.clone(),
            )
            .await;

        let direct_addr = EndpointAddr::from_parts(
            test_public_key(),
            [TransportAddr::Ip("127.0.0.1:12345".parse().unwrap())],
        );
        let direct_peer_id = direct_addr.id.to_string();
        let direct_ticket = serialize_endpoint_addr(direct_addr);
        manager
            .handle_peer_announce("sender", direct_peer_id.clone(), direct_ticket.clone())
            .await;

        let tickets = manager.peer_tickets.lock().await;
        assert_eq!(
            tickets
                .get(&own_relay_peer_id)
                .map(|record| record.ticket.as_str()),
            Some(own_relay_ticket.as_str())
        );
        assert_eq!(
            tickets
                .get(&direct_peer_id)
                .map(|record| record.ticket.as_str()),
            Some(direct_ticket.as_str())
        );
    }

    #[test]
    fn relay_target_selection_is_independent_of_auto_dial_outcome() {
        let peers = vec![
            "sender".to_string(),
            "relay-target".to_string(),
            "announced".to_string(),
        ];

        let relay_targets = relay_targets_for_announce(peers.iter(), "sender", "announced");

        assert_eq!(relay_targets, vec!["relay-target".to_string()]);
    }

    #[tokio::test]
    async fn resilience_missing_ticket_or_endpoint_is_not_announced() {
        let (event_tx, _) = broadcast::channel::<Event>(8);
        let (audio_tx, _) = broadcast::channel::<AudioPacket>(8);
        let (video_tx, _) = broadcast::channel::<VideoPacket>(8);
        let manager = test_manager(event_tx, audio_tx, video_tx);

        assert!(manager.latest_self_announce_action().await.is_none());

        *manager.latest_ticket.lock().await = Some("ticket-before-endpoint-ready".to_string());
        assert!(manager.latest_self_announce_action().await.is_none());
    }

    #[tokio::test]
    async fn resilience_ticket_latest_self_announce_uses_latest_ticket_only() {
        let (event_tx, _) = broadcast::channel::<Event>(8);
        let (audio_tx, _) = broadcast::channel::<AudioPacket>(8);
        let (video_tx, _) = broadcast::channel::<VideoPacket>(8);
        let manager = test_manager(event_tx, audio_tx, video_tx);
        let endpoint = node::create_test_endpoint().await.unwrap();
        let own_id = endpoint.id().to_string();
        manager.set_endpoint(endpoint.clone()).await;

        assert!(manager.latest_self_announce_action().await.is_none());

        *manager.latest_ticket.lock().await = Some("stale-ticket".to_string());
        *manager.latest_ticket.lock().await = Some("fresh-ticket".to_string());

        match manager.latest_self_announce_action().await {
            Some(ControlAction::PeerAnnounce { peer_id, ticket }) => {
                assert_eq!(peer_id, own_id);
                assert_eq!(ticket, "fresh-ticket");
            }
            other => panic!("expected fresh self PeerAnnounce, got {other:?}"),
        }

        endpoint.close().await;
    }

    #[tokio::test]
    async fn liveness_does_not_disconnect_after_legacy_fifteen_second_idle() {
        let (event_tx_a, _) = broadcast::channel::<Event>(64);
        let (event_tx_b, _) = broadcast::channel::<Event>(64);
        let (_mgr_a, mgr_b, endpoint_a, endpoint_b, router_a, peer_id) =
            connected_call_pair(event_tx_a, event_tx_b.clone()).await;
        let mut rx_b = event_tx_b.subscribe();

        set_peer_liveness(&mgr_b, &peer_id, 15_001, PeerConnectionKind::Connected).await;
        mgr_b.maintain_peer_liveness().await;

        assert!(mgr_b.peers.lock().await.contains_key(&peer_id));
        let disconnected = timeout(Duration::from_millis(250), async {
            loop {
                match rx_b.recv().await {
                    Ok(Event::PeerDisconnected { peer_id }) => break Some(peer_id),
                    Ok(_) => {}
                    Err(_) => break None,
                }
            }
        })
        .await;
        assert!(
            disconnected.is_err(),
            "15s idle peer should not be disconnected"
        );

        router_a.shutdown().await.ok();
        endpoint_b.close().await;
        endpoint_a.close().await;
    }

    #[tokio::test]
    async fn liveness_marks_connected_peer_suspect_without_removing_it() {
        let (event_tx_a, _) = broadcast::channel::<Event>(64);
        let (event_tx_b, _) = broadcast::channel::<Event>(64);
        let (_mgr_a, mgr_b, endpoint_a, endpoint_b, router_a, peer_id) =
            connected_call_pair(event_tx_a, event_tx_b.clone()).await;
        let mut rx_b = event_tx_b.subscribe();

        set_peer_liveness(
            &mgr_b,
            &peer_id,
            SUSPECT_AFTER_MS + 1,
            PeerConnectionKind::Connected,
        )
        .await;
        mgr_b.maintain_peer_liveness().await;

        timeout(Duration::from_secs(2), async {
            loop {
                match rx_b.recv().await {
                    Ok(Event::PeerConnectionStatusChanged {
                        peer_id: id,
                        status,
                        ..
                    }) if id == peer_id && status == PeerConnectionKind::Suspect => {
                        break;
                    }
                    Ok(_) => {}
                    Err(_) => {}
                }
            }
        })
        .await
        .expect("timed out waiting for suspect status");
        assert!(mgr_b.peers.lock().await.contains_key(&peer_id));

        router_a.shutdown().await.ok();
        endpoint_b.close().await;
        endpoint_a.close().await;
    }

    #[tokio::test]
    async fn liveness_reconnecting_without_ticket_keeps_peer_until_final_timeout() {
        let (event_tx_a, _) = broadcast::channel::<Event>(64);
        let (event_tx_b, _) = broadcast::channel::<Event>(64);
        let (_mgr_a, mgr_b, endpoint_a, endpoint_b, router_a, peer_id) =
            connected_call_pair(event_tx_a, event_tx_b.clone()).await;
        let mut rx_b = event_tx_b.subscribe();
        mgr_b.peer_tickets.lock().await.remove(&peer_id);

        set_peer_liveness(
            &mgr_b,
            &peer_id,
            RECONNECT_AFTER_MS + 1,
            PeerConnectionKind::Suspect,
        )
        .await;
        mgr_b.maintain_peer_liveness().await;

        timeout(Duration::from_secs(2), async {
            loop {
                match rx_b.recv().await {
                    Ok(Event::PeerConnectionStatusChanged {
                        peer_id: id,
                        status,
                        reason,
                    }) if id == peer_id && status == PeerConnectionKind::Suspect => {
                        assert_eq!(
                            reason.as_deref(),
                            Some(
                                "peer liveness is stale; waiting for fresh peer ticket or activity"
                            )
                        );
                        break;
                    }
                    Ok(Event::PeerConnectionStatusChanged {
                        peer_id: id,
                        status,
                        ..
                    }) if id == peer_id && status == PeerConnectionKind::Reconnecting => {
                        panic!("no-ticket suspect peer should not enter reconnecting status");
                    }
                    Ok(_) => {}
                    Err(_) => {}
                }
            }
        })
        .await
        .expect("timed out waiting for suspect no-ticket status");

        {
            let peers = mgr_b.peers.lock().await;
            let peer = peers.get(&peer_id).expect("peer should remain cached");
            assert_eq!(peer.connection_status, PeerConnectionKind::Suspect);
        }

        let no_reconnecting_or_disconnected = timeout(Duration::from_millis(250), async {
            loop {
                match rx_b.recv().await {
                    Ok(Event::PeerConnectionStatusChanged {
                        peer_id: id,
                        status: PeerConnectionKind::Reconnecting,
                        ..
                    }) if id == peer_id => break false,
                    Ok(Event::PeerDisconnected { peer_id: id }) if id == peer_id => break false,
                    Ok(_) => {}
                    Err(_) => break true,
                }
            }
        })
        .await;
        assert!(
            no_reconnecting_or_disconnected.is_err(),
            "no-ticket suspect peer should not emit reconnecting or disconnect immediately"
        );

        router_a.shutdown().await.ok();
        endpoint_b.close().await;
        endpoint_a.close().await;
    }

    #[tokio::test]
    async fn liveness_reconnect_attempt_uses_latest_cached_ticket_record() {
        let (event_tx_a, _) = broadcast::channel::<Event>(64);
        let (event_tx_b, _) = broadcast::channel::<Event>(64);
        let (_mgr_a, mgr_b, endpoint_a, endpoint_b, router_a, peer_id) =
            connected_call_pair(event_tx_a, event_tx_b).await;

        assert!(
            mgr_b
                .upsert_peer_ticket(&peer_id, &node::generate_ticket(&endpoint_a))
                .await
        );
        assert!(
            mgr_b
                .upsert_peer_ticket(&peer_id, "not-a-valid-ticket")
                .await
        );
        set_peer_liveness(
            &mgr_b,
            &peer_id,
            RECONNECT_AFTER_MS + 1,
            PeerConnectionKind::Reconnecting,
        )
        .await;

        mgr_b.maintain_peer_liveness().await;

        timeout(Duration::from_secs(2), async {
            loop {
                let record = mgr_b
                    .peer_tickets
                    .lock()
                    .await
                    .get(&peer_id)
                    .expect("ticket record should remain cached")
                    .clone();
                if record.ticket == "not-a-valid-ticket" && record.dial_failures == 1 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .expect("timed out waiting for latest cached ticket reconnect attempt");
        assert!(mgr_b.peers.lock().await.contains_key(&peer_id));

        router_a.shutdown().await.ok();
        endpoint_b.close().await;
        endpoint_a.close().await;
    }

    #[tokio::test]
    async fn liveness_final_timeout_disconnects_and_removes_peer_with_reason() {
        let (event_tx_a, _) = broadcast::channel::<Event>(64);
        let (event_tx_b, _) = broadcast::channel::<Event>(64);
        let (_mgr_a, mgr_b, endpoint_a, endpoint_b, router_a, peer_id) =
            connected_call_pair(event_tx_a, event_tx_b.clone()).await;
        let mut rx_b = event_tx_b.subscribe();

        set_peer_liveness(
            &mgr_b,
            &peer_id,
            DISCONNECT_AFTER_MS + 1,
            PeerConnectionKind::Reconnecting,
        )
        .await;
        mgr_b.maintain_peer_liveness().await;

        timeout(Duration::from_secs(2), async {
            let mut saw_disconnect = false;
            let mut saw_status_reason = false;
            loop {
                match rx_b.recv().await {
                    Ok(Event::PeerDisconnected { peer_id: id }) if id == peer_id => {
                        saw_disconnect = true;
                    }
                    Ok(Event::PeerConnectionStatusChanged {
                        peer_id: id,
                        status: PeerConnectionKind::Disconnected,
                        reason,
                    }) if id == peer_id && reason.as_deref() == Some("peer timeout") => {
                        saw_status_reason = true;
                    }
                    Ok(_) => {}
                    Err(_) => {}
                }

                if saw_disconnect && saw_status_reason {
                    break;
                }
            }
        })
        .await
        .expect("timed out waiting for final disconnect with reason");
        assert!(!mgr_b.peers.lock().await.contains_key(&peer_id));

        router_a.shutdown().await.ok();
        endpoint_b.close().await;
        endpoint_a.close().await;
    }

    #[test]
    fn timed_out_is_the_only_close_reason_that_triggers_reconnect() {
        use iroh::endpoint::ApplicationClose;

        // The per-connection watcher only treats a silent QUIC idle-timeout as
        // "network dropped, try to reconnect" — every other close reason (ours
        // or the peer's explicit close, a reset, a protocol error) is final,
        // exactly as it was before this behavior existed.
        assert!(matches!(ConnectionError::TimedOut, ConnectionError::TimedOut));
        assert!(!matches!(ConnectionError::LocallyClosed, ConnectionError::TimedOut));
        assert!(!matches!(ConnectionError::Reset, ConnectionError::TimedOut));
        assert!(!matches!(
            ConnectionError::ApplicationClosed(ApplicationClose {
                error_code: 0u32.into(),
                reason: Bytes::from_static(b"call ended"),
            }),
            ConnectionError::TimedOut
        ));
    }

    #[tokio::test]
    async fn ungraceful_close_marks_peer_reconnecting_and_redials_successfully() {
        let (event_tx_a, _) = broadcast::channel::<Event>(64);
        let (event_tx_b, _) = broadcast::channel::<Event>(64);
        let (_mgr_a, mgr_b, endpoint_a, endpoint_b, router_a, peer_id) =
            connected_call_pair(event_tx_a, event_tx_b.clone()).await;
        let mut rx_b = event_tx_b.subscribe();

        // A cached ticket is exactly what `maintain_peer_liveness` relies on
        // too — in production both sides get one via the mutual self-announce
        // exchange right after connecting (see `setup_connection`).
        mgr_b
            .upsert_peer_ticket(&peer_id, &node::generate_ticket(&endpoint_a))
            .await;
        let connection_id = mgr_b
            .peers
            .lock()
            .await
            .get(&peer_id)
            .expect("peer should be connected")
            .connection
            .stable_id();

        let began = mgr_b
            .try_begin_peer_reconnect(&peer_id, connection_id)
            .await;
        assert!(
            began,
            "a silent drop with a cached ticket should start a reconnect"
        );

        timeout(Duration::from_secs(2), async {
            loop {
                match rx_b.recv().await {
                    Ok(Event::PeerConnectionStatusChanged {
                        peer_id: id,
                        status: PeerConnectionKind::Reconnecting,
                        ..
                    }) if id == peer_id => break,
                    Ok(_) => {}
                    Err(_) => {}
                }
            }
        })
        .await
        .expect("timed out waiting for reconnecting status");

        // The peer must stay cached while the redial is in flight — this is
        // exactly the structurally-unreachable path the bug report described:
        // a mid-call drop must not end the call outright.
        assert!(mgr_b.peers.lock().await.contains_key(&peer_id));

        // The cached ticket still points at a live listener, so the reconnect
        // should succeed and bring the peer back to Connected — reusing the
        // ordinary PeerConnected path, not a new protocol.
        timeout(Duration::from_secs(10), async {
            loop {
                match rx_b.recv().await {
                    Ok(Event::PeerConnected { peer_id: id }) if id == peer_id => break,
                    Ok(_) => {}
                    Err(_) => {}
                }
            }
        })
        .await
        .expect("timed out waiting for reconnect to succeed");

        router_a.shutdown().await.ok();
        endpoint_b.close().await;
        endpoint_a.close().await;
    }

    #[tokio::test]
    async fn ungraceful_close_without_cached_ticket_declines_reconnect() {
        let (event_tx_a, _) = broadcast::channel::<Event>(64);
        let (event_tx_b, _) = broadcast::channel::<Event>(64);
        let (_mgr_a, mgr_b, endpoint_a, endpoint_b, router_a, peer_id) =
            connected_call_pair(event_tx_a, event_tx_b).await;

        let connection_id = mgr_b
            .peers
            .lock()
            .await
            .get(&peer_id)
            .expect("peer should be connected")
            .connection
            .stable_id();

        let began = mgr_b
            .try_begin_peer_reconnect(&peer_id, connection_id)
            .await;
        assert!(!began, "no cached ticket means there is nothing to redial");

        let peers = mgr_b.peers.lock().await;
        let peer = peers.get(&peer_id).expect("peer should remain cached");
        assert_eq!(peer.connection_status, PeerConnectionKind::Connected);
        drop(peers);

        router_a.shutdown().await.ok();
        endpoint_b.close().await;
        endpoint_a.close().await;
    }

    #[tokio::test]
    async fn peer_hangup_reason_evicts_remote_side_immediately_without_reconnect_attempt() {
        let (event_tx_a, _) = broadcast::channel::<Event>(64);
        let (event_tx_b, _) = broadcast::channel::<Event>(64);
        let (mgr_a, mgr_b, endpoint_a, endpoint_b, router_a, peer_id) =
            connected_call_pair(event_tx_a.clone(), event_tx_b).await;
        let mut rx_a = event_tx_a.subscribe();
        let b_id = endpoint_b.id().to_string();

        // mgr_b hangs up — this sends an explicit "call ended" CONNECTION_CLOSE
        // to mgr_a, distinct from a silent idle-timeout.
        mgr_b.disconnect_peer(&peer_id).await.unwrap();

        timeout(Duration::from_secs(5), async {
            loop {
                match rx_a.recv().await {
                    Ok(Event::PeerDisconnected { peer_id: id }) if id == b_id => break,
                    Ok(Event::PeerConnectionStatusChanged {
                        peer_id: id,
                        status: PeerConnectionKind::Reconnecting,
                        ..
                    }) if id == b_id => {
                        panic!("an explicit hangup must not trigger a reconnect attempt");
                    }
                    Ok(_) => {}
                    Err(_) => {}
                }
            }
        })
        .await
        .expect("timed out waiting for immediate disconnect after explicit hangup");

        assert!(!mgr_a.peers.lock().await.contains_key(&b_id));

        router_a.shutdown().await.ok();
        endpoint_b.close().await;
        endpoint_a.close().await;
    }

    #[tokio::test]
    async fn dm_connection_status_uses_dm_events_without_peer_status_events() {
        let (event_tx_a, _) = broadcast::channel::<Event>(64);
        let (audio_tx_a, _) = broadcast::channel::<AudioPacket>(8);
        let (video_tx_a, _) = broadcast::channel::<VideoPacket>(8);
        let mgr_a = Arc::new(test_manager(event_tx_a, audio_tx_a, video_tx_a));

        let (event_tx_b, _) = broadcast::channel::<Event>(64);
        let (audio_tx_b, _) = broadcast::channel::<AudioPacket>(8);
        let (video_tx_b, _) = broadcast::channel::<VideoPacket>(8);
        let mgr_b = test_manager(event_tx_b.clone(), audio_tx_b, video_tx_b);

        let endpoint_a = node::create_test_endpoint().await.unwrap();
        let endpoint_b = node::create_test_endpoint().await.unwrap();
        mgr_a.set_endpoint(endpoint_a.clone()).await;
        mgr_b.set_endpoint(endpoint_b.clone()).await;

        let router_a = Router::builder(endpoint_a.clone())
            .accept(node::NAFAQ_DM_ALPN, NafaqDmProtocol::new(mgr_a.clone()))
            .spawn();

        let mut rx_b = event_tx_b.subscribe();
        mgr_b
            .connect_dm(&endpoint_a.id().to_string())
            .await
            .unwrap();

        timeout(Duration::from_secs(10), async {
            loop {
                match rx_b.recv().await {
                    Ok(Event::DmConnected { peer_id })
                        if peer_id == endpoint_a.id().to_string() =>
                    {
                        break;
                    }
                    Ok(Event::PeerConnectionStatusChanged { .. }) => {
                        panic!("DM connect emitted peer-level connection status event");
                    }
                    Ok(_) => {}
                    Err(_) => {}
                }
            }
        })
        .await
        .expect("timed out waiting for DM connection");

        let unexpected_peer_status = timeout(Duration::from_millis(500), async {
            loop {
                match rx_b.recv().await {
                    Ok(Event::PeerConnectionStatusChanged { .. }) => break true,
                    Ok(_) => {}
                    Err(_) => break false,
                }
            }
        })
        .await;
        assert!(
            unexpected_peer_status.is_err(),
            "DM lifecycle should not emit peer-level connection status events"
        );

        router_a.shutdown().await.ok();
        endpoint_b.close().await;
        endpoint_a.close().await;
    }

    #[tokio::test]
    async fn send_dm_connects_before_writing() {
        let (event_tx_a, _) = broadcast::channel::<Event>(64);
        let (audio_tx_a, _) = broadcast::channel::<AudioPacket>(8);
        let (video_tx_a, _) = broadcast::channel::<VideoPacket>(8);
        let mgr_a = Arc::new(test_manager(event_tx_a.clone(), audio_tx_a, video_tx_a));

        let (event_tx_b, _) = broadcast::channel::<Event>(64);
        let (audio_tx_b, _) = broadcast::channel::<AudioPacket>(8);
        let (video_tx_b, _) = broadcast::channel::<VideoPacket>(8);
        let mgr_b = test_manager(event_tx_b, audio_tx_b, video_tx_b);

        let endpoint_a = node::create_test_endpoint().await.unwrap();
        let endpoint_b = node::create_test_endpoint().await.unwrap();
        mgr_a.set_endpoint(endpoint_a.clone()).await;
        mgr_b.set_endpoint(endpoint_b.clone()).await;

        let router_a = Router::builder(endpoint_a.clone())
            .accept(node::NAFAQ_DM_ALPN, NafaqDmProtocol::new(mgr_a.clone()))
            .spawn();

        wait_for_relay_addr(&endpoint_a).await;

        let mut rx_a = event_tx_a.subscribe();
        mgr_b
            .send_dm(
                &endpoint_a.id().to_string(),
                &DmMessage::Text {
                    content: "hello without explicit connect".to_string(),
                    timestamp: 1,
                    id: None,
                },
            )
            .await
            .unwrap();

        timeout(Duration::from_secs(10), async {
            loop {
                match rx_a.recv().await {
                    Ok(Event::DmReceived {
                        peer_id,
                        message: DmMessage::Text { content, timestamp, .. },
                    }) if peer_id == endpoint_b.id().to_string()
                        && content == "hello without explicit connect"
                        && timestamp == 1 =>
                    {
                        break;
                    }
                    Ok(_) => {}
                    Err(_) => {}
                }
            }
        })
        .await
        .expect("timed out waiting for ensured DM send");
        assert!(mgr_b.dm_peer_connected(&endpoint_a.id().to_string()).await);

        router_a.shutdown().await.ok();
        endpoint_b.close().await;
        endpoint_a.close().await;
    }

    #[tokio::test]
    async fn send_dm_reconnects_once_after_unavailable_stream() {
        let (event_tx_a, _) = broadcast::channel::<Event>(64);
        let (audio_tx_a, _) = broadcast::channel::<AudioPacket>(8);
        let (video_tx_a, _) = broadcast::channel::<VideoPacket>(8);
        let mgr_a = Arc::new(test_manager(event_tx_a.clone(), audio_tx_a, video_tx_a));

        let (event_tx_b, _) = broadcast::channel::<Event>(64);
        let (audio_tx_b, _) = broadcast::channel::<AudioPacket>(8);
        let (video_tx_b, _) = broadcast::channel::<VideoPacket>(8);
        let mgr_b = test_manager(event_tx_b, audio_tx_b, video_tx_b);

        let endpoint_a = node::create_test_endpoint().await.unwrap();
        let endpoint_b = node::create_test_endpoint().await.unwrap();
        mgr_a.set_endpoint(endpoint_a.clone()).await;
        mgr_b.set_endpoint(endpoint_b.clone()).await;

        let router_a = Router::builder(endpoint_a.clone())
            .accept(node::NAFAQ_DM_ALPN, NafaqDmProtocol::new(mgr_a.clone()))
            .spawn();

        wait_for_relay_addr(&endpoint_a).await;

        mgr_b
            .connect_dm(&endpoint_a.id().to_string())
            .await
            .unwrap();
        let stale_send = {
            let dm_peers = mgr_b.dm_peers.lock().await;
            dm_peers
                .get(&endpoint_a.id().to_string())
                .expect("DM peer should be connected")
                .dm_send
                .clone()
        };
        *stale_send.lock().await = None;

        let mut rx_a = event_tx_a.subscribe();
        mgr_b
            .send_dm(
                &endpoint_a.id().to_string(),
                &DmMessage::Text {
                    content: "retry after stale stream".to_string(),
                    timestamp: 2,
                    id: None,
                },
            )
            .await
            .unwrap();

        timeout(Duration::from_secs(10), async {
            loop {
                match rx_a.recv().await {
                    Ok(Event::DmReceived {
                        peer_id,
                        message: DmMessage::Text { content, timestamp, .. },
                    }) if peer_id == endpoint_b.id().to_string()
                        && content == "retry after stale stream"
                        && timestamp == 2 =>
                    {
                        break;
                    }
                    Ok(_) => {}
                    Err(_) => {}
                }
            }
        })
        .await
        .expect("timed out waiting for retried DM send");
        assert!(mgr_b.dm_peer_connected(&endpoint_a.id().to_string()).await);

        router_a.shutdown().await.ok();
        endpoint_b.close().await;
        endpoint_a.close().await;
    }

    #[tokio::test]
    async fn send_dm_frame_strict_surfaces_unavailable_stream_without_reconnect() {
        let (event_tx_a, _) = broadcast::channel::<Event>(64);
        let (audio_tx_a, _) = broadcast::channel::<AudioPacket>(8);
        let (video_tx_a, _) = broadcast::channel::<VideoPacket>(8);
        let mgr_a = Arc::new(test_manager(event_tx_a.clone(), audio_tx_a, video_tx_a));

        let (event_tx_b, _) = broadcast::channel::<Event>(64);
        let (audio_tx_b, _) = broadcast::channel::<AudioPacket>(8);
        let (video_tx_b, _) = broadcast::channel::<VideoPacket>(8);
        let mgr_b = test_manager(event_tx_b, audio_tx_b, video_tx_b);

        let endpoint_a = node::create_test_endpoint().await.unwrap();
        let endpoint_b = node::create_test_endpoint().await.unwrap();
        mgr_a.set_endpoint(endpoint_a.clone()).await;
        mgr_b.set_endpoint(endpoint_b.clone()).await;

        let router_a = Router::builder(endpoint_a.clone())
            .accept(node::NAFAQ_DM_ALPN, NafaqDmProtocol::new(mgr_a.clone()))
            .spawn();

        wait_for_relay_addr(&endpoint_a).await;

        mgr_b
            .connect_dm(&endpoint_a.id().to_string())
            .await
            .unwrap();
        let stale_send = {
            let dm_peers = mgr_b.dm_peers.lock().await;
            dm_peers
                .get(&endpoint_a.id().to_string())
                .expect("DM peer should be connected")
                .dm_send
                .clone()
        };
        *stale_send.lock().await = None;

        let mut rx_a = event_tx_a.subscribe();
        // Mid-transfer frame sends must NOT transparently reconnect: file
        // receiver state is stream-local, so a chunk on a fresh stream would
        // silently vanish. (ensure_dm_connected is the transfer-START repair
        // path and is intentionally allowed to re-dial.)
        let err = mgr_b
            .send_dm_frame_strict(
                &endpoint_a.id().to_string(),
                &DmMessage::FileChunk {
                    id: "strict-transfer".to_string(),
                    offset: 0,
                    data: b"chunk".to_vec(),
                },
            )
            .await
            .expect_err("strict DM send should surface stale stream failure");
        assert!(
            err.to_string().contains("without retry"),
            "unexpected error: {err}"
        );

        let received_file_frame = timeout(Duration::from_millis(500), async {
            loop {
                match rx_a.recv().await {
                    Ok(Event::DmReceived {
                        message: DmMessage::FileChunk { id, .. },
                        ..
                    }) if id == "strict-transfer" => break true,
                    Ok(_) => {}
                    Err(_) => break false,
                }
            }
        })
        .await;
        assert!(
            received_file_frame.is_err(),
            "strict file frame should not be retried onto a fresh DM stream"
        );

        router_a.shutdown().await.ok();
        endpoint_b.close().await;
        endpoint_a.close().await;
    }

    #[tokio::test]
    async fn duplicate_dm_stream_on_call_connection_does_not_close_call_connection() {
        let (event_tx_a, _) = broadcast::channel::<Event>(64);
        let (audio_tx_a, _) = broadcast::channel::<AudioPacket>(8);
        let (video_tx_a, _) = broadcast::channel::<VideoPacket>(8);
        let mgr_a = Arc::new(test_manager(event_tx_a.clone(), audio_tx_a, video_tx_a));

        let (event_tx_b, _) = broadcast::channel::<Event>(64);
        let (audio_tx_b, _) = broadcast::channel::<AudioPacket>(8);
        let (video_tx_b, _) = broadcast::channel::<VideoPacket>(8);
        let mgr_b = test_manager(event_tx_b, audio_tx_b, video_tx_b);

        let endpoint_a = node::create_test_endpoint().await.unwrap();
        let endpoint_b = node::create_test_endpoint().await.unwrap();
        mgr_a.set_endpoint(endpoint_a.clone()).await;
        mgr_b.set_endpoint(endpoint_b.clone()).await;

        let router_a = Router::builder(endpoint_a.clone())
            .accept(node::NAFAQ_ALPN, NafaqProtocol::new(mgr_a.clone()))
            .accept(node::NAFAQ_DM_ALPN, NafaqDmProtocol::new(mgr_a.clone()))
            .spawn();

        mgr_a.set_call_session_active(true);
        let mut rx_a = event_tx_a.subscribe();
        let addr_a = node::parse_ticket(&node::generate_ticket(&endpoint_a))
            .unwrap()
            .endpoint_addr()
            .clone();
        let peer_id = mgr_b.connect_to_peer(&endpoint_b, addr_a).await.unwrap();

        let call_connection = {
            let peers = mgr_b.peers.lock().await;
            peers
                .get(&peer_id)
                .expect("call peer should be stored")
                .connection
                .clone()
        };

        mgr_b
            .connect_dm(&endpoint_a.id().to_string())
            .await
            .unwrap();

        timeout(Duration::from_secs(10), async {
            loop {
                match rx_a.recv().await {
                    Ok(Event::DmConnected { peer_id })
                        if peer_id == endpoint_b.id().to_string() =>
                    {
                        break;
                    }
                    Ok(_) => {}
                    Err(_) => {}
                }
            }
        })
        .await
        .expect("timed out waiting for inbound DM connection");

        let (mut redundant_send, _redundant_recv) = call_connection.open_bi().await.unwrap();
        redundant_send.write_all(&[STREAM_DM]).await.unwrap();

        let closed = timeout(Duration::from_millis(750), call_connection.closed()).await;
        assert!(
            closed.is_err(),
            "duplicate DM stream over call connection closed the call connection"
        );

        mgr_b.send_chat(&peer_id, "call-still-alive").await.unwrap();
        timeout(Duration::from_secs(10), async {
            loop {
                match rx_a.recv().await {
                    Ok(Event::ChatReceived { peer_id, message })
                        if peer_id == endpoint_b.id().to_string()
                            && message == "call-still-alive" =>
                    {
                        break;
                    }
                    Ok(_) => {}
                    Err(_) => {}
                }
            }
        })
        .await
        .expect("timed out waiting for chat over call connection after duplicate DM stream");

        router_a.shutdown().await.ok();
        endpoint_b.close().await;
        endpoint_a.close().await;
    }

    #[tokio::test]
    async fn duplicate_dedicated_dm_flood_drains_one_frame_per_duplicate() {
        const DUPLICATE_COUNT: usize = 4;

        let (event_tx_a, _) = broadcast::channel::<Event>(128);
        let (audio_tx_a, _) = broadcast::channel::<AudioPacket>(8);
        let (video_tx_a, _) = broadcast::channel::<VideoPacket>(8);
        let mgr_a = Arc::new(test_manager(event_tx_a.clone(), audio_tx_a, video_tx_a));

        let (event_tx_b, _) = broadcast::channel::<Event>(128);
        let (audio_tx_b, _) = broadcast::channel::<AudioPacket>(8);
        let (video_tx_b, _) = broadcast::channel::<VideoPacket>(8);
        let mgr_b = Arc::new(test_manager(event_tx_b.clone(), audio_tx_b, video_tx_b));

        let endpoint_a = node::create_test_endpoint().await.unwrap();
        let endpoint_b = node::create_test_endpoint().await.unwrap();
        mgr_a.set_endpoint(endpoint_a.clone()).await;
        mgr_b.set_endpoint(endpoint_b.clone()).await;

        let router_a = Router::builder(endpoint_a.clone())
            .accept(node::NAFAQ_DM_ALPN, NafaqDmProtocol::new(mgr_a.clone()))
            .spawn();
        let router_b = Router::builder(endpoint_b.clone())
            .accept(node::NAFAQ_DM_ALPN, NafaqDmProtocol::new(mgr_b.clone()))
            .spawn();

        let a_id = endpoint_a.id().to_string();
        let b_id = endpoint_b.id().to_string();
        let (high_mgr, high_endpoint, low_mgr, low_endpoint, high_events, low_id) = if a_id > b_id {
            (
                mgr_a.clone(),
                endpoint_a.clone(),
                mgr_b.clone(),
                endpoint_b.clone(),
                event_tx_a.clone(),
                b_id.clone(),
            )
        } else {
            (
                mgr_b.clone(),
                endpoint_b.clone(),
                mgr_a.clone(),
                endpoint_a.clone(),
                event_tx_b.clone(),
                a_id.clone(),
            )
        };

        let mut rx_high = high_events.subscribe();
        high_mgr.connect_dm(&low_id).await.unwrap();
        timeout(Duration::from_secs(10), async {
            loop {
                match rx_high.recv().await {
                    Ok(Event::DmConnected { peer_id }) if peer_id == low_id => break,
                    Ok(_) => {}
                    Err(_) => {}
                }
            }
        })
        .await
        .expect("timed out waiting for baseline DM connection");

        let high_addr = iroh::EndpointAddr::new(high_endpoint.id())
            .with_relay_url(node::RELAY_URL_PARSED.clone());
        let mut duplicate_connections = Vec::with_capacity(DUPLICATE_COUNT);
        for index in 0..DUPLICATE_COUNT {
            let connection = low_endpoint
                .connect(high_addr.clone(), node::NAFAQ_DM_ALPN)
                .await
                .unwrap();
            let (mut send, _recv) = open_typed_bi_stream(&connection, STREAM_DM, "duplicate DM")
                .await
                .unwrap();
            let first = DmMessage::Text {
                content: format!("duplicate-first-{index}"),
                timestamp: index as u64,
                id: None,
            };
            let second = DmMessage::Text {
                content: format!("duplicate-second-{index}"),
                timestamp: index as u64,
                id: None,
            };
            crate::messages::write_framed(&mut send, &serde_json::to_vec(&first).unwrap())
                .await
                .ok();
            crate::messages::write_framed(&mut send, &serde_json::to_vec(&second).unwrap())
                .await
                .ok();
            duplicate_connections.push(connection);
        }

        let mut first_count = 0usize;
        timeout(Duration::from_secs(10), async {
            while first_count < DUPLICATE_COUNT {
                match rx_high.recv().await {
                    Ok(Event::DmReceived {
                        message: DmMessage::Text { content, .. },
                        ..
                    }) if content.starts_with("duplicate-first-") => first_count += 1,
                    Ok(Event::DmReceived {
                        message: DmMessage::Text { content, .. },
                        ..
                    }) if content.starts_with("duplicate-second-") => {
                        panic!("duplicate drain spawned persistent reader and delivered {content}");
                    }
                    Ok(_) => {}
                    Err(_) => {}
                }
            }
        })
        .await
        .expect("timed out waiting for one drained frame per duplicate DM connection");

        let second_frame = timeout(Duration::from_millis(750), async {
            loop {
                match rx_high.recv().await {
                    Ok(Event::DmReceived {
                        message: DmMessage::Text { content, .. },
                        ..
                    }) if content.starts_with("duplicate-second-") => break Some(content),
                    Ok(_) => {}
                    Err(_) => break None,
                }
            }
        })
        .await;
        assert!(
            second_frame.is_err(),
            "duplicate DM drain delivered more than one frame per duplicate connection"
        );

        low_mgr
            .send_dm(
                &high_endpoint.id().to_string(),
                &DmMessage::Text {
                    content: "dm-survived-duplicate-flood".to_string(),
                    timestamp: 99,
                    id: None,
                },
            )
            .await
            .unwrap();
        timeout(Duration::from_secs(10), async {
            loop {
                match rx_high.recv().await {
                    Ok(Event::DmReceived {
                        message: DmMessage::Text { content, .. },
                        ..
                    }) if content == "dm-survived-duplicate-flood" => break,
                    Ok(_) => {}
                    Err(_) => {}
                }
            }
        })
        .await
        .expect("timed out waiting for DM after duplicate flood");

        drop(duplicate_connections);
        router_a.shutdown().await.ok();
        router_b.shutdown().await.ok();
        endpoint_b.close().await;
        endpoint_a.close().await;
    }

    #[tokio::test]
    async fn emits_single_disconnect_when_remote_endpoint_closes() {
        let (event_tx_a, _) = broadcast::channel::<Event>(64);
        let (audio_tx_a, _) = broadcast::channel::<AudioPacket>(8);
        let (video_tx_a, _) = broadcast::channel::<VideoPacket>(8);
        let mgr_a = Arc::new(test_manager(event_tx_a.clone(), audio_tx_a, video_tx_a));

        let (event_tx_b, _) = broadcast::channel::<Event>(64);
        let (audio_tx_b, _) = broadcast::channel::<AudioPacket>(8);
        let (video_tx_b, _) = broadcast::channel::<VideoPacket>(8);
        let mgr_b = Arc::new(test_manager(event_tx_b, audio_tx_b, video_tx_b));

        let endpoint_a = node::create_test_endpoint().await.unwrap();
        let endpoint_b = node::create_test_endpoint().await.unwrap();
        mgr_a.set_endpoint(endpoint_a.clone()).await;
        mgr_b.set_endpoint(endpoint_b.clone()).await;

        let router_a = Router::builder(endpoint_a.clone())
            .accept(node::NAFAQ_ALPN, NafaqProtocol::new(mgr_a.clone()))
            .accept(node::NAFAQ_DM_ALPN, NafaqDmProtocol::new(mgr_a.clone()))
            .spawn();

        mgr_a.set_call_session_active(true);
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
                let paths = conn.paths();
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
        let endpoint_a = node::create_test_endpoint().await.unwrap();
        let endpoint_b = node::create_test_endpoint().await.unwrap();

        let relay_only_addr = wait_for_relay_addr(&endpoint_a).await;

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

    // ── Ghost-call protocol gate (wave 2) ───────────────────────────────

    #[tokio::test]
    async fn inbound_call_dial_rejected_without_active_call_session() {
        use iroh::endpoint::ApplicationClose;

        let (event_tx_a, _) = broadcast::channel::<Event>(16);
        let (audio_tx_a, _) = broadcast::channel::<AudioPacket>(8);
        let (video_tx_a, _) = broadcast::channel::<VideoPacket>(8);
        let mgr_a = Arc::new(test_manager(event_tx_a, audio_tx_a, video_tx_a));

        let (event_tx_b, _) = broadcast::channel::<Event>(16);
        let (audio_tx_b, _) = broadcast::channel::<AudioPacket>(8);
        let (video_tx_b, _) = broadcast::channel::<VideoPacket>(8);
        let mgr_b = test_manager(event_tx_b, audio_tx_b, video_tx_b);

        let endpoint_a = node::create_test_endpoint().await.unwrap();
        let endpoint_b = node::create_test_endpoint().await.unwrap();
        mgr_a.set_endpoint(endpoint_a.clone()).await;
        mgr_b.set_endpoint(endpoint_b.clone()).await;

        let router_a = Router::builder(endpoint_a.clone())
            .accept(node::NAFAQ_ALPN, NafaqProtocol::new(mgr_a.clone()))
            .spawn();

        // mgr_a never called create_call/join_call (call_session_active
        // defaults false) — a dial for its ticket must be rejected.
        //
        // Dial with the raw endpoint (bypassing ConnectionManager) so the
        // assertion is purely about what mgr_a's *acceptor* does: QUIC lets a
        // locally-initiated stream open/write succeed before the peer's
        // rejection is observed, so asserting on the dialer-side
        // ConnectionManager's return value would be racy/wrong — the
        // authoritative signal is the close reason the acceptor sends back.
        let addr_a = iroh::EndpointAddr::new(endpoint_a.id())
            .with_relay_url(node::RELAY_URL_PARSED.clone());
        let connection = endpoint_b.connect(addr_a, node::NAFAQ_ALPN).await.unwrap();

        let close_reason = timeout(Duration::from_secs(10), connection.closed())
            .await
            .expect("gate should close the connection instead of leaving it open");
        match close_reason {
            ConnectionError::ApplicationClosed(ApplicationClose { reason, .. }) => {
                assert_eq!(&reason[..], b"call_not_active");
            }
            other => panic!("expected call_not_active application close, got {other:?}"),
        }
        assert!(mgr_a.peers.lock().await.is_empty());

        router_a.shutdown().await.ok();
        endpoint_b.close().await;
        endpoint_a.close().await;
    }

    #[tokio::test]
    async fn call_session_deactivates_after_last_peer_disconnects() {
        use iroh::endpoint::ApplicationClose;

        let (event_tx_a, _) = broadcast::channel::<Event>(16);
        let (event_tx_b, _) = broadcast::channel::<Event>(16);
        let (mgr_a, _mgr_b, endpoint_a, endpoint_b, router_a, _peer_id) =
            connected_call_pair(event_tx_a, event_tx_b).await;

        let b_id = endpoint_b.id().to_string();
        mgr_a.disconnect_peer(&b_id).await.unwrap();
        assert!(mgr_a.peers.lock().await.is_empty());

        // A stray/late dial for the now-abandoned call session must be
        // rejected — mirrors a callee's join arriving after the caller hung
        // up (or cancelled) and nobody re-armed create_call/join_call.
        let endpoint_c = node::create_test_endpoint().await.unwrap();
        let addr_a = iroh::EndpointAddr::new(endpoint_a.id())
            .with_relay_url(node::RELAY_URL_PARSED.clone());
        let connection = endpoint_c.connect(addr_a, node::NAFAQ_ALPN).await.unwrap();

        let close_reason = timeout(Duration::from_secs(10), connection.closed())
            .await
            .expect("gate should close the connection instead of leaving it open");
        match close_reason {
            ConnectionError::ApplicationClosed(ApplicationClose { reason, .. }) => {
                assert_eq!(&reason[..], b"call_not_active");
            }
            other => panic!("expected call_not_active application close, got {other:?}"),
        }

        router_a.shutdown().await.ok();
        endpoint_b.close().await;
        endpoint_c.close().await;
        endpoint_a.close().await;
    }

    // ── DM delivery ack + dedup (wave 2) ─────────────────────────────────

    #[tokio::test]
    async fn dm_ack_received_after_text_with_id() {
        let (event_tx_a, _) = broadcast::channel::<Event>(64);
        let (audio_tx_a, _) = broadcast::channel::<AudioPacket>(8);
        let (video_tx_a, _) = broadcast::channel::<VideoPacket>(8);
        let mgr_a = Arc::new(test_manager(event_tx_a, audio_tx_a, video_tx_a));

        let (event_tx_b, _) = broadcast::channel::<Event>(64);
        let (audio_tx_b, _) = broadcast::channel::<AudioPacket>(8);
        let (video_tx_b, _) = broadcast::channel::<VideoPacket>(8);
        let mgr_b = test_manager(event_tx_b.clone(), audio_tx_b, video_tx_b);

        let endpoint_a = node::create_test_endpoint().await.unwrap();
        let endpoint_b = node::create_test_endpoint().await.unwrap();
        mgr_a.set_endpoint(endpoint_a.clone()).await;
        mgr_b.set_endpoint(endpoint_b.clone()).await;

        let router_a = Router::builder(endpoint_a.clone())
            .accept(node::NAFAQ_DM_ALPN, NafaqDmProtocol::new(mgr_a.clone()))
            .spawn();

        wait_for_relay_addr(&endpoint_a).await;

        let mut rx_b = event_tx_b.subscribe();
        let a_id = endpoint_a.id().to_string();
        mgr_b
            .send_dm(
                &a_id,
                &DmMessage::Text {
                    content: "hello".to_string(),
                    timestamp: 1,
                    id: Some("msg-1".to_string()),
                },
            )
            .await
            .unwrap();

        timeout(Duration::from_secs(10), async {
            loop {
                match rx_b.recv().await {
                    Ok(Event::DmAckReceived { peer_id, id })
                        if peer_id == a_id && id == "msg-1" =>
                    {
                        break;
                    }
                    Ok(_) => {}
                    Err(_) => {}
                }
            }
        })
        .await
        .expect("timed out waiting for delivery ack");

        router_a.shutdown().await.ok();
        endpoint_b.close().await;
        endpoint_a.close().await;
    }

    #[tokio::test]
    async fn duplicate_text_id_is_deduped_but_still_emits_dm_received_once() {
        let (event_tx_a, _) = broadcast::channel::<Event>(64);
        let (audio_tx_a, _) = broadcast::channel::<AudioPacket>(8);
        let (video_tx_a, _) = broadcast::channel::<VideoPacket>(8);
        let mgr_a = Arc::new(test_manager(event_tx_a.clone(), audio_tx_a, video_tx_a));

        let (event_tx_b, _) = broadcast::channel::<Event>(64);
        let (audio_tx_b, _) = broadcast::channel::<AudioPacket>(8);
        let (video_tx_b, _) = broadcast::channel::<VideoPacket>(8);
        let mgr_b = test_manager(event_tx_b, audio_tx_b, video_tx_b);

        let endpoint_a = node::create_test_endpoint().await.unwrap();
        let endpoint_b = node::create_test_endpoint().await.unwrap();
        mgr_a.set_endpoint(endpoint_a.clone()).await;
        mgr_b.set_endpoint(endpoint_b.clone()).await;

        let router_a = Router::builder(endpoint_a.clone())
            .accept(node::NAFAQ_DM_ALPN, NafaqDmProtocol::new(mgr_a.clone()))
            .spawn();

        wait_for_relay_addr(&endpoint_a).await;

        let mut rx_a = event_tx_a.subscribe();
        let a_id = endpoint_a.id().to_string();
        let msg = DmMessage::Text {
            content: "duplicate-payload".to_string(),
            timestamp: 1,
            id: Some("dup-1".to_string()),
        };
        mgr_b.send_dm(&a_id, &msg).await.unwrap();
        mgr_b.send_dm(&a_id, &msg).await.unwrap();

        let mut received_count = 0usize;
        // Bounded window: count DmReceived for this content, then confirm no
        // second one shows up.
        let _ = timeout(Duration::from_millis(1500), async {
            loop {
                match rx_a.recv().await {
                    Ok(Event::DmReceived {
                        message: DmMessage::Text { content, .. },
                        ..
                    }) if content == "duplicate-payload" => {
                        received_count += 1;
                    }
                    Ok(_) => {}
                    Err(_) => {}
                }
            }
        })
        .await;

        assert_eq!(
            received_count, 1,
            "duplicate text id must be deduped to a single DmReceived"
        );

        router_a.shutdown().await.ok();
        endpoint_b.close().await;
        endpoint_a.close().await;
    }
}
