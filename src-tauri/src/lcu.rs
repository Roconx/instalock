use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::broadcast;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;

#[derive(Debug, Clone)]
pub struct QueueModeInfo {
    pub game_mode: String,
    pub queue_id: i64,
}

#[derive(Debug, Clone)]
pub enum LcuEvent {
    Connected,
    Disconnected,
    ReadyCheck(serde_json::Value),
    ChampSelect(serde_json::Value),
    GameflowPhase(String),
    QueueMode(Option<QueueModeInfo>),
}

#[derive(Clone)]
pub struct LcuCredentials {
    pub port: u16,
    pub password: String,
}

impl LcuCredentials {
    fn auth_header(&self) -> String {
        let encoded =
            base64::engine::general_purpose::STANDARD.encode(format!("riot:{}", self.password));
        format!("Basic {}", encoded)
    }

    fn url(&self, path: &str) -> String {
        format!("https://127.0.0.1:{}{}", self.port, path)
    }
}

pub struct LcuMonitor {
    pub event_tx: broadcast::Sender<LcuEvent>,
    pub connected: Arc<AtomicBool>,
    running: Arc<AtomicBool>,
    credentials: Arc<tokio::sync::Mutex<Option<LcuCredentials>>>,
}

impl LcuMonitor {
    pub fn new() -> Self {
        // Sized generously: champ select emits a session update on every hover,
        // ban and timer tick, and a slow receiver that lags loses events.
        let (event_tx, _) = broadcast::channel(256);
        Self {
            event_tx,
            connected: Arc::new(AtomicBool::new(false)),
            running: Arc::new(AtomicBool::new(false)),
            credentials: Arc::new(tokio::sync::Mutex::new(None)),
        }
    }

    pub async fn get_credentials(&self) -> Option<LcuCredentials> {
        self.credentials.lock().await.clone()
    }

    pub fn start(&self) {
        self.running.store(true, Ordering::SeqCst);
        let running = self.running.clone();
        let connected = self.connected.clone();
        let event_tx = self.event_tx.clone();
        let credentials = self.credentials.clone();

        tokio::spawn(async move {
            while running.load(Ordering::SeqCst) {
                // find_lockfile does blocking file reads and spawns a process.
                let found = tokio::task::spawn_blocking(find_lockfile)
                    .await
                    .unwrap_or(None);

                if let Some(creds) = found {
                    if let Err(e) =
                        run_session(&creds, &event_tx, &running, &connected, &credentials).await
                    {
                        // A handshake failure here is expected when LoL has left a
                        // stale lockfile behind — log quietly and retry.
                        log::debug!("LCU session ended: {}", e);
                    }
                } else {
                    log::debug!("LCU not found, retrying in 5s...");
                }

                if running.load(Ordering::SeqCst) {
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                }
            }
        });
    }
}

fn find_lockfile() -> Option<LcuCredentials> {
    // Common install roots, tried across every fixed drive rather than just C:
    // and D: — a LoL install on E: used to mean a permanent "Desconnectat".
    const SUFFIXES: [&str; 3] = [
        "Riot Games\\League of Legends\\lockfile",
        "Program Files\\Riot Games\\League of Legends\\lockfile",
        "Program Files (x86)\\Riot Games\\League of Legends\\lockfile",
    ];

    for drive in 'C'..='Z' {
        for suffix in &SUFFIXES {
            let path = format!("{}:\\{}", drive, suffix);
            if let Ok(content) = std::fs::read_to_string(&path) {
                if let Some(creds) = parse_lockfile(&content) {
                    log::info!("Found lockfile at {}", path);
                    return Some(creds);
                }
            }
        }
    }

    find_lockfile_from_process()
}

