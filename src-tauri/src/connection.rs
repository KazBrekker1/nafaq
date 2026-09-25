use std::collections::{HashMap, HashSet, VecDeque};
use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU16, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use bytes::Bytes;
use iroh::endpoint::{Connection, ConnectionError, PathId, RecvStream, SendStream};
use tokio::sync::{Mutex, Notify, broadcast};

use crate::codec::is_keyframe;
use crate::video_transport::{
    MAX_VIDEO_FRAME_BYTES, PeerVideoWriter, PendingVideoFrame, ReceivedVideoFrame,
    VIDEO_FRAME_HEADER_LEN, VIDEO_FRAME_READ_TIMEOUT, VideoReorderBuffer, decode_video_frame,
};
use crate::messages::{
    AudioDatagram, AudioPacket, ControlAction, DmMessage, Event, MAX_CHAT_FRAME_BYTES,
    MAX_CONTROL_FRAME_BYTES, MAX_DM_FRAME_BYTES, PeerConnectionKind, STREAM_AUDIO, STREAM_CHAT,
    STREAM_CONTROL, STREAM_DM, STREAM_VIDEO, VideoLayerRequest, VideoPacket,
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
const CONTROL_WRITE_TIMEOUT: Duration = Duration::from_secs(5);
const CHAT_WRITE_TIMEOUT: Duration = Duration::from_secs(5);
const SUSPECT_AFTER_MS: u64 = 10_000;
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

/// Writes one length-prefixed frame to a shared send stream, bounding both
/// the wait for the stream lock and the write itself by `timeout`.
///
/// A write that times out may have put part of the frame on the wire; any
/// later frame would then be parsed from the middle of this one. So on a
/// write timeout the stream is taken out of its slot and reset: later writes
/// fail loudly ("unavailable") and the reconnect/liveness paths take over,
/// instead of silently corrupting the framing.
async fn write_frame_or_reset(
    slot: &Mutex<Option<SendStream>>,
    data: &[u8],
    timeout: Duration,
    stream_name: &str,
) -> Result<()> {
    let mut guard = tokio::time::timeout(timeout, slot.lock())
        .await
        .map_err(|_| anyhow::anyhow!("timed out waiting for {stream_name} stream"))?;
    let Some(send) = guard.as_mut() else {
        anyhow::bail!("{stream_name} stream is unavailable");
    };
    match tokio::time::timeout(timeout, crate::messages::write_framed(send, data)).await {
        Ok(result) => Ok(result?),
        Err(_) => {
            if let Some(mut send) = guard.take() {
                let _ = send.reset(0u32.into());
            }
            anyhow::bail!("timed out writing {stream_name} frame; stream reset")
        }
    }
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

/// Whether a peer-opened bidi stream of `stream_type` may be accepted on a
/// call connection: one each of chat, control and DM; records it as seen.
fn accept_call_bi_stream_type(seen: &mut HashSet<u8>, stream_type: u8) -> bool {
    matches!(stream_type, STREAM_CHAT | STREAM_CONTROL | STREAM_DM) && seen.insert(stream_type)
}

/// Tickets to announce to a newly connected call peer: every cached ticket of
/// another peer that is currently connected to us.
fn tickets_to_announce(
    tickets: &HashMap<String, PeerTicketRecord>,
    current_peer_ids: &HashSet<String>,
    new_peer_id: &str,
) -> Vec<(String, String)> {
    tickets
        .iter()
        .filter(|(id, _)| id.as_str() != new_peer_id && current_peer_ids.contains(id.as_str()))
        .map(|(id, record)| (id.clone(), record.ticket.clone()))
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

/// Transfer ids are echoed to the frontend and used as map keys; anything
/// outside a UUID-ish charset is rejected as a protocol violation.
fn is_valid_transfer_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Reduce a peer-supplied file name to a safe basename that is valid on every
/// platform we run on. `Path::join` with an absolute path replaces the base
/// entirely, so the raw name must never reach a join — strip directories,
/// control chars and leading dots. Windows additionally forbids `<>:"|?*`
/// (`:` also selects an NTFS alternate data stream), silently drops trailing
/// dots/spaces, and maps reserved device names (`CON`, `COM1`, … — with any
/// extension) to devices rather than files.
fn sanitize_file_name(name: &str) -> String {
    const FALLBACK: &str = "download";
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let cleaned: String = base
        .chars()
        .filter(|c| !c.is_control())
        .map(|c| if matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*') { '_' } else { c })
        .take(200)
        .collect();
    let trimmed = cleaned
        .trim_start_matches(['.', ' '])
        .trim_end_matches(['.', ' ']);
    if trimmed.is_empty() {
        return FALLBACK.to_string();
    }
    if is_windows_reserved_name(trimmed) {
        return format!("_{trimmed}");
    }
    trimmed.to_string()
}

/// `CON`, `PRN`, `AUX`, `NUL`, `COM1`-`COM9`, `LPT1`-`LPT9`, case-insensitive,
/// with or without an extension (`nul.txt` is still the NUL device).
fn is_windows_reserved_name(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or(name).trim_end_matches(' ');
    let upper = stem.to_ascii_uppercase();
    match upper.as_str() {
        "CON" | "PRN" | "AUX" | "NUL" => true,
        _ => {
            let bytes = upper.as_bytes();
            bytes.len() == 4
                && (upper.starts_with("COM") || upper.starts_with("LPT"))
                && (b'1'..=b'9').contains(&bytes[3])
        }
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

/// Tell the frontend a transfer ended without a saved file, so its card shows
/// "failed" instead of hanging at partial progress.
fn emit_file_transfer_failed(
    event_tx: &broadcast::Sender<Event>,
    peer_id: &str,
    file_id: &str,
    reason: &str,
) {
    let _ = event_tx.send(Event::DmFileTransferFailed {
        peer_id: peer_id.to_string(),
        file_id: file_id.to_string(),
        reason: Some(reason.to_string()),
    });
}

/// Where completed transfers are saved.
fn downloads_dir() -> std::path::PathBuf {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map(|h| std::path::PathBuf::from(h).join("Downloads"))
        .unwrap_or_else(|_| std::env::temp_dir())
}

/// Move a completed temp file to its final location, falling back to copy +
/// remove when rename fails (e.g. across filesystems).
async fn save_received_file(
    temp_path: &std::path::Path,
    final_path: &std::path::Path,
) -> std::io::Result<()> {
    match tokio::fs::rename(temp_path, final_path).await {
        Ok(()) => Ok(()),
        Err(rename_err) => {
            tracing::debug!("rename failed ({rename_err}), trying copy fallback");
            tokio::fs::copy(temp_path, final_path).await?;
            let _ = tokio::fs::remove_file(temp_path).await;
            Ok(())
        }
    }
}

/// Process a single DM message, handling file reconstruction when appropriate.
/// Returns `true` if the caller should `continue` (i.e. skip emitting DmReceived).
async fn handle_dm_file_message(
    dm_msg: &DmMessage,
    peer_id: &str,
    sender_is_contact: bool,
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
            // Only saved contacts may write files to disk.
            if !sender_is_contact {
                tracing::warn!("Rejecting file transfer {id} from {peer_id}: not a contact");
                emit_file_transfer_failed(event_tx, peer_id, id, "sender is not a contact");
                return true;
            }
            // Bound concurrent in-flight transfers so a peer can't exhaust file
            // descriptors / memory by opening unbounded FileStarts without ends.
            const MAX_CONCURRENT_TRANSFERS: usize = 16;
            if active_files.len() >= MAX_CONCURRENT_TRANSFERS && !active_files.contains_key(id) {
                tracing::warn!(
                    "Rejecting file transfer {id} from {peer_id}: too many concurrent transfers"
                );
                emit_file_transfer_failed(event_tx, peer_id, id, "too many concurrent transfers");
                return true;
            }
            // Named by a local uuid, never the peer-chosen id, and created
            // exclusively so an existing file (or a symlink planted in the
            // shared temp dir) is never opened or truncated.
            let temp_path =
                std::env::temp_dir().join(format!("nafaq_recv_{}", uuid::Uuid::new_v4()));
            let created = tokio::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp_path)
                .await;
            match created {
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
                    emit_file_transfer_failed(event_tx, peer_id, id, "could not create temp file");
                    return true;
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
                    emit_file_transfer_failed(event_tx, peer_id, id, "chunk exceeds declared size");
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
            let Some(mut recv) = active_files.remove(id) else {
                tracing::debug!("FileEnd for unknown transfer {id}, ignoring");
                return false;
            };
            // Flush and close the temp file
            let _ = recv.file.flush().await;
            drop(recv.file);

            if recv.received_bytes != recv.expected_size {
                tracing::warn!(
                    "File transfer {id} from {peer_id} ended at {} of {} bytes; discarding",
                    recv.received_bytes,
                    recv.expected_size
                );
                let _ = tokio::fs::remove_file(&recv.temp_path).await;
                emit_file_transfer_failed(event_tx, peer_id, id, "incomplete transfer");
                // Suppress DmReceived(FileEnd): it would mark the card complete.
                return true;
            }

            let downloads_dir = downloads_dir();
            let _ = tokio::fs::create_dir_all(&downloads_dir).await;
            let final_path = unique_file_path(&downloads_dir, &recv.final_name);
            // Belt and braces on top of sanitize_file_name: the file must
            // land directly in the downloads directory, nowhere else.
            if final_path.parent() != Some(downloads_dir.as_path()) {
                tracing::warn!(
                    "Refusing to save transfer {id}: {} escapes {}",
                    final_path.display(),
                    downloads_dir.display()
                );
                let _ = tokio::fs::remove_file(&recv.temp_path).await;
                emit_file_transfer_failed(event_tx, peer_id, id, "invalid file name");
                return true;
            }

            match save_received_file(&recv.temp_path, &final_path).await {
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
                    false
                }
                Err(e) => {
                    tracing::warn!("Failed to save file for transfer {id}: {e}");
                    let _ = tokio::fs::remove_file(&recv.temp_path).await;
                    emit_file_transfer_failed(event_tx, peer_id, id, "could not save file");
                    true
                }
            }
        }
        _ => false,
    }
}

/// Call-signaling variants (CallInvite/CallDecline/CallCancel) are handled
/// before generic DmReceived/file processing and never reach it. Returns
/// `true` if the message was fully handled here.
///
/// CallDecline additionally clears our own `call_session_active` flag when it
/// answers our outstanding invite and nobody joined: our ticket must stop
/// accepting inbound call dials (see `setup_connection`'s gate) even though
/// no call peer was ever established to disconnect.
async fn handle_call_signal(manager: &ConnectionManager, dm_msg: &DmMessage, peer_id: &str) -> bool {
    match dm_msg {
        DmMessage::CallInvite { ticket } => {
            let _ = manager.event_tx.send(Event::CallInviteReceived {
                peer_id: peer_id.to_string(),
                ticket: ticket.clone(),
            });
            true
        }
        DmMessage::CallDecline => {
            manager.handle_call_decline(peer_id).await;
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
    if handle_call_signal(manager, &dm_msg, peer_id).await {
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

    let sender_is_contact =
        !matches!(dm_msg, DmMessage::FileStart { .. }) || manager.is_contact(peer_id);
    let skip_dm_event = handle_dm_file_message(
        &dm_msg,
        peer_id,
        sender_is_contact,
        active_files,
        &manager.event_tx,
    )
    .await;
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
        emit_file_transfer_failed(event_tx, peer_id, &id, "stream closed mid-transfer");
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
        crate::messages::read_framed(recv, MAX_DM_FRAME_BYTES),
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
        match crate::messages::read_framed(recv, MAX_DM_FRAME_BYTES).await {
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

/// Inbound video state of one call connection. Owned by that connection's
/// receiver tasks: sequence numbers restart on every new connection, so a
/// reconnect naturally gets a fresh buffer and nothing needs cleaning up.
#[derive(Default)]
struct VideoReceiveState {
    reorder: VideoReorderBuffer,
    last_keyframe_request: Option<std::time::Instant>,
}

const RECEIVER_KEYFRAME_REQUEST_INTERVAL: Duration = Duration::from_millis(300);

#[derive(Debug, Clone, serde::Serialize)]
pub struct NetworkPeerStats {
    pub peer_id: String,
    pub rtt_ms: u64,
    pub lost_packets: u64,
    pub lost_bytes: u64,
    pub datagram_send_buffer_space: usize,
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
    event_tx: broadcast::Sender<Event>,
    audio_media_tx: broadcast::Sender<AudioPacket>,
    video_media_tx: broadcast::Sender<VideoPacket>,
    presence: Arc<Mutex<Option<Arc<crate::presence::PresenceManager>>>>,
    /// True while we've deliberately created or joined a call (create_call /
    /// join_call) and haven't cancelled or fully left it yet. Gates inbound
    /// call-media connection acceptance in `setup_connection` — see the
    /// wave-2 ghost-call fix. DM/presence/file streams are unaffected.
    call_session_active: Arc<AtomicBool>,
    /// Peer we most recently sent a CallInvite to and are still waiting on.
    /// A CallDecline only closes our call session when it comes from this
    /// peer while nobody has joined yet — a stray decline (or one for a
    /// third-party invite sent mid-call) must not shut the gate on a live call.
    pending_invitee: Arc<StdMutex<Option<String>>>,
    /// Peers whose stale call connection was evicted to make room for a
    /// replacement that is still being set up, mapped to the `stable_id` of
    /// that replacement. Their `PeerDisconnected` is withheld so a reconnect
    /// doesn't look like the peer left; it is emitted only if the replacement
    /// fails (see `setup_connection`). Keyed by connection so a concurrent,
    /// unrelated setup attempt for the same peer can't clear or emit it.
    replacing_peers: Arc<StdMutex<HashMap<String, usize>>>,
    /// Recently-seen DM `Text` message ids per peer, for ack + dedup. Bounded
    /// per peer so a chatty (or malicious) peer can't grow this unbounded.
    recent_dm_ids: Arc<Mutex<RecentDmIds>>,
    /// Node ids of saved contacts (mirrors the contacts store). Only contacts
    /// may send us files. Kept in sync by startup load and add/remove_contact.
    contacts: Arc<StdMutex<HashSet<String>>>,
}

const RECENT_DM_IDS_CAPACITY: usize = 256;
/// Peers tracked in `RecentDmIds`; the least recently active is evicted, so
/// many distinct senders can't grow the map without bound.
const RECENT_DM_PEERS_CAPACITY: usize = 256;

#[derive(Default)]
struct RecentIds {
    order: VecDeque<String>,
    set: HashSet<String>,
    /// `RecentDmIds::clock` value at this peer's last message.
    last_used: u64,
}

/// Per-peer recent DM ids, bounded both per peer and in number of peers.
#[derive(Default)]
struct RecentDmIds {
    peers: HashMap<String, RecentIds>,
    clock: u64,
}

impl RecentDmIds {
    /// Returns true if `id` was already seen from `peer_id`, otherwise records it.
    fn check_and_insert(&mut self, peer_id: &str, id: &str) -> bool {
        self.clock += 1;
        if !self.peers.contains_key(peer_id) && self.peers.len() >= RECENT_DM_PEERS_CAPACITY {
            if let Some(lru) = self
                .peers
                .iter()
                .min_by_key(|(_, ids)| ids.last_used)
                .map(|(peer, _)| peer.clone())
            {
                self.peers.remove(&lru);
            }
        }
        let ids = self.peers.entry(peer_id.to_string()).or_default();
        ids.last_used = self.clock;
        ids.check_and_insert(id)
    }
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
            event_tx,
            audio_media_tx,
            video_media_tx,
            presence: Arc::new(Mutex::new(None)),
            call_session_active: Arc::new(AtomicBool::new(false)),
            pending_invitee: Arc::new(StdMutex::new(None)),
            replacing_peers: Arc::new(StdMutex::new(HashMap::new())),
            recent_dm_ids: Arc::new(Mutex::new(RecentDmIds::default())),
            contacts: Arc::new(StdMutex::new(HashSet::new())),
        }
    }

    fn lock_contacts(&self) -> std::sync::MutexGuard<'_, HashSet<String>> {
        self.contacts
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    /// Replace the saved-contact set (startup load from the contacts store).
    pub fn set_contacts(&self, node_ids: impl IntoIterator<Item = String>) {
        *self.lock_contacts() = node_ids.into_iter().collect();
    }

    pub fn add_contact(&self, node_id: &str) {
        self.lock_contacts().insert(node_id.to_string());
    }

    pub fn remove_contact(&self, node_id: &str) {
        self.lock_contacts().remove(node_id);
    }

    fn is_contact(&self, node_id: &str) -> bool {
        self.lock_contacts().contains(node_id)
    }

    /// Mark whether we currently intend to accept inbound call-media dials —
    /// set on create_call/join_call, cleared when the call is cancelled
    /// before anyone joined or once the last peer leaves. See
    /// `setup_connection`'s inbound gate.
    pub fn set_call_session_active(&self, active: bool) {
        self.call_session_active.store(active, Ordering::SeqCst);
        if !active {
            self.pending_invitee.lock().unwrap().take();
        }
    }

    /// Handles a CallDecline from `peer_id`: closes the call session only if
    /// it answers our outstanding invite and no call peer is connected.
    async fn handle_call_decline(&self, peer_id: &str) {
        let answers_invite = self.pending_invitee.lock().unwrap().as_deref() == Some(peer_id);
        if answers_invite && self.peer_count().await == 0 {
            self.set_call_session_active(false);
        }
    }

    fn is_call_session_active(&self) -> bool {
        self.call_session_active.load(Ordering::SeqCst)
    }

    /// Records `id` as seen for `peer_id` and returns whether it was already
    /// present (i.e. this is a duplicate delivery).
    async fn record_and_check_duplicate_dm_id(&self, peer_id: &str, id: &str) -> bool {
        self.recent_dm_ids
            .lock()
            .await
            .check_and_insert(peer_id, id)
    }

    pub async fn set_endpoint(&self, endpoint: iroh::Endpoint) {
        *self.endpoint.lock().await = Some(endpoint);
    }

    pub async fn set_presence(&self, presence: Arc<crate::presence::PresenceManager>) {
        *self.presence.lock().await = Some(presence);
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
        candidate_connection_id: usize,
        reason: &'static [u8],
    ) {
        let evicted = Self::cleanup_peer_entry(
            peer_id,
            &self.peers,
            &self.dm_peers,
            &self.peer_tickets,
            &self.event_tx,
            Some(reason),
            Some(expected_connection_id),
            true, // reconnecting peer — keep its ticket
            false,
        )
        .await;
        if evicted {
            self.replacing_peers
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .insert(peer_id.to_string(), candidate_connection_id);
            self.emit_peer_connection_status(
                peer_id,
                PeerConnectionKind::Reconnecting,
                Some("replacing stale connection".to_string()),
            );
        }
    }

    async fn should_accept_call_connection(
        &self,
        peer_id: &str,
        direction: ConnectionDirection,
        candidate_connection_id: usize,
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
            self.evict_stale_call_peer(
                peer_id,
                existing_conn_id,
                candidate_connection_id,
                b"stale_call_connection",
            )
            .await;
            return true;
        }

        // Gossip presence saw the peer rejoin after this entry was established
        // — the remote restarted and its old QUIC connection is dead on its
        // side. Prefer the new inbound and evict the stale entry instead of
        // rejecting it via the lexicographic tiebreak. Same trigger as the DM
        // path: a NeighborUp newer than the entry. (A merely *recent*
        // NeighborUp is not evidence — initial presence is recent too.)
        if matches!(direction, ConnectionDirection::Inbound)
            && self.call_entry_predates_recent_rejoin(peer_id).await
        {
            self.evict_stale_call_peer(
                peer_id,
                existing_conn_id,
                candidate_connection_id,
                b"peer_rejoined_gossip",
            )
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
            self.evict_stale_call_peer(
                peer_id,
                existing_conn_id,
                candidate_connection_id,
                b"superseded_by_redial",
            )
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

    /// Puts a re-presented DM stream's send half into the existing entry's
    /// slot for the same connection. Returns `DrainDuplicate` if the slot is
    /// still occupied, and `CloseDuplicate` if the entry was cleaned up while
    /// no map lock was held (the stream would otherwise be stranded on an
    /// orphaned slot, silently dropping DMs).
    async fn reattach_dm_send_stream(
        &self,
        peer_id: &str,
        existing_send: Arc<Mutex<Option<SendStream>>>,
        new_send: Option<SendStream>,
    ) -> DmStreamRegistration {
        {
            let mut slot = existing_send.lock().await;
            if slot.is_some() {
                return DmStreamRegistration::DrainDuplicate;
            }
            *slot = new_send;
        }
        let still_registered = self
            .dm_peers
            .lock()
            .await
            .get(peer_id)
            .is_some_and(|entry| Arc::ptr_eq(&entry.dm_send, &existing_send));
        if !still_registered {
            if let Some(mut send) = existing_send.lock().await.take() {
                let _ = send.finish();
            }
            return DmStreamRegistration::CloseDuplicate;
        }
        self.dm_connect_done.notify_waiters();
        DmStreamRegistration::Stored
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
            if let Some(existing) = dm_peers
                .get(peer_id)
                .filter(|existing| existing.connection.stable_id() == connection.stable_id())
            {
                if ownership == DmConnectionOwnership::SharedCall {
                    return DmStreamRegistration::CloseDuplicate;
                }
                // Same underlying QUIC connection re-presented its DM stream.
                // Never await a dm_send lock while holding dm_peers (a writer
                // may hold it for up to DM_WRITE_TIMEOUT, stalling every DM
                // path): clone the slot, release the map, then lock it.
                let existing_send = existing.dm_send.clone();
                drop(dm_peers);
                let new_send = dm_send.lock().await.take();
                return self
                    .reattach_dm_send_stream(peer_id, existing_send, new_send)
                    .await;
            }
            let should_insert = match dm_peers.get(peer_id) {
                None => true,
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
        let connection_id = connection.stable_id();
        let result = self
            .setup_connection_inner(peer_id.clone(), connection, direction)
            .await;
        // Only the attempt that performed the eviction may resolve it.
        let was_replacing = {
            let mut replacing = self
                .replacing_peers
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            let ours = replacing.get(&peer_id) == Some(&connection_id);
            if ours {
                replacing.remove(&peer_id);
            }
            ours
        };
        if was_replacing && !self.call_peer_connected(&peer_id).await {
            // The old connection was evicted but the replacement never made
            // it in: now the peer really is gone.
            let _ = self.event_tx.send(Event::PeerDisconnected {
                peer_id: peer_id.clone(),
            });
            self.emit_peer_connection_status(
                &peer_id,
                PeerConnectionKind::Disconnected,
                Some("replacement connection failed".to_string()),
            );
        }
        result
    }

    async fn setup_connection_inner(
        &self,
        peer_id: String,
        connection: Connection,
        direction: ConnectionDirection,
    ) -> Result<()> {
        // Ghost-call protocol fix: a call connection with no locally active
        // call session (never created/joined one, or it was cancelled/ended)
        // is rejected outright — regardless of any per-peer duplicate/replace
        // logic below. This applies to outbound connections too: a mesh
        // auto-dial or reconnect that was in flight when the call ended must
        // not resurrect it. Re-checked under the peers lock right before
        // insertion (below), since stream setup awaits in between.
        // DM/presence/file streams don't go through this path at all.
        if !self.is_call_session_active() {
            tracing::info!(
                "Rejecting {direction:?} call connection with {peer_id}: no active call session"
            );
            connection.close(0u32.into(), b"call_not_active");
            return Ok(());
        }

        if !self
            .should_accept_call_connection(&peer_id, direction, connection.stable_id())
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

        let pending_keyframe = Arc::new(AtomicBool::new(false));
        let peer_conn = PeerConnection {
            connection: connection.clone(),
            direction,
            chat_send: Arc::new(Mutex::new(Some(chat_send))),
            control_send: Arc::new(Mutex::new(Some(control_send))),
            video_writer: PeerVideoWriter::new(pending_keyframe.clone()),
            requested_video_layer: Arc::new(AtomicU8::new(0)),
            pending_keyframe,
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
            // Last-moment gate, under the lock that end_call's teardown also
            // takes: the call may have ended while streams were being opened.
            if !self.is_call_session_active() {
                drop(peers);
                tracing::info!(
                    "Rejecting {direction:?} call connection with {peer_id}: call ended during setup"
                );
                peer_conn.connection.close(0u32.into(), b"call_not_active");
                return Ok(());
            }
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

        video_writer.spawn(peer_id.clone(), connection.clone());

        let _ = self.event_tx.send(Event::PeerConnected {
            peer_id: peer_id.clone(),
        });
        self.emit_peer_connection_status(&peer_id, PeerConnectionKind::Connected, None);

        Self::emit_quality_profile_if_changed(old_count, new_count, &self.event_tx);

        self.spawn_stream_receivers(peer_id.clone(), connection, direction);

        if let Some(announce_self) = self.latest_self_announce_action().await {
            let _ = self.send_control(&peer_id, &announce_self).await;
        }

        // Only relay tickets of peers that are in the call right now — the
        // cache also holds tickets of peers from earlier calls (kept for
        // redial), which must not leak into this one.
        let current_peer_ids: HashSet<String> = self.peers.lock().await.keys().cloned().collect();
        let stored_tickets: Vec<(String, String)> = {
            let tickets = self.peer_tickets.lock().await;
            tickets_to_announce(&tickets, &current_peer_ids, &peer_id)
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

    #[allow(clippy::too_many_arguments)]
    async fn cleanup_peer_internal(
        peer_id: &str,
        peers: &Arc<Mutex<HashMap<String, PeerConnection>>>,
        dm_peers: &Arc<Mutex<HashMap<String, DmPeerConnection>>>,
        peer_tickets: &Arc<Mutex<HashMap<String, PeerTicketRecord>>>,
        event_tx: &broadcast::Sender<Event>,
        close_reason: Option<&'static [u8]>,
        expected_connection_id: Option<usize>,
        preserve_ticket: bool,
    ) -> bool {
        Self::cleanup_peer_entry(
            peer_id,
            peers,
            dm_peers,
            peer_tickets,
            event_tx,
            close_reason,
            expected_connection_id,
            preserve_ticket,
            true,
        )
        .await
    }

    /// `announce_disconnect: false` is for evicting a stale connection that a
    /// replacement is about to take over — the peer isn't leaving.
    #[allow(clippy::too_many_arguments)]
    async fn cleanup_peer_entry(
        peer_id: &str,
        peers: &Arc<Mutex<HashMap<String, PeerConnection>>>,
        dm_peers: &Arc<Mutex<HashMap<String, DmPeerConnection>>>,
        peer_tickets: &Arc<Mutex<HashMap<String, PeerTicketRecord>>>,
        event_tx: &broadcast::Sender<Event>,
        close_reason: Option<&'static [u8]>,
        expected_connection_id: Option<usize>,
        preserve_ticket: bool,
        announce_disconnect: bool,
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

        // Keep the cached ticket when the connection merely dropped (QUIC idle
        // timeout) so the liveness/reconnect path can still re-dial the peer.
        // Forget it on a deliberate teardown (user ended the call, or the peer
        // was given up on after exhausting reconnect attempts) — even when the
        // entry is already gone (the close watcher may have removed it first),
        // or a stale ticket would be announced to every future call peer. A
        // connection-scoped cleanup that lost the race to a newer connection
        // must leave the ticket alone: it belongs to the live entry.
        if !preserve_ticket && (removed.is_some() || expected_connection_id.is_none()) {
            peer_tickets.lock().await.remove(peer_id);
        }

        let Some(peer) = removed else {
            return false;
        };

        if let Some(reason) = close_reason {
            peer.connection.close(0u32.into(), reason);
        }


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

        if announce_disconnect {
            let _ = event_tx.send(Event::PeerDisconnected {
                peer_id: peer_id.to_string(),
            });
            let _ = event_tx.send(Event::PeerConnectionStatusChanged {
                peer_id: peer_id.to_string(),
                status: PeerConnectionKind::Disconnected,
                reason: close_reason.map(|reason| String::from_utf8_lossy(reason).to_string()),
            });
            ConnectionManager::emit_quality_profile_if_changed(old_count, new_count, event_tx);
        }

        true
    }

    pub async fn handle_peer_announce(
        &self,
        sender_id: &str,
        announced_peer_id: String,
        ticket: String,
    ) {
        let endpoint_ticket = match crate::node::parse_external_ticket(&ticket) {
            Ok(endpoint_ticket) => endpoint_ticket,
            Err(e) => {
                tracing::warn!("Mesh: rejected ticket for {announced_peer_id}: {e}");
                return;
            }
        };
        let addr = endpoint_ticket.endpoint_addr().clone();

        // The ticket is what gets cached, relayed and dialed, so it — not the
        // peer-supplied id — decides who this announce is about. A mismatch
        // would let one peer poison another peer's cached ticket.
        if addr.id.to_string() != announced_peer_id {
            tracing::warn!(
                "Mesh: rejected announce from {sender_id}: ticket is for {}, not {announced_peer_id}",
                addr.id
            );
            return;
        }
        let is_self = {
            let guard = self.endpoint.lock().await;
            guard.as_ref().is_some_and(|ep| addr.id == ep.id())
        };
        if is_self {
            return;
        }

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
        // Per-connection: dies with this connection's receiver tasks.
        let video_state_uni = Arc::new(StdMutex::new(VideoReceiveState::default()));
        let event_tx_cleanup = self.event_tx.clone();
        let peer_id_uni = peer_id.clone();
        let connection_uni = connection.clone();
        let connection_id = connection.stable_id();
        let peers_ref_uni = peers_ref.clone();
        let manager_uni = self.clone();

        tokio::spawn(async move {
            loop {
                match connection_uni.accept_uni().await {
                    Ok(mut recv) => {
                        let peer_id = peer_id_uni.clone();
                        let audio_tx = audio_media_tx.clone();
                        let video_tx = video_media_tx.clone();
                        let video_state = video_state_uni.clone();
                        let peers_ref = peers_ref_uni.clone();
                        let manager = manager_uni.clone();
                        tokio::spawn(async move {
                            let mut type_buf = [0u8; 1];
                            if recv.read_exact(&mut type_buf).await.is_err() {
                                return;
                            }
                            match type_buf[0] {
                                STREAM_AUDIO => loop {
                                    match crate::messages::read_framed(
                                        &mut recv,
                                        MAX_CONTROL_FRAME_BYTES,
                                    )
                                    .await
                                    {
                                        Ok(Some(data)) => {
                                            if let Some(packet) = AudioDatagram::decode(&data) {
                                                Self::mark_peer_active_internal(
                                                    &peers_ref, &peer_id,
                                                )
                                                .await;
                                                let _ = audio_tx.send(AudioPacket {
                                                    peer_id: peer_id.clone(),
                                                    connection_id,
                                                    timestamp_ms: packet.timestamp_ms,
                                                    sequence: packet.sequence,
                                                    payload: packet.payload,
                                                });
                                            }
                                        }
                                        _ => break,
                                    }
                                },
                                STREAM_VIDEO => {
                                    // One frame per stream. A read error means
                                    // the sender abandoned it (reset) — the
                                    // reorder buffer handles the hole.
                                    // Bounded in size and time, so a peer
                                    // can't pin memory with stalled streams.
                                    let read = tokio::time::timeout(
                                        VIDEO_FRAME_READ_TIMEOUT,
                                        recv.read_to_end(
                                            MAX_VIDEO_FRAME_BYTES + VIDEO_FRAME_HEADER_LEN,
                                        ),
                                    )
                                    .await;
                                    let Ok(Ok(body)) = read else {
                                        let _ = recv.stop(0u32.into());
                                        return;
                                    };
                                    let Some((seq, timestamp_ms, payload)) =
                                        decode_video_frame(&body)
                                    else {
                                        return;
                                    };
                                    Self::mark_peer_active_internal(&peers_ref, &peer_id).await;
                                    let frame = ReceivedVideoFrame {
                                        seq,
                                        timestamp_ms,
                                        is_keyframe: is_keyframe(payload),
                                        payload: payload.to_vec(),
                                    };
                                    let now = std::time::Instant::now();
                                    let (ready, request_keyframe) = {
                                        let mut state = video_state
                                            .lock()
                                            .unwrap_or_else(|poison| poison.into_inner());
                                        let out = state.reorder.push(frame, now);
                                        let request = out.need_keyframe
                                            && state.last_keyframe_request.is_none_or(|at| {
                                                now.duration_since(at)
                                                    >= RECEIVER_KEYFRAME_REQUEST_INTERVAL
                                            });
                                        if request {
                                            state.last_keyframe_request = Some(now);
                                        }
                                        (out.ready, request)
                                    };
                                    for frame in ready {
                                        let _ = video_tx.send(VideoPacket {
                                            peer_id: peer_id.clone(),
                                            timestamp_ms: frame.timestamp_ms,
                                            payload: frame.payload,
                                        });
                                    }
                                    if request_keyframe {
                                        let _ = manager
                                            .send_control(
                                                &peer_id,
                                                &ControlAction::KeyframeRequest {
                                                    layer: VideoLayerRequest::High,
                                                },
                                            )
                                            .await;
                                    }
                                }
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
        let datagram_connection_id = connection_datagram.stable_id();
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
                                connection_id: datagram_connection_id,
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

            // Only a silent drop keeps the ticket for a later redial; an
            // explicit close (either side hung up) forgets it, so it isn't
            // re-announced to the peers of a future call.
            let preserve_ticket = matches!(close_reason, ConnectionError::TimedOut);
            Self::cleanup_peer_internal(
                &peer_id_closed,
                &peers_ref_closed,
                &dm_peers_ref_closed,
                &peer_tickets_ref,
                &event_tx_cleanup_closed,
                None,
                Some(connection_closed_id),
                preserve_ticket,
            )
            .await;
        });

        let peer_id_bi = peer_id.clone();
        let peers_ref_bi = peers_ref.clone();
        let manager_bi = self.clone();
        let connection_bi = connection.clone();
        tokio::spawn(async move {
            // A call connection carries exactly one chat, control and DM
            // stream from the peer; anything beyond that (or an unknown type)
            // would only buy the peer another reader task.
            let mut seen_stream_types: HashSet<u8> = HashSet::new();
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
                        if !accept_call_bi_stream_type(&mut seen_stream_types, type_buf[0]) {
                            tracing::warn!(
                                "Rejecting duplicate or unknown bi stream type {} from {peer_id}",
                                type_buf[0]
                            );
                            let _ = recv.stop(0u32.into());
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
        let max_frame_len = match stream_type {
            STREAM_CHAT => MAX_CHAT_FRAME_BYTES,
            STREAM_DM => MAX_DM_FRAME_BYTES,
            _ => MAX_CONTROL_FRAME_BYTES,
        };

        loop {
            match crate::messages::read_framed(&mut recv, max_frame_len).await {
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
                // A Reconnecting peer's connection is dead; sending would only
                // fail (and used to log a warning every 20 ms).
                .filter(|(_, peer)| peer.connection_status != PeerConnectionKind::Reconnecting)
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
                tracing::debug!("Audio datagram send failed for {peer_id}: {e}");
            }
        }
        Ok(())
    }

    pub async fn send_video_frame_all(&self, data: &[u8], timestamp: u64) -> Result<()> {
        if data.len() > MAX_VIDEO_FRAME_BYTES {
            // Receivers refuse it anyway; treat it as a lost frame so every
            // peer waits for (and we produce) the next keyframe.
            tracing::warn!("Dropping oversized video frame ({} bytes)", data.len());
            let peers = self.peers.lock().await;
            for peer in peers.values() {
                peer.video_writer.mark_gap();
                peer.pending_keyframe.store(true, Ordering::Relaxed);
            }
            return Ok(());
        }
        let (active, skipped): (Vec<PeerVideoWriter>, Vec<PeerVideoWriter>) = {
            let peers_guard = self.peers.lock().await;
            let mut active = Vec::with_capacity(peers_guard.len());
            let mut skipped = Vec::new();
            for peer in peers_guard.values() {
                // Skip peers the liveness ticker has flagged as silent
                // (Suspect/Reconnecting) — their connection may be dead — and
                // peers that asked us to stop (VideoQualityRequest{None}).
                let reachable = matches!(
                    peer.connection_status,
                    PeerConnectionKind::Connected | PeerConnectionKind::Connecting
                );
                let wanted = peer.requested_video_layer.load(Ordering::Relaxed)
                    != VideoLayerRequest::None.to_u8();
                if reachable && wanted {
                    active.push(peer.video_writer.clone());
                } else {
                    skipped.push(peer.video_writer.clone());
                }
            }
            (active, skipped)
        };

        // A skipped peer misses this frame, so whatever it gets next must be
        // a keyframe or it would decode garbage until the periodic IDR.
        for writer in skipped {
            writer.mark_gap();
        }
        if active.is_empty() {
            return Ok(());
        }
        let frame = PendingVideoFrame {
            timestamp_ms: timestamp,
            payload: Arc::new(data.to_vec()),
            is_keyframe: is_keyframe(data),
            queued_at: std::time::Instant::now(),
        };
        for writer in active {
            writer.enqueue(frame.clone());
        }
        Ok(())
    }

    /// Video frames each peer's writer skipped or abandoned since last call.
    pub async fn take_dropped_video_frames(&self) -> HashMap<String, u32> {
        let peers = self.peers.lock().await;
        peers
            .iter()
            .map(|(id, peer)| (id.clone(), peer.video_writer.take_dropped_frames()))
            .collect()
    }

    pub async fn peer_ids(&self) -> Vec<String> {
        self.peers.lock().await.keys().cloned().collect()
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

        write_frame_or_reset(&s, message.as_bytes(), CHAT_WRITE_TIMEOUT, "chat")
            .await
            .map_err(|e| anyhow::anyhow!("chat to {peer_id} failed: {e}"))
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
            if let Err(e) =
                write_frame_or_reset(&stream, message.as_bytes(), CHAT_WRITE_TIMEOUT, "chat").await
            {
                tracing::debug!("Chat to {peer_id} failed: {e}");
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

        // Concurrently: one peer with a stalled control stream must not hold
        // up everyone else's heartbeat (and the liveness pass after it).
        futures_util::future::join_all(
            peer_ids
                .iter()
                .map(|peer_id| self.send_control(peer_id, &ControlAction::Heartbeat)),
        )
        .await;
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

        // A peer that stopped reading would otherwise block this forever
        // (and every caller queued behind the stream lock).
        write_frame_or_reset(&s, &data, CONTROL_WRITE_TIMEOUT, "control")
            .await
            .map_err(|e| anyhow::anyhow!("control write to {peer_id} failed: {e}"))
    }

    pub async fn snapshot_network_stats(&self) -> Vec<NetworkPeerStats> {
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

                NetworkPeerStats {
                    peer_id: peer_id.clone(),
                    rtt_ms,
                    lost_packets,
                    lost_bytes,
                    datagram_send_buffer_space: peer.connection.datagram_send_buffer_space(),
                }
            })
            .collect()
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

        write_frame_or_reset(&s, data, DM_WRITE_TIMEOUT, "DM")
            .await
            .map_err(|e| anyhow::anyhow!("DM write to peer {peer_id} failed: {e}"))
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
            Ok(()) => {
                if matches!(message, DmMessage::CallInvite { .. }) {
                    *self.pending_invitee.lock().unwrap() = Some(peer_id.to_string());
                }
                Ok(())
            }
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
        // session active before anyone can dial in (or out) — without this,
        // the protocol-level ghost-call gate (setup_connection) rejects the
        // test connection below on both sides.
        mgr_a.set_call_session_active(true);
        mgr_b.set_call_session_active(true);

        let addr_a = node::parse_ticket(&node::generate_ticket(&endpoint_a))
            .unwrap()
            .endpoint_addr()
            .clone();
        let peer_id = mgr_b.connect_to_peer(&endpoint_b, addr_a).await.unwrap();

        (mgr_a, mgr_b, endpoint_a, endpoint_b, router_a, peer_id)
    }

    #[test]
    fn sanitize_file_name_strips_directories_and_leading_dots() {
        assert_eq!(sanitize_file_name("/etc/passwd"), "passwd");
        assert_eq!(sanitize_file_name("..\\..\\evil.exe"), "evil.exe");
        assert_eq!(sanitize_file_name("../.bashrc"), "bashrc");
        assert_eq!(sanitize_file_name("C:\\Windows\\win.ini"), "win.ini");
        assert_eq!(sanitize_file_name("report.pdf"), "report.pdf");
    }

    #[test]
    fn sanitize_file_name_replaces_windows_forbidden_and_control_chars() {
        assert_eq!(sanitize_file_name("a<b>c:d\"e|f?g*h.txt"), "a_b_c_d_e_f_g_h.txt");
        assert_eq!(sanitize_file_name("file.txt:stream"), "file.txt_stream");
        assert_eq!(sanitize_file_name("tab\there\u{7}.txt"), "tabhere.txt");
    }

    #[test]
    fn sanitize_file_name_strips_trailing_dots_and_spaces() {
        assert_eq!(sanitize_file_name("name.txt. . "), "name.txt");
        assert_eq!(sanitize_file_name("  spaced  "), "spaced");
    }

    #[test]
    fn sanitize_file_name_rejects_empty_names() {
        for name in ["", "   ", "...", ". .", "/", "dir/", "\\", "\u{1}\u{2}"] {
            assert_eq!(sanitize_file_name(name), "download", "input {name:?}");
        }
    }

    #[test]
    fn sanitize_file_name_rejects_windows_reserved_device_names() {
        for name in [
            "CON", "con", "Prn", "AUX", "nul", "COM1", "com9", "LPT1", "lpt9", "nul.txt",
            "CON.tar.gz", "aux .log", "com3.",
        ] {
            let sanitized = sanitize_file_name(name);
            assert!(
                !is_windows_reserved_name(&sanitized),
                "{name:?} sanitized to reserved {sanitized:?}"
            );
            assert!(sanitized.starts_with('_'), "{name:?} -> {sanitized:?}");
        }
        for name in ["COM0", "COM10", "LPT", "console.txt", "nullable", "auxiliary.md"] {
            assert_eq!(sanitize_file_name(name), name);
        }
    }

    fn next_transfer_failure(
        rx: &mut broadcast::Receiver<Event>,
    ) -> Option<(String, Option<String>)> {
        loop {
            match rx.try_recv() {
                Ok(Event::DmFileTransferFailed {
                    file_id, reason, ..
                }) => return Some((file_id, reason)),
                Ok(_) => continue,
                Err(_) => return None,
            }
        }
    }

    #[tokio::test]
    async fn file_start_from_non_contact_is_rejected_with_failure_event() {
        let (event_tx, mut rx) = broadcast::channel::<Event>(16);
        let mut active_files = HashMap::new();
        let start = DmMessage::FileStart {
            name: "x.bin".into(),
            size: 4,
            id: "t-stranger".into(),
        };

        let handled =
            handle_dm_file_message(&start, "stranger", false, &mut active_files, &event_tx).await;

        assert!(handled, "a stranger's file card must not be shown");
        assert!(active_files.is_empty());
        assert_eq!(
            next_transfer_failure(&mut rx),
            Some((
                "t-stranger".to_string(),
                Some("sender is not a contact".to_string())
            ))
        );
    }

    #[tokio::test]
    async fn too_many_concurrent_transfers_is_handled_and_reported() {
        let (event_tx, mut rx) = broadcast::channel::<Event>(64);
        let mut active_files = HashMap::new();
        for n in 0..16 {
            let start = DmMessage::FileStart {
                name: "x.bin".into(),
                size: 4,
                id: format!("t-busy-{n}"),
            };
            assert!(
                !handle_dm_file_message(&start, "peer", true, &mut active_files, &event_tx).await
            );
        }
        let overflow = DmMessage::FileStart {
            name: "x.bin".into(),
            size: 4,
            id: "t-busy-overflow".into(),
        };
        assert!(handle_dm_file_message(&overflow, "peer", true, &mut active_files, &event_tx).await);
        assert_eq!(
            next_transfer_failure(&mut rx).map(|(id, _)| id),
            Some("t-busy-overflow".to_string())
        );
        cleanup_active_dm_files(active_files, "peer", &event_tx).await;
    }

    #[tokio::test]
    async fn file_end_before_all_bytes_arrived_fails_and_deletes_temp_file() {
        let (event_tx, mut rx) = broadcast::channel::<Event>(16);
        let mut active_files = HashMap::new();
        let id = "t-short".to_string();
        let start = DmMessage::FileStart {
            name: "x.bin".into(),
            size: 8,
            id: id.clone(),
        };
        handle_dm_file_message(&start, "peer", true, &mut active_files, &event_tx).await;
        let temp_path = active_files[&id].temp_path.clone();
        let chunk = DmMessage::FileChunk {
            id: id.clone(),
            offset: 0,
            data: vec![1, 2, 3, 4],
        };
        handle_dm_file_message(&chunk, "peer", true, &mut active_files, &event_tx).await;

        let end = DmMessage::FileEnd { id: id.clone() };
        let handled =
            handle_dm_file_message(&end, "peer", true, &mut active_files, &event_tx).await;

        assert!(handled, "an incomplete FileEnd must not mark the card complete");
        assert!(!temp_path.exists());
        assert_eq!(
            next_transfer_failure(&mut rx),
            Some((id, Some("incomplete transfer".to_string())))
        );
    }

    #[test]
    fn call_connection_accepts_one_bi_stream_per_type() {
        let mut seen = HashSet::new();
        assert!(accept_call_bi_stream_type(&mut seen, STREAM_CHAT));
        assert!(accept_call_bi_stream_type(&mut seen, STREAM_CONTROL));
        assert!(accept_call_bi_stream_type(&mut seen, STREAM_DM));
        assert!(!accept_call_bi_stream_type(&mut seen, STREAM_CHAT));
        assert!(!accept_call_bi_stream_type(&mut seen, STREAM_CONTROL));
        assert!(!accept_call_bi_stream_type(&mut seen, STREAM_DM));
        assert!(!accept_call_bi_stream_type(&mut HashSet::new(), STREAM_VIDEO));
        assert!(!accept_call_bi_stream_type(&mut HashSet::new(), 0x7f));
    }

    #[test]
    fn dm_frame_cap_fits_a_full_file_chunk() {
        let chunk = DmMessage::FileChunk {
            id: "x".repeat(64),
            offset: u64::MAX,
            data: vec![255u8; 64 * 1024],
        };
        let encoded = serde_json::to_vec(&chunk).unwrap();
        assert!(encoded.len() <= MAX_DM_FRAME_BYTES, "{} bytes", encoded.len());
    }

    #[test]
    fn recent_dm_ids_evict_the_least_recently_active_peer() {
        let mut recent = RecentDmIds::default();
        for n in 0..RECENT_DM_PEERS_CAPACITY {
            assert!(!recent.check_and_insert(&format!("peer-{n}"), "m"));
        }
        // peer-0 becomes the most recently active; peer-1 is now the LRU.
        assert!(recent.check_and_insert("peer-0", "m"));
        assert!(!recent.check_and_insert("newcomer", "m"));
        assert_eq!(recent.peers.len(), RECENT_DM_PEERS_CAPACITY);
        assert!(!recent.peers.contains_key("peer-1"));
        assert!(recent.check_and_insert("peer-0", "m"), "peer-0 kept its ids");
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
    async fn announce_whose_ticket_is_for_another_peer_is_rejected() {
        let (event_tx, _) = broadcast::channel::<Event>(8);
        let (audio_tx, _) = broadcast::channel::<AudioPacket>(8);
        let (video_tx, _) = broadcast::channel::<VideoPacket>(8);
        let manager = test_manager(event_tx, audio_tx, video_tx);

        let victim_addr = EndpointAddr::from_parts(
            test_public_key(),
            [TransportAddr::Relay(node::RELAY_URL_PARSED.clone())],
        );
        let victim_id = victim_addr.id.to_string();
        let attacker_addr = EndpointAddr::from_parts(
            test_public_key(),
            [TransportAddr::Relay(node::RELAY_URL_PARSED.clone())],
        );
        let attacker_ticket = serialize_endpoint_addr(attacker_addr);

        manager
            .handle_peer_announce("sender", victim_id.clone(), attacker_ticket)
            .await;
        assert!(manager.peer_tickets.lock().await.is_empty());
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
    fn only_tickets_of_currently_connected_peers_are_announced() {
        let record = |ticket: &str| PeerTicketRecord {
            ticket: ticket.to_string(),
            last_updated_ms: 0,
            last_dial_failed_ms: None,
            dial_failures: 0,
        };
        let tickets = HashMap::from([
            ("in-call".to_string(), record("t-in-call")),
            ("past-call".to_string(), record("t-past-call")),
            ("newcomer".to_string(), record("t-newcomer")),
        ]);
        let connected = HashSet::from(["in-call".to_string(), "newcomer".to_string()]);

        let announced = tickets_to_announce(&tickets, &connected, "newcomer");

        assert_eq!(
            announced,
            vec![("in-call".to_string(), "t-in-call".to_string())]
        );
    }

    #[tokio::test]
    async fn deliberate_disconnect_forgets_ticket_even_without_peer_entry() {
        let (event_tx, _) = broadcast::channel::<Event>(8);
        let (audio_tx, _) = broadcast::channel::<AudioPacket>(8);
        let (video_tx, _) = broadcast::channel::<VideoPacket>(8);
        let manager = test_manager(event_tx, audio_tx, video_tx);

        // The close watcher already removed the entry (e.g. the remote hung up
        // first); the user's end_call must still forget the ticket.
        assert!(manager.upsert_peer_ticket("peer-a", "ticket-1").await);
        manager.disconnect_peer("peer-a").await.unwrap();
        assert!(!manager.peer_tickets.lock().await.contains_key("peer-a"));
    }

    #[tokio::test]
    async fn connection_scoped_cleanup_without_entry_keeps_ticket() {
        let (event_tx, _) = broadcast::channel::<Event>(8);
        let (audio_tx, _) = broadcast::channel::<AudioPacket>(8);
        let (video_tx, _) = broadcast::channel::<VideoPacket>(8);
        let manager = test_manager(event_tx, audio_tx, video_tx);

        // A stale connection's watcher that finds no matching entry must not
        // touch the ticket owned by whatever replaced it.
        assert!(manager.upsert_peer_ticket("peer-a", "ticket-1").await);
        let removed = ConnectionManager::cleanup_peer_internal(
            "peer-a",
            &manager.peers,
            &manager.dm_peers,
            &manager.peer_tickets,
            &manager.event_tx,
            None,
            Some(12345),
            false,
        )
        .await;
        assert!(!removed);
        assert!(manager.peer_tickets.lock().await.contains_key("peer-a"));
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
        mgr_b.set_call_session_active(true);
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
        mgr_b.set_call_session_active(true);
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
    async fn call_decline_only_closes_the_session_for_our_pending_invite() {
        let (event_tx, _) = broadcast::channel::<Event>(16);
        let (audio_tx, _) = broadcast::channel::<AudioPacket>(8);
        let (video_tx, _) = broadcast::channel::<VideoPacket>(8);
        let mgr = test_manager(event_tx, audio_tx, video_tx);

        mgr.set_call_session_active(true);
        *mgr.pending_invitee.lock().unwrap() = Some("callee".to_string());

        // A stray decline from someone we didn't invite leaves the gate open.
        mgr.handle_call_decline("someone-else").await;
        assert!(mgr.is_call_session_active());

        mgr.handle_call_decline("callee").await;
        assert!(!mgr.is_call_session_active());
        assert!(mgr.pending_invitee.lock().unwrap().is_none());
    }

    #[tokio::test]
    async fn video_frames_arrive_in_order_starting_at_a_keyframe() {
        let (event_tx_a, _) = broadcast::channel::<Event>(64);
        let (event_tx_b, _) = broadcast::channel::<Event>(64);
        let (mgr_a, mgr_b, endpoint_a, endpoint_b, router_a, _peer_id) =
            connected_call_pair(event_tx_a, event_tx_b).await;
        let mut video_rx = mgr_a.video_media_tx.subscribe();

        let key = |n: u8| vec![0, 0, 0, 1, 0x65, n];
        let delta = |n: u8| vec![0, 0, 0, 1, 0x41, n];
        // Deltas before any keyframe are useless to a fresh decoder: the
        // writer must hold them back (and ask the encoder for a keyframe).
        mgr_b.send_video_frame_all(&delta(0), 0).await.unwrap();
        assert!(mgr_b.consume_pending_keyframe_requests().await);

        let mut sent = Vec::new();
        for n in 1..=6u8 {
            let frame = if n == 1 { key(n) } else { delta(n) };
            mgr_b.send_video_frame_all(&frame, n as u64).await.unwrap();
            sent.push(frame);
            // Let each frame leave the single-slot queue before the next.
            tokio::time::sleep(Duration::from_millis(30)).await;
        }

        let mut received = Vec::new();
        timeout(Duration::from_secs(10), async {
            while received.len() < sent.len() {
                let packet = video_rx.recv().await.unwrap();
                received.push(packet.payload);
            }
        })
        .await
        .expect("all frames delivered");
        assert_eq!(received, sent);

        router_a.shutdown().await.ok();
        endpoint_b.close().await;
        endpoint_a.close().await;
    }

    #[tokio::test]
    async fn replacing_a_stale_connection_does_not_announce_a_disconnect() {
        let (event_tx_a, _) = broadcast::channel::<Event>(64);
        let (event_tx_b, _) = broadcast::channel::<Event>(64);
        let (mgr_a, _mgr_b, endpoint_a, endpoint_b, router_a, _peer_id) =
            connected_call_pair(event_tx_a.clone(), event_tx_b).await;
        let b_id = endpoint_b.id().to_string();
        let mut rx_a = event_tx_a.subscribe();

        // B redials in the same direction (as it does once it believes the
        // old path is dead): A replaces the entry.
        let addr_a = iroh::EndpointAddr::new(endpoint_a.id())
            .with_relay_url(node::RELAY_URL_PARSED.clone());
        let _redial = endpoint_b.connect(addr_a, node::NAFAQ_ALPN).await.unwrap();

        timeout(Duration::from_secs(10), async {
            loop {
                match rx_a.recv().await.unwrap() {
                    Event::PeerDisconnected { peer_id } if peer_id == b_id => {
                        panic!("replacement surfaced as a disconnect")
                    }
                    Event::PeerConnected { peer_id } if peer_id == b_id => break,
                    _ => {}
                }
            }
        })
        .await
        .expect("replacement connection announced");
        assert!(mgr_a.call_peer_connected(&b_id).await);

        router_a.shutdown().await.ok();
        endpoint_b.close().await;
        endpoint_a.close().await;
    }

    // ── Ghost-call protocol gate (wave 2) ───────────────────────────────

    #[tokio::test]
    async fn outbound_call_connection_rejected_without_active_call_session() {
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
        mgr_a.set_call_session_active(true);

        // mgr_b's call already ended (flag cleared) while this dial — e.g. a
        // late mesh auto-dial or reconnect — was in flight.
        let addr_a = iroh::EndpointAddr::new(endpoint_a.id())
            .with_relay_url(node::RELAY_URL_PARSED.clone());
        let connection = endpoint_b.connect(addr_a, node::NAFAQ_ALPN).await.unwrap();
        mgr_b
            .setup_connection(
                endpoint_a.id().to_string(),
                connection.clone(),
                ConnectionDirection::Outbound,
            )
            .await
            .unwrap();

        assert!(mgr_b.peers.lock().await.is_empty());
        assert!(connection.close_reason().is_some());

        router_a.shutdown().await.ok();
        endpoint_b.close().await;
        endpoint_a.close().await;
    }

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
