use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;

/// The mode key holding entries that apply in **every** game mode.
///
/// This is the escape hatch from per-mode memory: "always ban K'Sante, whatever
/// we're playing" lives here, while a Zac picked in Arena stays in Arena.
pub const GLOBAL_MODE: &str = "*";

/// Where entries go when no lobby is open and we have no mode to attribute them
/// to. Summoner's Rift is the resting state of the client.
pub const DEFAULT_MODE: &str = "CLASSIC";

/// The Arena game mode, the only place a Bravery entry can be used.
pub const ARENA_MODE: &str = "CHERRY";

/// The list entry that means "let Arena roll the champion for me".
///
/// Bravery is not a champion and never resolves through the champion table; it
/// is matched by name here and turned into the LCU sentinel below. Keeping it
/// in the list rather than in its own switch is what makes it orderable and
/// one-shottable for free: "Bravery, just this game, otherwise Zac".
pub const BRAVERY_ENTRY: &str = "Bravery";

/// The championId the client itself recognises for a Bravery commit.
pub const BRAVERY_ID: i32 = -3;

pub fn is_bravery(name: &str) -> bool {
    same_champion(name, BRAVERY_ENTRY)
}

/// Whether a role name is one of the five lanes, or something that means "no
/// lane": ARAM, blind pick, and the `default` bucket itself.
fn is_default_role(role: &str) -> bool {
    !matches!(
        role.to_ascii_lowercase().as_str(),
        "top"
            | "jungle"
            | "middle"
            | "mid"
            | "bottom"
            | "bot"
            | "adc"
            | "utility"
            | "support"
            | "sup"
    )
}

/// Whether two list entries name the same champion.
///
/// Goes through the same normalization as `resolve_id`, so "Kai'Sa" and "kaisa"
/// are one entry rather than two that both resolve to the same champion and
/// make the second unreachable.
fn same_champion(a: &str, b: &str) -> bool {
    crate::champions::normalize(a) == crate::champions::normalize(b)
}

/// One entry in a champion priority list.
///
/// No longer a bare name: an entry carries how long it lasts and how widely it
/// applies, which is what lets "just this game" and "in every mode" sit in the
/// same list without a second screen to manage them.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChampionEntry {
    pub name: String,
    /// Only for the next game. Once the game actually starts this entry is
    /// *disabled*, not removed — the order you built survives, and re-arming it
    /// is one click.
    #[serde(default)]
    pub once: bool,
    /// Skipped when choosing. What a spent one-shot becomes, and what the user
    /// can set by hand to park a champion without losing its place.
    #[serde(default)]
    pub disabled: bool,
}

impl ChampionEntry {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            once: false,
            disabled: false,
        }
    }

    /// Whether this entry may be chosen right now.
    pub fn is_active(&self) -> bool {
        !self.disabled && !self.name.trim().is_empty()
    }
}

/// Ordered champion preferences, per assigned role.
///
/// The role names match `assignedPosition` in the champ-select session exactly,
/// which is lowercase. `default` covers ARAM, blind pick, and any role the user
/// left empty — so a single list still works for someone who doesn't care about
/// roles.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RoleLists {
    pub default: Vec<ChampionEntry>,
    pub top: Vec<ChampionEntry>,
    pub jungle: Vec<ChampionEntry>,
    pub middle: Vec<ChampionEntry>,
    pub bottom: Vec<ChampionEntry>,
    pub utility: Vec<ChampionEntry>,
}

impl RoleLists {
    /// The raw list stored against one role, with no `default` mixed in.
    fn own(&self, role: &str) -> &[ChampionEntry] {
        match role.to_ascii_lowercase().as_str() {
            "top" => &self.top,
            "jungle" => &self.jungle,
            // The champ-select session says "middle"; the Live Client API and
            // some queues say "mid". Accept both rather than silently
            // falling through to default.
            "middle" | "mid" => &self.middle,
            "bottom" | "bot" | "adc" => &self.bottom,
            "utility" | "support" | "sup" => &self.utility,
            _ => &self.default,
        }
    }

