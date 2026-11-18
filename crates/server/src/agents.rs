//! AI coding agents: hand an issue to an agent that prepares a pull request.
//!
//! An **agent** (admins define them) is a person of its own kind: a name and `@handle`,
//! instructions, the repository and base branch it works in, optionally another model than the
//! server's. Assigning an issue to it, or mentioning it in a comment, queues a **run** — one at a
//! time per agent and issue. A **runner** (the separate `taskcraft-runner` program, registered
//! with a one-time token) claims the run, fetches the repository through TaskCraft, lets the model
//! work on it and hands the result back: changed files become a branch and a draft pull request
//! on the Git server, linked to the issue; a question that needs no change becomes a comment.
//!
//! Credentials stay here. The runner never sees the Git token or the model key: it gets a token
//! for that one run, with which it can only download the source, ask the model (through this
//! server, counted per run), write its log and finish. The agent sees what its account may see —
//! private issues only where it's allowed — and everything it does is its own in the history and
//! the audit log. `testing` agents start only for workspace admins.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use base64::Engine;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::access::Role;
use crate::app::{AppState, audit, new_id};
use crate::auth::{CurrentUser, User, random_token, require_admin, token_hash};
use crate::error::{ApiError, ApiResult};
use crate::events::{self, Event};
use crate::gitapi::{FileChange, GitError, Repo};
use crate::issues::{Actor, Summary};

const RUNNER_PREFIX: &str = "tcr_";
const RUN_PREFIX: &str = "tcx_";
const MAX_MODEL_CALLS: i32 = 300;
const MAX_FILES: usize = 300;
const MAX_CHANGE_BYTES: usize = 5 * 1024 * 1024;
const MAX_SOURCE_BYTES: usize = 200 * 1024 * 1024;
/// A running run that hasn't been heard of for this long has lost its runner.
const STALE_MINUTES: i32 = 30;

// ---- starting runs ------------------------------------------------------------------------------

/// Queues a run when `agent_user` is an agent that may be started here by `by`. Returns whether
/// one was queued (not when one is queued or running already).
async fn start(state: &AppState, agent_user: Uuid, s: &Summary, trigger: &str, by: Option<Uuid>, prompt: Option<&str>) -> ApiResult<bool> {
    let agent: Option<(Uuid, Uuid, String)> =
        sqlx::query_as("SELECT id, workspace_id, state FROM agents WHERE user_id = $1").bind(agent_user).fetch_optional(&state.db).await?;
    let Some((agent, workspace, agent_state)) = agent else { return Ok(false) };
    if agent_state == "off" || !crate::features::on(state, workspace, "agents").await {
        return Ok(false);
    }
    // whoever starts it is a person; an agent being tried out listens to workspace admins only
    let Some(by) = by.filter(|b| *b != agent_user) else { return Ok(false) };
    if agent_state == "testing" {
        let admin: bool = sqlx::query_scalar("SELECT role IN ('owner', 'admin') FROM users WHERE id = $1").bind(by).fetch_one(&state.db).await?;
        if !admin {
            return Ok(false);
        }
    }
    // the agent works on what its account may see
    let sees: bool = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT EXISTS (SELECT 1 FROM issues i JOIN LATERAL tc_project_roles($2) r ON r.project_id = i.project_id AND r.rank >= 2 WHERE i.id = $1 AND {})",
        crate::access::sees_issue("i", "$2")
    )))
    .bind(s.id)
    .bind(agent_user)
    .fetch_one(&state.db)
    .await?;
    if !sees {
        return Ok(false);
    }
    let queued = sqlx::query("INSERT INTO agent_runs (id, workspace_id, agent_id, issue_id, trigger, triggered_by, prompt) VALUES ($1, $2, $3, $4, $5, $6, $7) ON CONFLICT DO NOTHING")
        .bind(new_id())
        .bind(workspace)
        .bind(agent)
        .bind(s.id)
        .bind(trigger)
        .bind(by)
        .bind(prompt.map(|p| p.chars().take(8000).collect::<String>()))
        .execute(&state.db)
        .await?
        .rows_affected()
        > 0;
    if queued {
        audit(state, workspace, Some(by), "agent.run.started", Some(s.key.clone()), json!({"agent": agent, "trigger": trigger})).await;
        events::publish(state, Event::new(workspace, "issue").project(s.project_id).issue(s.id, &s.key)).await;
    }
    Ok(queued)
}

/// After an issue was assigned: its new assignee may be an agent.
pub async fn assigned(state: &AppState, actor: &Actor, s: &Summary) {
    if let Some(a) = &s.assignee
        && let Err(e) = start(state, a.id, s, "assigned", actor.user, None).await
    {
        tracing::warn!(error = %e, "starting an agent");
    }
}

/// After a comment: the agents it mentions (`@handle`).
pub async fn mentioned(state: &AppState, actor: &Actor, s: &Summary, body: &Value) {
    for person in crate::doc::mentioned_people(body) {
        if let Err(e) = start(state, person, s, "mentioned", actor.user, Some(&crate::doc::to_markdown(body))).await {
            tracing::warn!(error = %e, "starting an agent");
        }
    }
}

/// Once a minute: runs whose runner went away.
pub async fn tick(state: &AppState) -> anyhow::Result<()> {
    let lost: Vec<(Uuid,)> = sqlx::query_as(
        "UPDATE agent_runs SET state = 'failed', finished_at = now(), token_hash = NULL, error = 'the runner stopped answering'
          WHERE state = 'running' AND COALESCE(heartbeat_at, started_at) < now() - make_interval(mins => $1) RETURNING id",
    )
    .bind(STALE_MINUTES)
    .fetch_all(&state.db)
    .await?;
    for (id,) in lost {
        changed(state, id).await;
    }
    Ok(())
}

