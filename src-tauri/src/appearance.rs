//! Background image storage for the custom UI background.
//!
//! The image is kept as a data URI in its own file next to settings.json
//! rather than inside it: settings.json is rewritten on every debounced
//! change, and a multi-megabyte base64 blob would be rewritten with it.

use std::fs;
use std::path::PathBuf;

use crate::settings::config_dir;

/// Refuse anything larger than this so a stray 4K PNG can't wedge startup.
const MAX_BYTES: usize = 12 * 1024 * 1024;

fn background_path() -> PathBuf {
    config_dir().join("background.txt")
}

#[tauri::command]
pub fn save_background(data_uri: String) -> Result<(), String> {
    if !data_uri.starts_with("data:image/") {
        return Err("Not an image data URI".into());
    }
    if data_uri.len() > MAX_BYTES {
        return Err(format!(
            "Image too large ({} MB, max {} MB)",
            data_uri.len() / 1024 / 1024,
            MAX_BYTES / 1024 / 1024
        ));
    }

    let path = background_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    fs::write(&path, data_uri).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn get_background() -> Option<String> {
    fs::read_to_string(background_path()).ok()
}

#[tauri::command]
pub fn clear_background() -> Result<(), String> {
    match fs::remove_file(background_path()) {
        Ok(_) => Ok(()),
        // Already gone is the desired end state, not a failure.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}