    /// The list for this role: its own entries first, then `default` as the
    /// safety net beneath them.
    ///
    /// `default` **extends** a role list rather than being replaced by it.
    /// The other way round — role list wins outright, default only used when
    /// the role is empty — meant that adding a single champion to Mid silently
    /// dropped every general fallback, and that adding a global entry to one
    /// role made your other global entries vanish from it. Nothing disappearing
    /// is worth more here than the ability to say "only these, for mid".
    pub fn for_role(&self, role: &str) -> Vec<ChampionEntry> {
        // `default` resolves to itself; concatenating would duplicate it.
        if is_default_role(role) {
            return self.default.clone();
        }

        let mut out = self.own(role).to_vec();
        for fallback in &self.default {
            if !out.iter().any(|e| same_champion(&e.name, &fallback.name)) {
                out.push(fallback.clone());
            }
        }
        out
    }

    fn all_roles_mut(&mut self) -> [&mut Vec<ChampionEntry>; 6] {
        [
            &mut self.default,
            &mut self.top,
            &mut self.jungle,
            &mut self.middle,
            &mut self.bottom,
            &mut self.utility,
        ]
    }

}

/// Champion preferences keyed by LCU `gameMode` — "CLASSIC", "ARAM", "CHERRY",
/// and whatever rotating mode is live this month — plus the [`GLOBAL_MODE`] key.
///
/// **There is no UI for the keys.** The app edits whichever mode you are
/// currently in, so a Zac picked in Arena never turns up in SoloQ, and the user
/// never has to think about it. Per-mode memory can be switched off in
/// settings, in which case everything is written to [`GLOBAL_MODE`] instead.
pub type ModeLists = HashMap<String, RoleLists>;

/// The list actually used for a pick or ban: global entries first, then the
/// ones belonging to the mode being played.
///
/// Globals lead because that is what "always ban K'Sante" means — it is the
/// priority, and the mode-specific entries are what it falls through to.
pub fn effective_list(lists: &ModeLists, mode_key: &str, role: &str) -> Vec<ChampionEntry> {
    let mut out: Vec<ChampionEntry> = lists
        .get(GLOBAL_MODE)
        .map(|l| l.for_role(role))
        .unwrap_or_default();

    if mode_key != GLOBAL_MODE {
        if let Some(mode) = lists.get(mode_key) {
            out.extend(mode.for_role(role));
        }
    }
    out
}

/// Spend every one-shot entry in these lists, across every mode and role.
///
/// Called once the game has actually started. Disabling rather than removing is
/// deliberate: the ordering the user built is often the point, and re-arming a
/// greyed entry is one click, whereas rebuilding a list is not.
pub fn spend_one_shots(lists: &mut ModeLists) -> usize {
    let mut spent = 0;
    for role_lists in lists.values_mut() {
        for entries in role_lists.all_roles_mut() {
            for entry in entries.iter_mut() {
                if entry.once && !entry.disabled {
                    entry.disabled = true;
                    spent += 1;
                }
            }
        }
    }
    spent
}

/// Record a champion that was actually picked or banned, so it can be offered
/// again with one click instead of being typed out.
pub fn push_recent(recents: &mut Vec<String>, name: &str) {
    recents.retain(|n| !n.eq_ignore_ascii_case(name));
    recents.insert(0, name.to_string());
    recents.truncate(MAX_RECENTS);
}

