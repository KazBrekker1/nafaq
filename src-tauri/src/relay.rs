use std::sync::Arc;
use std::time::Duration;

use iroh::{Endpoint, EndpointAddr};
use iroh_tickets::endpoint::EndpointTicket;
use iroh_tickets::Ticket;
use tokio::sync::{broadcast, Mutex};

use crate::messages::{Event, RelayStatusKind};
use crate::node;

const ONLINE_TIMEOUT: Duration = Duration::from_secs(10);
const CHECK_INTERVAL: Duration = Duration::from_secs(15);
const INITIAL_RETRY_BACKOFF: Duration = Duration::from_secs(2);
const MAX_RETRY_BACKOFF: Duration = Duration::from_secs(60);

pub fn ticket_available(addr: &EndpointAddr) -> bool {
    !addr.addrs.is_empty()
}

pub async fn monitor_relay(
    endpoint: Endpoint,
    latest_ticket: Arc<Mutex<Option<String>>>,
    relay_status: Arc<Mutex<RelayStatusKind>>,
    event_tx: broadcast::Sender<Event>,
) {
    let node_id = endpoint.id().to_string();
    let mut consecutive_failures = 0_u32;
    let mut retry_backoff = INITIAL_RETRY_BACKOFF;

    publish_status(
        &relay_status,
        &event_tx,
        &node_id,
        RelayStatusKind::Connecting,
        false,
        Some(format!("Connecting to relay {}", node::RELAY_URL)),
    )
    .await;

    loop {
        match check_relay(&endpoint).await {
            Ok(ticket) => {
                consecutive_failures = 0;
                retry_backoff = INITIAL_RETRY_BACKOFF;
                refresh_ticket(&latest_ticket, &event_tx, ticket).await;
                publish_status(
                    &relay_status,
                    &event_tx,
                    &node_id,
                    RelayStatusKind::Online,
                    true,
                    None,
                )
                .await;
            }
            Err(message) => {
                consecutive_failures = consecutive_failures.saturating_add(1);
                clear_ticket(&latest_ticket).await;
                let status = if consecutive_failures == 1 {
                    RelayStatusKind::Degraded
                } else {
                    RelayStatusKind::Offline
                };
                publish_status(
                    &relay_status,
                    &event_tx,
                    &node_id,
                    status,
                    false,
                    Some(message),
                )
                .await;
            }
        }

        let sleep_for = if consecutive_failures == 0 {
            CHECK_INTERVAL
        } else {
            let current = retry_backoff;
            retry_backoff = retry_backoff
                .checked_mul(2)
                .unwrap_or(MAX_RETRY_BACKOFF)
                .min(MAX_RETRY_BACKOFF);
            current
        };

        tokio::select! {
            _ = tokio::time::sleep(sleep_for) => {}
            _ = endpoint.closed() => {
                clear_ticket(&latest_ticket).await;
                publish_status(
                    &relay_status,
                    &event_tx,
                    &node_id,
                    RelayStatusKind::Offline,
                    false,
                    Some("Endpoint closed".to_string()),
                )
                .await;
                break;
            }
        }
    }
}

async fn check_relay(endpoint: &Endpoint) -> Result<String, String> {
    tokio::time::timeout(ONLINE_TIMEOUT, endpoint.online())
        .await
        .map_err(|_| format!("Timed out waiting for relay {}", node::RELAY_URL))?;

    let addr = endpoint.addr();
    if !ticket_available(&addr) {
        return Err(format!(
            "Relay {} has not published a dialable address yet",
            node::RELAY_URL
        ));
    }

    Ok(EndpointTicket::new(addr).serialize())
}

async fn refresh_ticket(
    latest_ticket: &Arc<Mutex<Option<String>>>,
    event_tx: &broadcast::Sender<Event>,
    ticket: String,
) {
    let mut current = latest_ticket.lock().await;
    if current.as_deref() == Some(ticket.as_str()) {
        return;
    }

    *current = Some(ticket.clone());
    drop(current);

    let _ = event_tx.send(Event::TicketRefreshed { ticket });
}

async fn clear_ticket(latest_ticket: &Arc<Mutex<Option<String>>>) {
    *latest_ticket.lock().await = None;
}

async fn publish_status(
    relay_status: &Arc<Mutex<RelayStatusKind>>,
    event_tx: &broadcast::Sender<Event>,
    node_id: &str,
    status: RelayStatusKind,
    ticket_available: bool,
    message: Option<String>,
) {
    let mut current = relay_status.lock().await;
    if *current == status {
        return;
    }

    *current = status.clone();
    drop(current);

    let _ = event_tx.send(Event::RelayStatusChanged {
        status,
        relay_url: node::RELAY_URL.to_string(),
        node_id: node_id.to_string(),
        ticket_available,
        message,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use iroh::{EndpointAddr, SecretKey, TransportAddr};

    fn public_key() -> iroh::PublicKey {
        let mut rng = rand::rng();
        SecretKey::generate(&mut rng).public()
    }

    #[test]
    fn ticket_unavailable_without_transport_addresses() {
        let addr = EndpointAddr::new(public_key());
        assert!(!ticket_available(&addr));
    }

    #[test]
    fn ticket_available_with_relay_address() {
        let relay_url = node::RELAY_URL.parse().unwrap();
        let addr = EndpointAddr::from_parts(public_key(), [TransportAddr::Relay(relay_url)]);
        assert!(ticket_available(&addr));
    }

    #[test]
    fn ticket_available_with_ip_address() {
        let socket_addr = "127.0.0.1:12345".parse().unwrap();
        let addr = EndpointAddr::from_parts(public_key(), [TransportAddr::Ip(socket_addr)]);
        assert!(ticket_available(&addr));
    }
}
