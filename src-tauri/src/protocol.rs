use std::sync::Arc;

use iroh::{
    endpoint::Connection,
    protocol::{AcceptError, ProtocolHandler},
};

use crate::connection::ConnectionManager;

#[derive(Debug, Clone)]
pub struct NafaqProtocol {
    conn_manager: Arc<ConnectionManager>,
}

impl NafaqProtocol {
    pub fn new(conn_manager: Arc<ConnectionManager>) -> Self {
        Self { conn_manager }
    }
}

impl ProtocolHandler for NafaqProtocol {
    async fn accept(&self, connection: Connection) -> std::result::Result<(), AcceptError> {
        let peer_id = connection.remote_id();
        tracing::info!("Accepted incoming connection from {peer_id}");

        self.conn_manager
            .handle_incoming(connection)
            .await
            .map_err(|e| AcceptError::from_boxed(e.into()))?;

        Ok(())
    }
}
