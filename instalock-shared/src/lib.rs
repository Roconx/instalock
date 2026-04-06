use serde::{Deserialize, Serialize};

/// Summoner spell static data
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct SpellInfo {
    pub id: i32,
    pub name: &'static str,
    pub base_cooldown: u32,
    pub icon_key: &'static str,
}

pub const SPELLS: &[SpellInfo] = &[
    SpellInfo { id: 4, name: "Flash", base_cooldown: 300, icon_key: "summoner_flash" },
    SpellInfo { id: 7, name: "Heal", base_cooldown: 240, icon_key: "summoner_heal" },
    SpellInfo { id: 6, name: "Ghost", base_cooldown: 210, icon_key: "summoner_haste" },
    SpellInfo { id: 21, name: "Barrier", base_cooldown: 180, icon_key: "summoner_barrier" },
    SpellInfo { id: 3, name: "Exhaust", base_cooldown: 210, icon_key: "summoner_exhaust" },
    SpellInfo { id: 14, name: "Ignite", base_cooldown: 180, icon_key: "summoner_dot" },
    SpellInfo { id: 1, name: "Cleanse", base_cooldown: 210, icon_key: "summoner_boost" },
    SpellInfo { id: 12, name: "Teleport", base_cooldown: 360, icon_key: "summoner_teleport" },
    SpellInfo { id: 11, name: "Smite", base_cooldown: 90, icon_key: "summoner_smite" },
    SpellInfo { id: 32, name: "Mark", base_cooldown: 80, icon_key: "summoner_snowball" },
];

/// Cosmic Insight rune ID — reduces summoner spell CD by 18s flat
pub const COSMIC_INSIGHT_ID: i32 = 8347;
pub const COSMIC_INSIGHT_REDUCTION: u32 = 18;

/// Unsealed Spellbook keystone ID
pub const UNSEALED_SPELLBOOK_ID: i32 = 8360;

pub fn spell_by_id(id: i32) -> Option<&'static SpellInfo> {
    SPELLS.iter().find(|s| s.id == id)
}

pub fn spell_cooldown(spell_id: i32, has_cosmic_insight: bool) -> u32 {
    let base = spell_by_id(spell_id).map(|s| s.base_cooldown).unwrap_or(300);
    if has_cosmic_insight {
        base.saturating_sub(COSMIC_INSIGHT_REDUCTION)
    } else {
        base
    }
}

// ── Sync protocol messages ──

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum SyncMessage {
    /// Client joins a room
    #[serde(rename = "join")]
    Join { game_id: String, player_name: String },
    /// A summoner spell timer was started
    #[serde(rename = "timer_start")]
    TimerStart {
        enemy_idx: u8,
        spell_idx: u8,
        cooldown_secs: u32,
        started_at: u64,
    },
    /// A summoner spell timer was cancelled
    #[serde(rename = "timer_cancel")]
    TimerCancel { enemy_idx: u8, spell_idx: u8 },
    /// Server sends current room state on join
    #[serde(rename = "room_state")]
    RoomState { timers: Vec<ActiveTimer> },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActiveTimer {
    pub enemy_idx: u8,
    pub spell_idx: u8,
    pub cooldown_secs: u32,
    pub started_at: u64,
}
