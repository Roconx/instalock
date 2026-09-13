//! Choosing which champion to pick or ban.
//!
//! The design point: availability is decided **before** the call, not by trying
//! and handling the failure. Every mature tool works this way, and it is
//! strictly better here — the candidate is recomputed on every session event,
//! so a champion that gets banned or hovered during the countdown is dropped
//! and the next one takes its place, with no failed request in between.
//!
//! Nothing in this module touches the network or any shared state: a snapshot
//! and a list go in, a decision comes out. That is what makes it testable, and
//! it is the riskiest logic in the app.

use crate::lol_state::Snapshot;
use crate::settings::ChampionEntry;
use std::collections::HashSet;

/// Why a candidate was passed over. Logged, and shown in the info panel, so a
/// list that quietly does nothing can be explained rather than guessed at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Skipped {
    /// The name doesn't resolve to a champion — a typo, or the champion table
    /// hasn't loaded yet.
    Unknown,
    /// Switched off by the user, or a one-shot that has already been spent.
    Disabled,
    /// Not in the client's pickable/bannable set: not owned, not in the free
    /// rotation, or restricted in this queue or mode.
    Unavailable,
    Banned,
    /// Locked in by someone else.
    Taken,
    /// An ally is hovering it.
    AllyHovering,
}

impl Skipped {
    /// Catalan, for the log. The UI language is Catalan; the code is not.
    pub fn reason(self) -> &'static str {
        match self {
            Skipped::Unknown => "no trobat",
            Skipped::Disabled => "desactivat",
            Skipped::Unavailable => "no disponible",
            Skipped::Banned => "banejat",
            Skipped::Taken => "ja agafat",
            Skipped::AllyHovering => "un aliat l'està hoverejant",
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PassedOver {
    pub name: String,
    pub reason: Skipped,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Choice {
    pub champion_id: i32,
    pub name: String,
    /// Candidates ahead of this one, and why each was skipped.
    pub passed_over: Vec<PassedOver>,
}

/// Nothing in the list was usable.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Exhausted {
    pub passed_over: Vec<PassedOver>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Intent {
    Pick,
    Ban,
}

/// Walk the list in order and take the first champion that is actually usable.
///
/// `resolve` maps a display name to a champion id; it is a parameter rather
/// than a `Champions` reference so this stays free of shared state.
pub fn choose(
    intent: Intent,
    list: &[ChampionEntry],
    snapshot: &Snapshot,
    resolve: impl Fn(&str) -> Option<i32>,
    avoid_ally_hover: bool,
    arena: bool,
) -> Result<Choice, Exhausted> {
    // Without a loaded grid the session's own fields are the fallback: they
    // carry bans and locks, though not hovers as reliably.
    let session = snapshot.champ_select.as_ref();
    let unavailable: HashSet<i32> = session
        .map(|s| s.unavailable_champions())
        .unwrap_or_default();
    let ally_hovers: HashSet<i32> = session.map(|s| s.ally_pick_intents()).unwrap_or_default();
    let allow_duplicates = session.map(|s| s.allow_duplicate_picks).unwrap_or(false);
    // For bans, the client's own rule wins over our preference.
    let hover_blocks_ban = session
        .map(|s| s.disallow_banning_teammate_hovered_champions)
        .unwrap_or(false);

    let mut passed_over = Vec::new();

    for entry in list {
        let name = &entry.name;

        // A spent one-shot, or one the user parked by hand. It keeps its place
        // in the list; it just is not a candidate.
        if !entry.is_active() {
            passed_over.push(PassedOver {
                name: name.clone(),
                reason: Skipped::Disabled,
            });
            continue;
        }

        // Bravery is a list entry, not a champion: it resolves to the LCU's own
        // sentinel and skips every availability check, because there is no grid
        // entry for "whatever Arena feels like". Outside an Arena pick it is
        // simply not on the table, so it falls through like any other
        // unavailable candidate.
        if crate::settings::is_bravery(name) {
            if arena && intent == Intent::Pick {
                return Ok(Choice {
                    champion_id: crate::settings::BRAVERY_ID,
                    name: name.clone(),
                    passed_over,
                });
            }
            passed_over.push(PassedOver {
                name: name.clone(),
                reason: Skipped::Unavailable,
            });
            continue;
        }

        let Some(id) = resolve(name) else {
            passed_over.push(PassedOver {
                name: name.clone(),
                reason: Skipped::Unknown,
            });
            continue;
        };

        let grid = snapshot.selection(id);

        // The server's hard rule set. An empty set means "not loaded yet", not
        // "nothing is allowed" — filtering on it then would reject everything.
        let allowed = match intent {
            Intent::Pick => &snapshot.pickable,
            Intent::Ban => &snapshot.bannable,
        };
        if !allowed.is_empty() && !allowed.contains(&id) {
            passed_over.push(PassedOver {
                name: name.clone(),
                reason: Skipped::Unavailable,
            });
            continue;
        }

        let is_banned = grid.map(|g| g.is_banned).unwrap_or(false)
            || session
                .map(|s| {
                    s.bans.my_team_bans.contains(&id) || s.bans.their_team_bans.contains(&id)
                })
                .unwrap_or(false);
        if is_banned {
            passed_over.push(PassedOver {
                name: name.clone(),
                reason: Skipped::Banned,
            });
            continue;
        }

        let hovered_by_ally = grid
            .map(|g| g.pick_intented && !g.pick_intented_by_me)
            .unwrap_or_else(|| ally_hovers.contains(&id));

        match intent {
            Intent::Pick => {
                // A champion already locked by someone else, unless the mode
                // lets both teams run the same one (ARAM, some rotating modes).
                let taken = grid
                    .map(|g| g.picked_by_other_or_banned && !g.selected_by_me)
                    .unwrap_or_else(|| unavailable.contains(&id));
                if taken && !allow_duplicates {
                    passed_over.push(PassedOver {
                        name: name.clone(),
                        reason: Skipped::Taken,
                    });
                    continue;
                }
                if avoid_ally_hover && hovered_by_ally {
                    passed_over.push(PassedOver {
                        name: name.clone(),
                        reason: Skipped::AllyHovering,
                    });
                    continue;
                }
            }
            Intent::Ban => {
                // Banning a teammate's hover is rejected by the client outright
                // when this flag is set, so it is not a preference here.
                if hover_blocks_ban && hovered_by_ally {
                    passed_over.push(PassedOver {
                        name: name.clone(),
                        reason: Skipped::AllyHovering,
                    });
                    continue;
                }
            }
        }

        return Ok(Choice {
            champion_id: id,
            name: name.clone(),
            passed_over,
        });
    }

    Err(Exhausted { passed_over })
}

/// A one-line summary of what was skipped, for the log.
/// Last resort when every candidate was rejected by the client's own
/// availability table and by nothing else.
///
/// Deciding availability before the call is still the right default — it is how
/// a champion banned mid-countdown gets dropped for the next one. But the table
/// is the one input we cannot verify, and when it is wrong the cost of trusting
/// it is total: in an Arena draft `bannable-champion-ids` did not list K'Sante,
/// every entry was skipped as unavailable, and the ban was simply never cast.
///
/// A rejected PATCH costs nothing and is re-armed on the next session event.
/// Not acting costs the whole action. So when the *only* thing standing in the
/// way is the table, try anyway.
pub fn ignoring_availability(
    list: &[ChampionEntry],
    exhausted: &Exhausted,
    resolve: impl Fn(&str) -> Option<i32>,
) -> Option<Choice> {
    // Only when nothing else was wrong. A list whose entries are banned, taken
    // or hovered has been answered honestly and must not be second-guessed.
    if exhausted.passed_over.is_empty()
        || !exhausted
            .passed_over
            .iter()
            .all(|p| p.reason == Skipped::Unavailable)
    {
        return None;
    }

    for entry in list {
        if !entry.is_active() || crate::settings::is_bravery(&entry.name) {
            continue;
        }
        if let Some(id) = resolve(&entry.name) {
            return Some(Choice {
                champion_id: id,
                name: entry.name.clone(),
                passed_over: Vec::new(),
            });
        }
    }
    None
}

pub fn describe(passed_over: &[PassedOver]) -> String {
    passed_over
        .iter()
        .map(|p| format!("{} ({})", p.name, p.reason.reason()))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lol_state::{ChampionSelection, Snapshot};

    const AATROX: i32 = 266;
    const AHRI: i32 = 103;
    const AKALI: i32 = 84;

    fn resolver(name: &str) -> Option<i32> {
        match name {
            "Aatrox" => Some(AATROX),
            "Ahri" => Some(AHRI),
            "Akali" => Some(AKALI),
            _ => None,
        }
    }

    fn list(names: &[&str]) -> Vec<ChampionEntry> {
        names.iter().map(|s| ChampionEntry::new(*s)).collect()
    }

    /// A snapshot with the availability tables loaded and everything allowed.
    fn snapshot_with_all_available() -> Snapshot {
        let mut snap = Snapshot::default();
        snap.pickable = [AATROX, AHRI, AKALI].into_iter().collect();
        snap.bannable = [AATROX, AHRI, AKALI].into_iter().collect();
        for id in [AATROX, AHRI, AKALI] {
            snap.grid.insert(id, ChampionSelection::default());
        }
        snap
    }

    fn pick(list: &[ChampionEntry], snap: &Snapshot) -> Result<Choice, Exhausted> {
        choose(Intent::Pick, list, snap, resolver, true, false)
    }

    // ── Availability fallback ──
    //
    // Narrow on purpose. The table being wrong is the only case where trying
    // anyway is better than trusting it; every other skip reason is a fact the
    // client has told us plainly.

    #[test]
    fn an_unlisted_ban_is_attempted_anyway() {
        let exhausted = Exhausted {
            passed_over: vec![PassedOver {
                name: "K'Sante".into(),
                reason: Skipped::Unavailable,
            }],
        };
        let choice = ignoring_availability(&list(&["K'Sante"]), &exhausted, |_| Some(897)).unwrap();
        assert_eq!(choice.champion_id, 897);
        assert!(choice.passed_over.is_empty());
    }

    /// A champion that is banned, taken or hovered was rejected on evidence, not
    /// on a table we cannot verify. Retrying it would be pure noise.
    #[test]
    fn a_real_reason_is_never_second_guessed() {
        for reason in [Skipped::Banned, Skipped::Taken, Skipped::AllyHovering] {
            let exhausted = Exhausted {
                passed_over: vec![
                    PassedOver {
                        name: "Aatrox".into(),
                        reason: Skipped::Unavailable,
                    },
                    PassedOver {
                        name: "Ahri".into(),
                        reason,
                    },
                ],
            };
            assert!(
                ignoring_availability(&list(&["Aatrox", "Ahri"]), &exhausted, resolver).is_none(),
                "{:?} must not be retried",
                reason
            );
        }
    }

    #[test]
    fn an_empty_list_has_nothing_to_retry() {
        let exhausted = Exhausted {
            passed_over: Vec::new(),
        };
        assert!(ignoring_availability(&list(&[]), &exhausted, resolver).is_none());
    }

    /// Bravery is not a champion and is never a ban, so it must not be what the
    /// fallback reaches for.
    #[test]
    fn the_fallback_skips_a_bravery_entry() {
        let exhausted = Exhausted {
            passed_over: vec![
                PassedOver {
                    name: "Bravery".into(),
                    reason: Skipped::Unavailable,
                },
                PassedOver {
                    name: "Aatrox".into(),
                    reason: Skipped::Unavailable,
                },
            ],
        };
        let choice =
            ignoring_availability(&list(&["Bravery", "Aatrox"]), &exhausted, resolver).unwrap();
        assert_eq!(choice.name, "Aatrox");
    }

    // ── Bravery ──
    //
    // It is a list entry, not a switch: it takes a position, it can be a
    // one-shot, and it competes with the champions around it.

    fn arena_pick(list: &[ChampionEntry], snap: &Snapshot) -> Result<Choice, Exhausted> {
        choose(Intent::Pick, list, snap, resolver, true, true)
    }

    #[test]
    fn bravery_wins_when_it_is_first_in_an_arena_pick() {
        let snap = Snapshot::default();
        let choice = arena_pick(&list(&["Bravery", "Aatrox"]), &snap).unwrap();
        assert_eq!(choice.champion_id, crate::settings::BRAVERY_ID);
        assert_eq!(choice.name, "Bravery");
        assert!(choice.passed_over.is_empty());
    }

    /// Outside Arena the entry is simply not available, and the list carries on
    /// to the next champion rather than stalling on it.
    #[test]
    fn bravery_is_skipped_outside_arena() {
        let snap = Snapshot::default();
        let choice = pick(&list(&["Bravery", "Aatrox"]), &snap).unwrap();
        assert_eq!(choice.name, "Aatrox");
        assert_eq!(choice.passed_over.len(), 1);
        assert_eq!(choice.passed_over[0].reason, Skipped::Unavailable);
    }

    /// There is no such thing as banning Bravery.
    #[test]
    fn bravery_is_never_a_ban() {
        let snap = Snapshot::default();
        let choice = choose(
            Intent::Ban,
            &list(&["Bravery", "Aatrox"]),
            &snap,
            resolver,
            true,
            true,
        )
        .unwrap();
        assert_eq!(choice.name, "Aatrox");
    }

    /// A spent one-shot Bravery is skipped like any other spent entry - that is
    /// the whole point of it living in the list.
    #[test]
    fn a_spent_one_shot_bravery_is_skipped() {
        let snap = Snapshot::default();
        let entries = vec![
            ChampionEntry {
                name: "Bravery".into(),
                once: true,
                disabled: true,
            },
            ChampionEntry::new("Aatrox"),
        ];
        let choice = arena_pick(&entries, &snap).unwrap();
        assert_eq!(choice.name, "Aatrox");
        assert_eq!(choice.passed_over[0].reason, Skipped::Disabled);
    }

    /// Bravery does not resolve through the champion table, so an empty roster
    /// (the first seconds after launch) must not turn it into "not found".
    #[test]
    fn bravery_does_not_need_the_champion_table() {
        let snap = Snapshot::default();
        let choice = choose(
            Intent::Pick,
            &list(&["Bravery"]),
            &snap,
            |_| None,
            true,
            true,
        )
        .unwrap();
        assert_eq!(choice.champion_id, crate::settings::BRAVERY_ID);
    }

    #[test]
    fn takes_the_first_available_champion() {
        let snap = snapshot_with_all_available();
        let choice = pick(&list(&["Aatrox", "Ahri"]), &snap).unwrap();
        assert_eq!(choice.champion_id, AATROX);
        assert!(choice.passed_over.is_empty());
    }

    #[test]
    fn falls_through_to_the_backup_when_the_first_is_banned() {
        let mut snap = snapshot_with_all_available();
        snap.grid.insert(
            AATROX,
            ChampionSelection {
                is_banned: true,
                ..Default::default()
            },
        );

        let choice = pick(&list(&["Aatrox", "Ahri"]), &snap).unwrap();
        assert_eq!(choice.champion_id, AHRI);
        assert_eq!(choice.passed_over.len(), 1);
        assert_eq!(choice.passed_over[0].reason, Skipped::Banned);
        assert_eq!(choice.passed_over[0].name, "Aatrox");
    }

    #[test]
    fn falls_through_when_an_enemy_already_locked_it() {
        let mut snap = snapshot_with_all_available();
        snap.grid.insert(
            AATROX,
            ChampionSelection {
                picked_by_other_or_banned: true,
                ..Default::default()
            },
        );

        let choice = pick(&list(&["Aatrox", "Ahri"]), &snap).unwrap();
        assert_eq!(choice.champion_id, AHRI);
        assert_eq!(choice.passed_over[0].reason, Skipped::Taken);
    }

    /// The case the old single-champion code could not express at all: an ally
    /// hovering your pick. Nothing had banned it, nothing had locked it, and
    /// the PATCH just failed.
    #[test]
    fn skips_a_champion_an_ally_is_hovering() {
        let mut snap = snapshot_with_all_available();
        snap.grid.insert(
            AATROX,
            ChampionSelection {
                pick_intented: true,
                ..Default::default()
            },
        );

        let choice = pick(&list(&["Aatrox", "Ahri"]), &snap).unwrap();
        assert_eq!(choice.champion_id, AHRI);
        assert_eq!(choice.passed_over[0].reason, Skipped::AllyHovering);
    }

    #[test]
    fn my_own_hover_is_not_a_conflict() {
        let mut snap = snapshot_with_all_available();
        snap.grid.insert(
            AATROX,
            ChampionSelection {
                pick_intented: true,
                pick_intented_by_me: true,
                ..Default::default()
            },
        );

        assert_eq!(pick(&list(&["Aatrox"]), &snap).unwrap().champion_id, AATROX);
    }

    #[test]
    fn ally_hovers_can_be_allowed_by_setting() {
        let mut snap = snapshot_with_all_available();
        snap.grid.insert(
            AATROX,
            ChampionSelection {
                pick_intented: true,
                ..Default::default()
            },
        );

        let choice = choose(Intent::Pick, &list(&["Aatrox"]), &snap, resolver, false, false).unwrap();
        assert_eq!(choice.champion_id, AATROX);
    }

    #[test]
    fn skips_a_champion_the_client_will_not_allow() {
        let mut snap = snapshot_with_all_available();
        // Not owned, not in rotation, or restricted in this queue.
        snap.pickable.remove(&AATROX);

        let choice = pick(&list(&["Aatrox", "Ahri"]), &snap).unwrap();
        assert_eq!(choice.champion_id, AHRI);
        assert_eq!(choice.passed_over[0].reason, Skipped::Unavailable);
    }

    /// Empty means "not loaded yet". Filtering on it would reject the whole
    /// roster and turn a slow client into a silent no-pick.
    #[test]
    fn an_unloaded_pickable_set_does_not_filter_anything() {
        let mut snap = snapshot_with_all_available();
        snap.pickable.clear();
        assert_eq!(pick(&list(&["Aatrox"]), &snap).unwrap().champion_id, AATROX);
    }

    #[test]
    fn an_unresolvable_name_is_skipped_not_fatal() {
        let snap = snapshot_with_all_available();
        let choice = pick(&list(&["Notachampion", "Ahri"]), &snap).unwrap();
        assert_eq!(choice.champion_id, AHRI);
        assert_eq!(choice.passed_over[0].reason, Skipped::Unknown);
    }

    #[test]
    fn an_exhausted_list_reports_every_reason() {
        let mut snap = snapshot_with_all_available();
        snap.grid.insert(
            AATROX,
            ChampionSelection { is_banned: true, ..Default::default() },
        );
        snap.grid.insert(
            AHRI,
            ChampionSelection { picked_by_other_or_banned: true, ..Default::default() },
        );

        let err = pick(&list(&["Aatrox", "Ahri", "Notachampion"]), &snap).unwrap_err();
        assert_eq!(err.passed_over.len(), 3);
        assert_eq!(err.passed_over[0].reason, Skipped::Banned);
        assert_eq!(err.passed_over[1].reason, Skipped::Taken);
        assert_eq!(err.passed_over[2].reason, Skipped::Unknown);
    }

    #[test]
    fn an_empty_list_is_exhausted_immediately() {
        let snap = snapshot_with_all_available();
        assert!(pick(&[], &snap).unwrap_err().passed_over.is_empty());
    }

    #[test]
    fn bans_use_the_bannable_set_not_the_pickable_one() {
        let mut snap = snapshot_with_all_available();
        // An unowned champion can still be banned.
        snap.pickable.remove(&AATROX);

        let choice = choose(Intent::Ban, &list(&["Aatrox"]), &snap, resolver, true, false).unwrap();
        assert_eq!(choice.champion_id, AATROX);
    }

    #[test]
    fn a_ban_skips_an_already_banned_champion() {
        let mut snap = snapshot_with_all_available();
        snap.grid.insert(
            AATROX,
            ChampionSelection { is_banned: true, ..Default::default() },
        );

        let choice = choose(Intent::Ban, &list(&["Aatrox", "Ahri"]), &snap, resolver, true, false).unwrap();
        assert_eq!(choice.champion_id, AHRI);
    }

    /// For bans, avoiding an ally's hover is the client's rule, not ours: the
    /// `avoid_ally_hover` setting must not override it either way.
    #[test]
    fn a_ban_respects_the_clients_teammate_hover_rule() {
        let mut snap = snapshot_with_all_available();
        snap.champ_select = Some(
            serde_json::from_value(serde_json::json!({
                "disallowBanningTeammateHoveredChampions": true
            }))
            .unwrap(),
        );
        snap.grid.insert(
            AATROX,
            ChampionSelection { pick_intented: true, ..Default::default() },
        );

        // Even with the preference off, the client's rule still applies.
        let choice = choose(Intent::Ban, &list(&["Aatrox", "Ahri"]), &snap, resolver, false, false).unwrap();
        assert_eq!(choice.champion_id, AHRI);
        assert_eq!(choice.passed_over[0].reason, Skipped::AllyHovering);
    }

    #[test]
    fn a_ban_allows_a_teammate_hover_when_the_client_does() {
        let mut snap = snapshot_with_all_available();
        snap.grid.insert(
            AATROX,
            ChampionSelection { pick_intented: true, ..Default::default() },
        );
        // disallowBanningTeammateHoveredChampions defaults false.
        let choice = choose(Intent::Ban, &list(&["Aatrox"]), &snap, resolver, true, false).unwrap();
        assert_eq!(choice.champion_id, AATROX);
    }

    /// Before the grid arrives, the session's own bans and locks still have to
    /// be honoured — otherwise the first pick of every draft ignores them.
    #[test]
    fn without_a_grid_the_session_is_the_fallback() {
        let mut snap = Snapshot::default();
        snap.champ_select = Some(
            serde_json::from_value(serde_json::json!({
                "localPlayerCellId": 0,
                "myTeam": [
                    { "cellId": 0 },
                    { "cellId": 1, "championId": AHRI, "championPickIntent": 0 },
                ],
                "bans": { "myTeamBans": [AATROX], "theirTeamBans": [] },
            }))
            .unwrap(),
        );

        let choice = pick(&list(&["Aatrox", "Ahri", "Akali"]), &snap).unwrap();
        assert_eq!(choice.champion_id, AKALI);
        assert_eq!(choice.passed_over[0].reason, Skipped::Banned);
        assert_eq!(choice.passed_over[1].reason, Skipped::Taken);
    }

    #[test]
    fn without_a_grid_ally_hovers_still_count() {
        let mut snap = Snapshot::default();
        snap.champ_select = Some(
            serde_json::from_value(serde_json::json!({
                "localPlayerCellId": 0,
                "myTeam": [
                    { "cellId": 0 },
                    { "cellId": 1, "championPickIntent": AATROX },
                ],
            }))
            .unwrap(),
        );

        let choice = pick(&list(&["Aatrox", "Ahri"]), &snap).unwrap();
        assert_eq!(choice.champion_id, AHRI);
        assert_eq!(choice.passed_over[0].reason, Skipped::AllyHovering);
    }

    /// ARAM and some rotating modes let both teams run the same champion.
    #[test]
    fn duplicate_picks_are_fine_when_the_mode_allows_them() {
        let mut snap = snapshot_with_all_available();
        snap.champ_select = Some(
            serde_json::from_value(serde_json::json!({ "allowDuplicatePicks": true })).unwrap(),
        );
        snap.grid.insert(
            AATROX,
            ChampionSelection { picked_by_other_or_banned: true, ..Default::default() },
        );

        assert_eq!(pick(&list(&["Aatrox"]), &snap).unwrap().champion_id, AATROX);
    }

    #[test]
    fn the_log_line_names_every_skip_and_why() {
        let passed = vec![
            PassedOver { name: "Aatrox".into(), reason: Skipped::Banned },
            PassedOver { name: "Ahri".into(), reason: Skipped::AllyHovering },
        ];
        assert_eq!(
            describe(&passed),
            "Aatrox (banejat), Ahri (un aliat l'està hoverejant)"
        );
    }
}

#[cfg(test)]
mod role_tests {
    use crate::settings::{
        effective_list, spend_one_shots, ChampionEntry, ModeLists, RoleLists, Settings,
        DEFAULT_MODE, GLOBAL_MODE,
    };

    fn entries(names: &[&str]) -> Vec<ChampionEntry> {
        names.iter().map(|n| ChampionEntry::new(*n)).collect()
    }

    fn names(list: &[ChampionEntry]) -> Vec<&str> {
        list.iter().map(|e| e.name.as_str()).collect()
    }

    // The role-list resolution rules themselves live in settings::list_rules;
    // what follows is the per-mode and one-shot behaviour layered on top.

    // ── Per-mode memory ──

    fn mode_lists() -> ModeLists {
        ModeLists::from([
            (
                GLOBAL_MODE.to_string(),
                RoleLists {
                    default: entries(&["K'Sante"]),
                    ..Default::default()
                },
            ),
            (
                "CHERRY".to_string(),
                RoleLists {
                    default: entries(&["Zac"]),
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
        ])
    }

    /// The whole point of per-mode memory: a Zac added in Arena must not turn
    /// up in SoloQ.
    #[test]
    fn a_champion_added_in_arena_stays_in_arena() {
        let lists = mode_lists();
        assert!(names(&effective_list(&lists, "CHERRY", "")).contains(&"Zac"));
        assert!(!names(&effective_list(&lists, "CLASSIC", "")).contains(&"Zac"));
    }

    /// Global entries lead, because "always ban K'Sante" is a priority, not a
    /// fallback.
    #[test]
    fn global_entries_apply_in_every_mode_and_come_first() {
        let lists = mode_lists();
        assert_eq!(names(&effective_list(&lists, "CHERRY", "")), ["K'Sante", "Zac"]);
        assert_eq!(names(&effective_list(&lists, "CLASSIC", "")), ["K'Sante", "Garen"]);
        assert_eq!(names(&effective_list(&lists, "ARAM", "")), ["K'Sante"]);
    }

    /// Global first, then the mode's own mid list, then the mode's Defecte list
    /// beneath it as the safety net.
    #[test]
    fn per_mode_lists_still_resolve_per_role() {
        let lists = mode_lists();
        assert_eq!(
            names(&effective_list(&lists, "CLASSIC", "middle")),
            ["K'Sante", "Ahri", "Garen"]
        );
    }

    /// Asking for the global bucket itself must not double up its own entries.
    #[test]
    fn the_global_bucket_is_not_concatenated_with_itself() {
        assert_eq!(names(&effective_list(&mode_lists(), GLOBAL_MODE, "")), ["K'Sante"]);
    }

    #[test]
    fn an_unknown_mode_still_gets_the_globals() {
        // A rotating mode nobody has configured yet.
        assert_eq!(names(&effective_list(&mode_lists(), "NEXUSBLITZ", "")), ["K'Sante"]);
    }

    // ── The mode key ──

    #[test]
    fn the_current_mode_decides_the_bucket() {
        let settings = Settings::default();
        assert_eq!(settings.mode_key(Some("CHERRY")), "CHERRY");
        assert_eq!(settings.mode_key(Some("ARAM")), "ARAM");
    }

    /// No lobby open means the user is configuring, and Summoner's Rift is the
    /// resting state of the client.
    #[test]
    fn no_mode_falls_back_to_summoners_rift() {
        let settings = Settings::default();
        assert_eq!(settings.mode_key(None), DEFAULT_MODE);
        assert_eq!(settings.mode_key(Some("")), DEFAULT_MODE);
    }

    /// Turning per-mode memory off routes everything through the global bucket,
    /// so one list serves every mode - and the per-mode buckets survive on disk
    /// untouched, ready for when it is switched back on.
    #[test]
    fn per_mode_memory_can_be_switched_off() {
        let settings = Settings {
            per_mode_lists: false,
            ..Settings::default()
        };
        assert_eq!(settings.mode_key(Some("CHERRY")), GLOBAL_MODE);
        assert_eq!(settings.mode_key(None), GLOBAL_MODE);
    }

    #[test]
    fn with_per_mode_off_arena_sees_the_one_shared_list() {
        let settings = Settings {
            per_mode_lists: false,
            ban_lists: mode_lists(),
            ..Settings::default()
        };
        // Only the globals: the CHERRY bucket is no longer consulted.
        assert_eq!(names(&settings.ban_list_for(Some("CHERRY"), "")), ["K'Sante"]);
    }

    // ── One-shot entries ──

    #[test]
    fn a_one_shot_is_disabled_by_the_game_starting_not_removed() {
        let mut lists = ModeLists::from([(
            "CLASSIC".to_string(),
            RoleLists {
                default: vec![
                    ChampionEntry { name: "Ahri".into(), once: true, disabled: false },
                    ChampionEntry::new("Garen"),
                ],
                ..Default::default()
            },
        )]);

        assert_eq!(spend_one_shots(&mut lists), 1);

        let after = &lists["CLASSIC"].default;
        assert_eq!(after.len(), 2, "the entry keeps its place in the order");
        assert_eq!(after[0].name, "Ahri");
        assert!(after[0].disabled, "spent, so no longer a candidate");
        assert!(after[0].once, "still flagged one-shot, so re-arming is one click");
        assert!(!after[1].disabled, "a permanent entry is untouched");
    }

    #[test]
    fn spending_one_shots_is_idempotent() {
        let mut lists = ModeLists::from([(
            "CLASSIC".to_string(),
            RoleLists {
                default: vec![ChampionEntry { name: "Ahri".into(), once: true, disabled: false }],
                ..Default::default()
            },
        )]);
        assert_eq!(spend_one_shots(&mut lists), 1);
        assert_eq!(spend_one_shots(&mut lists), 0, "already spent");
    }

    #[test]
    fn one_shots_are_spent_across_every_mode_and_role() {
        let mut lists = ModeLists::from([
            (
                GLOBAL_MODE.to_string(),
                RoleLists {
                    middle: vec![ChampionEntry { name: "Ahri".into(), once: true, disabled: false }],
                    ..Default::default()
                },
            ),
            (
                "CHERRY".to_string(),
                RoleLists {
                    default: vec![ChampionEntry { name: "Zac".into(), once: true, disabled: false }],
                    ..Default::default()
                },
            ),
        ]);
        assert_eq!(spend_one_shots(&mut lists), 2);
    }

    #[test]
    fn an_entry_with_a_blank_name_is_never_a_candidate() {
        assert!(!ChampionEntry::new("   ").is_active());
        assert!(ChampionEntry::new("Ahri").is_active());
        assert!(!ChampionEntry { name: "Ahri".into(), once: false, disabled: true }.is_active());
    }
}

#[cfg(test)]
mod recents_tests {
    use crate::settings::{push_recent, MAX_RECENTS};

    #[test]
    fn the_newest_champion_leads() {
        let mut recents = vec!["Garen".to_string()];
        push_recent(&mut recents, "Ahri");
        assert_eq!(recents, ["Ahri", "Garen"]);
    }

    /// Picking the same champion twice must move it to the front, not add a
    /// second chip for it.
    #[test]
    fn a_repeat_moves_to_the_front_rather_than_duplicating() {
        let mut recents = vec!["Ahri".to_string(), "Garen".to_string()];
        push_recent(&mut recents, "Garen");
        assert_eq!(recents, ["Garen", "Ahri"]);
    }

    #[test]
    fn matching_ignores_case() {
        let mut recents = vec!["Ahri".to_string()];
        push_recent(&mut recents, "ahri");
        assert_eq!(recents.len(), 1);
    }

    #[test]
    fn the_list_is_capped() {
        let mut recents = Vec::new();
        for i in 0..20 {
            push_recent(&mut recents, &format!("Champ{}", i));
        }
        assert_eq!(recents.len(), MAX_RECENTS);
        assert_eq!(recents[0], "Champ19", "newest first");
    }
}
