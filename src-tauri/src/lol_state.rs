//! A typed snapshot of what the League client is currently doing.
//!
//! Everything downstream — auto queue, pick/ban selection, the info panel —
//! reads from here rather than from raw `serde_json::Value`s threaded through
//! the event loop.
//!
//! Two rules govern the types below:
//!
//! 1. **Every field is `#[serde(default)]`.** The LCU is unsupported and
//!    reshuffles fields between patches; a missing one has to degrade to a
//!    default, never fail the whole parse and blind the app mid-champ-select.
//! 2. **Only fields we use are declared.** That is also the privacy mechanism:
//!    `multiUserChatPassword`, `mucJwtDto` and `bustedLeaverAccessToken` are
//!    credentials that arrive in these payloads, and a typed struct drops them
//!    on the floor instead of carrying them into logs.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use tokio::sync::RwLock;

// ── Lobby ──

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Lobby {
    /// The client's own "can I press Find Match" verdict. It already folds in
    /// queue eligibility, premade size, position requirements and penalties, so
    /// nothing here tries to recompute those rules.
    pub can_start_activity: bool,
    pub party_id: String,
    pub members: Vec<LobbyMember>,
    pub local_member: LobbyMember,
    pub game_config: GameConfig,
    /// Why `can_start_activity` is false, when it is.
    pub restrictions: Vec<Restriction>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct LobbyMember {
    /// Routinely exceeds 2^53, so it must never be handed to JavaScript as a
    /// number. `lobby_payload` stringifies it.
    pub summoner_id: u64,
    pub puuid: String,
    pub summoner_name: String,
    pub summoner_level: u32,
    pub summoner_icon_id: i32,
    pub is_leader: bool,
    pub is_spectator: bool,
    pub allowed_start_activity: bool,
    /// `Option` on purpose: absent and false mean different things, and recent
    /// patches have been seen carrying readiness under `memberData` instead.
    pub ready: Option<bool>,
    pub member_data: HashMap<String, serde_json::Value>,
    pub first_position_preference: String,
    pub second_position_preference: String,
}

impl Lobby {
    /// Whether we are the one who can press Find Match.
    ///
    /// `localMember` is the authority, but it has been seen lagging behind the
    /// members array after a host transfer, so the array is consulted too.
    /// Everything that asks this question must ask it here: the info panel and
    /// the queue gate used to compute it differently, and could disagree.
    pub fn i_am_leader(&self) -> bool {
        if self.local_member.is_leader {
            return true;
        }
        let me = self.local_member.summoner_id;
        me != 0 && self.members.iter().any(|m| m.summoner_id == me && m.is_leader)
    }
}

impl LobbyMember {
    /// Whether this member counts as ready.
    ///
    /// Worth knowing what this actually means: in queues with no ready-up step
    /// the client sets `ready` for everyone who is merely *queueable*, and
    /// clears it once searching starts — it is not a per-person button there.
    /// Arena is where it is a real opt-in. The UI says so.
    pub fn is_ready(&self) -> bool {
        self.ready
            .or_else(|| self.member_data.get("ready").and_then(|v| v.as_bool()))
            .unwrap_or(false)
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct GameConfig {
    pub game_mode: String,
    pub queue_id: i64,
    /// The real capacity. `maxHumanPlayers` sits next to it and is 0 in every
    /// matchmade queue, so it is not declared here at all.
    pub max_lobby_size: u32,
    pub max_team_size: u32,
    pub is_custom: bool,
    pub is_lobby_full: bool,
    pub show_position_selector: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Restriction {
    pub restriction_code: String,
    pub expired_timestamp: u64,
}

// ── Matchmaking search ──

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Search {
    /// "Invalid" | "Searching" | "Found" | "Canceled" | "Error" | ...
    pub search_state: String,
    pub is_currently_in_queue: bool,
    /// Seconds. Includes any low-priority penalty, so subtract
    /// `low_priority_data.penalty_time` before comparing with the estimate.
    pub time_in_queue: f64,
    pub estimated_queue_time: f64,
    pub errors: Vec<SearchError>,
    pub low_priority_data: LowPriorityData,
}

impl Search {
    /// Longest penalty still to serve, in seconds. Queuing while this is
    /// positive is refused by the client, so the gate checks it up front
    /// rather than hammering a POST that cannot succeed.
    pub fn penalty_remaining(&self) -> f64 {
        let from_errors = self
            .errors
            .iter()
            .map(|e| e.penalty_time_remaining)
            .fold(0.0_f64, f64::max);
        from_errors.max(self.low_priority_data.penalty_time_remaining)
    }

    pub fn is_searching(&self) -> bool {
        self.is_currently_in_queue || self.search_state == "Searching"
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SearchError {
    pub id: i32,
    pub error_type: String,
    pub penalty_time_remaining: f64,
    pub message: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct LowPriorityData {
    pub penalty_time: f64,
    pub penalty_time_remaining: f64,
    pub reason: String,
}

// ── Champ select ──

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ChampSelect {
    pub local_player_cell_id: i64,
    pub my_team: Vec<PlayerSelection>,
    pub their_team: Vec<PlayerSelection>,
    /// Groups of simultaneous actions, executed in order.
    pub actions: Vec<Vec<Action>>,
    pub bans: Bans,
    pub timer: Timer,
    pub queue_id: i64,
    pub allow_duplicate_picks: bool,
    pub disallow_banning_teammate_hovered_champions: bool,
    pub bench_enabled: bool,
    pub is_custom_game: bool,
    pub is_spectating: bool,
}

impl ChampSelect {
    pub fn me(&self) -> Option<&PlayerSelection> {
        self.my_team
            .iter()
            .find(|p| p.cell_id == self.local_player_cell_id)
    }

    /// The role this game, lowercase, or `""` in ARAM and blind pick.
    pub fn assigned_position(&self) -> &str {
        self.me().map(|p| p.assigned_position.as_str()).unwrap_or("")
    }

    /// My action that is still open, if any.
    ///
    /// Keyed on `completed` rather than `isInProgress`: the latter is unreliable
    /// in modes where a whole group of bans resolves at once, which is exactly
    /// where a missed ban is most expensive.
    pub fn my_open_action(&self) -> Option<&Action> {
        let group = self
            .actions
            .iter()
            .find(|group| !group.iter().all(|a| a.completed))?;
        group
            .iter()
            .find(|a| a.actor_cell_id == self.local_player_cell_id && !a.completed)
    }

    /// My first pick action that has not resolved — not necessarily the one in
    /// progress. This is what a pre-turn hover is PATCHed onto, so teammates can
    /// see the intent during planning.
    pub fn my_first_unfinished_pick(&self) -> Option<&Action> {
        self.actions
            .iter()
            .flatten()
            .find(|a| {
                a.actor_cell_id == self.local_player_cell_id
                    && a.action_type == "pick"
                    && !a.completed
            })
    }

    /// Champions an ally is hovering but has not locked.
    pub fn ally_pick_intents(&self) -> HashSet<i32> {
        self.my_team
            .iter()
            .filter(|p| p.cell_id != self.local_player_cell_id)
            .filter_map(|p| (p.champion_pick_intent > 0).then_some(p.champion_pick_intent))
            .collect()
    }

    /// Every champion already off the table: banned by either team, or locked.
    pub fn unavailable_champions(&self) -> HashSet<i32> {
        let mut out: HashSet<i32> = self
            .bans
            .my_team_bans
            .iter()
            .chain(self.bans.their_team_bans.iter())
            .copied()
            .filter(|id| *id > 0)
            .collect();
        for player in self.my_team.iter().chain(self.their_team.iter()) {
            if player.champion_id > 0 && player.cell_id != self.local_player_cell_id {
                out.insert(player.champion_id);
            }
        }
        out
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PlayerSelection {
    pub cell_id: i64,
    /// 0 until locked.
    pub champion_id: i32,
    /// 0 when not hovering; otherwise the champion being hovered.
    pub champion_pick_intent: i32,
    /// Lowercase: "top" | "jungle" | "middle" | "bottom" | "utility", or "".
    pub assigned_position: String,
    pub spell1_id: i32,
    pub spell2_id: i32,
    pub team: i32,
    pub is_autofilled: bool,
    /// "HIDDEN" in ranked solo, where the enemy team carries no identity at all.
    pub name_visibility_type: String,
    pub game_name: String,
    pub tag_line: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Action {
    pub id: i64,
    pub actor_cell_id: i64,
    pub champion_id: i32,
    #[serde(rename = "type")]
    pub action_type: String,
    pub completed: bool,
    pub is_in_progress: bool,
    pub is_ally_action: bool,
    pub pick_turn: i32,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Bans {
    pub my_team_bans: Vec<i32>,
    pub their_team_bans: Vec<i32>,
    pub num_bans: i32,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Timer {
    /// Milliseconds.
    pub adjusted_time_left_in_phase: f64,
    pub total_time_in_phase: f64,
    /// "PLANNING" | "BAN_PICK" | "FINALIZATION" | "GAME_STARTING"
    pub phase: String,
    pub is_infinite: bool,
    pub internal_now_in_epoch_ms: f64,
}

impl Timer {
    /// Seconds left, corrected for how long ago the client sent this.
    ///
    /// The session only updates on events, so between pushes
    /// `adjusted_time_left_in_phase` is stale — a lock scheduled for "2s before
    /// the timer expires" straight off the raw field lands late.
    pub fn seconds_left(&self, now_epoch_ms: f64) -> f64 {
        if self.is_infinite {
            return f64::INFINITY;
        }
        let elapsed = if self.internal_now_in_epoch_ms > 0.0 {
            (now_epoch_ms - self.internal_now_in_epoch_ms).max(0.0)
        } else {
            0.0
        };
        ((self.adjusted_time_left_in_phase - elapsed) / 1000.0).max(0.0)
    }
}

// ── Champion grid ──

/// Per-champion live state during champ select: who has it hovered, whether it
/// is banned, whether it is already taken.
///
/// `pickable-champion-ids` does not carry any of this — it is the server's hard
/// rule set (ownership, free rotation, queue restrictions) and nothing more.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ChampionSelection {
    pub selected_by_me: bool,
    pub ban_intented: bool,
    pub ban_intented_by_me: bool,
    pub is_banned: bool,
    pub pick_intented: bool,
    pub pick_intented_by_me: bool,
    pub picked_by_other_or_banned: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct GridChampion {
    pub id: i32,
    pub name: String,
    pub owned: bool,
    pub disabled: bool,
    pub free_to_play: bool,
    pub selection_status: ChampionSelection,
}

// ── The snapshot ──

/// Everything the app knows about the client right now.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub phase: String,
    pub lobby: Option<Lobby>,
    pub search: Option<Search>,
    pub champ_select: Option<ChampSelect>,
    /// The server's hard rule set: already accounts for ownership, free
    /// rotation, queue and mode restrictions. Empty means "not known yet",
    /// which the selection logic treats as "don't filter on this".
    pub pickable: HashSet<i32>,
    /// Includes `-1`, the empty-ban sentinel, when an empty ban is legal.
    pub bannable: HashSet<i32>,
    pub grid: HashMap<i32, ChampionSelection>,
}

impl Snapshot {
    /// Whether we are actually sitting in the queue waiting for a match.
    ///
    /// `isCurrentlyInQueue` is not that question. The client leaves it `true`
    /// through champ select and the game itself - it means "this lobby has a
    /// match in flight" - so without the phase the UI announces "Buscant
    /// partida" in the middle of a draft.
    pub fn is_searching(&self) -> bool {
        if !matches!(
            self.phase.as_str(),
            "" | "None" | "Lobby" | "Matchmaking" | "ReadyCheck"
        ) {
            return false;
        }
        self.search.as_ref().map(|s| s.is_searching()).unwrap_or(false)
    }

    /// Whether the champion grid has been loaded at all. Until it has, hover and
    /// ban state is unknown and the session's own fields are the fallback.
    pub fn has_grid(&self) -> bool {
        !self.grid.is_empty()
    }

    pub fn selection(&self, champion_id: i32) -> Option<&ChampionSelection> {
        self.grid.get(&champion_id)
    }
}

/// Shared, mutable, one writer per event. A `RwLock` rather than a `Mutex`
/// because the read side (selection, the info panel, the queue gate) runs far
/// more often than the write side.
#[derive(Default)]
pub struct LolState {
    inner: RwLock<Snapshot>,
}

impl LolState {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn get(&self) -> Snapshot {
        self.inner.read().await.clone()
    }

    pub async fn set_phase(&self, phase: &str) {
        self.inner.write().await.phase = phase.to_string();
    }

    /// `None` is the lobby's Delete event, which arrives with a null payload.
    pub async fn set_lobby(&self, lobby: Option<Lobby>) {
        self.inner.write().await.lobby = lobby;
    }

    pub async fn set_search(&self, search: Option<Search>) {
        self.inner.write().await.search = search;
    }

    pub async fn set_champ_select(&self, session: Option<ChampSelect>) {
        self.inner.write().await.champ_select = session;
    }

    pub async fn set_pickable(&self, ids: HashSet<i32>) {
        self.inner.write().await.pickable = ids;
    }

    pub async fn set_bannable(&self, ids: HashSet<i32>) {
        self.inner.write().await.bannable = ids;
    }

    pub async fn set_grid(&self, grid: HashMap<i32, ChampionSelection>) {
        self.inner.write().await.grid = grid;
    }

    /// Apply one champion's push update, leaving the rest of the grid alone.
    pub async fn update_grid_champion(&self, id: i32, selection: ChampionSelection) {
        self.inner.write().await.grid.insert(id, selection);
    }

    /// Drop everything that belongs to one game. Called when champ select ends,
    /// so a stale hover from the previous draft can't influence the next one.
    pub async fn clear_champ_select(&self) {
        let mut inner = self.inner.write().await;
        inner.champ_select = None;
        inner.pickable.clear();
        inner.bannable.clear();
        inner.grid.clear();
    }
}

/// Parse a payload, logging rather than propagating a shape we don't recognise.
///
/// Every LCU payload goes through here, so a patch that renames a field costs a
/// warning in the log and a degraded feature, not a panic.
pub fn parse<T: serde::de::DeserializeOwned>(what: &str, value: &serde_json::Value) -> Option<T> {
    match serde_json::from_value::<T>(value.clone()) {
        Ok(parsed) => Some(parsed),
        Err(e) => {
            log::warn!("Could not parse {}: {}", what, e);
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn champ_select(json: serde_json::Value) -> ChampSelect {
        serde_json::from_value(json).expect("champ select should parse")
    }

    /// A payload missing half its fields still has to parse — that is the whole
    /// point of defaulting everything.
    #[test]
    fn a_sparse_payload_still_parses() {
        let session = champ_select(serde_json::json!({ "localPlayerCellId": 3 }));
        assert_eq!(session.local_player_cell_id, 3);
        assert!(session.my_team.is_empty());
        assert_eq!(session.timer.phase, "");
    }

    #[test]
    fn unknown_fields_are_ignored() {
        let session = champ_select(serde_json::json!({
            "localPlayerCellId": 1,
            "someFieldRiotAddedLastPatch": { "nested": true },
        }));
        assert_eq!(session.local_player_cell_id, 1);
    }

    #[test]
    fn finds_my_role_and_my_open_action() {
        let session = champ_select(serde_json::json!({
            "localPlayerCellId": 2,
            "myTeam": [
                { "cellId": 1, "assignedPosition": "top" },
                { "cellId": 2, "assignedPosition": "middle" },
            ],
            "actions": [
                [ { "id": 10, "actorCellId": 1, "type": "ban", "completed": true } ],
                [ { "id": 11, "actorCellId": 2, "type": "pick", "completed": false } ],
            ],
        }));
        assert_eq!(session.assigned_position(), "middle");
        let action = session.my_open_action().unwrap();
        assert_eq!(action.id, 11);
        assert_eq!(action.action_type, "pick");
    }

    /// In simultaneous-ban modes `isInProgress` lies, so selection keys off
    /// `completed`. An action of mine that is not flagged in progress must
    /// still be found.
    #[test]
    fn an_open_action_is_found_without_is_in_progress() {
        let session = champ_select(serde_json::json!({
            "localPlayerCellId": 0,
            "actions": [[
                { "id": 5, "actorCellId": 0, "type": "ban", "completed": false, "isInProgress": false },
                { "id": 6, "actorCellId": 1, "type": "ban", "completed": false },
            ]],
        }));
        assert_eq!(session.my_open_action().unwrap().id, 5);
    }

    #[test]
    fn a_fully_completed_group_is_skipped() {
        let session = champ_select(serde_json::json!({
            "localPlayerCellId": 0,
            "actions": [
                [ { "id": 1, "actorCellId": 0, "type": "ban", "completed": true } ],
                [ { "id": 2, "actorCellId": 0, "type": "pick", "completed": false } ],
            ],
        }));
        assert_eq!(session.my_open_action().unwrap().id, 2);
    }

    #[test]
    fn the_pre_turn_hover_target_is_my_first_unresolved_pick() {
        let session = champ_select(serde_json::json!({
            "localPlayerCellId": 4,
            "actions": [
                [ { "id": 1, "actorCellId": 4, "type": "ban",  "completed": true } ],
                [ { "id": 2, "actorCellId": 0, "type": "pick", "completed": false } ],
                [ { "id": 3, "actorCellId": 4, "type": "pick", "completed": false } ],
            ],
        }));
        // Not the in-progress action (id 2, someone else's) - mine, id 3.
        assert_eq!(session.my_first_unfinished_pick().unwrap().id, 3);
    }

    #[test]
    fn collects_ally_hovers_but_not_my_own() {
        let session = champ_select(serde_json::json!({
            "localPlayerCellId": 1,
            "myTeam": [
                { "cellId": 0, "championPickIntent": 64 },
                { "cellId": 1, "championPickIntent": 103 },
                { "cellId": 2, "championPickIntent": 0 },
            ],
        }));
        let intents = session.ally_pick_intents();
        assert!(intents.contains(&64));
        assert!(!intents.contains(&103), "my own hover is not a conflict");
        assert_eq!(intents.len(), 1);
    }

    #[test]
    fn unavailable_covers_bans_and_locks_on_both_teams() {
        let session = champ_select(serde_json::json!({
            "localPlayerCellId": 1,
            "myTeam": [
                { "cellId": 0, "championId": 64 },
                { "cellId": 1, "championId": 103 },
            ],
            "theirTeam": [ { "cellId": 5, "championId": 157 } ],
            "bans": { "myTeamBans": [1, 0], "theirTeamBans": [2, -1] },
        }));
        let taken = session.unavailable_champions();
        assert!(taken.contains(&64), "ally lock");
        assert!(taken.contains(&157), "enemy lock");
        assert!(taken.contains(&1) && taken.contains(&2), "bans");
        assert!(!taken.contains(&103), "my own lock is not a conflict");
        assert!(!taken.contains(&0) && !taken.contains(&-1), "empty ban slots");
    }

    /// The raw field is milliseconds and goes stale between pushes; callers work
    /// in seconds and need the drift taken off.
    #[test]
    fn the_timer_is_corrected_for_drift() {
        let timer: Timer = serde_json::from_value(serde_json::json!({
            "adjustedTimeLeftInPhase": 30_000.0,
            "internalNowInEpochMs": 1_000_000.0,
            "phase": "BAN_PICK",
        }))
        .unwrap();

        // Read at the instant the client sent it: the full 30s.
        assert!((timer.seconds_left(1_000_000.0) - 30.0).abs() < 0.001);
        // Read 5s later: 25s.
        assert!((timer.seconds_left(1_005_000.0) - 25.0).abs() < 0.001);
        // Long past expiry: clamped, never negative.
        assert_eq!(timer.seconds_left(9_000_000.0), 0.0);
    }

    #[test]
    fn an_infinite_timer_never_forces_an_action() {
        let timer = Timer {
            is_infinite: true,
            ..Default::default()
        };
        assert!(timer.seconds_left(0.0).is_infinite());
    }

    #[test]
    fn readiness_falls_back_to_member_data() {
        let plain: LobbyMember =
            serde_json::from_value(serde_json::json!({ "ready": true })).unwrap();
        assert!(plain.is_ready());

        let nested: LobbyMember =
            serde_json::from_value(serde_json::json!({ "memberData": { "ready": true } })).unwrap();
        assert!(nested.is_ready(), "newer patches nest it under memberData");

        let absent: LobbyMember = serde_json::from_value(serde_json::json!({})).unwrap();
        assert!(!absent.is_ready());
    }

    /// A lobby with no gameConfig at all must parse to an empty mode rather
    /// than failing: the client pushes partial lobby payloads, and a failed
    /// parse there used to be read as "no mode" and reported as Summoner's
    /// Rift over an Arena lobby.
    #[test]
    fn a_lobby_without_a_game_config_still_parses() {
        let lobby: Lobby = serde_json::from_value(serde_json::json!({
            "canStartActivity": false,
            "members": [],
        }))
        .unwrap();
        assert_eq!(lobby.game_config.game_mode, "");
        assert_eq!(lobby.game_config.queue_id, 0);
    }

    /// The lobby payload carries chat credentials. Nothing declares them, so
    /// nothing can leak them into a log line.
    #[test]
    fn credentials_in_the_lobby_payload_are_not_retained() {
        let lobby: Lobby = serde_json::from_value(serde_json::json!({
            "canStartActivity": true,
            "multiUserChatPassword": "hunter2",
            "mucJwtDto": { "jwt": "secret" },
            "gameConfig": { "queueId": 420, "maxLobbySize": 5 },
        }))
        .unwrap();

        assert!(lobby.can_start_activity);
        assert_eq!(lobby.game_config.max_lobby_size, 5);
        assert!(!format!("{:?}", lobby).contains("hunter2"));
        assert!(!format!("{:?}", lobby).contains("secret"));
    }

    #[test]
    fn the_worst_penalty_wins() {
        let search: Search = serde_json::from_value(serde_json::json!({
            "searchState": "Error",
            "errors": [
                { "id": 1, "errorType": "QUEUE_DODGER", "penaltyTimeRemaining": 120.0 },
                { "id": 2, "errorType": "LEAVER_BUSTED", "penaltyTimeRemaining": 300.0 },
            ],
            "lowPriorityData": { "penaltyTimeRemaining": 60.0 },
        }))
        .unwrap();
        assert_eq!(search.penalty_remaining(), 300.0);
        assert!(!search.is_searching());
    }

    /// The client leaves `isCurrentlyInQueue` true through champ select and the
    /// whole game. Reading it without the phase is what put "Buscant partida"
    /// on screen in the middle of a draft.
    #[test]
    fn a_match_already_found_is_not_still_searching() {
        let search: Search = serde_json::from_value(serde_json::json!({
            "isCurrentlyInQueue": true,
            "estimatedQueueTime": 46.7,
        }))
        .unwrap();

        let mut snap = Snapshot {
            search: Some(search),
            ..Default::default()
        };

        snap.phase = "Matchmaking".into();
        assert!(snap.is_searching());

        for phase in ["ChampSelect", "InProgress", "EndOfGame"] {
            snap.phase = phase.into();
            assert!(!snap.is_searching(), "{} is not the queue", phase);
        }
    }

    #[test]
    fn a_clean_search_has_no_penalty() {
        let search: Search = serde_json::from_value(serde_json::json!({
            "searchState": "Searching",
            "isCurrentlyInQueue": true,
            "estimatedQueueTime": 130.9,
        }))
        .unwrap();
        assert_eq!(search.penalty_remaining(), 0.0);
        assert!(search.is_searching());
    }
}
