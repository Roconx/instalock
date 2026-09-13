use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Mutex;

/// One entry in the champion picker.
#[derive(Clone, Serialize)]
pub struct ChampionOption {
    pub id: i32,
    pub name: String,
}

#[derive(Deserialize)]
struct ChampionEntry {
    id: i32,
    name: String,
    alias: Option<String>,
}

pub struct Champions {
    name_to_id: Mutex<HashMap<String, i32>>,
    id_to_name: Mutex<HashMap<i32, String>>,
    names: Mutex<Vec<String>>,
}

impl Champions {
    pub fn new() -> Self {
        Self {
            name_to_id: Mutex::new(HashMap::new()),
            id_to_name: Mutex::new(HashMap::new()),
            names: Mutex::new(Vec::new()),
        }
    }

    pub async fn load(&self) {
        let url = "https://raw.communitydragon.org/latest/plugins/rcp-be-lol-game-data/global/default/v1/champion-summary.json";

        // The shared CDN client: certificate verification on, and built once
        // rather than rebuilt on every retry.
        let client = crate::http::cdn();

        match client.get(url).send().await {
            Ok(resp) => match resp.json::<Vec<ChampionEntry>>().await {
                Ok(entries) => {
                    let mut name_to_id = self.name_to_id.lock().unwrap();
                    let mut id_to_name = self.id_to_name.lock().unwrap();
                    let mut names = self.names.lock().unwrap();

                    names.clear();
                    id_to_name.clear();
                    name_to_id.clear();

                    for champ in entries {
                        if champ.id == -1 {
                            continue;
                        }
                        names.push(champ.name.clone());
                        id_to_name.insert(champ.id, champ.name.clone());

                        let normalized = normalize(&champ.name);
                        name_to_id.insert(normalized, champ.id);

                        if let Some(alias) = &champ.alias {
                            name_to_id.insert(normalize(alias), champ.id);
                        }
                    }
                    names.sort();
                    log::info!("Loaded {} champions", names.len());
                }
                Err(e) => log::error!("Failed to parse champions: {}", e),
            },
            Err(e) => log::error!("Failed to fetch champions: {}", e),
        }
    }

    /// Keep retrying until the champion table is loaded. Without it a fetch that
    /// fails at startup — common with autostart, which races the network — left
    /// every pick and ban resolving to None for the whole session.
    pub async fn load_with_retry(&self) {
        let mut delay = 2;
        loop {
            self.load().await;
            if !self.names.lock().unwrap().is_empty() {
                return;
            }
            log::warn!("Champion list empty, retrying in {}s", delay);
            tokio::time::sleep(std::time::Duration::from_secs(delay)).await;
            delay = (delay * 2).min(60);
        }
    }

    pub fn resolve_id(&self, input: &str) -> Option<i32> {
        let name_to_id = self.name_to_id.lock().unwrap();
        let normalized = normalize(input);
        if normalized.is_empty() {
            return None;
        }

        // Exact match
        if let Some(&id) = name_to_id.get(&normalized) {
            return Some(id);
        }

        // Prefix match. The old fallback returned the first `contains` hit while
        // iterating a HashMap, whose order is seeded per process — "Kar" banned
        // Karma on one launch and Karthus on the next. Collect every candidate
        // instead and only accept an unambiguous one.
        let mut matches: Vec<(&String, i32)> = name_to_id
            .iter()
            .filter(|(key, _)| key.starts_with(&normalized))
            .map(|(key, &id)| (key, id))
            .collect();

        // Aliases and display names both live in this map, so the same champion
        // can appear twice; that is not an ambiguity.
        matches.sort_by(|a, b| a.0.cmp(b.0));
        matches.dedup_by_key(|(_, id)| *id);

        match matches.as_slice() {
            [(key, id)] => {
                log::info!("Resolved '{}' to '{}' by prefix", input, key);
                Some(*id)
            }
            [] => None,
            ambiguous => {
                let names: Vec<&str> = ambiguous.iter().map(|(k, _)| k.as_str()).collect();
                log::warn!("'{}' is ambiguous, matches {:?} - refusing", input, names);
                None
            }
        }
    }

