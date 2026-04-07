// Prevents additional console window on Windows in release
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod actions;
mod champions;
mod focus;
mod hotkey;
mod lcu;
mod overlay;
mod settings;
mod sync;

use champions::Champions;
use lcu::{LcuEvent, LcuMonitor, QueueModeInfo};
use overlay::OverlayState;
use settings::{Settings, SettingsManager};
use std::sync::Arc;
use tauri::{Emitter, Manager};
use tauri_plugin_autostart::ManagerExt;

struct AppState {
    settings_manager: SettingsManager,
    champions: Champions,
    lcu_monitor: LcuMonitor,
    last_action: tokio::sync::Mutex<String>,
    current_queue_mode: tokio::sync::Mutex<Option<QueueModeInfo>>,
    overlay_state: Arc<OverlayState>,
    sync_client: tokio::sync::Mutex<Option<Arc<sync::SyncClient>>>,
}

// Tauri commands called from the frontend

#[tauri::command]
fn get_settings(state: tauri::State<'_, Arc<AppState>>) -> Settings {
    state.settings_manager.get()
}

#[tauri::command]
fn update_settings(
    app: tauri::AppHandle,
    state: tauri::State<'_, Arc<AppState>>,
    settings: Settings,
) {
    let old = state.settings_manager.get();
    let overlay_changed = settings.overlay_enabled != old.overlay_enabled;
    let overlay_on = settings.overlay_enabled;
    let opacity_changed = (settings.overlay_opacity - old.overlay_opacity).abs() > 0.001;
    let opacity = settings.overlay_opacity;

    // Save first, react after
    state.settings_manager.update(settings.clone());

    if opacity_changed {
        let _ = app.emit("overlay-opacity", opacity);
    }

    if overlay_changed {
        let overlay_state = state.overlay_state.clone();
        let ah = app.clone();
        // Spawn overlay creation/destruction off the main thread to avoid deadlock
        tauri::async_runtime::spawn(async move {
            if overlay_on {
                let s = Settings { overlay_enabled: true, overlay_opacity: opacity, ..settings };
                match create_overlay_window(&ah, &s) {
                    Ok(_) => {
                        let _ = ah.emit("log", "Overlay activat");
                        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                        let enemies = overlay_state.enemies.lock().await.clone();
                        let _ = ah.emit("overlay-data", &enemies);
                        overlay::poll_live_client_runes(overlay_state.clone()).await;
                        let enemies = overlay_state.enemies.lock().await.clone();
                        let _ = ah.emit("overlay-data", &enemies);
                    }
                    Err(e) => {
                        let _ = ah.emit("log", &format!("Error overlay: {}", e));
                    }
                }
            } else {
                if let Some(w) = ah.get_webview_window("overlay") {
                    let _ = w.close();
                }
                let _ = ah.emit("log", "Overlay desactivat");
            }
        });
    }
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

#[tauri::command]
async fn get_overlay_data(
    state: tauri::State<'_, Arc<AppState>>,
) -> Result<Vec<overlay::EnemyData>, String> {
    Ok(state.overlay_state.enemies.lock().await.clone())
}

#[tauri::command]
async fn save_overlay_position(
    state: tauri::State<'_, Arc<AppState>>,
    x: f64,
    y: f64,
) -> Result<(), String> {
    let mut settings = state.settings_manager.get();
    settings.overlay_x = Some(x);
    settings.overlay_y = Some(y);
    state.settings_manager.update(settings);
    Ok(())
}

#[tauri::command]
async fn send_timer_event(
    state: tauri::State<'_, Arc<AppState>>,
    enemy_idx: u8,
    spell_idx: u8,
    cooldown_secs: u32,
) -> Result<(), String> {
    let started_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();

    // Broadcast to sync server if connected
    let client = state.sync_client.lock().await;
    if let Some(ref c) = *client {
        let _ = c
            .send(instalock_shared::SyncMessage::TimerStart {
                enemy_idx,
                spell_idx,
                cooldown_secs,
                started_at,
            })
            .await;
    }
    Ok(())
}

#[tauri::command]
async fn cancel_timer_event(
    state: tauri::State<'_, Arc<AppState>>,
    enemy_idx: u8,
    spell_idx: u8,
) -> Result<(), String> {
    let client = state.sync_client.lock().await;
    if let Some(ref c) = *client {
        let _ = c
            .send(instalock_shared::SyncMessage::TimerCancel {
                enemy_idx,
                spell_idx,
            })
            .await;
    }
    Ok(())
}

fn main() {
    let overlay_state = Arc::new(OverlayState::new());

    let app_state = Arc::new(AppState {
        settings_manager: SettingsManager::new(),
        champions: Champions::new(),
        lcu_monitor: LcuMonitor::new(),
        last_action: tokio::sync::Mutex::new(String::new()),
        current_queue_mode: tokio::sync::Mutex::new(None),
        overlay_state: overlay_state.clone(),
        sync_client: tokio::sync::Mutex::new(None),
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
            get_overlay_data,
            save_overlay_position,
            send_timer_event,
            cancel_timer_event,
        ])
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { .. } = event {
                if window.label() == "main" {
                    // Close overlay when main window is closed
                    if let Some(overlay) = window.app_handle().get_webview_window("overlay") {
                        let _ = overlay.close();
                    }
                }
            }
        })
        .setup(move |app| {
            let app_handle = app.handle().clone();
            let state = app_state.clone();

            // Poll Shift key state: hold to interact with overlay, release to click-through
            let ah_shortcut = app.handle().clone();
            hotkey::start_shift_poll(move |pressed| {
                if let Some(overlay_win) = ah_shortcut.get_webview_window("overlay") {
                    let _ = overlay_win.set_ignore_cursor_events(!pressed);
                    let _ = ah_shortcut.emit("overlay-interactive", pressed);
                }
            });

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
                            // Extract enemy data for overlay
                            let id_to_name = state.champions.get_id_to_name();
                            let enemies = overlay::extract_enemies(&data, &id_to_name);
                            if !enemies.is_empty() {
                                *state.overlay_state.enemies.lock().await = enemies;
                            }

                            handle_champ_select(&state, &app_handle, &data).await;
                        }
                        LcuEvent::GameflowPhase(phase) => {
                            handle_gameflow_phase(&state, &app_handle, &phase).await;
                        }
                        LcuEvent::QueueMode(info) => {
                            *state.current_queue_mode.lock().await = info.clone();
                            let _ =
                                app_handle.emit("queue-mode", build_queue_mode_payload(&info));
                        }
                    }
                }
            });

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

