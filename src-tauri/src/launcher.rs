//! Start the League client through the Riot Client.
//!
//! This runs the exact command Riot's own "League of Legends" shortcut uses:
//! `RiotClientServices.exe --launch-product=league_of_legends --launch-patchline=live`
//!
//! Two things were tried and rejected, both verified on a real install:
//!
//! - Spawning `LeagueClient.exe` directly fails with "Access is denied" even
//!   though the file grants Users execute rights — Riot blocks starting the
//!   game client from a parent other than the Riot Client.
//! - Nothing here can force a launch when the Riot Client is already running
//!   and sitting at sign-in: the request reaches the live instance, which just
//!   navigates to the League tab. The desktop shortcut behaves identically, so
//!   this is the ceiling for any launcher, not a limitation of ours.

use std::path::PathBuf;

/// Riot records every install here, and the Riot Client reads the same file, so
/// this stays correct for non-default install locations.
const INSTALLS_JSON: &str = r"C:\ProgramData\Riot Games\RiotClientInstalls.json";

fn from_installs_json() -> Option<PathBuf> {
    let data = std::fs::read_to_string(INSTALLS_JSON).ok()?;
    let json: serde_json::Value = serde_json::from_str(&data).ok()?;

    // rc_live is the live patchline; rc_default is what a plain install writes.
    for key in ["rc_live", "rc_default", "rc_beta"] {
        if let Some(path) = json.get(key).and_then(|v| v.as_str()) {
            let path = PathBuf::from(path);
            if path.is_file() {
                return Some(path);
            }
        }
    }
    None
}

/// Same reasoning as the lockfile scan in `lcu`: don't assume C:.
fn from_drive_scan() -> Option<PathBuf> {
    for drive in 'C'..='Z' {
        let path = PathBuf::from(format!(
            r"{}:\Riot Games\Riot Client\RiotClientServices.exe",
            drive
        ));
        if path.is_file() {
            return Some(path);
        }
    }
    None
}

pub fn riot_client_path() -> Option<PathBuf> {
    from_installs_json().or_else(from_drive_scan)
}

/// Ask the Riot Client to start League. Returns as soon as the process is
/// spawned; the LCU connection is what actually confirms League came up, and
/// the button hides itself when that happens.
pub fn launch_league() -> Result<(), String> {
    let exe = riot_client_path()
        .ok_or_else(|| "No s'ha trobat el Riot Client (RiotClientServices.exe)".to_string())?;

    log::info!("Launching League via {}", exe.display());

    std::process::Command::new(&exe)
        .args([
            "--launch-product=league_of_legends",
            "--launch-patchline=live",
        ])
        .spawn()
        .map_err(|e| format!("No s'ha pogut obrir el Riot Client: {}", e))?;

    Ok(())
}
