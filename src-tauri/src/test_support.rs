#![cfg(test)]

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use iroh::protocol::Router;
use iroh::{Endpoint, SecretKey};
use iroh_gossip::net::Gossip;
use tokio::sync::{broadcast, Mutex};

use crate::connection::ConnectionManager;
use crate::messages::{AudioPacket, Event, VideoPacket};
use crate::node;
use crate::presence::PresenceManager;
use crate::protocol::{NafaqDmProtocol, NafaqProtocol};

/// A self-contained nafaq node: endpoint, connection manager, presence manager,
/// router with all three ALPNs (call, DM, gossip), and the event broadcast
/// channel. Mirrors the production lifecycle in `lib.rs` so scenario tests
/// exercise the same code paths as the running app.
pub struct TestNode {
    pub endpoint: Endpoint,
    pub mgr: Arc<ConnectionManager>,
    pub presence: Arc<PresenceManager>,
    pub router: Router,
    pub event_tx: broadcast::Sender<Event>,
    pub secret_key: SecretKey,
}

impl TestNode {
    pub async fn new() -> Result<Self> {
        let mut rng = rand::rng();
        let key = SecretKey::generate(&mut rng);
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

        let mgr = Arc::new(ConnectionManager::new(
            event_tx.clone(),
            audio_tx,
            video_tx,
            Arc::new(Mutex::new(None)),
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

        Ok(Self {
            endpoint,
            mgr,
            presence,
            router,
            event_tx,
            secret_key,
        })
    }

    pub fn node_id_str(&self) -> String {
        self.endpoint.id().to_string()
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
