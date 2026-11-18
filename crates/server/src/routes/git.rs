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
    // what it says about a pull request's checks, reviews and draft state
    let status = match provider.as_str() {
        "github" => git::status_github(&event, &payload),
        "gitlab" => git::status_gitlab(&event, &payload),
        _ => None,
    };
    let activity = match parsed {
        Ok(Some(a)) => a,
        Ok(None) => {
            note(&state, hook.id, &event, None).await;
            if let Some(s) = &status {
                let updated = git::apply_status(&state, &hook, s).await?;
                return Ok(Json(json!({"ok": true, "event": event, "updated": updated})));
            }
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
            if let Some(s) = &status {
                git::apply_status(&state, &hook, s).await?;
            }
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
    api_kind: Option<String>,
    api_url: Option<String>,
    default_branch: Option<String>,
}

const ROW_SQL: &str = "SELECT id, provider, repo_url, hook_key, created_at, deliveries, last_event, last_event_at, last_error,
                              CASE WHEN api_token IS NOT NULL THEN api_kind END AS api_kind, api_url, default_branch FROM git_integrations";

fn row_json(state: &AppState, r: &Row) -> Value {
    json!({
        "id": r.id, "provider": r.provider, "repoUrl": r.repo_url, "repo": git::repo_name(&r.repo_url),
        "webhookUrl": webhook_url(state, &r.provider, &r.hook_key), "createdAt": r.created_at,
        "deliveries": r.deliveries, "lastEvent": r.last_event, "lastEventAt": r.last_event_at, "lastError": r.last_error,
        // the server's API (branches and pull requests from issues); the token is never shown
        "api": r.api_kind.as_ref().map(|k| json!({"kind": k, "url": r.api_url, "defaultBranch": r.default_branch})),
    })
}

/// `GET /projects/{key}/git`: connected repositories and the rules (project admins).
pub async fn settings(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(key): Path<String>) -> ApiResult<Json<Value>> {
    let (p, _) = by_key(&state, &user, &key, Role::Admin).await?;
    let rows: Vec<Row> =
        sqlx::query_as(sqlx::AssertSqlSafe(format!("{ROW_SQL} WHERE project_id = $1 ORDER BY created_at"))).bind(p.id).fetch_all(&state.db).await?;
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
    Ok(sqlx::query_as(sqlx::AssertSqlSafe(format!("{ROW_SQL} WHERE id = $1"))).bind(id).fetch_one(&state.db).await?)
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
    let (id, project, role) = issue_for(&state, &user, &key, Role::Viewer).await?;
    let (ikey, title): (String, String) =
        sqlx::query_as("SELECT p.key || '-' || i.number, i.title FROM issues i JOIN projects p ON p.id = i.project_id WHERE i.id = $1")
            .bind(id)
            .fetch_one(&state.db)
            .await?;
    let connected: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM git_integrations WHERE project_id = $1)").bind(project).fetch_one(&state.db).await?;
    // the repositories a branch can be created in, when the caller may
    let may = role >= Role::Member && access::require_action(&state, project, role, "createBranches").await.is_ok();
    let repos: Vec<(Uuid, String, String, Option<String>)> = if may {
        sqlx::query_as("SELECT id, repo_url, api_kind, default_branch FROM git_integrations WHERE project_id = $1 AND api_token IS NOT NULL AND api_kind IS NOT NULL ORDER BY created_at")
            .bind(project)
            .fetch_all(&state.db)
            .await?
    } else {
        vec![]
    };
    Ok(Json(json!({
        "links": git::links(&state, id).await?,
        "connected": connected,
        "branchName": git::branch_name(&ikey, &title),
        "repositories": repos.iter().map(|(id, url, kind, base)| json!({"id": id, "repo": git::repo_name(url), "kind": kind, "defaultBranch": base})).collect::<Vec<_>>(),
    })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiAccess {
    /// github | gitlab | gitea (also Forgejo)
    kind: Option<String>,
    /// null or empty: no access any more
    token: Option<String>,
    url: Option<String>,
}

/// `PUT /git-integrations/{id}/api`: the token TaskCraft creates branches and draft pull requests
/// with (project admins). It's tried at once: the answer names the repository's default branch.
pub async fn put_api(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<Uuid>,
    Json(req): Json<ApiAccess>,
) -> ApiResult<Json<Value>> {
    let (_, key) = integration_for(&state, &user, id).await?;
    let row = load(&state, id).await?;
    let token = req.token.as_deref().map(str::trim).filter(|t| !t.is_empty());
    let Some(token) = token else {
        sqlx::query("UPDATE git_integrations SET api_token = NULL, api_kind = NULL, api_url = NULL, default_branch = NULL WHERE id = $1")
            .bind(id)
            .execute(&state.db)
            .await?;
        audit(&state, user.workspace_id, Some(user.id), "git.api", Some(key), json!({"id": id, "on": false})).await;
        return Ok(Json(row_json(&state, &load(&state, id).await?)));
    };
    if token.len() > 500 {
        return Err(ApiError::bad("that doesn't look like a token"));
    }
    let kind = req.kind.as_deref().unwrap_or(if row.provider == "gitlab" { "gitlab" } else { "github" });
    let url = req.url.as_deref().map(str::trim).filter(|u| !u.is_empty());
    let repo = crate::gitapi::Repo::new(kind, &row.repo_url, url, token).map_err(ApiError::BadRequest)?;
    let branch = repo.default_branch(&state).await.map_err(|e| ApiError::bad(e.to_string()))?;
    sqlx::query("UPDATE git_integrations SET api_kind = $2, api_url = $3, api_token = $4, default_branch = $5 WHERE id = $1")
        .bind(id)
        .bind(kind)
        .bind(url)
        .bind(token)
        .bind(&branch)
        .execute(&state.db)
        .await?;
    audit(&state, user.workspace_id, Some(user.id), "git.api", Some(key), json!({"id": id, "on": true, "kind": kind})).await;
    Ok(Json(row_json(&state, &load(&state, id).await?)))
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct NewBranch {
    /// which repository (needed when the project has several)
    pub integration_id: Option<Uuid>,
    /// default: the suggested name
    pub name: Option<String>,
    /// default: the repository's default branch
    pub base: Option<String>,
    /// also open a draft pull / merge request
    #[serde(default)]
    pub pull_request: bool,
}

/// `POST /issues/{key}/branch`: creates a branch for the issue on the Git server — and a draft
/// pull request, if asked — and links it right away, before anything is pushed.
pub async fn create_branch_route(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Path(key): Path<String>,
    Json(req): Json<NewBranch>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    Ok((StatusCode::CREATED, Json(create_branch(&state, &user, &key, req).await?)))
}

/// Creates the branch as `user` (the REST route and the MCP tool).
pub async fn create_branch(state: &AppState, user: &User, key: &str, req: NewBranch) -> ApiResult<Value> {
    let (id, project, role) = issue_for(state, user, key, Role::Member).await?;
    access::require_action(state, project, role, "createBranches").await?;
    let (ikey, title, workspace): (String, String, Uuid) =
        sqlx::query_as("SELECT p.key || '-' || i.number, i.title, p.workspace_id FROM issues i JOIN projects p ON p.id = i.project_id WHERE i.id = $1")
            .bind(id)
            .fetch_one(&state.db)
            .await?;
    type Conn = (Uuid, String, String, Option<String>, String, Option<String>, String);
    let conns: Vec<Conn> = sqlx::query_as(
        "SELECT id, repo_url, api_kind, api_url, api_token, default_branch, secret FROM git_integrations
          WHERE project_id = $1 AND api_token IS NOT NULL AND api_kind IS NOT NULL ORDER BY created_at",
    )
    .bind(project)
    .fetch_all(&state.db)
    .await?;
    let conn = match req.integration_id {
        Some(i) => conns.into_iter().find(|c| c.0 == i).ok_or_else(|| ApiError::bad("that repository isn't connected for creating branches"))?,
        None if conns.len() == 1 => conns.into_iter().next().unwrap(),
        None if conns.is_empty() => {
            return Err(ApiError::bad("no repository of this project is connected for creating branches (Project settings → Integrations → Git)"));
        }
        None => return Err(ApiError::bad("the project has several repositories: say which one (integrationId)")),
    };
    let (integration, repo_url, kind, api_url, token, default_branch, secret) = conn;
    let name = req.name.as_deref().map(str::trim).filter(|n| !n.is_empty()).map(str::to_string).unwrap_or_else(|| git::branch_name(&ikey, &title));
    if !crate::gitapi::valid_branch(&name) {
        return Err(ApiError::bad("that isn't a branch name Git accepts (no spaces, .., ~ ^ : ? * [ or \\)"));
    }
    let base = req.base.as_deref().map(str::trim).filter(|b| !b.is_empty()).map(str::to_string).or(default_branch).unwrap_or_else(|| "main".into());
    if !crate::gitapi::valid_branch(&base) {
        return Err(ApiError::bad("the base isn't a branch name"));
    }
    let repo = crate::gitapi::Repo::new(&kind, &repo_url, api_url.as_deref(), &token).map_err(ApiError::BadRequest)?;
    let refused = |e: crate::gitapi::GitError| match e {
        crate::gitapi::GitError::Exists(m) => ApiError::Conflict(m),
        crate::gitapi::GitError::Other(m) => ApiError::bad(m),
    };
    repo.create_branch(state, &name, &base).await.map_err(refused)?;
    // linked like a pushed branch would be (the same address: a later push doesn't double it)
    let hook = git::Hook { id: integration, project_id: project, workspace_id: workspace, provider: kind.clone(), repo_url: repo_url.clone(), secret };
    let author = Some(user.name.clone());
    git::link_created(state, &hook, id, user.id, "branch", &name, &repo.branch_url(&name), &name, None, author.as_deref()).await?;
    let mut out = json!({"branch": {"name": name, "url": repo.branch_url(&name), "base": base}});
    if req.pull_request {
        let body = format!("{}\n\n{}", state.config.issue_url(&ikey), title);
        match repo.draft_pull_request(state, &name, &base, &format!("{ikey} {title}"), &body, &format!("{ikey}: start work")).await {
            Ok((number, url)) => {
                git::link_created(state, &hook, id, user.id, "pull_request", &format!("{ikey} {title}"), &url, &name, number, author.as_deref()).await?;
                out["pullRequest"] = json!({"number": number, "url": url});
            }
            // the branch is there: say what's missing instead of failing the whole thing
            Err(e) => out["warning"] = json!(format!("The branch was created, but not the pull request: {e}")),
        }
    }
    git::after_created(state, &hook, id, &ikey).await;
    out["links"] = json!(git::links(state, id).await?);
    Ok(out)
}
