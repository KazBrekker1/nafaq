use base64::Engine;
use tauri::{ipc::Channel, Emitter, State};

use crate::codec::{AudioEncoder, VideoEncoder};
use crate::messages::{
    ControlAction, MediaBridgeMode, MediaBridgeRegistration as MediaBridgeRegistrationRequest,
    MediaPlaybackStatus, MediaReceiveAudioMode, MediaReceiveVideoMode, MediaSendIngressMode,
    MediaSessionProfile,
};
use crate::node;
use crate::state::{AppState, MediaBridgeRegistration, MediaBridgeState};

const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

const MAX_PEER_ID_LEN: usize = 256;
const MAX_TICKET_LEN: usize = 4096;
const MAX_CHAT_LEN: usize = 64 * 1024; // 64 KB
const MAX_RESOLUTION: u32 = 4096;
const PROBE_PEER_ID: &str = "__bridge_probe__";

#[derive(Clone, serde::Serialize)]
struct ProbeAudioEvent {
    peer_id: &'static str,
    data: &'static str,
    timestamp: u64,
}

fn pack_audio_probe_packet() -> Vec<u8> {
    let peer_id_bytes = PROBE_PEER_ID.as_bytes();
    let mut packet = Vec::with_capacity(2 + peer_id_bytes.len() + 8 + 4);
    packet.extend_from_slice(&(peer_id_bytes.len() as u16).to_le_bytes());
    packet.extend_from_slice(peer_id_bytes);
    packet.extend_from_slice(&0u64.to_le_bytes());
    packet.extend_from_slice(&0u32.to_le_bytes());
    packet
}

fn validate_peer_id(peer_id: &str) -> Result<(), String> {
    if peer_id.is_empty() || peer_id.len() > MAX_PEER_ID_LEN {
        return Err("Invalid peer_id".into());
    }
    Ok(())
}

fn validate_resolution(width: u32, height: u32) -> Result<(), String> {
    if width == 0 || width > MAX_RESOLUTION || height == 0 || height > MAX_RESOLUTION {
        return Err("Invalid resolution".into());
    }
    Ok(())
}

#[derive(serde::Serialize)]
pub struct NodeInfo {
    pub id: String,
    pub ticket: String,
}

#[tauri::command]
pub async fn get_node_info(state: State<'_, AppState>) -> Result<NodeInfo, String> {
    let ticket = node::generate_ticket(&state.endpoint);
    Ok(NodeInfo {
        id: state.endpoint.id().to_string(),
        ticket,
    })
}

#[tauri::command]
pub async fn create_call(state: State<'_, AppState>) -> Result<String, String> {
    Ok(node::generate_ticket(&state.endpoint))
}

