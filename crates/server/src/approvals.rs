//! Workflow approvals: statuses that wait for a decision.
//!
//! A status may carry an approval (`statuses.approval`):
//! `{"steps": [{"name": "Finance", "approvers": [{"kind": "user", "id": …}, {"kind": "team", "id": …},
//!   {"kind": "role", "role": "admin" | "member"}, {"kind": "field", "id": <person field>},
//!   {"kind": "assignee"}, {"kind": "reporter"}, {"kind": "requester"}],
//!   "quorum": "any" | "all" | "count" | "percent", "value": 2, "rejection": "any" | "quorum"}],
//!   "parallel": false, "approvedStatusId": …, "rejectedStatusId": …, "selfApproval": false,
//!   "remindHours": 24, "timeout": {"hours": 72, "action": "reassign" | "skip" | "reject", "userId": …},
//!   "pauseSla": true}`.
//!
//! An issue entering the status opens a request ([`entered`]); its steps ask their approvers one
//! after another (or all at once, `parallel`). When the request is decided the issue moves to the
//! approved or the rejected status — nobody takes those two moves by hand while it's pending, and
//! leaving the status another way cancels the request ([`leaving`]). People away are joined by
//! their substitutes; an approver can hand their vote to someone else. Decisions come from the
//! app, from the link in the e-mail (a page that asks for confirmation) or, for requesters, from
//! the portal. Everything lands in the issue's history and the audit log.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use chrono::{DateTime, Utc};
use serde_json::{Map, Value, json};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::access::Role;
use crate::app::{AppState, audit, new_id};
use crate::auth::{CurrentUser, User};
use crate::error::{ApiError, ApiResult};
use crate::events::{self, Event};
use crate::i18n::tr;
use crate::issues::{Actor, Change};

const MAX_STEPS: usize = 5;
const MAX_APPROVERS: usize = 50;

// ---- configuration -------------------------------------------------------------------------------

fn id_of(v: &Value, what: &str) -> ApiResult<Uuid> {
    v.as_str().and_then(|s| s.parse().ok()).ok_or_else(|| ApiError::bad(format!("{what} must be an id")))
}

/// Checks and tidies a status's approval (null: none).
pub async fn check_config(state: &AppState, workspace: Uuid, project: Uuid, status: Uuid, v: &Value) -> ApiResult<Option<Value>> {
    if v.is_null() {
        return Ok(None);
    }
    let o = v.as_object().ok_or_else(|| ApiError::bad("approval must be an object or null"))?;
    if let Some(k) = o.keys().find(|k| {
        !matches!(k.as_str(), "steps" | "parallel" | "approvedStatusId" | "rejectedStatusId" | "selfApproval" | "remindHours" | "timeout" | "pauseSla")
    }) {
        return Err(ApiError::bad(format!("unknown approval setting {k}")));
    }
    let statuses: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM statuses WHERE project_id = $1").bind(project).fetch_all(&state.db).await?;
    let target = |k: &str, what: &str| -> ApiResult<Uuid> {
        let id = id_of(&v[k], k).map_err(|_| ApiError::bad(format!("choose the status an issue moves to when it's {what}")))?;
        if id == status || !statuses.contains(&id) {
            return Err(ApiError::bad(format!("{k} must be another status of this project")));
        }
        Ok(id)
    };
    let approved = target("approvedStatusId", "approved")?;
    let rejected = target("rejectedStatusId", "rejected")?;
    let list = v["steps"].as_array().filter(|s| !s.is_empty() && s.len() <= MAX_STEPS).ok_or_else(|| ApiError::bad("an approval has 1 to 5 steps"))?;
    let mut steps = Vec::new();
    for (n, s) in list.iter().enumerate() {
        let name: String = match s["name"].as_str().map(str::trim).filter(|x| !x.is_empty()) {
            Some(x) if x.chars().count() <= 60 => x.to_string(),
            Some(_) => return Err(ApiError::bad("step names are at most 60 characters")),
            None if list.len() == 1 => "Approval".into(),
            None => format!("Step {}", n + 1),
        };
        let asked =
            s["approvers"].as_array().filter(|a| !a.is_empty() && a.len() <= 20).ok_or_else(|| ApiError::bad(format!("{name}: choose who approves")))?;
        let mut approvers: Vec<Value> = Vec::new();
        for a in asked {
            let tidy = match a["kind"].as_str().unwrap_or("") {
                "user" => {
                    let id = id_of(&a["id"], "an approver")?;
                    let ok: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM users WHERE id = $1 AND workspace_id = $2 AND status = 'active')")
                        .bind(id)
                        .bind(workspace)
                        .fetch_one(&state.db)
                        .await?;
                    if !ok {
                        return Err(ApiError::bad(format!("{name}: an approver isn't an active person of this workspace")));
                    }
                    json!({"kind": "user", "id": id})
                }
                "team" => {
                    let id = id_of(&a["id"], "a team")?;
                    let ok: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM teams WHERE id = $1 AND workspace_id = $2)")
                        .bind(id)
                        .bind(workspace)
                        .fetch_one(&state.db)
                        .await?;
                    if !ok {
                        return Err(ApiError::bad(format!("{name}: unknown team")));
                    }
                    json!({"kind": "team", "id": id})
                }
                "role" => match a["role"].as_str() {
                    Some(r @ ("admin" | "member")) => json!({"kind": "role", "role": r}),
                    _ => return Err(ApiError::bad("a role is admin or member")),
                },
                "field" => {
                    let id = id_of(&a["id"], "a field")?;
                    let ok: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM custom_fields WHERE id = $1 AND project_id = $2 AND kind = 'user')")
                        .bind(id)
                        .bind(project)
                        .fetch_one(&state.db)
                        .await?;
                    if !ok {
                        return Err(ApiError::bad(format!("{name}: approvers come from a person field of this project")));
                    }
                    json!({"kind": "field", "id": id})
                }
                k @ ("assignee" | "reporter" | "requester") => json!({"kind": k}),
                other => return Err(ApiError::bad(format!("unknown kind of approver {other}"))),
            };
            if !approvers.contains(&tidy) {
                approvers.push(tidy);
            }
        }
        let quorum = match s["quorum"].as_str() {
            None => "any",
            Some(q @ ("any" | "all" | "count" | "percent")) => q,
            Some(_) => return Err(ApiError::bad("quorum is any, all, count or percent")),
        };
        let mut step = json!({"name": name, "approvers": approvers, "quorum": quorum});
        match quorum {
            "count" => {
                step["value"] = json!(s["value"].as_i64().filter(|n| (1..=50).contains(n)).ok_or_else(|| ApiError::bad("say how many must approve (1 to 50)"))?)
            }
            "percent" => {
                step["value"] =
                    json!(s["value"].as_i64().filter(|n| (1..=100).contains(n)).ok_or_else(|| ApiError::bad("say what share must approve (1 to 100 percent)"))?)
            }
            _ => {}
        }
        step["rejection"] = match s["rejection"].as_str() {
            None => json!("any"),
            Some(r @ ("any" | "quorum")) => json!(r),
            Some(_) => return Err(ApiError::bad("rejection is any (one rejection fails the step) or quorum (when the approvals can't be reached any more)")),
        };
        steps.push(step);
    }
    let mut out = json!({
        "steps": steps,
        "parallel": v["parallel"].as_bool().unwrap_or(false),
        "approvedStatusId": approved,
        "rejectedStatusId": rejected,
        "selfApproval": v["selfApproval"].as_bool().unwrap_or(false),
        "pauseSla": v["pauseSla"].as_bool().unwrap_or(false),
    });
    if !v["remindHours"].is_null() {
        out["remindHours"] = json!(v["remindHours"].as_i64().filter(|h| (1..=720).contains(h)).ok_or_else(|| ApiError::bad("remindHours is 1 to 720"))?);
    }
    if !v["timeout"].is_null() {
        let t = &v["timeout"];
        let hours = t["hours"].as_i64().filter(|h| (1..=8760).contains(h)).ok_or_else(|| ApiError::bad("a timeout is 1 to 8760 hours"))?;
        out["timeout"] = match t["action"].as_str() {
            Some(a @ ("skip" | "reject")) => json!({"hours": hours, "action": a}),
            Some("reassign") => {
                let id = id_of(&t["userId"], "the person a timeout hands the step to")?;
                let ok: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM users WHERE id = $1 AND workspace_id = $2 AND status = 'active')")
                    .bind(id)
                    .bind(workspace)
                    .fetch_one(&state.db)
                    .await?;
                if !ok {
                    return Err(ApiError::bad("the person a timeout hands the step to isn't active in this workspace"));
                }
                json!({"hours": hours, "action": "reassign", "userId": id})
            }
            _ => return Err(ApiError::bad("a timeout reassigns, skips or rejects")),
        };
    }
    Ok(Some(out))
}