/// Screens showing the run's issue refresh.
async fn changed(state: &AppState, run: Uuid) {
    let row: Option<(Uuid, Uuid, Uuid, String)> = sqlx::query_as(
        "SELECT r.workspace_id, i.project_id, i.id, p.key || '-' || i.number FROM agent_runs r JOIN issues i ON i.id = r.issue_id JOIN projects p ON p.id = i.project_id WHERE r.id = $1",
    )
    .bind(run)
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten();
    if let Some((workspace, project, issue, key)) = row {
        events::publish(state, Event::new(workspace, "issue").project(project).issue(issue, &key)).await;
    }
}

// ---- what people see of runs --------------------------------------------------------------------

#[derive(sqlx::FromRow)]
struct RunRow {
    id: Uuid,
    agent_name: String,
    handle: String,
    state: String,
    trigger: String,
    triggered_by: Option<Uuid>,
    triggered_by_name: Option<String>,
    created_at: DateTime<Utc>,
    started_at: Option<DateTime<Utc>>,
    finished_at: Option<DateTime<Utc>>,
    result: Option<Value>,
    error: Option<String>,
    model_calls: i32,
    tokens_in: i64,
    tokens_out: i64,
    issue_key: String,
    issue_title: String,
}

const RUN_SQL: &str =
    "SELECT r.id, au.name AS agent_name, a.handle, r.state, r.trigger, r.triggered_by, tu.name AS triggered_by_name, r.created_at, r.started_at, r.finished_at,
                              r.result, r.error, r.model_calls, r.tokens_in, r.tokens_out, p.key || '-' || i.number AS issue_key, i.title AS issue_title
                         FROM agent_runs r JOIN agents a ON a.id = r.agent_id JOIN users au ON au.id = a.user_id LEFT JOIN users tu ON tu.id = r.triggered_by
                         JOIN issues i ON i.id = r.issue_id JOIN projects p ON p.id = i.project_id";

fn run_json(r: &RunRow, can_cancel: bool) -> Value {
    json!({
        "id": r.id, "agent": {"name": r.agent_name, "handle": r.handle}, "state": r.state, "trigger": r.trigger,
        "triggeredBy": r.triggered_by.map(|id| json!({"id": id, "name": r.triggered_by_name})),
        "createdAt": r.created_at, "startedAt": r.started_at, "finishedAt": r.finished_at, "result": r.result, "error": r.error,
        "modelCalls": r.model_calls, "tokensIn": r.tokens_in, "tokensOut": r.tokens_out,
        "issue": {"key": r.issue_key, "title": r.issue_title},
        "canCancel": can_cancel && matches!(r.state.as_str(), "queued" | "running"),
    })
}

/// `GET /issues/{key}/agent-runs`: the agents' work on an issue, latest first.
pub async fn for_issue(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(key): Path<String>) -> ApiResult<Json<Value>> {
    let (id, _, role) = crate::routes::issues::issue_for(&state, &user, &key, Role::Viewer).await?;
    let rows: Vec<RunRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!("{RUN_SQL} WHERE r.issue_id = $1 ORDER BY r.created_at DESC LIMIT 20")))
        .bind(id)
        .fetch_all(&state.db)
        .await?;
    Ok(Json(json!(rows.iter().map(|r| run_json(r, role >= Role::Admin || user.is_admin() || r.triggered_by == Some(user.id))).collect::<Vec<_>>())))
}

/// The run, when the caller sees its issue: (issue, their role in its project, who started it).
async fn visible(state: &AppState, user: &User, run: Uuid) -> ApiResult<(Uuid, Role, Option<Uuid>)> {
    let row: Option<(Uuid, Option<Uuid>)> = sqlx::query_as("SELECT issue_id, triggered_by FROM agent_runs WHERE id = $1 AND workspace_id = $2")
        .bind(run)
        .bind(user.workspace_id)
        .fetch_optional(&state.db)
        .await?;
    let (issue, by) = row.ok_or(ApiError::NotFound("run"))?;
    let (_, _, role) = crate::issues::resolve(state, user, &issue.to_string(), false).await?;
    Ok((issue, role, by))
}

#[derive(Deserialize)]
pub struct LogQuery {
    after: Option<i64>,
}

/// `GET /agent-runs/{id}/log?after=`: the run's state and its log lines (after a line's id).
pub async fn log(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(id): Path<Uuid>, Query(q): Query<LogQuery>) -> ApiResult<Json<Value>> {
    visible(&state, &user, id).await?;
    let run_state: String = sqlx::query_scalar("SELECT state FROM agent_runs WHERE id = $1").bind(id).fetch_one(&state.db).await?;
    let lines: Vec<(i64, DateTime<Utc>, String)> =
        sqlx::query_as("SELECT id, at, line FROM agent_run_logs WHERE run_id = $1 AND id > $2 ORDER BY id LIMIT 1000")
            .bind(id)
            .bind(q.after.unwrap_or(0))
            .fetch_all(&state.db)
            .await?;
    Ok(Json(json!({"state": run_state, "lines": lines.iter().map(|(id, at, line)| json!({"id": id, "at": at, "line": line})).collect::<Vec<_>>()})))
}

