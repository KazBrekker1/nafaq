use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use futures_util::StreamExt;
use iroh::address_lookup::memory::MemoryLookup;
use iroh::{EndpointAddr, PublicKey};
use iroh_gossip::api::{Event as GossipEvent, GossipReceiver};
use iroh_gossip::net::Gossip;
use iroh_gossip::proto::TopicId;
use tokio::sync::{broadcast, Mutex};
use tokio::task::JoinHandle;

use crate::messages::Event;
use crate::node::RELAY_URL_PARSED;

const DOMAIN_SEPARATOR: &[u8] = b"nafaq-presence-v1";

/// Tracks an active gossip subscription to one contact-pair topic. The task is
/// a supervisor that re-subscribes on failure, so the entry stays valid for the
/// lifetime of the contact and is only torn down on `untrack_contact`.
struct ContactSubscription {
    task: JoinHandle<()>,
}

impl Drop for ContactSubscription {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub struct PresenceManager {
    gossip: Gossip,
    local_id: PublicKey,
    event_tx: broadcast::Sender<Event>,
    address_lookup: MemoryLookup,
    subscriptions: Arc<Mutex<HashMap<String, ContactSubscription>>>,
    online: Arc<Mutex<HashMap<String, bool>>>,
    recent_neighbor_ups: Arc<Mutex<HashMap<String, Instant>>>,
}

impl PresenceManager {
    pub fn new(
        gossip: Gossip,
        local_id: PublicKey,
        event_tx: broadcast::Sender<Event>,
        address_lookup: MemoryLookup,
    ) -> Self {
        Self {
            gossip,
            local_id,
            event_tx,
            address_lookup,
            subscriptions: Arc::new(Mutex::new(HashMap::new())),
            online: Arc::new(Mutex::new(HashMap::new())),
            recent_neighbor_ups: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Begin tracking presence for a contact. Idempotent.
    pub async fn track_contact(&self, remote_id_str: &str) -> Result<()> {
        let remote_id: PublicKey = remote_id_str
            .parse()
            .with_context(|| format!("invalid node id: {remote_id_str}"))?;

        if remote_id == self.local_id {
            return Ok(());
        }

        {
            let mut subs = self.subscriptions.lock().await;
            if let Some(existing) = subs.get(remote_id_str) {
                if !existing.task.is_finished() {
                    return Ok(());
                }
                // The supervisor task exited unexpectedly (e.g. panic). Drop the
                // dead entry and re-spawn below so presence self-heals instead of
                // staying silently blocked behind a stale map entry.
                subs.remove(remote_id_str);
            }
        }

        // Teach the endpoint how to dial this peer (project relay) so gossip's
        // bootstrap dial can resolve the bare NodeId. add_endpoint_info merges
        // with anything already known about the peer, so this is safe to call
        // repeatedly.
        let peer_addr = EndpointAddr::new(remote_id).with_relay_url(RELAY_URL_PARSED.clone());
        self.address_lookup.add_endpoint_info(peer_addr);

        let topic = derive_topic(&self.local_id, &remote_id);
        let gossip = self.gossip.clone();
        let remote_id_owned = remote_id_str.to_string();
        let event_tx = self.event_tx.clone();
        let online = self.online.clone();
        let recent_ups = self.recent_neighbor_ups.clone();
        let expected_neighbor = remote_id;

        let task = tokio::spawn(async move {
            supervise_subscription(
                gossip,
                topic,
                expected_neighbor,
                remote_id_owned,
                event_tx,
                online,
                recent_ups,
            )
            .await;
        });

        let mut subs = self.subscriptions.lock().await;
        subs.insert(remote_id_str.to_string(), ContactSubscription { task });

        Ok(())
    }

    /// Stop tracking presence for a contact. Idempotent.
    pub async fn untrack_contact(&self, remote_id_str: &str) {
        let removed = {
            let mut subs = self.subscriptions.lock().await;
            subs.remove(remote_id_str)
        };
        drop(removed); // aborts task via Drop

        let mut online = self.online.lock().await;
        if online.remove(remote_id_str).is_some() {
            let _ = self.event_tx.send(Event::PresenceChanged {
                peer_id: remote_id_str.to_string(),
                online: false,
            });
        }

        self.recent_neighbor_ups.lock().await.remove(remote_id_str);
    }

    /// Snapshot of current online state, for frontend hydration.
    pub async fn snapshot(&self) -> HashMap<String, bool> {
        self.online.lock().await.clone()
    }

    /// Returns the Instant of the most recent `NeighborUp` for this peer, if any.
    /// Used by the outbound DM path to detect stale DM entries that pre-date a remote restart.
    pub async fn last_neighbor_up(&self, remote_id_str: &str) -> Option<Instant> {
        self.recent_neighbor_ups
            .lock()
            .await
            .get(remote_id_str)
            .copied()
    }
}

/// Supervises a single contact-pair subscription. Subscribes, runs the event
/// loop until the gossip stream errors/lags/ends, then re-subscribes with
/// exponential backoff. Runs forever until the task is aborted (untrack).
/// This makes presence resilient to transient gossip stream failures, which
/// would otherwise silently and permanently disable presence + restart
/// detection for the peer.
async fn supervise_subscription(
    gossip: Gossip,
    topic: TopicId,
    expected_neighbor: PublicKey,
    remote_id_str: String,
    event_tx: broadcast::Sender<Event>,
    online: Arc<Mutex<HashMap<String, bool>>>,
    recent_ups: Arc<Mutex<HashMap<String, Instant>>>,
) {
    let mut backoff = Duration::from_secs(1);
    loop {
        match gossip.subscribe(topic, vec![expected_neighbor]).await {
            Ok(gossip_topic) => {
                backoff = Duration::from_secs(1);
                // The sender half must stay alive for the duration of the inner
                // loop to keep the subscription open; we never broadcast on it.
                let (_sender, receiver) = gossip_topic.split();
                run_subscription_loop(
                    remote_id_str.clone(),
                    expected_neighbor,
                    receiver,
                    event_tx.clone(),
                    online.clone(),
                    recent_ups.clone(),
                )
                .await;
                drop(_sender);
            }
            Err(e) => {
                tracing::warn!("gossip presence subscribe failed for {remote_id_str}: {e}");
            }
        }
        // We are detached from the mesh until the next subscribe succeeds —
        // fail safe by reporting the peer offline rather than leaving a stale
        // "online" hanging.
        mark_offline(&online, &event_tx, &remote_id_str).await;
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(Duration::from_secs(30));
    }
}

async fn mark_offline(
    online: &Arc<Mutex<HashMap<String, bool>>>,
    event_tx: &broadcast::Sender<Event>,
    remote_id_str: &str,
) {
    let changed = {
        let mut map = online.lock().await;
        map.insert(remote_id_str.to_string(), false) != Some(false)
    };
    if changed {
        let _ = event_tx.send(Event::PresenceChanged {
            peer_id: remote_id_str.to_string(),
            online: false,
        });
    }
}

async fn run_subscription_loop(
    remote_id_str: String,
    expected_neighbor: PublicKey,
    mut receiver: GossipReceiver,
    event_tx: broadcast::Sender<Event>,
    online: Arc<Mutex<HashMap<String, bool>>>,
    recent_ups: Arc<Mutex<HashMap<String, Instant>>>,
) {
    while let Some(event_result) = receiver.next().await {
        match event_result {
            Ok(GossipEvent::NeighborUp(id)) => {
                if id != expected_neighbor {
                    // A different node joined our pair-topic — shouldn't happen with a
                    // hash-derived bilateral topic, but ignore defensively.
                    tracing::warn!(
                        "unexpected neighbor {id} on pair-topic with {remote_id_str}; ignoring"
                    );
                    continue;
                }
                {
                    let mut ups = recent_ups.lock().await;
                    ups.insert(remote_id_str.clone(), Instant::now());
                }
                let changed = {
                    let mut map = online.lock().await;
                    let prev = map.insert(remote_id_str.clone(), true);
                    prev != Some(true)
                };
                if changed {
                    let _ = event_tx.send(Event::PresenceChanged {
                        peer_id: remote_id_str.clone(),
                        online: true,
                    });
                }
            }
            Ok(GossipEvent::NeighborDown(id)) => {
                if id != expected_neighbor {
                    continue;
                }
                let changed = {
                    let mut map = online.lock().await;
                    let prev = map.insert(remote_id_str.clone(), false);
                    prev != Some(false)
                };
                if changed {
                    let _ = event_tx.send(Event::PresenceChanged {
                        peer_id: remote_id_str.clone(),
                        online: false,
                    });
                }
            }
            Ok(GossipEvent::Received(_)) => {
                // No message-level payload yet; reserved for future use.
            }
            Ok(GossipEvent::Lagged) => {
                // The bounded gossip channel overflowed and dropped events — we
                // may have missed a NeighborDown (peer left) or NeighborUp.
                // Tear down and let the supervisor re-subscribe for a fresh mesh
                // view rather than trusting now-possibly-stale online state.
                tracing::warn!("gossip presence stream lagged for {remote_id_str}; resubscribing");
                return;
            }
            Err(e) => {
                tracing::warn!("gossip presence stream error for {remote_id_str}: {e}");
                return;
            }
        }
    }
    // Stream ended; the supervisor marks the peer offline and re-subscribes.
}

/// Derive a deterministic, symmetric topic id for the pair (a, b).
/// `derive_topic(a, b) == derive_topic(b, a)` for any a, b.
fn derive_topic(a: &PublicKey, b: &PublicKey) -> TopicId {
    let a_bytes = a.as_bytes();
    let b_bytes = b.as_bytes();
    let (low, high) = if a_bytes <= b_bytes {
        (a_bytes, b_bytes)
    } else {
        (b_bytes, a_bytes)
    };
    let mut hasher = blake3::Hasher::new();
    hasher.update(DOMAIN_SEPARATOR);
    hasher.update(&[0u8]);
    hasher.update(low);
    hasher.update(high);
    let hash = hasher.finalize();
    TopicId::from_bytes(*hash.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use iroh::SecretKey;

    fn random_key() -> PublicKey {
        SecretKey::generate().public()
    }

    #[test]
    fn topic_derivation_is_symmetric() {
        let a = random_key();
        let b = random_key();
        assert_eq!(derive_topic(&a, &b), derive_topic(&b, &a));
    }

    #[test]
    fn topic_derivation_differs_for_different_pairs() {
        let a = random_key();
        let b = random_key();
        let c = random_key();
        assert_ne!(derive_topic(&a, &b), derive_topic(&a, &c));
    }

    #[test]
    fn topic_derivation_includes_domain_separator() {
        // Sanity check: changing the domain separator changes the output.
        // This guards against accidental collision if someone hashes raw bytes elsewhere.
        let a = random_key();
        let b = random_key();
        let topic = derive_topic(&a, &b);
        let (low, high) = if a.as_bytes() <= b.as_bytes() {
            (a.as_bytes(), b.as_bytes())
        } else {
            (b.as_bytes(), a.as_bytes())
        };
        let mut raw = blake3::Hasher::new();
        raw.update(low);
        raw.update(high);
        let raw_topic = TopicId::from_bytes(*raw.finalize().as_bytes());
        assert_ne!(topic, raw_topic);
    }
}
