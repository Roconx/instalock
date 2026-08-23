// Prevents additional console window on Windows in release
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod actions;
mod appearance;
mod champions;
mod focus;
mod hotkey;
mod launcher;
mod lcu;
mod overlay;
mod settings;
mod sync;

use champions::Champions;
use lcu::{LcuEvent, LcuMonitor, QueueModeInfo};
use overlay::OverlayState;
use settings::{Settings, SettingsManager};
use std::sync::Arc;
use std::time::Duration;
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
    /// One accept per ready check. The LCU re-emits the ready-check event about
    /// once a second for the whole window and each one is handled on its own
    /// task, so without this every event would fire its own accept.
    accept_in_flight: std::sync::atomic::AtomicBool,
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
    let theme_changed = settings.theme != old.theme;
    let theme = settings.theme.clone();
    let pin_changed = settings.always_on_top != old.always_on_top;
    let pinned = settings.always_on_top;

    // Preserve overlay position (managed by save_overlay_position, not the frontend)
    let mut settings = settings;
    if settings.overlay_x.is_none() {
        settings.overlay_x = old.overlay_x;
    }
    if settings.overlay_y.is_none() {
        settings.overlay_y = old.overlay_y;
    }

    // Save first, react after
    state.settings_manager.update(settings.clone());

    if opacity_changed {
        let _ = app.emit("overlay-opacity", opacity);
    }

    // The overlay is a separate document, so it needs the theme pushed to it.
    if theme_changed {
        let _ = app.emit("theme", theme);
    }

    // The frontend already calls setAlwaysOnTop; mirroring it here keeps the
    // window right when the setting is changed from anywhere else.
    if pin_changed {
        if let Some(win) = app.get_webview_window("main") {
            let _ = win.set_always_on_top(pinned);
        }
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
                close_overlay_window(&ah);
                let _ = ah.emit("log", "Overlay desactivat");
            }
        });
    }
}

#[tauri::command]
fn get_champions(state: tauri::State<'_, Arc<AppState>>) -> Vec<String> {
    state.champions.get_names()
}

/// Champion names paired with their ids, so the picker can show icons.
#[tauri::command]
fn get_champion_options(
    state: tauri::State<'_, Arc<AppState>>,
) -> Vec<champions::ChampionOption> {
    state.champions.get_entries()
}

#[tauri::command]
fn is_lcu_connected(state: tauri::State<'_, Arc<AppState>>) -> bool {
    state
        .lcu_monitor
        .connected
        .load(std::sync::atomic::Ordering::SeqCst)
}

/// Start the League client. The button that calls this is only shown while the
/// LCU is disconnected, but a second call is harmless: the Riot Client just
/// focuses the session it already has.
#[tauri::command]
async fn launch_league(app: tauri::AppHandle) -> Result<(), String> {
    // Reads files and spawns a process - keep it off the async worker.
    let result = tokio::task::spawn_blocking(launcher::launch_league)
        .await
        .map_err(|e| e.to_string())?;

    match &result {
        Ok(_) => {
            let _ = app.emit("log", "Obrint League of Legends...");
        }
        Err(e) => {
            log::error!("Launch failed: {}", e);
            let _ = app.emit("log", &format!("Error obrint LoL: {}", e));
        }
    }

    result
}

