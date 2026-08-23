use axum::{
    extract::{
        ws::{Message, WebSocket},
        State, WebSocketUpgrade,
    },
    response::IntoResponse,
    routing::get,
    Router,
};
use futures_util::{SinkExt, StreamExt};
use instalock_shared::{ActiveTimer, SyncMessage};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{broadcast, RwLock};
use tower_http::cors::CorsLayer;

struct Room {
    tx: broadcast::Sender<String>,
    timers: RwLock<Vec<ActiveTimer>>,
}

struct AppState {
    rooms: RwLock<HashMap<String, Arc<Room>>>,
}

impl AppState {
    fn new() -> Self {
        Self {
            rooms: RwLock::new(HashMap::new()),
        }
    }
}

#[tokio::main]
async fn main() {
    let state = Arc::new(AppState::new());

    // Spawn cleanup task: remove empty rooms every 5 minutes
    let cleanup_state = state.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(300)).await;
            let mut rooms = cleanup_state.rooms.write().await;
            rooms.retain(|_, room| room.tx.receiver_count() > 0);
        }
    });

    let app = Router::new()
        .route("/ws", get(ws_handler))
        .route("/health", get(|| async { "ok" }))
        .layer(CorsLayer::permissive())
        .with_state(state);

    let addr = "0.0.0.0:9876";
    println!("InstaLock relay server listening on {}", addr);
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

async fn ws_handler(ws: WebSocketUpgrade, State(state): State<Arc<AppState>>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

async fn handle_socket(mut socket: WebSocket, state: Arc<AppState>) {
    // Wait for the first message to be a Join
    let room_arc = loop {
        let Some(Ok(Message::Text(text))) = socket.recv().await else {
            return;
        };
        let Ok(msg) = serde_json::from_str::<SyncMessage>(&text) else {
            continue;
        };
        if let SyncMessage::Join { game_id, .. } = msg {
            let room = get_or_create_room(&state, &game_id).await;
            // Send current room state
            {
                let timers = room.timers.read().await;
                let state_msg = SyncMessage::RoomState {
                    timers: cleanup_expired_timers(&timers),
                };
                if let Ok(json) = serde_json::to_string(&state_msg) {
                    let _ = socket.send(Message::Text(json.into())).await;
                }
            }
            break room;
        }
    };

    let mut rx = room_arc.tx.subscribe();

    let (mut ws_tx, mut ws_rx) = socket.split();

    // Relay broadcast messages to this client
    let room_for_send = room_arc.clone();
    let send_task = tokio::spawn(async move {
        let _ = room_for_send; // keep alive
        // Lagged means this client fell behind, not that the stream ended -
        // returning here would silently stop relaying for the rest of the game.
        loop {
            let msg = match rx.recv().await {
                Ok(msg) => msg,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    eprintln!("client lagged, {} messages dropped", n);
                    continue;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            };
            if ws_tx.send(Message::Text(msg.into())).await.is_err() {
                break;
            }
        }
    });

    // Receive messages from this client and broadcast
    while let Some(Ok(msg)) = ws_rx.next().await {
        let Message::Text(text) = msg else { continue };
        let Ok(sync_msg) = serde_json::from_str::<SyncMessage>(&text) else {
            continue;
        };

        match &sync_msg {
            SyncMessage::TimerStart {
                enemy_idx,
                spell_idx,
                cooldown_secs,
                started_at,
            } => {
                let mut timers = room_arc.timers.write().await;
                // Remove existing timer for same spell
                timers.retain(|t| !(t.enemy_idx == *enemy_idx && t.spell_idx == *spell_idx));
                timers.push(ActiveTimer {
                    enemy_idx: *enemy_idx,
                    spell_idx: *spell_idx,
                    cooldown_secs: *cooldown_secs,
                    started_at: *started_at,
                });
            }
            SyncMessage::TimerCancel {
                enemy_idx,
                spell_idx,
            } => {
                let mut timers = room_arc.timers.write().await;
                timers.retain(|t| !(t.enemy_idx == *enemy_idx && t.spell_idx == *spell_idx));
            }
            _ => {}
        }

        // Broadcast to all clients in the room
        let _ = room_arc.tx.send(text.to_string());
    }

    send_task.abort();
}

async fn get_or_create_room(state: &AppState, game_id: &str) -> Arc<Room> {
    {
        let rooms = state.rooms.read().await;
        if let Some(room) = rooms.get(game_id) {
            return room.clone();
        }
    }
    let mut rooms = state.rooms.write().await;
    rooms
        .entry(game_id.to_string())
        .or_insert_with(|| {
            let (tx, _) = broadcast::channel(64);
            Arc::new(Room {
                tx,
                timers: RwLock::new(Vec::new()),
            })
        })
        .clone()
}

fn cleanup_expired_timers(timers: &[ActiveTimer]) -> Vec<ActiveTimer> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    timers
        .iter()
        .filter(|t| t.started_at + t.cooldown_secs as u64 > now)
        .cloned()
        .collect()
}
