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

pub async fn pick_champion(
    creds: &LcuCredentials,
    action_id: i64,
    champion_id: i32,
) -> Result<(), String> {
    hover_and_lock(creds, action_id, champion_id, HOVER_LOCK_GAP).await
}

pub async fn ban_champion(
    creds: &LcuCredentials,
    action_id: i64,
    champion_id: i32,
) -> Result<(), String> {
    hover_and_lock(creds, action_id, champion_id, HOVER_LOCK_GAP).await
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

/// Hover then commit, waiting `gap` in between (never less than
/// `HOVER_LOCK_GAP`). A failed hover is not fatal — the lock PATCH carries the
/// championId anyway — so we log it and still try to commit.
pub async fn hover_and_lock(
    creds: &LcuCredentials,
    action_id: i64,
    champion_id: i32,
    gap: Duration,
) -> Result<(), String> {
    if let Err(e) = hover_champion(creds, action_id, champion_id).await {
        log::warn!(
            "hover failed (action {}, champion {}): {} - continuing to lock",
            action_id,
            champion_id,
            e
        );
    }
    tokio::time::sleep(gap.max(HOVER_LOCK_GAP)).await;
    lock_champion(creds, action_id, champion_id).await?;
    Ok(())
}
