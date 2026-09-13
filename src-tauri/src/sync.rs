use futures_util::{SinkExt, StreamExt};
use instalock_shared::SyncMessage;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{broadcast, mpsc};
use tokio_tungstenite::tungstenite::Message;

/// Backoff bounds for reconnecting. A game lasts ~30 minutes, so there is no
/// point climbing past a delay that would burn a meaningful slice of it.
const RECONNECT_MIN: Duration = Duration::from_secs(2);
const RECONNECT_MAX: Duration = Duration::from_secs(30);

/// Outgoing keepalive, matching the relay server's own interval. Without
/// traffic in both directions a half-open connection looks alive from here
/// while the server has already given up on it.
const PING_INTERVAL: Duration = Duration::from_secs(30);

/// What the sync connection is doing, for the UI log.
///
/// Before this existed a mid-game disconnect was completely invisible: the
/// socket died, `sync_client` stayed `Some`, every timer silently went nowhere,
/// and the log still said "Sync connectat" from twenty minutes earlier.
#[derive(Debug, Clone)]
pub enum SyncStatus {
    Connected,
    Lost(String),
    Retrying { in_secs: u64 },
}

pub struct SyncClient {
    /// Incoming messages from the relay server
    pub incoming_tx: broadcast::Sender<SyncMessage>,
    /// Connection lifecycle, for logging
    pub status_tx: broadcast::Sender<SyncStatus>,
    /// Send messages to the relay server. Survives reconnects: the supervisor
    /// task owns the receiving end, so a queued timer isn't lost when the
    /// socket underneath it is replaced.
    outgoing_tx: mpsc::Sender<SyncMessage>,
    connected: Arc<AtomicBool>,
    shutdown: Arc<AtomicBool>,
}

impl SyncClient {
    /// Start the supervisor. Returns immediately — the first connection happens
    /// in the background, and every later one is a retry rather than a failure.
    pub fn start(server_url: &str, game_id: &str, player_name: &str) -> Arc<Self> {
        let (incoming_tx, _) = broadcast::channel(64);
        let (status_tx, _) = broadcast::channel(16);
        let (outgoing_tx, mut outgoing_rx) = mpsc::channel::<SyncMessage>(64);
        let connected = Arc::new(AtomicBool::new(false));
        let shutdown = Arc::new(AtomicBool::new(false));

        let client = Arc::new(Self {
            incoming_tx: incoming_tx.clone(),
            status_tx: status_tx.clone(),
            outgoing_tx,
            connected: connected.clone(),
            shutdown: shutdown.clone(),
        });

        let url = format!("{}/ws", server_url.trim_end_matches('/'));
        let join = SyncMessage::Join {
            game_id: game_id.to_string(),
            player_name: player_name.to_string(),
        };

        tokio::spawn(async move {
            let mut delay = RECONNECT_MIN;

            while !shutdown.load(Ordering::SeqCst) {
                let outcome = run_session(
                    &url,
                    &join,
                    &mut outgoing_rx,
                    &incoming_tx,
                    &status_tx,
                    &connected,
                    &shutdown,
                )
                .await;

                connected.store(false, Ordering::SeqCst);

                if shutdown.load(Ordering::SeqCst) {
                    break;
                }

                match outcome {
                    // A session that ran and then ended: the peer went away.
                    // Retry promptly, the next attempt usually succeeds.
                    Ok(()) => {
                        let _ = status_tx.send(SyncStatus::Lost("connexió tancada".into()));
                        delay = RECONNECT_MIN;
                    }
                    Err(e) => {
                        let _ = status_tx.send(SyncStatus::Lost(e));
                    }
                }

                let _ = status_tx.send(SyncStatus::Retrying {
                    in_secs: delay.as_secs(),
                });
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(RECONNECT_MAX);
            }

            log::info!("Sync supervisor stopped");
        });

        client
    }

    /// Send a timer event to all teammates.
    ///
    /// Fails loudly rather than silently when the socket is down, so the caller
    /// can say so instead of pretending the teammates saw it.
    pub async fn send(&self, msg: SyncMessage) -> Result<(), String> {
        if !self.is_connected() {
            return Err("sync desconnectat".to_string());
        }
        self.outgoing_tx.send(msg).await.map_err(|e| e.to_string())
    }

    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::SeqCst)
    }
}

impl Drop for SyncClient {
    /// Retire the supervisor when the client is dropped — which is what
    /// `*state.sync_client.lock().await = None` at end of game does. Without
    /// this the reconnect loop would outlive the game it belongs to and keep
    /// dialling a room nobody is in.
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
    }
}

/// One connection, from handshake to close. Returns `Ok` when the peer ended it
/// cleanly and `Err` when it never got off the ground.
async fn run_session(
    url: &str,
    join: &SyncMessage,
    outgoing_rx: &mut mpsc::Receiver<SyncMessage>,
    incoming_tx: &broadcast::Sender<SyncMessage>,
    status_tx: &broadcast::Sender<SyncStatus>,
    connected: &Arc<AtomicBool>,
    shutdown: &Arc<AtomicBool>,
) -> Result<(), String> {
    // Plain `connect_async` rather than a connector with certificate
    // verification disabled: the relay is a real remote host, not loopback, so
    // there is a middle to be in. `ws://` skips TLS entirely and `wss://` gets
    // properly validated.
    let (ws, _) = tokio_tungstenite::connect_async(url)
        .await
        .map_err(|e| format!("no s'ha pogut connectar: {}", e))?;

    let (mut ws_tx, mut ws_rx) = ws.split();

    let join_json = serde_json::to_string(join).map_err(|e| e.to_string())?;
    ws_tx
        .send(Message::Text(join_json.into()))
        .await
        .map_err(|e| format!("join fallit: {}", e))?;

    connected.store(true, Ordering::SeqCst);
    let _ = status_tx.send(SyncStatus::Connected);
    log::info!("Sync connected to {}", url);

    let mut ping = tokio::time::interval(PING_INTERVAL);
    ping.tick().await; // the first tick is immediate; skip it

    loop {
        if shutdown.load(Ordering::SeqCst) {
            let _ = ws_tx.send(Message::Close(None)).await;
            return Ok(());
        }

        tokio::select! {
            _ = ping.tick() => {
                if ws_tx.send(Message::Ping(Vec::new().into())).await.is_err() {
                    return Ok(());
                }
            }
            outgoing = outgoing_rx.recv() => {
                let Some(msg) = outgoing else {
                    // The client was dropped; nothing left to send.
                    return Ok(());
                };
                if let Ok(json) = serde_json::to_string(&msg) {
                    if ws_tx.send(Message::Text(json.into())).await.is_err() {
                        return Ok(());
                    }
                }
            }
            incoming = ws_rx.next() => {
                match incoming {
                    Some(Ok(Message::Text(text))) => {
                        if let Ok(sync_msg) = serde_json::from_str::<SyncMessage>(&text) {
                            let _ = incoming_tx.send(sync_msg);
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => return Ok(()),
                    Some(Err(e)) => return Err(format!("connexió perduda: {}", e)),
                    // Ping/Pong/Binary: tungstenite answers pings itself.
                    Some(Ok(_)) => {}
                }
            }
        }
    }
}
