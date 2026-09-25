use std::collections::HashMap;

use base64::Engine;
use tauri::{
    ipc::{Channel, InvokeResponseBody},
    Emitter, State,
};
use tauri_plugin_store::StoreExt;

use crate::codec::{AudioEncoder, VideoEncoder};
use crate::identity;
use crate::messages::{
    Contact, ControlAction, DmMessage, Event, MediaBridgeMode,
    MediaBridgeRegistration as MediaBridgeRegistrationRequest, MediaReceiveVideoMode,
    MediaSessionProfile, RelayStatusKind,
};
use crate::node;
use crate::state::{AppState, MediaBridgeRegistration, MediaBridgeState};

const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

const MAX_PEER_ID_LEN: usize = 256;
const MAX_TICKET_LEN: usize = 4096;
const MAX_CHAT_LEN: usize = 64 * 1024; // 64 KB
const MAX_RESOLUTION: u32 = 4096;
const MAX_DISPLAY_NAME_LEN: usize = 64;
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
#[serde(rename_all = "camelCase")]
pub struct NodeInfo {
    pub id: String,
    pub ticket: Option<String>,
    pub relay_status: RelayStatusKind,
}

#[tauri::command]
pub async fn get_node_info(state: State<'_, AppState>) -> Result<NodeInfo, String> {
    let ticket = state.latest_ticket.lock().await.clone();
    let relay_status = state.relay_status.lock().await.clone();
    Ok(NodeInfo {
        id: state.endpoint.id().to_string(),
        ticket,
        relay_status,
    })
}

#[tauri::command]
pub async fn create_call(state: State<'_, AppState>) -> Result<String, String> {
    // Declares intent to accept an inbound call dial for our ticket — must be
    // set before the ticket is handed out (via CallInvite) so a fast joiner
    // can't race the gate. See ConnectionManager::setup_connection.
    state.conn_manager.set_call_session_active(true);

    if let Some(ticket) = state.latest_ticket.lock().await.clone() {
        return Ok(ticket);
    }

    let ticket = node::generate_ticket_when_online(&state.endpoint)
        .await
        .map_err(|e| e.to_string())?;

    let mut latest_ticket = state.latest_ticket.lock().await;
    if latest_ticket.as_deref() != Some(ticket.as_str()) {
        *latest_ticket = Some(ticket.clone());
        let _ = state.event_tx.send(Event::TicketRefreshed {
            ticket: ticket.clone(),
        });
    }

    Ok(ticket)
}

#[tauri::command]
pub async fn join_call(
    ticket: String,
    state: State<'_, AppState>,
) -> Result<String, String> {
    if ticket.len() > MAX_TICKET_LEN {
        return Err("Ticket too large".into());
    }
    // We're deliberately joining a call too, so we must also be able to
    // accept inbound mesh dials from other participants once connected.
    state.conn_manager.set_call_session_active(true);
    let peer_id = state
        .conn_manager
        .connect_to_peer_with_ticket(&state.endpoint, &ticket)
        .await
        .map_err(|e| e.to_string())?;
    // Do NOT emit "peer-connected" here: setup_connection already broadcasts
    // Event::PeerConnected, which the event forwarder emits to the frontend.
    // A second emit double-fired the joiner's listener.
    Ok(peer_id)
}

