use serde::Deserialize;
use std::collections::HashMap;
use std::sync::Mutex;

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

        let client = reqwest::Client::builder()
            .danger_accept_invalid_certs(true)
            .build()
            .unwrap();

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

    pub fn get_id_to_name(&self) -> HashMap<i32, String> {
        self.id_to_name.lock().unwrap().clone()
    }
}

fn normalize(name: &str) -> String {
    name.to_lowercase()
        .replace(['\'', ' ', '.'], "")
        .replace('&', "and")
}