/// `POST /agent-runs/{id}/cancel`: whoever started it, project admins and workspace admins.
pub async fn cancel(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    let (_, role, by) = visible(&state, &user, id).await?;
    if !(role >= Role::Admin || user.is_admin() || by == Some(user.id)) {
        return Err(ApiError::Denied("whoever started the agent, or an admin, can stop it".into()));
    }
    let done =
        sqlx::query("UPDATE agent_runs SET state = 'cancelled', finished_at = now(), token_hash = NULL WHERE id = $1 AND state IN ('queued', 'running')")
            .bind(id)
            .execute(&state.db)
            .await?;
    if done.rows_affected() == 0 {
        return Err(ApiError::Conflict("the run has ended already".into()));
    }
    sqlx::query("INSERT INTO agent_run_logs (run_id, line) VALUES ($1, $2)").bind(id).bind(format!("Cancelled by {}.", user.name)).execute(&state.db).await?;
    audit(&state, user.workspace_id, Some(user.id), "agent.run.cancelled", Some(id.to_string()), json!({})).await;
    changed(&state, id).await;
    Ok(StatusCode::NO_CONTENT)
}

// ---- administration ---------------------------------------------------------------------------------

fn clean_handle(h: &str) -> ApiResult<String> {
    let h = h.trim().trim_start_matches('@').to_lowercase();
    if h.len() < 2 || h.len() > 30 || !h.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_') {
        return Err(ApiError::bad("a handle is 2 to 30 lower-case letters, digits, - or _"));
    }
    Ok(h)
}

/// The repository an agent works in must be connected with API access, in this workspace.
async fn check_repo(state: &AppState, workspace: Uuid, v: &Value) -> ApiResult<Option<Uuid>> {
    if v.is_null() {
        return Ok(None);
    }
    let id: Uuid = v.as_str().and_then(|s| s.parse().ok()).ok_or_else(|| ApiError::bad("integrationId is a connected repository's id"))?;
    let ok: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM git_integrations g JOIN projects p ON p.id = g.project_id WHERE g.id = $1 AND p.workspace_id = $2 AND g.api_token IS NOT NULL)")
        .bind(id)
        .bind(workspace)
        .fetch_one(&state.db)
        .await?;
    if !ok {
        return Err(ApiError::bad("that repository isn't connected with a token for creating branches (Project settings → Integrations → Git)"));
    }
    Ok(Some(id))
}

fn check_state(v: &Value) -> ApiResult<&str> {
    match v.as_str() {
        Some(s @ ("testing" | "active" | "off")) => Ok(s),
        _ => Err(ApiError::bad("state is testing, active or off")),
    }
}

/// `GET /admin/agents`: agents, runners and the repositories agents can work in.
pub async fn list(State(state): State<AppState>, CurrentUser(user): CurrentUser) -> ApiResult<Json<Value>> {
    require_admin(&user)?;
    type Row = (Uuid, Uuid, String, String, String, Option<String>, Option<Uuid>, Option<String>, String, Option<String>, i64, i64, i64, i64);
    let agents: Vec<Row> = sqlx::query_as(
        "SELECT a.id, a.user_id, u.name, a.handle, a.instructions, a.model, a.integration_id, a.base_branch, a.state, g.repo_url,
                (SELECT count(*) FROM agent_runs r WHERE r.agent_id = a.id),
                (SELECT count(*) FROM agent_runs r WHERE r.agent_id = a.id AND r.state IN ('queued', 'running')),
                COALESCE((SELECT sum(r.tokens_in) FROM agent_runs r WHERE r.agent_id = a.id), 0)::bigint,
                COALESCE((SELECT sum(r.tokens_out) FROM agent_runs r WHERE r.agent_id = a.id), 0)::bigint
           FROM agents a JOIN users u ON u.id = a.user_id LEFT JOIN git_integrations g ON g.id = a.integration_id WHERE a.workspace_id = $1 ORDER BY lower(u.name)",
    )
    .bind(user.workspace_id)
    .fetch_all(&state.db)
    .await?;
    let runners: Vec<(Uuid, String, Option<DateTime<Utc>>, Option<DateTime<Utc>>, DateTime<Utc>)> =
        sqlx::query_as("SELECT id, name, registered_at, last_seen_at, created_at FROM agent_runners WHERE workspace_id = $1 ORDER BY created_at")
            .bind(user.workspace_id)
            .fetch_all(&state.db)
            .await?;
    let repos: Vec<(Uuid, String, String, Option<String>)> = sqlx::query_as(
        "SELECT g.id, g.repo_url, p.key, g.default_branch FROM git_integrations g JOIN projects p ON p.id = g.project_id WHERE p.workspace_id = $1 AND g.api_token IS NOT NULL ORDER BY g.repo_url",
    )
    .bind(user.workspace_id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(json!({
        "agents": agents.iter().map(|(id, uid, name, handle, instructions, model, integration, base, st, repo, runs, active, tin, tout)| json!({
            "id": id, "userId": uid, "name": name, "handle": handle, "instructions": instructions, "model": model, "integrationId": integration, "baseBranch": base, "state": st,
            "repository": repo.as_deref().map(crate::git::repo_name), "runs": runs, "activeRuns": active, "tokensIn": tin, "tokensOut": tout,
        })).collect::<Vec<_>>(),
        "runners": runners.iter().map(|(id, name, registered, seen, created)| json!({"id": id, "name": name, "registeredAt": registered, "lastSeenAt": seen, "createdAt": created})).collect::<Vec<_>>(),
        "repositories": repos.iter().map(|(id, url, project, base)| json!({"id": id, "repo": crate::git::repo_name(url), "projectKey": project, "defaultBranch": base})).collect::<Vec<_>>(),
        // without a model on the server no agent can think
        "modelConfigured": crate::ai::configured(&state),
        "serverUrl": state.config.base(),
    })))
}

