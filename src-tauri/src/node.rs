use anyhow::Result;
use iroh::{endpoint::presets, Endpoint, RelayMode, RelayUrl, SecretKey};
use iroh_tickets::endpoint::EndpointTicket;
use iroh_tickets::Ticket;
use std::sync::LazyLock;
use std::time::Duration;

/// Creates and configures an Iroh endpoint for the nafaq protocol.
pub const NAFAQ_ALPN: &[u8] = b"nafaq/call/1";
pub const NAFAQ_DM_ALPN: &[u8] = b"nafaq/dm/1";
pub const RELAY_URL: &str = "https://iroh-relay.sanad.ink";
pub static RELAY_URL_PARSED: LazyLock<RelayUrl> =
    LazyLock::new(|| RELAY_URL.parse().expect("invalid relay URL"));
const TICKET_ONLINE_TIMEOUT: Duration = Duration::from_secs(20);

#[allow(dead_code)]
pub async fn create_endpoint() -> Result<Endpoint> {
    create_endpoint_with_key(None).await
}

pub async fn create_endpoint_with_key(secret_key: Option<SecretKey>) -> Result<Endpoint> {
    use noq_proto::congestion::BbrConfig;
    use std::sync::Arc;

    let transport_config = iroh::endpoint::QuicTransportConfig::builder()
        .congestion_controller_factory(Arc::new(BbrConfig::default()))
        .keep_alive_interval(Duration::from_secs(5))
        .max_idle_timeout(Some(Duration::from_secs(30).try_into()?))
        .max_concurrent_uni_streams(1024_u32.into())
        .stream_receive_window((2 * 1024 * 1024_u32).into())
        .receive_window((8 * 1024 * 1024_u32).into())
        .send_window(8 * 1024 * 1024)
        .datagram_receive_buffer_size(Some(2 * 1024 * 1024))
        .datagram_send_buffer_size(2 * 1024 * 1024)
        .build();

    let relay_url = RELAY_URL_PARSED.clone();

    let mut builder = Endpoint::builder(presets::N0)
        .alpns(vec![NAFAQ_ALPN.to_vec(), NAFAQ_DM_ALPN.to_vec()])
        .transport_config(transport_config)
        .relay_mode(RelayMode::custom([relay_url]));

    if let Some(key) = secret_key {
        builder = builder.secret_key(key);
    }

    let endpoint = builder.bind().await?;

    // Wait for relay connection with a timeout — online() can hang indefinitely
    // if the relay's QUIC endpoint is unreachable (even if HTTP is up).
    // The relay will continue connecting in the background after timeout.
    match tokio::time::timeout(Duration::from_secs(10), endpoint.online()).await {
        Ok(_) => tracing::info!("Connected to relay"),
        Err(_) => tracing::warn!(
            "Timed out waiting for relay — continuing, but tickets will stay unavailable until {} is reachable",
            RELAY_URL
        ),
    }

    tracing::info!("Iroh endpoint started with ID: {}", endpoint.id());
    Ok(endpoint)
}

/// Generate a shareable ticket string from the endpoint's current address.
pub fn generate_ticket(endpoint: &Endpoint) -> String {
    let ticket = EndpointTicket::new(endpoint.addr());
    ticket.serialize()
}

pub async fn generate_ticket_when_online(endpoint: &Endpoint) -> Result<String> {
    tokio::time::timeout(TICKET_ONLINE_TIMEOUT, endpoint.online())
        .await
        .map_err(|_| {
            anyhow::anyhow!(
                "Relay not reachable yet at {RELAY_URL}. Check TCP 443 and UDP 7842, then retry."
            )
        })?;

    let addr = endpoint.addr();
    if addr.addrs.is_empty() {
        anyhow::bail!(
            "Relay is not publishing a dialable address yet for {RELAY_URL}. Retry in a moment."
        );
    }

    Ok(EndpointTicket::new(addr).serialize())
}

/// Parse a ticket string back into an EndpointTicket.
pub fn parse_ticket(ticket_str: &str) -> Result<EndpointTicket> {
    let ticket = EndpointTicket::deserialize(ticket_str)?;
    Ok(ticket)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_create_endpoint() {
        let endpoint = create_endpoint().await.unwrap();
        let id = endpoint.id();
        assert!(!id.to_string().is_empty());
        endpoint.close().await;
    }

    #[tokio::test]
    async fn test_ticket_roundtrip() {
        let endpoint = create_endpoint().await.unwrap();
        let ticket_str = generate_ticket(&endpoint);
        assert!(!ticket_str.is_empty());
        let ticket = parse_ticket(&ticket_str).unwrap();
        assert_eq!(ticket.endpoint_addr().id, endpoint.id().into());
        endpoint.close().await;
    }

    #[tokio::test]
    async fn test_generate_ticket_when_online() {
        let endpoint = create_endpoint().await.unwrap();
        let ticket_str = generate_ticket_when_online(&endpoint).await.unwrap();
        let ticket = parse_ticket(&ticket_str).unwrap();
        assert_eq!(ticket.endpoint_addr().id, endpoint.id().into());
        assert!(
            !ticket.endpoint_addr().addrs.is_empty(),
            "shareable ticket should contain at least one dialable address"
        );
        endpoint.close().await;
    }
}
