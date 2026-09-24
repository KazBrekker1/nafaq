#![cfg(test)]

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use iroh::protocol::Router;
use iroh::{Endpoint, SecretKey};
use iroh_gossip::net::Gossip;
use tokio::sync::{broadcast, Mutex};

use crate::connection::ConnectionManager;
use crate::messages::{AudioPacket, ControlAction, Event, VideoPacket};
use crate::node;
use crate::presence::PresenceManager;
use crate::protocol::{NafaqDmProtocol, NafaqProtocol};

/// A self-contained nafaq node: endpoint, connection manager, presence manager,
/// router with all three ALPNs (call, DM, gossip), and the event broadcast
/// channel. Mirrors the production lifecycle in `lib.rs` so scenario tests
/// exercise the same code paths as the running app — including the
/// PeerAnnounce/TicketRefreshed control dispatcher that `lib.rs` spawns
/// alongside the Tauri app (see `lib.rs`'s "Spawn PeerAnnounce +
/// VideoQualityRequest handler" task). Without that dispatcher, a received
/// `ControlAction::PeerAnnounce` would just sit in the event broadcast
/// unactioned and the mesh would never self-connect.
pub struct TestNode {
    pub endpoint: Endpoint,
    pub mgr: Arc<ConnectionManager>,
    pub presence: Arc<PresenceManager>,
    pub router: Router,
    pub event_tx: broadcast::Sender<Event>,
    pub secret_key: SecretKey,
    /// Mirrors `AppState::latest_ticket` — the ticket this node hands out
    /// when it acts as a call host. Set it via [`TestNode::create_call_ticket`].
    pub latest_ticket: Arc<Mutex<Option<String>>>,
}

impl TestNode {
    pub async fn new() -> Result<Self> {
        let key = SecretKey::generate();
        Self::with_key(key).await
    }

    pub async fn with_key(secret_key: SecretKey) -> Result<Self> {
        let node::NafaqEndpoint {
            endpoint,
            address_lookup,
        } = node::create_endpoint_with_key(secret_key.clone()).await?;

        let (event_tx, _) = broadcast::channel::<Event>(256);
        let (audio_tx, _) = broadcast::channel::<AudioPacket>(8);
        let (video_tx, _) = broadcast::channel::<VideoPacket>(8);

        let latest_ticket = Arc::new(Mutex::new(None));
        let mgr = Arc::new(ConnectionManager::new(
            event_tx.clone(),
            audio_tx,
            video_tx,
            latest_ticket.clone(),
        ));
        mgr.set_endpoint(endpoint.clone()).await;

        let gossip = Gossip::builder().spawn(endpoint.clone());
        let presence = Arc::new(PresenceManager::new(
            gossip.clone(),
            endpoint.id(),
            event_tx.clone(),
            address_lookup,
        ));
        mgr.set_presence(presence.clone()).await;

        let router = Router::builder(endpoint.clone())
            .accept(node::NAFAQ_ALPN, NafaqProtocol::new(mgr.clone()))
            .accept(node::NAFAQ_DM_ALPN, NafaqDmProtocol::new(mgr.clone()))
            .accept(iroh_gossip::ALPN, gossip)
            .spawn();

        // Mirrors the control dispatcher `lib.rs` spawns in `.setup()`: turns
        // a received PeerAnnounce into the mesh auto-dial/relay in
        // `ConnectionManager::handle_peer_announce`, and re-announces this
        // node to already-connected peers whenever its own ticket refreshes.
        // Inert for tests that never call `create_call_ticket` (no DM/presence
        // scenario test does), so this is safe to run unconditionally.
        let control_mgr = mgr.clone();
        let mut control_rx = event_tx.subscribe();
        tokio::spawn(async move {
            loop {
                match control_rx.recv().await {
                    Ok(Event::ControlReceived {
                        peer_id,
                        action:
                            ControlAction::PeerAnnounce {
                                peer_id: announced_id,
                                ticket,
                            },
                    }) => {
                        control_mgr
                            .handle_peer_announce(&peer_id, announced_id, ticket)
                            .await;
                    }
                    Ok(Event::TicketRefreshed { ticket }) => {
                        control_mgr.send_self_announce_to_all(ticket).await;
                    }
                    Ok(_) => {}
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });

        Ok(Self {
            endpoint,
            mgr,
            presence,
            router,
            event_tx,
            secret_key,
            latest_ticket,
        })
    }

    pub fn node_id_str(&self) -> String {
        self.endpoint.id().to_string()
    }

    /// Mirrors `commands::create_call`: declares this node ready to accept
    /// inbound call dials, then generates (or reuses) this node's own
    /// shareable ticket, stores it as `latest_ticket`, and — if it changed —
    /// emits `TicketRefreshed` so the control dispatcher re-announces this
    /// node to any peers it's already connected to. Call this before another
    /// node joins so the mesh can learn (and later relay) this node's
    /// address.
    pub async fn create_call_ticket(&self) -> Result<String> {
        // Must be set before the ticket is handed out, mirroring
        // commands::create_call — otherwise setup_connection rejects any
        // inbound call dial (including a mesh auto-dial) for this node.
        self.mgr.set_call_session_active(true);

        if let Some(ticket) = self.latest_ticket.lock().await.clone() {
            return Ok(ticket);
        }

        let ticket = node::generate_ticket_when_online(&self.endpoint).await?;

        let mut latest_ticket = self.latest_ticket.lock().await;
        if latest_ticket.as_deref() != Some(ticket.as_str()) {
            *latest_ticket = Some(ticket.clone());
            drop(latest_ticket);
            let _ = self.event_tx.send(Event::TicketRefreshed {
                ticket: ticket.clone(),
            });
        }

        Ok(ticket)
    }

    /// Graceful shutdown — sends QUIC CONNECTION_CLOSE frames. Mirrors a clean
    /// app quit. Use this when you want the remote side to learn promptly.
    pub async fn shutdown_graceful(self) {
        let _ = self.router.shutdown().await;
        self.endpoint.close().await;
    }
}

/// Wait for an event matching `pred` on `rx`. Returns `Some(event)` if one
/// arrives within `dur`, `None` otherwise. Tolerates broadcast lag (continues
/// listening rather than failing).
pub async fn wait_for_event<F>(
    rx: &mut broadcast::Receiver<Event>,
    dur: Duration,
    mut pred: F,
) -> Option<Event>
where
    F: FnMut(&Event) -> bool,
{
    tokio::time::timeout(dur, async {
        loop {
            match rx.recv().await {
                Ok(e) if pred(&e) => return Some(e),
                Ok(_) => continue,
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    })
    .await
    .ok()
    .flatten()
}
