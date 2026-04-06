// Prevents additional console window on Windows in release
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod actions;
mod champions;
mod focus;
mod lcu;
mod settings;

use champions::Champions;
use lcu::{LcuEvent, LcuMonitor, QueueModeInfo};
use settings::{Settings, SettingsManager};
use std::sync::Arc;
use tauri::Emitter;
use tauri_plugin_autostart::ManagerExt;

struct AppState {
    settings_manager: SettingsManager,
    champions: Champions,
    lcu_monitor: LcuMonitor,
    last_action: tokio::sync::Mutex<String>,
    current_queue_mode: tokio::sync::Mutex<Option<QueueModeInfo>>,
}

// Tauri commands called from the frontend

#[tauri::command]
fn get_settings(state: tauri::State<'_, Arc<AppState>>) -> Settings {
    state.settings_manager.get()
}

#[tauri::command]
fn update_settings(state: tauri::State<'_, Arc<AppState>>, settings: Settings) {
    state.settings_manager.update(settings);
}

#[tauri::command]
fn get_champions(state: tauri::State<'_, Arc<AppState>>) -> Vec<String> {
    state.champions.get_names()
}

#[tauri::command]
fn is_lcu_connected(state: tauri::State<'_, Arc<AppState>>) -> bool {
    state
        .lcu_monitor
        .connected
        .load(std::sync::atomic::Ordering::SeqCst)
}

#[tauri::command]
fn get_autostart(app: tauri::AppHandle) -> bool {
    app.autolaunch().is_enabled().unwrap_or(false)
}

#[tauri::command]
fn set_autostart(app: tauri::AppHandle, enabled: bool) {
    let autostart = app.autolaunch();
    if enabled {
        let _ = autostart.enable();
    } else {
        let _ = autostart.disable();
    }
}

