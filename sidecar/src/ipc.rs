use std::sync::Arc;

use anyhow::Result;
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpListener;
use tokio::sync::broadcast;
use tokio_tungstenite::{accept_async, tungstenite::Message};

use crate::connection::ConnectionManager;
use crate::messages::{
    Command, Event, MediaFrame, STREAM_AUDIO, STREAM_VIDEO,
};
use crate::node;

/// Start the WebSocket IPC server on the given port.
///
/// This blocks until the server shuts down or encounters a fatal error.
/// For each connected WebSocket client it:
/// - Forwards broadcast events and media frames to the client
/// - Reads incoming text (JSON commands) and binary (media frames) messages
pub async fn run_ws_server(
    port: u16,
    endpoint: iroh::Endpoint,
    conn_manager: Arc<ConnectionManager>,
    event_tx: broadcast::Sender<Event>,
    media_tx: broadcast::Sender<Vec<u8>>,
) -> Result<()> {
    let listener = TcpListener::bind(format!("127.0.0.1:{port}")).await?;
    tracing::info!("WebSocket IPC server listening on 127.0.0.1:{port}");

    loop {
        let (stream, addr) = listener.accept().await?;
        tracing::info!("New TCP connection from {addr}");

        let endpoint = endpoint.clone();
        let conn_manager = conn_manager.clone();
        let event_rx = event_tx.subscribe();
        let media_rx = media_tx.subscribe();

        tokio::spawn(async move {
            match accept_async(stream).await {
                Ok(ws_stream) => {
                    tracing::info!("WebSocket handshake complete with {addr}");
                    if let Err(e) =
                        handle_ws_client(ws_stream, endpoint, conn_manager, event_rx, media_rx)
                            .await
                    {
                        tracing::warn!("WebSocket client {addr} error: {e}");
                    }
                }
                Err(e) => {
                    tracing::warn!("WebSocket handshake failed for {addr}: {e}");
                }
            }
        });
    }
}