#[tauri::command]
pub async fn join_call(
    ticket: String,
    state: State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<String, String> {
    if ticket.len() > MAX_TICKET_LEN {
        return Err("Ticket too large".into());
    }
    let endpoint_ticket = node::parse_ticket(&ticket).map_err(|e| e.to_string())?;
    let addr = endpoint_ticket.endpoint_addr().clone();
    let peer_id = state
        .conn_manager
        .connect_to_peer(&state.endpoint, addr)
        .await
        .map_err(|e| e.to_string())?;
    let _ = app.emit("peer-connected", &peer_id);
    Ok(peer_id)
}

#[tauri::command]
pub async fn end_call(peer_id: String, state: State<'_, AppState>) -> Result<(), String> {
    validate_peer_id(&peer_id)?;
    state
        .conn_manager
        .disconnect_peer(&peer_id)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn send_chat(
    peer_id: String,
    message: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    validate_peer_id(&peer_id)?;
    if message.len() > MAX_CHAT_LEN {
        return Err("Message too long".into());
    }
    state
        .conn_manager
        .send_chat(&peer_id, &message)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn send_chat_all(
    message: String,
    state: State<'_, AppState>,
) -> Result<Vec<String>, String> {
    if message.len() > MAX_CHAT_LEN {
        return Err("Message too long".into());
    }
    Ok(state.conn_manager.send_chat_to_all(&message).await)
}

#[tauri::command]
pub async fn send_control(
    peer_id: String,
    action: ControlAction,
    state: State<'_, AppState>,
) -> Result<(), String> {
    validate_peer_id(&peer_id)?;
    state
        .conn_manager
        .send_control(&peer_id, &action)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn register_media_bridge(
    registration: MediaBridgeRegistrationRequest,
    audio: Channel<Vec<u8>>,
    video: Channel<Vec<u8>>,
    bridge: State<'_, MediaBridgeState>,
) -> Result<MediaSessionProfile, String> {
    if registration.session_id.is_empty() {
        return Err("Missing session_id".into());
    }

    let selected_mode = if registration
        .preferred_bridge_modes
        .contains(&MediaBridgeMode::ChannelBinary)
    {
        MediaBridgeMode::ChannelBinary
    } else {
        MediaBridgeMode::EventBase64
    };

    let profile = MediaSessionProfile {
        session_id: registration.session_id.clone(),
        receive_bridge_mode: selected_mode,
        receive_video_mode: MediaReceiveVideoMode::DecodedJpeg,
        receive_audio_mode: MediaReceiveAudioMode::DecodedPcm,
        send_ingress_mode: MediaSendIngressMode::InvokeRaw,
        playback_ready: registration.playback_ready,
        bridge_ready: false,
    };

    *bridge.current.lock().await = Some(MediaBridgeRegistration {
        profile: profile.clone(),
        audio_channel: if selected_mode == MediaBridgeMode::ChannelBinary {
            Some(audio)
        } else {
            None
        },
        video_channel: if selected_mode == MediaBridgeMode::ChannelBinary {
            Some(video)
        } else {
            None
        },
    });

    tracing::info!(
        "Registered media bridge session={} mode={:?}",
        profile.session_id,
        profile.receive_bridge_mode
    );

    Ok(profile)
}

#[tauri::command]
pub async fn clear_media_bridge(
    session_id: String,
    bridge: State<'_, MediaBridgeState>,
) -> Result<(), String> {
    let mut guard = bridge.current.lock().await;
    if guard
        .as_ref()
        .is_some_and(|current| current.profile.session_id == session_id)
    {
        *guard = None;
        tracing::info!("Cleared media bridge session={session_id}");
    }
    Ok(())
}

#[tauri::command]
pub async fn ack_media_bridge_ready(
    session_id: String,
    bridge: State<'_, MediaBridgeState>,
) -> Result<(), String> {
    let mut guard = bridge.current.lock().await;
    let Some(current) = guard.as_mut() else {
        return Err("No registered media bridge".into());
    };
    if current.profile.session_id != session_id {
        return Err("Media bridge session mismatch".into());
    }
    current.profile.bridge_ready = true;
    tracing::info!("Media bridge ready session={session_id}");
    Ok(())
}

#[tauri::command]
pub async fn report_media_playback_status(
    status: MediaPlaybackStatus,
    bridge: State<'_, MediaBridgeState>,
) -> Result<(), String> {
    let mut guard = bridge.current.lock().await;
    let Some(current) = guard.as_mut() else {
        return Err("No registered media bridge".into());
    };
    if current.profile.session_id != status.session_id {
        return Err("Media bridge session mismatch".into());
    }
    current.profile.playback_ready = status.audio_ready || status.video_ready;
    if let Some(last_failure) = status.last_failure.as_deref() {
        tracing::warn!(
            "Media playback degraded session={} failure={last_failure}",
            status.session_id
        );
    } else {
        tracing::info!(
            "Media playback status session={} audio_ready={} video_ready={}",
            status.session_id,
            status.audio_ready,
            status.video_ready
        );
    }
    Ok(())
}

#[tauri::command]
pub async fn probe_media_bridge(
    session_id: String,
    bridge: State<'_, MediaBridgeState>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    let current = bridge.current.lock().await.clone();
    let Some(current) = current else {
        return Err("No registered media bridge".into());
    };
    if current.profile.session_id != session_id {
        return Err("Media bridge session mismatch".into());
    }

    match current.profile.receive_bridge_mode {
        MediaBridgeMode::ChannelBinary => {
            if let Some(channel) = current.audio_channel {
                let _ = channel.send(pack_audio_probe_packet());
            } else {
                return Err("Missing audio probe channel".into());
            }
        }
        MediaBridgeMode::EventBase64 => {
            let _ = app.emit(
                "audio-received",
                ProbeAudioEvent {
                    peer_id: PROBE_PEER_ID,
                    data: "",
                    timestamp: 0,
                },
            );
        }
    }

    tracing::info!("Probed media bridge session={session_id}");
    Ok(())
}

#[tauri::command]
pub async fn init_codecs(
    width: u32,
    height: u32,
    state: State<'_, AppState>,
) -> Result<(), String> {
    validate_resolution(width, height)?;
    *state.audio_codec.encoder.lock().await = Some(AudioEncoder::new());
    // Audio decoders are created per-peer on demand — no init needed
    *state.video_codec.encoder.lock().await = Some(VideoEncoder::new(width, height));
    tracing::info!("Codecs initialized: {width}x{height}");
    Ok(())
}

#[tauri::command]
pub async fn destroy_codecs(state: State<'_, AppState>) -> Result<(), String> {
    *state.audio_codec.encoder.lock().await = None;
    state.audio_codec.decoders.lock().await.clear();
    *state.video_codec.encoder.lock().await = None;
    state.video_codec.decoders.lock().await.clear();
    tracing::info!("Codecs destroyed");
    Ok(())
}

#[tauri::command]
pub async fn reinit_video_encoder(
    width: u32,
    height: u32,
    state: State<'_, AppState>,
) -> Result<(), String> {
    validate_resolution(width, height)?;
    *state.video_codec.encoder.lock().await = Some(VideoEncoder::new(width, height));
    tracing::info!("Video encoder reinitialized: {width}x{height}");
    Ok(())
}

#[tauri::command]
pub async fn reinit_video_encoder_with_config(
    width: u32,
    height: u32,
    bitrate_bps: u32,
    fps: f32,
    state: State<'_, AppState>,
) -> Result<(), String> {
    validate_resolution(width, height)?;
    *state.video_codec.encoder.lock().await =
        Some(VideoEncoder::new_with_config(width, height, bitrate_bps, fps));
    tracing::info!(
        "Video encoder reinitialized: {width}x{height} @ {bitrate_bps}bps {fps}fps"
    );
    Ok(())
}

// ── Encode-once broadcast commands ──────────────────────────────────

async fn encode_and_send_audio_all(
    state: &AppState,
    pcm_bytes: &[u8],
    timestamp: u64,
) -> Result<(), String> {
    let pcm: Vec<i16> = pcm_bytes
        .chunks_exact(2)
        .map(|c| i16::from_le_bytes([c[0], c[1]]))
        .collect();

    let mut codec = state.audio_codec.encoder.lock().await;
    let encoded = match codec.as_mut() {
        Some(c) => c.encode(&pcm),
        None => return Ok(()),
    };
    drop(codec);

    if let Some(encoded) = encoded {
        state
            .conn_manager
            .send_audio_to_all(&encoded, timestamp)
            .await
            .map_err(|e| e.to_string())
    } else {
        Ok(())
    }
}

/// Encode audio once and send to all peers
#[tauri::command]
pub async fn send_audio_all(
    request: tauri::ipc::Request<'_>,
    state: State<'_, AppState>,
) -> Result<(), String> {
    match request.body() {
        tauri::ipc::InvokeBody::Raw(data) => {
            if data.len() < 8 {
                return Err("Payload too short".into());
            }
            let timestamp = u64::from_le_bytes(data[..8].try_into().unwrap());
            encode_and_send_audio_all(&state, &data[8..], timestamp).await
        }
        tauri::ipc::InvokeBody::Json(value) => {
            let data_b64 = value
                .get("data")
                .and_then(|v| v.as_str())
                .ok_or("Missing data")?;
            let timestamp = value.get("timestamp").and_then(|v| v.as_u64()).unwrap_or(0);
            let pcm_bytes = B64
                .decode(data_b64)
                .map_err(|e| format!("base64 decode error: {e}"))?;
            encode_and_send_audio_all(&state, &pcm_bytes, timestamp).await
        }
    }
}

/// Encode video once and send to all peers
#[tauri::command]
pub async fn send_video_all(
    request: tauri::ipc::Request<'_>,
    state: State<'_, AppState>,
) -> Result<(), String> {
    if !state.conn_manager.has_peers().await {
        return Ok(());
    }

    match request.body() {
        tauri::ipc::InvokeBody::Raw(data) => {
            // Desktop path: [w:u32LE][h:u32LE][kf:u8][ts:u64LE][rgba...]
            if data.len() < 17 {
                return Err("Payload too short".into());
            }
            let mut offset = 0usize;
            let width = u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap());
            offset += 4;
            let height = u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap());
            offset += 4;
            let keyframe = data[offset] != 0;
            offset += 1;
            let timestamp = u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap());
            offset += 8;
            let rgba = &data[offset..];

            validate_resolution(width, height)?;

            encode_and_send_video_all(&state, rgba, width, height, keyframe, timestamp).await
        }
        tauri::ipc::InvokeBody::Json(value) => {
            // Android fallback
            let data_b64 = value
                .get("data")
                .and_then(|v| v.as_str())
                .ok_or("Missing data")?;
            let width = value
                .get("width")
                .and_then(|v| v.as_u64())
                .ok_or("Missing width")? as u32;
            let height = value
                .get("height")
                .and_then(|v| v.as_u64())
                .ok_or("Missing height")? as u32;
            let keyframe = value
                .get("keyframe")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let timestamp = value.get("timestamp").and_then(|v| v.as_u64()).unwrap_or(0);

            validate_resolution(width, height)?;

            let rgba = B64
                .decode(data_b64)
                .map_err(|e| format!("base64 decode error: {e}"))?;

            encode_and_send_video_all(&state, &rgba, width, height, keyframe, timestamp).await
        }
    }
}

async fn encode_and_send_video_all(
    state: &AppState,
    rgba: &[u8],
    width: u32,
    height: u32,
    keyframe: bool,
    timestamp: u64,
) -> Result<(), String> {
    let force_keyframe = state.conn_manager.consume_pending_keyframe_requests().await;

    let encoded = {
        let mut video = state.video_codec.encoder.lock().await;
        let encoder = match video.as_mut() {
            Some(encoder) => encoder,
            None => return Ok(()),
        };
        encoder.encode(rgba, width, height, keyframe || force_keyframe)
    };

    if let Some(encoded) = encoded {
        state
            .conn_manager
            .send_video_frame_all(&encoded, timestamp)
            .await
            .map_err(|e| e.to_string())
    } else {
        Ok(())
    }
}
