mod codec;
mod commands;
mod connection;
mod messages;
mod node;
mod protocol;
mod state;

use std::sync::Arc;

use codec::CodecState;
use connection::ConnectionManager;
use iroh::protocol::Router;
use messages::{Event, MediaFrame, STREAM_AUDIO, STREAM_VIDEO};
use protocol::NafaqProtocol;
use state::AppState;
use tauri::Emitter;
use tokio::sync::broadcast;

#[derive(Clone, serde::Serialize)]
struct MediaEvent {
    stream_type: u8,
    data: Vec<u8>,
    timestamp: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    width: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    height: Option<u32>,
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
    let (media_tx, _) = broadcast::channel::<Vec<u8>>(1024);
    let media_tx_for_setup = media_tx.clone();
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

    let codec = Arc::new(CodecState::new());

    let app_state = AppState {
        endpoint,
        router,
        conn_manager: conn_manager.clone(),
        event_tx: event_tx.clone(),
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

            // Spawn media forwarder (binary frames → decode → Tauri events)
            let app_handle2 = app.handle().clone();
            let mut media_rx = media_tx_for_setup.subscribe();
            let codec_for_media = codec.clone();

            tauri::async_runtime::spawn(async move {
                loop {
                    match media_rx.recv().await {
                        Ok(raw) => {
                            if let Some(frame) = MediaFrame::decode(&raw) {
                                match frame.stream_type {
                                    STREAM_AUDIO => {
                                        let mut audio = codec_for_media.audio.lock().await;
                                        if let Some(ref mut dec) = *audio {
                                            if let Some(pcm) = dec.decode(&frame.payload) {
                                                let data: Vec<u8> = pcm.iter()
                                                    .flat_map(|s| s.to_le_bytes())
                                                    .collect();
                                                let _ = app_handle2.emit("audio-received", MediaEvent {
                                                    stream_type: frame.stream_type,
                                                    data,
                                                    timestamp: frame.timestamp_ms,
                                                    width: None,
                                                    height: None,
                                                });
                                            }
                                        }
                                    }
                                    STREAM_VIDEO => {
                                        let mut video = codec_for_media.video.lock().await;
                                        if let Some(ref mut dec) = *video {
                                            if let Some((rgba, w, h)) = dec.decode(&frame.payload) {
                                                let jpeg = crate::codec::decoded_to_jpeg(&rgba, w, h);
                                                let _ = app_handle2.emit("video-received", MediaEvent {
                                                    stream_type: frame.stream_type,
                                                    data: jpeg,
                                                    timestamp: frame.timestamp_ms,
                                                    width: Some(w),
                                                    height: Some(h),
                                                });
                                            }
                                        }
                                    }
                                    _ => {}
                                }
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            tracing::warn!("Media forwarder lagged by {n} frames");
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
            commands::init_codecs,
            commands::destroy_codecs,
        ])
        .run(tauri::generate_context!())
        .expect("error running nafaq");
}
