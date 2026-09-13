use crate::lcu::{lcu_request, LcuCredentials};
use std::time::Duration;

/// Minimum separation between the hover PATCH and the lock PATCH on the same
/// action. Sending both back-to-back makes the LCU reject the second one, so
/// every hover+lock path must respect this floor.
pub const HOVER_LOCK_GAP: Duration = Duration::from_millis(300);

pub async fn accept_match(creds: &LcuCredentials) -> Result<(), String> {
    lcu_request(creds, "POST", "/lol-matchmaking/v1/ready-check/accept", None).await?;
    Ok(())
}

/// Start matchmaking for the current lobby. No body; 204 on success.
///
/// Deliberately the `lol-lobby` route and not `POST /lol-matchmaking/v1/search`:
/// the latter returns 500 for every TeamBuilder-managed queue, which is ARAM,
/// Arena, Swiftplay and more. This one works for all of them.
///
/// The response is not the source of truth — the search is confirmed by
/// `searchState` turning to "Searching", or by the gameflow phase moving on.
pub async fn start_matchmaking(creds: &LcuCredentials) -> Result<(), String> {
    lcu_request(
        creds,
        "POST",
        "/lol-lobby/v2/lobby/matchmaking/search",
        None,
    )
    .await?;
    Ok(())
}

pub async fn cancel_matchmaking(creds: &LcuCredentials) -> Result<(), String> {
    lcu_request(
        creds,
        "DELETE",
        "/lol-lobby/v2/lobby/matchmaking/search",
        None,
    )
    .await?;
    Ok(())
}

/// Commit a pick outright, with no hover step.
///
/// This is what "hover abans de lockejar" being off has to mean. It used to go
/// through `hover_and_lock` anyway, which both defeated the setting and added
/// HOVER_LOCK_GAP on top of a delay that had already been clamped against the
/// phase timer - so the clamp was 300 ms short of the guarantee it claimed.
pub async fn pick_champion(
    creds: &LcuCredentials,
    action_id: i64,
    champion_id: i32,
) -> Result<(), String> {
    lock_champion(creds, action_id, champion_id).await
}

/// Bans are committed in one PATCH: there is nothing for a ban hover to tell
/// anyone, and the extra round trip only eats into the phase timer.
pub async fn ban_champion(
    creds: &LcuCredentials,
    action_id: i64,
    champion_id: i32,
) -> Result<(), String> {
    lock_champion(creds, action_id, champion_id).await
}

/// Arena Bravery: a single PATCH with the sentinel championId -3 and
/// completed=true. The server recognizes the sentinel, picks a random
/// champion, and awards the Bravery rewards (+50 Fame, voucher).
///
/// Only valid in CHERRY game mode. Unlike a normal pick this does NOT use a
/// hover step — committing with -3 directly is what the official client does.
pub async fn pick_bravery(creds: &LcuCredentials, action_id: i64) -> Result<(), String> {
    let url = format!("/lol-champ-select/v1/session/actions/{}", action_id);
    let body = serde_json::json!({
        "championId": -3,
        "completed": true,
    });
    lcu_request(creds, "PATCH", &url, Some(body)).await?;
    Ok(())
}

pub async fn hover_champion(
    creds: &LcuCredentials,
    action_id: i64,
    champion_id: i32,
) -> Result<(), String> {
    let url = format!("/lol-champ-select/v1/session/actions/{}", action_id);
    let body = serde_json::json!({ "championId": champion_id });
    lcu_request(creds, "PATCH", &url, Some(body)).await?;
    Ok(())
}

pub async fn lock_champion(
    creds: &LcuCredentials,
    action_id: i64,
    champion_id: i32,
) -> Result<(), String> {
    let url = format!("/lol-champ-select/v1/session/actions/{}", action_id);
    let body = serde_json::json!({
        "championId": champion_id,
        "completed": true
    });
    lcu_request(creds, "PATCH", &url, Some(body)).await?;
    Ok(())
}

