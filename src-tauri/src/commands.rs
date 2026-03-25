use tauri::{Emitter, State};

use crate::messages::ControlAction;
use crate::node;
use crate::state::AppState;

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
    state
        .conn_manager
        .send_control(&peer_id, &action)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn send_audio(
    peer_id: String,
    data: Vec<u8>,
    state: State<'_, AppState>,
) -> Result<(), String> {
    state
        .conn_manager
        .send_audio(&peer_id, &data)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn send_video(
    peer_id: String,
    data: Vec<u8>,
    state: State<'_, AppState>,
) -> Result<(), String> {
    state
        .conn_manager
        .send_video(&peer_id, &data)
        .await
        .map_err(|e| e.to_string())
}
