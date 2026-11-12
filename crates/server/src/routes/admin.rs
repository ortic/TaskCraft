//! Workspace administration: settings, audit log, webhooks, usage.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::app::{AppState, audit, new_id};
use crate::auth::{CurrentUser, random_token, require_admin};
use crate::error::{ApiError, ApiResult};

pub async fn put_workspace(State(state): State<AppState>, CurrentUser(user): CurrentUser, Json(req): Json<Value>) -> ApiResult<StatusCode> {
    require_admin(&user)?;
    if let Some(n) = req["name"].as_str().map(str::trim).filter(|n| !n.is_empty()) {
        sqlx::query("UPDATE workspaces SET name = $2 WHERE id = $1").bind(user.workspace_id).bind(n).execute(&state.db).await?;
    }
    let mut settings: Value = sqlx::query_scalar("SELECT settings FROM workspaces WHERE id = $1").bind(user.workspace_id).fetch_one(&state.db).await?;
    for k in ["accentColor", "defaultLanguage", "weekStart"] {
        if let Some(v) = req.get(k) {
            if k == "accentColor" && v.as_str().is_some_and(|c| !(c.len() == 7 && c.starts_with('#') && c[1..].chars().all(|x| x.is_ascii_hexdigit()))) {
                return Err(ApiError::bad("the accent colour looks like #4f6bed"));
            }
            settings[k] = v.clone();
        }
    }
    sqlx::query("UPDATE workspaces SET settings = $2 WHERE id = $1").bind(user.workspace_id).bind(settings).execute(&state.db).await?;
    audit(&state, user.workspace_id, Some(user.id), "workspace.updated", None, req).await;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn audit_log(State(state): State<AppState>, CurrentUser(user): CurrentUser) -> ApiResult<Json<Value>> {
    require_admin(&user)?;
    let rows: Vec<(Uuid, Option<String>, String, Option<String>, Value, chrono::DateTime<chrono::Utc>)> = sqlx::query_as(
        "SELECT a.id, u.name, a.action, a.target, a.data, a.created_at FROM audit_log a LEFT JOIN users u ON u.id = a.actor_id WHERE a.workspace_id = $1 ORDER BY a.created_at DESC LIMIT 300",
    )
    .bind(user.workspace_id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(json!(
        rows.into_iter().map(|(id, who, a, t, d, at)| json!({"id": id, "actor": who, "action": a, "target": t, "data": d, "at": at})).collect::<Vec<_>>()
    )))
}

pub async fn stats(State(state): State<AppState>, CurrentUser(user): CurrentUser) -> ApiResult<Json<Value>> {
    require_admin(&user)?;
    let (users, projects, issues, comments, attachments, bytes): (i64, i64, i64, i64, i64, Option<i64>) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM users WHERE workspace_id = $1 AND status = 'active'),
                (SELECT count(*) FROM projects WHERE workspace_id = $1),
                (SELECT count(*) FROM issues WHERE workspace_id = $1 AND deleted_at IS NULL),
                (SELECT count(*) FROM comments c JOIN issues i ON i.id = c.issue_id WHERE i.workspace_id = $1),
                (SELECT count(*) FROM attachments a JOIN issues i ON i.id = a.issue_id WHERE i.workspace_id = $1),
                (SELECT sum(a.size)::bigint FROM attachments a JOIN issues i ON i.id = a.issue_id WHERE i.workspace_id = $1)",
    )
    .bind(user.workspace_id)
    .fetch_one(&state.db)
    .await?;
    Ok(Json(
        json!({"users": users, "projects": projects, "issues": issues, "comments": comments, "attachments": attachments, "attachmentBytes": bytes.unwrap_or(0)}),
    ))
}

pub async fn webhooks(State(state): State<AppState>, CurrentUser(user): CurrentUser) -> ApiResult<Json<Value>> {
    require_admin(&user)?;
    let rows: Vec<(Uuid, Option<Uuid>, Option<String>, String, Vec<String>, bool, Option<i32>, Option<String>, Option<chrono::DateTime<chrono::Utc>>)> = sqlx::query_as(
        "SELECT w.id, w.project_id, p.key, w.url, w.events, w.active, w.last_status, w.last_error, w.last_sent_at FROM webhooks w LEFT JOIN projects p ON p.id = w.project_id
          WHERE w.workspace_id = $1 ORDER BY w.created_at",
    )
    .bind(user.workspace_id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(json!({
        "events": crate::webhooks::EVENTS,
        "webhooks": rows.into_iter().map(|(id, pid, pk, url, ev, active, st, err, at)| json!({
            "id": id, "projectId": pid, "projectKey": pk, "url": url, "events": ev, "active": active, "lastStatus": st, "lastError": err, "lastSentAt": at
        })).collect::<Vec<_>>()
    })))
}