/// `POST /admin/agents` `{name, handle, instructions?, model?, integrationId?, baseBranch?, state?}`:
/// a new agent, with an account of its own (a member; give it project roles like anyone).
pub async fn create(State(state): State<AppState>, CurrentUser(user): CurrentUser, Json(req): Json<Value>) -> ApiResult<(StatusCode, Json<Value>)> {
    require_admin(&user)?;
    let name: String =
        req["name"].as_str().map(str::trim).filter(|n| !n.is_empty()).ok_or_else(|| ApiError::bad("give the agent a name"))?.chars().take(60).collect();
    let handle = clean_handle(req["handle"].as_str().unwrap_or(""))?;
    let integration = check_repo(&state, user.workspace_id, &req["integrationId"]).await?;
    let agent_state = if req["state"].is_null() { "testing" } else { check_state(&req["state"])? };
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM agents WHERE workspace_id = $1").bind(user.workspace_id).fetch_one(&state.db).await?;
    if count >= 50 {
        return Err(ApiError::bad("a workspace has up to 50 agents"));
    }
    let mut tx = state.db.begin().await?;
    let uid = new_id();
    sqlx::query("INSERT INTO users (id, workspace_id, name, role, status, agent, title) VALUES ($1, $2, $3, 'member', 'active', true, 'AI agent')")
        .bind(uid)
        .bind(user.workspace_id)
        .bind(&name)
        .execute(&mut *tx)
        .await?;
    let id = new_id();
    sqlx::query("INSERT INTO agents (id, workspace_id, user_id, handle, instructions, model, integration_id, base_branch, state, created_by) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)")
        .bind(id)
        .bind(user.workspace_id)
        .bind(uid)
        .bind(&handle)
        .bind(req["instructions"].as_str().unwrap_or("").chars().take(20_000).collect::<String>())
        .bind(req["model"].as_str().map(str::trim).filter(|m| !m.is_empty()))
        .bind(integration)
        .bind(req["baseBranch"].as_str().map(str::trim).filter(|b| !b.is_empty()))
        .bind(agent_state)
        .bind(user.id)
        .execute(&mut *tx)
        .await
        .map_err(|e| if e.as_database_error().is_some_and(|d| d.is_unique_violation()) { ApiError::Conflict(format!("the handle @{handle} is taken")) } else { e.into() })?;
    tx.commit().await?;
    audit(&state, user.workspace_id, Some(user.id), "agent.created", Some(handle), json!({"name": name})).await;
    Ok((StatusCode::CREATED, Json(json!({"id": id, "userId": uid}))))
}

