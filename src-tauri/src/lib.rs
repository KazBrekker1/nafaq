mod codec;
mod commands;
mod connection;
mod messages;
mod node;
mod protocol;
mod state;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use base64::Engine;
use codec::{AudioDecoder, AudioCodecState, VideoCodecState};
use connection::ConnectionManager;
use iroh::protocol::Router;
use messages::{AudioPacket, ControlAction, Event, VideoPacket};
use protocol::NafaqProtocol;
use state::{AppState, MediaBridgeState};
use tauri::Emitter;
use tokio::sync::broadcast;

const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

#[derive(Clone, serde::Serialize)]
struct VideoEvent {
    peer_id: String,
    data: String,
    width: u32,
    height: u32,
    timestamp: u64,
}

#[derive(Clone, serde::Serialize)]
struct AudioEvent {
    peer_id: String,
    data: String,
    timestamp: u64,
}

fn pack_audio_channel_packet(peer_id: &str, timestamp: u64, pcm: &[u8]) -> Option<Vec<u8>> {
    let peer_id_bytes = peer_id.as_bytes();
    let peer_id_len = u16::try_from(peer_id_bytes.len()).ok()?;
    let pcm_len = u32::try_from(pcm.len()).ok()?;
    let mut packet = Vec::with_capacity(2 + peer_id_bytes.len() + 8 + 4 + pcm.len());
    packet.extend_from_slice(&peer_id_len.to_le_bytes());
    packet.extend_from_slice(peer_id_bytes);
    packet.extend_from_slice(&timestamp.to_le_bytes());
    packet.extend_from_slice(&pcm_len.to_le_bytes());
    packet.extend_from_slice(pcm);
    Some(packet)
}

fn pack_video_channel_raw_nalu(
    peer_id: &str,
    timestamp: u64,
    h264_data: &[u8],
    is_keyframe: bool,
) -> Option<Vec<u8>> {
    let peer_id_bytes = peer_id.as_bytes();
    let peer_id_len = u16::try_from(peer_id_bytes.len()).ok()?;
    let data_len = u32::try_from(h264_data.len()).ok()?;
    let mut packet = Vec::with_capacity(2 + peer_id_bytes.len() + 8 + 1 + 4 + h264_data.len());
    packet.extend_from_slice(&peer_id_len.to_le_bytes());
    packet.extend_from_slice(peer_id_bytes);
    packet.extend_from_slice(&timestamp.to_le_bytes());
    packet.push(if is_keyframe { 1 } else { 0 });
    packet.extend_from_slice(&data_len.to_le_bytes());
    packet.extend_from_slice(h264_data);
    Some(packet)
}

