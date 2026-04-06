use futures_util::{SinkExt, StreamExt};
use instalock_shared::SyncMessage;
use std::sync::Arc;
use tokio::sync::broadcast;
use tokio_tungstenite::tungstenite::Message;

pub struct SyncClient {
    /// Incoming messages from the relay server
    pub incoming_tx: broadcast::Sender<SyncMessage>,
    /// Send messages to the relay server
    outgoing_tx: tokio::sync::mpsc::Sender<SyncMessage>,
}

impl SyncClient {
    /// Connect to the relay server and join a room
    pub async fn connect(
        server_url: &str,
        game_id: &str,
        player_name: &str,
    ) -> Result<Arc<Self>, String> {
        let url = format!("{}/ws", server_url.trim_end_matches('/'));

        let tls = native_tls::TlsConnector::builder()
            .danger_accept_invalid_certs(true)
            .build()
            .map_err(|e| e.to_string())?;

        let (ws, _) = tokio_tungstenite::connect_async_tls_with_config(
            &url,
            None,
            false,
            Some(tokio_tungstenite::Connector::NativeTls(tls)),
        )
        .await
        .map_err(|e| format!("sync connect failed: {}", e))?;

        let (mut ws_tx, mut ws_rx) = ws.split();

        // Send join message
        let join = SyncMessage::Join {
            game_id: game_id.to_string(),
            player_name: player_name.to_string(),
        };
        let join_json = serde_json::to_string(&join).map_err(|e| e.to_string())?;
        ws_tx
            .send(Message::Text(join_json.into()))
            .await
            .map_err(|e| e.to_string())?;

        let (incoming_tx, _) = broadcast::channel(64);
        let (outgoing_tx, mut outgoing_rx) = tokio::sync::mpsc::channel::<SyncMessage>(64);

        let client = Arc::new(Self {
            incoming_tx: incoming_tx.clone(),
            outgoing_tx,
        });

        // Send task: forward outgoing messages to WS
        tokio::spawn(async move {
            while let Some(msg) = outgoing_rx.recv().await {
                if let Ok(json) = serde_json::to_string(&msg) {
                    if ws_tx.send(Message::Text(json.into())).await.is_err() {
                        break;
                    }
                }
            }
        });

        // Receive task: forward WS messages to incoming broadcast
        tokio::spawn(async move {
            while let Some(Ok(msg)) = ws_rx.next().await {
                if let Message::Text(text) = msg {
                    if let Ok(sync_msg) = serde_json::from_str::<SyncMessage>(&text) {
                        let _ = incoming_tx.send(sync_msg);
                    }
                }
            }
        });

        Ok(client)
    }

    /// Send a timer event to all teammates
    pub async fn send(&self, msg: SyncMessage) -> Result<(), String> {
        self.outgoing_tx
            .send(msg)
            .await
            .map_err(|e| e.to_string())
    }
}
