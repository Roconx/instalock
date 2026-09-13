//! Deciding whether to start matchmaking, and why not when not.
//!
//! The one rule that matters here: **`lobby.canStartActivity` is not
//! reimplemented.** The client already folds queue eligibility, premade size,
//! position requirements, level and rank restrictions and penalty timers into
//! that one boolean. Everything below it is either a check the client cannot
//! make for us (are we the leader, has the user's own trigger been met) or a
//! check that exists purely to produce a better message than "no".
//!
//! Pure, like `selection`: a snapshot and the settings go in, a verdict comes
//! out. No network, no shared state.

use crate::lol_state::Snapshot;
use crate::settings::Settings;

/// How the user wants the queue started.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    /// Every seat filled. For solo queue the lobby is "full" at one.
    Full,
    /// A configurable headcount, for duos and flex.
    Members,
    /// Every member flagged ready.
    Ready,
}

impl Trigger {
    pub fn parse(value: &str) -> Self {
        match value {
            "members" => Trigger::Members,
            "ready" => Trigger::Ready,
            _ => Trigger::Full,
        }
    }
}

/// Why we are or aren't starting a search.
#[derive(Debug, Clone, PartialEq)]
pub enum Gate {
    Ready,
    Disabled,
    NoLobby,
    /// Custom games have no matchmaking to start.
    CustomGame,
    /// Not in the lobby phase — already searching, in champ select, in a game.
    WrongPhase(String),
    AlreadySearching,
    /// A non-leader cannot start a search; the client rejects it outright.
    NotLeader,
    /// A dodge or leaver penalty still running. Queuing now cannot succeed.
    Penalty { secs: f64 },
    /// The client says no and named a reason.
    Restricted(String),
    /// The user's own trigger has not been met yet.
    WaitingForMembers { have: usize, need: usize },
    NotEveryoneReady { ready: usize, total: usize },
    /// `canStartActivity` is false and nothing told us why.
    ClientRefuses,
    /// The user left the queue by hand. Auto queue holds off until the lobby
    /// itself changes, because re-queueing someone who just cancelled is the
    /// one failure mode that makes the feature unusable.
    CancelledByUser,
}

impl Gate {
    pub fn is_ready(&self) -> bool {
        matches!(self, Gate::Ready)
    }

    /// Whether this is worth telling the user about.
    ///
    /// "Waiting for a fifth" is the normal state of a lobby and would spam the
    /// log; a penalty or a restriction is something they need to know.
    pub fn is_noteworthy(&self) -> bool {
        matches!(
            self,
            Gate::Penalty { .. } | Gate::Restricted(_) | Gate::ClientRefuses | Gate::NotLeader
        )
    }

    /// Catalan, for the log and the info panel.
    pub fn describe(&self) -> String {
        match self {
            Gate::Ready => "A punt per encuar".into(),
            Gate::Disabled => "Auto cerca desactivada".into(),
            Gate::NoLobby => "Cap sala oberta".into(),
            Gate::CustomGame => "Partida personalitzada: no hi ha cua".into(),
            Gate::WrongPhase(phase) => format!("No s'està a la sala ({})", phase),
            Gate::AlreadySearching => "Ja s'està buscant partida".into(),
            Gate::NotLeader => "No ets el líder de la sala".into(),
            Gate::Penalty { secs } => {
                format!("Penalització activa: {}", format_duration(*secs))
            }
            Gate::Restricted(code) => format!("El client no deixa encuar: {}", restriction(code)),
            Gate::WaitingForMembers { have, need } => {
                format!("Esperant jugadors ({}/{})", have, need)
            }
            Gate::NotEveryoneReady { ready, total } => {
                format!("Falta gent per estar preparada ({}/{})", ready, total)
            }
            Gate::ClientRefuses => "El client encara no deixa encuar".into(),
            Gate::CancelledByUser => {
                "Has sortit de la cua. Torna a activar l'interruptor per encuar sol.".into()
            }
        }
    }
}

fn format_duration(secs: f64) -> String {
    let total = secs.max(0.0).round() as u64;
    if total >= 60 {
        format!("{} min {} s", total / 60, total % 60)
    } else {
        format!("{} s", total)
    }
}

