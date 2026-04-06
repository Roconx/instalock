use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub auto_accept: bool,
    pub auto_pick: bool,
    pub auto_ban: bool,
    pub pick_champion: String,
    pub ban_champion: String,
    #[serde(default)]
    pub bravery_enabled: bool,
    #[serde(default = "default_true")]
    pub restore_focus_after_action: bool,
    #[serde(default)]
    pub accept_delay_secs: f64,
    #[serde(default)]
    pub pick_delay_secs: f64,
    #[serde(default)]
    pub ban_delay_secs: f64,
    #[serde(default = "default_margin")]
    pub action_margin_secs: f64,
    // Overlay settings
    #[serde(default = "default_true")]
    pub overlay_enabled: bool,
    #[serde(default = "default_opacity")]
    pub overlay_opacity: f64,
    #[serde(default)]
    pub overlay_x: Option<f64>,
    #[serde(default)]
    pub overlay_y: Option<f64>,
    // Sync settings
    #[serde(default)]
    pub sync_enabled: bool,
    #[serde(default = "default_server_url")]
    pub sync_server_url: String,
}

fn default_true() -> bool {
    true
}

fn default_margin() -> f64 {
    1.5
}

fn default_opacity() -> f64 {
    0.8
}

fn default_server_url() -> String {
    "ws://localhost:9876".to_string()
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            auto_accept: true,
            auto_pick: false,
            auto_ban: false,
            pick_champion: String::new(),
            ban_champion: String::new(),
            bravery_enabled: false,
            restore_focus_after_action: true,
            accept_delay_secs: 0.0,
            pick_delay_secs: 0.0,
            ban_delay_secs: 0.0,
            action_margin_secs: 1.5,
            overlay_enabled: true,
            overlay_opacity: 0.8,
            overlay_x: None,
            overlay_y: None,
            sync_enabled: false,
            sync_server_url: default_server_url(),
        }
    }
}

pub struct SettingsManager {
    pub settings: Mutex<Settings>,
    path: PathBuf,
}

impl SettingsManager {
    pub fn new() -> Self {
        let path = dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("instalock")
            .join("settings.json");

        let settings = Self::load_from(&path);

        Self {
            settings: Mutex::new(settings),
            path,
        }
    }

    fn load_from(path: &PathBuf) -> Settings {
        match fs::read_to_string(path) {
            Ok(data) => serde_json::from_str(&data).unwrap_or_default(),
            Err(_) => Settings::default(),
        }
    }

    pub fn save(&self) {
        let settings = self.settings.lock().unwrap().clone();
        if let Some(parent) = self.path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let data = serde_json::to_string_pretty(&settings).unwrap();
        let _ = fs::write(&self.path, data);
    }

    pub fn get(&self) -> Settings {
        self.settings.lock().unwrap().clone()
    }

    pub fn update(&self, new_settings: Settings) {
        *self.settings.lock().unwrap() = new_settings;
        self.save();
    }
}