fn pack_video_channel_packet(
    peer_id: &str,
    timestamp: u64,
    width: u32,
    height: u32,
    jpeg: &[u8],
) -> Option<Vec<u8>> {
    let peer_id_bytes = peer_id.as_bytes();
    let peer_id_len = u16::try_from(peer_id_bytes.len()).ok()?;
    let jpeg_len = u32::try_from(jpeg.len()).ok()?;
    let mut packet =
        Vec::with_capacity(2 + peer_id_bytes.len() + 8 + 4 + 4 + 4 + jpeg.len());
    packet.extend_from_slice(&peer_id_len.to_le_bytes());
    packet.extend_from_slice(peer_id_bytes);
    packet.extend_from_slice(&timestamp.to_le_bytes());
    packet.extend_from_slice(&width.to_le_bytes());
    packet.extend_from_slice(&height.to_le_bytes());
    packet.extend_from_slice(&jpeg_len.to_le_bytes());
    packet.extend_from_slice(jpeg);
    Some(packet)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    use tauri_plugin_store::StoreExt;

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("nafaq=info".parse().unwrap()),
        )
        .init();

    let mut builder = tauri::Builder::default();

    #[cfg(desktop)]
    {
        builder = builder.plugin(tauri_plugin_shell::init());
    }

    builder = builder.plugin(tauri_plugin_store::Builder::new().build());

    builder
        .setup(move |app| {
            // Load optional persisted secret key from store
            let secret_key = {
                let store = app
                    .handle()
                    .store("settings.json")?;
                let persistent = store
                    .get("persistent_identity")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                if persistent {
                    store
                        .get("secret_key")
                        .and_then(|v| v.as_str().map(String::from))
                        .and_then(|hex| {
                            hex.parse::<iroh::SecretKey>().ok().or_else(|| {
                                // Fallback: try base64 decode
                                use base64::Engine;
                                B64.decode(&hex).ok().and_then(|bytes| {
                                    bytes.as_slice().try_into().ok().map(|arr: [u8; 32]| {
                                        iroh::SecretKey::from_bytes(&arr)
                                    })
                                })
                            })
                        })
                } else {
                    None
                }
            };

            // Initialize Iroh synchronously on the async runtime
            let (event_tx, _) = broadcast::channel::<Event>(256);
            let (audio_media_tx, _) = broadcast::channel::<AudioPacket>(256);
            let (video_media_tx, _) = broadcast::channel::<VideoPacket>(16);

            let audio_media_tx_for_setup = audio_media_tx.clone();
            let video_media_tx_for_setup = video_media_tx.clone();

            let conn_manager = Arc::new(ConnectionManager::new(
                event_tx.clone(),
                audio_media_tx.clone(),
                video_media_tx.clone(),
            ));

            let conn_manager_for_rt = conn_manager.clone();
            let (endpoint, router) = tauri::async_runtime::handle().block_on(async {
                let endpoint = node::create_endpoint_with_key(secret_key)
                    .await
                    .expect("Failed to create Iroh endpoint");
                tracing::info!("Node ID: {}", endpoint.id());

                // Give connection manager a reference to the endpoint for mesh formation
                conn_manager_for_rt.set_endpoint(endpoint.clone()).await;

                let router = Router::builder(endpoint.clone())
                    .accept(node::NAFAQ_ALPN, NafaqProtocol::new(conn_manager_for_rt.clone()))
                    .spawn();

                (endpoint, router)
            });

            let audio_codec = Arc::new(AudioCodecState::new());
            let video_codec = Arc::new(VideoCodecState::new());
            let media_bridge = MediaBridgeState::default();

            let video_runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .thread_name("nafaq-video")
                .enable_all()
                .build()
                .expect("Failed to create video runtime");
            let video_runtime_handle = video_runtime.handle().clone();
            // Keep runtime alive for the app's lifetime
            std::mem::forget(video_runtime);

            let app_state = AppState {
                endpoint,
                router,
                conn_manager: conn_manager.clone(),
                event_tx: event_tx.clone(),
                audio_media_tx: audio_media_tx.clone(),
                video_media_tx: video_media_tx.clone(),
                audio_codec: audio_codec.clone(),
                video_codec: video_codec.clone(),
                video_runtime: video_runtime_handle.clone(),
            };

            let media_bridge_ref = media_bridge.current.clone();

            app.manage(app_state);
            app.manage(media_bridge);

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
                                Event::QualityProfileChanged { .. } => "quality-profile-changed",
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

            // Spawn PeerAnnounce + VideoQualityRequest handler
            let conn_manager_for_control = conn_manager.clone();
            let mut control_rx = event_tx.subscribe();

            tauri::async_runtime::spawn(async move {
                loop {
                    match control_rx.recv().await {
                        Ok(Event::ControlReceived {
                            peer_id,
                            action:
                                ControlAction::PeerAnnounce {
                                    peer_id: announced_id,
                                    ticket,
                                },
                        }) => {
                            conn_manager_for_control
                                .handle_peer_announce(&peer_id, announced_id, ticket)
                                .await;
                        }
                        Ok(Event::ControlReceived {
                            peer_id,
                            action: ControlAction::VideoQualityRequest { layer },
                        }) => {
                            conn_manager_for_control
                                .set_peer_video_layer(&peer_id, layer)
                                .await;
                            tracing::debug!("Peer {peer_id} requested video layer: {layer:?}");
                        }
                        Ok(Event::ControlReceived {
                            peer_id,
                            action: ControlAction::KeyframeRequest { layer },
                        }) => {
                            conn_manager_for_control
                                .request_peer_keyframe(&peer_id, layer)
                                .await;
                            tracing::debug!(
                                "Peer {peer_id} requested keyframe for layer: {layer:?}"
                            );
                        }
                        Ok(_) => {} // ignore other events
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            tracing::warn!("Control handler lagged by {n} events");
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
            });

            // Spawn audio forwarder with per-peer decoders.
            let app_handle_audio = app.handle().clone();
            let codec_audio = audio_codec.clone();
            let audio_bridge = media_bridge_ref.clone();

            tauri::async_runtime::spawn(async move {
                let mut audio_rx = audio_media_tx_for_setup.subscribe();
                let mut last_active: HashMap<String, std::time::Instant> = HashMap::new();
                let mut last_sequence: HashMap<String, u16> = HashMap::new();
                let mut last_prune = std::time::Instant::now();
                let mut peer_energy: HashMap<String, f32> = HashMap::new();
                loop {
                    match audio_rx.recv().await {
                        Ok(packet) => {
                            let peer_id = packet.peer_id;
                            let timestamp = packet.timestamp_ms;
                            let payload = packet.payload;
                            // Prune stale peers every 5 seconds
                            let now_inst = std::time::Instant::now();
                            if now_inst.duration_since(last_prune).as_secs() >= 5 {
                                last_prune = now_inst;
                                let stale: Vec<String> = last_active
                                    .iter()
                                    .filter(|(_, t)| now_inst.duration_since(**t).as_secs() >= 10)
                                    .map(|(k, _)| k.clone())
                                    .collect();
                                for k in &stale {
                                    last_active.remove(k);
                                    last_sequence.remove(k);
                                    peer_energy.remove(k);
                                    codec_audio.remove_peer_decoders(k).await;
                                }
                            }
                            last_active.insert(peer_id.clone(), now_inst);

                            let packet_lost =
                                match last_sequence.insert(peer_id.clone(), packet.sequence) {
                                    Some(previous) if packet.sequence <= previous => {
                                        continue;
                                    }
                                    Some(previous) => packet.sequence != previous.wrapping_add(1),
                                    None => false,
                                };

                            // Selective decode at 5+ peers: skip quiet speakers
                            let peer_count = last_active.len();
                            if peer_count >= 5 {
                                let mut energies: Vec<(&String, &f32)> =
                                    peer_energy.iter().collect();
                                energies.sort_by(|a, b| {
                                    b.1.partial_cmp(a.1).unwrap_or(std::cmp::Ordering::Equal)
                                });
                                let top_speakers: HashSet<&String> =
                                    energies.iter().take(3).map(|(id, _)| *id).collect();

                                // Allow new peers (not yet in energy map) through for initial decode
                                if peer_energy.contains_key(&peer_id)
                                    && !top_speakers.contains(&peer_id)
                                {
                                    continue;
                                }
                            }

                            let mut decoders = codec_audio.decoders.lock().await;
                            let decoder = decoders
                                .entry(peer_id.clone())
                                .or_insert_with(AudioDecoder::new);

                            if let Some(pcm) = decoder.decode(&payload, packet_lost) {
                                let rms = (pcm
                                    .iter()
                                    .map(|&s| (s as f32).powi(2))
                                    .sum::<f32>()
                                    / pcm.len() as f32)
                                    .sqrt();
                                peer_energy.insert(peer_id.clone(), rms);

                                let raw: Vec<u8> =
                                    pcm.iter().flat_map(|s| s.to_le_bytes()).collect();
                                let registration = audio_bridge.lock().await.clone();
                                if let Some(registration) = registration {
                                    if let Some(channel) = registration.audio_channel {
                                        let Some(channel_payload) =
                                            pack_audio_channel_packet(&peer_id, timestamp, &raw)
                                        else {
                                            continue;
                                        };
                                        let _ = channel.send(channel_payload);
                                    } else {
                                        let _ = app_handle_audio.emit(
                                            "audio-received",
                                            AudioEvent {
                                                peer_id: peer_id.clone(),
                                                data: B64.encode(&raw),
                                                timestamp,
                                            },
                                        );
                                    }
                                } else {
                                    let _ = app_handle_audio.emit(
                                        "audio-received",
                                        AudioEvent {
                                            peer_id: peer_id.clone(),
                                            data: B64.encode(&raw),
                                            timestamp,
                                        },
                                    );
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

            // Spawn video forwarder (H.264 NALUs -> binary channel)
            let app_handle_video = app.handle().clone();
            let codec_video = video_codec.clone();
            let video_bridge = media_bridge_ref.clone();
            let video_runtime_handle = video_runtime_handle.clone();

            tauri::async_runtime::spawn(async move {
                let mut video_rx = video_media_tx_for_setup.subscribe();
                loop {
                    match video_rx.recv().await {
                        Ok(packet) => {
                            // Check bridge registration first — if WebCodecs is active,
                            // forward raw NALUs and skip the decode+JPEG path entirely.
                            let registration = video_bridge.lock().await.clone();
                            if let Some(ref reg) = registration {
                                if reg.webcodecs_active {
                                    let kf = codec::is_keyframe(&packet.payload);
                                    if let Some(channel) = &reg.video_channel {
                                        if let Some(raw_packet) = pack_video_channel_raw_nalu(
                                            &packet.peer_id,
                                            packet.timestamp_ms,
                                            &packet.payload,
                                            kf,
                                        ) {
                                            let _ = channel.send(raw_packet);
                                        }
                                    }
                                    continue; // Skip the decode+JPEG path
                                }
                            }

                            // Decode + encode is CPU-bound; run on the dedicated
                            // video runtime so the main runtime isn't starved.
                            let codec_video_clone = codec_video.clone();
                            let peer_id_clone = packet.peer_id.clone();
                            let payload = packet.payload.clone();
                            let jpeg_result = video_runtime_handle.spawn(async move {
                                let mut decoders = codec_video_clone.decoders.lock().await;
                                let decoder = decoders
                                    .entry(peer_id_clone)
                                    .or_insert_with(codec::VideoDecoder::new);
                                decoder.decode_rgba(&payload).and_then(|(rgba, w, h)| {
                                    codec::encode_jpeg(&rgba, w, h, 70).map(|j| (j, w, h))
                                })
                            }).await.ok().flatten();
                            if let Some((jpeg, width, height)) = jpeg_result {
                                let registration = video_bridge.lock().await.clone();
                                if let Some(registration) = registration {
                                    if let Some(channel) = registration.video_channel {
                                        let Some(channel_payload) = pack_video_channel_packet(
                                            &packet.peer_id,
                                            packet.timestamp_ms,
                                            width,
                                            height,
                                            &jpeg,
                                        ) else {
                                            continue;
                                        };
                                        let _ = channel.send(channel_payload);
                                    } else {
                                        let _ = app_handle_video.emit(
                                            "video-received",
                                            VideoEvent {
                                                peer_id: packet.peer_id.clone(),
                                                data: B64.encode(jpeg),
                                                width,
                                                height,
                                                timestamp: packet.timestamp_ms,
                                            },
                                        );
                                    }
                                } else {
                                    let _ = app_handle_video.emit(
                                        "video-received",
                                        VideoEvent {
                                            peer_id: packet.peer_id.clone(),
                                            data: B64.encode(jpeg),
                                            width,
                                            height,
                                            timestamp: packet.timestamp_ms,
                                        },
                                    );
                                }
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            tracing::warn!("Video forwarder lagged by {n} frames");
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
            });

            let audio_cleanup = audio_codec.clone();
            let video_cleanup = video_codec.clone();
            let mut disconnect_rx = event_tx.subscribe();
            tauri::async_runtime::spawn(async move {
                loop {
                    match disconnect_rx.recv().await {
                        Ok(Event::PeerDisconnected { peer_id }) => {
                            audio_cleanup.remove_peer_decoders(&peer_id).await;
                            video_cleanup.remove_peer_decoders(&peer_id).await;
                        }
                        Ok(_) => {}
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            tracing::warn!("Disconnect cleanup lagged by {n} events");
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
            });

            let app_handle_stats = app.handle().clone();
            let conn_manager_stats = conn_manager.clone();
            tauri::async_runtime::spawn(async move {
                let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
                interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                loop {
                    interval.tick().await;
                    for stats in conn_manager_stats.snapshot_network_stats().await {
                        // Per-peer quality adaptation
                        let target = if stats.rtt_ms > 200 || stats.lost_packets > 50 {
                            100_000 // 100kbps for degraded peers
                        } else {
                            0 // Use global profile (no override)
                        };
                        let current = conn_manager_stats.get_peer_outbound_bitrate(&stats.peer_id).await;
                        if current != target {
                            conn_manager_stats.set_peer_outbound_bitrate(&stats.peer_id, target).await;
                        }
                        let _ = app_handle_stats.emit("network-stats", &stats);
                    }
                }
            });

            let conn_manager_liveness = conn_manager.clone();
            tauri::async_runtime::spawn(async move {
                let mut interval = tokio::time::interval(std::time::Duration::from_secs(5));
                interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                loop {
                    interval.tick().await;
                    conn_manager_liveness.send_heartbeat_to_all().await;
                    conn_manager_liveness.prune_stale_peers(15_000).await;
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
            commands::send_chat_all,
            commands::send_control,
            commands::register_media_bridge,
            commands::clear_media_bridge,
            commands::ack_media_bridge_ready,
            commands::probe_media_bridge,
            commands::report_media_playback_status,
            commands::send_audio_all,
            commands::send_video_all,
            commands::init_codecs,
            commands::destroy_codecs,
            commands::reinit_video_encoder,
            commands::reinit_video_encoder_with_config,
            commands::get_pinned_name,
            commands::set_pinned_name,
            commands::toggle_persistent_identity,
        ])
        .run(tauri::generate_context!())
        .expect("error running nafaq");
}
