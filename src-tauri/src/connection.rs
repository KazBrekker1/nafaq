use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use iroh::endpoint::{Connection, RecvStream, SendStream};
use tokio::sync::{broadcast, watch, Mutex};

use crate::codec::is_keyframe;
use crate::messages::{
    ControlAction, Event, MediaPacket, STREAM_AUDIO, STREAM_CHAT, STREAM_CONTROL, STREAM_VIDEO,
};

struct PeerConnection {
    connection: Connection,
    audio_send: Arc<Mutex<Option<SendStream>>>,
    chat_send: Arc<Mutex<Option<SendStream>>>,
    control_send: Arc<Mutex<Option<SendStream>>>,
}

pub struct ConnectionManager {
    peers: Arc<Mutex<HashMap<String, PeerConnection>>>,
    event_tx: broadcast::Sender<Event>,
    audio_media_tx: broadcast::Sender<MediaPacket>,
    video_watch_tx: watch::Sender<Option<MediaPacket>>,
}

impl std::fmt::Debug for ConnectionManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConnectionManager").finish()
    }
}

impl ConnectionManager {
    pub fn new(
        event_tx: broadcast::Sender<Event>,
        audio_media_tx: broadcast::Sender<MediaPacket>,
        video_watch_tx: watch::Sender<Option<MediaPacket>>,
    ) -> Self {
        Self {
            peers: Arc::new(Mutex::new(HashMap::new())),
            event_tx,
            audio_media_tx,
            video_watch_tx,
        }
    }

    pub async fn handle_incoming(&self, connection: Connection) -> Result<()> {
        let peer_id = connection.remote_id().to_string();
        tracing::info!("Setting up incoming connection from {peer_id}");
        self.setup_connection(peer_id, connection).await
    }

    pub async fn connect_to_peer(
        &self,
        endpoint: &iroh::Endpoint,
        addr: iroh::EndpointAddr,
    ) -> Result<String> {
        let connection = endpoint.connect(addr, crate::node::NAFAQ_ALPN).await?;
        let peer_id = connection.remote_id().to_string();
        tracing::info!("Connected to peer {peer_id}");
        self.setup_connection(peer_id.clone(), connection).await?;
        Ok(peer_id)
    }

    async fn setup_connection(&self, peer_id: String, connection: Connection) -> Result<()> {
        // Audio: long-lived uni-stream with high priority
        let mut audio_send = connection.open_uni().await?;
        audio_send.write_all(&[STREAM_AUDIO]).await?;
        audio_send.set_priority(90)?;

        // Chat: bi-stream with low priority
        let (mut chat_send, _) = connection.open_bi().await?;
        chat_send.write_all(&[STREAM_CHAT]).await?;
        chat_send.set_priority(10)?;

        // Control: bi-stream with highest priority
        let (mut control_send, _) = connection.open_bi().await?;
        control_send.write_all(&[STREAM_CONTROL]).await?;
        control_send.set_priority(100)?;

        let peer_conn = PeerConnection {
            connection: connection.clone(),
            audio_send: Arc::new(Mutex::new(Some(audio_send))),
            chat_send: Arc::new(Mutex::new(Some(chat_send))),
            control_send: Arc::new(Mutex::new(Some(control_send))),
        };

        {
            let mut peers = self.peers.lock().await;
            peers.insert(peer_id.clone(), peer_conn);
        }

        let _ = self.event_tx.send(Event::PeerConnected {
            peer_id: peer_id.clone(),
        });

        self.spawn_stream_receivers(peer_id, connection);
        Ok(())
    }

    fn spawn_stream_receivers(&self, peer_id: String, connection: Connection) {
        let audio_media_tx = self.audio_media_tx.clone();
        let video_watch_tx = self.video_watch_tx.clone();
        let event_tx_uni = self.event_tx.clone();
        let peers_ref = self.peers.clone();
        let peer_id_uni = peer_id.clone();
        let connection_uni = connection.clone();

        // Uni stream receiver (audio/video from peer)
        tokio::spawn(async move {
            loop {
                match connection_uni.accept_uni().await {
                    Ok(mut recv) => {
                        let peer_id = peer_id_uni.clone();
                        let audio_tx = audio_media_tx.clone();
                        let video_tx = video_watch_tx.clone();
                        tokio::spawn(async move {
                            let mut type_buf = [0u8; 1];
                            if recv.read_exact(&mut type_buf).await.is_err() {
                                return;
                            }
                            match type_buf[0] {
                                STREAM_AUDIO => {
                                    // Audio: read all frames from long-lived stream
                                    loop {
                                        match crate::messages::read_framed(&mut recv).await {
                                            Ok(Some(data)) if data.len() >= 8 => {
                                                let ts = u64::from_be_bytes(
                                                    data[..8].try_into().unwrap(),
                                                );
                                                let payload = data[8..].to_vec();
                                                let _ = audio_tx
                                                    .send((peer_id.clone(), ts, payload));
                                            }
                                            _ => break,
                                        }
                                    }
                                }
                                STREAM_VIDEO => {
                                    // Video: single frame per stream (stream-per-frame)
                                    if let Ok(Some(data)) =
                                        crate::messages::read_framed(&mut recv).await
                                    {
                                        if data.len() >= 8 {
                                            let ts = u64::from_be_bytes(
                                                data[..8].try_into().unwrap(),
                                            );
                                            let payload = data[8..].to_vec();
                                            let _ = video_tx.send(Some((
                                                peer_id.clone(),
                                                ts,
                                                payload,
                                            )));
                                        }
                                    }
                                }
                                _ => {}
                            }
                        });
                    }
                    Err(_) => {
                        tracing::info!("Connection lost for peer {peer_id_uni}");
                        peers_ref.lock().await.remove(&peer_id_uni);
                        let _ = event_tx_uni.send(Event::PeerDisconnected {
                            peer_id: peer_id_uni.clone(),
                        });
                        break;
                    }
                }
            }
        });

        let event_tx = self.event_tx.clone();
        let peer_id_bi = peer_id.clone();

        // Bi stream receiver (chat/control from peer)
        tokio::spawn(async move {
            loop {
                match connection.accept_bi().await {
                    Ok((_, mut recv)) => {
                        let peer_id = peer_id_bi.clone();
                        let event_tx = event_tx.clone();
                        let mut type_buf = [0u8; 1];
                        if recv.read_exact(&mut type_buf).await.is_err() {
                            continue;
                        }
                        tokio::spawn(async move {
                            Self::handle_bi_stream(type_buf[0], &peer_id, recv, event_tx).await;
                        });
                    }
                    Err(_) => {
                        // Don't double-emit — uni receiver handles disconnect
                        tracing::info!("Bi stream accept ended for peer {peer_id_bi}");
                        break;
                    }
                }
            }
        });
    }

