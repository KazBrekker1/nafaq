use anyhow::Result;
use iroh::address_lookup::memory::MemoryLookup;
use iroh::{Endpoint, RelayMode, RelayUrl, SecretKey, TransportAddr};
use iroh_tickets::endpoint::EndpointTicket;
use iroh_tickets::Ticket;
use std::sync::LazyLock;
use std::time::Duration;

/// Creates and configures an Iroh endpoint for the nafaq protocol.
/// v2: one QUIC uni-stream per video frame (see video_transport.rs). Not
/// wire-compatible with v1 video, hence the bump.
pub const NAFAQ_ALPN: &[u8] = b"nafaq/call/2";
pub const NAFAQ_DM_ALPN: &[u8] = b"nafaq/dm/1";
pub const RELAY_URL: &str = "https://iroh-relay.sanad.ink";
pub static RELAY_URL_PARSED: LazyLock<RelayUrl> =
    LazyLock::new(|| RELAY_URL.parse().expect("invalid relay URL"));
const TICKET_ONLINE_TIMEOUT: Duration = Duration::from_secs(20);

pub struct NafaqEndpoint {
    pub endpoint: Endpoint,
    pub address_lookup: MemoryLookup,
}

pub async fn create_endpoint_with_key(secret_key: SecretKey) -> Result<NafaqEndpoint> {
    use noq_proto::congestion::Bbr3Config;
    use std::sync::Arc;

    let transport_config = iroh::endpoint::QuicTransportConfig::builder()
        .congestion_controller_factory(Arc::new(Bbr3Config::default()))
        // A dead path is noticed after 15 s (was 30 s) and handed to the
        // reconnect ladder; 3 s keep-alives keep a quiet-but-healthy
        // connection (muted, camera off) comfortably inside that window.
        .keep_alive_interval(Duration::from_secs(3))
        .max_idle_timeout(Some(Duration::from_secs(15).try_into()?))
        // Bound concurrent uni-streams at the QUIC layer. A peer's stream-open
        // blocks once it hits the cap, which in turn bounds how many
        // accept_uni/tokio::spawn reader tasks we can be pushed into — a peer
        // can't flood us into unbounded task growth. 256 is ample headroom for
        // pipelined video while far below the old effectively-unbounded value.
        // (Bidi streams are already bounded by quinn's default cap of 100, and
        // legitimate use needs only a few: chat/control/dm.)
        .max_concurrent_uni_streams(256_u32.into())
        .stream_receive_window((2 * 1024 * 1024_u32).into())
        .receive_window((8 * 1024 * 1024_u32).into())
        .send_window(8 * 1024 * 1024)
        // Audio is the only datagram traffic (~100 B every 20 ms). Keep the
        // queues to well under a second so a stall drops stale audio (oldest
        // first) instead of flushing minutes of it afterwards.
        .datagram_receive_buffer_size(Some(16 * 1024))
        .datagram_send_buffer_size(8 * 1024)
        .build();

    let relay_url = RELAY_URL_PARSED.clone();
    let address_lookup = MemoryLookup::new();

    // Minimal = mandatory options only (rustls crypto provider), no n0
    // discovery/relay defaults — the 0.98 equivalent of the old empty_builder.
    let endpoint = Endpoint::builder(iroh::endpoint::presets::Minimal)
        .alpns(vec![NAFAQ_ALPN.to_vec(), NAFAQ_DM_ALPN.to_vec()])
        .transport_config(transport_config)
        .relay_mode(RelayMode::custom([relay_url]))
        .address_lookup(address_lookup.clone())
        .secret_key(secret_key)
        .bind()
        .await?;

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
    Ok(NafaqEndpoint {
        endpoint,
        address_lookup,
    })
}

#[cfg(test)]
pub async fn create_test_endpoint() -> Result<Endpoint> {
    Ok(create_endpoint_with_key(SecretKey::generate())
        .await?
        .endpoint)
}

/// Generate a shareable ticket string from the endpoint's current address.
#[cfg(test)]
pub fn generate_ticket(endpoint: &Endpoint) -> String {
    let ticket = EndpointTicket::new(endpoint.addr());
    ticket.encode_string()
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

    Ok(EndpointTicket::new(addr).encode_string())
}

/// Validate that an endpoint address only references the project relay.
pub fn validate_project_relay_addr(addr: &iroh::EndpointAddr) -> Result<()> {
    for transport_addr in &addr.addrs {
        if let TransportAddr::Relay(url) = transport_addr {
            if url != &*RELAY_URL_PARSED {
                anyhow::bail!("ticket uses unsupported relay {url}");
            }
        }
    }

    Ok(())
}

/// Parse a ticket string back into an EndpointTicket.
pub fn parse_ticket(ticket_str: &str) -> Result<EndpointTicket> {
    let ticket = EndpointTicket::decode_string(ticket_str)?;
    Ok(ticket)
}

/// Parse and validate a ticket supplied by another peer.
pub fn parse_external_ticket(ticket_str: &str) -> Result<EndpointTicket> {
    let ticket = parse_ticket(ticket_str)?;
    validate_project_relay_addr(ticket.endpoint_addr())?;
    Ok(ticket)
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn wait_for_online_ticket(endpoint: &Endpoint) -> String {
        tokio::time::timeout(Duration::from_secs(60), async {
            loop {
                if let Ok(ticket) = generate_ticket_when_online(endpoint).await {
                    return ticket;
                }
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        })
        .await
        .expect("endpoint did not publish an online ticket")
    }

    #[tokio::test]
    async fn test_create_endpoint() {
        let endpoint = create_test_endpoint().await.unwrap();
        let id = endpoint.id();
        assert!(!id.to_string().is_empty());
        endpoint.close().await;
    }

    #[tokio::test]
    async fn test_ticket_roundtrip() {
        let endpoint = create_test_endpoint().await.unwrap();
        let ticket_str = generate_ticket(&endpoint);
        assert!(!ticket_str.is_empty());
        let ticket = parse_ticket(&ticket_str).unwrap();
        assert_eq!(ticket.endpoint_addr().id, endpoint.id());
        endpoint.close().await;
    }

    #[tokio::test]
    async fn test_generate_ticket_when_online() {
        let endpoint = create_test_endpoint().await.unwrap();
        let ticket_str = wait_for_online_ticket(&endpoint).await;
        let ticket = parse_ticket(&ticket_str).unwrap();
        assert_eq!(ticket.endpoint_addr().id, endpoint.id());
        assert!(
            !ticket.endpoint_addr().addrs.is_empty(),
            "shareable ticket should contain at least one dialable address"
        );
        endpoint.close().await;
    }
}
