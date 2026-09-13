// Prevents additional console window on Windows in release
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod actions;
mod champions;
mod focus;
mod hotkey;
mod http;
mod launcher;
mod lcu;
mod lol_state;
mod overlay;
mod queue;
mod queues;
mod selection;
mod settings;
mod summoners;
mod sync;

use champions::Champions;
use lcu::{LcuEvent, LcuMonitor, QueueModeInfo};
use lol_state::LolState;
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
    /// Last declared pick intent. Planning re-emits the session constantly, so
    /// without this every event would fire another hover PATCH.
    last_intent: tokio::sync::Mutex<String>,
    current_queue_mode: tokio::sync::Mutex<Option<QueueModeInfo>>,
    /// The client.s own queue table: queue id -> mode, map and display name.
    queues: Arc<queues::QueueTable>,
    /// puuid -> Riot ID. The lobby resource stopped carrying names.
    summoners: Arc<summoners::SummonerNames>,
    overlay_state: Arc<OverlayState>,
    /// Typed snapshot of the client: lobby, champ select, availability.
    lol_state: Arc<LolState>,
    sync_client: tokio::sync::Mutex<Option<Arc<sync::SyncClient>>>,
    /// Retires a pending auto-queue timer. Bumped on every re-evaluation, so a
    /// timer armed under an older verdict finds its generation stale and exits.
    auto_queue_generation: std::sync::atomic::AtomicUsize,
    /// Last auto-queue verdict, so a change is reported once rather than on
    /// each of the two Update events the client emits per lobby change.
    last_queue_gate: tokio::sync::Mutex<String>,
    /// What selection last decided, and what it passed over. Feeds the info
    /// panel, which is what turns pick/ban from a black box into something
    /// debuggable.
    last_decision: tokio::sync::Mutex<Option<serde_json::Value>>,
    /// Set when the user leaves the queue by hand, cleared when the lobby they
    /// cancelled in stops being the lobby they are in. Without it, auto queue
    /// re-queues about two seconds after every cancel and the search cannot be
    /// left at all.
    queue_cancelled_for: tokio::sync::Mutex<Option<String>>,
    /// Whether we were in the queue at the last event, so leaving it can be
    /// told apart from never having been in it.
    was_searching: std::sync::atomic::AtomicBool,
    /// Throttle for the info-panel payload: champ select re-emits on every
    /// hover and every timer tick.
    last_state_emit: std::sync::Mutex<Option<std::time::Instant>>,
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

    // Auto queue must react to its own switch. A lobby that is already sitting
    // full produces no further events, so without this, turning the setting on
    // would do nothing until someone joined or left.
    if settings.auto_queue != old.auto_queue
        || settings.auto_queue_trigger != old.auto_queue_trigger
        || settings.auto_queue_min_members != old.auto_queue_min_members
    {
        let st = (*state).clone();
        let ah = app.clone();
        tauri::async_runtime::spawn(async move {
            // The verdict is reported on change, so clear it or the first
            // evaluation under the new settings stays silent.
            *st.last_queue_gate.lock().await = String::new();
            // Flipping the switch is the way back in after a cancel, without
            // having to change the lobby to prove you mean it.
            *st.queue_cancelled_for.lock().await = None;
            evaluate_auto_queue(&st, &ah).await;
            emit_state(&st, &ah, true).await;
        });
    }

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


/// The info panel, on demand. The panel is event-fed, but the frontend can
/// load between two events, so it asks once at startup rather than showing its
/// empty state over a live lobby.
#[tauri::command]
async fn get_client_state(
    state: tauri::State<'_, Arc<AppState>>,
) -> Result<serde_json::Value, String> {
    Ok(build_client_state(&state).await)
}

/// Start matchmaking by hand, from the info panel.
///
/// Deliberately separate from the auto-queue gate: pressing the button is an
/// explicit instruction, so it is not second-guessed. The client refuses what
/// it must refuse and the error comes back to the UI.
#[tauri::command]
async fn start_queue(state: tauri::State<'_, Arc<AppState>>) -> Result<(), String> {
    // Pressing this is the clearest possible statement that the user does want
    // to be in the queue, so it lifts any suppression a previous cancel left.
    *state.queue_cancelled_for.lock().await = None;

    let creds = state
        .lcu_monitor
        .get_credentials()
        .await
        .ok_or("El client de League no està connectat")?;
    actions::start_matchmaking(&creds).await
}