/// Handle a single WebSocket client connection.
///
/// Spawns forwarding tasks for events and media, then processes incoming messages.
async fn handle_ws_client(
    ws_stream: tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
    endpoint: iroh::Endpoint,
    conn_manager: Arc<ConnectionManager>,
    mut event_rx: broadcast::Receiver<Event>,
    mut media_rx: broadcast::Receiver<Vec<u8>>,
) -> Result<()> {
    let (mut ws_sink, mut ws_source) = ws_stream.split();

    // We use an mpsc channel so that the forwarding tasks and the read loop
    // can all send outgoing messages through a single sink.
    let (out_tx, mut out_rx) = tokio::sync::mpsc::channel::<Message>(256);

    // Task: drain the mpsc channel into the WebSocket sink
    let sink_task = tokio::spawn(async move {
        while let Some(msg) = out_rx.recv().await {
            if ws_sink.send(msg).await.is_err() {
                break;
            }
        }
    });

    // Task: forward Event broadcasts → WS text frames
    let out_tx_events = out_tx.clone();
    let event_fwd = tokio::spawn(async move {
        loop {
            match event_rx.recv().await {
                Ok(event) => {
                    if let Ok(json) = serde_json::to_string(&event) {
                        if out_tx_events.send(Message::text(json)).await.is_err() {
                            break;
                        }
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!("Event broadcast lagged, skipped {n} messages");
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });

    // Task: forward media broadcasts → WS binary frames
    let out_tx_media = out_tx.clone();
    let media_fwd = tokio::spawn(async move {
        loop {
            match media_rx.recv().await {
                Ok(data) => {
                    if out_tx_media.send(Message::binary(data)).await.is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!("Media broadcast lagged, skipped {n} messages");
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });

    // Read loop: incoming WS messages
    while let Some(msg_result) = ws_source.next().await {
        match msg_result {
            Ok(Message::Text(text)) => {
                let response =
                    handle_text_command(text.as_str(), &endpoint, &conn_manager).await;
                if let Some(reply) = response {
                    if out_tx.send(reply).await.is_err() {
                        break;
                    }
                }
            }
            Ok(Message::Binary(data)) => {
                handle_binary_frame(&data, &conn_manager).await;
            }
            Ok(Message::Ping(_)) | Ok(Message::Pong(_)) => {
                // tokio-tungstenite handles ping/pong automatically
            }
            Ok(Message::Close(_)) => {
                tracing::info!("WebSocket client sent close frame");
                break;
            }
            Ok(_) => {}
            Err(e) => {
                tracing::warn!("WebSocket read error: {e}");
                break;
            }
        }
    }

    // Clean up forwarding tasks
    event_fwd.abort();
    media_fwd.abort();
    drop(out_tx);
    let _ = sink_task.await;

    Ok(())
}

/// Parse and dispatch a JSON text command from the WebSocket client.
///
/// Returns an optional `Message` to send back as a reply.
async fn handle_text_command(
    text: &str,
    endpoint: &iroh::Endpoint,
    conn_manager: &Arc<ConnectionManager>,
) -> Option<Message> {
    let command: Command = match serde_json::from_str(text) {
        Ok(cmd) => cmd,
        Err(e) => {
            tracing::warn!("Failed to parse command: {e}");
            let err_event = Event::Error {
                message: format!("Invalid command JSON: {e}"),
            };
            return Some(Message::text(serde_json::to_string(&err_event).ok()?));
        }
    };

    tracing::debug!("Received command: {command:?}");

    match command {
        Command::GetNodeInfo => {
            let id = endpoint.id().to_string();
            let ticket = node::generate_ticket(endpoint);
            let event = Event::NodeInfo { id, ticket };
            Some(Message::text(serde_json::to_string(&event).ok()?))
        }
        Command::CreateCall => {
            let ticket = node::generate_ticket(endpoint);
            let event = Event::CallCreated { ticket };
            Some(Message::text(serde_json::to_string(&event).ok()?))
        }
        Command::JoinCall { ticket } => {
            match node::parse_ticket(&ticket) {
                Ok(endpoint_ticket) => {
                    let addr = endpoint_ticket.endpoint_addr().clone();
                    match conn_manager.connect_to_peer(endpoint, addr).await {
                        Ok(peer_id) => {
                            let event = Event::PeerConnected { peer_id };
                            Some(Message::text(serde_json::to_string(&event).ok()?))
                        }
                        Err(e) => {
                            let event = Event::Error {
                                message: format!("Failed to connect: {e}"),
                            };
                            Some(Message::text(serde_json::to_string(&event).ok()?))
                        }
                    }
                }
                Err(e) => {
                    let event = Event::Error {
                        message: format!("Invalid ticket: {e}"),
                    };
                    Some(Message::text(serde_json::to_string(&event).ok()?))
                }
            }
        }
        Command::EndCall { peer_id } => {
            if let Err(e) = conn_manager.disconnect_peer(&peer_id).await {
                let event = Event::Error {
                    message: format!("Failed to end call: {e}"),
                };
                Some(Message::text(serde_json::to_string(&event).ok()?))
            } else {
                // PeerDisconnected event is sent by disconnect_peer via broadcast
                None
            }
        }
        Command::SendChat { peer_id, message } => {
            if let Err(e) = conn_manager.send_chat(&peer_id, &message).await {
                let event = Event::Error {
                    message: format!("Failed to send chat: {e}"),
                };
                Some(Message::text(serde_json::to_string(&event).ok()?))
            } else {
                None
            }
        }
        Command::SendControl { peer_id, action } => {
            if let Err(e) = conn_manager.send_control(&peer_id, &action).await {
                let event = Event::Error {
                    message: format!("Failed to send control: {e}"),
                };
                Some(Message::text(serde_json::to_string(&event).ok()?))
            } else {
                None
            }
        }
    }
}

/// Decode a binary WebSocket frame as a `MediaFrame` and forward it to the appropriate peer.
///
/// The `peer_id` field in the frame is the raw 32-byte public key of the target peer.
async fn handle_binary_frame(data: &[u8], conn_manager: &Arc<ConnectionManager>) {
    let frame = match MediaFrame::decode(data) {
        Some(f) => f,
        None => {
            tracing::warn!("Received invalid binary frame ({} bytes)", data.len());
            return;
        }
    };

    // Convert the 32-byte public key back to the string representation
    let peer_id = match iroh::EndpointId::from_bytes(&frame.peer_id) {
        Ok(id) => id.to_string(),
        Err(e) => {
            tracing::warn!("Invalid peer_id in binary frame: {e}");
            return;
        }
    };

    let result = match frame.stream_type {
        STREAM_AUDIO => conn_manager.send_audio(&peer_id, &frame.payload).await,
        STREAM_VIDEO => conn_manager.send_video(&peer_id, &frame.payload).await,
        other => {
            tracing::warn!("Unknown media stream type in binary frame: {other}");
            return;
        }
    };

    if let Err(e) = result {
        tracing::warn!("Failed to forward media to {peer_id}: {e}");
    }
}