/// Whether we can offer to launch League at all - the button stays hidden on a
/// machine where the Riot Client isn't installed.
#[tauri::command]
fn can_launch_league() -> bool {
    launcher::riot_client_path().is_some()
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

fn setup_file_logger() {
    use std::io::Write;

    let log_dir = dirs::data_local_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("InstaLock");
    let _ = std::fs::create_dir_all(&log_dir);
    let log_path = log_dir.join("instalock.log");

    // Truncate if > 5MB
    if let Ok(meta) = std::fs::metadata(&log_path) {
        if meta.len() > 5_000_000 {
            let _ = std::fs::remove_file(&log_path);
        }
    }

    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .expect("Cannot open log file");

    env_logger::Builder::new()
        .filter_level(log::LevelFilter::Debug)
        .format(move |buf, record| {
            writeln!(
                buf,
                "[{}] {} - {}",
                chrono::Local::now().format("%Y-%m-%d %H:%M:%S"),
                record.level(),
                record.args()
            )
        })
        .target(env_logger::Target::Pipe(Box::new(file)))
        .init();

    log::info!("=== InstaLock started ===");
    log::info!("Log file: {}", log_path.display());
}

/// System tray. Left click restores the window; the menu offers open and a real
/// quit, which is the only way out once minimize-to-tray is on.
fn setup_tray(app: &tauri::AppHandle) -> tauri::Result<()> {
    use tauri::menu::{Menu, MenuItem};
    use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};

    let open_item = MenuItem::with_id(app, "open", "Obrir InstaLock", true, None::<&str>)?;
    let quit_item = MenuItem::with_id(app, "quit", "Sortir", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open_item, &quit_item])?;

    let mut builder = TrayIconBuilder::with_id("instalock-tray")
        .tooltip("InstaLock")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "open" => restore_main_window(app),
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                restore_main_window(tray.app_handle());
            }
        });

    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }

    builder.build(app)?;
    Ok(())
}

/// Ask DWM to round the window and give it a real drop shadow.
///
/// Drawing the corner ourselves (border-radius on a transparent window) looked
/// wrong: the webview antialiases against an empty surface and the outward
/// box-shadow has nowhere to go but into the corner it just cut out, leaving a
/// grey smear around a soft curve. Letting the compositor own the shape gives
/// the same crisp corner as every other Windows 11 window.
#[cfg(windows)]
fn apply_native_rounding(window: &tauri::WebviewWindow) {
    use windows_sys::Win32::Foundation::HWND;
    use windows_sys::Win32::Graphics::Dwm::{
        DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
    };

    let Ok(hwnd) = window.hwnd() else { return };
    let pref: i32 = DWMWCP_ROUND;
    unsafe {
        DwmSetWindowAttribute(
            hwnd.0 as HWND,
            DWMWA_WINDOW_CORNER_PREFERENCE as u32,
            &pref as *const i32 as *const std::ffi::c_void,
            std::mem::size_of::<i32>() as u32,
        );
    }
}

#[cfg(not(windows))]
fn apply_native_rounding(_window: &tauri::WebviewWindow) {}

fn restore_main_window(app: &tauri::AppHandle) {
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.show();
        let _ = win.unminimize();
        let _ = win.set_focus();
    }
}