/// How many approvals a step with `n` approvers needs.
pub fn needed(step: &Value, n: usize) -> usize {
    let value = step["value"].as_u64().unwrap_or(1) as usize;
    match step["quorum"].as_str() {
        Some("all") => n,
        Some("count") => value.min(n),
        Some("percent") => (n * value).div_ceil(100),
        _ => 1,
    }
    .max(1)
}

/// A step's outcome so far: approved (true), rejected (false) or open.
pub fn verdict(step: &Value, n: usize, approved: usize, rejected: usize) -> Option<bool> {
    if n == 0 {
        return None;
    }
    let need = needed(step, n);
    if approved >= need {
        return Some(true);
    }
    let failed = match step["rejection"].as_str() {
        Some("quorum") => n - rejected < need,
        _ => rejected > 0,
    };
    failed.then_some(false)
}

// ---- opening and cancelling (inside the change of an issue) --------------------------------------

async fn history(tx: &mut Transaction<'_, Postgres>, issue: Uuid, project: Uuid, actor: Option<Uuid>, new: Value) -> ApiResult<()> {
    let c = Change { field: "approval", old: Value::Null, new };
    crate::issues::record(tx, issue, project, actor, "approval", Some(&c)).await
}

/// Before an issue's status changes: with a request pending, the approved and rejected statuses
/// can't be taken by hand; any other move cancels the request.
pub async fn leaving(tx: &mut Transaction<'_, Postgres>, actor: &Actor, issue: Uuid, project: Uuid, to: Uuid) -> ApiResult<()> {
    let pending: Option<(Uuid, Value, String)> =
        sqlx::query_as("SELECT id, config, status_name FROM approvals WHERE issue_id = $1 AND state = 'pending' FOR UPDATE")
            .bind(issue)
            .fetch_optional(&mut **tx)
            .await?;
    let Some((id, config, name)) = pending else { return Ok(()) };
    let to = to.to_string();
    if !actor.is_quiet() && (config["approvedStatusId"].as_str() == Some(&to) || config["rejectedStatusId"].as_str() == Some(&to)) {
        return Err(ApiError::Denied(format!("this issue waits for approval in {name}: it moves on when that's decided")));
    }
    cancel(tx, id, issue, project, actor.user).await
}

async fn cancel(tx: &mut Transaction<'_, Postgres>, id: Uuid, issue: Uuid, project: Uuid, actor: Option<Uuid>) -> ApiResult<()> {
    sqlx::query("UPDATE approvals SET state = 'cancelled', decided_at = now() WHERE id = $1").bind(id).execute(&mut **tx).await?;
    sqlx::query("UPDATE approval_steps SET state = 'cancelled', decided_at = now() WHERE approval_id = $1 AND state IN ('pending', 'waiting')")
        .bind(id)
        .execute(&mut **tx)
        .await?;
    history(tx, issue, project, actor, json!({"action": "cancelled"})).await
}

