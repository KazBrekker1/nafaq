use std::sync::Arc;

use iroh::protocol::Router;
use tokio::sync::{broadcast, watch};

use crate::codec::CodecState;
use crate::connection::ConnectionManager;
use crate::messages::{Event, MediaPacket};

pub struct AppState {
    pub endpoint: iroh::Endpoint,
    pub router: Router,
    pub conn_manager: Arc<ConnectionManager>,
    pub event_tx: broadcast::Sender<Event>,
    pub audio_media_tx: broadcast::Sender<MediaPacket>,
    pub video_watch_tx: watch::Sender<Option<MediaPacket>>,
    pub codec: Arc<CodecState>,
}
