use std::sync::Arc;

use iroh::protocol::Router;
use tauri::ipc::Channel;
use tokio::sync::broadcast;
use tokio::sync::Mutex;

use crate::codec::{AudioCodecState, VideoCodecState};
use crate::connection::ConnectionManager;
use crate::messages::{AudioPacket, Event, MediaSessionProfile, VideoPacket};

#[derive(Clone)]
pub struct MediaBridgeRegistration {
    pub profile: MediaSessionProfile,
    pub audio_channel: Option<Channel<Vec<u8>>>,
    pub video_channel: Option<Channel<Vec<u8>>>,
    pub webcodecs_active: bool,
}

#[derive(Default)]
pub struct MediaBridgeState {
    pub current: Arc<Mutex<Option<MediaBridgeRegistration>>>,
}

#[allow(dead_code)]
pub struct AppState {
    pub endpoint: iroh::Endpoint,
    pub router: Router,
    pub conn_manager: Arc<ConnectionManager>,
    pub event_tx: broadcast::Sender<Event>,
    pub audio_media_tx: broadcast::Sender<AudioPacket>,
    pub video_media_tx: broadcast::Sender<VideoPacket>,
    pub audio_codec: Arc<AudioCodecState>,
    pub video_codec: Arc<VideoCodecState>,
    pub video_runtime: tokio::runtime::Handle,
}
