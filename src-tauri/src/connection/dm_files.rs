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

/// Process a single DM message, handling file reconstruction when appropriate.
/// Returns `true` if the caller should `continue` (i.e. skip emitting DmReceived).
pub(super) async fn handle_dm_file_message(
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
                            last_progress_bytes: 0,
                            last_progress_at: std::time::Instant::now(),
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
                        // Tiny, throttled progress ping (no payload) so the
                        // receiver's progress bar advances without
                        // re-streaming the chunk.
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
        assert!(should_emit_file_progress(10 * mib, 10 * mib, 10 * mib - 1, short));
    }
}
