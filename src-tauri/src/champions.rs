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

                    for champ in entries {
                        if champ.id == -1 {
                            continue;
                        }
                        names.push(champ.name.clone());
                        id_to_name.insert(champ.id, champ.name.clone());

                        let normalized = normalize(&champ.name);
                        name_to_id.insert(normalized, champ.id);

                        if let Some(alias) = &champ.alias {
                            name_to_id.insert(alias.to_lowercase(), champ.id);
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

    pub fn resolve_id(&self, input: &str) -> Option<i32> {
        let name_to_id = self.name_to_id.lock().unwrap();
        let normalized = normalize(input);

        // Exact match
        if let Some(&id) = name_to_id.get(&normalized) {
            return Some(id);
        }

        // Partial match
        for (key, &id) in name_to_id.iter() {
            if key.contains(&normalized) {
                return Some(id);
            }
        }

        None
    }

    pub fn get_names(&self) -> Vec<String> {
        self.names.lock().unwrap().clone()
    }
}

fn normalize(name: &str) -> String {
    name.to_lowercase()
        .replace(['\'', ' ', '.'], "")
        .replace('&', "and")
}
