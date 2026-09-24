mod codec;
mod commands;
mod connection;
mod identity;
mod messages;
mod node;
mod presence;
mod protocol;
mod quality;
mod relay;
mod state;
mod video_transport;

#[cfg(test)]
mod scenarios;
#[cfg(test)]
mod test_support;

use std::collections::{hash_map::Entry, HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use codec::{AudioCodecState, AudioDecoder, VideoCodecState};
use connection::ConnectionManager;
use iroh::protocol::Router;
use iroh_gossip::net::Gossip;
use messages::{AudioPacket, Contact, ControlAction, Event, RelayStatusKind, VideoPacket};
use presence::PresenceManager;
use protocol::{NafaqDmProtocol, NafaqProtocol};
use state::{AppState, MediaBridgeState};
use tauri::ipc::InvokeResponseBody;
use tauri::{Emitter, Manager};
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_store::StoreExt;
use tokio::sync::{broadcast, Mutex};

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

/// Wrapping-aware high-water mark for a peer's audio datagrams. Returns how
/// many packets were lost before `sequence`, or `None` if it must be dropped:
/// a duplicate, or a late packet whose slot FEC/concealment already filled
/// (playing it would repeat 20 ms out of order and disturb decoder state).
fn audio_sequence_gap(previous: Option<u16>, sequence: u16) -> Option<u16> {
    let Some(previous) = previous else {
        return Some(0);
    };
    let advance = sequence.wrapping_sub(previous);
    if advance == 0 || advance >= 0x8000 {
        None
    } else {
        Some(advance - 1)
    }
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
    let mut packet = Vec::with_capacity(2 + peer_id_bytes.len() + 8 + 4 + 4 + 4 + jpeg.len());
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
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("nafaq=info".parse().unwrap()),
        )
        .init();

    let mut builder = tauri::Builder::default();

    builder = builder.plugin(tauri_plugin_os::init());
    builder = builder.plugin(tauri_plugin_opener::init());
    builder = builder.plugin(tauri_plugin_store::Builder::new().build());
    builder = builder.plugin(tauri_plugin_dialog::init());

    #[cfg(desktop)]
    {
        builder = builder
            .plugin(tauri_plugin_updater::Builder::new().build())
            .plugin(tauri_plugin_process::init());
    }

    builder
        .setup(move |app| {
            let loaded_identity = identity::load_or_create_persistent_identity(app.handle())?;
            let identity_status = loaded_identity.status.clone();
            let secret_key = loaded_identity.secret_key;

            // Initialize Iroh synchronously on the async runtime
            let (event_tx, _) = broadcast::channel::<Event>(256);
            let (audio_media_tx, _) = broadcast::channel::<AudioPacket>(256);
            // 64 frames ≈ 5s headroom at 12fps. The old capacity of 16 let a
            // brief decode/JPEG stall drop frames — including keyframes, which
            // froze video until the next one arrived.
            let (video_media_tx, _) = broadcast::channel::<VideoPacket>(64);
            let latest_ticket = Arc::new(Mutex::new(None));
            let relay_status = Arc::new(Mutex::new(RelayStatusKind::Starting));

            let audio_media_tx_for_setup = audio_media_tx.clone();
            let video_media_tx_for_setup = video_media_tx.clone();

            let conn_manager = Arc::new(ConnectionManager::new(
                event_tx.clone(),
                audio_media_tx.clone(),
                video_media_tx.clone(),
                latest_ticket.clone(),
            ));

            let conn_manager_for_rt = conn_manager.clone();
            let event_tx_for_presence = event_tx.clone();
            // A UDP bind failure (port conflict, VPN/firewall/sandbox denial) must
            // not take down the whole app before a window even shows. Retry a
            // couple of times with a short backoff — most such failures are
            // transient (another process still releasing the port, etc.) — and
            // if it's still failing, exit cleanly with a visible dialog instead
            // of panicking into a raw backtrace.
            const ENDPOINT_INIT_ATTEMPTS: u32 = 3;
            let endpoint_setup: anyhow::Result<(iroh::Endpoint, Router, Arc<PresenceManager>)> =
                tauri::async_runtime::handle().block_on(async {
                    let mut last_err = None;
                    for attempt in 1..=ENDPOINT_INIT_ATTEMPTS {
                        match node::create_endpoint_with_key(secret_key.clone()).await {
                            Ok(node::NafaqEndpoint {
                                endpoint,
                                address_lookup,
                            }) => {
                                tracing::info!("Node ID: {}", endpoint.id());

                                // Give connection manager a reference to the endpoint for mesh formation
                                conn_manager_for_rt.set_endpoint(endpoint.clone()).await;

                                let gossip = Gossip::builder().spawn(endpoint.clone());
                                let local_id = endpoint.id();
                                let presence = Arc::new(PresenceManager::new(
                                    gossip.clone(),
                                    local_id,
                                    event_tx_for_presence,
                                    address_lookup,
                                ));
                                conn_manager_for_rt.set_presence(presence.clone()).await;

                                let router = Router::builder(endpoint.clone())
                                    .accept(
                                        node::NAFAQ_ALPN,
                                        NafaqProtocol::new(conn_manager_for_rt.clone()),
                                    )
                                    .accept(
                                        node::NAFAQ_DM_ALPN,
                                        NafaqDmProtocol::new(conn_manager_for_rt.clone()),
                                    )
                                    .accept(iroh_gossip::ALPN, gossip)
                                    .spawn();

                                return Ok((endpoint, router, presence));
                            }
                            Err(e) => {
                                tracing::warn!(
                                    "Iroh endpoint init attempt {attempt}/{ENDPOINT_INIT_ATTEMPTS} failed: {e}"
                                );
                                last_err = Some(e);
                                if attempt < ENDPOINT_INIT_ATTEMPTS {
                                    tokio::time::sleep(Duration::from_millis(500 * attempt as u64))
                                        .await;
                                }
                            }
                        }
                    }
                    Err(last_err.expect("loop always records an error before exhausting attempts"))
                });

            let (endpoint, router, presence) = match endpoint_setup {
                Ok(parts) => parts,
                Err(e) => {
                    tracing::error!("Failed to initialize networking after retries: {e}");
                    let handle = app.handle().clone();
                    let message = format!(
                        "nafaq couldn't start its networking (endpoint init failed after {ENDPOINT_INIT_ATTEMPTS} attempts):\n\n{e}\n\nCheck for port conflicts, VPN/firewall rules, or sandbox network restrictions, then relaunch."
                    );
                    handle
                        .dialog()
                        .message(message)
                        .title("nafaq failed to start")
                        .kind(tauri_plugin_dialog::MessageDialogKind::Error)
                        .blocking_show();
                    handle.exit(1);
                    return Ok(());
                }
            };

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

            let endpoint_for_relay = endpoint.clone();
            let latest_ticket_for_relay = latest_ticket.clone();
            let relay_status_for_relay = relay_status.clone();
            let event_tx_for_relay = event_tx.clone();

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
                identity_status,
                latest_ticket,
                relay_status,
                presence: presence.clone(),
            };

            let media_bridge_ref = media_bridge.current.clone();

            app.manage(app_state);
            app.manage(media_bridge);

            // Track presence for every contact on startup.
            let presence_for_bootstrap = presence.clone();
            let app_handle_for_bootstrap = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let store = match app_handle_for_bootstrap.store("contacts.json") {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::warn!("contacts store unavailable for presence bootstrap: {e}");
                        return;
                    }
                };
                let contacts: Vec<Contact> = store
                    .get("contacts")
                    .and_then(|v| serde_json::from_value(v).ok())
                    .unwrap_or_default();
                for contact in contacts {
                    if let Err(e) = presence_for_bootstrap.track_contact(&contact.node_id).await {
                        tracing::warn!("presence track failed for {}: {e}", contact.node_id);
                    }
                }
            });

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
                                Event::PeerConnectionStatusChanged { .. } => {
                                    "peer-connection-status-changed"
                                }
                                Event::Error { .. } => "nafaq-error",
                                Event::QualityProfileChanged { .. } => "quality-profile-changed",
                                Event::RelayStatusChanged { .. } => "relay-status-changed",
                                Event::TicketRefreshed { .. } => "ticket-refreshed",
                                Event::DmReceived { .. } => "dm-received",
                                Event::DmConnected { .. } => "dm-connected",
                                Event::DmDisconnected { .. } => "dm-disconnected",
                                Event::CallInviteReceived { .. } => "call-invite-received",
                                Event::CallDeclineReceived { .. } => "call-decline-received",
                                Event::CallCancelReceived { .. } => "call-cancel-received",
                                Event::DmAckReceived { .. } => "dm-ack-received",
                                Event::DmFileSaved { .. } => "dm-file-saved",
                                Event::DmFileTransferFailed { .. } => "dm-file-transfer-failed",
                                Event::DmFileProgress { .. } => "dm-file-progress",
                                Event::PresenceChanged { .. } => "presence-changed",
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

            tauri::async_runtime::spawn(relay::monitor_relay(
                endpoint_for_relay,
                latest_ticket_for_relay,
                relay_status_for_relay,
                event_tx_for_relay,
            ));

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
                        Ok(Event::ControlReceived {
                            peer_id,
                            action: ControlAction::PerPeerQualityBps { bitrate_bps },
                        }) => {
                            // Clamp so a misbehaving peer can't force the encoder
                            // to a useless bitrate; 0 clears the override.
                            let clamped = if bitrate_bps == 0 {
                                0
                            } else {
                                bitrate_bps.clamp(50_000, 2_000_000)
                            };
                            conn_manager_for_control
                                .set_peer_outbound_bitrate(&peer_id, clamped)
                                .await;
                            tracing::debug!(
                                "Peer {peer_id} requested outbound bitrate {clamped} bps"
                            );
                        }
                        Ok(Event::TicketRefreshed { ticket }) => {
                            conn_manager_for_control
                                .send_self_announce_to_all(ticket)
                                .await;
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
                let mut top_speakers: HashSet<String> = HashSet::new();
                let mut last_speaker_update = std::time::Instant::now();
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

                            let Some(lost_count) = audio_sequence_gap(
                                last_sequence.get(&peer_id).copied(),
                                packet.sequence,
                            ) else {
                                continue;
                            };
                            last_sequence.insert(peer_id.clone(), packet.sequence);

                            // Lightweight energy proxy from Opus payload size (no decode
                            // needed), smoothed with an EWMA so one large packet (e.g. a
                            // DTX burst) can't instantly displace a genuinely loud
                            // speaker. Updated after the sequence guard so out-of-order/
                            // duplicate packets don't skew the estimate.
                            let prev_energy = peer_energy.get(&peer_id).copied().unwrap_or(0.0);
                            let energy_proxy = 0.3 * payload.len() as f32 + 0.7 * prev_energy;
                            peer_energy.insert(peer_id.clone(), energy_proxy);

                            // Selective decode at 5+ peers: skip quiet speakers
                            let peer_count = last_active.len();
                            if peer_count >= 5 {
                                // Recompute top speakers at most every 200ms
                                let now = std::time::Instant::now();
                                if now.duration_since(last_speaker_update).as_millis() >= 200 {
                                    last_speaker_update = now;
                                    let mut energies: Vec<(String, f32)> =
                                        peer_energy.iter().map(|(k, v)| (k.clone(), *v)).collect();
                                    energies.sort_by(|a, b| {
                                        b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal)
                                    });
                                    top_speakers =
                                        energies.iter().take(3).map(|(id, _)| id.clone()).collect();
                                }
                                // Allow new peers through, skip quiet peers
                                if !top_speakers.is_empty()
                                    && peer_energy.contains_key(&peer_id)
                                    && !top_speakers.contains(&peer_id)
                                {
                                    continue;
                                }
                            }

                            // Decode while holding the decoders lock, but dispatch to
                            // the bridge after releasing it — holding it across the
                            // IPC below would stall destroy_codecs and peer cleanup.
                            let pcm_frames: Vec<Vec<i16>> = {
                                let mut decoders = codec_audio.decoders.lock().await;
                                let decoder = match decoders.entry(peer_id.clone()) {
                                    Entry::Occupied(entry) => entry.into_mut(),
                                    Entry::Vacant(entry) => match AudioDecoder::new() {
                                        Ok(decoder) => entry.insert(decoder),
                                        Err(e) => {
                                            tracing::warn!(
                                                "Opus decoder init failed for {peer_id}: {e}"
                                            );
                                            continue;
                                        }
                                    },
                                };
                                let mut frames = Vec::with_capacity(2);
                                if lost_count > 0 {
                                    // Opus in-band FEC can only reconstruct the single
                                    // frame preceding this packet. Step the decoder
                                    // through PLC for earlier missing frames (capped —
                                    // long gaps aren't worth filling with concealment),
                                    // then recover the last one from FEC.
                                    for _ in 0..(lost_count - 1).min(2) {
                                        if let Some(pcm) = decoder.decode(&[], true) {
                                            frames.push(pcm);
                                        }
                                    }
                                    if let Some(pcm) = decoder.decode(&payload, true) {
                                        frames.push(pcm);
                                    }
                                }
                                if let Some(pcm) = decoder.decode(&payload, false) {
                                    frames.push(pcm);
                                }
                                frames
                            };

                            if pcm_frames.is_empty() {
                                continue;
                            }
                            let registration = audio_bridge.lock().await.clone();
                            let channel = registration.and_then(|r| r.audio_channel);
                            let total = pcm_frames.len() as u64;
                            for (i, pcm) in pcm_frames.iter().enumerate() {
                                // Concealment frames precede the real one; back-date
                                // them one 20ms frame each so playback order holds.
                                let ts = timestamp.saturating_sub(20 * (total - 1 - i as u64));
                                let raw: Vec<u8> =
                                    pcm.iter().flat_map(|s| s.to_le_bytes()).collect();
                                if let Some(channel) = &channel {
                                    let Some(channel_payload) =
                                        pack_audio_channel_packet(&peer_id, ts, &raw)
                                    else {
                                        continue;
                                    };
                                    let _ = channel.send(InvokeResponseBody::Raw(channel_payload));
                                } else {
                                    let _ = app_handle_audio.emit(
                                        "audio-received",
                                        AudioEvent {
                                            peer_id: peer_id.clone(),
                                            data: B64.encode(&raw),
                                            timestamp: ts,
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
            let conn_manager_video = conn_manager.clone();

            tauri::async_runtime::spawn(async move {
                let mut video_rx = video_media_tx_for_setup.subscribe();
                // Peers whose frames were lost to broadcast lag: their chain is
                // broken, so skip deltas until a keyframe comes through.
                let mut resync: HashSet<String> = HashSet::new();
                loop {
                    match video_rx.recv().await {
                        Ok(packet) => {
                            let kf = codec::is_keyframe(&packet.payload);
                            if !resync.is_empty() {
                                if kf {
                                    resync.remove(&packet.peer_id);
                                } else if resync.contains(&packet.peer_id) {
                                    continue;
                                }
                            }
                            // Check bridge registration first — if WebCodecs is active,
                            // forward raw NALUs and skip the decode+JPEG path entirely.
                            // Only on the binary channel: event mode can't carry
                            // NALUs, so it always takes the decode+JPEG path.
                            let registration = video_bridge.lock().await.clone();
                            if let Some(ref reg) = registration {
                                if reg.webcodecs_active && reg.video_channel.is_some() {
                                    if let Some(channel) = &reg.video_channel {
                                        if let Some(raw_packet) = pack_video_channel_raw_nalu(
                                            &packet.peer_id,
                                            packet.timestamp_ms,
                                            &packet.payload,
                                            kf,
                                        ) {
                                            let _ = channel.send(InvokeResponseBody::Raw(raw_packet));
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
                            let jpeg_result = video_runtime_handle
                                .spawn(async move {
                                    let mut decoders = codec_video_clone.decoders.lock().await;
                                    let decoder = match decoders.entry(peer_id_clone) {
                                        Entry::Occupied(entry) => entry.into_mut(),
                                        Entry::Vacant(entry) => match codec::VideoDecoder::new() {
                                            Ok(decoder) => entry.insert(decoder),
                                            Err(e) => {
                                                tracing::warn!("H264 decoder init failed: {e}");
                                                return None;
                                            }
                                        },
                                    };
                                    decoder.decode_rgba(&payload).and_then(|(rgba, w, h)| {
                                        codec::encode_jpeg(&rgba, w, h, 70).map(|j| (j, w, h))
                                    })
                                })
                                .await
                                .ok()
                                .flatten();
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
                                        let _ = channel.send(InvokeResponseBody::Raw(channel_payload));
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
                            // Unknown which peers lost frames: resync them all
                            // and ask for keyframes now instead of waiting.
                            resync.extend(conn_manager_video.peer_ids().await);
                            conn_manager_video.request_keyframes_from_all_peers().await;
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
            let video_codec_stats = video_codec.clone();
            tauri::async_runtime::spawn(async move {
                let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
                interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                // lost_packets from path stats is cumulative over the connection
                // lifetime; congestion must be judged on the per-tick delta.
                let mut last_lost: HashMap<String, u64> = HashMap::new();
                let mut controller = quality::SendQualityController::new();
                let mut last_level: Option<u8> = None;
                loop {
                    interval.tick().await;
                    let snapshot = conn_manager_stats.snapshot_network_stats().await;
                    let mut dropped = conn_manager_stats.take_dropped_video_frames().await;
                    let mut seen: HashSet<String> = HashSet::with_capacity(snapshot.len());
                    let mut signals = Vec::with_capacity(snapshot.len());
                    for stats in &snapshot {
                        seen.insert(stats.peer_id.clone());
                        let prev = last_lost
                            .insert(stats.peer_id.clone(), stats.lost_packets)
                            .unwrap_or(stats.lost_packets);
                        signals.push((
                            stats.peer_id.clone(),
                            quality::PeerSignal {
                                rtt_ms: stats.rtt_ms,
                                lost_packets: stats.lost_packets.saturating_sub(prev),
                                dropped_video_frames: dropped
                                    .remove(&stats.peer_id)
                                    .unwrap_or_default(),
                            },
                        ));
                        let _ = app_handle_stats.emit("network-stats", &stats);
                    }
                    last_lost.retain(|peer_id, _| seen.contains(peer_id));

                    let level = controller.tick(&signals);
                    if last_level != Some(level) {
                        last_level = Some(level);
                        let _ = app_handle_stats.emit(
                            "send-quality-changed",
                            serde_json::json!({ "level": level }),
                        );
                    }
                    // The encoder is shared across peers: apply the controller
                    // level, then any stricter peer-requested cap.
                    let peer_cap = conn_manager_stats.min_peer_outbound_bitrate().await;
                    if let Some(encoder) = video_codec_stats.encoder.lock().await.as_mut() {
                        let target = quality::apply_level(
                            quality::VideoProfile {
                                bitrate_bps: encoder.base_bitrate_bps(),
                                fps: encoder.base_fps().round() as u32,
                                max_width: 0,
                                max_height: 0,
                            },
                            level,
                        );
                        let bitrate = if peer_cap > 0 {
                            target.bitrate_bps.min(peer_cap)
                        } else {
                            target.bitrate_bps
                        };
                        encoder.set_runtime_rate(bitrate, target.fps as f32);
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
                    conn_manager_liveness.maintain_peer_liveness().await;
                }
            });

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_node_info,
            commands::create_call,
            commands::join_call,
            commands::end_call,
            commands::send_encoded_video_all,
            commands::leave_call_session,
            commands::cancel_call,
            commands::send_call_decline,
            commands::send_chat,
            commands::send_chat_all,
            commands::send_control,
            commands::register_media_bridge,
            commands::clear_media_bridge,
            commands::probe_media_bridge,
            commands::send_audio_all,
            commands::send_video_all,
            commands::init_codecs,
            commands::destroy_codecs,
            commands::reinit_video_encoder,
            commands::reinit_video_encoder_with_config,
            commands::get_quality_profile,
            commands::get_pinned_name,
            commands::set_pinned_name,
            commands::toggle_persistent_identity,
            commands::get_settings,
            commands::update_settings,
            commands::get_contacts,
            commands::add_contact,
            commands::remove_contact,
            commands::get_presence_snapshot,
            commands::connect_dm,
            commands::send_dm,
            commands::send_file,
            commands::disconnect_dm,
        ])
        .run(tauri::generate_context!())
        .expect("error running nafaq");
}

#[cfg(test)]
mod audio_sequence_tests {
    use super::audio_sequence_gap;

    #[test]
    fn first_packet_and_in_order_packets_have_no_gap() {
        assert_eq!(audio_sequence_gap(None, 42), Some(0));
        assert_eq!(audio_sequence_gap(Some(42), 43), Some(0));
    }

    #[test]
    fn a_jump_reports_the_missing_packets() {
        assert_eq!(audio_sequence_gap(Some(5), 8), Some(2));
    }

    #[test]
    fn duplicates_and_late_packets_are_dropped() {
        assert_eq!(audio_sequence_gap(Some(7), 7), None);
        assert_eq!(audio_sequence_gap(Some(7), 6), None);
    }

    #[test]
    fn wraps_around_u16() {
        assert_eq!(audio_sequence_gap(Some(u16::MAX), 0), Some(0));
        assert_eq!(audio_sequence_gap(Some(u16::MAX - 1), 1), Some(2));
        assert_eq!(audio_sequence_gap(Some(0), u16::MAX), None);
    }
}
