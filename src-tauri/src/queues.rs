//! The client's own queue table.
//!
//! This replaces a hand-written `queueId -> mode` match, which was wrong within
//! one patch of being written: Riot rotates game-mode codenames constantly
//! (`CLASSIC`, `JADE`, `KIWI`, `KIWI_JADE`, `BRAWL`, `STRAWBERRY`…) and adds
//! queue ids for every seasonal variant. Asking the client means the app is
//! never more out of date than the client it is attached to.
//!
//! It matters more than it looks. When champ select starts, the client
//! **deletes the lobby**, so from that moment the only thing identifying the
//! game is `session.queueId`. A queue id we could not resolve fell back to
//! Summoner's Rift, and the app then picked from the wrong per-mode list — in
//! an Arena draft it would read the SoloQ list and never touch the Arena one.

use crate::lcu::{lcu_request, LcuCredentials};
use serde::Deserialize;
use std::collections::HashMap;
use tokio::sync::RwLock;

/// Howling Abyss. Every ARAM-family queue lives here — plain ARAM, ARAM Clash,
/// ARAM: Mayhem and whatever it is called next patch — and none of them has a
/// pick or a ban phase.
const HOWLING_ABYSS: i64 = 12;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct RawQueue {
    id: i64,
    game_mode: String,
    map_id: i64,
    /// Sometimes empty (queue 4310 ships with no name), hence the fallbacks.
    name: String,
    short_name: String,
    description: String,
}

#[derive(Debug, Clone, Default)]
pub struct QueueInfo {
    pub game_mode: String,
    pub map_id: i64,
    /// What the client calls it, in the client's own language.
    pub name: String,
}

impl QueueInfo {
    /// Whether this queue has a champion select with picks and bans at all.
    ///
    /// Keyed on the map rather than the mode name: map ids are stable across
    /// patches, mode codenames are not.
    pub fn has_champ_select(&self) -> bool {
        if self.map_id == HOWLING_ABYSS {
            return false;
        }
        // Swarm and Teamfight Tactics have no champion select in any sense.
        !matches!(self.game_mode.as_str(), "STRAWBERRY" | "TFT")
            && !self.game_mode.starts_with("TUTORIAL")
    }
}

#[derive(Default)]
pub struct QueueTable {
    by_id: RwLock<HashMap<i64, QueueInfo>>,
}

impl QueueTable {
    pub fn new() -> Self {
        Self::default()
    }

    /// Fetch the table once per client session.
    ///
    /// A failure is not fatal: every caller falls back to the game mode the
    /// lobby already reports, which is enough for everything except naming the
    /// queue and surviving a deleted lobby.
    pub async fn load(&self, creds: &LcuCredentials) {
        let data = match lcu_request(creds, "GET", "/lol-game-queues/v1/queues", None).await {
            Ok(data) => data,
            Err(e) => {
                log::warn!("Could not load the queue table: {}", e);
                return;
            }
        };

        let Some(raw) = data.as_array() else {
            log::warn!("Queue table was not an array");
            return;
        };

        let mut by_id = HashMap::new();
        for value in raw {
            let Ok(q) = serde_json::from_value::<RawQueue>(value.clone()) else {
                continue;
            };
            if q.id <= 0 || q.game_mode.is_empty() {
                continue;
            }
            let name = [&q.name, &q.short_name, &q.description]
                .into_iter()
                .find(|s| !s.is_empty())
                .cloned()
                .unwrap_or_else(|| q.game_mode.clone());
            by_id.insert(
                q.id,
                QueueInfo {
                    game_mode: q.game_mode,
                    map_id: q.map_id,
                    name,
                },
            );
        }

        log::info!("Loaded {} queues from the client", by_id.len());
        *self.by_id.write().await = by_id;
    }

    pub async fn get(&self, queue_id: i64) -> Option<QueueInfo> {
        self.by_id.read().await.get(&queue_id).cloned()
    }

    /// The display name for a queue, or the mode's own name when the table has
    /// not loaded or does not know it.
    pub async fn label(&self, queue_id: i64, game_mode: &str) -> String {
        if let Some(info) = self.get(queue_id).await {
            return info.name;
        }
        fallback_label(game_mode)
    }
}

/// Last resort, for the seconds before the table loads. Deliberately short:
/// anything longer would be a hand-written queue table again.
pub fn fallback_label(game_mode: &str) -> String {
    match game_mode {
        "CLASSIC" => "Summoner's Rift",
        "ARAM" => "ARAM",
        "CHERRY" => "Arena",
        "URF" => "URF",
        "STRAWBERRY" => "Swarm",
        "" => "Desconegut",
        other => other,
    }
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(game_mode: &str, map_id: i64) -> QueueInfo {
        QueueInfo {
            game_mode: game_mode.into(),
            map_id,
            name: String::new(),
        }
    }

    /// Every ARAM variant the client has shipped lives on map 12, whatever it
    /// is called that patch. `KIWI` is ARAM: Mayhem.
    #[test]
    fn the_howling_abyss_never_has_a_pick_or_ban_phase() {
        assert!(!info("ARAM", 12).has_champ_select());
        assert!(!info("KIWI", 12).has_champ_select());
        assert!(!info("KIWI_JADE", 12).has_champ_select());
    }

    #[test]
    fn the_rift_and_arena_do() {
        assert!(info("CLASSIC", 11).has_champ_select());
        assert!(info("CHERRY", 30).has_champ_select());
        // A rift variant under a new codename must not need a code change.
        assert!(info("JADE", 453).has_champ_select());
        assert!(info("BRAWL", 35).has_champ_select());
    }

    #[test]
    fn modes_with_no_champion_select_at_all_are_excluded() {
        assert!(!info("STRAWBERRY", 33).has_champ_select());
        assert!(!info("TFT", 22).has_champ_select());
        assert!(!info("TUTORIAL_MODULE_1", 11).has_champ_select());
    }

    #[test]
    fn a_name_falls_back_through_short_name_and_description() {
        let raw: RawQueue = serde_json::from_value(serde_json::json!({
            "id": 4310, "gameMode": "JADE", "mapId": 453,
            "name": "", "shortName": "Classic", "description": "Classic 5v5",
        }))
        .unwrap();
        let name = [&raw.name, &raw.short_name, &raw.description]
            .into_iter()
            .find(|s| !s.is_empty())
            .cloned()
            .unwrap();
        assert_eq!(name, "Classic");
    }

    #[test]
    fn an_unknown_mode_is_reported_as_itself_rather_than_guessed() {
        assert_eq!(fallback_label("KIWI"), "KIWI");
        assert_eq!(fallback_label(""), "Desconegut");
    }
}
