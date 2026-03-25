mod commands;
mod connection;
mod messages;
mod node;
mod protocol;
mod state;

use std::sync::Arc;

use connection::ConnectionManager;
use iroh::protocol::Router;
use messages::Event;
use protocol::NafaqProtocol;
use state::AppState;
use tauri::Emitter;
use tokio::sync::broadcast;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("nafaq=info".parse().unwrap()),
        )
        .init();

    // Initialize Iroh synchronously during setup using Tauri's async runtime
    let rt = tauri::async_runtime::handle();

    let (event_tx, _) = broadcast::channel::<Event>(256);
    let (media_tx, _) = broadcast::channel::<Vec<u8>>(1024);
    let conn_manager = Arc::new(ConnectionManager::new(event_tx.clone(), media_tx));

    // Create endpoint + router on the async runtime
    let (endpoint, router) = rt.block_on(async {
        let endpoint = node::create_endpoint().await.expect("Failed to create Iroh endpoint");
        tracing::info!("Node ID: {}", endpoint.id());

        let router = Router::builder(endpoint.clone())
            .accept(node::NAFAQ_ALPN, NafaqProtocol::new(conn_manager.clone()))
            .spawn();

        (endpoint, router)
    });

    let app_state = AppState {
        endpoint,
        router,
        conn_manager: conn_manager.clone(),
        event_tx: event_tx.clone(),
    };

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .manage(app_state)
        .setup(move |app| {
            // Spawn event forwarder (broadcast → Tauri events)
            let app_handle = app.handle().clone();
            let mut event_rx = event_tx.subscribe();

            tauri::async_runtime::spawn(async move {
                loop {
                    match event_rx.recv().await {
                        Ok(event) => {
                            let event_name = match &event {
                                Event::PeerConnected { .. } => "peer-connected",
                                Event::PeerDisconnected { .. } => "peer-disconnected",
                                Event::ChatReceived { .. } => "chat-received",
                                Event::ControlReceived { .. } => "control-received",
                                Event::ConnectionStatus { .. } => "connection-status",
                                Event::Error { .. } => "nafaq-error",
                                _ => continue,
                            };
                            let _ = app_handle.emit(event_name, &event);
                        }
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            tracing::warn!("Event forwarder lagged by {n} messages");
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
            });

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_node_info,
            commands::create_call,
            commands::join_call,
            commands::end_call,
            commands::send_chat,
            commands::send_control,
            commands::send_audio,
            commands::send_video,
        ])
        .run(tauri::generate_context!())
        .expect("error running nafaq");
}