/// Enough to cover a session's worth of champions without the row wrapping
/// past a couple of lines.
pub const MAX_RECENTS: usize = 8;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub auto_accept: bool,
    pub auto_pick: bool,
    pub auto_ban: bool,
    /// Superseded by `pick_lists` / `ban_lists`. Kept so an existing
    /// settings.json still means something on first load — `migrate` seeds the
    /// lists from these, and because the frontend no longer sends them, the
    /// next save clears them for good. Removing them outright would have
    /// silently wiped everyone's configured champion.
    #[serde(default)]
    pub pick_champion: String,
    #[serde(default)]
    pub ban_champion: String,
    #[serde(default)]
    pub pick_lists: ModeLists,
    #[serde(default)]
    pub ban_lists: ModeLists,
    /// Remember preferences separately per game mode. On by default and
    /// invisible: it is what stops an Arena pick turning up in SoloQ. Turning it
    /// off routes everything through `GLOBAL_MODE` instead, so one list serves
    /// every mode.
    #[serde(default = "default_true")]
    pub per_mode_lists: bool,
    /// Champions recently picked or banned, newest first. Offered as one-click
    /// chips so a champion used last game doesn't have to be typed again.
    #[serde(default)]
    pub recent_picks: Vec<String>,
    #[serde(default)]
    pub recent_bans: Vec<String>,
    /// Start the queue by ourselves once the lobby meets `auto_queue_trigger`.
    /// Off by default: nothing should put the user in a queue unasked.
    #[serde(default)]
    pub auto_queue: bool,
    /// "full" | "members" | "ready". See `queue::Trigger`.
    #[serde(default = "default_queue_trigger")]
    pub auto_queue_trigger: String,
    #[serde(default = "default_min_members")]
    pub auto_queue_min_members: u32,
    /// Breathing room between the lobby becoming queueable and the POST, so a
    /// member joining and leaving again doesn't fire a search — and so the user
    /// has a moment to cancel.
    #[serde(default = "default_queue_delay")]
    pub auto_queue_delay_secs: f64,
    /// Skip a champion an ally is already hovering and take the next one.
    /// Off for bans, where the client's own
    /// `disallowBanningTeammateHoveredChampions` governs instead.
    #[serde(default = "default_true")]
    pub avoid_ally_hover: bool,
    #[serde(default)]
    pub bravery_enabled: bool,
    #[serde(default = "default_true")]
    pub restore_focus_after_action: bool,
    #[serde(default)]
    pub hover_pick: bool,
    #[serde(default)]
    pub accept_delay_secs: f64,
    #[serde(default)]
    pub pick_delay_secs: f64,
    #[serde(default)]
    pub ban_delay_secs: f64,
    #[serde(default = "default_margin")]
    pub action_margin_secs: f64,
    // Overlay settings. Off by default: a fresh install should not put a
    // window on top of the game until the user asks for it. Existing
    // settings.json files already carry the key, so they keep their value.
    #[serde(default)]
    pub overlay_enabled: bool,
    #[serde(default = "default_opacity")]
    pub overlay_opacity: f64,
    #[serde(default)]
    pub overlay_x: Option<f64>,
    #[serde(default)]
    pub overlay_y: Option<f64>,
    // Sync settings
    #[serde(default)]
    pub sync_enabled: bool,
    #[serde(default = "default_server_url")]
    pub sync_server_url: String,
    // Appearance settings. All defaulted so settings.json files written by
    // earlier versions keep loading without a migration step.
    #[serde(default = "default_theme")]
    pub theme: String,
    #[serde(default = "default_panel_opacity")]
    pub panel_opacity: f64,
    #[serde(default)]
    pub always_on_top: bool,
    #[serde(default = "default_true")]
    pub minimize_to_tray: bool,
}

fn default_true() -> bool {
    true
}

fn default_queue_trigger() -> String {
    "full".to_string()
}

fn default_min_members() -> u32 {
    2
}

/// Long enough that a member joining and immediately leaving does not fire a
/// search, short enough not to feel broken.
fn default_queue_delay() -> f64 {
    2.0
}

fn default_margin() -> f64 {
    1.5
}

fn default_opacity() -> f64 {
    0.8
}

fn default_server_url() -> String {
    "ws://localhost:9876".to_string()
}

fn default_theme() -> String {
    "dark".to_string()
}

fn default_panel_opacity() -> f64 {
    0.55
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            auto_accept: true,
            auto_pick: false,
            auto_ban: false,
            pick_champion: String::new(),
            ban_champion: String::new(),
            pick_lists: ModeLists::new(),
            // Pre-filled so enabling Auto Ban does something useful straight
            // away, and filed under GLOBAL_MODE so it applies in every mode —
            // which is exactly what a default ban should do. Exact spelling
            // from champion-summary.json (ASCII apostrophe), which is what
            // resolve_id normalizes against.
            ban_lists: ModeLists::from([(
                GLOBAL_MODE.to_string(),
                RoleLists {
                    default: vec![ChampionEntry::new("K'Sante")],
                    ..Default::default()
                },
            )]),
            per_mode_lists: true,
            recent_picks: Vec::new(),
            recent_bans: Vec::new(),
            auto_queue: false,
            auto_queue_trigger: default_queue_trigger(),
            auto_queue_min_members: default_min_members(),
            auto_queue_delay_secs: default_queue_delay(),
            avoid_ally_hover: true,
            bravery_enabled: false,
            hover_pick: false,
            restore_focus_after_action: true,
            accept_delay_secs: 0.0,
            pick_delay_secs: 0.0,
            ban_delay_secs: 0.0,
            action_margin_secs: 1.5,
            overlay_enabled: false,
            overlay_opacity: 0.8,
            overlay_x: None,
            overlay_y: None,
            sync_enabled: false,
            sync_server_url: default_server_url(),
            theme: default_theme(),
            panel_opacity: default_panel_opacity(),
            always_on_top: false,
            minimize_to_tray: true,
        }
    }
}

