use std::sync::Arc;
use std::time::Duration;

use tokio::sync::broadcast;
use tokio::time::timeout;

use nafaq_sidecar::connection::ConnectionManager;
use nafaq_sidecar::messages::Event;
use nafaq_sidecar::node::{self, NAFAQ_ALPN};
use nafaq_sidecar::protocol::NafaqProtocol;

use iroh::protocol::Router;

/// Integration test: two Iroh nodes connect and exchange a chat message.
///
/// This test requires network access (Iroh connects to relay servers) and
/// typically takes 5-15 seconds.
#[tokio::test]
async fn two_nodes_connect_and_chat() -> anyhow::Result<()> {
    // Optional tracing for debugging — silently ignored if already initialized.
    tracing_subscriber::fmt().try_init().ok();

    // ── Node A (the acceptor) ──────────────────────────────────────────

    let (event_tx_a, mut event_rx_a) = broadcast::channel::<Event>(256);
    let (media_tx_a, _media_rx_a) = broadcast::channel::<Vec<u8>>(256);

    let endpoint_a = timeout(Duration::from_secs(30), node::create_endpoint())
        .await
        .expect("Node A endpoint creation timed out")?;

    let conn_manager_a = Arc::new(ConnectionManager::new(event_tx_a.clone(), media_tx_a.clone()));
    let protocol_a = NafaqProtocol::new(conn_manager_a.clone());

    let router_a = Router::builder(endpoint_a.clone())
        .accept(NAFAQ_ALPN.to_vec(), protocol_a)
        .spawn();

    // Generate a ticket so Node B can reach Node A.
    let ticket_str = node::generate_ticket(&endpoint_a);
    assert!(!ticket_str.is_empty(), "Ticket must not be empty");

    // ── Node B (the connector) ─────────────────────────────────────────

    let (event_tx_b, _event_rx_b) = broadcast::channel::<Event>(256);
    let (media_tx_b, _media_rx_b) = broadcast::channel::<Vec<u8>>(256);

    let endpoint_b = timeout(Duration::from_secs(30), node::create_endpoint())
        .await
        .expect("Node B endpoint creation timed out")?;

    let conn_manager_b = Arc::new(ConnectionManager::new(event_tx_b.clone(), media_tx_b.clone()));
    let protocol_b = NafaqProtocol::new(conn_manager_b.clone());

    let router_b = Router::builder(endpoint_b.clone())
        .accept(NAFAQ_ALPN.to_vec(), protocol_b)
        .spawn();

    // ── Node B connects to Node A ──────────────────────────────────────

    let ticket = node::parse_ticket(&ticket_str)?;
    let peer_id_a = timeout(
        Duration::from_secs(30),
        conn_manager_b.connect_to_peer(&endpoint_b, ticket.endpoint_addr().clone()),
    )
    .await
    .expect("connect_to_peer timed out")?;

    tracing::info!("Node B connected to Node A (peer_id = {peer_id_a})");

    // ── Verify Node A received PeerConnected ───────────────────────────

    let peer_connected = timeout(Duration::from_secs(30), async {
        loop {
            match event_rx_a.recv().await {
                Ok(Event::PeerConnected { peer_id }) => break peer_id,
                Ok(_) => continue, // skip other events
                Err(e) => panic!("event_rx_a recv error: {e}"),
            }
        }
    })
    .await
    .expect("Timed out waiting for PeerConnected on Node A");

    tracing::info!("Node A saw PeerConnected from {peer_connected}");
    assert!(
        !peer_connected.is_empty(),
        "PeerConnected peer_id must not be empty"
    );

    // ── Node B sends a chat message to Node A ──────────────────────────

    let test_message = "Hello from Node B!";

    // Give Node A a moment to finish setting up its accept_bi receiver
    // before Node B writes to the stream.
    tokio::time::sleep(Duration::from_millis(500)).await;

    conn_manager_b
        .send_chat(&peer_id_a, test_message)
        .await?;

    tracing::info!("Node B sent chat: {test_message}");

    // ── Verify Node A received ChatReceived ────────────────────────────

    let (received_peer, received_msg) = timeout(Duration::from_secs(30), async {
        loop {
            match event_rx_a.recv().await {
                Ok(Event::ChatReceived { peer_id, message }) => break (peer_id, message),
                Ok(_) => continue,
                Err(e) => panic!("event_rx_a recv error while waiting for ChatReceived: {e}"),
            }
        }
    })
    .await
    .expect("Timed out waiting for ChatReceived on Node A");

    tracing::info!("Node A received chat from {received_peer}: {received_msg}");
    assert_eq!(received_msg, test_message);

    // ── Cleanup ────────────────────────────────────────────────────────

    conn_manager_b.disconnect_peer(&peer_id_a).await?;

    router_b.shutdown().await.ok();
    router_a.shutdown().await.ok();

    endpoint_b.close().await;
    endpoint_a.close().await;

    tracing::info!("Integration test completed successfully");
    Ok(())
}
