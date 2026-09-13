use instalock_shared::{SPELLS, COSMIC_INSIGHT_ID, UNSEALED_SPELLBOOK_ID};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};

const CD_BASE: &str = "https://raw.communitydragon.org/latest/plugins/rcp-be-lol-game-data/global/default/";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnemyData {
    pub champion_id: i32,
    pub champion_name: String,
    pub spell1_id: i32,
    pub spell2_id: i32,
    pub spell1_name: String,
    pub spell2_name: String,
    pub spell1_icon: String,
    pub spell2_icon: String,
    pub spell1_cooldown: u32,
    pub spell2_cooldown: u32,
    pub has_cosmic_insight: bool,
    pub has_unsealed_spellbook: bool,
}

/// Dynamic spell data loaded from Community Dragon CDN
#[derive(Debug, Clone)]
pub struct SpellData {
    pub name: String,
    pub icon_url: String,
    pub cooldown: u32,
}

/// Registry of all summoner spells, loaded dynamically from CDN
#[derive(Default)]
pub struct SpellRegistry {
    by_id: HashMap<i32, SpellData>,
}

impl SpellRegistry {
    /// Fetch summoner-spells.json from Community Dragon and build the registry.
    ///
    /// `None` means "ask again later", not "use nothing": the caller is already
    /// holding the hardcoded fallback and only swaps it for a real answer.
    pub async fn load() -> Option<Self> {
        let url = format!("{}v1/summoner-spells.json", CD_BASE);
        log::info!("Loading spell data from CDN...");

        // Via the shared CDN client: the bare reqwest::get this replaced had no
        // timeout at all, and this call used to run before the window existed.
        let spells: Vec<serde_json::Value> = match crate::http::cdn().get(&url).send().await {
            Ok(resp) => match resp.json().await {
                Ok(data) => data,
                Err(e) => {
                    log::error!("Failed to parse summoner-spells.json: {}", e);
                    return None;
                }
            },
            Err(e) => {
                log::error!("Failed to fetch summoner-spells.json: {}", e);
                return None;
            }
        };

        let mut by_id = HashMap::new();
        for spell in &spells {
            let Some(id) = spell.get("id").and_then(|v| v.as_i64()) else { continue };
            let name = spell.get("name").and_then(|v| v.as_str()).unwrap_or("Unknown");
            let cooldown = spell.get("cooldown").and_then(|v| v.as_u64()).unwrap_or(300) as u32;
            let icon_path = spell.get("iconPath").and_then(|v| v.as_str()).unwrap_or("");

            // iconPath: "/lol-game-data/assets/DATA/Spells/Icons2D/Summoner_boost.png"
            // CDN URL:  CD_BASE + "data/spells/icons2d/summoner_boost.png" (lowercase)
            let icon_url = if let Some(rest) = icon_path.strip_prefix("/lol-game-data/assets/") {
                format!("{}{}", CD_BASE, rest.to_lowercase())
            } else {
                String::new()
            };

            by_id.insert(id as i32, SpellData {
                name: name.to_string(),
                icon_url,
                cooldown,
            });
        }

        if by_id.is_empty() {
            log::error!("summoner-spells.json parsed to an empty table");
            return None;
        }

        log::info!("Loaded {} spells from CDN", by_id.len());
        for (id, data) in &by_id {
            log::debug!("Spell {}: {} -> {}", id, data.name, data.icon_url);
        }
        Some(Self { by_id })
    }

    /// Fallback using hardcoded data from instalock-shared
    fn fallback() -> Self {
        log::warn!("Using fallback spell data");
        let mut by_id = HashMap::new();
        for spell in SPELLS {
            by_id.insert(spell.id, SpellData {
                name: spell.name.to_string(),
                icon_url: String::new(),
                cooldown: spell.base_cooldown,
            });
        }
        Self { by_id }
    }

    pub fn get(&self, id: i32) -> Option<&SpellData> {
        self.by_id.get(&id)
    }

