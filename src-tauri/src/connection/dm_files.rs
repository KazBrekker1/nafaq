//! Receiving files over DM streams: reassembly into a temp file, validation
//! and saving into the downloads directory.

use super::*;

pub(super) struct ActiveFileReceive {
    pub(super) file: tokio::fs::File,
    pub(super) temp_path: std::path::PathBuf,
    pub(super) final_name: String,
    pub(super) expected_size: u64,
    pub(super) received_bytes: u64,
    /// Progress last reported to the frontend, for throttling.
    pub(super) last_progress_bytes: u64,
    pub(super) last_progress_at: std::time::Instant,
}

/// Emit `DmFileProgress` at most every this many bytes...
pub(super) const FILE_PROGRESS_MIN_BYTES: u64 = 1024 * 1024;
/// ...or this often, whichever comes first; the first and final ones are
/// always sent.
/// A 64 KiB chunk per event would overflow the 256-slot event broadcast on a
/// large transfer and make the forwarder drop unrelated events.
pub(super) const FILE_PROGRESS_MIN_INTERVAL: Duration = Duration::from_millis(100);

pub(super) fn should_emit_file_progress(
    received: u64,
    expected: u64,
    last_bytes: u64,
    since_last: Duration,
) -> bool {
    last_bytes == 0
        || received >= expected
        || received.saturating_sub(last_bytes) >= FILE_PROGRESS_MIN_BYTES
        || since_last >= FILE_PROGRESS_MIN_INTERVAL
}

/// Maximum size a peer may declare for an inbound transfer. Mirrors the
/// sender-side cap in `send_file`; without this a peer can declare an
/// arbitrarily large file and legitimately stream it until the disk fills.
pub(super) const MAX_INCOMING_FILE_SIZE: u64 = 100 * 1024 * 1024;