/// After an issue entered a status (or was created in it): opens the status's approval request.
pub async fn entered(state: &AppState, tx: &mut Transaction<'_, Postgres>, actor: &Actor, issue: Uuid, project: Uuid, status: Uuid) -> ApiResult<()> {
    if actor.is_quiet() {
        return Ok(());
    }
    let (name, config): (String, Option<Value>) = sqlx::query_as("SELECT name, approval FROM statuses WHERE id = $1").bind(status).fetch_one(&mut **tx).await?;
    let Some(config) = config.filter(|c| c["steps"].as_array().is_some_and(|s| !s.is_empty())) else { return Ok(()) };
    if let Some(old) =
        sqlx::query_scalar::<_, Uuid>("SELECT id FROM approvals WHERE issue_id = $1 AND state = 'pending'").bind(issue).fetch_optional(&mut **tx).await?
    {
        cancel(tx, old, issue, project, actor.user).await?;
    }
    let id = new_id();
    sqlx::query(
        "INSERT INTO approvals (id, workspace_id, project_id, issue_id, status_id, status_name, config, requested_by) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(id)
    .bind(actor.workspace)
    .bind(project)
    .bind(issue)
    .bind(status)
    .bind(&name)
    .bind(&config)
    .bind(actor.user)
    .execute(&mut **tx)
    .await?;
    let parallel = config["parallel"].as_bool().unwrap_or(false);
    for (n, s) in config["steps"].as_array().into_iter().flatten().enumerate() {
        let step = new_id();
        sqlx::query("INSERT INTO approval_steps (id, approval_id, position, name) VALUES ($1, $2, $3, $4)")
            .bind(step)
            .bind(id)
            .bind(n as i32)
            .bind(s["name"].as_str().unwrap_or("Approval"))
            .execute(&mut **tx)
            .await?;
        if parallel || n == 0 {
            start_step(state, tx, id, step, s).await?;
        }
    }
    history(tx, issue, project, actor.user, json!({"action": "requested", "status": name})).await
}

struct Voter {
    user: Option<Uuid>,
    email: Option<String>,
    for_user: Option<Uuid>,
}

/// Who a step asks, as the issue is now.
async fn resolve(state: &AppState, tx: &mut Transaction<'_, Postgres>, approval: Uuid, step: &Value) -> ApiResult<Vec<Voter>> {
    let (issue, project, workspace, requested_by, self_ok): (Uuid, Uuid, Uuid, Option<Uuid>, bool) = sqlx::query_as(
        "SELECT issue_id, project_id, workspace_id, requested_by, COALESCE((config->>'selfApproval')::boolean, false) FROM approvals WHERE id = $1",
    )
    .bind(approval)
    .fetch_one(&mut **tx)
    .await?;
    let exclude = requested_by.filter(|_| !self_ok);
    let (assignee, reporter, custom): (Option<Uuid>, Option<Uuid>, Value) =
        sqlx::query_as("SELECT assignee_id, reporter_id, custom FROM issues WHERE id = $1").bind(issue).fetch_one(&mut **tx).await?;
    let mut users: Vec<Uuid> = Vec::new();
    let mut email: Option<String> = None;
    for a in step["approvers"].as_array().into_iter().flatten() {
        let id = a["id"].as_str().and_then(|s| s.parse::<Uuid>().ok());
        match a["kind"].as_str().unwrap_or("") {
            "user" => users.extend(id),
            "team" => users.extend(sqlx::query_scalar::<_, Uuid>("SELECT user_id FROM team_members WHERE team_id = $1").bind(id).fetch_all(&mut **tx).await?),
            "role" => users.extend(project_role(tx, workspace, project, if a["role"] == "admin" { 3 } else { 2 }).await?),
            "field" => users.extend(id.and_then(|f| custom[f.to_string()].as_str()?.parse::<Uuid>().ok())),
            "assignee" => users.extend(assignee),
            "reporter" => users.extend(reporter),
            "requester" => {
                email = sqlx::query_scalar("SELECT email FROM issue_requesters WHERE issue_id = $1").bind(issue).fetch_optional(&mut **tx).await?;
            }
            _ => {}
        }
    }
    let mut seen = std::collections::HashSet::new();
    users.retain(|u| Some(*u) != exclude && seen.insert(*u));
    let mut users = able(tx, issue, &users).await?;
    if users.is_empty() && email.is_none() {
        // nobody to ask: the project's lead, else its admins
        let lead: Option<Uuid> = sqlx::query_scalar("SELECT lead_id FROM projects WHERE id = $1").bind(project).fetch_one(&mut **tx).await?;
        let lead: Vec<Uuid> = lead.into_iter().filter(|u| Some(*u) != exclude).collect();
        users = able(tx, issue, &lead).await?;
        if users.is_empty() {
            let mut admins = project_role(tx, workspace, project, 3).await?;
            admins.retain(|u| Some(*u) != exclude);
            users = able(tx, issue, &admins).await?;
        }
    }
    users.truncate(MAX_APPROVERS);
    let mut out: Vec<Voter> = users.iter().map(|u| Voter { user: Some(*u), email: None, for_user: None }).collect();
    // people away are joined by their substitutes
    for u in &users {
        if let Some((sub, _)) = crate::absences::substitute(state, workspace, *u).await
            && Some(sub) != exclude
            && !out.iter().any(|v| v.user == Some(sub))
            && !able(tx, issue, &[sub]).await?.is_empty()
        {
            out.push(Voter { user: Some(sub), email: None, for_user: Some(*u) });
        }
    }
    if let Some(e) = email {
        out.push(Voter { user: None, email: Some(e), for_user: None });
    }
    Ok(out)
}

/// The active people with at least `rank` in a project.
async fn project_role(tx: &mut Transaction<'_, Postgres>, workspace: Uuid, project: Uuid, rank: i32) -> ApiResult<Vec<Uuid>> {
    Ok(sqlx::query_scalar(
        "SELECT u.id FROM users u JOIN LATERAL tc_project_roles(u.id) r ON r.project_id = $2 AND r.rank >= $3
          WHERE u.workspace_id = $1 AND u.status = 'active' ORDER BY lower(u.name) LIMIT 200",
    )
    .bind(workspace)
    .bind(project)
    .bind(rank)
    .fetch_all(&mut **tx)
    .await?)
}

/// Those of `users` who are active and see the issue (in the order given).
async fn able(tx: &mut Transaction<'_, Postgres>, issue: Uuid, users: &[Uuid]) -> ApiResult<Vec<Uuid>> {
    if users.is_empty() {
        return Ok(Vec::new());
    }
    let sql = format!(
        "SELECT u.id FROM users u JOIN issues i ON i.id = $2 JOIN LATERAL tc_project_roles(u.id) r ON r.project_id = i.project_id AND r.rank >= 1
          WHERE u.id = ANY($1) AND u.status = 'active' AND {}",
        crate::access::sees_issue("i", "u.id")
    );
    let ok: Vec<Uuid> = sqlx::query_scalar(sqlx::AssertSqlSafe(sql)).bind(users).bind(issue).fetch_all(&mut **tx).await?;
    Ok(users.iter().copied().filter(|u| ok.contains(u)).collect())
}

async fn start_step(state: &AppState, tx: &mut Transaction<'_, Postgres>, approval: Uuid, step: Uuid, config: &Value) -> ApiResult<()> {
    for v in resolve(state, tx, approval, config).await? {
        sqlx::query("INSERT INTO approval_votes (id, step_id, user_id, email, for_user_id) VALUES ($1, $2, $3, $4, $5) ON CONFLICT DO NOTHING")
            .bind(new_id())
            .bind(step)
            .bind(v.user)
            .bind(v.email)
            .bind(v.for_user)
            .execute(&mut **tx)
            .await?;
    }
    sqlx::query("UPDATE approval_steps SET state = 'pending', started_at = now() WHERE id = $1").bind(step).execute(&mut **tx).await?;
    Ok(())
}

// ---- deciding ------------------------------------------------------------------------------------

/// Who decides: a person, or a requester by their address.
pub enum Who {
    User(Uuid),
    Email(String),
}

/// Works out where a request stands after a vote or a timeout: closes steps, starts the next one,
/// decides the request.
async fn advance(state: &AppState, tx: &mut Transaction<'_, Postgres>, approval: Uuid, actor: Option<Uuid>) -> ApiResult<()> {
    let (config, issue, project): (Value, Uuid, Uuid) =
        sqlx::query_as("SELECT config, issue_id, project_id FROM approvals WHERE id = $1").bind(approval).fetch_one(&mut **tx).await?;
    let steps: Vec<(Uuid, i32, String)> =
        sqlx::query_as("SELECT id, position, state FROM approval_steps WHERE approval_id = $1 ORDER BY position").bind(approval).fetch_all(&mut **tx).await?;
    let mut states: Vec<(Uuid, i32, String)> = Vec::new();
    for (id, pos, mut st) in steps {
        if st == "pending" {
            let (n, yes, no): (i64, i64, i64) = sqlx::query_as(
                "SELECT count(*), count(*) FILTER (WHERE decision = 'approved'), count(*) FILTER (WHERE decision = 'rejected') FROM approval_votes WHERE step_id = $1",
            )
            .bind(id)
            .fetch_one(&mut **tx)
            .await?;
            if let Some(ok) = verdict(&config["steps"][pos as usize], n as usize, yes as usize, no as usize) {
                st = if ok { "approved" } else { "rejected" }.to_string();
                sqlx::query("UPDATE approval_steps SET state = $2, decided_at = now() WHERE id = $1").bind(id).bind(&st).execute(&mut **tx).await?;
            }
        }
        states.push((id, pos, st));
    }
    let result = if states.iter().any(|s| s.2 == "rejected") {
        Some("rejected")
    } else if states.iter().all(|s| matches!(s.2.as_str(), "approved" | "skipped")) {
        Some("approved")
    } else {
        None
    };
    match result {
        Some(r) => {
            sqlx::query("UPDATE approvals SET state = $2, decided_at = now() WHERE id = $1").bind(approval).bind(r).execute(&mut **tx).await?;
            sqlx::query("UPDATE approval_steps SET state = 'cancelled', decided_at = now() WHERE approval_id = $1 AND state IN ('pending', 'waiting')")
                .bind(approval)
                .execute(&mut **tx)
                .await?;
            history(tx, issue, project, actor, json!({"action": r})).await?;
        }
        None => {
            // one after another: the next step starts when none is open
            if !states.iter().any(|s| s.2 == "pending")
                && let Some((id, pos, _)) = states.iter().find(|s| s.2 == "waiting")
            {
                start_step(state, tx, approval, *id, &config["steps"][*pos as usize]).await?;
            }
        }
    }
    Ok(())
}

/// Records a decision and what follows from it.
pub async fn decide(state: &AppState, approval: Uuid, step: Option<Uuid>, who: &Who, approve: bool, comment: Option<&str>) -> ApiResult<()> {
    let comment = comment.map(str::trim).filter(|c| !c.is_empty()).map(|c| c.chars().take(2000).collect::<String>());
    let mut tx = state.db.begin().await?;
    let row: Option<(String, Uuid, Uuid, Uuid)> = sqlx::query_as("SELECT state, issue_id, project_id, workspace_id FROM approvals WHERE id = $1 FOR UPDATE")
        .bind(approval)
        .fetch_optional(&mut *tx)
        .await?;
    let (st, issue, project, workspace) = row.ok_or(ApiError::NotFound("approval"))?;
    if st != "pending" {
        return Err(ApiError::Conflict(format!("this request is already {st}")));
    }
    let (user, email) = match who {
        Who::User(u) => (Some(*u), None),
        Who::Email(e) => (None, Some(e.to_lowercase())),
    };
    let vote: Option<(Uuid, String)> = sqlx::query_as(
        "SELECT v.id, s.name FROM approval_votes v JOIN approval_steps s ON s.id = v.step_id
          WHERE s.approval_id = $1 AND s.state = 'pending' AND v.decision IS NULL AND ($2::uuid IS NULL OR s.id = $2)
            AND (v.user_id = $3 OR (v.user_id IS NULL AND lower(v.email) = $4)) ORDER BY s.position LIMIT 1",
    )
    .bind(approval)
    .bind(step)
    .bind(user)
    .bind(&email)
    .fetch_optional(&mut *tx)
    .await?;
    let (vote, step_name) = vote.ok_or_else(|| ApiError::Denied("you aren't asked to decide this (any more)".into()))?;
    let decision = if approve { "approved" } else { "rejected" };
    sqlx::query("UPDATE approval_votes SET decision = $2, comment = $3, decided_at = now() WHERE id = $1")
        .bind(vote)
        .bind(decision)
        .bind(&comment)
        .execute(&mut *tx)
        .await?;
    let mut entry = json!({"action": if approve { "voteApproved" } else { "voteRejected" }, "step": step_name});
    if let Some(c) = &comment {
        entry["comment"] = json!(c);
    }
    if let Some(e) = &email {
        entry["by"] = json!(e);
    }
    history(&mut tx, issue, project, user, entry).await?;
    advance(state, &mut tx, approval, user).await?;
    tx.commit().await?;
    let key = issue_key(state, issue).await;
    audit(state, workspace, user, &format!("approval.{decision}"), key, json!({"step": step_name, "by": email, "comment": comment})).await;
    follow_up(state, approval).await;
    Ok(())
}

async fn issue_key(state: &AppState, issue: Uuid) -> Option<String> {
    sqlx::query_scalar("SELECT p.key || '-' || i.number FROM issues i JOIN projects p ON p.id = i.project_id WHERE i.id = $1")
        .bind(issue)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten()
}

/// After a request changed: moves the issue when it's decided, tells whoever is asked now.
pub async fn follow_up(state: &AppState, approval: Uuid) {
    if let Err(e) = follow_up_inner(state, approval).await {
        tracing::warn!(error = %e, "approval follow-up");
    }
}

async fn follow_up_inner(state: &AppState, approval: Uuid) -> ApiResult<()> {
    // once, also when two replicas get here
    let decided: Option<(String, Uuid, Uuid, Uuid, Value, Option<Uuid>, String)> = sqlx::query_as(
        "UPDATE approvals SET applied_at = now() WHERE id = $1 AND state IN ('approved', 'rejected') AND applied_at IS NULL
         RETURNING state, issue_id, project_id, workspace_id, config, requested_by, status_name",
    )
    .bind(approval)
    .fetch_optional(&state.db)
    .await?;
    if let Some((st, issue, project, workspace, config, requested_by, status_name)) = decided {
        let bot = Actor { user: None, workspace, depth: 1 };
        let target = config[if st == "approved" { "approvedStatusId" } else { "rejectedStatusId" }].clone();
        let mut patch = Map::new();
        patch.insert("statusId".into(), target);
        let s = match crate::issues::update(state, &bot, issue, &patch).await {
            Ok((s, _)) => s,
            Err(e) => {
                tracing::warn!(error = %e, "moving an issue after its approval");
                crate::issues::summary(state, crate::issues::load(state, issue).await?)
            }
        };
        audit(state, workspace, None, &format!("approval.{st}.final"), Some(s.key.clone()), json!({"status": status_name})).await;
        if let Some(u) = requested_by {
            crate::notify::notify(state, workspace, u, &s, None, "approvalResult", json!({"result": st, "status": status_name})).await;
        }
        crate::automation::run_event(state, &bot, &s, if st == "approved" { "approved" } else { "rejected" }).await;
        events::publish(state, Event::new(workspace, "issue").project(project).issue(issue, &s.key)).await;
    }
    ask(state, Some(approval), false).await?;
    let row: Option<(Uuid, Uuid, Uuid)> =
        sqlx::query_as("SELECT workspace_id, project_id, issue_id FROM approvals WHERE id = $1").bind(approval).fetch_optional(&state.db).await?;
    if let Some((workspace, project, issue)) = row
        && let Some(key) = issue_key(state, issue).await
    {
        events::publish(state, Event::new(workspace, "issue").project(project).issue(issue, &key)).await;
    }
    Ok(())
}

/// After an issue was created or moved: tells the approvers of the request this opened, if any.
pub async fn asked(state: &AppState, issue: Uuid) {
    let open: Option<Uuid> =
        sqlx::query_scalar("SELECT id FROM approvals WHERE issue_id = $1 AND state = 'pending'").bind(issue).fetch_optional(&state.db).await.ok().flatten();
    if let Some(id) = open
        && let Err(e) = ask(state, Some(id), false).await
    {
        tracing::warn!(error = %e, "asking approvers");
    }
}

/// The link in an approver's e-mail: their vote's id, signed.
async fn link_token(state: &AppState, vote: Uuid) -> ApiResult<String> {
    Ok(format!("{}.{}", vote.simple(), crate::portal::sign(state, &format!("approval:{vote}")).await?))
}

async fn vote_of_token(state: &AppState, token: &str) -> ApiResult<Uuid> {
    let (id, sig) = token.split_once('.').ok_or(ApiError::NotFound("link"))?;
    let vote: Uuid = id.parse().map_err(|_| ApiError::NotFound("link"))?;
    let want = crate::portal::sign(state, &format!("approval:{vote}")).await?;
    // constant time: both are hex digests of the same length
    let same = sig.len() == want.len() && sig.bytes().zip(want.bytes()).fold(0u8, |a, (x, y)| a | (x ^ y)) == 0;
    if !same {
        return Err(ApiError::NotFound("link"));
    }
    Ok(vote)
}

/// Tells approvers who haven't been told (of one request, or of all), or reminds them.
async fn ask(state: &AppState, approval: Option<Uuid>, reminder: bool) -> ApiResult<usize> {
    let votes: Vec<(Uuid, Uuid, Uuid, Option<Uuid>, Option<String>, String, Option<Uuid>, Option<Uuid>, String)> = sqlx::query_as(
        "SELECT v.id, a.issue_id, a.workspace_id, v.user_id, v.email, s.name, v.for_user_id, a.requested_by, a.status_name
           FROM approval_votes v JOIN approval_steps s ON s.id = v.step_id JOIN approvals a ON a.id = s.approval_id
          WHERE a.state = 'pending' AND s.state = 'pending' AND v.decision IS NULL AND ($1::uuid IS NULL OR a.id = $1)
            AND (v.notified_at IS NULL OR $2) LIMIT 500",
    )
    .bind(approval)
    .bind(reminder)
    .fetch_all(&state.db)
    .await?;
    let mut told = 0;
    for (vote, issue, workspace, user, email, step, for_user, requested_by, status) in votes {
        // marked first: nobody is told twice
        let fresh = sqlx::query("UPDATE approval_votes SET notified_at = now() WHERE id = $1 AND (notified_at IS NULL OR $2)")
            .bind(vote)
            .bind(reminder)
            .execute(&state.db)
            .await?
            .rows_affected();
        if fresh == 0 {
            continue;
        }
        let Ok(row) = crate::issues::load(state, issue).await else { continue };
        let s = crate::issues::summary(state, row);
        told += 1;
        match (user, email) {
            (Some(u), _) => {
                let on_behalf: Option<String> = match for_user {
                    Some(f) => sqlx::query_scalar("SELECT name FROM users WHERE id = $1").bind(f).fetch_optional(&state.db).await?,
                    None => None,
                };
                let (muted, pref, address): (bool, Option<String>, Option<String>) =
                    sqlx::query_as("SELECT COALESCE((preferences->'muted') ? 'approval', false), preferences->>'email', email FROM users WHERE id = $1")
                        .bind(u)
                        .fetch_one(&state.db)
                        .await?;
                if muted {
                    continue;
                }
                // the e-mail is this module's own (it carries the link to decide), not the bundle's
                sqlx::query(
                    "INSERT INTO notifications (id, user_id, issue_id, actor_id, kind, data, emailed_at) VALUES ($1, $2, $3, $4, 'approval', $5, now())",
                )
                .bind(new_id())
                .bind(u)
                .bind(issue)
                .bind(requested_by.filter(|r| *r != u))
                .bind(json!({"key": s.key, "title": s.title, "step": step, "status": status, "for": on_behalf, "reminder": reminder}))
                .execute(&state.db)
                .await?;
                let mut e = Event::new(workspace, "notification");
                e.user_id = Some(u);
                events::publish(state, e).await;
                if let Some(to) = address.filter(|_| state.mailer.enabled() && pref.as_deref() != Some("off")) {
                    let l = crate::i18n::of_user(&state.db, u).await;
                    let name: String = sqlx::query_scalar("SELECT name FROM users WHERE id = $1").bind(u).fetch_one(&state.db).await?;
                    let url = format!("{}/approve/{}", state.config.base(), link_token(state, vote).await?);
                    let (subject, text, html) = request_mail(l, &name, &s.key, &s.title, &step, on_behalf.as_deref(), &url, reminder);
                    let _ = crate::mail::queue(state, &crate::mail::Mail { to, subject, text, html, purpose: "notification" }).await;
                }
            }
            (None, Some(to)) => {
                let l = crate::i18n::of_workspace(&state.db, workspace).await;
                let url = crate::portal::request_url(state, &s.key);
                let (subject, text, html) = request_mail(l, "", &s.key, &s.title, &step, None, &url, reminder);
                let _ = crate::mail::queue(state, &crate::mail::Mail { to, subject, text, html, purpose: "notification" }).await;
            }
            _ => {}
        }
    }
    Ok(told)
}

/// The e-mail asking for a decision: (subject, text, html).
fn request_mail(l: &str, name: &str, key: &str, title: &str, step: &str, on_behalf: Option<&str>, url: &str, reminder: bool) -> (String, String, String) {
    let subject =
        if reminder { tr(l, "[{key}] Reminder: your approval is needed", &[("key", key)]) } else { tr(l, "[{key}] Your approval is needed", &[("key", key)]) };
    let greeting = if name.is_empty() { tr(l, "Hi,", &[]) } else { tr(l, "Hi {name},", &[("name", name)]) };
    let mut intro = tr(l, "{key} {title} waits for your decision ({step}).", &[("key", key), ("title", title), ("step", step)]);
    if let Some(n) = on_behalf {
        intro.push(' ');
        intro.push_str(&tr(l, "You're asked in place of {name}.", &[("name", n)]));
    }
    let (text, html) = crate::mail::action_mail(
        l,
        &greeting,
        &intro,
        &tr(l, "Approve or reject", &[]),
        url,
        &tr(l, "Nothing is decided until you confirm on that page.", &[]),
    );
    (subject, text, html)
}

/// Once a minute: approvers not told yet, requests whose issue left the status some other way,
/// reminders and timeouts.
pub async fn tick(state: &AppState) -> anyhow::Result<()> {
    // an issue that left the status without passing `leaving` (its status was deleted, it moved
    // to another project)
    let strays: Vec<(Uuid, Uuid, Uuid)> = sqlx::query_as(
        "SELECT a.id, a.issue_id, a.project_id FROM approvals a JOIN issues i ON i.id = a.issue_id
          WHERE a.state = 'pending' AND (a.status_id IS NULL OR i.status_id <> a.status_id OR i.project_id <> a.project_id) LIMIT 200",
    )
    .fetch_all(&state.db)
    .await?;
    for (id, issue, _) in strays {
        let mut tx = state.db.begin().await?;
        let project: Uuid = sqlx::query_scalar("SELECT project_id FROM issues WHERE id = $1").bind(issue).fetch_one(&mut *tx).await?;
        cancel(&mut tx, id, issue, project, None).await.map_err(|e| anyhow::anyhow!("{e}"))?;
        tx.commit().await?;
    }
    ask(state, None, false).await.map_err(|e| anyhow::anyhow!("{e}"))?;
    // reminders
    let due: Vec<(Uuid, Uuid)> = sqlx::query_as(
        "SELECT s.id, a.id FROM approval_steps s JOIN approvals a ON a.id = s.approval_id
          WHERE s.state = 'pending' AND a.state = 'pending' AND a.config ? 'remindHours'
            AND COALESCE(s.reminded_at, s.started_at) < now() - make_interval(hours => (a.config->>'remindHours')::int) LIMIT 200",
    )
    .fetch_all(&state.db)
    .await?;
    for (step, approval) in due {
        let fresh = sqlx::query("UPDATE approval_steps SET reminded_at = now() WHERE id = $1 AND state = 'pending'").bind(step).execute(&state.db).await?;
        if fresh.rows_affected() > 0 {
            ask(state, Some(approval), true).await.map_err(|e| anyhow::anyhow!("{e}"))?;
        }
    }
    // timeouts
    let late: Vec<(Uuid, Uuid, Value)> = sqlx::query_as(
        "SELECT s.id, a.id, a.config->'timeout' FROM approval_steps s JOIN approvals a ON a.id = s.approval_id
          WHERE s.state = 'pending' AND a.state = 'pending' AND a.config ? 'timeout' AND s.escalated_at IS NULL
            AND s.started_at < now() - make_interval(hours => (a.config->'timeout'->>'hours')::int) LIMIT 200",
    )
    .fetch_all(&state.db)
    .await?;
    for (step, approval, timeout) in late {
        if let Err(e) = time_out(state, approval, step, &timeout).await {
            tracing::warn!(error = %e, "approval timeout");
        }
    }
    Ok(())
}

async fn time_out(state: &AppState, approval: Uuid, step: Uuid, timeout: &Value) -> ApiResult<()> {
    let mut tx = state.db.begin().await?;
    let row: Option<(Uuid, Uuid, Uuid)> =
        sqlx::query_as("SELECT issue_id, project_id, workspace_id FROM approvals WHERE id = $1 AND state = 'pending' FOR UPDATE")
            .bind(approval)
            .fetch_optional(&mut *tx)
            .await?;
    let Some((issue, project, workspace)) = row else { return Ok(()) };
    let name: Option<String> =
        sqlx::query_scalar("UPDATE approval_steps SET escalated_at = now() WHERE id = $1 AND state = 'pending' AND escalated_at IS NULL RETURNING name")
            .bind(step)
            .fetch_optional(&mut *tx)
            .await?;
    let Some(name) = name else { return Ok(()) };
    let action = timeout["action"].as_str().unwrap_or("");
    match action {
        "skip" | "reject" => {
            let st = if action == "skip" { "skipped" } else { "rejected" };
            sqlx::query("UPDATE approval_steps SET state = $2, decided_at = now() WHERE id = $1").bind(step).bind(st).execute(&mut *tx).await?;
        }
        _ => {
            let Some(to) = timeout["userId"].as_str().and_then(|u| u.parse::<Uuid>().ok()) else { return Ok(()) };
            if able(&mut tx, issue, &[to]).await?.is_empty() {
                return Ok(());
            }
            sqlx::query("INSERT INTO approval_votes (id, step_id, user_id) VALUES ($1, $2, $3) ON CONFLICT DO NOTHING")
                .bind(new_id())
                .bind(step)
                .bind(to)
                .execute(&mut *tx)
                .await?;
        }
    }
    history(&mut tx, issue, project, None, json!({"action": "timeout", "step": name, "outcome": action})).await?;
    advance(state, &mut tx, approval, None).await?;
    tx.commit().await?;
    audit(state, workspace, None, "approval.timeout", issue_key(state, issue).await, json!({"step": name, "action": action})).await;
    follow_up(state, approval).await;
    Ok(())
}

// ---- reading -------------------------------------------------------------------------------------

#[derive(sqlx::FromRow)]
struct ApprovalRow {
    id: Uuid,
    issue_id: Uuid,
    status_id: Option<Uuid>,
    status_name: String,
    state: String,
    config: Value,
    requested_by: Option<Uuid>,
    requester_name: Option<String>,
    created_at: DateTime<Utc>,
    decided_at: Option<DateTime<Utc>>,
}

const APPROVAL_SQL: &str =
    "SELECT a.id, a.issue_id, a.status_id, a.status_name, a.state, a.config, a.requested_by, u.name AS requester_name, a.created_at, a.decided_at
                              FROM approvals a LEFT JOIN users u ON u.id = a.requested_by";

/// Requests as the API shows them; `me`: whose open votes `canDecide` names; `admin`: may hand
/// steps to others.
async fn render(state: &AppState, rows: Vec<ApprovalRow>, me: Option<&Who>, admin: bool) -> ApiResult<Vec<Value>> {
    let ids: Vec<Uuid> = rows.iter().map(|r| r.id).collect();
    let steps: Vec<(Uuid, Uuid, i32, String, String, Option<DateTime<Utc>>, Option<DateTime<Utc>>)> = sqlx::query_as(
        "SELECT id, approval_id, position, name, state, started_at, decided_at FROM approval_steps WHERE approval_id = ANY($1) ORDER BY position",
    )
    .bind(&ids)
    .fetch_all(&state.db)
    .await?;
    let step_ids: Vec<Uuid> = steps.iter().map(|s| s.0).collect();
    type Vote = (Uuid, Uuid, Option<Uuid>, Option<String>, Option<String>, Option<String>, Option<String>, Option<DateTime<Utc>>, Option<Uuid>, Option<String>);
    let votes: Vec<Vote> = sqlx::query_as(
        "SELECT v.id, v.step_id, v.user_id, u.name, v.email, v.decision, v.comment, v.decided_at, v.for_user_id, f.name
           FROM approval_votes v LEFT JOIN users u ON u.id = v.user_id LEFT JOIN users f ON f.id = v.for_user_id
          WHERE v.step_id = ANY($1) ORDER BY v.created_at, v.id",
    )
    .bind(&step_ids)
    .fetch_all(&state.db)
    .await?;
    let mine = |v: &Vote| match me {
        Some(Who::User(u)) => v.2 == Some(*u),
        Some(Who::Email(e)) => v.2.is_none() && v.4.as_deref().is_some_and(|x| x.eq_ignore_ascii_case(e)),
        None => false,
    };
    Ok(rows
        .into_iter()
        .map(|a| {
            let pending = a.state == "pending";
            let mut can = Vec::new();
            let steps: Vec<Value> = steps
                .iter()
                .filter(|s| s.1 == a.id)
                .map(|s| {
                    let cfg = &a.config["steps"][s.2 as usize];
                    let of_step: Vec<&Vote> = votes.iter().filter(|v| v.1 == s.0).collect();
                    if pending && s.4 == "pending" && of_step.iter().any(|v| v.5.is_none() && mine(v)) {
                        can.push(s.0);
                    }
                    let due = a.config["timeout"]["hours"].as_i64().and_then(|h| s.5.map(|t| t + chrono::Duration::hours(h))).filter(|_| s.4 == "pending");
                    json!({
                        "id": s.0, "name": s.3, "state": s.4, "quorum": cfg["quorum"], "value": cfg["value"], "needed": needed(cfg, of_step.len()),
                        "startedAt": s.5, "decidedAt": s.6, "dueAt": due,
                        "approvers": of_step.iter().map(|v| json!({
                            "id": v.0,
                            "user": v.2.map(|u| json!({"id": u, "name": v.3})),
                            "email": if v.2.is_none() { json!(v.4) } else { Value::Null },
                            "decision": v.5, "comment": v.6, "decidedAt": v.7,
                            "for": v.8.map(|f| json!({"id": f, "name": v.9})),
                        })).collect::<Vec<_>>(),
                    })
                })
                .collect();
            json!({
                "id": a.id, "issueId": a.issue_id, "state": a.state, "statusId": a.status_id, "statusName": a.status_name,
                "requestedBy": a.requested_by.map(|u| json!({"id": u, "name": a.requester_name})),
                "createdAt": a.created_at, "decidedAt": a.decided_at, "parallel": a.config["parallel"],
                "approvedStatusId": a.config["approvedStatusId"], "rejectedStatusId": a.config["rejectedStatusId"],
                "steps": steps, "canDecide": can, "canManage": admin && pending,
            })
        })
        .collect())
}

/// `GET /issues/{key}/approvals`: the issue's requests, latest first.
pub async fn for_issue(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(key): Path<String>) -> ApiResult<Json<Value>> {
    let (id, _, role) = crate::routes::issues::issue_for(&state, &user, &key, Role::Viewer).await?;
    let rows: Vec<ApprovalRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!("{APPROVAL_SQL} WHERE a.issue_id = $1 ORDER BY a.created_at DESC LIMIT 20")))
        .bind(id)
        .fetch_all(&state.db)
        .await?;
    Ok(Json(json!(render(&state, rows, Some(&Who::User(user.id)), role >= Role::Admin).await?)))
}

