use base64::Engine;
use tauri::{Emitter, State};

use crate::codec::{AudioDecoder, AudioEncoder, VideoDecoder, VideoEncoder};
use crate::messages::ControlAction;
use crate::node;
use crate::state::AppState;

const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

const MAX_PEER_ID_LEN: usize = 256;
const MAX_TICKET_LEN: usize = 4096;
const MAX_CHAT_LEN: usize = 64 * 1024; // 64 KB
const MAX_RESOLUTION: u32 = 4096;

fn validate_peer_id(peer_id: &str) -> Result<(), String> {
    if peer_id.is_empty() || peer_id.len() > MAX_PEER_ID_LEN {
        return Err("Invalid peer_id".into());
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
pub async fn init_codecs(
    width: u32,
    height: u32,
    state: State<'_, AppState>,
) -> Result<(), String> {
    if width == 0 || width > MAX_RESOLUTION || height == 0 || height > MAX_RESOLUTION {
        return Err("Invalid resolution".into());
    }
    *state.codec.audio_encoder.lock().await = Some(AudioEncoder::new());
    *state.codec.audio_decoder.lock().await = Some(AudioDecoder::new());
    *state.codec.video_encoder.lock().await = Some(VideoEncoder::new(width, height));
    *state.codec.video_decoder.lock().await = Some(VideoDecoder::new());
    tracing::info!("Codecs initialized: {width}x{height}");
    Ok(())
}

#[tauri::command]
pub async fn destroy_codecs(state: State<'_, AppState>) -> Result<(), String> {
    *state.codec.audio_encoder.lock().await = None;
    *state.codec.audio_decoder.lock().await = None;
    *state.codec.video_encoder.lock().await = None;
    *state.codec.video_decoder.lock().await = None;
    tracing::info!("Codecs destroyed");
    Ok(())
}

#[tauri::command]
pub async fn reinit_video_encoder(
    width: u32,
    height: u32,
    state: State<'_, AppState>,
) -> Result<(), String> {
    if width == 0 || width > MAX_RESOLUTION || height == 0 || height > MAX_RESOLUTION {
        return Err("Invalid resolution".into());
    }
    *state.codec.video_encoder.lock().await = Some(VideoEncoder::new(width, height));
    tracing::info!("Video encoder reinitialized: {width}x{height}");
    Ok(())
}

#[tauri::command]
pub async fn send_video(
    request: tauri::ipc::Request<'_>,
    state: State<'_, AppState>,
) -> Result<(), String> {
    match request.body() {
        tauri::ipc::InvokeBody::Raw(data) => {
            // Desktop path: binary header + RGBA payload
            // Format: [peer_id_len:u8][peer_id][w:u32LE][h:u32LE][kf:u8][ts:u64LE][rgba...]
            if data.is_empty() {
                return Err("Empty payload".into());
            }
            let mut offset = 0usize;
            let peer_id_len = data[offset] as usize;
            offset += 1;
            if data.len() < offset + peer_id_len + 17 {
                return Err("Payload too short".into());
            }
            let peer_id =
                String::from_utf8_lossy(&data[offset..offset + peer_id_len]).to_string();
            offset += peer_id_len;
            let width =
                u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap());
            offset += 4;
            let height =
                u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap());
            offset += 4;
            let keyframe = data[offset] != 0;
            offset += 1;
            let timestamp =
                u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap());
            offset += 8;
            let rgba = &data[offset..];

            validate_peer_id(&peer_id)?;
            if width == 0 || width > MAX_RESOLUTION || height == 0 || height > MAX_RESOLUTION {
                return Err("Invalid resolution".into());
            }

            let mut codec = state.codec.video_encoder.lock().await;
            let encoded = match codec.as_mut() {
                Some(c) => c.encode(rgba, width, height, keyframe),
                None => return Ok(()), // codecs not initialized
            };
            drop(codec);

            if let Some(encoded) = encoded {
                state
                    .conn_manager
                    .send_video(&peer_id, &encoded, timestamp)
                    .await
                    .map_err(|e| e.to_string())
            } else {
                Ok(())
            }
        }
        tauri::ipc::InvokeBody::Json(value) => {
            // Android fallback: JSON with base64-encoded RGBA
            let peer_id = value
                .get("peerId")
                .and_then(|v| v.as_str())
                .ok_or("Missing peerId")?
                .to_string();
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
            let timestamp = value
                .get("timestamp")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);

            validate_peer_id(&peer_id)?;
            if width == 0 || width > MAX_RESOLUTION || height == 0 || height > MAX_RESOLUTION {
                return Err("Invalid resolution".into());
            }

            let rgba = B64
                .decode(data_b64)
                .map_err(|e| format!("base64 decode error: {e}"))?;

            let mut codec = state.codec.video_encoder.lock().await;
            let encoded = match codec.as_mut() {
                Some(c) => c.encode(&rgba, width, height, keyframe),
                None => return Ok(()),
            };
            drop(codec);

            if let Some(encoded) = encoded {
                state
                    .conn_manager
                    .send_video(&peer_id, &encoded, timestamp)
                    .await
                    .map_err(|e| e.to_string())
            } else {
                Ok(())
            }
        }
    }
}