/// `PATCH /admin/agents/{id}`: any of the fields of creating one.
pub async fn update(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(id): Path<Uuid>, Json(req): Json<Value>) -> ApiResult<StatusCode> {
    require_admin(&user)?;
    let uid: Uuid = sqlx::query_scalar("SELECT user_id FROM agents WHERE id = $1 AND workspace_id = $2")
        .bind(id)
        .bind(user.workspace_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(ApiError::NotFound("agent"))?;
    if let Some(n) = req["name"].as_str().map(str::trim).filter(|n| !n.is_empty()) {
        sqlx::query("UPDATE users SET name = $2 WHERE id = $1").bind(uid).bind(n.chars().take(60).collect::<String>()).execute(&state.db).await?;
    }
    if let Some(h) = req["handle"].as_str() {
        let h = clean_handle(h)?;
        sqlx::query("UPDATE agents SET handle = $2 WHERE id = $1").bind(id).bind(&h).execute(&state.db).await.map_err(|e| {
            if e.as_database_error().is_some_and(|d| d.is_unique_violation()) { ApiError::Conflict(format!("the handle @{h} is taken")) } else { e.into() }
        })?;
    }
    if let Some(i) = req["instructions"].as_str() {
        sqlx::query("UPDATE agents SET instructions = $2 WHERE id = $1").bind(id).bind(i.chars().take(20_000).collect::<String>()).execute(&state.db).await?;
    }
    if let Some(m) = req.get("model") {
        sqlx::query("UPDATE agents SET model = $2 WHERE id = $1").bind(id).bind(m.as_str().map(str::trim).filter(|m| !m.is_empty())).execute(&state.db).await?;
    }
    if let Some(v) = req.get("integrationId") {
        sqlx::query("UPDATE agents SET integration_id = $2 WHERE id = $1")
            .bind(id)
            .bind(check_repo(&state, user.workspace_id, v).await?)
            .execute(&state.db)
            .await?;
    }
    if let Some(b) = req.get("baseBranch") {
        sqlx::query("UPDATE agents SET base_branch = $2 WHERE id = $1")
            .bind(id)
            .bind(b.as_str().map(str::trim).filter(|b| !b.is_empty()))
            .execute(&state.db)
            .await?;
    }
    if let Some(s) = req.get("state") {
        sqlx::query("UPDATE agents SET state = $2 WHERE id = $1").bind(id).bind(check_state(s)?).execute(&state.db).await?;
    }
    audit(&state, user.workspace_id, Some(user.id), "agent.updated", Some(id.to_string()), json!({"state": req["state"]})).await;
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /admin/agents/{id}`: the agent goes; its account is deactivated, so what it wrote stays.
pub async fn remove(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    require_admin(&user)?;
    let uid: Option<Uuid> = sqlx::query_scalar("DELETE FROM agents WHERE id = $1 AND workspace_id = $2 RETURNING user_id")
        .bind(id)
        .bind(user.workspace_id)
        .fetch_optional(&state.db)
        .await?;
    let uid = uid.ok_or(ApiError::NotFound("agent"))?;
    sqlx::query("UPDATE users SET status = 'deactivated' WHERE id = $1").bind(uid).execute(&state.db).await?;
    sqlx::query("UPDATE issues i SET assignee_id = NULL FROM statuses s WHERE s.id = i.status_id AND s.category <> 'done' AND i.assignee_id = $1")
        .bind(uid)
        .execute(&state.db)
        .await?;
    audit(&state, user.workspace_id, Some(user.id), "agent.removed", Some(id.to_string()), json!({})).await;
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /admin/agents/{id}/runs`: what an agent did, with the tokens it took.
pub async fn runs(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(id): Path<Uuid>) -> ApiResult<Json<Value>> {
    require_admin(&user)?;
    let rows: Vec<RunRow> =
        sqlx::query_as(sqlx::AssertSqlSafe(format!("{RUN_SQL} WHERE r.agent_id = $1 AND r.workspace_id = $2 ORDER BY r.created_at DESC LIMIT 100")))
            .bind(id)
            .bind(user.workspace_id)
            .fetch_all(&state.db)
            .await?;
    Ok(Json(json!(rows.iter().map(|r| run_json(r, true)).collect::<Vec<_>>())))
}

/// `POST /admin/agent-runners` `{name}`: a runner to be; the answer has its one-time registration token.
pub async fn add_runner(State(state): State<AppState>, CurrentUser(user): CurrentUser, Json(req): Json<Value>) -> ApiResult<(StatusCode, Json<Value>)> {
    require_admin(&user)?;
    let name: String =
        req["name"].as_str().map(str::trim).filter(|n| !n.is_empty()).ok_or_else(|| ApiError::bad("give the runner a name"))?.chars().take(60).collect();
    let token = format!("{RUNNER_PREFIX}reg_{}", random_token());
    let id = new_id();
    sqlx::query("INSERT INTO agent_runners (id, workspace_id, name, token_hash, created_by) VALUES ($1, $2, $3, $4, $5)")
        .bind(id)
        .bind(user.workspace_id)
        .bind(&name)
        .bind(token_hash(&token))
        .bind(user.id)
        .execute(&state.db)
        .await?;
    audit(&state, user.workspace_id, Some(user.id), "agent.runner.added", Some(name), json!({})).await;
    Ok((StatusCode::CREATED, Json(json!({"id": id, "registrationToken": token}))))
}

/// `DELETE /admin/agent-runners/{id}`: the runner can't claim work any more.
pub async fn remove_runner(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    require_admin(&user)?;
    let name: Option<String> = sqlx::query_scalar("DELETE FROM agent_runners WHERE id = $1 AND workspace_id = $2 RETURNING name")
        .bind(id)
        .bind(user.workspace_id)
        .fetch_optional(&state.db)
        .await?;
    audit(&state, user.workspace_id, Some(user.id), "agent.runner.removed", Some(name.ok_or(ApiError::NotFound("runner"))?), json!({})).await;
    Ok(StatusCode::NO_CONTENT)
}

// ---- the runner's side ------------------------------------------------------------------------------

fn bearer(headers: &HeaderMap, prefix: &str) -> ApiResult<String> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|t| t.starts_with(prefix))
        .map(str::to_string)
        .ok_or(ApiError::Unauthorized)
}

/// `POST /agent-runner/register` `{token, name?}`: trades the one-time registration token for the
/// runner's own.
pub async fn register(State(state): State<AppState>, Json(req): Json<Value>) -> ApiResult<Json<Value>> {
    let given = req["token"].as_str().unwrap_or("");
    if !given.starts_with(&format!("{RUNNER_PREFIX}reg_")) {
        return Err(ApiError::Unauthorized);
    }
    let token = format!("{RUNNER_PREFIX}{}", random_token());
    let row: Option<(Uuid, Uuid, String)> = sqlx::query_as(
        "UPDATE agent_runners SET token_hash = $2, registered_at = now(), last_seen_at = now(), name = COALESCE($3, name) WHERE token_hash = $1 AND registered_at IS NULL RETURNING id, workspace_id, name",
    )
    .bind(token_hash(given))
    .bind(token_hash(&token))
    .bind(req["name"].as_str().map(str::trim).filter(|n| !n.is_empty()).map(|n| n.chars().take(60).collect::<String>()))
    .fetch_optional(&state.db)
    .await?;
    let (id, workspace, name) = row.ok_or(ApiError::Unauthorized)?;
    audit(&state, workspace, None, "agent.runner.registered", Some(name.clone()), json!({})).await;
    Ok(Json(json!({"id": id, "name": name, "token": token})))
}