/// Fallback: read the port and token straight off the running client's command
/// line. wmic is gone on current Windows 11, so this goes through CIM instead.
fn find_lockfile_from_process() -> Option<LcuCredentials> {
    let mut cmd = std::process::Command::new("powershell");
    cmd.args([
        "-NoProfile",
        "-NonInteractive",
        "-Command",
        "(Get-CimInstance Win32_Process -Filter \"name='LeagueClientUx.exe'\").CommandLine",
    ]);

    // The app is built with windows_subsystem = "windows", so without this a
    // console window flashes every 5 seconds while LoL is closed.
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    let output = cmd.output().ok()?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let port: u16 = extract_arg(&stdout, "--app-port=")?.parse().ok()?;
    let password = extract_arg(&stdout, "--remoting-auth-token=")?;

    Some(LcuCredentials { port, password })
}

fn extract_arg(text: &str, prefix: &str) -> Option<String> {
    let start = text.find(prefix)? + prefix.len();
    let rest = &text[start..];
    let end = rest
        .find(|c: char| c.is_whitespace() || c == '"')
        .unwrap_or(rest.len());
    Some(rest[..end].to_string())
}

fn parse_lockfile(content: &str) -> Option<LcuCredentials> {
    let parts: Vec<&str> = content.split(':').collect();
    if parts.len() < 5 {
        return None;
    }
    Some(LcuCredentials {
        port: parts[2].parse().ok()?,
        password: parts[3].to_string(),
    })
}

async fn run_session(
    creds: &LcuCredentials,
    event_tx: &broadcast::Sender<LcuEvent>,
    running: &Arc<AtomicBool>,
    connected: &Arc<AtomicBool>,
    credentials: &Arc<tokio::sync::Mutex<Option<LcuCredentials>>>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // 1. Perform the WebSocket handshake FIRST. If it fails (e.g. the lockfile
    //    is stale because LoL was closed without cleaning up), we return early
    //    and never announce Connected — so the UI stays stably on Desconnectat
    //    instead of flapping every 5 seconds.
    let url = format!("wss://127.0.0.1:{}/", creds.port);
    let mut request = url.into_client_request()?;
    request
        .headers_mut()
        .insert("Authorization", creds.auth_header().parse()?);

    let tls = native_tls::TlsConnector::builder()
        .danger_accept_invalid_certs(true)
        .build()?;
    let connector = tokio_tungstenite::Connector::NativeTls(tls);

    let (mut ws, _) =
        tokio_tungstenite::connect_async_tls_with_config(request, None, false, Some(connector))
            .await?;

    // 2. Handshake succeeded — the LCU is actually reachable. Announce connected.
    log::info!("LCU connected on port {}", creds.port);
    *credentials.lock().await = Some(creds.clone());
    connected.store(true, Ordering::SeqCst);
    let _ = event_tx.send(LcuEvent::Connected);

    // Fire an initial queue-mode snapshot so the UI reflects any lobby that
    // was already open when the app started (otherwise we'd wait for a WS
    // update that may never come).
    let _ = event_tx.send(fetch_initial_lobby(creds).await);

    // Fire an initial gameflow phase snapshot so the overlay opens if already in-game
    if let Ok(data) = lcu_request(creds, "GET", "/lol-gameflow/v1/gameflow-phase", None).await {
        if let Some(phase) = data.as_str() {
            let _ = event_tx.send(LcuEvent::GameflowPhase(phase.to_string()));
        }
    }

    // If already in champ select, fetch the session for enemy data
    if let Ok(data) = lcu_request(creds, "GET", "/lol-champ-select/v1/session", None).await {
        if data.get("theirTeam").is_some() {
            let _ = event_tx.send(LcuEvent::ChampSelect(data));
        }
    }

    // 3. Subscribe and run the message loop. Any error from here on represents a
    //    real disconnect that must be paired with a Disconnected event below.
    let loop_result: Result<(), Box<dyn std::error::Error + Send + Sync>> = async {
        ws.send(Message::Text("[5, \"OnJsonApiEvent\"]".into()))
            .await?;

        while let Some(msg) = ws.next().await {
            if !running.load(Ordering::SeqCst) {
                break;
            }

            match msg {
                Ok(Message::Text(text)) => parse_lcu_message(&text.to_string(), event_tx),
                Ok(Message::Close(_)) | Err(_) => break,
                _ => {}
            }
        }

        Ok(())
    }
    .await;

    // 4. Always pair the Connected from step 2 with a Disconnected on exit.
    log::info!("LCU WebSocket closed");
    connected.store(false, Ordering::SeqCst);
    *credentials.lock().await = None;
    let _ = event_tx.send(LcuEvent::Disconnected);

    loop_result
}

