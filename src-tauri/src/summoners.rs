//! Turning a puuid into a name you can read.
//!
//! The lobby resource used to carry `summonerName`. It does not any more — on a
//! current client every member arrives with `summonerName: ""` and nothing else
//! but a puuid, because Riot moved to Riot IDs and the name now lives on the
//! summoner resource. Anything that wanted to show who is in the lobby had to
//! fall back to "Invocador" for everyone.
//!
//! So: resolve once per puuid and remember it. Names change rarely, the lookup
//! is a loopback request, and a lobby is at most five people.

use crate::lcu::{lcu_request, LcuCredentials};
use serde::Deserialize;
use std::collections::HashMap;
use tokio::sync::RwLock;

/// Enough entries for a long session's worth of lobbies and champ selects
/// without the map being something to think about.
const MAX_CACHED: usize = 256;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct SummonerResponse {
    game_name: String,
    tag_line: String,
    /// Deprecated and empty on current clients; read last, for old ones.
    display_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RiotId {
    /// What to show in a list.
    pub name: String,
    /// `Name#TAG`, for the tooltip — two people can share a game name.
    pub full: String,
}

#[derive(Default)]
pub struct SummonerNames {
    by_puuid: RwLock<HashMap<String, RiotId>>,
}

impl SummonerNames {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn get(&self, puuid: &str) -> Option<RiotId> {
        self.by_puuid.read().await.get(puuid).cloned()
    }

    /// Look up every puuid we do not already know.
    ///
    /// Returns true if anything new was learned, so the caller can re-emit
    /// rather than push a payload that has not changed.
    pub async fn resolve_missing(&self, creds: &LcuCredentials, puuids: &[String]) -> bool {
        let mut wanted: Vec<String> = Vec::new();
        {
            let known = self.by_puuid.read().await;
            for puuid in puuids {
                if !puuid.is_empty() && !known.contains_key(puuid) && !wanted.contains(puuid) {
                    wanted.push(puuid.clone());
                }
            }
        }
        if wanted.is_empty() {
            return false;
        }

        let mut learned = Vec::new();
        for puuid in wanted {
            let url = format!("/lol-summoner/v2/summoners/puuid/{}", puuid);
            let Ok(data) = lcu_request(creds, "GET", &url, None).await else {
                // A summoner the client cannot see (privacy, or an account that
                // has left) is not an error worth reporting every poll.
                log::debug!("Could not resolve summoner {}", puuid);
                continue;
            };
            let Ok(parsed) = serde_json::from_value::<SummonerResponse>(data) else {
                continue;
            };
            if let Some(id) = riot_id(&parsed) {
                learned.push((puuid, id));
            }
        }

        if learned.is_empty() {
            return false;
        }

        let mut by_puuid = self.by_puuid.write().await;
        // Cheap bound: a session that somehow sees hundreds of players starts
        // over rather than growing without limit.
        if by_puuid.len() + learned.len() > MAX_CACHED {
            by_puuid.clear();
        }
        for (puuid, id) in learned {
            by_puuid.insert(puuid, id);
        }
        true
    }
}

/// `gameName` first, then the deprecated `displayName` for older clients.
fn riot_id(s: &SummonerResponse) -> Option<RiotId> {
    let name = if !s.game_name.is_empty() {
        s.game_name.clone()
    } else if !s.display_name.is_empty() {
        s.display_name.clone()
    } else {
        return None;
    };

    let full = if s.tag_line.is_empty() {
        name.clone()
    } else {
        format!("{}#{}", name, s.tag_line)
    };

    Some(RiotId { name, full })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(json: serde_json::Value) -> SummonerResponse {
        serde_json::from_value(json).unwrap()
    }

    #[test]
    fn a_current_client_answers_with_a_riot_id() {
        let id = riot_id(&parse(serde_json::json!({
            "gameName": "Assembly Enjoyer",
            "tagLine": "HolyC",
            "displayName": "",
        })))
        .unwrap();
        assert_eq!(id.name, "Assembly Enjoyer");
        assert_eq!(id.full, "Assembly Enjoyer#HolyC");
    }

    /// Older clients filled `displayName` and left the Riot ID fields empty.
    #[test]
    fn an_older_client_still_resolves() {
        let id = riot_id(&parse(serde_json::json!({ "displayName": "Faker" }))).unwrap();
        assert_eq!(id.name, "Faker");
        assert_eq!(id.full, "Faker", "no tag to append");
    }

    /// A response with no name at all must not be cached as an empty string —
    /// the caller falls back to its own placeholder instead.
    #[test]
    fn a_nameless_response_resolves_to_nothing() {
        assert!(riot_id(&parse(serde_json::json!({}))).is_none());
        assert!(riot_id(&parse(serde_json::json!({ "tagLine": "EUW" }))).is_none());
    }
}