/// Carry a settings.json written before per-role lists existed forward.
///
/// Deliberately one-way and idempotent: the old single champion becomes the
/// first entry of the default list, and since the frontend no longer sends the
/// legacy fields, the next save writes them back empty and this stops firing.
/// An explicitly emptied list is never repopulated, because the legacy field is
/// gone by then.
fn migrate(settings: &mut Settings) {
    // The old single champion becomes a global entry: it was already the one
    // champion for every mode, so GLOBAL_MODE preserves exactly that meaning.
    let seed = |lists: &mut ModeLists, champion: &str, what: &str| {
        if !lists.is_empty() || champion.is_empty() {
            return;
        }
        log::info!("Migrating {} '{}' to a global list entry", what, champion);
        lists.insert(
            GLOBAL_MODE.to_string(),
            RoleLists {
                default: vec![ChampionEntry::new(champion)],
                ..Default::default()
            },
        );
    };

    let pick_champion = settings.pick_champion.clone();
    let ban_champion = settings.ban_champion.clone();
    seed(&mut settings.pick_lists, &pick_champion, "pickChampion");
    seed(&mut settings.ban_lists, &ban_champion, "banChampion");

    // Bravery used to be its own switch. It is a list entry now, so the old
    // flag becomes the first entry of the Arena pick list - which is exactly
    // what "Bravery is on" meant.
    if settings.bravery_enabled {
        settings.bravery_enabled = false;
        // Under the global key when per-mode lists are off, because that is the
        // only bucket `mode_key` will ever read for such a user - filing it
        // under CHERRY would migrate the setting into a list nothing opens.
        let key = if settings.per_mode_lists {
            ARENA_MODE
        } else {
            GLOBAL_MODE
        };
        let bucket = settings.pick_lists.entry(key.to_string()).or_default();
        if !bucket.default.iter().any(|e| is_bravery(&e.name)) {
            log::info!("Migrating braveryEnabled to a {} pick-list entry", key);
            bucket.default.insert(0, ChampionEntry::new(BRAVERY_ENTRY));
        }
    }
}

impl Settings {
    /// Which mode bucket to read and write right now.
    ///
    /// With per-mode memory off everything collapses onto the global key, so
    /// one list serves every mode and nothing is lost — the per-mode buckets
    /// stay on disk, just unused.
    pub fn mode_key(&self, current_mode: Option<&str>) -> String {
        if !self.per_mode_lists {
            return GLOBAL_MODE.to_string();
        }
        match current_mode {
            Some(mode) if !mode.is_empty() => mode.to_string(),
            // No lobby open: the user is configuring, and Summoner's Rift is
            // the resting state of the client.
            _ => DEFAULT_MODE.to_string(),
        }
    }

    pub fn pick_list_for(&self, current_mode: Option<&str>, role: &str) -> Vec<ChampionEntry> {
        effective_list(&self.pick_lists, &self.mode_key(current_mode), role)
    }

    pub fn ban_list_for(&self, current_mode: Option<&str>, role: &str) -> Vec<ChampionEntry> {
        effective_list(&self.ban_lists, &self.mode_key(current_mode), role)
    }
}

/// Directory holding settings.json.
fn config_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("instalock")
}

pub struct SettingsManager {
    pub settings: Mutex<Settings>,
    path: PathBuf,
}

impl SettingsManager {
    pub fn new() -> Self {
        let path = config_dir().join("settings.json");

        let (settings, migrated) = Self::load_from(&path);

        let manager = Self {
            settings: Mutex::new(settings),
            path,
        };

        // Write a migration straight back out. Leaving it in memory only meant
        // the old flag stayed `true` on disk, so deleting the entry it produced
        // resurrected it on the next launch.
        if migrated {
            manager.save();
        }

        manager
    }

    fn load_from(path: &PathBuf) -> (Settings, bool) {
        let mut settings = match fs::read_to_string(path) {
            Ok(data) => serde_json::from_str(&data).unwrap_or_default(),
            Err(_) => Settings::default(),
        };
        let before = settings.clone();
        migrate(&mut settings);
        let migrated = serde_json::to_string(&before).ok() != serde_json::to_string(&settings).ok();
        (settings, migrated)
    }

    pub fn save(&self) {
        let settings = self.settings.lock().unwrap().clone();
        if let Some(parent) = self.path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let data = serde_json::to_string_pretty(&settings).unwrap();
        let _ = fs::write(&self.path, data);
    }