/// Leave the queue. Also retires any pending auto-queue timer, so a cancel is
/// not undone a second later by a verdict taken before the click.
#[tauri::command]
async fn cancel_queue(state: tauri::State<'_, Arc<AppState>>) -> Result<(), String> {
    // Retiring the pending timer is not enough: the DELETE itself produces a
    // Search push, which re-evaluates the gate, which finds a full lobby and
    // queues again. Mark the lobby so nothing re-arms until it changes.
    let inner: Arc<AppState> = (*state).clone();
    suppress_auto_queue(&inner, "has cancel·lat la cerca").await;
    let creds = state
        .lcu_monitor
        .get_credentials()
        .await
        .ok_or("El client de League no està connectat")?;
    actions::cancel_matchmaking(&creds).await
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
        if let Err(e) = c
            .send(instalock_shared::SyncMessage::TimerStart {
                enemy_idx,
                spell_idx,
                cooldown_secs,
                started_at,
            })
            .await
        {
            // The timer still runs locally; it just didn't reach the team.
            log::warn!("Timer not broadcast: {}", e);
        }
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
        if let Err(e) = c
            .send(instalock_shared::SyncMessage::TimerCancel {
                enemy_idx,
                spell_idx,
            })
            .await
        {
            log::warn!("Timer cancel not broadcast: {}", e);
        }
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

    // A read-only %LOCALAPPDATA%, a roaming profile that hasn't synced, or an
    // antivirus holding the file open used to be fatal here — before any window
    // existed, so the app just never appeared and left nothing to explain why.
    // Losing the log file is worth a degraded run, not a crash.
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path);

    let mut builder = env_logger::Builder::new();
    builder
        .filter_level(log::LevelFilter::Debug)
        .format(move |buf, record| {
            writeln!(
                buf,
                "[{}] {} - {}",
                chrono::Local::now().format("%Y-%m-%d %H:%M:%S"),
                record.level(),
                record.args()
            )
        });

    match file {
        Ok(file) => {
            builder.target(env_logger::Target::Pipe(Box::new(file)));
            builder.init();
            log::info!("=== InstaLock started ===");
            log::info!("Log file: {}", log_path.display());
        }
        Err(e) => {
            // Stderr goes nowhere in a windows_subsystem build, but the app runs.
            builder.init();
            log::warn!("=== InstaLock started (no log file) ===");
            log::warn!("Cannot open {}: {}", log_path.display(), e);
        }
    }
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

    // Cheap and synchronous. The spell table is fetched later, on the app's own
    // runtime, by the setup task below.
    let overlay_state = Arc::new(OverlayState::new());

    let app_state = Arc::new(AppState {
        settings_manager: SettingsManager::new(),
        champions: Champions::new(),
        lcu_monitor: LcuMonitor::new(),
        last_action: tokio::sync::Mutex::new(String::new()),
        last_intent: tokio::sync::Mutex::new(String::new()),
        current_queue_mode: tokio::sync::Mutex::new(None),
        queues: Arc::new(queues::QueueTable::new()),
        summoners: Arc::new(summoners::SummonerNames::new()),
        overlay_state: overlay_state.clone(),
        lol_state: Arc::new(LolState::new()),
        sync_client: tokio::sync::Mutex::new(None),
        auto_queue_generation: std::sync::atomic::AtomicUsize::new(0),
        last_queue_gate: tokio::sync::Mutex::new(String::new()),
        last_decision: tokio::sync::Mutex::new(None),
        queue_cancelled_for: tokio::sync::Mutex::new(None),
        was_searching: std::sync::atomic::AtomicBool::new(false),
        last_state_emit: std::sync::Mutex::new(None),
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
            get_client_state,
            start_queue,
            cancel_queue,
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

            // The summoner spell table. Independent of everything else, so it
            // gets its own task rather than delaying the LCU monitor.
            let spell_state = app_state.clone();
            tauri::async_runtime::spawn(async move {
                spell_state.overlay_state.load_spells().await;
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
                            // Once per client session: names every queue and,
                            // more importantly, maps a queue id back to its
                            // mode when the lobby is gone.
                            if let Some(creds) = state.lcu_monitor.get_credentials().await {
                                state.queues.load(&creds).await;
                            }
                            emit_state(&state, &app_handle, true).await;
                        }
                        LcuEvent::Disconnected => {
                            let _ = app_handle.emit("lcu-status", false);
                            let _ = app_handle.emit("log", "LCU desconnectat");
                            // Reset dedup guards
                            *state.last_action.lock().await = String::new();
                            *state.last_intent.lock().await = String::new();
                            state
                                .accept_in_flight
                                .store(false, std::sync::atomic::Ordering::SeqCst);
                            // Clear queue mode so the UI indicator disappears
                            *state.current_queue_mode.lock().await = None;
                            let _ = app_handle.emit("queue-mode", serde_json::Value::Null);
                            state.lol_state.set_lobby(None).await;
                            state.lol_state.clear_champ_select().await;
                            emit_mode_context(&state, &app_handle).await;
                            *state.last_queue_gate.lock().await = String::new();
                            emit_state(&state, &app_handle, true).await;
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
                            // Keep the typed snapshot current before anything
                            // reacts to it: selection and the info panel both
                            // read from there, not from this payload.
                            if let Some(session) =
                                lol_state::parse::<lol_state::ChampSelect>("champ select", &data)
                            {
                                let role_changed = state
                                    .lol_state
                                    .get()
                                    .await
                                    .champ_select
                                    .as_ref()
                                    .map(|old| old.assigned_position() != session.assigned_position())
                                    .unwrap_or(true);
                                state.lol_state.set_champ_select(Some(session)).await;
                                // Only on a real change: the session re-emits on
                                // every hover and every timer tick.
                                if role_changed {
                                    emit_mode_context(&state, &app_handle).await;
                                }
                                // The lobby is deleted when champ select
                                // starts, so this is where the mode has to be
                                // recovered from the session queue id.
                                refresh_queue_mode(&state, &app_handle).await;
                            }

                            // The availability tables are pushed over the WS, but
                            // only when they change — entering champ select after
                            // they were last emitted would leave us blind, so the
                            // first session event of a draft fetches them once.
                            if !state.lol_state.get().await.has_grid() {
                                let state = state.clone();
                                tauri::async_runtime::spawn(async move {
                                    fetch_champ_select_statics(&state).await;
                                });
                            }

                            // Extract enemy data for overlay
                            let id_to_name = state.champions.get_id_to_name();
                            let spells = state.overlay_state.spells.read().await;
                            let enemies = overlay::extract_enemies(&data, &id_to_name, &spells);
                            drop(spells);
                            if !enemies.is_empty() {
                                *state.overlay_state.enemies.lock().await = enemies;
                            }
                            emit_state(&state, &app_handle, false).await;


                            // Spawned: the handler sleeps for the configured
                            // pick/ban delays. `last_action` is written inside
                            // its mutex before any await, so overlapping
                            // invocations still dedupe correctly.
                            let state = state.clone();
                            let app_handle = app_handle.clone();
                            tauri::async_runtime::spawn(async move {
                                handle_champ_select(&state, &app_handle).await;
                            });
                        }
                        LcuEvent::ChampSelectEnded => {
                            state.lol_state.clear_champ_select().await;
                            *state.last_intent.lock().await = String::new();
                            refresh_queue_mode(&state, &app_handle).await;
                            // Back to "no game in progress": every role list is
                            // reachable again for configuring.
                            emit_mode_context(&state, &app_handle).await;
                            emit_state(&state, &app_handle, true).await;
                        }
                        LcuEvent::GameflowPhase(phase) => {
                            state.lol_state.set_phase(&phase).await;
                            handle_gameflow_phase(&state, &app_handle, &phase).await;
                            note_search_state(&state).await;
                            evaluate_auto_queue(&state, &app_handle).await;
                            emit_state(&state, &app_handle, true).await;
                        }
                        LcuEvent::Lobby(data) => {
                            let lobby = data
                                .as_ref()
                                .and_then(|d| lol_state::parse::<lol_state::Lobby>("lobby", d));
                            state.lol_state.set_lobby(lobby).await;
                            // The lobby is where we learn whether this queue has
                            // lanes at all, so the UI can drop the role picker.
                            refresh_queue_mode(&state, &app_handle).await;
                            emit_mode_context(&state, &app_handle).await;
                            resolve_lobby_names(&state, &app_handle);
                            evaluate_auto_queue(&state, &app_handle).await;
                            emit_state(&state, &app_handle, true).await;
                        }
                        LcuEvent::Search(data) => {
                            let search = data
                                .as_ref()
                                .and_then(|d| lol_state::parse::<lol_state::Search>("search", d));
                            state.lol_state.set_search(search).await;
                            note_search_state(&state).await;
                            evaluate_auto_queue(&state, &app_handle).await;
                            emit_state(&state, &app_handle, false).await;
                        }
                        LcuEvent::PickableChampions(ids) => {
                            state.lol_state.set_pickable(ids.into_iter().collect()).await;
                        }
                        LcuEvent::BannableChampions(ids) => {
                            state.lol_state.set_bannable(ids.into_iter().collect()).await;
                        }
                        LcuEvent::GridChampion(data) => {
                            if let Some(champ) =
                                lol_state::parse::<lol_state::GridChampion>("grid champion", &data)
                            {
                                state
                                    .lol_state
                                    .update_grid_champion(champ.id, champ.selection_status)
                                    .await;
                            }
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

/// Pull the champ-select tables that arrive by push but are not re-sent on
/// entry: the pickable and bannable id sets, and the champion grid.
///
/// Without this, joining a draft after the client last emitted them leaves
/// selection with no idea what is available, and every candidate looks fine.
async fn fetch_champ_select_statics(state: &Arc<AppState>) {
    let Some(creds) = state.lcu_monitor.get_credentials().await else {
        return;
    };

    if let Ok(data) = lcu::lcu_request(
        &creds,
        "GET",
        "/lol-champ-select/v1/pickable-champion-ids",
        None,
    )
    .await
    {
        if let Some(ids) = data.as_array() {
            let ids: std::collections::HashSet<i32> =
                ids.iter().filter_map(|v| v.as_i64()).map(|v| v as i32).collect();
            log::info!("{} pickable champions", ids.len());
            state.lol_state.set_pickable(ids).await;
        }
    }

    if let Ok(data) = lcu::lcu_request(
        &creds,
        "GET",
        "/lol-champ-select/v1/bannable-champion-ids",
        None,
    )
    .await
    {
        if let Some(ids) = data.as_array() {
            let ids: std::collections::HashSet<i32> =
                ids.iter().filter_map(|v| v.as_i64()).map(|v| v as i32).collect();
            log::info!("{} bannable champions", ids.len());
            state.lol_state.set_bannable(ids).await;
        }
    }

    // The grid is what carries live hover and ban state per champion; the id
    // sets above are only the server's static rules.
    if let Ok(data) =
        lcu::lcu_request(&creds, "GET", "/lol-champ-select/v1/all-grid-champions", None).await
    {
        if let Some(entries) = data.as_array() {
            let mut grid = std::collections::HashMap::new();
            for entry in entries {
                if let Some(champ) =
                    lol_state::parse::<lol_state::GridChampion>("grid champion", entry)
                {
                    grid.insert(champ.id, champ.selection_status);
                }
            }
            log::info!("Champion grid loaded: {} entries", grid.len());
            state.lol_state.set_grid(grid).await;
        }
    }
}

/// Join the relay room for this game and wire its two streams into the app:
/// timers out to the overlay, connection status out to the log.
async fn connect_sync(state: &Arc<AppState>, app_handle: &tauri::AppHandle, settings: &Settings) {
    // The room is keyed on the game id, so a missing one would put every player
    // of every game in one shared room and cross-feed their timers. Skip
    // syncing rather than that.
    let Some(game_id) = resolve_game_id(state).await else {
        log::warn!("No game id available, skipping sync connect");
        let _ = app_handle.emit("log", "Sync omes: no s'ha pogut identificar la partida");
        return;
    };

    let client = sync::SyncClient::start(&settings.sync_server_url, &game_id, "player");

    // Forward incoming sync messages to the overlay
    let mut rx = client.incoming_tx.subscribe();
    let ah = app_handle.clone();
    tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(msg) => {
                    let _ = ah.emit("sync-message", &msg);
                }
                // Same trap as the LCU loop: Lagged is not the end of the stream.
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    log::warn!("Sync forwarder lagged, {} dropped", n);
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });

    // Report the connection honestly. "Sync connectat" is now emitted when the
    // socket is actually up, not when we decided to try.
    let mut status = client.status_tx.subscribe();
    let ah = app_handle.clone();
    tokio::spawn(async move {
        loop {
            let line = match status.recv().await {
                Ok(sync::SyncStatus::Connected) => "Sync connectat".to_string(),
                Ok(sync::SyncStatus::Lost(why)) => format!("Error de sync: {}", why),
                Ok(sync::SyncStatus::Retrying { in_secs }) => {
                    format!("Reconnectant sync en {}s...", in_secs)
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            };
            let _ = ah.emit("log", &line);
        }
    });

    *state.sync_client.lock().await = Some(client);
}

async fn handle_gameflow_phase(state: &Arc<AppState>, app_handle: &tauri::AppHandle, phase: &str) {
    match phase {
        "ChampSelect" => {
            // Reset is handled by GameflowPhase != ChampSelect below
        }
        "InProgress" => {
            // The game is real now, so one-shot list entries are spent. Tied to
            // InProgress and not to the pick itself: a dodged champ select never
            // became a game, and burning the one-shot there would leave the user
            // disarmed for a re-queue they did not ask for.
            spend_one_shot_entries(state, app_handle).await;

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
            }

            // Sync, deliberately outside the overlay branch. It used to be
            // nested inside it, so with the overlay off — which is now the
            // default — turning sync on did nothing at all and said nothing.
            if settings.sync_enabled && !settings.sync_server_url.is_empty() {
                connect_sync(state, app_handle, &settings).await;
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
        // Reset both dedup guards when leaving champ select. `last_intent` keys
        // on the action id, and the client restarts those from small integers
        // every draft - two games in a row with the same role and the same
        // first choice would otherwise reuse the key and skip the second
        // pre-turn hover entirely.
        *state.last_action.lock().await = String::new();
        *state.last_intent.lock().await = String::new();
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

/// Hover a champion on a pick action that is not ours yet, so the rest of the
/// team can see what we intend to play.
///
/// Only the championId is sent — no `completed` — which is what turns the same
/// PATCH into an intent rather than a lock.
async fn declare_pick_intent(
    state: &Arc<AppState>,
    app_handle: &tauri::AppHandle,
    session: &lol_state::ChampSelect,
    snapshot: &lol_state::Snapshot,
    settings: &Settings,
    mode: Option<&str>,
) {
    let Some(action) = session.my_first_unfinished_pick() else {
        return;
    };

    let list = settings.pick_list_for(mode, session.assigned_position());
    let Ok(choice) = selection::choose(
        selection::Intent::Pick,
        &list,
        snapshot,
        |name| state.champions.resolve_id(name),
        settings.avoid_ally_hover,
        mode == Some(settings::ARENA_MODE),
    ) else {
        return;
    };

    // Never declare Bravery as an intent: -3 is a commit sentinel, not a
    // champion the client can show as a hover.
    if choice.champion_id == settings::BRAVERY_ID {
        return;
    }

    // Nothing to declare once the hover is already what we want.
    if action.champion_id == choice.champion_id {
        return;
    }

    // Dedup on the champion, not just the action: the session re-emits
    // constantly during planning and each event would fire another PATCH.
    let key = format!("intent:{}:{}", action.id, choice.champion_id);
    {
        let mut last = state.last_intent.lock().await;
        if *last == key {
            return;
        }
        *last = key;
    }

    let Some(creds) = state.lcu_monitor.get_credentials().await else {
        return;
    };

    match actions::hover_champion(&creds, action.id, choice.champion_id).await {
        Ok(_) => {
            log::info!(
                "Declared pick intent {} (id {}) on action {}",
                choice.name,
                choice.champion_id,
                action.id
            );
            let _ = app_handle.emit("log", &format!("Intenció: {}", choice.name));
        }
        Err(e) => {
            // Not worth surfacing: declaring intent early is a courtesy, and
            // the client refuses it in plenty of harmless situations.
            log::debug!("Pick intent failed on action {}: {}", action.id, e);
            *state.last_intent.lock().await = String::new();
        }
    }
}

async fn handle_champ_select(state: &Arc<AppState>, app_handle: &tauri::AppHandle) {
    let settings = state.settings_manager.get();
    let Some(creds) = state.lcu_monitor.get_credentials().await else {
        return;
    };

    // Read the typed snapshot rather than the raw payload: it is written before
    // this handler is spawned, and it is what selection reasons about.
    let snapshot = state.lol_state.get().await;
    let Some(session) = snapshot.champ_select.clone() else {
        return;
    };

    if session.is_spectating {
        return;
    }

    // Resolve the current game mode. The lobby is authoritative while it lasts;
    // the session's own queueId covers joining a draft already in progress.
    let mode: Option<String> = {
        let current = state.current_queue_mode.lock().await.as_ref().map(|q| q.game_mode.clone());
        match current {
            Some(mode) => Some(mode),
            // The lobby is gone by now, so the session queue id is all there
            // is - and it is resolved through the client's own table, not a
            // hand-written match. That match knew 450, 1700 and 1710, so Arena
            // 3x6 (1750) and ARAM: Mayhem (2400) fell through to nothing and
            // the SoloQ list got read in an Arena draft.
            None => state
                .queues
                .get(session.queue_id)
                .await
                .map(|info| info.game_mode),
        }
    };

    // ARAM and its variants hand out champions from the bench: there is no
    // action of ours to take. Asked of the queue table by map id, because the
    // mode codename for this changes every patch.
    let has_champ_select = match state.queues.get(session.queue_id).await {
        Some(info) => info.has_champ_select(),
        None => mode.as_deref() != Some("ARAM"),
    };
    if !has_champ_select {
        return;
    }

    // The session only updates on events, so the raw timer is stale between
    // pushes; seconds_left takes that drift off.
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as f64)
        .unwrap_or(0.0);
    let timer_left = session.timer.seconds_left(now_ms);
    let timer_phase = session.timer.phase.as_str();

    // Planning is where intent is declared, never where anything is committed.
    if timer_phase == "PLANNING" {
        if settings.auto_pick && settings.hover_pick {
            declare_pick_intent(state, app_handle, &session, &snapshot, &settings, mode.as_deref())
                .await;
        }
        return;
    }

    if timer_phase == "GAME_STARTING" || timer_left <= 0.0 {
        return;
    }

    let Some(action) = session.my_open_action() else {
        return;
    };
    let action_type = action.action_type.clone();
    let action_id = action.id;

    // Dedup: don't repeat the same action
    let dedup_key = format!("{}:{}", action_type, session.local_player_cell_id);
    {
        let mut last = state.last_action.lock().await;
        if *last == dedup_key {
            return;
        }
        *last = dedup_key;
    }

    let is_arena = mode.as_deref() == Some(settings::ARENA_MODE);
    let role = session.assigned_position();
    let role_label = if role.is_empty() { "-" } else { role };
    let mode_label = mode.as_deref().unwrap_or("unknown");
    log::info!(
        "Champ select action: mode={} role={} type={} action_id={} cell={} timer_left={:.1}s pickable={} bannable={} auto_pick={} auto_ban={} hover_pick={}",
        mode_label,
        role_label,
        action_type,
        action_id,
        session.local_player_cell_id,
        timer_left,
        snapshot.pickable.len(),
        snapshot.bannable.len(),
        settings.auto_pick,
        settings.auto_ban,
        settings.hover_pick
    );

    // Capture focus before any action
    let captured_hwnd = if settings.restore_focus_after_action {
        focus::capture_foreground()
    } else {
        None
    };

    match action_type.as_str() {
        "ban" if settings.auto_ban => {
            let list = settings.ban_list_for(mode.as_deref(), role);
            let choice = match selection::choose(
                selection::Intent::Ban,
                &list,
                &snapshot,
                |name| state.champions.resolve_id(name),
                settings.avoid_ally_hover,
                is_arena,
            ) {
                Ok(choice) => choice,
                Err(exhausted) => {
                    // The client table said no to everything. It has been wrong
                    // - Arena did not list K'Sante as bannable - and a rejected
                    // PATCH costs less than never banning.
                    match selection::ignoring_availability(&list, &exhausted, |name| {
                        state.champions.resolve_id(name)
                    }) {
                        Some(choice) => {
                            log::warn!(
                                "Ban table did not list {}; banning it anyway",
                                choice.name
                            );
                            choice
                        }
                        None => {
                            report_exhausted(state, app_handle, "ban", &exhausted).await;
                            return;
                        }
                    }
                }
            };

            announce_fallback(app_handle, "Ban", &choice);
            record_decision(state, "Ban", Some(&choice), &choice.passed_over).await;

            let delay = clamp_delay(
                settings.ban_delay_secs,
                timer_left,
                settings.action_margin_secs,
            );
            if delay > 0.0 {
                tokio::time::sleep(Duration::from_secs_f64(delay)).await;
            }

            match actions::ban_champion(&creds, action_id, choice.champion_id).await {
                Ok(_) => {
                    log::info!(
                        "Banned {} (id {}) in {} after {:.1}s",
                        choice.name,
                        choice.champion_id,
                        mode_label,
                        delay
                    );
                    let _ = app_handle.emit("log", &format!("Banned {}!", choice.name));
                    remember_recent(state, app_handle, selection::Intent::Ban, &choice.name).await;
                    spawn_focus_restore(captured_hwnd);
                }
                Err(e) => {
                    log::error!(
                        "Ban failed: {} (id {}) action {} in {}: {}",
                        choice.name,
                        choice.champion_id,
                        action_id,
                        mode_label,
                        e
                    );
                    let _ = app_handle.emit("log", &format!("Error banning: {}", e));
                    // Re-arm so the next session event recomputes against a
                    // fresh snapshot and reaches for the next candidate. The
                    // error body is not inspected: no source documents what the
                    // client returns for an unavailable champion, so the
                    // recompute is the recovery.
                    *state.last_action.lock().await = String::new();
                }
            }
        }
        "pick" if settings.auto_pick => {
            let delay = clamp_delay(
                settings.pick_delay_secs,
                timer_left,
                settings.action_margin_secs,
            );

            let list = settings.pick_list_for(mode.as_deref(), role);
            let choice = match selection::choose(
                selection::Intent::Pick,
                &list,
                &snapshot,
                |name| state.champions.resolve_id(name),
                settings.avoid_ally_hover,
                is_arena,
            ) {
                Ok(choice) => choice,
                Err(exhausted) => {
                    report_exhausted(state, app_handle, "pick", &exhausted).await;
                    return;
                }
            };

            announce_fallback(app_handle, "Pick", &choice);
            record_decision(state, "Pick", Some(&choice), &choice.passed_over).await;

            // A Bravery entry is not a champion: the client wants a single
            // commit carrying the sentinel, and hovering it would be rejected.
            if choice.champion_id == settings::BRAVERY_ID {
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
                return;
            }

            let result = if settings.hover_pick {
                // Hover immediately so teammates see the pick, commit later.
                if let Err(e) = actions::hover_champion(&creds, action_id, choice.champion_id).await
                {
                    log::warn!("Hover failed for {}: {}", choice.name, e);
                    let _ = app_handle.emit("log", &format!("Error hovering: {}", e));
                } else {
                    let _ = app_handle.emit("log", &format!("Hovering {}...", choice.name));
                }
                // Never commit sooner than HOVER_LOCK_GAP after the hover:
                // back-to-back PATCHes on the same action get rejected.
                let gap = Duration::from_secs_f64(delay).max(actions::HOVER_LOCK_GAP);
                tokio::time::sleep(gap).await;
                actions::lock_champion(&creds, action_id, choice.champion_id).await
            } else {
                if delay > 0.0 {
                    tokio::time::sleep(Duration::from_secs_f64(delay)).await;
                }
                actions::pick_champion(&creds, action_id, choice.champion_id).await
            };

            match result {
                Ok(_) => {
                    log::info!(
                        "Picked {} (id {}) in {} as {} after {:.1}s (hover_pick={})",
                        choice.name,
                        choice.champion_id,
                        mode_label,
                        role_label,
                        delay,
                        settings.hover_pick
                    );
                    let _ = app_handle.emit("log", &format!("Picked {}!", choice.name));
                    remember_recent(state, app_handle, selection::Intent::Pick, &choice.name).await;
                    spawn_focus_restore(captured_hwnd);
                }
                Err(e) => {
                    log::error!(
                        "Pick failed: {} (id {}) action {} in {}: {}",
                        choice.name,
                        choice.champion_id,
                        action_id,
                        mode_label,
                        e
                    );
                    let _ = app_handle.emit("log", &format!("Error picking: {}", e));
                    *state.last_action.lock().await = String::new();
                }
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

/// Say which backup was taken and what it replaced. Silent when the first
/// choice went through, which is the common case.
fn announce_fallback(app_handle: &tauri::AppHandle, what: &str, choice: &selection::Choice) {
    if choice.passed_over.is_empty() {
        return;
    }
    let skipped = selection::describe(&choice.passed_over);
    log::info!("{} fell through to {}: skipped {}", what, choice.name, skipped);
    let _ = app_handle.emit(
        "log",
        &format!("{} alternatiu: {} (omesos: {})", what, choice.name, skipped),
    );
}

/// Nothing in the list was usable. Name every entry and why, rather than
/// leaving the user to work out why nothing happened — which is exactly what
/// the old single-champion code did.
async fn report_exhausted(
    state: &Arc<AppState>,
    app_handle: &tauri::AppHandle,
    what: &str,
    exhausted: &selection::Exhausted,
) {
    if exhausted.passed_over.is_empty() {
        log::warn!("No {} list configured", what);
        let _ = app_handle.emit("log", &format!("Cap campió configurat per al {}", what));
    } else {
        let skipped = selection::describe(&exhausted.passed_over);
        log::warn!("{} list exhausted: {}", what, skipped);
        let _ = app_handle.emit("log", &format!("Llista de {} esgotada: {}", what, skipped));
    }
    record_decision(state, what, None, &exhausted.passed_over).await;
    // Re-arm: a champion may free up before the timer runs out.
    *state.last_action.lock().await = String::new();
}


/// How often the info panel is refreshed at most.
///
/// Champ select re-emits its session on every hover and every timer tick, which
/// is several times a second; the panel does not need that.
const STATE_EMIT_INTERVAL: Duration = Duration::from_millis(400);

/// Build and push the payload behind the info panel.
///
/// `force` skips the throttle, for events that are rare and significant on
/// their own (a lobby change, a phase change) rather than the champ-select
/// firehose.
async fn emit_state(state: &Arc<AppState>, app_handle: &tauri::AppHandle, force: bool) {
    if !force {
        let mut last = state.last_state_emit.lock().unwrap();
        let now = std::time::Instant::now();
        match *last {
            Some(at) if now.duration_since(at) < STATE_EMIT_INTERVAL => return,
            _ => *last = Some(now),
        }
    } else {
        *state.last_state_emit.lock().unwrap() = Some(std::time::Instant::now());
    }

    let _ = app_handle.emit("client-state", build_client_state(state).await);
}

/// The info-panel payload. Split out from `emit_state` so the frontend can ask
/// for it on load: the panel is fed by events, and a tab opened between two of
/// them would otherwise sit on its empty state while a lobby is wide open.
async fn build_client_state(state: &Arc<AppState>) -> serde_json::Value {
    let snapshot = state.lol_state.get().await;
    let settings = state.settings_manager.get();
    let id_to_name = state.champions.get_id_to_name();
    let gate = queue::evaluate_with(&snapshot, &settings, cancel_suppressed(state).await);

    // Resolved up front: the payload is built in a synchronous closure, and the
    // cache read is async.
    let mut names: std::collections::HashMap<String, summoners::RiotId> =
        std::collections::HashMap::new();
    if let Some(lobby) = snapshot.lobby.as_ref() {
        for member in &lobby.members {
            if let Some(id) = state.summoners.get(&member.puuid).await {
                names.insert(member.puuid.clone(), id);
            }
        }
    }

    let lobby = snapshot.lobby.as_ref().map(|lobby| {
        let members: Vec<serde_json::Value> = lobby
            .members
            .iter()
            .map(|m| {
                serde_json::json!({
                    // Summoner ids routinely exceed 2^53, so they cross into
                    // JavaScript as strings or they come back corrupted.
                    "summonerId": m.summoner_id.to_string(),
                    // The lobby resource stopped carrying names; these come
                    // from the summoner resource, keyed by puuid. The old field
                    // is still read for older clients that do fill it.
                    "name": names
                        .get(&m.puuid)
                        .map(|id| id.name.clone())
                        .filter(|n| !n.is_empty())
                        .or_else(|| (!m.summoner_name.is_empty()).then(|| m.summoner_name.clone())),
                    "fullName": names.get(&m.puuid).map(|id| id.full.clone()),
                    "level": m.summoner_level,
                    "iconId": m.summoner_icon_id,
                    "isLeader": m.is_leader,
                    "isSpectator": m.is_spectator,
                    "ready": m.is_ready(),
                    "positions": position_preferences(m),
                })
            })
            .collect();

        serde_json::json!({
            "members": members,
            "maxSize": lobby.game_config.max_lobby_size,
            "gameMode": lobby.game_config.game_mode,
            "queueId": lobby.game_config.queue_id,
            "isCustom": lobby.game_config.is_custom,
            // Whether *we* are the leader. Only the leader can start a search,
            // so this is what decides if the queue controls are offered at all.
            "iAmLeader": lobby.i_am_leader(),
            "canStartActivity": lobby.can_start_activity,
        })
    });

    let searching = snapshot.is_searching();
    let search = snapshot.search.as_ref().map(|s| {
        serde_json::json!({
            "searching": searching,
            "state": s.search_state,
            // The low-priority delay is counted inside timeInQueue, so take it
            // off before showing a queue time.
            "timeInQueue": (s.time_in_queue - s.low_priority_data.penalty_time).max(0.0),
            "estimated": s.estimated_queue_time,
            "penaltyRemaining": s.penalty_remaining(),
        })
    });

    let now_ms = unix_millis();
    let champ_select = snapshot.champ_select.as_ref().map(|session| {
        let player = |p: &lol_state::PlayerSelection| {
            serde_json::json!({
                "cellId": p.cell_id,
                "championId": p.champion_id,
                "championName": champion_name(&id_to_name, p.champion_id),
                "intentId": p.champion_pick_intent,
                "intentName": champion_name(&id_to_name, p.champion_pick_intent),
                "position": p.assigned_position,
                "isMe": p.cell_id == session.local_player_cell_id,
                "isAutofilled": p.is_autofilled,
            })
        };

        let ban_names = |ids: &[i32]| -> Vec<serde_json::Value> {
            ids.iter()
                .filter(|id| **id > 0)
                .map(|id| {
                    serde_json::json!({
                        "championId": id,
                        "championName": champion_name(&id_to_name, *id),
                    })
                })
                .collect()
        };

        serde_json::json!({
            "assignedPosition": session.assigned_position(),
            "timerPhase": session.timer.phase,
            "timeLeft": session.timer.seconds_left(now_ms),
            "myTeam": session.my_team.iter().map(player).collect::<Vec<_>>(),
            "theirTeam": session.their_team.iter().map(player).collect::<Vec<_>>(),
            "myBans": ban_names(&session.bans.my_team_bans),
            "theirBans": ban_names(&session.bans.their_team_bans),
        })
    });

    serde_json::json!({
        "phase": snapshot.phase,
        "connected": state
            .lcu_monitor
            .connected
            .load(std::sync::atomic::Ordering::SeqCst),
        "queue": {
            "autoQueue": settings.auto_queue,
            "gate": gate.describe(),
            "ready": gate.is_ready(),
        },
        // The mode travels with the state, not only as its own event: the
        // event fires on change, and a frontend that loads between two of them
        // would never learn the mode at all.
        "queueMode": build_queue_mode_payload(state, &state.current_queue_mode.lock().await.clone()).await,
        "lobby": lobby,
        "search": search,
        "champSelect": champ_select,
        "decision": state.last_decision.lock().await.clone(),
    })
}

fn unix_millis() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as f64)
        .unwrap_or(0.0)
}

fn champion_name(id_to_name: &std::collections::HashMap<i32, String>, id: i32) -> Option<String> {
    if id <= 0 {
        return None;
    }
    Some(
        id_to_name
            .get(&id)
            .cloned()
            .unwrap_or_else(|| format!("Champion {}", id)),
    )
}

/// "MIDDLE / TOP", or nothing for queues without a lane picker.
fn position_preferences(member: &lol_state::LobbyMember) -> Option<String> {
    let parts: Vec<&str> = [
        member.first_position_preference.as_str(),
        member.second_position_preference.as_str(),
    ]
    .into_iter()
    .filter(|p| !p.is_empty() && *p != "UNSELECTED")
    .collect();

    (!parts.is_empty()).then(|| parts.join(" / "))
}

/// Record what selection decided, for the info panel.
async fn record_decision(
    state: &Arc<AppState>,
    what: &str,
    chosen: Option<&selection::Choice>,
    passed_over: &[selection::PassedOver],
) {
    *state.last_decision.lock().await = Some(serde_json::json!({
        // Stable key, so the panel does not have to care about the casing
        // used by the log strings: "pick" or "ban".
        "what": what.to_lowercase(),
        "chosen": chosen.map(|c| c.name.clone()),
        "championId": chosen.map(|c| c.champion_id),
        "passedOver": passed_over
            .iter()
            .map(|p| serde_json::json!({ "name": p.name, "reason": p.reason.reason() }))
            .collect::<Vec<_>>(),
    }));
}

/// Fill in the names the lobby payload no longer carries.
///
/// Spawned rather than awaited: it is one loopback request per person we have
/// not seen before, and the lobby should paint immediately with the fallback
/// rather than wait on the network. Re-emits only when something was actually
/// learned, so a lobby of familiar faces costs nothing.
fn resolve_lobby_names(state: &Arc<AppState>, app_handle: &tauri::AppHandle) {
    let state = state.clone();
    let app_handle = app_handle.clone();
    tauri::async_runtime::spawn(async move {
        let snapshot = state.lol_state.get().await;
        let Some(lobby) = snapshot.lobby.as_ref() else {
            return;
        };
        let puuids: Vec<String> = lobby.members.iter().map(|m| m.puuid.clone()).collect();
        if puuids.is_empty() {
            return;
        }
        let Some(creds) = state.lcu_monitor.get_credentials().await else {
            return;
        };
        if state.summoners.resolve_missing(&creds, &puuids).await {
            emit_state(&state, &app_handle, true).await;
        }
    });
}

/// A fingerprint of "this lobby, as it is right now".
///
/// A hand cancel suppresses auto queue until one of these changes: the party,
/// the queue, or who is in it. Anything coarser (a plain boolean) either sticks
/// forever or is cleared by the very Search push the cancel itself produced.
async fn lobby_fingerprint(state: &Arc<AppState>) -> Option<String> {
    let snapshot = state.lol_state.get().await;
    let lobby = snapshot.lobby.as_ref()?;
    let mut ids: Vec<u64> = lobby.members.iter().map(|m| m.summoner_id).collect();
    ids.sort_unstable();
    Some(format!(
        "{}:{}:{:?}",
        lobby.party_id, lobby.game_config.queue_id, ids
    ))
}

/// Mark the current lobby as "the user does not want to be in the queue".
///
/// Auto queue holds off until the lobby itself changes.
async fn suppress_auto_queue(state: &Arc<AppState>, why: &str) {
    // Retire any timer that is already counting down, then mark the lobby.
    state
        .auto_queue_generation
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);

    let fingerprint = lobby_fingerprint(state).await;
    if fingerprint.is_none() {
        return;
    }
    let mut cancelled = state.queue_cancelled_for.lock().await;
    if *cancelled != fingerprint {
        log::info!("Auto queue suppressed: {}", why);
        *cancelled = fingerprint;
    }
}

/// Notice that a search ended without a match, whoever ended it.
///
/// The in-app Cancel·lar button is the easy half. The other half is the button
/// in the League client itself, which produces no signal of its own — all we
/// see is a search that stopped. So the rule is written in terms of the
/// transition rather than the actor: we were in the queue, we are not any more,
/// and we did not end up in a champ select. Whatever caused it, re-queueing two
/// seconds later is not what the user wanted.
///
/// `ready_check` and `champ_select` phases are excluded, so a match that was
/// actually found does not count as a cancel.
async fn note_search_state(state: &Arc<AppState>) {
    let snapshot = state.lol_state.get().await;
    let searching = snapshot.is_searching();
    let was_searching = state
        .was_searching
        .swap(searching, std::sync::atomic::Ordering::SeqCst);

    if !was_searching || searching {
        return;
    }

    // Left the queue. Only a return to the lobby is a cancel; anything else is
    // the match starting.
    if matches!(snapshot.phase.as_str(), "" | "None" | "Lobby") {
        suppress_auto_queue(state, "la cerca s'ha cancel·lat").await;
    }
}

/// Whether auto queue is currently suppressed by a hand cancel, clearing the
/// suppression once the lobby it was set for has moved on.
async fn cancel_suppressed(state: &Arc<AppState>) -> bool {
    let Some(marked) = state.queue_cancelled_for.lock().await.clone() else {
        return false;
    };
    // Read before re-taking the lock: nothing holds a guard across an await.
    if lobby_fingerprint(state).await.as_deref() == Some(marked.as_str()) {
        return true;
    }
    // Different lobby, or no lobby at all: the user has moved on and so does
    // the suppression.
    *state.queue_cancelled_for.lock().await = None;
    false
}

/// Re-evaluate the auto-queue gate and act on it.
///
/// Called on every lobby, search and phase event. The POST is not sent inline:
/// a pending timer is armed instead, and any state change before it fires
/// retires it. That is what stops a member joining and leaving again from
/// firing a search, and it is what gives the user a moment to cancel.
async fn evaluate_auto_queue(state: &Arc<AppState>, app_handle: &tauri::AppHandle) {
    let settings = state.settings_manager.get();
    let snapshot = state.lol_state.get().await;
    let cancelled = cancel_suppressed(state).await;
    let gate = queue::evaluate_with(&snapshot, &settings, cancelled);

    // Every re-evaluation retires the pending timer. If the gate still says go,
    // a fresh one is armed below; if it doesn't, nothing fires.
    let generation = state
        .auto_queue_generation
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        + 1;

    // Report a change of verdict once, not on every one of the two Update
    // events the client emits for each lobby change.
    {
        let mut last = state.last_queue_gate.lock().await;
        let described = gate.describe();
        if *last != described {
            *last = described.clone();
            if gate.is_noteworthy() {
                log::info!("Auto queue blocked: {}", described);
                let _ = app_handle.emit("log", &format!("Auto cerca: {}", described));
            } else {
                log::debug!("Auto queue gate: {}", described);
            }
        }
    }


    if !gate.is_ready() {
        return;
    }

    let delay = settings.auto_queue_delay_secs.clamp(0.0, 30.0);
    let state = state.clone();
    let app_handle = app_handle.clone();

    tauri::async_runtime::spawn(async move {
        if delay > 0.0 {
            tokio::time::sleep(Duration::from_secs_f64(delay)).await;
        }

        // Someone re-evaluated while we slept: they own the decision now.
        if state
            .auto_queue_generation
            .load(std::sync::atomic::Ordering::SeqCst)
            != generation
        {
            return;
        }

        // The lobby can have changed during the delay without producing an
        // event we saw, so the gate is checked again against fresh state
        // rather than trusting the verdict from before the sleep.
        let settings = state.settings_manager.get();
        let snapshot = state.lol_state.get().await;
        let cancelled = cancel_suppressed(&state).await;
        if !queue::evaluate_with(&snapshot, &settings, cancelled).is_ready() {
            return;
        }

        let Some(creds) = state.lcu_monitor.get_credentials().await else {
            return;
        };

        match actions::start_matchmaking(&creds).await {
            Ok(_) => {
                log::info!("Auto queue started matchmaking");
                // Past tense on purpose: the statusbar shows the last log line,
                // and a present-tense "Buscant partida..." sat there claiming to
                // be live status long after the search had been left.
                let _ = app_handle.emit("log", "Cua iniciada");
            }
            Err(e) => {
                // The POST result is not the source of truth - the search is
                // confirmed by searchState - so a failure here is logged and
                // the next lobby event re-evaluates.
                log::warn!("Auto queue start failed: {}", e);
                let _ = app_handle.emit("log", &format!("Error encuant: {}", e));
            }
        }
    });
}

/// Spend every "just this game" entry, now that the game has actually started.
///
/// Deliberately tied to InProgress rather than to the pick itself: a champ
/// select that gets dodged never became a game, and burning the one-shot there
/// would leave the user with a disabled list for the re-queue they didn't ask
/// for.
async fn spend_one_shot_entries(state: &Arc<AppState>, app_handle: &tauri::AppHandle) {
    let mut settings = state.settings_manager.get();
    let spent = settings::spend_one_shots(&mut settings.pick_lists)
        + settings::spend_one_shots(&mut settings.ban_lists);

    if spent == 0 {
        return;
    }

    state.settings_manager.update(settings.clone());
    log::info!("Spent {} one-shot list entries", spent);
    let _ = app_handle.emit(
        "log",
        &format!(
            "{} {} d'un sol ús {}",
            spent,
            if spent == 1 { "champion" } else { "champions" },
            if spent == 1 {
                "s'ha desactivat"
            } else {
                "s'han desactivat"
            }
        ),
    );
    // The lists are the user's own data; push the change so the UI greys the
    // chips immediately instead of showing them armed until the next restart.
    let _ = app_handle.emit("settings-changed", &settings);
}

/// Remember a champion InstaLock actually locked or banned, so it is one click
/// away next time instead of being typed again.
async fn remember_recent(
    state: &Arc<AppState>,
    app_handle: &tauri::AppHandle,
    intent: selection::Intent,
    name: &str,
) {
    let mut settings = state.settings_manager.get();
    match intent {
        selection::Intent::Pick => settings::push_recent(&mut settings.recent_picks, name),
        selection::Intent::Ban => settings::push_recent(&mut settings.recent_bans, name),
    }
    state.settings_manager.update(settings.clone());
    let _ = app_handle.emit("settings-changed", &settings);
}

/// Whether this queue assigns lanes at all.
///
/// ARAM, Arena, URF and blind pick don't, so per-role champion lists are
/// meaningless there and the UI collapses to the single default list. The
/// lobby's own `showPositionSelector` is the authority — it is the same flag
/// the client uses to decide whether to show its lane picker — and the mode
/// name is only the fallback for when we join a game with no lobby left.
fn has_positions(snapshot: &lol_state::Snapshot, _mode: Option<&str>) -> bool {
    if let Some(lobby) = &snapshot.lobby {
        return lobby.game_config.show_position_selector;
    }
    // Mid-champ-select with no lobby left: the assignment itself settles it.
    if let Some(session) = &snapshot.champ_select {
        return !session.assigned_position().is_empty();
    }
    // Nothing open at all — League closed, or sitting in the client menu. This
    // is exactly when the user is configuring their lists, so show every role.
    // Hiding them here would make the lane lists unreachable.
    true
}

/// The mode-shaped facts the frontend needs to decide what to show.
///
/// Emitted whenever the lobby or the champ-select session moves, so the UI
/// never has to guess from a hardcoded list of queue ids.
async fn emit_mode_context(state: &Arc<AppState>, app_handle: &tauri::AppHandle) {
    let snapshot = state.lol_state.get().await;
    let mode = state
        .current_queue_mode
        .lock()
        .await
        .as_ref()
        .map(|q| q.game_mode.clone());

    let assigned = snapshot
        .champ_select
        .as_ref()
        .map(|s| s.assigned_position().to_string())
        .unwrap_or_default();

    let lobby_position = snapshot
        .lobby
        .as_ref()
        .map(|l| l.local_member.first_position_preference.to_lowercase())
        .filter(|p| !p.is_empty() && p != "unselected" && p != "fill")
        .unwrap_or_default();

    let queue_id = state
        .current_queue_mode
        .lock()
        .await
        .as_ref()
        .map(|q| q.queue_id)
        .unwrap_or(0);
    let has_champ_select = match state.queues.get(queue_id).await {
        Some(info) => info.has_champ_select(),
        // Unknown queue, or the table has not loaded: show the cards. Hiding
        // them on a guess would make pick and ban unreachable.
        None => true,
    };

    let _ = app_handle.emit(
        "mode-context",
        serde_json::json!({
            "hasPositions": has_positions(&snapshot, mode.as_deref()),
            "assignedPosition": assigned,
            // What the user asked for in the lobby, before champ select has
            // assigned anything. It is what makes the role tabs land on the
            // list that is actually going to be read.
            "lobbyPosition": lobby_position,
            // ARAM-family queues have no pick or ban phase at all, so their
            // cards are hidden. Driven by the client map id, never by the mode
            // codename - those change every patch.
            "hasChampSelect": has_champ_select,
            "phase": snapshot.phase,
        }),
    );
}

/// Work out which queue we are actually in, in order of authority.
///
/// The order matters. The client **deletes the lobby the moment champ select
/// starts**, so anything that trusted only the lobby went blind exactly when
/// the pick and ban lists were about to be read. From there the session's
/// `queueId` is the only identifier left, and resolving it needs the client's
/// own queue table — a hand-written match knew 450, 1700 and 1710, so Arena
/// 3x6 (1750) and ARAM: Mayhem (2400) both fell through to Summoner's Rift.
async fn resolve_queue_mode(state: &Arc<AppState>) -> Option<QueueModeInfo> {
    let snapshot = state.lol_state.get().await;

    if let Some(lobby) = &snapshot.lobby {
        if !lobby.game_config.game_mode.is_empty() {
            return Some(QueueModeInfo {
                game_mode: lobby.game_config.game_mode.clone(),
                queue_id: lobby.game_config.queue_id,
                map_id: state
                    .queues
                    .get(lobby.game_config.queue_id)
                    .await
                    .map(|q| q.map_id)
                    .unwrap_or(0),
            });
        }
    }

    if let Some(session) = &snapshot.champ_select {
        if session.queue_id > 0 {
            if let Some(info) = state.queues.get(session.queue_id).await {
                return Some(QueueModeInfo {
                    game_mode: info.game_mode,
                    queue_id: session.queue_id,
                    map_id: info.map_id,
                });
            }
        }
        // The table did not load, or does not know this queue. Hold the last
        // known mode rather than falling back to Summoner's Rift: picking from
        // the wrong per-mode list is worse than picking from a stale one.
        return state.current_queue_mode.lock().await.clone();
    }

    // No lobby and no session. During champ select and the game itself that is
    // a transient - the lobby Delete lands before the first session push - and
    // clearing the mode here would throw away the very thing the branch above
    // needs to fall back on.
    if matches!(
        snapshot.phase.as_str(),
        "ChampSelect" | "InProgress" | "GameStart" | "Reconnect"
    ) {
        return state.current_queue_mode.lock().await.clone();
    }

    None
}

/// Recompute the mode and, only if it actually changed, store it, tell the UI
/// and write one line to the log.
///
/// The log line is the point: a mode that silently disagrees with the client is
/// the kind of bug that is impossible to chase after the fact, and every
/// transition is now on the record with its queue id and map.
async fn refresh_queue_mode(state: &Arc<AppState>, app_handle: &tauri::AppHandle) {
    let resolved = resolve_queue_mode(state).await;

    let mut current = state.current_queue_mode.lock().await;
    let changed = match (&*current, &resolved) {
        (None, None) => false,
        (Some(a), Some(b)) => a.game_mode != b.game_mode || a.queue_id != b.queue_id,
        _ => true,
    };
    if !changed {
        return;
    }

    match &resolved {
        Some(q) => log::info!(
            "Queue mode -> {} (queueId {}, map {})",
            q.game_mode,
            q.queue_id,
            q.map_id
        ),
        None => log::info!("Queue mode -> none"),
    }

    *current = resolved.clone();
    drop(current);

    let payload = build_queue_mode_payload(state, &resolved).await;
    let _ = app_handle.emit("queue-mode", payload);
    emit_mode_context(state, app_handle).await;
}

async fn build_queue_mode_payload(
    state: &Arc<AppState>,
    info: &Option<QueueModeInfo>,
) -> serde_json::Value {
    match info {
        Some(q) => serde_json::json!({
            "gameMode": q.game_mode,
            "queueId": q.queue_id,
            "mapId": q.map_id,
            // The client's own name for the queue: "ARAM: Mayhem", "Arena 3x6".
            // Anything hand-written here is stale within a patch.
            "displayName": state.queues.label(q.queue_id, &q.game_mode).await,
        }),
        None => serde_json::Value::Null,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The LCU reports phase timers in milliseconds and every configured delay
    /// is in seconds. Mixing the two is what commit 316862d fixed, and nothing
    /// had guarded it since.
    #[test]
    fn a_delay_that_fits_is_used_as_is() {
        assert_eq!(clamp_delay(5.0, 30.0, 1.5), 5.0);
    }

    #[test]
    fn a_delay_past_the_timer_is_cut_to_the_margin() {
        // 8s left, 1.5s margin: act at 6.5s however long the user asked for.
        assert_eq!(clamp_delay(20.0, 8.0, 1.5), 6.5);
    }

    /// Acting immediately beats not acting: a timer already inside the margin
    /// must still produce a pick, not a negative sleep.
    #[test]
    fn never_goes_negative() {
        assert_eq!(clamp_delay(10.0, 1.0, 1.5), 0.0);
        assert_eq!(clamp_delay(0.0, 0.5, 1.5), 0.0);
        assert_eq!(clamp_delay(3.0, 0.0, 1.5), 0.0);
    }

    #[test]
    fn zero_delay_stays_zero() {
        assert_eq!(clamp_delay(0.0, 30.0, 1.5), 0.0);
    }

}