#[derive(serde::Deserialize)]
pub struct ListQuery {
    /// mine (waiting for me, the default) | requested (asked by me, still open)
    scope: Option<String>,
}

/// `GET /approvals`: the requests waiting for the caller's decision (or the ones they asked for),
/// each with its issue.
pub async fn list(State(state): State<AppState>, CurrentUser(user): CurrentUser, Query(q): Query<ListQuery>) -> ApiResult<Json<Value>> {
    let filter = match q.scope.as_deref() {
        None | Some("mine") => {
            "a.id IN (SELECT s.approval_id FROM approval_votes v JOIN approval_steps s ON s.id = v.step_id WHERE v.user_id = $1 AND v.decision IS NULL AND s.state = 'pending')"
        }
        Some("requested") => "a.requested_by = $1",
        Some(_) => return Err(ApiError::bad("scope is mine or requested")),
    };
    let rows: Vec<ApprovalRow> =
        sqlx::query_as(sqlx::AssertSqlSafe(format!("{APPROVAL_SQL} WHERE a.state = 'pending' AND {filter} ORDER BY a.created_at LIMIT 200")))
            .bind(user.id)
            .fetch_all(&state.db)
            .await?;
    let issue_ids: Vec<Uuid> = rows.iter().map(|r| r.issue_id).collect();
    let issues = crate::issues::load_many_for(&state, user.id, &issue_ids).await?;
    let list = render(&state, rows, Some(&Who::User(user.id)), false).await?;
    Ok(Json(json!(
        list.into_iter()
            .filter_map(|mut a| {
                let issue = issues.iter().find(|i| a["issueId"].as_str() == Some(&i.id.to_string()))?;
                a["issue"] = json!(issue);
                Some(a)
            })
            .collect::<Vec<_>>()
    )))
}

