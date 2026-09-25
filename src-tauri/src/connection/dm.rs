//! Direct-message connections: dedicated DM connections (and DM streams
//! riding a call connection), their arbitration, dispatch and sending.

use super::*;

pub(super) const DM_DIAL_TIMEOUT: Duration = Duration::from_secs(12);
pub(super) const DM_DUPLICATE_DRAIN_TIMEOUT: Duration = Duration::from_secs(2);
pub(super) const DM_CONNECT_WAIT_TIMEOUT: Duration = Duration::from_secs(21);
/// A single DM frame write must complete in this long or the peer's flow
/// control window is stuck (e.g. it stopped reading). Without this,
/// `write_all` blocks forever and the frontend's message sits "sending"
/// indefinitely instead of surfacing a failure.
pub(super) const DM_WRITE_TIMEOUT: Duration = Duration::from_secs(10);

/// Call-signaling variants (CallInvite/CallDecline/CallCancel) are handled
/// before generic DmReceived/file processing and never reach it. Returns
/// `true` if the message was fully handled here.
///
/// CallDecline additionally clears our own `call_session_active` flag when it
/// answers our outstanding invite and nobody joined: our ticket must stop
/// accepting inbound call dials (see `setup_connection`'s gate) even though
/// no call peer was ever established to disconnect.
pub(super) async fn handle_call_signal(
    manager: &ConnectionManager,
    dm_msg: &DmMessage,
    peer_id: &str,
) -> bool {
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
pub(super) async fn handle_dm_frame_payload(
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
    if let DmMessage::FileReject { id, reason } = &dm_msg {
        manager.handle_file_reject(peer_id, id, reason);
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
    let outcome = handle_dm_file_message(
        &dm_msg,
        peer_id,
        sender_is_contact,
        active_files,
        &manager.event_tx,
    )
    .await;
    if let Some((id, reason)) = outcome.reject {
        // Tell the sender so it stops streaming (and doesn't report success).
        // Best effort: an older peer just drops the unknown frame.
        let reject = DmMessage::FileReject {
            id: id.clone(),
            reason: reason.to_string(),
        };
        if let Err(e) = manager.send_dm_frame_strict(peer_id, &reject).await {
            tracing::debug!("Failed to send FileReject for {id} to {peer_id}: {e}");
        }
    }
    if !outcome.skip_dm_event {
        let _ = manager.event_tx.send(Event::DmReceived {
            peer_id: peer_id.to_string(),
            message: dm_msg,
        });
    }
}

pub(super) async fn drain_duplicate_dm_frame_once(
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
pub(super) async fn run_dm_reader(
    recv: &mut iroh::endpoint::RecvStream,
    peer_id: &str,
    manager: &ConnectionManager,
) {
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
pub(super) enum DmConnectionOwnership {
    Dedicated,
    SharedCall,
}

/// Outcome of offering a DM connection or stream for registration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DmDecision {
    /// The candidate is (or may become) the peer's DM path.
    Store,
    /// An existing path wins; read one frame the remote may already have
    /// sent on the candidate so it isn't lost, then close it.
    DrainDuplicate,
    /// An existing path wins; close the candidate without reading.
    CloseDuplicate,
}

/// How a DM candidate on a new connection relates to a live existing entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DmArbitration {
    /// Same direction as the existing entry: the initiator re-dialed, which it
    /// only does once it considers the old path dead (e.g. its
    /// CONNECTION_CLOSE was lost). Newest wins; the old connection is closed.
    Redial,
    /// Crossed simultaneous dial and the candidate wins the tiebreak. The old
    /// connection's stream is finished gracefully: the remote resolves the
    /// same tiebreak the other way round.
    CandidateWins,
    /// Crossed simultaneous dial and the existing entry wins the tiebreak.
    ExistingWins,
    /// Our own id is unknown (endpoint not set yet), so the tiebreak can't be
    /// evaluated: keep what we have.
    Undecidable,
}

pub(super) fn arbitrate_dm_candidate(
    local_node_id: Option<&str>,
    remote_peer_id: &str,
    existing_direction: ConnectionDirection,
    candidate_direction: ConnectionDirection,
) -> DmArbitration {
    if existing_direction == candidate_direction {
        return DmArbitration::Redial;
    }
    match local_node_id {
        Some(local_id)
            if should_replace_connection(
                local_id,
                remote_peer_id,
                existing_direction,
                candidate_direction,
            ) =>
        {
            DmArbitration::CandidateWins
        }
        Some(_) => DmArbitration::ExistingWins,
        None => DmArbitration::Undecidable,
    }
}

pub(super) struct DmPeerConnection {
    pub(super) connection: Connection,
    pub(super) direction: ConnectionDirection,
    pub(super) dm_send: Arc<Mutex<Option<SendStream>>>,
    pub(super) ownership: DmConnectionOwnership,
    pub(super) established_at: std::time::Instant,
}

impl DmPeerConnection {
    pub(super) fn close_if_owned(&self, reason: &'static [u8]) {
        if self.ownership == DmConnectionOwnership::Dedicated {
            self.connection.close(0u32.into(), reason);
        }
    }

    pub(super) async fn finish_send_stream(&self) {
        let mut guard = self.dm_send.lock().await;
        if let Some(mut send) = guard.take() {
            let _ = send.finish();
        }
    }
}

pub(super) const RECENT_DM_IDS_CAPACITY: usize = 256;
/// Peers tracked in `RecentDmIds`; the least recently active is evicted, so
/// many distinct senders can't grow the map without bound.
pub(super) const RECENT_DM_PEERS_CAPACITY: usize = 256;

#[derive(Default)]
pub(super) struct RecentIds {
    order: VecDeque<String>,
    set: HashSet<String>,
    /// `RecentDmIds::clock` value at this peer's last message.
    last_used: u64,
}

/// Per-peer recent DM ids, bounded both per peer and in number of peers.
#[derive(Default)]
pub(super) struct RecentDmIds {
    pub(super) peers: HashMap<String, RecentIds>,
    pub(super) clock: u64,
}

impl RecentDmIds {
    /// Returns true if `id` was already seen from `peer_id`, otherwise records it.
    pub(super) fn check_and_insert(&mut self, peer_id: &str, id: &str) -> bool {
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
    pub(super) fn check_and_insert(&mut self, id: &str) -> bool {
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

impl ConnectionManager {
    pub(super) fn lock_contacts(&self) -> std::sync::MutexGuard<'_, HashSet<String>> {
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

    pub(super) fn is_contact(&self, node_id: &str) -> bool {
        self.lock_contacts().contains(node_id)
    }

    /// Records `id` as seen for `peer_id` and returns whether it was already
    /// present (i.e. this is a duplicate delivery).
    pub(super) async fn record_and_check_duplicate_dm_id(&self, peer_id: &str, id: &str) -> bool {
        self.recent_dm_ids
            .lock()
            .await
            .check_and_insert(peer_id, id)
    }

    /// True if the existing DM entry for `peer_id` pre-dates the most recent
    /// gossip NeighborUp signal for that peer. Indicates the entry is stale —
    /// QUIC's idle timeout hasn't fired yet on a connection whose remote half
    /// is already gone, but presence already told us the peer rebooted.
    pub(super) async fn dm_entry_predates_recent_rejoin(&self, peer_id: &str) -> bool {
        let entry_established = {
            let dm_peers = self.dm_peers.lock().await;
            dm_peers.get(peer_id).map(|p| p.established_at)
        };
        let Some(established_at) = entry_established else {
            return false;
        };
        let Some(presence) = self.presence.get() else {
            return false;
        };
        match presence.last_neighbor_up(peer_id).await {
            Some(up_at) => up_at > established_at,
            None => false,
        }
    }

    #[cfg(test)]
    pub(super) async fn reserve_dm_connecting(&self, peer_id: &str) -> bool {
        self.dm_connecting
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .insert(peer_id.to_string())
    }

    pub(super) fn reserve_dm_connecting_guard(
        &self,
        peer_id: &str,
    ) -> Option<ConnectingReservation> {
        ConnectingReservation::try_reserve(
            self.dm_connecting.clone(),
            peer_id,
            Some(self.dm_connect_done.clone()),
        )
    }

    #[cfg(test)]
    pub(super) async fn clear_dm_connecting(&self, peer_id: &str) {
        self.dm_connecting
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .remove(peer_id);
    }

    pub(super) fn dm_connect_in_progress(&self, peer_id: &str) -> bool {
        self.dm_connecting
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .contains(peer_id)
    }

    /// Evicts `peer_id`'s DM entry if it can no longer carry frames: its
    /// connection is closed (the closed() cleanup task may not have fired
    /// yet) or its send slot is empty (the stream was reset after a write
    /// timeout, or finished). Such an entry must never take part in
    /// arbitration — a live candidate could lose to it, leaving every later
    /// send failing with "stream is unavailable". The single rule shared by
    /// the inbound (`dm_connection_handling`) and store paths. An entry on
    /// `keep_connection_id` is left alone: that is the same connection
    /// re-presenting its stream (`reattach_dm_send_stream`). Returns whether
    /// an entry was evicted.
    pub(super) async fn evict_stale_dm_entry(
        &self,
        peer_id: &str,
        keep_connection_id: Option<usize>,
    ) -> bool {
        let probe = {
            let dm_peers = self.dm_peers.lock().await;
            dm_peers.get(peer_id).map(|existing| {
                (
                    existing.connection.stable_id(),
                    existing.connection.close_reason().is_some(),
                    existing.dm_send.clone(),
                )
            })
        };
        let Some((connection_id, conn_closed, dm_send)) = probe else {
            return false;
        };
        if keep_connection_id == Some(connection_id) {
            return false;
        }
        if !conn_closed && dm_send.lock().await.is_some() {
            return false;
        }
        self.cleanup_dm(peer_id, Some(b"stale_dm_connection"), Some(connection_id))
            .await
    }

    pub(super) async fn dm_connection_handling(
        &self,
        peer_id: &str,
        direction: ConnectionDirection,
    ) -> DmDecision {
        let local_node_id = self.local_node_id().await;

        if self.evict_stale_dm_entry(peer_id, None).await {
            return DmDecision::Store;
        }
        let existing_direction = {
            let dm_peers = self.dm_peers.lock().await;
            dm_peers.get(peer_id).map(|existing| existing.direction)
        };
        let Some(existing_direction) = existing_direction else {
            return DmDecision::Store;
        };

        // NeighborUp is stale-DM evidence only when it is newer than the stored
        // entry. Initial blank-state presence is also recent, so it must not
        // bypass crossed-dial arbitration.
        if matches!(direction, ConnectionDirection::Inbound)
            && self.dm_entry_predates_recent_rejoin(peer_id).await
        {
            self.cleanup_dm(peer_id, Some(b"peer_rejoined_gossip"), None)
                .await;
            return DmDecision::Store;
        }

        match arbitrate_dm_candidate(
            local_node_id.as_deref(),
            peer_id,
            existing_direction,
            direction,
        ) {
            DmArbitration::Redial => {
                // Rejecting it would strand both sides until the QUIC idle
                // timeout.
                self.cleanup_dm(peer_id, Some(b"superseded_by_redial"), None)
                    .await;
                DmDecision::Store
            }
            DmArbitration::CandidateWins => DmDecision::Store,
            DmArbitration::ExistingWins => DmDecision::DrainDuplicate,
            DmArbitration::Undecidable => DmDecision::CloseDuplicate,
        }
    }

    pub(super) async fn store_dm_peer_connection(
        &self,
        peer_id: &str,
        connection: Connection,
        direction: ConnectionDirection,
        dm_send: SendStream,
    ) -> DmDecision {
        self.store_dm_peer_connection_with_ownership(
            peer_id,
            connection,
            direction,
            dm_send,
            DmConnectionOwnership::Dedicated,
        )
        .await
    }

    pub(super) async fn register_dm_stream_on_call_connection(
        &self,
        peer_id: &str,
        connection: Connection,
        direction: ConnectionDirection,
        dm_send: SendStream,
    ) -> DmDecision {
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
    pub(super) async fn reattach_dm_send_stream(
        &self,
        peer_id: &str,
        existing_send: Arc<Mutex<Option<SendStream>>>,
        new_send: Option<SendStream>,
    ) -> DmDecision {
        {
            let mut slot = existing_send.lock().await;
            if slot.is_some() {
                return DmDecision::DrainDuplicate;
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
            return DmDecision::CloseDuplicate;
        }
        self.dm_connect_done.notify_waiters();
        DmDecision::Store
    }

    pub(super) async fn store_dm_peer_connection_with_ownership(
        &self,
        peer_id: &str,
        connection: Connection,
        direction: ConnectionDirection,
        dm_send: SendStream,
        ownership: DmConnectionOwnership,
    ) -> DmDecision {
        // Same rule as dm_connection_handling (the outbound connect_dm and
        // call-connection paths come straight here).
        self.evict_stale_dm_entry(peer_id, Some(connection.stable_id()))
            .await;

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
                    return DmDecision::CloseDuplicate;
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
            let arbitration = dm_peers.get(peer_id).map(|existing| {
                arbitrate_dm_candidate(
                    local_node_id.as_deref(),
                    peer_id,
                    existing.direction,
                    direction,
                )
            });
            let should_insert = match arbitration {
                None | Some(DmArbitration::Redial) => true,
                Some(DmArbitration::CandidateWins) => {
                    close_replaced_connection = false;
                    true
                }
                Some(DmArbitration::ExistingWins) => {
                    close_ignored_candidate = false;
                    false
                }
                Some(DmArbitration::Undecidable) => false,
            };

            if !should_insert {
                drop(dm_peers);
                tracing::info!(
                    "Ignoring duplicate {direction:?} DM stream for peer {peer_id}; existing connection wins"
                );
                if close_ignored_candidate {
                    dm_peer.close_if_owned(b"duplicate_dm_connection");
                    return DmDecision::CloseDuplicate;
                }
                return match ownership {
                    DmConnectionOwnership::Dedicated => DmDecision::DrainDuplicate,
                    DmConnectionOwnership::SharedCall => DmDecision::CloseDuplicate,
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
        DmDecision::Store
    }

    /// Remove a DM entry, optionally only if it still belongs to
    /// `connection_id`, closing it with `close_reason` if we own it. Returns
    /// whether an entry was removed.
    pub(super) async fn cleanup_dm(
        &self,
        peer_id: &str,
        close_reason: Option<&'static [u8]>,
        connection_id: Option<usize>,
    ) -> bool {
        let removed = {
            let mut dm_peers = self.dm_peers.lock().await;
            let should_remove = dm_peers.get(peer_id).is_some_and(|peer| {
                connection_id.is_none_or(|id| peer.connection.stable_id() == id)
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

        let _ = self.event_tx.send(Event::DmDisconnected {
            peer_id: peer_id.to_string(),
        });
        true
    }

    pub async fn handle_incoming_dm(&self, connection: Connection) -> Result<()> {
        let peer_id = connection.remote_id().to_string();
        tracing::info!("Setting up incoming DM connection from {peer_id}");
        self.setup_dm_connection(peer_id, connection, ConnectionDirection::Inbound)
            .await
    }

    pub(super) async fn setup_dm_connection(
        &self,
        peer_id: String,
        connection: Connection,
        direction: ConnectionDirection,
    ) -> Result<()> {
        let connection_handling = self.dm_connection_handling(&peer_id, direction).await;
        match connection_handling {
            DmDecision::Store => {}
            DmDecision::DrainDuplicate => {
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
            DmDecision::CloseDuplicate => {
                tracing::info!(
                    "Closing duplicate {direction:?} DM connection for peer {peer_id}; existing connection wins"
                );
                connection.close(0u32.into(), b"duplicate_dm_connection");
                return Ok(());
            }
        }

        self.spawn_dm_connection_tasks(peer_id, connection, direction, None);

        Ok(())
    }

    /// Spawns the tasks serving a dedicated DM connection: a reader for the
    /// DM stream we opened (`opened_stream`, outbound only), and an accept
    /// loop for peer-opened streams. On an inbound connection the peer's DM
    /// stream is registered as it arrives; on an outbound one we already
    /// registered our own, so a peer-opened DM stream is a duplicate — one
    /// frame is drained and the connection closed. Non-DM streams are
    /// ignored. When the loop ends the connection is closed (if it isn't
    /// already) and its DM entry cleaned up, exactly once.
    pub(super) fn spawn_dm_connection_tasks(
        &self,
        peer_id: String,
        connection: Connection,
        direction: ConnectionDirection,
        opened_stream: Option<RecvStream>,
    ) {
        let accepts_peer_stream = opened_stream.is_none();
        if let Some(mut recv) = opened_stream {
            let manager = self.clone();
            let peer_id = peer_id.clone();
            tokio::spawn(async move {
                run_dm_reader(&mut recv, &peer_id, &manager).await;
            });
        }

        let manager = self.clone();
        tokio::spawn(async move {
            let connection_id = connection.stable_id();
            while let Ok((send, mut recv)) = connection.accept_bi().await {
                let mut type_buf = [0u8; 1];
                if recv.read_exact(&mut type_buf).await.is_err() || type_buf[0] != STREAM_DM {
                    continue;
                }
                let decision = if accepts_peer_stream {
                    manager
                        .store_dm_peer_connection(&peer_id, connection.clone(), direction, send)
                        .await
                } else {
                    DmDecision::DrainDuplicate
                };
                match decision {
                    DmDecision::Store => {
                        let manager = manager.clone();
                        let peer_id = peer_id.clone();
                        tokio::spawn(async move {
                            run_dm_reader(&mut recv, &peer_id, &manager).await;
                        });
                    }
                    DmDecision::DrainDuplicate => {
                        drain_duplicate_dm_frame_once(&mut recv, &peer_id, &manager).await;
                        connection.close(0u32.into(), b"duplicate_dm_drained");
                        break;
                    }
                    DmDecision::CloseDuplicate => {
                        connection.close(0u32.into(), b"duplicate_dm_connection");
                        break;
                    }
                }
            }

            // Connection closed (or we just closed it) — the single cleanup
            // point for this connection's DM entry.
            connection.closed().await;
            manager
                .cleanup_dm(&peer_id, None, Some(connection_id))
                .await;
        });
    }

    pub async fn connect_dm(&self, node_id_str: &str) -> Result<()> {
        if self.dm_peer_connected(node_id_str).await {
            return Ok(());
        }
        let Some(_reservation) = self.reserve_dm_connecting_guard(node_id_str) else {
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

            let endpoint = self
                .endpoint
                .get()
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("Endpoint not initialized"))?;

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
                DmDecision::Store => {}
                DmDecision::DrainDuplicate => {
                    drain_duplicate_dm_frame_once(&mut dm_recv, &peer_id, self).await;
                    connection.close(0u32.into(), b"duplicate_dm_drained");
                    return Ok(());
                }
                DmDecision::CloseDuplicate => {
                    connection.close(0u32.into(), b"duplicate_dm_connection");
                    return Ok(());
                }
            }

            self.spawn_dm_connection_tasks(
                peer_id,
                connection,
                ConnectionDirection::Outbound,
                Some(dm_recv),
            );

            Ok(())
        }
        .await;

        if result.is_err() && self.dm_peer_connected(node_id_str).await {
            return Ok(());
        }

        result
    }

    pub(super) async fn wait_for_dm_connecting_to_finish(&self, peer_id: &str) -> Result<()> {
        with_timeout(
            DM_CONNECT_WAIT_TIMEOUT,
            format!("timed out waiting for DM connection to peer {peer_id}"),
            async {
                loop {
                    // Arm the notification BEFORE checking the condition so a
                    // state change between check and wait can't be missed.
                    let notified = self.dm_connect_done.notified();
                    if self.dm_peer_connected(peer_id).await
                        || !self.dm_connect_in_progress(peer_id)
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
                    self.cleanup_dm(peer_id, Some(b"dm_entry_predates_rejoin"), None)
                        .await;
                    continue;
                }
                return Ok(());
            }

            if self.dm_connect_in_progress(peer_id) {
                self.wait_for_dm_connecting_to_finish(peer_id).await?;
                continue;
            }

            match self.connect_dm(peer_id).await {
                Ok(()) => return Ok(()),
                Err(_) if self.dm_peer_connected(peer_id).await => return Ok(()),
                Err(err) if self.dm_connect_in_progress(peer_id) => {
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

    pub(super) async fn write_dm_frame(&self, peer_id: &str, data: &[u8]) -> Result<()> {
        let stream = {
            let dm_peers = self.dm_peers.lock().await;
            dm_peers
                .get(peer_id)
                .map(|p| (p.dm_send.clone(), p.connection.stable_id()))
        };
        let Some((s, connection_id)) = stream else {
            anyhow::bail!("DM peer {peer_id} is not connected");
        };

        let result = write_frame_or_reset(&s, data, DM_WRITE_TIMEOUT, "DM").await;
        if let Err(error) = &result {
            if error.is_stalled() {
                // The reset stream can't be reopened on this path. Retire it
                // now: closing a dedicated connection also makes the peer drop
                // its (otherwise still "live") entry, so our redial isn't
                // rejected by arbitration against it. A DM riding a call
                // connection only loses its entry; the peer notices the reset
                // on its reader (`handle_bi_stream`).
                self.cleanup_dm(peer_id, Some(b"dm_stream_stalled"), Some(connection_id))
                    .await;
            }
        }
        result.map_err(|e| anyhow::anyhow!("DM write to peer {peer_id} failed: {e}"))
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
                self.cleanup_dm(peer_id, Some(b"dm_write_failed"), active_connection_id)
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

    #[cfg(test)]
    pub async fn disconnect_dm(&self, peer_id: &str) {
        self.cleanup_dm(peer_id, Some(b"dm_closed"), None).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(
            recent.check_and_insert("peer-0", "m"),
            "peer-0 kept its ids"
        );
    }

    #[test]
    fn dm_arbitration_covers_redial_crossed_dial_and_unknown_local_id() {
        use ConnectionDirection::{Inbound, Outbound};
        // Same direction: always a redial, regardless of ids.
        assert_eq!(
            arbitrate_dm_candidate(Some("node-a"), "node-z", Inbound, Inbound),
            DmArbitration::Redial
        );
        assert_eq!(
            arbitrate_dm_candidate(None, "node-z", Outbound, Outbound),
            DmArbitration::Redial
        );
        // Crossed dial: the higher id keeps its outbound connection.
        assert_eq!(
            arbitrate_dm_candidate(Some("node-z"), "node-a", Inbound, Outbound),
            DmArbitration::CandidateWins
        );
        assert_eq!(
            arbitrate_dm_candidate(Some("node-z"), "node-a", Outbound, Inbound),
            DmArbitration::ExistingWins
        );
        assert_eq!(
            arbitrate_dm_candidate(None, "node-a", Outbound, Inbound),
            DmArbitration::Undecidable
        );
    }
}