    pub fn get(&self) -> Settings {
        self.settings.lock().unwrap().clone()
    }

    pub fn update(&self, new_settings: Settings) {
        *self.settings.lock().unwrap() = new_settings;
        self.save();
    }
}

#[cfg(test)]
mod list_rules {
    use super::*;

    fn entries(names: &[&str]) -> Vec<ChampionEntry> {
        names.iter().map(|n| ChampionEntry::new(*n)).collect()
    }

    fn names(list: &[ChampionEntry]) -> Vec<&str> {
        list.iter().map(|e| e.name.as_str()).collect()
    }

    fn lists() -> RoleLists {
        RoleLists {
            default: entries(&["Garen", "Kai'Sa"]),
            middle: entries(&["Ahri"]),
            ..Default::default()
        }
    }

    /// The rule the UI has to mirror exactly: a role's own entries first, then
    /// the Defecte list beneath them.
    #[test]
    fn a_role_list_is_extended_by_default_not_replaced_by_it() {
        assert_eq!(names(&lists().for_role("middle")), ["Ahri", "Garen", "Kai'Sa"]);
    }

    /// Replacing was the old rule, and it meant adding one champion to Mid
    /// silently dropped every general fallback. Nothing may disappear.
    #[test]
    fn adding_to_a_role_does_not_drop_the_general_list() {
        let mut lists = lists();
        lists.middle.push(ChampionEntry::new("Akali"));
        assert_eq!(
            names(&lists.for_role("middle")),
            ["Ahri", "Akali", "Garen", "Kai'Sa"]
        );
    }

    #[test]
    fn a_role_with_no_list_of_its_own_is_just_the_default_list() {
        assert_eq!(names(&lists().for_role("top")), ["Garen", "Kai'Sa"]);
    }

    #[test]
    fn the_default_role_is_not_concatenated_with_itself() {
        assert_eq!(names(&lists().for_role("")), ["Garen", "Kai'Sa"]);
        assert_eq!(names(&lists().for_role("default")), ["Garen", "Kai'Sa"]);
    }

    /// A champion in both keeps the role's own copy - and its position, and its
    /// once/disabled flags - rather than appearing twice.
    #[test]
    fn a_champion_in_both_lists_appears_once_with_the_roles_settings() {
        let lists = RoleLists {
            default: vec![ChampionEntry::new("Ahri"), ChampionEntry::new("Garen")],
            middle: vec![ChampionEntry {
                name: "Ahri".into(),
                once: true,
                disabled: false,
            }],
            ..Default::default()
        };
        let resolved = lists.for_role("middle");
        assert_eq!(names(&resolved), ["Ahri", "Garen"]);
        assert!(resolved[0].once, "the role's own entry wins");
    }

    /// Dedup goes through the same normalization as resolve_id, so two
    /// spellings of one champion don't make the second unreachable.
    #[test]
    fn dedup_ignores_spelling() {
        let lists = RoleLists {
            default: entries(&["kaisa"]),
            middle: entries(&["Kai'Sa"]),
            ..Default::default()
        };
        assert_eq!(names(&lists.for_role("middle")), ["Kai'Sa"]);
    }

    #[test]
    fn lane_names_from_either_api_resolve_the_same() {
        assert_eq!(names(&lists().for_role("MIDDLE")), names(&lists().for_role("mid")));
        assert!(!is_default_role("utility"));
        assert!(!is_default_role("SUPPORT"));
        // ARAM and blind pick report no position at all.
        assert!(is_default_role(""));
        assert!(is_default_role("default"));
    }

    /// The full stack: global bucket + mode bucket, each with its own role and
    /// default lists. This is exactly what the list on screen has to show.
    #[test]
    fn the_effective_list_stacks_globals_then_mode_then_defaults() {
        let lists = ModeLists::from([
            (
                GLOBAL_MODE.to_string(),
                RoleLists {
                    default: entries(&["K'Sante"]),
                    ..Default::default()
                },
            ),
            (
                "CLASSIC".to_string(),
                RoleLists {
                    default: entries(&["Garen"]),
                    middle: entries(&["Ahri"]),
                    ..Default::default()
                },
            ),
        ]);

        assert_eq!(
            names(&effective_list(&lists, "CLASSIC", "middle")),
            ["K'Sante", "Ahri", "Garen"]
        );
        assert_eq!(
            names(&effective_list(&lists, "CLASSIC", "top")),
            ["K'Sante", "Garen"]
        );
    }
}
