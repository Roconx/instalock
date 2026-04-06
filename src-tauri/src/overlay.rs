use instalock_shared::{spell_by_id, spell_cooldown, COSMIC_INSIGHT_ID, UNSEALED_SPELLBOOK_ID};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnemyData {
    pub champion_id: i32,
    pub champion_name: String,
    pub spell1_id: i32,
    pub spell2_id: i32,
    pub spell1_name: String,
    pub spell2_name: String,
    pub spell1_cooldown: u32,
    pub spell2_cooldown: u32,
    pub has_cosmic_insight: bool,
    pub has_unsealed_spellbook: bool,
}

#[derive(Default)]
pub struct OverlayState {
    pub enemies: Mutex<Vec<EnemyData>>,
    pub game_id: Mutex<Option<String>>,
}

impl OverlayState {
    pub fn new() -> Self {
        Self::default()
    }
}

/// Extract enemy team data from champ select session JSON
pub fn extract_enemies(
    session: &serde_json::Value,
    id_to_name: &std::collections::HashMap<i32, String>,
) -> Vec<EnemyData> {
    let Some(their_team) = session.get("theirTeam").and_then(|v| v.as_array()) else {
        return Vec::new();
    };

    their_team
        .iter()
        .filter_map(|player| {
            let champion_id = player.get("championId")?.as_i64()? as i32;
            if champion_id <= 0 {
                return None;
            }
            let spell1_id = player.get("spell1Id")?.as_i64()? as i32;
            let spell2_id = player.get("spell2Id")?.as_i64()? as i32;

            let champion_name = id_to_name
                .get(&champion_id)
                .cloned()
                .unwrap_or_else(|| format!("Champion {}", champion_id));

            Some(EnemyData {
                champion_id,
                champion_name,
                spell1_name: spell_by_id(spell1_id)
                    .map(|s| s.name.to_string())
                    .unwrap_or_else(|| "Unknown".into()),
                spell2_name: spell_by_id(spell2_id)
                    .map(|s| s.name.to_string())
                    .unwrap_or_else(|| "Unknown".into()),
                spell1_cooldown: spell_cooldown(spell1_id, false),
                spell2_cooldown: spell_cooldown(spell2_id, false),
                spell1_id,
                spell2_id,
                has_cosmic_insight: false,
                has_unsealed_spellbook: false,
            })
        })
        .collect()
}

/// Poll the Live Client Data API to detect runes for enemies
pub async fn poll_live_client_runes(overlay_state: Arc<OverlayState>) {
    let client = match reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .build()
    {
        Ok(c) => c,
        Err(_) => return,
    };

    // Retry up to 30 times (game takes ~30s to load)
    for _ in 0..30 {
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;

        let resp = match client
            .get("https://127.0.0.1:2999/liveclientdata/allgamedata")
            .send()
            .await
        {
            Ok(r) if r.status().is_success() => r,
            _ => continue,
        };

        let data: serde_json::Value = match resp.json().await {
            Ok(d) => d,
            Err(_) => continue,
        };

        let Some(all_players) = data.get("allPlayers").and_then(|v| v.as_array()) else {
            continue;
        };

        // Get active player's team to identify enemies
        let active_team = data
            .get("activePlayer")
            .and_then(|p| p.get("teamID"))
            .and_then(|t| t.as_str())
            .unwrap_or("");

        let enemy_team = if active_team == "ORDER" {
            "CHAOS"
        } else {
            "ORDER"
        };

        let mut enemies = overlay_state.enemies.lock().await;
        if enemies.is_empty() {
            continue;
        }

        // Match enemies by champion name and update rune data
        for player in all_players {
            let team = player.get("team").and_then(|t| t.as_str()).unwrap_or("");
            if team != enemy_team {
                continue;
            }

            let champ_name = player
                .get("championName")
                .and_then(|n| n.as_str())
                .unwrap_or("");

            // Find matching enemy by champion name
            if let Some(enemy) = enemies.iter_mut().find(|e| e.champion_name == champ_name) {
                let has_cosmic = has_rune(player, COSMIC_INSIGHT_ID);
                let has_spellbook = has_rune(player, UNSEALED_SPELLBOOK_ID);

                enemy.has_cosmic_insight = has_cosmic;
                enemy.has_unsealed_spellbook = has_spellbook;
                enemy.spell1_cooldown = spell_cooldown(enemy.spell1_id, has_cosmic);
                enemy.spell2_cooldown = spell_cooldown(enemy.spell2_id, has_cosmic);
            }
        }

        // Successfully parsed — done polling
        return;
    }
}

fn has_rune(player: &serde_json::Value, rune_id: i32) -> bool {
    let runes = match player.get("runes") {
        Some(r) => r,
        None => return false,
    };

    // Check keystone
    if let Some(keystone) = runes.get("keystone").and_then(|k| k.get("id")) {
        if keystone.as_i64() == Some(rune_id as i64) {
            return true;
        }
    }

    // Check general runes
    if let Some(general) = runes.get("generalRunes").and_then(|g| g.as_array()) {
        for rune in general {
            if let Some(id) = rune.get("id").and_then(|i| i.as_i64()) {
                if id == rune_id as i64 {
                    return true;
                }
            }
        }
    }

    false
}