/// Catalan for the restriction codes worth naming. Anything else falls back to
/// the raw code — better an untranslated token than a silent "no".
fn restriction(code: &str) -> &str {
    match code {
        "PlayerDodgeRestriction" => "penalització per dodge",
        "PlayerLeaverBustedRestriction" | "PlayerLeaverQueueLockoutRestriction" => {
            "penalització per abandonar partides"
        }
        "PlayerReadyCheckFailRestriction" => "no has acceptat una partida",
        "PlayerMinLevelRestriction" => "nivell insuficient per a aquesta cua",
        "QueueDisabled" => "cua desactivada",
        "TeamMinSizeRestriction" => "falta gent a la sala",
        "TeamMaxSizeRestriction" => "hi ha massa gent a la sala",
        "FullPartyUnranked" => "el grup no pot jugar aquesta cua",
        "QPInvalidPositionSelectionRestriction" | "QPPartyPositionCoverageRestriction" => {
            "falten posicions per triar"
        }
        "QPInvalidChampionSelectionRestriction" => "falten champions per triar",
        "QPScarcePositionsNotAvailableRestriction" => "posicions no disponibles",
        other => other,
    }
}

/// Should we start a search right now, and if not, why not.
/// As above, plus the one piece of state that is not in the snapshot: whether
/// the user cancelled this lobby search themselves.
pub fn evaluate_with(snapshot: &Snapshot, settings: &Settings, cancelled: bool) -> Gate {
    if !settings.auto_queue {
        return Gate::Disabled;
    }

    // Searching already, or past the lobby entirely. `phase` is empty before the
    // first gameflow event arrives, which is not the same as being in a lobby.
    if snapshot.is_searching() {
        return Gate::AlreadySearching;
    }
    if !snapshot.phase.is_empty() && snapshot.phase != "Lobby" {
        return Gate::WrongPhase(snapshot.phase.clone());
    }

    // After "already searching", so leaving the queue is reported as the user
    // choice it was rather than as an error, and before everything else, so no
    // other condition can talk us back into queueing.
    if cancelled {
        return Gate::CancelledByUser;
    }

    let Some(lobby) = &snapshot.lobby else {
        return Gate::NoLobby;
    };

    if lobby.game_config.is_custom {
        return Gate::CustomGame;
    }

    // The client rejects a search from anyone but the leader.
    if !lobby.i_am_leader() {
        return Gate::NotLeader;
    }

    // Checked before canStartActivity so the message names the real problem
    // rather than the generic refusal it causes.
    if let Some(search) = &snapshot.search {
        let penalty = search.penalty_remaining();
        if penalty > 0.0 {
            return Gate::Penalty { secs: penalty };
        }
    }

    if let Some(restriction) = lobby.restrictions.first() {
        return Gate::Restricted(restriction.restriction_code.clone());
    }

    // The user's own trigger.
    let members = lobby.members.len();
    match Trigger::parse(&settings.auto_queue_trigger) {
        Trigger::Full => {
            // The client's own verdict first. Comparing members against
            // maxLobbySize looks right and is not: Arena reports a capacity of
            // 16-18 while you queue solo or as a duo, so the comparison could
            // never be satisfied and the default trigger simply never fired
            // there. maxHumanPlayers is worse still - it is 0 in every
            // matchmade queue.
            let capacity = lobby.game_config.max_lobby_size as usize;
            if !lobby.game_config.is_lobby_full && capacity > 0 && members < capacity {
                return Gate::WaitingForMembers {
                    have: members,
                    need: capacity,
                };
            }
        }
        Trigger::Members => {
            let need = settings.auto_queue_min_members.max(1) as usize;
            if members < need {
                return Gate::WaitingForMembers {
                    have: members,
                    need,
                };
            }
        }
        Trigger::Ready => {
            let ready = lobby.members.iter().filter(|m| m.is_ready()).count();
            if ready < members || members == 0 {
                return Gate::NotEveryoneReady {
                    ready,
                    total: members,
                };
            }
        }
    }

    // Everything the client knows, in one boolean. Last, so the specific
    // messages above get a chance to explain a false first.
    if !lobby.can_start_activity {
        return Gate::ClientRefuses;
    }

    Gate::Ready
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lol_state::{Lobby, Search, Snapshot};

    /// The tests exercise the gate without the hand-cancel flag, which is the
    /// caller's state, not the snapshot's.
    fn evaluate(snapshot: &Snapshot, settings: &Settings) -> Gate {
        evaluate_with(snapshot, settings, false)
    }

    fn settings(trigger: &str) -> Settings {
        Settings {
            auto_queue: true,
            auto_queue_trigger: trigger.to_string(),
            ..Settings::default()
        }
    }

    /// A lobby in the state the client reports just before Find Match lights up.
    fn lobby(json: serde_json::Value) -> Lobby {
        serde_json::from_value(json).expect("lobby should parse")
    }

    fn ready_snapshot() -> Snapshot {
        Snapshot {
            phase: "Lobby".into(),
            lobby: Some(lobby(serde_json::json!({
                "canStartActivity": true,
                "localMember": { "isLeader": true },
                "members": [{ "isLeader": true, "ready": true }],
                "gameConfig": { "queueId": 420, "maxLobbySize": 1, "gameMode": "CLASSIC" },
            }))),
            ..Default::default()
        }
    }

    #[test]
    fn a_full_leader_owned_lobby_queues() {
        assert_eq!(evaluate(&ready_snapshot(), &settings("full")), Gate::Ready);
    }

    /// Leaving the queue by hand outranks every reason to re-enter it. The check
    /// sits after "already searching", so a cancel is reported as the choice it
    /// was, and before everything else, so no other condition can talk the gate
    /// back into queueing two seconds later.
    #[test]
    fn a_hand_cancel_beats_a_lobby_that_is_otherwise_ready() {
        let snap = ready_snapshot();
        let settings = settings("full");

        assert_eq!(evaluate(&snap, &settings), Gate::Ready);
        assert_eq!(evaluate_with(&snap, &settings, true), Gate::CancelledByUser);
        assert!(!evaluate_with(&snap, &settings, true).is_ready());
    }

    #[test]
    fn off_by_default_and_silent_when_off() {
        let settings = Settings::default();
        assert!(!settings.auto_queue, "must never queue unasked");
        assert_eq!(evaluate(&ready_snapshot(), &settings), Gate::Disabled);
    }

    /// The client refuses a search from a non-leader, so there is no point
    /// sending one - and the user gets told why rather than nothing happening.
    #[test]
    fn a_non_leader_never_queues() {
        let mut snap = ready_snapshot();
        snap.lobby.as_mut().unwrap().local_member.is_leader = false;
        assert_eq!(evaluate(&snap, &settings("full")), Gate::NotLeader);
    }

    #[test]
    fn a_custom_game_has_no_queue_to_start() {
        let mut snap = ready_snapshot();
        snap.lobby.as_mut().unwrap().game_config.is_custom = true;
        assert_eq!(evaluate(&snap, &settings("full")), Gate::CustomGame);
    }

    #[test]
    fn nothing_happens_outside_the_lobby() {
        let mut snap = ready_snapshot();
        snap.phase = "ChampSelect".into();
        assert_eq!(
            evaluate(&snap, &settings("full")),
            Gate::WrongPhase("ChampSelect".into())
        );
    }

    /// The single most important guard: never POST a second search while one is
    /// already running.
    #[test]
    fn an_active_search_is_not_restarted() {
        let mut snap = ready_snapshot();
        snap.search = Some(
            serde_json::from_value::<Search>(serde_json::json!({
                "searchState": "Searching",
                "isCurrentlyInQueue": true,
            }))
            .unwrap(),
        );
        assert_eq!(evaluate(&snap, &settings("full")), Gate::AlreadySearching);
    }

    /// Queuing during a dodge timer cannot succeed, so the gate stops rather
    /// than hammering a POST the client will reject.
    #[test]
    fn a_penalty_blocks_the_queue_and_says_how_long() {
        let mut snap = ready_snapshot();
        snap.search = Some(
            serde_json::from_value::<Search>(serde_json::json!({
                "errors": [{ "id": 1, "errorType": "QUEUE_DODGER", "penaltyTimeRemaining": 300.0 }],
            }))
            .unwrap(),
        );
        match evaluate(&snap, &settings("full")) {
            Gate::Penalty { secs } => assert_eq!(secs, 300.0),
            other => panic!("expected a penalty, got {:?}", other),
        }
        assert!(evaluate(&snap, &settings("full")).describe().contains("5 min"));
    }

    #[test]
    fn a_restriction_is_named_rather_than_swallowed() {
        let mut snap = ready_snapshot();
        snap.lobby.as_mut().unwrap().can_start_activity = false;
        snap.lobby.as_mut().unwrap().restrictions = serde_json::from_value(serde_json::json!([
            { "restrictionCode": "QueueDisabled" }
        ]))
        .unwrap();

        let gate = evaluate(&snap, &settings("full"));
        assert_eq!(gate, Gate::Restricted("QueueDisabled".into()));
        assert!(gate.describe().contains("cua desactivada"));
    }

    /// An unmapped code still reaches the user, untranslated, rather than
    /// becoming a generic "no".
    #[test]
    fn an_unknown_restriction_code_is_passed_through() {
        assert!(Gate::Restricted("SomeNewRiotCode".into())
            .describe()
            .contains("SomeNewRiotCode"));
    }

    // ── Triggers ──

    #[test]
    fn full_waits_for_every_seat() {
        let mut snap = ready_snapshot();
        snap.lobby.as_mut().unwrap().game_config.max_lobby_size = 5;
        assert_eq!(
            evaluate(&snap, &settings("full")),
            Gate::WaitingForMembers { have: 1, need: 5 }
        );
    }

    /// Solo queue: the lobby is "full" at one, so this must not wait forever.
    #[test]
    fn solo_queue_is_full_at_one() {
        assert_eq!(evaluate(&ready_snapshot(), &settings("full")), Gate::Ready);
    }

    #[test]
    fn the_member_count_trigger_is_configurable() {
        let mut snap = ready_snapshot();
        snap.lobby.as_mut().unwrap().game_config.max_lobby_size = 5;
        let mut settings = settings("members");
        settings.auto_queue_min_members = 2;

        assert_eq!(
            evaluate(&snap, &settings),
            Gate::WaitingForMembers { have: 1, need: 2 }
        );

        snap.lobby.as_mut().unwrap().members = serde_json::from_value(serde_json::json!([
            { "isLeader": true }, { "isLeader": false }
        ]))
        .unwrap();
        assert_eq!(evaluate(&snap, &settings), Gate::Ready);
    }

    /// A count of zero would queue an empty lobby; clamp it.
    #[test]
    fn a_member_count_of_zero_still_needs_one_member() {
        let mut snap = ready_snapshot();
        snap.lobby.as_mut().unwrap().members = Vec::new();
        snap.lobby.as_mut().unwrap().game_config.max_lobby_size = 5;
        let mut settings = settings("members");
        settings.auto_queue_min_members = 0;
        assert_eq!(
            evaluate(&snap, &settings),
            Gate::WaitingForMembers { have: 0, need: 1 }
        );
    }

    #[test]
    fn the_ready_trigger_waits_for_everyone() {
        let mut snap = ready_snapshot();
        snap.lobby.as_mut().unwrap().members = serde_json::from_value(serde_json::json!([
            { "ready": true }, { "ready": false }
        ]))
        .unwrap();
        assert_eq!(
            evaluate(&snap, &settings("ready")),
            Gate::NotEveryoneReady { ready: 1, total: 2 }
        );

        snap.lobby.as_mut().unwrap().members = serde_json::from_value(serde_json::json!([
            { "ready": true }, { "ready": true }
        ]))
        .unwrap();
        assert_eq!(evaluate(&snap, &settings("ready")), Gate::Ready);
    }

    /// Newer patches have been seen nesting readiness under memberData.
    #[test]
    fn the_ready_trigger_reads_nested_readiness_too() {
        let mut snap = ready_snapshot();
        snap.lobby.as_mut().unwrap().members = serde_json::from_value(serde_json::json!([
            { "memberData": { "ready": true } }
        ]))
        .unwrap();
        assert_eq!(evaluate(&snap, &settings("ready")), Gate::Ready);
    }

    #[test]
    fn an_empty_lobby_is_never_everyone_ready() {
        let mut snap = ready_snapshot();
        snap.lobby.as_mut().unwrap().members = Vec::new();
        assert_eq!(
            evaluate(&snap, &settings("ready")),
            Gate::NotEveryoneReady { ready: 0, total: 0 }
        );
    }

    /// The trigger being met is not enough: the client still has the last word,
    /// because it knows about things we never see.
    #[test]
    fn the_client_still_has_the_last_word() {
        let mut snap = ready_snapshot();
        snap.lobby.as_mut().unwrap().can_start_activity = false;
        assert_eq!(evaluate(&snap, &settings("full")), Gate::ClientRefuses);
    }

    #[test]
    fn an_unknown_trigger_falls_back_to_full() {
        assert_eq!(Trigger::parse("nonsense"), Trigger::Full);
        assert_eq!(Trigger::parse(""), Trigger::Full);
    }

    // ── Messages ──

    /// Waiting for a fifth is the normal state of a lobby; a penalty is not.
    /// Only the second belongs in the log.
    #[test]
    fn only_real_problems_are_worth_logging() {
        assert!(!Gate::WaitingForMembers { have: 3, need: 5 }.is_noteworthy());
        assert!(!Gate::Disabled.is_noteworthy());
        assert!(!Gate::AlreadySearching.is_noteworthy());
        assert!(Gate::Penalty { secs: 60.0 }.is_noteworthy());
        assert!(Gate::NotLeader.is_noteworthy());
        assert!(Gate::Restricted("QueueDisabled".into()).is_noteworthy());
    }

    #[test]
    fn durations_read_as_minutes_past_a_minute() {
        assert_eq!(format_duration(45.0), "45 s");
        assert_eq!(format_duration(60.0), "1 min 0 s");
        assert_eq!(format_duration(330.0), "5 min 30 s");
        assert_eq!(format_duration(-5.0), "0 s");
    }
}