async fn handle_gameflow_phase(state: &Arc<AppState>, app_handle: &tauri::AppHandle, phase: &str) {
    match phase {
        "ChampSelect" => {
            // Reset is handled by GameflowPhase != ChampSelect below
        }
        "InProgress" => {
            let settings = state.settings_manager.get();
            if settings.overlay_enabled {
                // Create overlay window
                if let Err(e) = create_overlay_window(app_handle, &settings) {
                    log::error!("Failed to create overlay: {}", e);
                    let _ = app_handle.emit("log", &format!("Error overlay: {}", e));
                } else {
                    let _ = app_handle.emit("log", "Overlay obert");

                    // Start polling for rune data + emit to overlay once ready
                    let overlay_state = state.overlay_state.clone();
                    let ah = app_handle.clone();
                    tokio::spawn(async move {
                        // Wait a bit for the overlay window to initialize its JS listeners
                        tokio::time::sleep(std::time::Duration::from_secs(1)).await;

                        // Send initial data
                        let enemies = overlay_state.enemies.lock().await.clone();
                        let _ = ah.emit("overlay-data", &enemies);

                        // Poll for rune data (retries internally)
                        overlay::poll_live_client_runes(overlay_state.clone()).await;

                        // Send updated data with runes
                        let enemies = overlay_state.enemies.lock().await.clone();
                        let _ = ah.emit("overlay-data", &enemies);
                    });
                }

                // Connect to sync server if enabled
                if settings.sync_enabled && !settings.sync_server_url.is_empty() {
                    let game_id = state
                        .overlay_state
                        .game_id
                        .lock()
                        .await
                        .clone()
                        .unwrap_or_else(|| "unknown".to_string());

                    match sync::SyncClient::connect(
                        &settings.sync_server_url,
                        &game_id,
                        "player",
                    )
                    .await
                    {
                        Ok(client) => {
                            // Forward incoming sync messages to the overlay
                            let mut rx = client.incoming_tx.subscribe();
                            let ah = app_handle.clone();
                            tokio::spawn(async move {
                                while let Ok(msg) = rx.recv().await {
                                    let _ = ah.emit("sync-message", &msg);
                                }
                            });

                            *state.sync_client.lock().await = Some(client);
                            let _ = app_handle.emit("log", "Sync connectat");
                        }
                        Err(e) => {
                            log::error!("Sync connect failed: {}", e);
                            let _ =
                                app_handle.emit("log", &format!("Sync error: {}", e));
                        }
                    }
                }
            }
        }
        "EndOfGame" | "Lobby" | "None" | "WaitingForStats" => {
            // Destroy overlay window
            if let Some(overlay_win) = app_handle.get_webview_window("overlay") {
                let _ = overlay_win.close();
                let _ = app_handle.emit("log", "Overlay tancat");
            }

            // Disconnect sync
            *state.sync_client.lock().await = None;

            // Clear overlay state
            *state.overlay_state.enemies.lock().await = Vec::new();
            *state.overlay_state.game_id.lock().await = None;
        }
        _ => {}
    }

    if phase != "ChampSelect" {
        // Reset dedup guard when leaving champ select
        *state.last_action.lock().await = String::new();
    }
}