pub async fn create_webhook(State(state): State<AppState>, CurrentUser(user): CurrentUser, Json(req): Json<Value>) -> ApiResult<(StatusCode, Json<Value>)> {
    require_admin(&user)?;
    let url = req["url"].as_str().unwrap_or("");
    crate::webhooks::validate_url(url).map_err(ApiError::BadRequest)?;
    let events: Vec<String> = req["events"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|e| e.as_str())
        .filter(|e| crate::webhooks::EVENTS.contains(e) || *e == "*")
        .map(str::to_string)
        .collect();
    let project = match req["projectKey"].as_str().filter(|k| !k.is_empty()) {
        Some(k) => Some(super::projects::by_key(&state, &user, k, crate::access::Role::Admin).await?.0.id),
        None => None,
    };
    let secret = req["secret"].as_str().filter(|s| s.len() >= 8).map(str::to_string).unwrap_or_else(random_token);
    let id = new_id();
    sqlx::query("INSERT INTO webhooks (id, workspace_id, project_id, url, secret, events, created_by) VALUES ($1, $2, $3, $4, $5, $6, $7)")
        .bind(id)
        .bind(user.workspace_id)
        .bind(project)
        .bind(url.trim())
        .bind(&secret)
        .bind(&events)
        .bind(user.id)
        .execute(&state.db)
        .await?;
    audit(&state, user.workspace_id, Some(user.id), "webhook.created", Some(url.to_string()), json!({"events": events})).await;
    Ok((StatusCode::CREATED, Json(json!({"id": id, "secret": secret}))))
}

pub async fn update_webhook(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<Uuid>,
    Json(req): Json<Value>,
) -> ApiResult<StatusCode> {
    require_admin(&user)?;
    if let Some(a) = req["active"].as_bool() {
        sqlx::query("UPDATE webhooks SET active = $3 WHERE id = $1 AND workspace_id = $2").bind(id).bind(user.workspace_id).bind(a).execute(&state.db).await?;
    }
    if let Some(ev) = req["events"].as_array() {
        let events: Vec<String> =
            ev.iter().filter_map(|e| e.as_str()).filter(|e| crate::webhooks::EVENTS.contains(e) || *e == "*").map(str::to_string).collect();
        sqlx::query("UPDATE webhooks SET events = $3 WHERE id = $1 AND workspace_id = $2")
            .bind(id)
            .bind(user.workspace_id)
            .bind(&events)
            .execute(&state.db)
            .await?;
    }
    Ok(StatusCode::NO_CONTENT)
}

pub async fn delete_webhook(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    require_admin(&user)?;
    sqlx::query("DELETE FROM webhooks WHERE id = $1 AND workspace_id = $2").bind(id).bind(user.workspace_id).execute(&state.db).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Sends a `ping` right away and reports how it went.
pub async fn test_webhook(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(id): Path<Uuid>) -> ApiResult<Json<Value>> {
    require_admin(&user)?;
    let row: Option<(String, String)> = sqlx::query_as("SELECT url, secret FROM webhooks WHERE id = $1 AND workspace_id = $2")
        .bind(id)
        .bind(user.workspace_id)
        .fetch_optional(&state.db)
        .await?;
    let (url, secret) = row.ok_or(ApiError::NotFound("webhook"))?;
    let payload = json!({"event": "ping", "timestamp": chrono::Utc::now(), "webhookId": id, "deliveryId": new_id(), "data": {}});
    let result = crate::webhooks::deliver(&state, &url, &secret, &payload).await;
    sqlx::query("UPDATE webhooks SET last_status = $2, last_error = $3, last_sent_at = now() WHERE id = $1")
        .bind(id)
        .bind(result.as_ref().ok().map(|s| *s as i32))
        .bind(result.as_ref().err())
        .execute(&state.db)
        .await?;
    Ok(Json(match result {
        Ok(status) => json!({"ok": true, "status": status}),
        Err(e) => json!({"ok": false, "error": e}),
    }))
}