async fn fetch_initial_lobby(creds: &LcuCredentials) -> LcuEvent {
    match lcu_request(creds, "GET", "/lol-lobby/v2/lobby", None).await {
        Ok(data) => LcuEvent::QueueMode(parse_queue_mode(&data)),
        // 404 => no lobby open yet. Any other error is also treated as "no info"
        // so we don't crash startup if the client is in a weird state.
        Err(_) => LcuEvent::QueueMode(None),
    }
}

fn parse_lcu_message(text: &str, event_tx: &broadcast::Sender<LcuEvent>) {
    let Ok(arr) = serde_json::from_str::<serde_json::Value>(text) else {
        return;
    };
    let Some(arr) = arr.as_array() else { return };
    if arr.len() < 3 || arr[0].as_u64() != Some(8) {
        return;
    }

    let Some(payload) = arr[2].as_object() else {
        return;
    };
    let uri = payload
        .get("uri")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let event_type = payload
        .get("eventType")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let data = payload
        .get("data")
        .cloned()
        .unwrap_or(serde_json::Value::Null);

    match uri {
        "/lol-matchmaking/v1/ready-check" => {
            let _ = event_tx.send(LcuEvent::ReadyCheck(data));
        }
        "/lol-champ-select/v1/session" => {
            let _ = event_tx.send(LcuEvent::ChampSelect(data));
        }
        "/lol-gameflow/v1/gameflow-phase" => {
            if let Some(phase) = data.as_str() {
                let _ = event_tx.send(LcuEvent::GameflowPhase(phase.to_string()));
            }
        }
        "/lol-lobby/v2/lobby" => {
            let info = if event_type == "Delete" {
                None
            } else {
                parse_queue_mode(&data)
            };
            let _ = event_tx.send(LcuEvent::QueueMode(info));
        }
        _ => {}
    }
}

fn parse_queue_mode(data: &serde_json::Value) -> Option<QueueModeInfo> {
    let game_mode = data
        .get("gameConfig")?
        .get("gameMode")?
        .as_str()?
        .to_string();
    let queue_id = data.get("gameConfig")?.get("queueId")?.as_i64()?;
    Some(QueueModeInfo {
        game_mode,
        queue_id,
    })
}

/// One client for the whole process: rebuilding it per request repeats the TLS
/// setup on the latency-critical lock path, and the config never varies.
static HTTP: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();

fn http() -> &'static reqwest::Client {
    HTTP.get_or_init(|| {
        reqwest::Client::builder()
            .danger_accept_invalid_certs(true)
            // Without this a stalled LCU hangs a pick forever: the hover PATCH
            // never returns, the lock never runs, and nothing is ever logged.
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .expect("failed to build HTTP client")
    })
}

/// Make an HTTP request to the LCU API
pub async fn lcu_request(
    creds: &LcuCredentials,
    method: &str,
    path: &str,
    body: Option<serde_json::Value>,
) -> Result<serde_json::Value, String> {
    let client = http();

    let mut req = match method {
        "POST" => client.post(creds.url(path)),
        "PATCH" => client.patch(creds.url(path)),
        "PUT" => client.put(creds.url(path)),
        "DELETE" => client.delete(creds.url(path)),
        _ => client.get(creds.url(path)),
    };

    req = req.header("Authorization", creds.auth_header());

    if let Some(body) = body {
        req = req.json(&body);
    }

    let resp = req.send().await.map_err(|e| e.to_string())?;
    let status = resp.status();
    let text = resp.text().await.map_err(|e| e.to_string())?;

    if !status.is_success() && status.as_u16() != 204 {
        return Err(format!("HTTP {} - {}", status.as_u16(), text));
    }

    if text.is_empty() {
        Ok(serde_json::Value::Null)
    } else {
        Ok(serde_json::from_str(&text).unwrap_or(serde_json::Value::Null))
    }
}