fn main() {
    let app_state = Arc::new(AppState {
        settings_manager: SettingsManager::new(),
        champions: Champions::new(),
        lcu_monitor: LcuMonitor::new(),
        last_action: tokio::sync::Mutex::new(String::new()),
        current_queue_mode: tokio::sync::Mutex::new(None),
    });

    tauri::Builder::default()
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec!["--autostart"]),
        ))
        .manage(app_state.clone())
        .invoke_handler(tauri::generate_handler![
            get_settings,
            update_settings,
            get_champions,
            is_lcu_connected,
            get_autostart,
            set_autostart,
        ])
        .setup(move |app| {
            let app_handle = app.handle().clone();
            let state = app_state.clone();

            // Spawn background tasks
            tauri::async_runtime::spawn(async move {
                // Load champion data
                state.champions.load().await;

                // Emit champions loaded event
                let _ = app_handle.emit("champions-loaded", ());

                // Start LCU monitor
                state.lcu_monitor.start();
                let mut event_rx = state.lcu_monitor.event_tx.subscribe();

                // Listen for LCU events and react
                while let Ok(event) = event_rx.recv().await {
                    match event {
                        LcuEvent::Connected => {
                            let _ = app_handle.emit("lcu-status", true);
                            let _ = app_handle.emit("log", "LCU connectat");
                        }
                        LcuEvent::Disconnected => {
                            let _ = app_handle.emit("lcu-status", false);
                            let _ = app_handle.emit("log", "LCU desconnectat");
                            // Reset dedup guard
                            *state.last_action.lock().await = String::new();
                            // Clear queue mode so the UI indicator disappears
                            *state.current_queue_mode.lock().await = None;
                            let _ = app_handle.emit("queue-mode", serde_json::Value::Null);
                        }
                        LcuEvent::ReadyCheck(data) => {
                            handle_ready_check(&state, &app_handle, &data).await;
                        }
                        LcuEvent::ChampSelect(data) => {
                            handle_champ_select(&state, &app_handle, &data).await;
                        }
                        LcuEvent::GameflowPhase(phase) => {
                            if phase != "ChampSelect" {
                                // Reset dedup guard when leaving champ select
                                *state.last_action.lock().await = String::new();
                            }
                        }
                        LcuEvent::QueueMode(info) => {
                            *state.current_queue_mode.lock().await = info.clone();
                            let _ = app_handle.emit("queue-mode", build_queue_mode_payload(&info));
                        }
                    }
                }
            });

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

/// Spawn a background task that waits for LoL to grab focus and then forces
/// the user's original window back to the foreground. No-op if `hwnd` is None.
fn spawn_focus_restore(hwnd: Option<isize>) {
    if let Some(hwnd) = hwnd {
        tauri::async_runtime::spawn(async move {
            // Give the LoL client time to call SetForegroundWindow so our
            // restore actually overrides it rather than being overridden.
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            focus::restore_foreground(hwnd);
        });
    }
}

async fn handle_ready_check(
    state: &Arc<AppState>,
    app_handle: &tauri::AppHandle,
    data: &serde_json::Value,
) {
    let settings = state.settings_manager.get();
    if !settings.auto_accept {
        return;
    }

    let is_in_progress = data["state"].as_str() == Some("InProgress");
    let no_response = data["playerResponse"].as_str() == Some("None");

    if is_in_progress && no_response {
        // Delay before accepting (capped so we always act before timer expires)
        let accept_delay = settings
            .accept_delay_secs
            .min(12.0 - settings.action_margin_secs)
            .max(0.0);
        if accept_delay > 0.0 {
            tokio::time::sleep(std::time::Duration::from_secs_f64(accept_delay)).await;
        }

        // Capture the user's current foreground window BEFORE accepting so we
        // can restore it after LoL inevitably steals focus on the ready check.
        let captured_hwnd = if settings.restore_focus_after_action {
            focus::capture_foreground()
        } else {
            None
        };

        if let Some(creds) = state.lcu_monitor.get_credentials().await {
            match actions::accept_match(&creds).await {
                Ok(_) => {
                    let _ = app_handle.emit("log", "Partida acceptada!");
                    spawn_focus_restore(captured_hwnd);
                }
                Err(e) => {
                    let _ = app_handle.emit("log", &format!("Error acceptant: {}", e));
                }
            }
        }
    }
}

async fn handle_champ_select(
    state: &Arc<AppState>,
    app_handle: &tauri::AppHandle,
    session: &serde_json::Value,
) {
    let settings = state.settings_manager.get();
    let Some(creds) = state.lcu_monitor.get_credentials().await else {
        return;
    };

    // Check the timer - only act when there's an active countdown
    // This prevents acting during PLANNING phase or before the phase truly starts
    let timer_left = session["timer"]["adjustedTimeLeftInPhase"]
        .as_f64()
        .unwrap_or(0.0);
    let timer_phase = session["timer"]["phase"]
        .as_str()
        .unwrap_or("");

    // Skip if we're in planning phase or timer hasn't started
    if timer_phase == "PLANNING" || timer_phase == "GAME_STARTING" || timer_left <= 0.0 {
        return;
    }

    // Resolve the current game mode: prefer the lobby-derived value, fall back
    // to the session's own queueId mapping so we still act correctly if the app
    // started mid-champ-select and never saw a lobby event.
    let mode: Option<String> = {
        let current = state.current_queue_mode.lock().await;
        current
            .as_ref()
            .map(|q| q.game_mode.clone())
            .or_else(|| match session["queueId"].as_i64() {
                Some(450) => Some("ARAM".to_string()),
                Some(1700) | Some(1710) => Some("CHERRY".to_string()),
                _ => None,
            })
    };

    // ARAM has no human pick or ban phase — skip entirely regardless of settings.
    if mode.as_deref() == Some("ARAM") {
        return;
    }

    let my_cell_id = match session["localPlayerCellId"].as_i64() {
        Some(id) => id,
        None => return,
    };

    // Find my current action, capturing both type and action id so the Bravery
    // path can PATCH the action directly without a second find_my_action round-trip.
    let mut my_action: Option<(&str, i64)> = None;
    if let Some(actions) = session["actions"].as_array() {
        for group in actions {
            if let Some(group_arr) = group.as_array() {
                for action in group_arr {
                    let actor = action["actorCellId"].as_i64().unwrap_or(-1);
                    let in_progress = action["isInProgress"].as_bool().unwrap_or(false);
                    let completed = action["completed"].as_bool().unwrap_or(true);

                    if actor == my_cell_id && in_progress && !completed {
                        if let (Some(t), Some(id)) =
                            (action["type"].as_str(), action["id"].as_i64())
                        {
                            my_action = Some((t, id));
                        }
                    }
                }
            }
        }
    }

    let Some((action_type, action_id)) = my_action else {
        return;
    };

    // Dedup: don't repeat the same action
    let dedup_key = format!("{}:{}", action_type, my_cell_id);
    {
        let mut last = state.last_action.lock().await;
        if *last == dedup_key {
            return;
        }
        *last = dedup_key;
    }

    // Arena (CHERRY) has no human ban phase — the system pre-completes all bans.
    // Skip the ban branch to avoid spurious PATCH attempts.
    if action_type == "ban" && mode.as_deref() == Some("CHERRY") {
        return;
    }

    // Capture focus before any action so we can restore it after the LCU PATCH
    // (LoL client often grabs focus on ban/pick just like on ready check).
    let captured_hwnd = if settings.restore_focus_after_action {
        focus::capture_foreground()
    } else {
        None
    };

    match action_type {
        "ban" if settings.auto_ban && !settings.ban_champion.is_empty() => {
            let delay = settings.ban_delay_secs.min(timer_left - settings.action_margin_secs).max(0.0);
            if delay > 0.0 {
                tokio::time::sleep(std::time::Duration::from_secs_f64(delay)).await;
            }
            if let Some(champ_id) = state.champions.resolve_id(&settings.ban_champion) {
                match actions::ban_champion(&creds, action_id, champ_id).await {
                    Ok(_) => {
                        let _ = app_handle
                            .emit("log", &format!("Banned {}!", settings.ban_champion));
                        spawn_focus_restore(captured_hwnd);
                    }
                    Err(e) => {
                        let _ =
                            app_handle.emit("log", &format!("Error banning: {}", e));
                        *state.last_action.lock().await = String::new();
                    }
                }
            } else {
                let _ = app_handle.emit("log", &format!("Champion '{}' no trobat per ban", settings.ban_champion));
                *state.last_action.lock().await = String::new();
            }
        }
        "pick" if settings.auto_pick && mode.as_deref() == Some("CHERRY") && settings.bravery_enabled => {
            let delay = settings.pick_delay_secs.min(timer_left - settings.action_margin_secs).max(0.0);
            if delay > 0.0 {
                tokio::time::sleep(std::time::Duration::from_secs_f64(delay)).await;
            }
            match actions::pick_bravery(&creds, action_id).await {
                Ok(_) => {
                    let _ = app_handle.emit("log", "Bravery activada!");
                    spawn_focus_restore(captured_hwnd);
                }
                Err(e) => {
                    let _ = app_handle.emit("log", &format!("Error Bravery: {}", e));
                    *state.last_action.lock().await = String::new();
                }
            }
        }
        "pick" if settings.auto_pick && !settings.pick_champion.is_empty() => {
            let delay = settings.pick_delay_secs.min(timer_left - settings.action_margin_secs).max(0.0);
            if delay > 0.0 {
                tokio::time::sleep(std::time::Duration::from_secs_f64(delay)).await;
            }
            if let Some(champ_id) = state.champions.resolve_id(&settings.pick_champion) {
                match actions::pick_champion(&creds, action_id, champ_id).await {
                    Ok(_) => {
                        let _ = app_handle
                            .emit("log", &format!("Picked {}!", settings.pick_champion));
                        spawn_focus_restore(captured_hwnd);
                    }
                    Err(e) => {
                        let _ =
                            app_handle.emit("log", &format!("Error picking: {}", e));
                        *state.last_action.lock().await = String::new();
                    }
                }
            } else {
                let _ = app_handle.emit("log", &format!("Champion '{}' no trobat per pick", settings.pick_champion));
                *state.last_action.lock().await = String::new();
            }
        }
        _ => {
            // Reset dedup if we didn't act (e.g. feature disabled)
            *state.last_action.lock().await = String::new();
        }
    }
}

fn build_queue_mode_payload(info: &Option<QueueModeInfo>) -> serde_json::Value {
    match info {
        Some(q) => serde_json::json!({
            "gameMode": q.game_mode,
            "queueId": q.queue_id,
            "displayName": display_name_for(&q.game_mode),
        }),
        None => serde_json::Value::Null,
    }
}

fn display_name_for(game_mode: &str) -> String {
    match game_mode {
        "CLASSIC" => "Clàssic",
        "ARAM" => "ARAM",
        "CHERRY" => "Arena",
        "URF" => "URF",
        other => other,
    }
    .to_string()
}