    /// Match a display name (potentially evolved, e.g. "Unleashed Teleport") to a spell ID
    pub fn id_from_display_name(&self, display_name: &str) -> i32 {
        // Hardcoded aliases for evolved/variant spells and ambiguous names
        let lower = display_name.to_lowercase();
        let alias = match lower.as_str() {
            "flash" => Some(4),               // Avoid CDN duplicates (2202/2203 have CD=0)
            "unleashed teleport" => Some(12),  // TP evolved
            "hexflash" => Some(4),             // Flash variant (use Flash icon/CD)
            _ => None,
        };
        if let Some(id) = alias {
            if self.by_id.contains_key(&id) {
                return id;
            }
        }

        // Try exact match first
        for (&id, spell) in &self.by_id {
            if spell.name.to_lowercase() == lower {
                return id;
            }
        }
        // Try contains match (for evolved variants), skip empty names
        for (&id, spell) in &self.by_id {
            let sname = spell.name.to_lowercase();
            if !sname.is_empty() && lower.contains(&sname) {
                return id;
            }
        }
        log::warn!("Unknown spell display name: '{}'", display_name);
        0
    }

    fn spell_name(&self, id: i32) -> String {
        self.get(id).map(|s| s.name.clone()).unwrap_or_else(|| "Unknown".into())
    }

    fn spell_icon(&self, id: i32) -> String {
        self.get(id).map(|s| s.icon_url.clone()).unwrap_or_default()
    }

    fn spell_cooldown(&self, id: i32, has_cosmic: bool) -> u32 {
        let base = self.get(id).map(|s| s.cooldown).unwrap_or(300);
        if has_cosmic {
            base.saturating_sub(instalock_shared::COSMIC_INSIGHT_REDUCTION)
        } else {
            base
        }
    }
}

pub struct OverlayState {
    pub enemies: Mutex<Vec<EnemyData>>,
    pub game_id: Mutex<Option<String>>,
    /// Behind a lock because it starts as the hardcoded fallback and is
    /// replaced once the CDN answers. See `load_spells`.
    pub spells: RwLock<SpellRegistry>,
}

impl OverlayState {
    /// Synchronous, and deliberately so.
    ///
    /// This used to be async and fetch the spell table inline, which meant
    /// `main` blocked on a CDN request with no timeout before any window
    /// existed — on a machine whose network wasn't up yet (autostart) the app
    /// simply didn't appear. Worse, it ran on a `Runtime` that was a temporary,
    /// dropped at the end of the statement, so anything it spawned died at once.
    pub fn new() -> Self {
        Self {
            enemies: Mutex::new(Vec::new()),
            game_id: Mutex::new(None),
            // Cooldowns but no icons: enough to be useful from the first frame.
            spells: RwLock::new(SpellRegistry::fallback()),
        }
    }