/// `POST /agent-runner/claim`: the oldest queued run of the runner's workspace, with what it
/// takes to work on it — or 204 when nothing waits.
pub async fn claim(State(state): State<AppState>, headers: HeaderMap) -> ApiResult<Response> {
    let token = bearer(&headers, RUNNER_PREFIX)?;
    let runner: Option<(Uuid, Uuid)> =
        sqlx::query_as("UPDATE agent_runners SET last_seen_at = now() WHERE token_hash = $1 AND registered_at IS NOT NULL RETURNING id, workspace_id")
            .bind(token_hash(&token))
            .fetch_optional(&state.db)
            .await?;
    let (runner, workspace) = runner.ok_or(ApiError::Unauthorized)?;
    if !crate::features::on(&state, workspace, "agents").await {
        return Ok(StatusCode::NO_CONTENT.into_response());
    }
    let run_token = format!("{RUN_PREFIX}{}", random_token());
    let run: Option<(Uuid, Uuid, Uuid, Option<String>, String)> = sqlx::query_as(
        "UPDATE agent_runs SET state = 'running', runner_id = $2, token_hash = $3, started_at = now(), heartbeat_at = now()
          WHERE id = (SELECT id FROM agent_runs WHERE workspace_id = $1 AND state = 'queued' ORDER BY created_at LIMIT 1 FOR UPDATE SKIP LOCKED)
          RETURNING id, agent_id, issue_id, prompt, trigger",
    )
    .bind(workspace)
    .bind(runner)
    .bind(token_hash(&run_token))
    .fetch_optional(&state.db)
    .await?;
    let Some((run, agent, issue, prompt, trigger)) = run else { return Ok(StatusCode::NO_CONTENT.into_response()) };
    let (name, handle, instructions, integration, base): (String, String, String, Option<Uuid>, Option<String>) = sqlx::query_as(
        "SELECT u.name, a.handle, a.instructions, a.integration_id, a.base_branch FROM agents a JOIN users u ON u.id = a.user_id WHERE a.id = $1",
    )
    .bind(agent)
    .fetch_one(&state.db)
    .await?;
    let s = crate::issues::summary(&state, crate::issues::load(&state, issue).await?);
    let description: String = sqlx::query_scalar("SELECT description_text FROM issues WHERE id = $1").bind(issue).fetch_one(&state.db).await?;
    // the conversation so far: what everyone may read (nothing restricted to a group)
    let comments: Vec<(Option<String>, String, DateTime<Utc>)> = sqlx::query_as(
        "SELECT u.name, c.body_text, c.created_at FROM comments c LEFT JOIN users u ON u.id = c.author_id WHERE c.issue_id = $1 AND c.visibility IS NULL ORDER BY c.created_at DESC LIMIT 30",
    )
    .bind(issue)
    .fetch_all(&state.db)
    .await?;
    let repo: Option<(String, Option<String>)> = match integration {
        Some(i) => {
            sqlx::query_as("SELECT repo_url, default_branch FROM git_integrations WHERE id = $1 AND api_token IS NOT NULL")
                .bind(i)
                .fetch_optional(&state.db)
                .await?
        }
        None => None,
    };
    sqlx::query("INSERT INTO agent_run_logs (run_id, line) VALUES ($1, 'Picked up by a runner.')").bind(run).execute(&state.db).await?;
    changed(&state, run).await;
    Ok(Json(json!({
        "run": {"id": run, "token": run_token, "trigger": trigger},
        "agent": {"name": name, "handle": handle, "instructions": instructions},
        "issue": {
            "key": s.key, "title": s.title, "type": s.kind.name, "status": s.status.name, "priority": s.priority, "labels": s.labels, "url": s.url, "description": description,
            "comments": comments.iter().rev().map(|(author, text, at)| json!({"author": author, "text": text, "at": at})).collect::<Vec<_>>(),
        },
        "prompt": prompt,
        "repository": repo.map(|(url, default)| json!({"name": crate::git::repo_name(&url), "baseBranch": base.or(default).unwrap_or_else(|| "main".into())})),
    }))
    .into_response())
}

/// A running run, by its own token: (run, workspace, agent, issue).
async fn running(state: &AppState, headers: &HeaderMap, id: Uuid) -> ApiResult<(Uuid, Uuid, Uuid)> {
    let token = bearer(headers, RUN_PREFIX)?;
    let row: Option<(Uuid, Uuid, Uuid, String)> =
        sqlx::query_as("SELECT workspace_id, agent_id, issue_id, state FROM agent_runs WHERE id = $1 AND token_hash = $2")
            .bind(id)
            .bind(token_hash(&token))
            .fetch_optional(&state.db)
            .await?;
    match row {
        Some((workspace, agent, issue, st)) if st == "running" => Ok((workspace, agent, issue)),
        // cancelled or timed out: the token is gone with it
        _ => Err(ApiError::Conflict("this run has ended".into())),
    }
}

/// The repository the run's agent works in.
async fn repo_of(state: &AppState, agent: Uuid) -> ApiResult<Option<(Repo, crate::git::Hook, String)>> {
    type Row = (Uuid, Uuid, Uuid, String, String, Option<String>, String, Option<String>, String, Option<String>);
    let row: Option<Row> = sqlx::query_as(
        "SELECT g.id, g.project_id, p.workspace_id, g.repo_url, g.api_kind, g.api_url, g.api_token, g.default_branch, g.secret, a.base_branch
           FROM agents a JOIN git_integrations g ON g.id = a.integration_id JOIN projects p ON p.id = g.project_id WHERE a.id = $1 AND g.api_token IS NOT NULL AND g.api_kind IS NOT NULL",
    )
    .bind(agent)
    .fetch_optional(&state.db)
    .await?;
    let Some((id, project_id, workspace_id, repo_url, kind, api_url, token, default, secret, base)) = row else { return Ok(None) };
    let repo = Repo::new(&kind, &repo_url, api_url.as_deref(), &token).map_err(ApiError::BadRequest)?;
    let hook = crate::git::Hook { id, project_id, workspace_id, provider: kind, repo_url, secret };
    Ok(Some((repo, hook, base.or(default).unwrap_or_else(|| "main".into()))))
}