#[tauri::command]
pub async fn send_audio(
    request: tauri::ipc::Request<'_>,
    state: State<'_, AppState>,
) -> Result<(), String> {
    match request.body() {
        tauri::ipc::InvokeBody::Raw(data) => {
            // Desktop path: binary header + PCM payload
            // Format: [peer_id_len:u8][peer_id][ts:u64LE][pcm...]
            if data.is_empty() {
                return Err("Empty payload".into());
            }
            let mut offset = 0usize;
            let peer_id_len = data[offset] as usize;
            offset += 1;
            if data.len() < offset + peer_id_len + 8 {
                return Err("Payload too short".into());
            }
            let peer_id =
                String::from_utf8_lossy(&data[offset..offset + peer_id_len]).to_string();
            offset += peer_id_len;
            let timestamp =
                u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap());
            offset += 8;
            let pcm_bytes = &data[offset..];

            validate_peer_id(&peer_id)?;

            let pcm: Vec<i16> = pcm_bytes
                .chunks_exact(2)
                .map(|c| i16::from_le_bytes([c[0], c[1]]))
                .collect();

            let mut codec = state.codec.audio_encoder.lock().await;
            let encoded = match codec.as_mut() {
                Some(c) => c.encode(&pcm),
                None => return Ok(()),
            };
            drop(codec);

            if let Some(encoded) = encoded {
                state
                    .conn_manager
                    .send_audio(&peer_id, &encoded, timestamp)
                    .await
                    .map_err(|e| e.to_string())
            } else {
                Ok(())
            }
        }
        tauri::ipc::InvokeBody::Json(value) => {
            // Android fallback: JSON with base64-encoded PCM
            let peer_id = value
                .get("peerId")
                .and_then(|v| v.as_str())
                .ok_or("Missing peerId")?
                .to_string();
            let data_b64 = value
                .get("data")
                .and_then(|v| v.as_str())
                .ok_or("Missing data")?;
            let timestamp = value
                .get("timestamp")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);

            validate_peer_id(&peer_id)?;

            let pcm_bytes = B64
                .decode(data_b64)
                .map_err(|e| format!("base64 decode error: {e}"))?;

            let pcm: Vec<i16> = pcm_bytes
                .chunks_exact(2)
                .map(|c| i16::from_le_bytes([c[0], c[1]]))
                .collect();

            let mut codec = state.codec.audio_encoder.lock().await;
            let encoded = match codec.as_mut() {
                Some(c) => c.encode(&pcm),
                None => return Ok(()),
            };
            drop(codec);

            if let Some(encoded) = encoded {
                state
                    .conn_manager
                    .send_audio(&peer_id, &encoded, timestamp)
                    .await
                    .map_err(|e| e.to_string())
            } else {
                Ok(())
            }
        }
    }
}