fn main() {
    setup_file_logger();

    let overlay_state = Arc::new(
        tokio::runtime::Runtime::new().unwrap().block_on(OverlayState::new())
    );

    let app_state = Arc::new(AppState {
        settings_manager: SettingsManager::new(),
        champions: Champions::new(),
        lcu_monitor: LcuMonitor::new(),
        last_action: tokio::sync::Mutex::new(String::new()),
        current_queue_mode: tokio::sync::Mutex::new(None),
        overlay_state: overlay_state.clone(),
        sync_client: tokio::sync::Mutex::new(None),
        accept_in_flight: std::sync::atomic::AtomicBool::new(false),
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
            get_champion_options,
            is_lcu_connected,
            launch_league,
            can_launch_league,
            get_autostart,
            set_autostart,
            get_overlay_data,
            save_overlay_position,
            send_timer_event,
            cancel_timer_event,
            appearance::save_background,
            appearance::get_background,
            appearance::clear_background,
        ])
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                if window.label() == "main" {
                    let app = window.app_handle();
                    let to_tray = app
                        .state::<Arc<AppState>>()
                        .settings_manager
                        .get()
                        .minimize_to_tray;
                    if to_tray {
                        // Keep running in the tray instead of exiting.
                        api.prevent_close();
                        let _ = window.hide();
                    } else {
                        // Close overlay when main window is closed
                        close_overlay_window(app);
                    }
                }
            }
        })
        .setup(move |app| {
            let app_handle = app.handle().clone();
            let state = app_state.clone();

            setup_tray(app.handle())?;

            // Restore the always-on-top preference before the window is shown.
            {
                let s = app.state::<Arc<AppState>>().settings_manager.get();
                if let Some(main_win) = app.get_webview_window("main") {
                    let _ = main_win.set_always_on_top(s.always_on_top);
                    apply_native_rounding(&main_win);
                }
            }

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
                state.champions.load_with_retry().await;

                // Emit champions loaded event
                let _ = app_handle.emit("champions-loaded", ());

                // Start LCU monitor
                state.lcu_monitor.start();
                let mut event_rx = state.lcu_monitor.event_tx.subscribe();

                // Listen for LCU events and react.
                //
                // A broadcast receiver reports Err(Lagged) when it falls behind
                // rather than yielding an event. Treating that as termination
                // would silently kill every reaction (pick, ban, overlay) for
                // the rest of the session, so only Closed ends the loop.
                loop {
                    let event = match event_rx.recv().await {
                        Ok(event) => event,
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            log::warn!("LCU event receiver lagged, {} events dropped", n);
                            continue;
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    };

                    match event {
                        LcuEvent::Connected => {
                            let _ = app_handle.emit("lcu-status", true);
                            let _ = app_handle.emit("log", "LCU connectat");
                        }
                        LcuEvent::Disconnected => {
                            let _ = app_handle.emit("lcu-status", false);
                            let _ = app_handle.emit("log", "LCU desconnectat");
                            // Reset dedup guards
                            *state.last_action.lock().await = String::new();
                            state
                                .accept_in_flight
                                .store(false, std::sync::atomic::Ordering::SeqCst);
                            // Clear queue mode so the UI indicator disappears
                            *state.current_queue_mode.lock().await = None;
                            let _ = app_handle.emit("queue-mode", serde_json::Value::Null);
                        }
                        LcuEvent::ReadyCheck(data) => {
                            // Spawned: the handler sleeps for the configured
                            // accept delay and must not stall event reception.
                            let state = state.clone();
                            let app_handle = app_handle.clone();
                            tauri::async_runtime::spawn(async move {
                                handle_ready_check(&state, &app_handle, &data).await;
                            });
                        }
                        LcuEvent::ChampSelect(data) => {
                            // Extract enemy data for overlay
                            let id_to_name = state.champions.get_id_to_name();
                            let enemies = overlay::extract_enemies(&data, &id_to_name, &state.overlay_state.spells);
                            if !enemies.is_empty() {
                                *state.overlay_state.enemies.lock().await = enemies;
                            }

                            // Spawned: the handler sleeps for the configured
                            // pick/ban delays. `last_action` is written inside
                            // its mutex before any await, so overlapping
                            // invocations still dedupe correctly.
                            let state = state.clone();
                            let app_handle = app_handle.clone();
                            tauri::async_runtime::spawn(async move {
                                handle_champ_select(&state, &app_handle, &data).await;
                            });
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

/// The sync room key. Nothing ever wrote `overlay_state.game_id`, so ask the
/// LCU for the real one and cache it for the rest of the game.
async fn resolve_game_id(state: &Arc<AppState>) -> Option<String> {
    if let Some(cached) = state.overlay_state.game_id.lock().await.clone() {
        return Some(cached);
    }

    let creds = state.lcu_monitor.get_credentials().await?;
    let session = lcu::lcu_request(&creds, "GET", "/lol-gameflow/v1/session", None)
        .await
        .ok()?;
    let id = session["gameData"]["gameId"].as_i64().filter(|id| *id > 0)?;

    let id = id.to_string();
    *state.overlay_state.game_id.lock().await = Some(id.clone());
    log::info!("Resolved game id {}", id);
    Some(id)
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
                        for e in &enemies {
                            log::info!("Overlay emit: {} spell1={}({}) spell2={}({})",
                                e.champion_name, e.spell1_name, e.spell1_icon, e.spell2_name, e.spell2_icon);
                        }
                        let _ = ah.emit("overlay-data", &enemies);
                    });
                }

                // Connect to sync server if enabled
                if settings.sync_enabled && !settings.sync_server_url.is_empty() {
                    // The room is keyed on the game id, so a missing one would
                    // put every player of every game in one shared room and
                    // cross-feed their timers. Skip syncing rather than that.
                    let game_id = resolve_game_id(state).await;
                    if game_id.is_none() {
                        log::warn!("No game id available, skipping sync connect");
                        let _ = app_handle
                            .emit("log", "Sync omes: no s'ha pogut identificar la partida");
                    }
                    if let Some(game_id) = game_id {
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
                                loop {
                                    match rx.recv().await {
                                        Ok(msg) => {
                                            let _ = ah.emit("sync-message", &msg);
                                        }
                                        // Same trap as the LCU loop: Lagged is
                                        // not the end of the stream.
                                        Err(tokio::sync::broadcast::error::RecvError::Lagged(
                                            n,
                                        )) => {
                                            log::warn!("Sync forwarder lagged, {} dropped", n);
                                        }
                                        Err(
                                            tokio::sync::broadcast::error::RecvError::Closed,
                                        ) => break,
                                    }
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
        }
        "EndOfGame" | "Lobby" | "None" | "WaitingForStats" => {
            // Destroy overlay window
            if app_handle.get_webview_window("overlay").is_some() {
                close_overlay_window(app_handle);
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

    if phase != "ReadyCheck" {
        state
            .accept_in_flight
            .store(false, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Bumped every time the overlay is closed. The Win32 watchdog thread below
/// captures the value it was born with and exits as soon as it changes, so a
/// retired thread can never poke a destroyed (or recycled) HWND.
#[cfg(windows)]
static OVERLAY_GENERATION: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// Close the overlay window and retire its watchdog thread.
fn close_overlay_window(app_handle: &tauri::AppHandle) {
    #[cfg(windows)]
    OVERLAY_GENERATION.fetch_add(1, std::sync::atomic::Ordering::SeqCst);

    if let Some(overlay_win) = app_handle.get_webview_window("overlay") {
        let _ = overlay_win.close();
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
    .inner_size(230.0, 310.0)
    .transparent(true)
    .shadow(false)
    .decorations(false)
    .always_on_top(true)
    .skip_taskbar(true)
    .resizable(false)
    .focused(false);

    builder = builder.center();

    let window = builder.build().map_err(|e| e.to_string())?;

    // The saved coordinates come from outerPosition(), which is physical, but
    // WebviewWindowBuilder::position takes logical pixels - restoring them
    // there made the overlay drift on scaled displays.
    if let (Some(x), Some(y)) = (settings.overlay_x, settings.overlay_y) {
        let _ = window.set_position(tauri::PhysicalPosition::new(x, y));
    }
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

        let generation = OVERLAY_GENERATION.load(std::sync::atomic::Ordering::SeqCst);

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

                    // Stop as soon as this overlay is retired, otherwise one
                    // thread would leak per game and keep driving a dead HWND.
                    if OVERLAY_GENERATION.load(std::sync::atomic::Ordering::SeqCst) != generation {
                        log::debug!("Overlay watchdog {} exiting", generation);
                        break;
                    }

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

    if !is_in_progress {
        // The ready check ended (accepted, declined or cancelled) - re-arm.
        state
            .accept_in_flight
            .store(false, std::sync::atomic::Ordering::SeqCst);
        return;
    }

    if no_response {
        // Claim this ready check; a second event for the same one is a no-op.
        if state
            .accept_in_flight
            .swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            return;
        }

        // Capture the foreground window BEFORE the delay. LoL grabs focus when
        // the ready check pops, but the user has usually clicked back to
        // whatever they were doing by the time this event reaches us; capturing
        // after the sleep would just record LoL's own window and restoring it
        // would do the opposite of what the setting promises.
        let captured_hwnd = if settings.restore_focus_after_action {
            focus::capture_foreground()
        } else {
            None
        };

        // Delay before accepting (capped so we always act before timer expires)
        let accept_delay = settings
            .accept_delay_secs
            .min(12.0 - settings.action_margin_secs)
            .max(0.0);
        if accept_delay > 0.0 {
            tokio::time::sleep(std::time::Duration::from_secs_f64(accept_delay)).await;
        }

        if let Some(creds) = state.lcu_monitor.get_credentials().await {
            match actions::accept_match(&creds).await {
                Ok(_) => {
                    let _ = app_handle.emit("log", "Partida acceptada!");
                    spawn_focus_restore(captured_hwnd);
                }
                Err(e) => {
                    log::error!("Accept failed: {}", e);
                    let _ = app_handle.emit("log", &format!("Error acceptant: {}", e));
                    // Let the next ready-check event retry.
                    state
                        .accept_in_flight
                        .store(false, std::sync::atomic::Ordering::SeqCst);
                }
            }
        } else {
            state
                .accept_in_flight
                .store(false, std::sync::atomic::Ordering::SeqCst);
        }
    }
}

/// Clamp a configured delay so we always act before the phase timer expires.
fn clamp_delay(delay_secs: f64, timer_left: f64, margin: f64) -> f64 {
    delay_secs.min(timer_left - margin).max(0.0)
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
    // The LCU reports phase timers in milliseconds; every delay below is in
    // seconds, and clamp_delay compares the two.
    let timer_left = session["timer"]["adjustedTimeLeftInPhase"]
        .as_f64()
        .unwrap_or(0.0)
        / 1000.0;
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

    // Which ban/pick phases exist is decided by the LCU, not by us: if it hands
    // us an action for our cell we act on it, whatever the mode.
    let mode_label = mode.as_deref().unwrap_or("unknown");
    log::info!(
        "Champ select action: mode={} type={} action_id={} cell={} timer_left={:.1}s auto_pick={} auto_ban={} bravery={} hover_pick={}",
        mode_label,
        action_type,
        action_id,
        my_cell_id,
        timer_left,
        settings.auto_pick,
        settings.auto_ban,
        settings.bravery_enabled,
        settings.hover_pick
    );

    // Capture focus before any action
    let captured_hwnd = if settings.restore_focus_after_action {
        focus::capture_foreground()
    } else {
        None
    };

    match action_type {
        "ban" if settings.auto_ban && !settings.ban_champion.is_empty() => {
            let delay = clamp_delay(
                settings.ban_delay_secs,
                timer_left,
                settings.action_margin_secs,
            );
            if delay > 0.0 {
                tokio::time::sleep(Duration::from_secs_f64(delay)).await;
            }
            if let Some(champ_id) = state.champions.resolve_id(&settings.ban_champion) {
                match actions::ban_champion(&creds, action_id, champ_id).await {
                    Ok(_) => {
                        log::info!(
                            "Banned {} (id {}) in {} after {:.1}s",
                            settings.ban_champion,
                            champ_id,
                            mode_label,
                            delay
                        );
                        let _ = app_handle
                            .emit("log", &format!("Banned {}!", settings.ban_champion));
                        spawn_focus_restore(captured_hwnd);
                    }
                    Err(e) => {
                        log::error!(
                            "Ban failed: {} (id {}) action {} in {}: {}",
                            settings.ban_champion,
                            champ_id,
                            action_id,
                            mode_label,
                            e
                        );
                        let _ = app_handle.emit("log", &format!("Error banning: {}", e));
                        *state.last_action.lock().await = String::new();
                    }
                }
            } else {
                log::warn!("Champion '{}' not found for ban", settings.ban_champion);
                let _ = app_handle.emit(
                    "log",
                    &format!("Champion '{}' no trobat per ban", settings.ban_champion),
                );
                *state.last_action.lock().await = String::new();
            }
        }
        "pick" if settings.auto_pick => {
            // Bravery is Arena-only. Every other mode uses the configured
            // champion, so the two settings are independent.
            let use_bravery = settings.bravery_enabled && mode.as_deref() == Some("CHERRY");
            let delay = clamp_delay(
                settings.pick_delay_secs,
                timer_left,
                settings.action_margin_secs,
            );

            if use_bravery {
                if delay > 0.0 {
                    tokio::time::sleep(Duration::from_secs_f64(delay)).await;
                }
                match actions::pick_bravery(&creds, action_id).await {
                    Ok(_) => {
                        log::info!("Bravery picked on action {} after {:.1}s", action_id, delay);
                        let _ = app_handle.emit("log", "Bravery activada!");
                        spawn_focus_restore(captured_hwnd);
                    }
                    Err(e) => {
                        log::error!("Bravery failed on action {}: {}", action_id, e);
                        let _ = app_handle.emit("log", &format!("Error Bravery: {}", e));
                        *state.last_action.lock().await = String::new();
                    }
                }
            } else if settings.pick_champion.is_empty() {
                log::warn!(
                    "No pick champion configured for mode {} (bravery only applies to Arena)",
                    mode_label
                );
                let _ = app_handle.emit("log", "Cap campio configurat per al pick");
                *state.last_action.lock().await = String::new();
            } else if let Some(champ_id) = state.champions.resolve_id(&settings.pick_champion) {
                let result = if settings.hover_pick {
                    // Hover immediately so teammates see the pick, commit later.
                    if let Err(e) = actions::hover_champion(&creds, action_id, champ_id).await {
                        log::warn!("Hover failed for {}: {}", settings.pick_champion, e);
                        let _ = app_handle.emit("log", &format!("Error hovering: {}", e));
                    } else {
                        let _ = app_handle
                            .emit("log", &format!("Hovering {}...", settings.pick_champion));
                    }
                    // Never commit sooner than HOVER_LOCK_GAP after the hover:
                    // back-to-back PATCHes on the same action get rejected.
                    let gap = Duration::from_secs_f64(delay).max(actions::HOVER_LOCK_GAP);
                    tokio::time::sleep(gap).await;
                    actions::lock_champion(&creds, action_id, champ_id).await
                } else {
                    if delay > 0.0 {
                        tokio::time::sleep(Duration::from_secs_f64(delay)).await;
                    }
                    actions::pick_champion(&creds, action_id, champ_id).await
                };

                match result {
                    Ok(_) => {
                        log::info!(
                            "Picked {} (id {}) in {} after {:.1}s (hover_pick={})",
                            settings.pick_champion,
                            champ_id,
                            mode_label,
                            delay,
                            settings.hover_pick
                        );
                        let _ = app_handle
                            .emit("log", &format!("Picked {}!", settings.pick_champion));
                        spawn_focus_restore(captured_hwnd);
                    }
                    Err(e) => {
                        log::error!(
                            "Pick failed: {} (id {}) action {} in {}: {}",
                            settings.pick_champion,
                            champ_id,
                            action_id,
                            mode_label,
                            e
                        );
                        let _ = app_handle.emit("log", &format!("Error picking: {}", e));
                        *state.last_action.lock().await = String::new();
                    }
                }
            } else {
                log::warn!("Champion '{}' not found for pick", settings.pick_champion);
                let _ = app_handle.emit(
                    "log",
                    &format!("Champion '{}' no trobat per pick", settings.pick_champion),
                );
                *state.last_action.lock().await = String::new();
            }
        }
        _ => {
            // Reset dedup if we didn't act (e.g. feature disabled)
            log::info!(
                "Ignoring {} action {} in {} (auto_pick={}, auto_ban={})",
                action_type,
                action_id,
                mode_label,
                settings.auto_pick,
                settings.auto_ban
            );
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
