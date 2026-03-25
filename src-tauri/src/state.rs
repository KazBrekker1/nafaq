use std::sync::Arc;

use iroh::Endpoint;
use iroh::protocol::Router;
use tokio::sync::broadcast;

use crate::codec::CodecState;
use crate::connection::ConnectionManager;
use crate::messages::Event;

pub struct AppState {
    pub endpoint: Endpoint,
    pub router: Router,
    pub conn_manager: Arc<ConnectionManager>,
    pub event_tx: broadcast::Sender<Event>,
    pub codec: Arc<CodecState>,
}