    pub fn get_names(&self) -> Vec<String> {
        self.names.lock().unwrap().clone()
    }

    /// Name paired with its numeric id, sorted by name. The picker needs the id
    /// to build the Community Dragon icon URL.
    pub fn get_entries(&self) -> Vec<ChampionOption> {
        let id_to_name = self.id_to_name.lock().unwrap();
        let mut out: Vec<ChampionOption> = id_to_name
            .iter()
            .map(|(&id, name)| ChampionOption {
                id,
                name: name.clone(),
            })
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    pub fn get_id_to_name(&self) -> HashMap<i32, String> {
        self.id_to_name.lock().unwrap().clone()
    }
}

/// Champion-name normalization. Public because settings.rs dedupes list entries
/// with it, and because `normalize` in src/main.js must stay byte-identical to
/// this - a name the UI accepts that the backend cannot resolve fails silently
/// at pick time.
pub fn normalize(name: &str) -> String {
    name.to_lowercase()
        .replace(['\'', ' ', '.'], "")
        .replace('&', "and")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a table without touching the network.
    fn table(entries: &[(i32, &str, Option<&str>)]) -> Champions {
        let champs = Champions::new();
        {
            let mut name_to_id = champs.name_to_id.lock().unwrap();
            let mut id_to_name = champs.id_to_name.lock().unwrap();
            let mut names = champs.names.lock().unwrap();
            for (id, name, alias) in entries {
                names.push(name.to_string());
                id_to_name.insert(*id, name.to_string());
                name_to_id.insert(normalize(name), *id);
                if let Some(alias) = alias {
                    name_to_id.insert(normalize(alias), *id);
                }
            }
            names.sort();
        }
        champs
    }

    /// These are the exact cases `normalize` in src/main.js has to agree on.
    /// The two implementations are independent and nothing else checks them.
    #[test]
    fn normalize_matches_the_frontend() {
        assert_eq!(normalize("Kai'Sa"), "kaisa");
        assert_eq!(normalize("Lee Sin"), "leesin");
        assert_eq!(normalize("Dr. Mundo"), "drmundo");
        assert_eq!(normalize("Nunu & Willump"), "nunuandwillump");
        assert_eq!(normalize("K'Sante"), "ksante");
        assert_eq!(normalize("Rek'Sai"), "reksai");
        assert_eq!(normalize(""), "");
    }

    #[test]
    fn resolves_an_exact_name_however_it_is_typed() {
        let champs = table(&[(145, "Kai'Sa", Some("Kaisa"))]);
        for spelling in ["Kai'Sa", "kaisa", "KAI SA", "kai'sa"] {
            assert_eq!(champs.resolve_id(spelling), Some(145), "{}", spelling);
        }
    }

    #[test]
    fn resolves_an_unambiguous_prefix() {
        let champs = table(&[(43, "Karma", None), (30, "Karthus", None)]);
        assert_eq!(champs.resolve_id("Kart"), Some(30));
    }

    /// The old fallback returned the first `contains` hit while iterating a
    /// HashMap, whose order is seeded per process - "Kar" banned Karma on one
    /// launch and Karthus on the next. Refusing is the fix.
    #[test]
    fn refuses_an_ambiguous_prefix_instead_of_guessing() {
        let champs = table(&[(43, "Karma", None), (30, "Karthus", None)]);
        assert_eq!(champs.resolve_id("Kar"), None);
    }

    /// An alias and a display name for the same champion are two keys, not two
    /// candidates - dedup by id has to happen before the ambiguity check.
    #[test]
    fn an_alias_is_not_an_ambiguity() {
        let champs = table(&[(62, "Wukong", Some("MonkeyKing"))]);
        assert_eq!(champs.resolve_id("Wuk"), Some(62));
    }

    #[test]
    fn empty_and_unknown_resolve_to_nothing() {
        let champs = table(&[(43, "Karma", None)]);
        assert_eq!(champs.resolve_id(""), None);
        assert_eq!(champs.resolve_id("   "), None);
        assert_eq!(champs.resolve_id("Zyra"), None);
    }
}
