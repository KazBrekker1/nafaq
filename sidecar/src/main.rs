use std::sync::Arc;

use clap::Parser;
use tokio::sync::broadcast;

use nafaq_sidecar::connection::ConnectionManager;
use nafaq_sidecar::ipc;
use nafaq_sidecar::messages::Event;
use nafaq_sidecar::node;
use nafaq_sidecar::protocol::NafaqProtocol;

#[derive(Parser)]
#[command(name = "nafaq-sidecar")]
struct Cli {
    /// WebSocket port for IPC with Electrobun
    #[arg(short, long, default_value = "9320")]
    port: u16,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("nafaq_sidecar=info".parse()?),
        )
        .init();

    let cli = Cli::parse();
    tracing::info!("nafaq-sidecar starting on port {}", cli.port);

    // Broadcast channels for events (JSON) and media (binary)
    let (event_tx, _event_rx) = broadcast::channel::<Event>(256);
    let (media_tx, _media_rx) = broadcast::channel::<Vec<u8>>(256);

    // Connection manager
    let conn_manager = Arc::new(ConnectionManager::new(event_tx.clone(), media_tx.clone()));

    // Create Iroh endpoint
    let endpoint = node::create_endpoint().await?;
    tracing::info!("Iroh endpoint ID: {}", endpoint.id());

    // Set up the protocol router
    let protocol = NafaqProtocol::new(conn_manager.clone());
    let router = iroh::protocol::Router::builder(endpoint.clone())
        .accept(node::NAFAQ_ALPN.to_vec(), protocol)
        .spawn();
    tracing::info!("Protocol router started");

    // Run the WebSocket IPC server (blocks until shutdown)
    let ws_result = ipc::run_ws_server(
        cli.port,
        endpoint.clone(),
        conn_manager,
        event_tx,
        media_tx,
    )
    .await;

    // Shutdown
    tracing::info!("Shutting down...");
    router.shutdown().await.ok();
    endpoint.close().await;

    ws_result
}