    async fn handle_bi_stream(
        stream_type: u8,
        peer_id: &str,
        mut recv: RecvStream,
        event_tx: broadcast::Sender<Event>,
    ) {
        loop {
            match crate::messages::read_framed(&mut recv).await {
                Ok(Some(data)) => match stream_type {
                    STREAM_CHAT => {
                        if let Ok(message) = String::from_utf8(data) {
                            let _ = event_tx.send(Event::ChatReceived {
                                peer_id: peer_id.to_string(),
                                message,
                            });
                        }
                    }
                    STREAM_CONTROL => {
                        if let Ok(action) = serde_json::from_slice::<ControlAction>(&data) {
                            let _ = event_tx.send(Event::ControlReceived {
                                peer_id: peer_id.to_string(),
                                action,
                            });
                        }
                    }
                    _ => {
                        tracing::warn!("Unknown bi stream type: {stream_type}");
                    }
                },
                Ok(None) => break,
                Err(e) => {
                    tracing::warn!("Error reading bi stream from {peer_id}: {e}");
                    break;
                }
            }
        }
    }

    async fn send_on_stream(stream: &Arc<Mutex<Option<SendStream>>>, data: &[u8]) -> Result<()> {
        let mut guard = stream.lock().await;
        if let Some(ref mut send) = *guard {
            crate::messages::write_framed(send, data).await?;
        }
        Ok(())
    }

    /// Send audio on the long-lived audio stream with timestamp prepended
    pub async fn send_audio(&self, peer_id: &str, data: &[u8], timestamp: u64) -> Result<()> {
        let stream = {
            let peers = self.peers.lock().await;
            peers.get(peer_id).map(|p| p.audio_send.clone())
        };
        if let Some(s) = stream {
            let mut payload = Vec::with_capacity(8 + data.len());
            payload.extend_from_slice(&timestamp.to_be_bytes());
            payload.extend_from_slice(data);
            Self::send_on_stream(&s, &payload).await
        } else {
            Ok(())
        }
    }

    /// Send video via stream-per-frame: opens a new uni-stream, sets priority, writes, finishes
    pub async fn send_video(&self, peer_id: &str, data: &[u8], timestamp: u64) -> Result<()> {
        let connection = {
            let peers = self.peers.lock().await;
            peers.get(peer_id).map(|p| p.connection.clone())
        };
        if let Some(conn) = connection {
            let mut send = conn.open_uni().await?;
            let priority = if is_keyframe(data) { 50 } else { 30 };
            send.set_priority(priority)?;
            send.write_all(&[STREAM_VIDEO]).await?;
            let mut payload = Vec::with_capacity(8 + data.len());
            payload.extend_from_slice(&timestamp.to_be_bytes());
            payload.extend_from_slice(data);
            crate::messages::write_framed(&mut send, &payload).await?;
            send.finish()?;
        }
        Ok(())
    }

    pub async fn send_chat(&self, peer_id: &str, message: &str) -> Result<()> {
        let stream = {
            let peers = self.peers.lock().await;
            peers.get(peer_id).map(|p| p.chat_send.clone())
        };
        if let Some(s) = stream {
            let mut guard = s.lock().await;
            if let Some(ref mut send) = *guard {
                crate::messages::write_framed(send, message.as_bytes()).await?;
            }
        }
        Ok(())
    }

    pub async fn send_control(&self, peer_id: &str, action: &ControlAction) -> Result<()> {
        let data = serde_json::to_vec(action)?;
        let stream = {
            let peers = self.peers.lock().await;
            peers.get(peer_id).map(|p| p.control_send.clone())
        };
        if let Some(s) = stream {
            let mut guard = s.lock().await;
            if let Some(ref mut send) = *guard {
                crate::messages::write_framed(send, &data).await?;
            }
        }
        Ok(())
    }

    pub async fn disconnect_peer(&self, peer_id: &str) -> Result<()> {
        let mut peers = self.peers.lock().await;
        if let Some(peer) = peers.remove(peer_id) {
            peer.connection.close(0u32.into(), b"call ended");
            let _ = self.event_tx.send(Event::PeerDisconnected {
                peer_id: peer_id.to_string(),
            });
        }
        Ok(())
    }

    pub async fn connected_peers(&self) -> Vec<String> {
        let peers = self.peers.lock().await;
        peers.keys().cloned().collect()
    }
}
