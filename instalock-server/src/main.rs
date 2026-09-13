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

/// How often the server pings each client, and how long it waits for any frame
/// back before giving up on the socket.
///
/// Without this a half-open connection (NAT timeout, laptop asleep) keeps its
/// broadcast::Receiver alive forever, so `receiver_count() > 0` stays true and
/// the room is never collected — one leaked room per abandoned game.
const PING_INTERVAL: std::time::Duration = std::time::Duration::from_secs(30);
const CLIENT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(90);

/// Bounds on what a client may address. `enemy_idx` and `spell_idx` arrive
/// straight off the wire as u8, so without these a peer can push 65_536 timers
/// into a room and make every subsequent TimerStart an O(n) scan.
const MAX_ENEMIES: u8 = 5;
const MAX_SPELLS: u8 = 2;
/// Teleport is the longest summoner spell at 360s; leave room for future ones.
const MAX_COOLDOWN_SECS: u32 = 900;

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
    // Wait for the first message to be a Join.
    //
    // Anything that is not a Join is skipped rather than fatal: a Ping, Pong or
    // Binary frame arriving first used to drop the socket here, so any client or
    // proxy that pings on connect could never join.
    let (room_arc, mut rx) = loop {
        let msg = match socket.recv().await {
            Some(Ok(msg)) => msg,
            // None = closed, Err = broken. Either way there is no joining.
            _ => return,
        };
        let Message::Text(text) = msg else { continue };
        let Ok(SyncMessage::Join { game_id, .. }) = serde_json::from_str::<SyncMessage>(&text)
        else {
            continue;
        };

        // Subscribing happens inside get_or_create_room, under the same lock
        // that created the room. Sending the room state first left a window
        // where receiver_count() was still 0: if the 5-minute sweep landed in
        // it, the room was dropped from the map while this client kept its Arc,
        // and the next player to join built a fresh room under the same game id.
        // The two then never saw each other's timers.
        let (room, rx) = get_or_create_room(&state, &game_id).await;

        // Expired timers are dropped from the room itself, not just from the
        // copy on the wire, so the vector actually shrinks.
        {
            let mut timers = room.timers.write().await;
            timers.retain(is_live);
            let state_msg = SyncMessage::RoomState {
                timers: timers.clone(),
            };
            if let Ok(json) = serde_json::to_string(&state_msg) {
                let _ = socket.send(Message::Text(json.into())).await;
            }
        }
        break (room, rx);
    };

    let (mut ws_tx, mut ws_rx) = socket.split();

    // Relay broadcast messages to this client
    let room_for_send = room_arc.clone();
    let send_task = tokio::spawn(async move {
        let _ = room_for_send; // keep alive
        // Lagged means this client fell behind, not that the stream ended -
        // returning here would silently stop relaying for the rest of the game.
        let mut ping = tokio::time::interval(PING_INTERVAL);
        // The first tick fires immediately; skip it so a fresh client isn't
        // pinged before it has finished setting up.
        ping.tick().await;

        loop {
            let msg = tokio::select! {
                // A peer that has gone away without closing the TCP connection
                // only reveals itself when we try to write to it. The pings are
                // what eventually produce that write error, which is what frees
                // the room.
                _ = ping.tick() => {
                    if ws_tx.send(Message::Ping(Vec::new().into())).await.is_err() {
                        break;
                    }
                    continue;
                }
                received = rx.recv() => match received {
                    Ok(msg) => msg,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        eprintln!("client lagged, {} messages dropped", n);
                        continue;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                },
            };
            if ws_tx.send(Message::Text(msg.into())).await.is_err() {
                break;
            }
        }
    });

    // Receive messages from this client and broadcast.
    //
    // Any frame resets the clock, Pong included - that is what makes the ping
    // above a liveness check rather than just traffic.
    loop {
        let next = match tokio::time::timeout(CLIENT_TIMEOUT, ws_rx.next()).await {
            Ok(Some(Ok(msg))) => msg,
            Ok(_) => break,
            Err(_) => {
                eprintln!("client timed out after {:?}, dropping", CLIENT_TIMEOUT);
                break;
            }
        };

        let Message::Text(text) = next else { continue };
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
                if !addresses_a_real_spell(*enemy_idx, *spell_idx)
                    || *cooldown_secs == 0
                    || *cooldown_secs > MAX_COOLDOWN_SECS
                {
                    continue;
                }
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
                if !addresses_a_real_spell(*enemy_idx, *spell_idx) {
                    continue;
                }
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

/// Hand back the room *and* a receiver taken while the map is still locked.
///
/// The two have to be produced together: the cleanup sweep collects any room
/// whose `receiver_count()` is 0, so a caller that subscribes later can have its
/// brand-new room swept out from under it.
async fn get_or_create_room(
    state: &AppState,
    game_id: &str,
) -> (Arc<Room>, broadcast::Receiver<String>) {
    {
        let rooms = state.rooms.read().await;
        if let Some(room) = rooms.get(game_id) {
            let rx = room.tx.subscribe();
            return (room.clone(), rx);
        }
    }
    let mut rooms = state.rooms.write().await;
    let room = rooms
        .entry(game_id.to_string())
        .or_insert_with(|| {
            let (tx, _) = broadcast::channel(64);
            Arc::new(Room {
                tx,
                timers: RwLock::new(Vec::new()),
            })
        })
        .clone();
    let rx = room.tx.subscribe();
    (room, rx)
}

fn addresses_a_real_spell(enemy_idx: u8, spell_idx: u8) -> bool {
    enemy_idx < MAX_ENEMIES && spell_idx < MAX_SPELLS
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Whether a timer still has time left to run.
///
/// `started_at` comes from a peer's wall clock, so the addition is checked: a
/// bogus value near u64::MAX would otherwise panic in debug and wrap in release.
fn is_live(timer: &ActiveTimer) -> bool {
    match timer.started_at.checked_add(timer.cooldown_secs as u64) {
        Some(ends_at) => ends_at > unix_now(),
        None => false,
    }
}
