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

/// Role order for sorting: Top → Jungle → Mid → Bot → Support
fn role_order(position: &str) -> u8 {
    match position.to_uppercase().as_str() {
        "TOP" => 0,
        "JUNGLE" => 1,
        "MIDDLE" | "MID" => 2,
        "BOTTOM" | "BOT" | "ADC" => 3,
        "UTILITY" | "SUPPORT" | "SUP" => 4,
        _ => 5,
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

    let mut enemies: Vec<(u8, EnemyData)> = their_team
        .iter()
        .filter_map(|player| {
            let champion_id = player.get("championId")?.as_i64()? as i32;
            if champion_id <= 0 {
                return None;
            }
            let spell1_id = player.get("spell1Id").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
            let spell2_id = player.get("spell2Id").and_then(|v| v.as_i64()).unwrap_or(0) as i32;

            let champion_name = id_to_name
                .get(&champion_id)
                .cloned()
                .unwrap_or_else(|| format!("Champion {}", champion_id));

            let position = player
                .get("assignedPosition")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let order = role_order(position);

            Some((order, EnemyData {
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
            }))
        })
        .collect();

    enemies.sort_by_key(|(order, _)| *order);
    enemies.into_iter().map(|(_, e)| e).collect()
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

        // Find active player's team by matching summoner name in allPlayers
        let active_name = data
            .get("activePlayer")
            .and_then(|p| p.get("riotIdGameName"))
            .or_else(|| data.get("activePlayer").and_then(|p| p.get("summonerName")))
            .and_then(|n| n.as_str())
            .unwrap_or("");

        let active_team = all_players
            .iter()
            .find(|p| {
                let name = p.get("riotIdGameName")
                    .or_else(|| p.get("summonerName"))
                    .and_then(|n| n.as_str())
                    .unwrap_or("");
                name == active_name
            })
            .and_then(|p| p.get("team"))
            .and_then(|t| t.as_str())
            .unwrap_or("ORDER");

        let enemy_team = if active_team == "ORDER" {
            "CHAOS"
        } else {
            "ORDER"
        };

        let mut enemies = overlay_state.enemies.lock().await;

        // Collect enemy players from Live Client Data
        let mut live_enemies: Vec<&serde_json::Value> = Vec::new();
        for player in all_players {
            let team = player.get("team").and_then(|t| t.as_str()).unwrap_or("");
            if team == enemy_team {
                live_enemies.push(player);
            }
        }

        if live_enemies.is_empty() {
            continue;
        }

        // If enemies list is empty (e.g. practice tool, no champ select data),
        // populate it from the Live Client Data API
        if enemies.is_empty() {
            for player in &live_enemies {
                let champ_name = player
                    .get("championName")
                    .and_then(|n| n.as_str())
                    .unwrap_or("Unknown")
                    .to_string();

                // Live Client Data uses summonerSpells.summonerSpellOne/Two
                let spell1_id = player
                    .pointer("/summonerSpells/summonerSpellOne/rawDescription")
                    .and_then(|_| player.pointer("/summonerSpells/summonerSpellOne/displayName"))
                    .and_then(|n| n.as_str())
                    .map(spell_id_from_display_name)
                    .unwrap_or(4); // default Flash

                let spell2_id = player
                    .pointer("/summonerSpells/summonerSpellTwo/rawDescription")
                    .and_then(|_| player.pointer("/summonerSpells/summonerSpellTwo/displayName"))
                    .and_then(|n| n.as_str())
                    .map(spell_id_from_display_name)
                    .unwrap_or(14); // default Ignite

                let has_cosmic = has_rune(player, COSMIC_INSIGHT_ID);
                let has_spellbook = has_rune(player, UNSEALED_SPELLBOOK_ID);

                enemies.push(EnemyData {
                    champion_id: 0,
                    champion_name: champ_name,
                    spell1_name: spell_by_id(spell1_id)
                        .map(|s| s.name.to_string())
                        .unwrap_or_else(|| "Unknown".into()),
                    spell2_name: spell_by_id(spell2_id)
                        .map(|s| s.name.to_string())
                        .unwrap_or_else(|| "Unknown".into()),
                    spell1_cooldown: spell_cooldown(spell1_id, has_cosmic),
                    spell2_cooldown: spell_cooldown(spell2_id, has_cosmic),
                    spell1_id,
                    spell2_id,
                    has_cosmic_insight: has_cosmic,
                    has_unsealed_spellbook: has_spellbook,
                });
            }

            // Sort by role
            let positions: Vec<(String, u8)> = live_enemies
                .iter()
                .map(|p| {
                    let name = p.get("championName").and_then(|n| n.as_str()).unwrap_or("");
                    let pos = p.get("position").and_then(|n| n.as_str()).unwrap_or("");
                    (name.to_string(), role_order(pos))
                })
                .collect();

            enemies.sort_by_key(|e| {
                positions
                    .iter()
                    .find(|(name, _)| *name == e.champion_name)
                    .map(|(_, order)| *order)
                    .unwrap_or(5)
            });
        } else {
            // Update existing enemies with rune data + spell IDs from Live Client Data
            for player in &live_enemies {
                let champ_name = player
                    .get("championName")
                    .and_then(|n| n.as_str())
                    .unwrap_or("");

                if let Some(enemy) = enemies.iter_mut().find(|e| e.champion_name == champ_name) {
                    let has_cosmic = has_rune(player, COSMIC_INSIGHT_ID);
                    let has_spellbook = has_rune(player, UNSEALED_SPELLBOOK_ID);

                    enemy.has_cosmic_insight = has_cosmic;
                    enemy.has_unsealed_spellbook = has_spellbook;

                    // Update spell IDs from Live Client Data (champ select may have 0s)
                    if let Some(spell1_name) = player
                        .pointer("/summonerSpells/summonerSpellOne/displayName")
                        .and_then(|n| n.as_str())
                    {
                        let id = spell_id_from_display_name(spell1_name);
                        enemy.spell1_id = id;
                        enemy.spell1_name = spell_by_id(id)
                            .map(|s| s.name.to_string())
                            .unwrap_or_else(|| spell1_name.to_string());
                    }
                    if let Some(spell2_name) = player
                        .pointer("/summonerSpells/summonerSpellTwo/displayName")
                        .and_then(|n| n.as_str())
                    {
                        let id = spell_id_from_display_name(spell2_name);
                        enemy.spell2_id = id;
                        enemy.spell2_name = spell_by_id(id)
                            .map(|s| s.name.to_string())
                            .unwrap_or_else(|| spell2_name.to_string());
                    }

                    enemy.spell1_cooldown = spell_cooldown(enemy.spell1_id, has_cosmic);
                    enemy.spell2_cooldown = spell_cooldown(enemy.spell2_id, has_cosmic);
                }
            }

            // Sort by role using Live Client Data position
            let positions: Vec<(String, u8)> = live_enemies
                .iter()
                .map(|p| {
                    let name = p.get("championName").and_then(|n| n.as_str()).unwrap_or("");
                    let pos = p.get("position").and_then(|n| n.as_str()).unwrap_or("");
                    (name.to_string(), role_order(pos))
                })
                .collect();

            enemies.sort_by_key(|e| {
                positions
                    .iter()
                    .find(|(name, _)| *name == e.champion_name)
                    .map(|(_, order)| *order)
                    .unwrap_or(5)
            });
        }

        // Successfully parsed — done polling
        return;
    }
}

/// Map Live Client Data API display name to spell ID
fn spell_id_from_display_name(name: &str) -> i32 {
    match name {
        "Flash" => 4,
        "Heal" => 7,
        "Ghost" => 6,
        "Barrier" => 21,
        "Exhaust" => 3,
        "Ignite" => 14,
        "Cleanse" => 1,
        "Teleport" => 12,
        "Smite" => 11,
        "Mark" => 32,
        _ => 4, // fallback
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