/// `GET /agent-runs/{id}/source`: the repository at the base branch, as a .tar.gz — fetched by
/// this server with its token, so the runner needs none.
pub async fn source(State(state): State<AppState>, headers: HeaderMap, Path(id): Path<Uuid>) -> ApiResult<Response> {
    let (_, agent, _) = running(&state, &headers, id).await?;
    let (repo, _, base) = repo_of(&state, agent).await?.ok_or_else(|| ApiError::bad("the agent has no repository"))?;
    let bytes = repo.archive(&state, &base, MAX_SOURCE_BYTES).await.map_err(|e| ApiError::bad(e.to_string()))?;
    sqlx::query("UPDATE agent_runs SET heartbeat_at = now() WHERE id = $1").bind(id).execute(&state.db).await?;
    Ok(([(header::CONTENT_TYPE, "application/gzip")], bytes).into_response())
}

/// `POST /agent-runs/{id}/model` `{system, messages: [{role, content}]}`: one turn with the model,
/// asked by this server with its key and counted on the run.
pub async fn model(State(state): State<AppState>, headers: HeaderMap, Path(id): Path<Uuid>, Json(req): Json<Value>) -> ApiResult<Json<Value>> {
    let (_, agent, _) = running(&state, &headers, id).await?;
    let calls: i32 = sqlx::query_scalar("UPDATE agent_runs SET model_calls = model_calls + 1, heartbeat_at = now() WHERE id = $1 RETURNING model_calls")
        .bind(id)
        .fetch_one(&state.db)
        .await?;
    if calls > MAX_MODEL_CALLS {
        return Err(ApiError::TooMany(format!("a run asks the model at most {MAX_MODEL_CALLS} times")));
    }
    let messages: Vec<(String, String)> = req["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|m| (m["role"].as_str().unwrap_or("user").to_string(), m["content"].as_str().unwrap_or("").to_string()))
        .collect();
    if messages.is_empty() {
        return Err(ApiError::bad("messages is empty"));
    }
    let model: Option<String> = sqlx::query_scalar("SELECT model FROM agents WHERE id = $1").bind(agent).fetch_one(&state.db).await?;
    let (text, tokens_in, tokens_out) = crate::ai::complete(&state, model.as_deref(), req["system"].as_str().unwrap_or(""), &messages).await?;
    sqlx::query("UPDATE agent_runs SET tokens_in = tokens_in + $2, tokens_out = tokens_out + $3, heartbeat_at = now() WHERE id = $1")
        .bind(id)
        .bind(tokens_in)
        .bind(tokens_out)
        .execute(&state.db)
        .await?;
    Ok(Json(json!({"text": text, "usage": {"input": tokens_in, "output": tokens_out}})))
}

/// `POST /agent-runs/{id}/log` `{lines: […]}`: what the runner is doing, for the issue's page.
pub async fn write_log(State(state): State<AppState>, headers: HeaderMap, Path(id): Path<Uuid>, Json(req): Json<Value>) -> ApiResult<StatusCode> {
    running(&state, &headers, id).await?;
    for line in req["lines"].as_array().into_iter().flatten().filter_map(Value::as_str).take(200) {
        sqlx::query("INSERT INTO agent_run_logs (run_id, line) VALUES ($1, $2)")
            .bind(id)
            .bind(line.chars().take(2000).collect::<String>())
            .execute(&state.db)
            .await?;
    }
    sqlx::query("UPDATE agent_runs SET heartbeat_at = now() WHERE id = $1").bind(id).execute(&state.db).await?;
    Ok(StatusCode::NO_CONTENT)
}

fn safe_path(p: &str) -> bool {
    !p.is_empty()
        && p.len() <= 400
        && !p.starts_with('/')
        && !p.contains('\\')
        && !p.contains('\0')
        && p.split('/').all(|seg| !seg.is_empty() && seg != "." && seg != ".." && seg != ".git")
}

async fn fail(state: &AppState, id: Uuid, error: &str) -> ApiResult<()> {
    sqlx::query("UPDATE agent_runs SET state = 'failed', finished_at = now(), token_hash = NULL, error = $2 WHERE id = $1")
        .bind(id)
        .bind(error.chars().take(2000).collect::<String>())
        .execute(&state.db)
        .await?;
    sqlx::query("INSERT INTO agent_run_logs (run_id, line) VALUES ($1, $2)")
        .bind(id)
        .bind(format!("Failed: {error}").chars().take(2000).collect::<String>())
        .execute(&state.db)
        .await?;
    changed(state, id).await;
    Ok(())
}

/// `POST /agent-runs/{id}/finish` `{summary?, answer?, files?: [{path, status, content}], error?}`:
/// the result. Changed files become a branch and a draft pull request, linked to the issue and
/// announced in a comment by the agent; without changes the answer is the comment.
pub async fn finish(State(state): State<AppState>, headers: HeaderMap, Path(id): Path<Uuid>, Json(req): Json<Value>) -> ApiResult<Json<Value>> {
    let (workspace, agent, issue) = running(&state, &headers, id).await?;
    let agent_user: Uuid = sqlx::query_scalar("SELECT user_id FROM agents WHERE id = $1").bind(agent).fetch_one(&state.db).await?;
    let actor = Actor { user: Some(agent_user), workspace, depth: 1 };
    let s = crate::issues::summary(&state, crate::issues::load(&state, issue).await?);
    if let Some(e) = req["error"].as_str().filter(|e| !e.trim().is_empty()) {
        fail(&state, id, e).await?;
        return Ok(Json(json!({"state": "failed"})));
    }
    let text = |k: &str| req[k].as_str().map(str::trim).filter(|t| !t.is_empty()).map(|t| t.chars().take(20_000).collect::<String>());
    let (summary, answer) = (text("summary"), text("answer"));
    let mut files = Vec::new();
    let mut total = 0;
    for f in req["files"].as_array().into_iter().flatten() {
        let path = f["path"].as_str().unwrap_or("");
        let status = f["status"].as_str().unwrap_or("modified");
        if !safe_path(path) || !matches!(status, "added" | "modified" | "deleted") {
            fail(&state, id, &format!("the result names a file it can't change: {path}")).await?;
            return Err(ApiError::bad("a file's path or status isn't acceptable"));
        }
        let content = match (status, f["content"].as_str()) {
            ("deleted", _) => Vec::new(),
            (_, Some(c)) => base64::engine::general_purpose::STANDARD.decode(c).map_err(|_| ApiError::bad("a file's content is base64"))?,
            _ => return Err(ApiError::bad("a changed file needs its content")),
        };
        total += content.len();
        files.push(FileChange { path: path.to_string(), status: status.to_string(), content });
    }
    if files.len() > MAX_FILES || total > MAX_CHANGE_BYTES {
        fail(&state, id, "the change is too large (more than 300 files or 5 MB)").await?;
        return Err(ApiError::TooLarge);
    }
    let result = if files.is_empty() {
        // a question, or nothing to change: the answer is a comment
        let body = answer.or(summary).unwrap_or_else(|| "I looked into this and found nothing to change.".into());
        crate::routes::comments::add(&state, &actor, issue, crate::doc::from_markdown(&body)).await?;
        json!({"kind": "answer", "summary": body.chars().take(500).collect::<String>()})
    } else {
        let Some((repo, hook, base)) = repo_of(&state, agent).await? else {
            fail(&state, id, "the agent has no repository to put its changes in").await?;
            return Err(ApiError::bad("the agent has no repository"));
        };
        let summary = summary.or(answer).unwrap_or_else(|| format!("Changes for {}.", s.key));
        // a branch of its own: the suggested name, or the next free one
        let wanted = crate::git::branch_name(&s.key, &s.title);
        let mut branch = None;
        for n in 1..=6 {
            let name = if n == 1 { wanted.clone() } else { format!("{wanted}-{n}") };
            match repo.create_branch(&state, &name, &base).await {
                Ok(()) => {
                    branch = Some(name);
                    break;
                }
                Err(GitError::Exists(_)) => continue,
                Err(e) => {
                    fail(&state, id, &e.to_string()).await?;
                    return Err(ApiError::bad(e.to_string()));
                }
            }
        }
        let Some(branch) = branch else {
            fail(&state, id, "no free branch name is left for this issue").await?;
            return Err(ApiError::Conflict("no free branch name".into()));
        };
        let title = format!("{} {}", s.key, s.title);
        let pushed = async {
            repo.commit_files(&state, &branch, &format!("{}: {}\n\n{}", s.key, s.title, summary.chars().take(2000).collect::<String>()), &files).await?;
            repo.open_pull_request(&state, &branch, &base, &title, &format!("{}\n\n{}", summary, s.url)).await
        }
        .await;
        let author: String = sqlx::query_scalar("SELECT name FROM users WHERE id = $1").bind(agent_user).fetch_one(&state.db).await?;
        crate::git::link_created(&state, &hook, issue, agent_user, "branch", &branch, &repo.branch_url(&branch), &branch, None, Some(&author)).await?;
        match pushed {
            Ok((number, url)) => {
                crate::git::link_created(&state, &hook, issue, agent_user, "pull_request", &title, &url, &branch, number, Some(&author)).await?;
                crate::routes::comments::add(&state, &actor, issue, crate::doc::from_markdown(&format!("{summary}\n\nDraft pull request: {url}"))).await?;
                json!({"kind": "pull_request", "summary": summary.chars().take(500).collect::<String>(), "branch": branch, "url": url, "number": number, "files": files.len()})
            }
            Err(e) => {
                fail(&state, id, &format!("the branch {branch} was created, but: {e}")).await?;
                return Err(ApiError::bad(e.to_string()));
            }
        }
    };
    sqlx::query("UPDATE agent_runs SET state = 'succeeded', finished_at = now(), token_hash = NULL, result = $2 WHERE id = $1")
        .bind(id)
        .bind(&result)
        .execute(&state.db)
        .await?;
    sqlx::query("INSERT INTO agent_run_logs (run_id, line) VALUES ($1, 'Done.')").bind(id).execute(&state.db).await?;
    audit(&state, workspace, Some(agent_user), "agent.run.finished", Some(s.key.clone()), json!({"kind": result["kind"], "url": result["url"]})).await;
    changed(&state, id).await;
    Ok(Json(json!({"state": "succeeded", "result": result})))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_and_handles() {
        for ok in ["src/main.rs", "README.md", "a/b/c.txt", ".github/workflows/ci.yml"] {
            assert!(safe_path(ok), "{ok}");
        }
        for bad in ["", "/etc/passwd", "../x", "a/../b", ".git/config", "a//b", "a\\b", "x/.git/hooks/pre-commit"] {
            assert!(!safe_path(bad), "{bad}");
        }
        assert_eq!(clean_handle("@Coder-1").unwrap(), "coder-1");
        assert!(clean_handle("x").is_err() && clean_handle("has space").is_err());
    }
}
