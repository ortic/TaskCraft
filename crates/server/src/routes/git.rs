//! Git integration endpoints: the webhook receivers (`POST /integrations/git/{provider}/{key}`),
//! connected repositories and rules in project settings, and an issue's development links.

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use super::issues::issue_for;
use super::projects::by_key;
use crate::access::{self, Role};
use crate::app::{AppState, audit, new_id};
use crate::auth::{CurrentUser, User, random_token};
use crate::error::{ApiError, ApiResult};
use crate::events::{self, Event};
use crate::git::{self, Hook};

fn header<'a>(h: &'a HeaderMap, name: &str) -> Option<&'a str> {
    h.get(name).and_then(|v| v.to_str().ok())
}

async fn note(state: &AppState, hook: Uuid, event: &str, error: Option<&str>) {
    let _ = sqlx::query("UPDATE git_integrations SET deliveries = deliveries + 1, last_event = $2, last_event_at = now(), last_error = $3 WHERE id = $1")
        .bind(hook)
        .bind(event)
        .bind(error)
        .execute(&state.db)
        .await;
}

/// `POST /integrations/git/{provider}/{key}`: a delivery from GitHub, GitLab or another server.
pub async fn receive(State(state): State<AppState>, Path((provider, key)): Path<(String, String)>, headers: HeaderMap, body: Bytes) -> ApiResult<Json<Value>> {
    let hook: Hook = sqlx::query_as(
        "SELECT g.id, g.project_id, p.workspace_id, g.provider, g.repo_url, g.secret FROM git_integrations g JOIN projects p ON p.id = g.project_id
          WHERE g.hook_key = $1 AND g.provider = $2",
    )
    .bind(&key)
    .bind(&provider)
    .fetch_optional(&state.db)
    .await?
    .ok_or(ApiError::NotFound("integration"))?;
    let signature = header(&headers, "x-hub-signature-256");
    let authentic = match provider.as_str() {
        "github" => signature.is_some_and(|s| git::signature_valid(&hook.secret, &body, s)),
        "gitlab" => header(&headers, "x-gitlab-token").is_some_and(|t| git::token_valid(&hook.secret, t)),
        _ => {
            let token = header(&headers, "x-taskcraft-token").or_else(|| header(&headers, "authorization").and_then(|a| a.strip_prefix("Bearer ")));
            token.is_some_and(|t| git::token_valid(&hook.secret, t)) || signature.is_some_and(|s| git::signature_valid(&hook.secret, &body, s))
        }
    };
    if !authentic {
        let why = match provider.as_str() {
            "github" => "the X-Hub-Signature-256 signature doesn't match the secret",
            "gitlab" => "the X-Gitlab-Token doesn't match the secret",
            _ => "neither an X-TaskCraft-Token nor an X-Hub-Signature-256 matching the secret",
        };
        note(&state, hook.id, "rejected", Some(why)).await;
        return Err(ApiError::Unauthorized);
    }
    let event = match provider.as_str() {
        "github" => {
            header(&headers, "x-github-event").or_else(|| header(&headers, "x-gitea-event")).or_else(|| header(&headers, "x-forgejo-event")).unwrap_or("")
        }
        "gitlab" => header(&headers, "x-gitlab-event").unwrap_or(""),
        _ => "generic",
    }
    .to_string();
    let payload: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            let msg = format!("the body isn't JSON (send it as application/json): {e}");
            note(&state, hook.id, &event, Some(&msg)).await;
            return Err(ApiError::BadRequest(msg));
        }
    };
    let parsed = match provider.as_str() {
        "github" => git::parse_github(&event, &payload, &hook.repo_url),
        "gitlab" => git::parse_gitlab(&event, &payload, &hook.repo_url),
        _ => git::parse_generic(&payload, &hook.repo_url).map(Some),
    };
    let activity = match parsed {
        Ok(Some(a)) => a,
        Ok(None) => {
            note(&state, hook.id, &event, None).await;
            return Ok(Json(json!({"ok": true, "ignored": event})));
        }
        Err(e) => {
            note(&state, hook.id, &event, Some(&e)).await;
            return Err(ApiError::BadRequest(e));
        }
    };
    match git::process(&state, &hook, &activity).await {
        Ok(out) => {
            note(&state, hook.id, &event, None).await;
            Ok(Json(json!({"ok": true, "event": event, "issues": out.issues, "newLinks": out.new_links, "moved": out.moved})))
        }
        Err(e) => {
            note(&state, hook.id, &event, Some("processing failed (see the server log)")).await;
            Err(e)
        }
    }
}