    /// Swap the fallback table for the CDN one. Runs on the app's own runtime,
    /// after the window is up, and retries — the same reasoning as
    /// `Champions::load_with_retry`.
    pub async fn load_spells(&self) {
        let mut delay = 2;
        loop {
            if let Some(loaded) = SpellRegistry::load().await {
                *self.spells.write().await = loaded;
                return;
            }
            log::warn!("Spell table unavailable, retrying in {}s", delay);
            tokio::time::sleep(std::time::Duration::from_secs(delay)).await;
            delay = (delay * 2).min(60);
        }
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
    id_to_name: &HashMap<i32, String>,
    spells: &SpellRegistry,
) -> Vec<EnemyData> {
    let Some(their_team) = session.get("theirTeam").and_then(|v| v.as_array()) else {
        return Vec::new();
    };

    log::debug!("extract_enemies: theirTeam has {} players", their_team.len());

    let mut enemies: Vec<(u8, EnemyData)> = their_team
        .iter()
        .filter_map(|player| {
            let champion_id = player.get("championId")?.as_i64()? as i32;
            if champion_id <= 0 {
                return None;
            }
            let spell1_id = player.get("spell1Id").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
            let spell2_id = player.get("spell2Id").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
            let position = player.get("assignedPosition").and_then(|v| v.as_str()).unwrap_or("");
            log::debug!("  champId={} spell1={} spell2={} pos={}", champion_id, spell1_id, spell2_id, position);

            let champion_name = id_to_name
                .get(&champion_id)
                .cloned()
                .unwrap_or_else(|| format!("Champion {}", champion_id));

            let order = role_order(position);

            Some((order, EnemyData {
                champion_id,
                champion_name,
                spell1_name: spells.spell_name(spell1_id),
                spell2_name: spells.spell_name(spell2_id),
                spell1_icon: spells.spell_icon(spell1_id),
                spell2_icon: spells.spell_icon(spell2_id),
                spell1_cooldown: spells.spell_cooldown(spell1_id, false),
                spell2_cooldown: spells.spell_cooldown(spell2_id, false),
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

/// Poll the Live Client Data API to detect runes and spells for enemies
pub async fn poll_live_client_runes(overlay_state: Arc<OverlayState>) {
    // The Live Client Data API is on loopback with a self-signed certificate,
    // so it shares the local client with the LCU.
    let client = crate::http::local();

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

        // Find active player's team
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

        let enemy_team = if active_team == "ORDER" { "CHAOS" } else { "ORDER" };

        let mut enemies = overlay_state.enemies.lock().await;
        let spells = overlay_state.spells.read().await;
        let spells = &*spells;

        let live_enemies: Vec<&serde_json::Value> = all_players
            .iter()
            .filter(|p| p.get("team").and_then(|t| t.as_str()).unwrap_or("") == enemy_team)
            .collect();

        if live_enemies.is_empty() {
            continue;
        }

        // Log raw enemy data
        for player in &live_enemies {
            let name = player.get("championName").and_then(|n| n.as_str()).unwrap_or("?");
            let pos = player.get("position").and_then(|n| n.as_str()).unwrap_or("?");
            let s1 = player.pointer("/summonerSpells/summonerSpellOne/displayName")
                .and_then(|n| n.as_str()).unwrap_or("?");
            let s2 = player.pointer("/summonerSpells/summonerSpellTwo/displayName")
                .and_then(|n| n.as_str()).unwrap_or("?");
            log::info!("LiveClient enemy: {} [{}] spells: {}, {}", name, pos, s1, s2);
        }

        if enemies.is_empty() {
            // Populate from Live Client Data (e.g. practice tool, no champ select)
            for player in &live_enemies {
                let champ_name = player.get("championName")
                    .and_then(|n| n.as_str()).unwrap_or("Unknown").to_string();

                let spell1_display = player.pointer("/summonerSpells/summonerSpellOne/displayName")
                    .and_then(|n| n.as_str()).unwrap_or("");
                let spell2_display = player.pointer("/summonerSpells/summonerSpellTwo/displayName")
                    .and_then(|n| n.as_str()).unwrap_or("");

                let spell1_id = if spell1_display.is_empty() { 0 } else { spells.id_from_display_name(spell1_display) };
                let spell2_id = if spell2_display.is_empty() { 0 } else { spells.id_from_display_name(spell2_display) };

                let has_cosmic = has_rune(player, COSMIC_INSIGHT_ID);
                let has_spellbook = has_rune(player, UNSEALED_SPELLBOOK_ID);

                enemies.push(EnemyData {
                    champion_id: 0,
                    champion_name: champ_name,
                    spell1_name: spell1_display.to_string(),
                    spell2_name: spell2_display.to_string(),
                    spell1_icon: spells.spell_icon(spell1_id),
                    spell2_icon: spells.spell_icon(spell2_id),
                    spell1_cooldown: spells.spell_cooldown(spell1_id, has_cosmic),
                    spell2_cooldown: spells.spell_cooldown(spell2_id, has_cosmic),
                    spell1_id,
                    spell2_id,
                    has_cosmic_insight: has_cosmic,
                    has_unsealed_spellbook: has_spellbook,
                });
            }
        } else {
            // Update existing enemies with spells + runes from Live Client Data
            for player in &live_enemies {
                let champ_name = player.get("championName")
                    .and_then(|n| n.as_str()).unwrap_or("");

                if let Some(enemy) = enemies.iter_mut().find(|e| e.champion_name == champ_name) {
                    let has_cosmic = has_rune(player, COSMIC_INSIGHT_ID);
                    let has_spellbook = has_rune(player, UNSEALED_SPELLBOOK_ID);

                    enemy.has_cosmic_insight = has_cosmic;
                    enemy.has_unsealed_spellbook = has_spellbook;

                    // Update spell IDs from Live Client Data (champ select may have 0s)
                    if let Some(display_name) = player.pointer("/summonerSpells/summonerSpellOne/displayName")
                        .and_then(|n| n.as_str())
                    {
                        let id = spells.id_from_display_name(display_name);
                        enemy.spell1_id = id;
                        enemy.spell1_name = display_name.to_string();
                        enemy.spell1_icon = spells.spell_icon(id);
                    }
                    if let Some(display_name) = player.pointer("/summonerSpells/summonerSpellTwo/displayName")
                        .and_then(|n| n.as_str())
                    {
                        let id = spells.id_from_display_name(display_name);
                        enemy.spell2_id = id;
                        enemy.spell2_name = display_name.to_string();
                        enemy.spell2_icon = spells.spell_icon(id);
                    }

                    enemy.spell1_cooldown = spells.spell_cooldown(enemy.spell1_id, has_cosmic);
                    enemy.spell2_cooldown = spells.spell_cooldown(enemy.spell2_id, has_cosmic);
                }
            }
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
            positions.iter()
                .find(|(name, _)| *name == e.champion_name)
                .map(|(_, order)| *order)
                .unwrap_or(5)
        });

        return;
    }
}

fn has_rune(player: &serde_json::Value, rune_id: i32) -> bool {
    let runes = match player.get("runes") {
        Some(r) => r,
        None => return false,
    };

    if let Some(keystone) = runes.get("keystone").and_then(|k| k.get("id")) {
        if keystone.as_i64() == Some(rune_id as i64) {
            return true;
        }
    }

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

#[cfg(test)]
mod tests {
    use super::*;

    /// Champ select and the Live Client Data API spell positions differently
    /// ("MIDDLE" vs "MID", "UTILITY" vs "SUPPORT"), and both feed this.
    #[test]
    fn roles_sort_top_to_support() {
        let mut roles = ["UTILITY", "TOP", "BOTTOM", "JUNGLE", "MIDDLE"];
        roles.sort_by_key(|r| role_order(r));
        assert_eq!(roles, ["TOP", "JUNGLE", "MIDDLE", "BOTTOM", "UTILITY"]);
    }

    #[test]
    fn the_two_apis_spellings_agree() {
        assert_eq!(role_order("MIDDLE"), role_order("MID"));
        assert_eq!(role_order("BOTTOM"), role_order("ADC"));
        assert_eq!(role_order("UTILITY"), role_order("SUPPORT"));
        // assignedPosition is lowercase in the champ select session.
        assert_eq!(role_order("jungle"), role_order("JUNGLE"));
    }

    /// ARAM and blind pick report no position; those sort last rather than
    /// colliding with TOP at 0.
    #[test]
    fn an_unknown_role_sorts_last() {
        assert!(role_order("") > role_order("UTILITY"));
        assert!(role_order("NONE") > role_order("UTILITY"));
    }

    #[test]
    fn enemies_without_a_champion_are_skipped() {
        let session = serde_json::json!({
            "theirTeam": [
                { "championId": 0,  "spell1Id": 4, "spell2Id": 14, "assignedPosition": "top" },
                { "championId": 64, "spell1Id": 4, "spell2Id": 11, "assignedPosition": "jungle" },
                { "championId": -1, "spell1Id": 4, "spell2Id": 3,  "assignedPosition": "utility" },
            ]
        });
        let mut names = HashMap::new();
        names.insert(64, "Lee Sin".to_string());

        let enemies = extract_enemies(&session, &names, &SpellRegistry::fallback());
        assert_eq!(enemies.len(), 1);
        assert_eq!(enemies[0].champion_name, "Lee Sin");
        assert_eq!(enemies[0].spell2_name, "Smite");
    }

    #[test]
    fn enemies_come_back_in_role_order() {
        let session = serde_json::json!({
            "theirTeam": [
                { "championId": 1, "assignedPosition": "utility" },
                { "championId": 2, "assignedPosition": "top" },
                { "championId": 3, "assignedPosition": "middle" },
            ]
        });
        let enemies = extract_enemies(&session, &HashMap::new(), &SpellRegistry::fallback());
        let ids: Vec<i32> = enemies.iter().map(|e| e.champion_id).collect();
        assert_eq!(ids, vec![2, 3, 1]);
    }

    /// A session with no enemy team at all (ARAM pre-lock, spectator) must be
    /// empty, not a panic.
    #[test]
    fn a_session_without_an_enemy_team_is_empty() {
        let enemies = extract_enemies(
            &serde_json::json!({}),
            &HashMap::new(),
            &SpellRegistry::fallback(),
        );
        assert!(enemies.is_empty());
    }

    #[test]
    fn cosmic_insight_takes_eighteen_seconds_off() {
        let spells = SpellRegistry::fallback();
        // Flash: 300s base.
        assert_eq!(spells.spell_cooldown(4, false), 300);
        assert_eq!(spells.spell_cooldown(4, true), 282);
        // An unknown spell falls back to 300 rather than 0, which would render
        // as a permanently-ready icon.
        assert_eq!(spells.spell_cooldown(9999, false), 300);
    }
}