/// The request, when the caller sees its issue: (issue, project, their role).
async fn visible(state: &AppState, user: &User, approval: Uuid) -> ApiResult<(Uuid, Uuid, Role)> {
    let issue: Uuid = sqlx::query_scalar("SELECT issue_id FROM approvals WHERE id = $1 AND workspace_id = $2")
        .bind(approval)
        .bind(user.workspace_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(ApiError::NotFound("approval"))?;
    crate::issues::resolve(state, user, &issue.to_string(), false).await
}

fn step_of(req: &Value) -> ApiResult<Option<Uuid>> {
    match &req["stepId"] {
        Value::Null => Ok(None),
        v => id_of(v, "stepId").map(Some),
    }
}

/// `POST /approvals/{id}/decide` `{decision: "approve" | "reject", comment?, stepId?}`.
pub async fn decide_route(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<Uuid>,
    Json(req): Json<Value>,
) -> ApiResult<StatusCode> {
    visible(&state, &user, id).await?;
    let approve = match req["decision"].as_str() {
        Some("approve") => true,
        Some("reject") => false,
        _ => return Err(ApiError::bad("decision is approve or reject")),
    };
    decide(&state, id, step_of(&req)?, &Who::User(user.id), approve, req["comment"].as_str()).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /approvals/{id}/delegate` `{userId, stepId?, comment?}`: an approver hands their open
/// vote to someone else; a project admin (`from`: whose vote, or none to add an approver) hands a
/// step to someone.
pub async fn delegate(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(id): Path<Uuid>, Json(req): Json<Value>) -> ApiResult<StatusCode> {
    let (issue, project, role) = visible(&state, &user, id).await?;
    let to = id_of(&req["userId"], "userId")?;
    let step = step_of(&req)?;
    let mut tx = state.db.begin().await?;
    let (st, requested_by, self_ok): (String, Option<Uuid>, bool) =
        sqlx::query_as("SELECT state, requested_by, COALESCE((config->>'selfApproval')::boolean, false) FROM approvals WHERE id = $1 FOR UPDATE")
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
    if st != "pending" {
        return Err(ApiError::Conflict(format!("this request is already {st}")));
    }
    if to == user.id {
        return Err(ApiError::bad("choose someone else"));
    }
    if !self_ok && Some(to) == requested_by {
        return Err(ApiError::bad("whoever asked for the approval can't decide it"));
    }
    if able(&mut tx, issue, &[to]).await?.is_empty() {
        return Err(ApiError::bad("that person can't see this issue"));
    }
    // the caller's own open vote, else (project admins) the step itself
    let own: Option<(Uuid, Uuid, String)> = sqlx::query_as(
        "SELECT v.id, s.id, s.name FROM approval_votes v JOIN approval_steps s ON s.id = v.step_id
          WHERE s.approval_id = $1 AND s.state = 'pending' AND v.decision IS NULL AND v.user_id = $2 AND ($3::uuid IS NULL OR s.id = $3) ORDER BY s.position LIMIT 1",
    )
    .bind(id)
    .bind(user.id)
    .bind(step)
    .fetch_optional(&mut *tx)
    .await?;
    let step_name = match own {
        Some((vote, step_id, name)) => {
            let there: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM approval_votes WHERE step_id = $1 AND user_id = $2)")
                .bind(step_id)
                .bind(to)
                .fetch_one(&mut *tx)
                .await?;
            if there {
                return Err(ApiError::Conflict("that person is asked already".into()));
            }
            sqlx::query("UPDATE approval_votes SET user_id = $2, for_user_id = $3, notified_at = NULL WHERE id = $1")
                .bind(vote)
                .bind(to)
                .bind(user.id)
                .execute(&mut *tx)
                .await?;
            name
        }
        None if role >= Role::Admin => {
            let s: Option<(Uuid, String)> = sqlx::query_as(
                "SELECT id, name FROM approval_steps WHERE approval_id = $1 AND state = 'pending' AND ($2::uuid IS NULL OR id = $2) ORDER BY position LIMIT 1",
            )
            .bind(id)
            .bind(step)
            .fetch_optional(&mut *tx)
            .await?;
            let (step_id, name) = s.ok_or_else(|| ApiError::bad("no step is open"))?;
            let added = sqlx::query("INSERT INTO approval_votes (id, step_id, user_id) VALUES ($1, $2, $3) ON CONFLICT DO NOTHING")
                .bind(new_id())
                .bind(step_id)
                .bind(to)
                .execute(&mut *tx)
                .await?
                .rows_affected();
            if added == 0 {
                return Err(ApiError::Conflict("that person is asked already".into()));
            }
            name
        }
        None => return Err(ApiError::Denied("you aren't asked to decide this (any more)".into())),
    };
    let to_name: String = sqlx::query_scalar("SELECT name FROM users WHERE id = $1").bind(to).fetch_one(&mut *tx).await?;
    let mut entry = json!({"action": "delegated", "step": step_name, "to": to_name});
    if let Some(c) = req["comment"].as_str().map(str::trim).filter(|c| !c.is_empty()) {
        entry["comment"] = json!(c.chars().take(2000).collect::<String>());
    }
    history(&mut tx, issue, project, Some(user.id), entry).await?;
    tx.commit().await?;
    audit(&state, user.workspace_id, Some(user.id), "approval.delegated", issue_key(&state, issue).await, json!({"step": step_name, "to": to_name})).await;
    follow_up(&state, id).await;
    Ok(StatusCode::NO_CONTENT)
}

// ---- the link in the e-mail ----------------------------------------------------------------------

async fn link_vote(state: &AppState, token: &str) -> ApiResult<(Uuid, Uuid, Uuid, Value)> {
    let vote = vote_of_token(state, token).await?;
    let row: Option<(Uuid, Uuid, Option<Uuid>, String, String, Option<String>, String, String, String, Option<String>)> = sqlx::query_as(
        "SELECT a.id, s.id, v.user_id, a.state, s.state, v.decision, s.name, p.key || '-' || i.number, i.title, u.name
           FROM approval_votes v JOIN approval_steps s ON s.id = v.step_id JOIN approvals a ON a.id = s.approval_id
           JOIN issues i ON i.id = a.issue_id JOIN projects p ON p.id = i.project_id LEFT JOIN users u ON u.id = a.requested_by
          WHERE v.id = $1 AND i.deleted_at IS NULL",
    )
    .bind(vote)
    .fetch_optional(&state.db)
    .await?;
    let (approval, step, user, a_state, s_state, decision, step_name, key, title, by) = row.ok_or(ApiError::NotFound("link"))?;
    let user = user.ok_or(ApiError::NotFound("link"))?;
    let open = a_state == "pending" && s_state == "pending" && decision.is_none();
    let info = json!({
        "key": key, "title": title, "step": step_name, "requestedBy": by, "state": a_state, "decision": decision, "open": open,
        "url": state.config.issue_url(&key),
    });
    Ok((approval, step, user, info))
}

/// `GET /approval-links/{token}` (public: the link is the key): what the e-mail asks to decide.
pub async fn link_info(State(state): State<AppState>, Path(token): Path<String>) -> ApiResult<Json<Value>> {
    Ok(Json(link_vote(&state, &token).await?.3))
}

/// `POST /approval-links/{token}` `{decision, comment?}`: the confirmed decision.
pub async fn link_decide(State(state): State<AppState>, Path(token): Path<String>, Json(req): Json<Value>) -> ApiResult<Json<Value>> {
    let (approval, step, user, _) = link_vote(&state, &token).await?;
    let approve = match req["decision"].as_str() {
        Some("approve") => true,
        Some("reject") => false,
        _ => return Err(ApiError::bad("decision is approve or reject")),
    };
    let active: bool = sqlx::query_scalar("SELECT status = 'active' FROM users WHERE id = $1").bind(user).fetch_one(&state.db).await?;
    if !active {
        return Err(ApiError::NotFound("link"));
    }
    decide(&state, approval, Some(step), &Who::User(user), approve, req["comment"].as_str()).await?;
    Ok(Json(link_vote(&state, &token).await?.3))
}

// ---- the portal ----------------------------------------------------------------------------------

/// What a requester sees of their request's open approval (null: none, or they aren't asked).
pub async fn for_requester(state: &AppState, issue: Uuid, email: &str) -> ApiResult<Value> {
    let row: Option<(Uuid, String)> = sqlx::query_as(
        "SELECT a.id, s.name FROM approval_votes v JOIN approval_steps s ON s.id = v.step_id JOIN approvals a ON a.id = s.approval_id
          WHERE a.issue_id = $1 AND a.state = 'pending' AND s.state = 'pending' AND v.decision IS NULL AND v.user_id IS NULL AND lower(v.email) = lower($2) LIMIT 1",
    )
    .bind(issue)
    .bind(email)
    .fetch_optional(&state.db)
    .await?;
    Ok(row.map(|(id, step)| json!({"id": id, "step": step})).unwrap_or(Value::Null))
}

/// `POST /portal/requests/{key}/approval` `{decision, comment?}`: the requester's decision.
pub async fn portal_decide(
    State(state): State<AppState>,
    crate::portal::Requester(email): crate::portal::Requester,
    Path(key): Path<String>,
    Json(req): Json<Value>,
) -> ApiResult<StatusCode> {
    let (k, n) = crate::query::split_key(&key).ok_or(ApiError::NotFound("request"))?;
    let approval: Option<Uuid> = sqlx::query_scalar(
        "SELECT a.id FROM approvals a JOIN issues i ON i.id = a.issue_id JOIN projects p ON p.id = i.project_id
           JOIN approval_steps s ON s.approval_id = a.id JOIN approval_votes v ON v.step_id = s.id
          WHERE upper(p.key) = $1 AND i.number = $2 AND i.deleted_at IS NULL AND a.state = 'pending' AND s.state = 'pending'
            AND v.decision IS NULL AND v.user_id IS NULL AND lower(v.email) = lower($3) LIMIT 1",
    )
    .bind(&k)
    .bind(n)
    .bind(&email)
    .fetch_optional(&state.db)
    .await?;
    let approval = approval.ok_or(ApiError::NotFound("request"))?;
    let approve = match req["decision"].as_str() {
        Some("approve") => true,
        Some("reject") => false,
        _ => return Err(ApiError::bad("decision is approve or reject")),
    };
    decide(&state, approval, None, &Who::Email(email), approve, req["comment"].as_str()).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// The open approval of an issue, for its page: `{id, statusName}` or null.
pub async fn pending_of(state: &AppState, issue: Uuid) -> ApiResult<Value> {
    let row: Option<(Uuid, String)> =
        sqlx::query_as("SELECT id, status_name FROM approvals WHERE issue_id = $1 AND state = 'pending'").bind(issue).fetch_optional(&state.db).await?;
    Ok(row.map(|(id, name)| json!({"id": id, "statusName": name})).unwrap_or(Value::Null))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quorums() {
        let any = json!({"quorum": "any"});
        assert_eq!(verdict(&any, 3, 0, 0), None);
        assert_eq!(verdict(&any, 3, 1, 0), Some(true));
        assert_eq!(verdict(&any, 3, 0, 1), Some(false), "one rejection fails the step");
        let all = json!({"quorum": "all", "rejection": "quorum"});
        assert_eq!(verdict(&all, 3, 2, 0), None);
        assert_eq!(verdict(&all, 3, 3, 0), Some(true));
        assert_eq!(verdict(&all, 3, 2, 1), Some(false), "all can't be reached any more");
        let two = json!({"quorum": "count", "value": 2, "rejection": "quorum"});
        assert_eq!(verdict(&two, 3, 1, 1), None, "two of three is still possible");
        assert_eq!(verdict(&two, 3, 1, 2), Some(false));
        assert_eq!(verdict(&two, 3, 2, 1), Some(true));
        assert_eq!(needed(&two, 1), 1, "never more than there are approvers");
        let most = json!({"quorum": "percent", "value": 60});
        assert_eq!(needed(&most, 5), 3);
        assert_eq!(needed(&most, 1), 1);
        assert_eq!(verdict(&any, 0, 0, 0), None, "nobody to ask: it waits");
    }
}