/// The local user left the call screen: stop accepting inbound call dials
/// for our ticket. Covers the paths where no `end_call` runs because the
/// remote side already hung up (or nobody ever joined).
#[tauri::command]
pub async fn leave_call_session(state: State<'_, AppState>) -> Result<(), String> {
    if state.conn_manager.peer_count().await == 0 {
        state.conn_manager.set_call_session_active(false);
    }
    Ok(())
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

/// Caller gives up on a pending invite before the callee answered — either an
/// explicit cancel (hang up while "waiting") or the caller-side ring timeout.
/// Best-effort notifies the callee so it can dismiss the ringing banner and
/// record a missed call, and unconditionally clears our own call-session-
/// active flag so a late/stray join dial for the now-abandoned ticket is
/// rejected at the protocol level (see setup_connection).
#[tauri::command]
pub async fn cancel_call(peer_id: String, state: State<'_, AppState>) -> Result<(), String> {
    validate_peer_id(&peer_id)?;
    if let Err(e) = state
        .conn_manager
        .send_dm(&peer_id, &DmMessage::CallCancel)
        .await
    {
        tracing::warn!("Failed to deliver call cancel to {peer_id}: {e}");
    }
    state.conn_manager.set_call_session_active(false);
    Ok(())
}

/// Callee explicitly declines a ringing invite (or its 30s auto-timeout
/// fires). Tells the caller so it can stop waiting instead of ringing out.
#[tauri::command]
pub async fn send_call_decline(peer_id: String, state: State<'_, AppState>) -> Result<(), String> {
    validate_peer_id(&peer_id)?;
    state
        .conn_manager
        .send_dm(&peer_id, &DmMessage::CallDecline)
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
    // The webview may only send user-facing call controls. PeerAnnounce and
    // PerPeerQualityBps are backend-originated mesh/QoS signaling — letting a
    // compromised webview send them would allow ticket spoofing into the mesh.
    match &action {
        ControlAction::Mute { .. }
        | ControlAction::VideoOff { .. }
        | ControlAction::VideoQualityRequest { .. }
        | ControlAction::KeyframeRequest { .. } => {}
        ControlAction::SetDisplayName { name } => {
            if name.len() > MAX_DISPLAY_NAME_LEN {
                return Err("Display name too long".into());
            }
        }
        _ => return Err("Control action not allowed from frontend".into()),
    }
    state
        .conn_manager
        .send_control(&peer_id, &action)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn register_media_bridge(
    registration: MediaBridgeRegistrationRequest,
    audio: Channel<InvokeResponseBody>,
    video: Channel<InvokeResponseBody>,
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

    let receive_video_mode = if registration.webcodecs_active {
        MediaReceiveVideoMode::RawH264Nalu
    } else {
        MediaReceiveVideoMode::DecodedJpeg
    };

    let profile = MediaSessionProfile {
        session_id: registration.session_id.clone(),
        receive_bridge_mode: selected_mode,
        receive_video_mode,
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
        webcodecs_active: registration.webcodecs_active,
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
                let _ = channel.send(InvokeResponseBody::Raw(pack_audio_probe_packet()));
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

/// `bitrate_bps`/`fps` are the current call-size profile; omitted means the
/// default 1:1 profile.
#[tauri::command]
pub async fn init_codecs(
    width: u32,
    height: u32,
    bitrate_bps: Option<u32>,
    fps: Option<f32>,
    state: State<'_, AppState>,
) -> Result<(), String> {
    validate_resolution(width, height)?;
    let (bitrate_bps, fps) = validate_rate(bitrate_bps.unwrap_or(400_000), fps.unwrap_or(12.0))?;
    *state.audio_codec.encoder.lock().await = Some(AudioEncoder::new().map_err(|e| e.to_string())?);
    // Audio decoders are created per-peer on demand — no init needed
    *state.video_codec.encoder.lock().await = Some(
        VideoEncoder::new_with_config(width, height, bitrate_bps, fps)
            .map_err(|e| e.to_string())?,
    );
    tracing::info!("Codecs initialized: {width}x{height} @ {bitrate_bps}bps {fps}fps");
    Ok(())
}

fn validate_rate(bitrate_bps: u32, fps: f32) -> Result<(u32, f32), String> {
    if !(50_000..=4_000_000).contains(&bitrate_bps) {
        return Err(format!("Invalid bitrate {bitrate_bps}"));
    }
    if !(1.0..=60.0).contains(&fps) {
        return Err(format!("Invalid fps {fps}"));
    }
    Ok((bitrate_bps, fps))
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

/// Resolution change only: keeps the encoder's current bitrate/fps profile.
#[tauri::command]
pub async fn reinit_video_encoder(
    width: u32,
    height: u32,
    state: State<'_, AppState>,
) -> Result<(), String> {
    validate_resolution(width, height)?;
    let mut guard = state.video_codec.encoder.lock().await;
    let (bitrate_bps, fps) = guard
        .as_ref()
        .map(|e| (e.base_bitrate_bps(), e.base_fps()))
        .unwrap_or((400_000, 12.0));
    *guard = Some(
        VideoEncoder::new_with_config(width, height, bitrate_bps, fps)
            .map_err(|e| e.to_string())?,
    );
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
    let (bitrate_bps, fps) = validate_rate(bitrate_bps, fps)?;
    *state.video_codec.encoder.lock().await = Some(
        VideoEncoder::new_with_config(width, height, bitrate_bps, fps)
            .map_err(|e| e.to_string())?,
    );
    tracing::info!("Video encoder reinitialized: {width}x{height} @ {bitrate_bps}bps {fps}fps");
    Ok(())
}

/// Same shape as the `quality-profile-changed` event payload.
#[derive(serde::Serialize)]
pub struct QualityProfile {
    peer_count: usize,
    bitrate_bps: u32,
    fps: u32,
    max_width: u32,
    max_height: u32,
}

/// Current call-size profile, so a transport starting after the last
/// `quality-profile-changed` event doesn't encode at the 1:1 default.
#[tauri::command]
pub async fn get_quality_profile(state: State<'_, AppState>) -> Result<QualityProfile, String> {
    let peer_count = state.conn_manager.peer_count().await;
    let (bitrate_bps, fps, max_width, max_height) =
        crate::connection::ConnectionManager::quality_profile_for_peers(peer_count);
    Ok(QualityProfile {
        peer_count,
        bitrate_bps,
        fps,
        max_width,
        max_height,
    })
}

// ── Presence (gossip-driven) ────────────────────────────────────────

#[tauri::command]
pub async fn get_presence_snapshot(
    state: State<'_, AppState>,
) -> Result<HashMap<String, bool>, String> {
    Ok(state.presence.snapshot().await)
}

// ── Contacts helpers ────────────────────────────────────────────────

fn load_contacts(store: &tauri_plugin_store::Store<tauri::Wry>) -> Vec<Contact> {
    store
        .get("contacts")
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default()
}

// ── DM commands ────────────────────────────────────────────────────

#[derive(serde::Serialize)]
pub struct FileTransferResult {
    pub id: String,
    pub size: u64,
}

#[tauri::command]
pub async fn send_file(
    peer_id: String,
    file_path: String,
    state: State<'_, AppState>,
) -> Result<FileTransferResult, String> {
    validate_peer_id(&peer_id)?;
    let path = std::path::PathBuf::from(&file_path);

    // Resolve symlinks and ../ traversal
    let canonical = tokio::fs::canonicalize(&path)
        .await
        .map_err(|e| format!("Invalid file path: {e}"))?;

    let name = canonical
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or("Invalid file name")?
        .to_string();
    let metadata = tokio::fs::metadata(&canonical)
        .await
        .map_err(|e| e.to_string())?;
    if !metadata.is_file() {
        return Err("Path is not a regular file".into());
    }
    let size = metadata.len();
    const MAX_FILE_SIZE: u64 = 100 * 1024 * 1024; // 100 MB
    if size > MAX_FILE_SIZE {
        return Err(format!(
            "File too large ({} bytes, max {} bytes)",
            size, MAX_FILE_SIZE
        ));
    }
    let id = uuid::Uuid::new_v4().to_string();

    // File transfer receiver state is tied to a single DM stream, so ensure once
    // and then use strict frame sends. A mid-transfer stream loss must be
    // reported instead of reconnecting and retrying a chunk/end on a fresh stream
    // without FileStart.
    state
        .conn_manager
        .ensure_dm_connected(&peer_id)
        .await
        .map_err(|e| e.to_string())?;

    state
        .conn_manager
        .send_dm_frame_strict(
            &peer_id,
            &DmMessage::FileStart {
                name,
                size,
                id: id.clone(),
            },
        )
        .await
        .map_err(|e| e.to_string())?;

    // Stream file in 64KB chunks
    use tokio::io::AsyncReadExt;
    let mut file = tokio::fs::File::open(&canonical)
        .await
        .map_err(|e| e.to_string())?;
    let mut offset = 0u64;
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf).await.map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        state
            .conn_manager
            .send_dm_frame_strict(
                &peer_id,
                &DmMessage::FileChunk {
                    id: id.clone(),
                    offset,
                    data: buf[..n].to_vec(),
                },
            )
            .await
            .map_err(|e| e.to_string())?;
        offset += n as u64;
    }

    // Send FileEnd
    state
        .conn_manager
        .send_dm_frame_strict(&peer_id, &DmMessage::FileEnd { id: id.clone() })
        .await
        .map_err(|e| e.to_string())?;

    tracing::info!("Sent file to {peer_id}: {offset} bytes, transfer_id={id}");
    Ok(FileTransferResult { id, size })
}

#[tauri::command]
pub async fn connect_dm(node_id: String, state: State<'_, AppState>) -> Result<(), String> {
    validate_peer_id(&node_id)?;
    state
        .conn_manager
        .connect_dm(&node_id)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn send_dm(
    peer_id: String,
    message: serde_json::Value,
    state: State<'_, AppState>,
) -> Result<(), String> {
    validate_peer_id(&peer_id)?;
    let dm_msg: DmMessage = serde_json::from_value(message).map_err(|e| e.to_string())?;
    if let DmMessage::Text { ref content, .. } = dm_msg {
        if content.len() > MAX_CHAT_LEN {
            return Err("DM text too long".into());
        }
    }
    state
        .conn_manager
        .send_dm(&peer_id, &dm_msg)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn disconnect_dm(peer_id: String, state: State<'_, AppState>) -> Result<(), String> {
    validate_peer_id(&peer_id)?;
    state.conn_manager.disconnect_dm(&peer_id).await;
    Ok(())
}

// ── Settings commands ───────────────────────────────────────────────

#[tauri::command]
pub async fn get_settings(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<serde_json::Value, String> {
    let store = app.store("settings.json").map_err(|e| e.to_string())?;
    let mut settings = store.get("app_settings").unwrap_or(serde_json::json!({}));
    // persistent_identity is stored at the top level (not inside app_settings),
    // so merge it into the returned object for the frontend. Persistent identity
    // is now required; normalize older false/missing settings to true.
    if store.get("persistent_identity").and_then(|v| v.as_bool()) != Some(true) {
        store.set("persistent_identity", serde_json::Value::Bool(true));
        store.save().map_err(|e| e.to_string())?;
    }
    if let serde_json::Value::Object(ref mut obj) = settings {
        obj.insert(
            "persistentIdentity".to_string(),
            serde_json::Value::Bool(true),
        );
        let identity_status = identity::status_from_store(&store, &state.identity_status);
        obj.insert(
            "identityStatus".to_string(),
            serde_json::to_value(identity_status).map_err(|e| e.to_string())?,
        );
    }
    Ok(settings)
}

/// Settings keys the webview is allowed to persist. Everything else is either
/// backend-owned (identityStatus, persistentIdentity) or unknown — letting
/// arbitrary keys through would let a compromised webview poison the store.
/// The display name is persisted separately via set_pinned_name.
const ALLOWED_SETTINGS_KEYS: &[&str] = &[
    "preferredMic",
    "preferredCamera",
    "preferredSpeaker",
    "videoQuality",
    "dataSaver",
];
const MAX_SETTINGS_VALUE_LEN: usize = 1024;

#[tauri::command]
pub async fn update_settings(
    settings: serde_json::Value,
    app: tauri::AppHandle,
) -> Result<(), String> {
    let store = app.store("settings.json").map_err(|e| e.to_string())?;
    let mut current = store.get("app_settings").unwrap_or(serde_json::json!({}));
    if let (serde_json::Value::Object(ref mut current_obj), serde_json::Value::Object(ref patch)) =
        (&mut current, &settings)
    {
        for (k, v) in patch {
            if !ALLOWED_SETTINGS_KEYS.contains(&k.as_str()) {
                tracing::warn!("update_settings: ignoring unknown key {k}");
                continue;
            }
            let scalar = v.is_null() || v.is_boolean() || v.is_number() || v.is_string();
            if !scalar || v.as_str().is_some_and(|s| s.len() > MAX_SETTINGS_VALUE_LEN) {
                return Err(format!("Invalid value for settings key {k}"));
            }
            current_obj.insert(k.clone(), v.clone());
        }
    }
    store.set("app_settings", current);
    store.save().map_err(|e| e.to_string())?;
    Ok(())
}

// ── Contacts commands ───────────────────────────────────────────────

#[tauri::command]
pub async fn get_contacts(app: tauri::AppHandle) -> Result<Vec<Contact>, String> {
    let store = app.store("contacts.json").map_err(|e| e.to_string())?;
    Ok(load_contacts(&store))
}

#[tauri::command]
pub async fn add_contact(
    contact: Contact,
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    // Reject malformed ids before persisting — otherwise an invalid contact is
    // saved to disk and only fails later at presence.track_contact.
    contact
        .node_id
        .parse::<iroh::PublicKey>()
        .map_err(|_| "Invalid contact node id".to_string())?;
    if contact.display_name.len() > MAX_DISPLAY_NAME_LEN {
        return Err("Display name too long".into());
    }
    let store = app.store("contacts.json").map_err(|e| e.to_string())?;
    let mut contacts = load_contacts(&store);
    let node_id = contact.node_id.clone();
    let is_new = !contacts.iter().any(|c| c.node_id == node_id);
    // Upsert by node_id
    if let Some(existing) = contacts.iter_mut().find(|c| c.node_id == contact.node_id) {
        existing.display_name = contact.display_name;
        existing.last_seen = contact.last_seen;
    } else {
        contacts.push(contact);
    }
    store.set(
        "contacts",
        serde_json::to_value(&contacts).map_err(|e| e.to_string())?,
    );
    store.save().map_err(|e| e.to_string())?;

    if is_new {
        if let Err(e) = state.presence.track_contact(&node_id).await {
            tracing::warn!("presence track failed for {node_id}: {e}");
        }
    }
    Ok(())
}

#[tauri::command]
pub async fn remove_contact(
    node_id: String,
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let store = app.store("contacts.json").map_err(|e| e.to_string())?;
    let mut contacts = load_contacts(&store);
    contacts.retain(|c| c.node_id != node_id);
    store.set(
        "contacts",
        serde_json::to_value(&contacts).map_err(|e| e.to_string())?,
    );
    store.save().map_err(|e| e.to_string())?;

    state.presence.untrack_contact(&node_id).await;
    Ok(())
}

// ── Identity persistence commands ───────────────────────────────────

#[tauri::command]
pub async fn toggle_persistent_identity(
    enabled: bool,
    state: State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    if !enabled {
        return Err("Persistent identity cannot be disabled".into());
    }

    let store = app.store("settings.json").map_err(|e| e.to_string())?;
    let key = state.endpoint.secret_key();
    identity::persist_secret_key(&store, key).map_err(|e| e.to_string())?;
    Ok(())
}

// ── Name persistence commands ───────────────────────────────────────

#[tauri::command]
pub async fn get_pinned_name(app: tauri::AppHandle) -> Result<Option<String>, String> {
    let store = app.store("settings.json").map_err(|e| e.to_string())?;
    let pinned = store
        .get("name_pinned")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if !pinned {
        return Ok(None);
    }
    Ok(store
        .get("display_name")
        .and_then(|v| v.as_str().map(String::from)))
}

#[tauri::command]
pub async fn set_pinned_name(
    app: tauri::AppHandle,
    name: Option<String>,
    pinned: bool,
) -> Result<(), String> {
    if name.as_deref().is_some_and(|n| n.len() > MAX_DISPLAY_NAME_LEN) {
        return Err("Display name too long".into());
    }
    let store = app.store("settings.json").map_err(|e| e.to_string())?;
    store.set("name_pinned", serde_json::json!(pinned));
    if let Some(n) = name {
        store.set("display_name", serde_json::json!(n));
    }
    store.save().map_err(|e| e.to_string())
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

/// Forward a frame the webview already encoded (WebCodecs `VideoEncoder`,
/// Annex B H.264). Used on Android, where shipping raw RGBA over the JSON IPC
/// bridge costs ~1.2 MB of base64 per frame. Returns whether the next frame
/// must be a keyframe (a peer asked for one, or a writer lost its chain).
#[tauri::command]
pub async fn send_encoded_video_all(
    request: tauri::ipc::Request<'_>,
    state: State<'_, AppState>,
) -> Result<bool, String> {
    let (timestamp, payload) = match request.body() {
        // [ts:u64LE][annex-b...]
        tauri::ipc::InvokeBody::Raw(data) => {
            if data.len() <= 8 {
                return Err("Payload too short".into());
            }
            let timestamp = u64::from_le_bytes(data[..8].try_into().unwrap());
            (timestamp, data[8..].to_vec())
        }
        tauri::ipc::InvokeBody::Json(value) => {
            let data_b64 = value
                .get("data")
                .and_then(|v| v.as_str())
                .ok_or("Missing data")?;
            let timestamp = value.get("timestamp").and_then(|v| v.as_u64()).unwrap_or(0);
            let payload = B64
                .decode(data_b64)
                .map_err(|e| format!("base64 decode error: {e}"))?;
            (timestamp, payload)
        }
    };
    if payload.len() > crate::video_transport::MAX_VIDEO_FRAME_BYTES {
        return Err("Encoded frame too large".into());
    }
    if state.conn_manager.has_peers().await {
        state
            .conn_manager
            .send_video_frame_all(&payload, timestamp)
            .await
            .map_err(|e| e.to_string())?;
    }
    Ok(state.conn_manager.consume_pending_keyframe_requests().await)
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

    // RGBA→YUV + H.264 encode is tens of ms of CPU: keep it off the async
    // worker threads so it can't stall networking tasks.
    let codec = state.video_codec.clone();
    let rgba = rgba.to_vec();
    let encoded = tokio::task::spawn_blocking(move || {
        let mut video = codec.encoder.blocking_lock();
        video
            .as_mut()
            .and_then(|encoder| encoder.encode(&rgba, width, height, keyframe || force_keyframe))
    })
    .await
    .map_err(|e| e.to_string())?;

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
