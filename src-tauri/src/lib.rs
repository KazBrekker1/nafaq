mod codec;
mod commands;
mod connection;
mod messages;
mod node;
mod protocol;
mod state;

use std::sync::Arc;

use base64::Engine;
use codec::CodecState;
use connection::ConnectionManager;
use iroh::protocol::Router;
use messages::{Event, MediaPacket};
use protocol::NafaqProtocol;
use state::AppState;
use tauri::Emitter;
use tokio::sync::{broadcast, watch};

const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

#[derive(Clone, serde::Serialize)]
struct VideoEvent {
    data: String,    // base64-encoded H.264 NALUs
    timestamp: u64,
}

#[derive(Clone, serde::Serialize)]
struct AudioEvent {
    data: String,    // base64-encoded PCM Int16 LE
    timestamp: u64,
}

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
    let (audio_media_tx, _) = broadcast::channel::<MediaPacket>(16);
    let (video_watch_tx, _) = watch::channel::<Option<MediaPacket>>(None);

    let audio_media_tx_for_setup = audio_media_tx.clone();
    let video_watch_tx_for_setup = video_watch_tx.clone();

    let conn_manager = Arc::new(ConnectionManager::new(
        event_tx.clone(),
        audio_media_tx.clone(),
        video_watch_tx.clone(),
    ));

    // Create endpoint + router on the async runtime
    let (endpoint, router) = rt.block_on(async {
        let endpoint = node::create_endpoint().await.expect("Failed to create Iroh endpoint");
        tracing::info!("Node ID: {}", endpoint.id());

        // Give connection manager a reference to the endpoint for mesh formation
        conn_manager.set_endpoint(endpoint.clone()).await;

        let router = Router::builder(endpoint.clone())
            .accept(node::NAFAQ_ALPN, NafaqProtocol::new(conn_manager.clone()))
            .spawn();

        (endpoint, router)
    });

    let codec = Arc::new(CodecState::new());

    let app_state = AppState {
        endpoint,
        router,
        conn_manager: conn_manager.clone(),
        event_tx: event_tx.clone(),
        audio_media_tx: audio_media_tx.clone(),
        video_watch_tx: video_watch_tx.clone(),
        codec: codec.clone(),
    };

    let mut builder = tauri::Builder::default()
        .manage(app_state);

    #[cfg(desktop)]
    {
        builder = builder.plugin(tauri_plugin_shell::init());
    }

    builder
        .setup(move |app| {
            // Spawn event forwarder (broadcast -> Tauri events)
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
                                Event::NodeInfo { .. } | Event::CallCreated { .. } => continue,
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

            // Spawn PeerAnnounce handler for automatic mesh formation
            let conn_manager_for_mesh = conn_manager.clone();
            let mut mesh_rx = event_tx.subscribe();

            tauri::async_runtime::spawn(async move {
                loop {
                    match mesh_rx.recv().await {
                        Ok(Event::ControlReceived { peer_id, action: messages::ControlAction::PeerAnnounce { peer_id: announced_id, ticket } }) => {
                            conn_manager_for_mesh.handle_peer_announce(&peer_id, announced_id, ticket).await;
                        }
                        Ok(_) => {} // ignore other events
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            tracing::warn!("Mesh handler lagged by {n} events");
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
            });

            // Spawn audio forwarder (Opus -> PCM -> base64 -> Tauri event)
            let app_handle_audio = app.handle().clone();
            let codec_audio = codec.clone();

            tauri::async_runtime::spawn(async move {
                let mut audio_rx = audio_media_tx_for_setup.subscribe();
                loop {
                    match audio_rx.recv().await {
                        Ok((_peer_id, timestamp, payload)) => {
                            let mut dec = codec_audio.audio_decoder.lock().await;
                            if let Some(ref mut d) = *dec {
                                if let Some(pcm) = d.decode(&payload) {
                                    let raw: Vec<u8> = pcm.iter().flat_map(|s| s.to_le_bytes()).collect();
                                    let _ = app_handle_audio.emit("audio-received", AudioEvent {
                                        data: B64.encode(&raw),
                                        timestamp,
                                    });
                                }
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            tracing::warn!("Audio forwarder lagged by {n} frames");
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
            });

            // Spawn video forwarder (H.264 NALUs -> base64 -> Tauri event)
            let app_handle_video = app.handle().clone();
            let mut video_rx = video_watch_tx_for_setup.subscribe();

            tauri::async_runtime::spawn(async move {
                loop {
                    if video_rx.changed().await.is_err() { break; }
                    let frame = video_rx.borrow_and_update().clone();
                    if let Some((_peer_id, timestamp, payload)) = frame {
                        let _ = app_handle_video.emit("video-received", VideoEvent {
                            data: B64.encode(&payload),
                            timestamp,
                        });
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
            commands::init_codecs,
            commands::destroy_codecs,
            commands::reinit_video_encoder,
        ])
        .run(tauri::generate_context!())
        .expect("error running nafaq");
}