/// Transfer ids are echoed to the frontend and used as map keys; anything
/// outside a UUID-ish charset is rejected as a protocol violation.
pub(super) fn is_valid_transfer_id(id: &str) -> bool {
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
pub(super) fn sanitize_file_name(name: &str) -> String {
    const FALLBACK: &str = "download";
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let cleaned: String = base
        .chars()
        .filter(|c| !c.is_control())
        .map(|c| {
            if matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*') {
                '_'
            } else {
                c
            }
        })
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
pub(super) fn is_windows_reserved_name(name: &str) -> bool {
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
pub(super) fn unique_file_path(dir: &std::path::Path, name: &str) -> std::path::PathBuf {
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
pub(super) fn emit_file_transfer_failed(
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

/// Recently finished outgoing transfers remembered for a late `FileReject`
/// (the receiver can still fail to save after our `FileEnd`).
const RECENT_FILE_SENDS_CAPACITY: usize = 64;
/// Cap on a peer-supplied rejection reason echoed to the frontend.
const MAX_FILE_REJECT_REASON_CHARS: usize = 200;

/// Sender-side record of outgoing file transfers.
#[derive(Default)]
pub(super) struct OutgoingFileSends {
    /// In-flight transfer id → (receiving peer, rejection reason once the
    /// receiver rejected it).
    active: HashMap<String, (String, Option<String>)>,
    /// Recently finished transfers as (peer, id).
    recent: VecDeque<(String, String)>,
}

/// Registers an outgoing transfer for its lifetime (see
/// `ConnectionManager::begin_file_send`).
pub struct FileSendGuard {
    sends: Arc<StdMutex<OutgoingFileSends>>,
    id: String,
}

impl Drop for FileSendGuard {
    fn drop(&mut self) {
        let mut sends = self
            .sends
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if let Some((peer_id, rejected)) = sends.active.remove(&self.id) {
            if rejected.is_none() {
                sends.recent.push_back((peer_id, self.id.clone()));
                if sends.recent.len() > RECENT_FILE_SENDS_CAPACITY {
                    sends.recent.pop_front();
                }
            }
        }
    }
}

impl ConnectionManager {
    fn lock_file_sends(&self) -> std::sync::MutexGuard<'_, OutgoingFileSends> {
        self.file_sends
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    /// Tracks outgoing transfer `id` to `peer_id` until the guard drops, so a
    /// `FileReject` from that peer can cancel it.
    pub fn begin_file_send(&self, peer_id: &str, id: &str) -> FileSendGuard {
        // Rejections arrive keyed by the connection's canonical remote id.
        let peer_id = canonical_node_id(peer_id).unwrap_or_else(|| peer_id.to_string());
        self.lock_file_sends()
            .active
            .insert(id.to_string(), (peer_id, None));
        FileSendGuard {
            sends: self.file_sends.clone(),
            id: id.to_string(),
        }
    }

    /// The receiver's reason if it rejected outgoing transfer `id`; the
    /// sender must stop streaming it.
    pub fn file_send_rejection(&self, id: &str) -> Option<String> {
        self.lock_file_sends()
            .active
            .get(id)
            .and_then(|(_, rejected)| rejected.clone())
    }

    /// A receiver rejected our transfer `id`. Only honoured from the peer we
    /// are sending it to (another peer must not cancel it): marks it
    /// cancelled for `send_file`'s chunk loop and reports it failed. A
    /// rejection of a just-finished transfer (e.g. the receiver couldn't
    /// save it) still reports the failure.
    pub(super) fn handle_file_reject(&self, peer_id: &str, id: &str, reason: &str) {
        let reason: String = reason
            .chars()
            .filter(|c| !c.is_control())
            .take(MAX_FILE_REJECT_REASON_CHARS)
            .collect();
        let report = {
            let mut sends = self.lock_file_sends();
            match sends.active.get_mut(id) {
                Some((to, rejected)) if to == peer_id => rejected.replace(reason.clone()).is_none(),
                Some(_) => false,
                None => {
                    let before = sends.recent.len();
                    sends
                        .recent
                        .retain(|(to, sent)| !(to == peer_id && sent == id));
                    sends.recent.len() != before
                }
            }
        };
        if report {
            tracing::info!("{peer_id} rejected file transfer {id}: {reason}");
            emit_file_transfer_failed(&self.event_tx, peer_id, id, &reason);
        } else {
            tracing::debug!("Ignoring FileReject for unknown transfer {id} from {peer_id}");
        }
    }
}

/// Where completed transfers are saved.
pub(super) fn downloads_dir() -> std::path::PathBuf {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map(|h| std::path::PathBuf::from(h).join("Downloads"))
        .unwrap_or_else(|_| std::env::temp_dir())
}

/// Move a completed temp file to its final location, falling back to copy +
/// remove when rename fails (e.g. across filesystems).
pub(super) async fn save_received_file(
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

/// What `handle_dm_file_message` did with a frame.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct FileFrameOutcome {
    /// Don't emit the frame as `DmReceived` (it was consumed or rejected).
    pub(super) skip_dm_event: bool,
    /// The transfer failed on our side: tell the sender with a `FileReject`
    /// carrying this `(id, reason)` so it stops streaming.
    pub(super) reject: Option<(String, &'static str)>,
}

impl FileFrameOutcome {
    const EMIT: Self = Self {
        skip_dm_event: false,
        reject: None,
    };
    const SKIP: Self = Self {
        skip_dm_event: true,
        reject: None,
    };

    /// The transfer failed here: mark the local card failed, suppress the
    /// frame, and reject it to the sender.
    fn failed(
        event_tx: &broadcast::Sender<Event>,
        peer_id: &str,
        id: &str,
        reason: &'static str,
    ) -> Self {
        emit_file_transfer_failed(event_tx, peer_id, id, reason);
        Self {
            skip_dm_event: true,
            reject: Some((id.to_string(), reason)),
        }
    }
}

/// Process a single DM message, handling file reconstruction when appropriate.
pub(super) async fn handle_dm_file_message(
    dm_msg: &DmMessage,
    peer_id: &str,
    sender_is_contact: bool,
    active_files: &mut HashMap<String, ActiveFileReceive>,
    event_tx: &broadcast::Sender<Event>,
) -> FileFrameOutcome {
    use tokio::io::{AsyncSeekExt, AsyncWriteExt};

    match dm_msg {
        DmMessage::FileStart { name, size, id } => {
            // Protocol-violation rejects suppress the DmReceived event entirely
            // so a malicious FileStart can't spam phantom file cards in the
            // frontend. An invalid id isn't echoed back in a FileReject.
            if !is_valid_transfer_id(id) {
                tracing::warn!("Rejecting file transfer from {peer_id}: invalid transfer id");
                return FileFrameOutcome::SKIP;
            }
            if *size > MAX_INCOMING_FILE_SIZE {
                tracing::warn!(
                    "Rejecting file transfer {id} from {peer_id}: declared size {size} exceeds {MAX_INCOMING_FILE_SIZE}"
                );
                return FileFrameOutcome {
                    skip_dm_event: true,
                    reject: Some((id.clone(), "file too large")),
                };
            }
            // Only saved contacts may write files to disk.
            if !sender_is_contact {
                tracing::warn!("Rejecting file transfer {id} from {peer_id}: not a contact");
                return FileFrameOutcome::failed(event_tx, peer_id, id, "sender is not a contact");
            }
            // Bound concurrent in-flight transfers so a peer can't exhaust file
            // descriptors / memory by opening unbounded FileStarts without ends.
            const MAX_CONCURRENT_TRANSFERS: usize = 16;
            if active_files.len() >= MAX_CONCURRENT_TRANSFERS && !active_files.contains_key(id) {
                tracing::warn!(
                    "Rejecting file transfer {id} from {peer_id}: too many concurrent transfers"
                );
                return FileFrameOutcome::failed(
                    event_tx,
                    peer_id,
                    id,
                    "too many concurrent transfers",
                );
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
                            last_progress_bytes: 0,
                            last_progress_at: std::time::Instant::now(),
                        },
                    );
                }
                Err(e) => {
                    tracing::warn!("Failed to create temp file for transfer {id}: {e}");
                    return FileFrameOutcome::failed(
                        event_tx,
                        peer_id,
                        id,
                        "could not create temp file",
                    );
                }
            }
            FileFrameOutcome::EMIT // still emit DmReceived so frontend shows the file
        }
        DmMessage::FileChunk { id, offset, data } => {
            // Chunks are persisted to disk here; the raw payload must NOT also
            // be re-emitted as a DmReceived IPC event (that floods the bridge
            // with the full file, e.g. ~68 MB for a 50 MB file). Chunks of an
            // unknown (never started, rejected or failed) transfer are
            // dropped without buffering.
            let Some(recv) = active_files.get_mut(id) else {
                tracing::debug!("FileChunk for unknown transfer {id}, ignoring");
                return FileFrameOutcome::SKIP;
            };
            // Reject writes that would extend the file past its declared size —
            // prevents a peer inflating an "8 KB" transfer into a disk-filling
            // write via large offsets.
            let failure = if offset.saturating_add(data.len() as u64) > recv.expected_size {
                tracing::warn!("FileChunk for {id} exceeds declared size; dropping transfer");
                Some("chunk exceeds declared size")
            } else {
                let written = match recv.file.seek(std::io::SeekFrom::Start(*offset)).await {
                    Ok(_) => recv.file.write_all(data).await,
                    Err(e) => Err(e),
                };
                match written {
                    Ok(()) => None,
                    Err(e) => {
                        tracing::warn!("Failed to write chunk for transfer {id}: {e}");
                        Some("could not write file")
                    }
                }
            };
            if let Some(reason) = failure {
                if let Some(recv) = active_files.remove(id) {
                    drop(recv.file);
                    let _ = tokio::fs::remove_file(&recv.temp_path).await;
                }
                return FileFrameOutcome::failed(event_tx, peer_id, id, reason);
            }
            recv.received_bytes = (*offset + data.len() as u64).max(recv.received_bytes);
            // Tiny, throttled progress ping (no payload) so the receiver's
            // progress bar advances without re-streaming the chunk.
            if should_emit_file_progress(
                recv.received_bytes,
                recv.expected_size,
                recv.last_progress_bytes,
                recv.last_progress_at.elapsed(),
            ) {
                recv.last_progress_bytes = recv.received_bytes;
                recv.last_progress_at = std::time::Instant::now();
                let _ = event_tx.send(Event::DmFileProgress {
                    peer_id: peer_id.to_string(),
                    file_id: id.clone(),
                    received: recv.received_bytes,
                });
            }
            FileFrameOutcome::SKIP
        }
        DmMessage::FileEnd { id } => {
            let Some(mut recv) = active_files.remove(id) else {
                tracing::debug!("FileEnd for unknown transfer {id}, ignoring");
                return FileFrameOutcome::EMIT;
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
                // Suppress DmReceived(FileEnd): it would mark the card complete.
                return FileFrameOutcome::failed(event_tx, peer_id, id, "incomplete transfer");
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
                return FileFrameOutcome::failed(event_tx, peer_id, id, "invalid file name");
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
                    FileFrameOutcome::EMIT
                }
                Err(e) => {
                    tracing::warn!("Failed to save file for transfer {id}: {e}");
                    let _ = tokio::fs::remove_file(&recv.temp_path).await;
                    FileFrameOutcome::failed(event_tx, peer_id, id, "could not save file")
                }
            }
        }
        _ => FileFrameOutcome::EMIT,
    }
}

pub(super) async fn cleanup_active_dm_files(
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

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(
            sanitize_file_name("a<b>c:d\"e|f?g*h.txt"),
            "a_b_c_d_e_f_g_h.txt"
        );
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
            "CON",
            "con",
            "Prn",
            "AUX",
            "nul",
            "COM1",
            "com9",
            "LPT1",
            "lpt9",
            "nul.txt",
            "CON.tar.gz",
            "aux .log",
            "com3.",
        ] {
            let sanitized = sanitize_file_name(name);
            assert!(
                !is_windows_reserved_name(&sanitized),
                "{name:?} sanitized to reserved {sanitized:?}"
            );
            assert!(sanitized.starts_with('_'), "{name:?} -> {sanitized:?}");
        }
        for name in [
            "COM0",
            "COM10",
            "LPT",
            "console.txt",
            "nullable",
            "auxiliary.md",
        ] {
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

        let outcome =
            handle_dm_file_message(&start, "stranger", false, &mut active_files, &event_tx).await;

        assert!(
            outcome.skip_dm_event,
            "a stranger's file card must not be shown"
        );
        assert_eq!(
            outcome.reject,
            Some(("t-stranger".to_string(), "sender is not a contact")),
            "the sender must be told"
        );
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
            assert_eq!(
                handle_dm_file_message(&start, "peer", true, &mut active_files, &event_tx).await,
                FileFrameOutcome::EMIT
            );
        }
        let overflow = DmMessage::FileStart {
            name: "x.bin".into(),
            size: 4,
            id: "t-busy-overflow".into(),
        };
        let outcome =
            handle_dm_file_message(&overflow, "peer", true, &mut active_files, &event_tx).await;
        assert!(outcome.skip_dm_event);
        assert_eq!(
            outcome.reject.map(|(id, _)| id),
            Some("t-busy-overflow".to_string())
        );
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
        let outcome =
            handle_dm_file_message(&end, "peer", true, &mut active_files, &event_tx).await;

        assert!(
            outcome.skip_dm_event,
            "an incomplete FileEnd must not mark the card complete"
        );
        assert!(!temp_path.exists());
        assert_eq!(
            next_transfer_failure(&mut rx),
            Some((id, Some("incomplete transfer".to_string())))
        );
    }

    #[tokio::test]
    async fn failed_transfer_is_rejected_and_later_chunks_are_dropped() {
        let (event_tx, mut rx) = broadcast::channel::<Event>(16);
        let mut active_files = HashMap::new();
        let id = "t-over".to_string();
        let start = DmMessage::FileStart {
            name: "x.bin".into(),
            size: 4,
            id: id.clone(),
        };
        handle_dm_file_message(&start, "peer", true, &mut active_files, &event_tx).await;
        let temp_path = active_files[&id].temp_path.clone();
        let chunk = |offset| DmMessage::FileChunk {
            id: id.clone(),
            offset,
            data: vec![0; 8],
        };

        let outcome =
            handle_dm_file_message(&chunk(0), "peer", true, &mut active_files, &event_tx).await;
        assert_eq!(
            outcome.reject,
            Some((id.clone(), "chunk exceeds declared size"))
        );
        assert!(active_files.is_empty());
        assert!(!temp_path.exists());
        assert_eq!(
            next_transfer_failure(&mut rx).map(|(file_id, _)| file_id),
            Some(id.clone())
        );

        // Chunks still in flight for the failed transfer are dropped, not
        // buffered, and not rejected again.
        let outcome =
            handle_dm_file_message(&chunk(8), "peer", true, &mut active_files, &event_tx).await;
        assert_eq!(outcome, FileFrameOutcome::SKIP);
        assert!(active_files.is_empty());
    }

    #[tokio::test]
    async fn file_reject_cancels_only_the_receivers_own_transfer() {
        let (event_tx, mut rx) = broadcast::channel::<Event>(16);
        let (audio_tx, _) = broadcast::channel(1);
        let (video_tx, _) = broadcast::channel(1);
        let manager =
            ConnectionManager::new(event_tx, audio_tx, video_tx, Arc::new(Mutex::new(None)));
        let receiver = iroh::SecretKey::generate().public().to_string();
        let other = iroh::SecretKey::generate().public().to_string();

        let guard = manager.begin_file_send(&receiver, "t-1");
        manager.handle_file_reject(&other, "t-1", "not yours to cancel");
        assert_eq!(manager.file_send_rejection("t-1"), None);
        assert_eq!(next_transfer_failure(&mut rx), None);

        manager.handle_file_reject(&receiver, "t-1", "sender is not a contact");
        assert_eq!(
            manager.file_send_rejection("t-1").as_deref(),
            Some("sender is not a contact")
        );
        assert_eq!(
            next_transfer_failure(&mut rx),
            Some((
                "t-1".to_string(),
                Some("sender is not a contact".to_string())
            ))
        );
        drop(guard);
        assert_eq!(manager.file_send_rejection("t-1"), None);

        // A rejection arriving after the transfer finished (e.g. the receiver
        // couldn't save it) is still reported, once.
        drop(manager.begin_file_send(&receiver, "t-2"));
        manager.handle_file_reject(&receiver, "t-2", "could not save file");
        manager.handle_file_reject(&receiver, "t-2", "could not save file");
        assert_eq!(
            next_transfer_failure(&mut rx).map(|(id, _)| id),
            Some("t-2".to_string())
        );
        assert_eq!(next_transfer_failure(&mut rx), None);
    }

    #[test]
    fn file_progress_is_throttled_but_final_progress_always_emits() {
        let short = Duration::from_millis(10);
        let mib = FILE_PROGRESS_MIN_BYTES;
        let last = 64 * 1024;
        // First progress always reports.
        assert!(should_emit_file_progress(last, 10 * mib, 0, short));
        // Small step, soon after the last event: suppressed.
        assert!(!should_emit_file_progress(2 * last, 10 * mib, last, short));
        // A full MiB since the last event.
        assert!(should_emit_file_progress(last + mib, 10 * mib, last, short));
        // Enough time passed.
        assert!(should_emit_file_progress(
            2 * last,
            10 * mib,
            last,
            FILE_PROGRESS_MIN_INTERVAL
        ));
        // Final chunk always reports.
        assert!(should_emit_file_progress(
            10 * mib,
            10 * mib,
            10 * mib - 1,
            short
        ));
    }
}