fn webhook_url(state: &AppState, provider: &str, key: &str) -> String {
    format!("{}/api/integrations/git/{provider}/{key}", state.config.base())
}

#[derive(sqlx::FromRow)]
struct Row {
    id: Uuid,
    provider: String,
    repo_url: String,
    hook_key: String,
    created_at: chrono::DateTime<chrono::Utc>,
    deliveries: i32,
    last_event: Option<String>,
    last_event_at: Option<chrono::DateTime<chrono::Utc>>,
    last_error: Option<String>,
}

fn row_json(state: &AppState, r: &Row) -> Value {
    json!({
        "id": r.id, "provider": r.provider, "repoUrl": r.repo_url, "repo": git::repo_name(&r.repo_url),
        "webhookUrl": webhook_url(state, &r.provider, &r.hook_key), "createdAt": r.created_at,
        "deliveries": r.deliveries, "lastEvent": r.last_event, "lastEventAt": r.last_event_at, "lastError": r.last_error,
    })
}

/// `GET /projects/{key}/git`: connected repositories and the rules (project admins).
pub async fn settings(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(key): Path<String>) -> ApiResult<Json<Value>> {
    let (p, _) = by_key(&state, &user, &key, Role::Admin).await?;
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT id, provider, repo_url, hook_key, created_at, deliveries, last_event, last_event_at, last_error FROM git_integrations WHERE project_id = $1 ORDER BY created_at",
    )
    .bind(p.id)
    .fetch_all(&state.db)
    .await?;
    let rules: Option<(Option<Uuid>, Option<Uuid>)> =
        sqlx::query_as("SELECT merged_status_id, opened_status_id FROM git_rules WHERE project_id = $1").bind(p.id).fetch_optional(&state.db).await?;
    let (merged, opened) = rules.unwrap_or_default();
    Ok(Json(json!({
        "integrations": rows.iter().map(|r| row_json(&state, r)).collect::<Vec<_>>(),
        "rules": {"mergedStatusId": merged, "openedStatusId": opened},
    })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewIntegration {
    provider: String,
    repo_url: String,
}

/// `POST /projects/{key}/git`: connects a repository; the answer carries the secret, once.
pub async fn create(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Path(key): Path<String>,
    Json(req): Json<NewIntegration>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let (p, _) = by_key(&state, &user, &key, Role::Admin).await?;
    if !git::PROVIDERS.contains(&req.provider.as_str()) {
        return Err(ApiError::bad("provider must be github, gitlab or generic"));
    }
    let repo = git::normalize_repo_url(&req.repo_url).ok_or_else(|| ApiError::bad("the repository needs its web address (https://…)"))?;
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM git_integrations WHERE project_id = $1").bind(p.id).fetch_one(&state.db).await?;
    if count >= 50 {
        return Err(ApiError::bad("a project can connect up to 50 repositories"));
    }
    let id = new_id();
    let secret = random_token();
    sqlx::query("INSERT INTO git_integrations (id, project_id, provider, repo_url, hook_key, secret, created_by) VALUES ($1, $2, $3, $4, $5, $6, $7)")
        .bind(id)
        .bind(p.id)
        .bind(&req.provider)
        .bind(&repo)
        .bind(random_token())
        .bind(&secret)
        .bind(user.id)
        .execute(&state.db)
        .await?;
    audit(&state, user.workspace_id, Some(user.id), "git.connected", Some(p.key.clone()), json!({"provider": req.provider, "repo": repo})).await;
    let row = load(&state, id).await?;
    let mut v = row_json(&state, &row);
    v["secret"] = json!(secret);
    Ok((StatusCode::CREATED, Json(v)))
}

async fn load(state: &AppState, id: Uuid) -> ApiResult<Row> {
    Ok(sqlx::query_as(
        "SELECT id, provider, repo_url, hook_key, created_at, deliveries, last_event, last_event_at, last_error FROM git_integrations WHERE id = $1",
    )
    .bind(id)
    .fetch_one(&state.db)
    .await?)
}

/// The integration's project, for its admins.
async fn integration_for(state: &AppState, user: &User, id: Uuid) -> ApiResult<(Uuid, String)> {
    let row: Option<(Uuid, String)> =
        sqlx::query_as("SELECT g.project_id, p.key FROM git_integrations g JOIN projects p ON p.id = g.project_id WHERE g.id = $1 AND p.workspace_id = $2")
            .bind(id)
            .bind(user.workspace_id)
            .fetch_optional(&state.db)
            .await?;
    let (project, key) = row.ok_or(ApiError::NotFound("integration"))?;
    access::require(state, user, project, Role::Admin).await?;
    Ok((project, key))
}

/// `DELETE /git-integrations/{id}`: disconnects a repository (its links stay on the issues).
pub async fn delete(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    let (_, key) = integration_for(&state, &user, id).await?;
    sqlx::query("DELETE FROM git_integrations WHERE id = $1").bind(id).execute(&state.db).await?;
    audit(&state, user.workspace_id, Some(user.id), "git.disconnected", Some(key), json!({"id": id})).await;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /git-integrations/{id}/secret`: a new secret (and webhook URL), shown once.
pub async fn rotate(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(id): Path<Uuid>) -> ApiResult<Json<Value>> {
    let (_, key) = integration_for(&state, &user, id).await?;
    let secret = random_token();
    sqlx::query("UPDATE git_integrations SET secret = $2, hook_key = $3, last_error = NULL WHERE id = $1")
        .bind(id)
        .bind(&secret)
        .bind(random_token())
        .execute(&state.db)
        .await?;
    audit(&state, user.workspace_id, Some(user.id), "git.secret", Some(key), json!({"id": id})).await;
    let mut v = row_json(&state, &load(&state, id).await?);
    v["secret"] = json!(secret);
    Ok(Json(v))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Rules {
    merged_status_id: Option<Uuid>,
    opened_status_id: Option<Uuid>,
}

/// `PUT /projects/{key}/git/rules`: `{"mergedStatusId": …, "openedStatusId": …}` (null: off).
pub async fn put_rules(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Path(key): Path<String>,
    Json(req): Json<Rules>,
) -> ApiResult<StatusCode> {
    let (p, _) = by_key(&state, &user, &key, Role::Admin).await?;
    for s in [req.merged_status_id, req.opened_status_id].into_iter().flatten() {
        let ok: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM statuses WHERE id = $1 AND project_id = $2)").bind(s).bind(p.id).fetch_one(&state.db).await?;
        if !ok {
            return Err(ApiError::bad("unknown status for this project"));
        }
    }
    sqlx::query(
        "INSERT INTO git_rules (project_id, merged_status_id, opened_status_id) VALUES ($1, $2, $3)
         ON CONFLICT (project_id) DO UPDATE SET merged_status_id = $2, opened_status_id = $3, updated_at = now()",
    )
    .bind(p.id)
    .bind(req.merged_status_id)
    .bind(req.opened_status_id)
    .execute(&state.db)
    .await?;
    events::publish(&state, Event::new(user.workspace_id, "project").project(p.id).actor(user.id)).await;
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /issues/{key}/development`: the issue's branches, commits and pull requests, whether its
/// project has a repository connected, and a branch name to start work with.
pub async fn development(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(key): Path<String>) -> ApiResult<Json<Value>> {
    let (id, project, _) = issue_for(&state, &user, &key, Role::Viewer).await?;
    let (ikey, title): (String, String) =
        sqlx::query_as("SELECT p.key || '-' || i.number, i.title FROM issues i JOIN projects p ON p.id = i.project_id WHERE i.id = $1")
            .bind(id)
            .fetch_one(&state.db)
            .await?;
    let connected: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM git_integrations WHERE project_id = $1)").bind(project).fetch_one(&state.db).await?;
    Ok(Json(json!({
        "links": git::links(&state, id).await?,
        "connected": connected,
        "branchName": git::branch_name(&ikey, &title),
    })))
}