fn create_overlay_window(
    app_handle: &tauri::AppHandle,
    settings: &Settings,
) -> Result<(), String> {
    // Don't create if already exists
    if app_handle.get_webview_window("overlay").is_some() {
        return Ok(());
    }

    let mut builder = tauri::WebviewWindowBuilder::new(
        app_handle,
        "overlay",
        tauri::WebviewUrl::App("overlay.html".into()),
    )
    .title("InstaLock Overlay")
    .inner_size(300.0, 340.0)
    .transparent(true)
    .shadow(false)
    .decorations(false)
    .always_on_top(true)
    .skip_taskbar(true)
    .resizable(false)
    .focused(false);

    if let (Some(x), Some(y)) = (settings.overlay_x, settings.overlay_y) {
        builder = builder.position(x, y);
    } else {
        builder = builder.center();
    }

    let window = builder.build().map_err(|e| e.to_string())?;
    let _ = window.set_ignore_cursor_events(true);

    // Windows: match electron-overlay-window pattern
    // - TOOLWINDOW + NOACTIVATE styles
    // - Monitor foreground changes with SetWinEventHook
    // - Re-assert TOPMOST when LoL has focus, hide when not
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::WindowsAndMessaging::*;
        let hwnd = window.hwnd().map_err(|e| e.to_string())?.0 as isize;

        unsafe {
            let ex_style = GetWindowLongW(hwnd as _, GWL_EXSTYLE);
            SetWindowLongW(
                hwnd as _,
                GWL_EXSTYLE,
                ex_style | WS_EX_TOOLWINDOW as i32 | WS_EX_NOACTIVATE as i32,
            );
        }

        std::thread::spawn(move || {
            use windows_sys::Win32::UI::WindowsAndMessaging::*;
            unsafe {
                // Initial TOPMOST
                SetWindowPos(
                    hwnd as *mut std::ffi::c_void,
                    HWND_TOPMOST,
                    0, 0, 0, 0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW,
                );

                // Poll foreground window — show overlay when LoL is focused
                loop {
                    std::thread::sleep(std::time::Duration::from_millis(250));

                    let fg = GetForegroundWindow();
                    if fg.is_null() {
                        continue;
                    }

                    // Check if foreground window is LoL or our overlay
                    let mut title = [0u16; 256];
                    let len = GetWindowTextW(fg, title.as_mut_ptr(), 256);
                    let title_str = String::from_utf16_lossy(&title[..len as usize]);

                    let is_lol = title_str.contains("League of Legends");
                    let is_overlay = fg as isize == hwnd;

                    if is_lol || is_overlay {
                        // Show and re-assert TOPMOST
                        ShowWindow(hwnd as _, SW_SHOWNOACTIVATE);
                        SetWindowPos(
                            hwnd as *mut std::ffi::c_void,
                            HWND_TOPMOST,
                            0, 0, 0, 0,
                            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW,
                        );
                    } else {
                        // Hide overlay when LoL is not focused
                        ShowWindow(hwnd as _, SW_HIDE);
                    }
                }
            }
        });
    }

    Ok(())
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
    let timer_left = session["timer"]["adjustedTimeLeftInPhase"]
        .as_f64()
        .unwrap_or(0.0);
    let timer_phase = session["timer"]["phase"].as_str().unwrap_or("");

    // Skip if we're in planning phase or timer hasn't started
    if timer_phase == "PLANNING" || timer_phase == "GAME_STARTING" || timer_left <= 0.0 {
        return;
    }

    // Resolve the current game mode
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

    // ARAM has no human pick or ban phase
    if mode.as_deref() == Some("ARAM") {
        return;
    }

    let my_cell_id = match session["localPlayerCellId"].as_i64() {
        Some(id) => id,
        None => return,
    };

    // Find my current action
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

    // Arena (CHERRY) has no human ban phase
    if action_type == "ban" && mode.as_deref() == Some("CHERRY") {
        return;
    }

    // Capture focus before any action
    let captured_hwnd = if settings.restore_focus_after_action {
        focus::capture_foreground()
    } else {
        None
    };

    match action_type {
        "ban" if settings.auto_ban && !settings.ban_champion.is_empty() => {
            let delay = settings
                .ban_delay_secs
                .min(timer_left - settings.action_margin_secs)
                .max(0.0);
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
                        let _ = app_handle.emit("log", &format!("Error banning: {}", e));
                        *state.last_action.lock().await = String::new();
                    }
                }
            } else {
                let _ = app_handle.emit(
                    "log",
                    &format!("Champion '{}' no trobat per ban", settings.ban_champion),
                );
                *state.last_action.lock().await = String::new();
            }
        }
        "pick"
            if settings.auto_pick
                && mode.as_deref() == Some("CHERRY")
                && settings.bravery_enabled =>
        {
            let delay = settings
                .pick_delay_secs
                .min(timer_left - settings.action_margin_secs)
                .max(0.0);
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
            if let Some(champ_id) = state.champions.resolve_id(&settings.pick_champion) {
                if settings.hover_pick {
                    // Hover immediately so teammates see the pick
                    if let Err(e) = actions::hover_champion(&creds, action_id, champ_id).await {
                        let _ = app_handle.emit("log", &format!("Error hovering: {}", e));
                    } else {
                        let _ = app_handle.emit("log", &format!("Hovering {}...", settings.pick_champion));
                    }
                    let delay = settings
                        .pick_delay_secs
                        .min(timer_left - settings.action_margin_secs)
                        .max(0.0);
                    if delay > 0.0 {
                        tokio::time::sleep(std::time::Duration::from_secs_f64(delay)).await;
                    }
                    match actions::lock_champion(&creds, action_id, champ_id).await {
                        Ok(_) => {
                            let _ = app_handle.emit("log", &format!("Picked {}!", settings.pick_champion));
                            spawn_focus_restore(captured_hwnd);
                        }
                        Err(e) => {
                            let _ = app_handle.emit("log", &format!("Error picking: {}", e));
                            *state.last_action.lock().await = String::new();
                        }
                    }
                } else {
                    let delay = settings
                        .pick_delay_secs
                        .min(timer_left - settings.action_margin_secs)
                        .max(0.0);
                    if delay > 0.0 {
                        tokio::time::sleep(std::time::Duration::from_secs_f64(delay)).await;
                    }
                    match actions::pick_champion(&creds, action_id, champ_id).await {
                        Ok(_) => {
                            let _ = app_handle.emit("log", &format!("Picked {}!", settings.pick_champion));
                            spawn_focus_restore(captured_hwnd);
                        }
                        Err(e) => {
                            let _ = app_handle.emit("log", &format!("Error picking: {}", e));
                            *state.last_action.lock().await = String::new();
                        }
                    }
                }
            } else {
                let _ = app_handle.emit(
                    "log",
                    &format!("Champion '{}' no trobat per pick", settings.pick_champion),
                );
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
