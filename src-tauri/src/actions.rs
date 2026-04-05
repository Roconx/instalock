use crate::lcu::{lcu_request, LcuCredentials};

pub async fn accept_match(creds: &LcuCredentials) -> Result<(), String> {
    lcu_request(creds, "POST", "/lol-matchmaking/v1/ready-check/accept", None).await?;
    Ok(())
}

pub async fn pick_champion(creds: &LcuCredentials, champion_id: i32) -> Result<(), String> {
    let action_id = find_my_action(creds, "pick").await?;
    hover_and_lock(creds, action_id, champion_id).await
}

pub async fn ban_champion(creds: &LcuCredentials, champion_id: i32) -> Result<(), String> {
    let action_id = find_my_action(creds, "ban").await?;
    hover_and_lock(creds, action_id, champion_id).await
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

async fn find_my_action(creds: &LcuCredentials, action_type: &str) -> Result<i64, String> {
    let session = lcu_request(creds, "GET", "/lol-champ-select/v1/session", None).await?;

    let my_cell_id = session["localPlayerCellId"]
        .as_i64()
        .ok_or("No localPlayerCellId")?;

    let actions = session["actions"]
        .as_array()
        .ok_or("No actions array")?;

    for group in actions {
        if let Some(group_arr) = group.as_array() {
            for action in group_arr {
                let actor = action["actorCellId"].as_i64().unwrap_or(-1);
                let atype = action["type"].as_str().unwrap_or("");
                let in_progress = action["isInProgress"].as_bool().unwrap_or(false);
                let completed = action["completed"].as_bool().unwrap_or(true);

                if actor == my_cell_id && atype == action_type && in_progress && !completed {
                    return action["id"].as_i64().ok_or("No action id".to_string());
                }
            }
        }
    }

    Err(format!("No active {} action found", action_type))
}

async fn hover_and_lock(
    creds: &LcuCredentials,
    action_id: i64,
    champion_id: i32,
) -> Result<(), String> {
    let url = format!("/lol-champ-select/v1/session/actions/{}", action_id);

    // Step 1: Select/hover the champion
    let hover_body = serde_json::json!({
        "championId": champion_id
    });
    lcu_request(creds, "PATCH", &url, Some(hover_body)).await?;

    // Small delay to let the client register the hover
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    // Step 2: Lock in (complete the action)
    let lock_body = serde_json::json!({
        "championId": champion_id,
        "completed": true
    });
    lcu_request(creds, "PATCH", &url, Some(lock_body)).await?;

    Ok(())
}
