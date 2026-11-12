//! Comments (rich text with mentions), reactions, and time tracking (work logs, timesheets).

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use chrono::NaiveDate;
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use super::issues::issue_for;
use crate::access::Role;
use crate::app::{AppState, new_id};
use crate::auth::CurrentUser;
use crate::error::{ApiError, ApiResult};
use crate::events::{self, Event};
use crate::issues::{self, Actor, Change};

pub async fn list(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(key): Path<String>) -> ApiResult<Json<Value>> {
    let (id, _, _) = issue_for(&state, &user, &key, Role::Viewer).await?;
    #[derive(sqlx::FromRow)]
    struct Row {
        id: Uuid,
        author_id: Option<Uuid>,
        name: Option<String>,
        body: Value,
        created_at: chrono::DateTime<chrono::Utc>,
        updated_at: chrono::DateTime<chrono::Utc>,
        edited: bool,
        author_email: Option<String>,
        author_name: Option<String>,
        via_email: bool,
        to_requester_at: Option<chrono::DateTime<chrono::Utc>>,
        for_requester: bool,
        via_portal: bool,
    }
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT c.id, c.author_id, u.name, c.body, c.created_at, c.updated_at, c.edited, c.author_email, c.author_name, c.via_email, c.to_requester_at,
                c.for_requester, c.via_portal
           FROM comments c LEFT JOIN users u ON u.id = c.author_id
          WHERE c.issue_id = $1 ORDER BY c.created_at",
    )
    .bind(id)
    .fetch_all(&state.db)
    .await?;
    let reactions: Vec<(Uuid, String, Uuid, String)> = sqlx::query_as(
        "SELECT r.comment_id, r.reaction, r.user_id, u.name FROM comment_reactions r JOIN users u ON u.id = r.user_id JOIN comments c ON c.id = r.comment_id WHERE c.issue_id = $1",
    )
    .bind(id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(json!(
        rows.into_iter()
            .map(|c| {
                let mut by_kind: std::collections::BTreeMap<String, Vec<Value>> = Default::default();
                for (rc, kind, uid, un) in &reactions {
                    if *rc == c.id {
                        by_kind.entry(kind.clone()).or_default().push(json!({"id": uid, "name": un}));
                    }
                }
                // someone without an account (the issue's requester: by e-mail or in the portal)
                let external = c.author_email.as_ref().filter(|_| c.author_id.is_none()).map(|e| json!({"email": e, "name": c.author_name}));
                json!({
                    "id": c.id, "author": c.author_id.map(|a| json!({"id": a, "name": c.name})), "external": external, "body": c.body,
                    "createdAt": c.created_at, "updatedAt": c.updated_at, "edited": c.edited, "viaEmail": c.via_email, "viaPortal": c.via_portal,
                    "forRequester": c.for_requester, "emailedAt": c.to_requester_at,
                    "reactions": by_kind.into_iter().map(|(k, people)| json!({"reaction": k, "people": people})).collect::<Vec<_>>()
                })
            })
            .collect::<Vec<_>>()
    )))
}

/// How a comment came about, beyond its author.
#[derive(Debug, Clone, Default)]
pub struct CommentOpts {
    /// it came in by e-mail
    pub via_email: bool,
    /// written in the service desk portal
    pub via_portal: bool,
    /// the issue's requester sees it (in the portal; marked for them, or written by them)
    pub for_requester: bool,
    /// written by someone without an account (an issue's requester): e-mail address and name
    pub external: Option<(String, Option<String>)>,
}

/// Adds a comment as `actor` (people and automation rules).
pub async fn add(state: &AppState, actor: &Actor, issue: Uuid, body: Value) -> ApiResult<Uuid> {
    add_with(state, actor, issue, body, &CommentOpts::default()).await
}

/// Adds a comment as `actor`, by e-mail or by a requester.
pub async fn add_with(state: &AppState, actor: &Actor, issue: Uuid, body: Value, opts: &CommentOpts) -> ApiResult<Uuid> {
    crate::doc::validate(&body).map_err(ApiError::BadRequest)?;
    let text = crate::doc::text(&body);
    if text.trim().is_empty() {
        return Err(ApiError::bad("the comment is empty"));
    }
    let project: Uuid = sqlx::query_scalar("SELECT project_id FROM issues WHERE id = $1").bind(issue).fetch_one(&state.db).await?;
    let id = new_id();
    let (author_email, author_name) = opts.external.clone().unzip();
    // a requester is named where there's no person to point to (history, notifications)
    let by = opts.external.as_ref().map(|(e, n)| n.clone().unwrap_or_else(|| e.clone()));
    let mut tx = state.db.begin().await?;
    sqlx::query(
        "INSERT INTO comments (id, issue_id, author_id, body, body_text, author_email, author_name, via_email, via_portal, for_requester)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
    )
    .bind(id)
    .bind(issue)
    .bind(actor.user)
    .bind(&body)
    .bind(&text)
    .bind(author_email)
    .bind(author_name.flatten())
    .bind(opts.via_email)
    .bind(opts.via_portal)
    .bind(opts.for_requester)
    .execute(&mut *tx)
    .await?;
    let excerpt: String = text.chars().take(200).collect();
    let mut new = json!({"id": id, "excerpt": excerpt});
    if let Some(by) = &by {
        new["by"] = json!(by);
    }
    issues::record(&mut tx, issue, project, actor.user, "comment", Some(&Change { field: "comment", old: Value::Null, new })).await?;
    if let Some(u) = actor.user {
        issues::watch(&mut tx, issue, u).await?;
    }
    sqlx::query("UPDATE issues SET updated_at = now() WHERE id = $1").bind(issue).execute(&mut *tx).await?;
    tx.commit().await?;
    let s = issues::summary(state, issues::load(state, issue).await?);
    crate::notify::comment(state, actor, &s, id, &body, by.as_deref()).await;
    let mut e = Event::new(actor.workspace, "comment").project(project).issue(issue, &s.key);
    e.actor_id = actor.user;
    events::publish(state, e).await;
    crate::webhooks::enqueue(
        state,
        actor.workspace,
        Some(project),
        "comment.created",
        json!({"issue": {"id": s.id, "key": s.key, "title": s.title}, "comment": {"id": id, "text": text}}),
    )
    .await;
    crate::automation::run_commented(state, actor, &s).await;
    Ok(id)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommentReq {
    body: Option<Value>,
    markdown: Option<String>,
    /// visible to the issue's requester: in the portal, and e-mailed to them when mail is set up
    /// (people who can edit the issue)
    #[serde(default)]
    for_requester: bool,
    /// the earlier name of `forRequester`
    #[serde(default)]
    email_requester: bool,
}

impl CommentReq {
    fn doc(self) -> ApiResult<Value> {
        match (self.body, self.markdown) {
            (Some(Value::String(s)), _) => Ok(crate::doc::from_plain(&s)),
            (Some(b), _) if !b.is_null() => Ok(b),
            (_, Some(md)) => Ok(crate::doc::from_markdown(&md)),
            _ => Err(ApiError::bad("body (rich text) or markdown is required")),
        }
    }
}

pub async fn create(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Path(key): Path<String>,
    Json(req): Json<CommentReq>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let (issue, _, role) = issue_for(&state, &user, &key, Role::Viewer).await?;
    let to_requester = req.for_requester || req.email_requester;
    if to_requester {
        if role < Role::Member {
            return Err(ApiError::Denied("only people who can edit the issue may write to its requester".into()));
        }
        if crate::mail_in::requester(&state, issue).await?.is_none() {
            return Err(ApiError::bad("this issue has no requester"));
        }
    }
    let body = req.doc()?;
    let opts = CommentOpts { for_requester: to_requester, ..Default::default() };
    let id = add_with(&state, &Actor::of(&user), issue, body.clone(), &opts).await?;
    let emailed = to_requester
        && crate::mail_in::mail_requester(&state, issue, id, &body, &user.name).await.unwrap_or_else(|e| {
            tracing::warn!("e-mailing the requester failed: {e:#}");
            false
        });
    Ok((StatusCode::CREATED, Json(json!({"id": id, "forRequester": to_requester, "emailed": emailed}))))
}

async fn comment_issue(state: &AppState, id: Uuid) -> ApiResult<(Uuid, Option<Uuid>)> {
    sqlx::query_as("SELECT issue_id, author_id FROM comments WHERE id = $1").bind(id).fetch_optional(&state.db).await?.ok_or(ApiError::NotFound("comment"))
}

pub async fn update(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(id): Path<Uuid>, Json(req): Json<CommentReq>) -> ApiResult<StatusCode> {
    let (issue, author) = comment_issue(&state, id).await?;
    let (_, project, _) = issue_for(&state, &user, &issue.to_string(), Role::Viewer).await?;
    if author != Some(user.id) {
        return Err(ApiError::Denied("only the author can edit a comment".into()));
    }
    let body = req.doc()?;
    crate::doc::validate(&body).map_err(ApiError::BadRequest)?;
    sqlx::query("UPDATE comments SET body = $2, body_text = $3, updated_at = now(), edited = true WHERE id = $1")
        .bind(id)
        .bind(&body)
        .bind(crate::doc::text(&body))
        .execute(&state.db)
        .await?;
    events::publish(&state, Event::new(user.workspace_id, "comment").project(project).actor(user.id)).await;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn delete(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    let (issue, author) = comment_issue(&state, id).await?;
    let (_, project, role) = issue_for(&state, &user, &issue.to_string(), Role::Viewer).await?;
    if author != Some(user.id) && role < Role::Admin {
        return Err(ApiError::Denied("only the author or a project admin can delete a comment".into()));
    }
    sqlx::query("DELETE FROM comments WHERE id = $1").bind(id).execute(&state.db).await?;
    events::publish(&state, Event::new(user.workspace_id, "comment").project(project).actor(user.id)).await;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn react(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(id): Path<Uuid>, Json(req): Json<Value>) -> ApiResult<StatusCode> {
    let (issue, _) = comment_issue(&state, id).await?;
    let (_, project, _) = issue_for(&state, &user, &issue.to_string(), Role::Viewer).await?;
    let reaction = req["reaction"]
        .as_str()
        .filter(|r| matches!(*r, "thumbs-up" | "heart" | "check" | "eyes" | "party"))
        .ok_or_else(|| ApiError::bad("unknown reaction"))?;
    let exists: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM comment_reactions WHERE comment_id = $1 AND user_id = $2 AND reaction = $3)")
        .bind(id)
        .bind(user.id)
        .bind(reaction)
        .fetch_one(&state.db)
        .await?;
    if exists {
        sqlx::query("DELETE FROM comment_reactions WHERE comment_id = $1 AND user_id = $2 AND reaction = $3")
            .bind(id)
            .bind(user.id)
            .bind(reaction)
            .execute(&state.db)
            .await?;
    } else {
        sqlx::query("INSERT INTO comment_reactions (comment_id, user_id, reaction) VALUES ($1, $2, $3)")
            .bind(id)
            .bind(user.id)
            .bind(reaction)
            .execute(&state.db)
            .await?;
    }
    events::publish(&state, Event::new(user.workspace_id, "comment").project(project).actor(user.id)).await;
    Ok(StatusCode::NO_CONTENT)
}

// ---- time tracking ----

pub async fn worklogs(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(key): Path<String>) -> ApiResult<Json<Value>> {
    let (id, _, _) = issue_for(&state, &user, &key, Role::Viewer).await?;
    let rows: Vec<(Uuid, Option<Uuid>, Option<String>, i32, NaiveDate, Option<String>)> = sqlx::query_as(
        "SELECT w.id, w.user_id, u.name, w.minutes, w.work_date, w.note FROM worklogs w LEFT JOIN users u ON u.id = w.user_id WHERE w.issue_id = $1 ORDER BY w.work_date DESC, w.created_at DESC",
    )
    .bind(id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(json!(
        rows.into_iter()
            .map(|(wid, uid, un, m, d, n)| json!({"id": wid, "user": uid.map(|u| json!({"id": u, "name": un})), "minutes": m, "date": d, "note": n}))
            .collect::<Vec<_>>()
    )))
}

/// `1h 30m`, `90m`, `1.5h`, `2d` (8-hour days), or plain minutes.
pub fn parse_duration(s: &str) -> Option<i32> {
    let s = s.trim().to_lowercase();
    if let Ok(n) = s.parse::<f64>() {
        return (n > 0.0).then_some(n.round() as i32);
    }
    let mut total = 0.0;
    let mut num = String::new();
    let mut any = false;
    for c in s.chars() {
        if c.is_ascii_digit() || c == '.' || c == ',' {
            num.push(if c == ',' { '.' } else { c });
        } else if matches!(c, 'w' | 'd' | 'h' | 'm') {
            let n: f64 = num.parse().ok()?;
            total += n * match c {
                'w' => 5.0 * 8.0 * 60.0,
                'd' => 8.0 * 60.0,
                'h' => 60.0,
                _ => 1.0,
            };
            num.clear();
            any = true;
        } else if !c.is_whitespace() {
            return None;
        }
    }
    if !num.is_empty() {
        return None;
    }
    (any && total > 0.0).then_some(total.round() as i32)
}

async fn refresh_spent(state: &AppState, issue: Uuid) -> ApiResult<()> {
    sqlx::query("UPDATE issues SET spent_minutes = COALESCE((SELECT sum(minutes) FROM worklogs WHERE issue_id = $1), 0), updated_at = now() WHERE id = $1")
        .bind(issue)
        .execute(&state.db)
        .await?;
    Ok(())
}

pub async fn log_work(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Path(key): Path<String>,
    Json(req): Json<Value>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let (id, project, _) = issue_for(&state, &user, &key, Role::Member).await?;
    let minutes = match &req["minutes"] {
        Value::Number(n) => n.as_f64().map(|m| m.round() as i32),
        _ => req["time"].as_str().and_then(parse_duration),
    }
    .filter(|m| *m > 0 && *m <= 24 * 60 * 7)
    .ok_or_else(|| ApiError::bad("time spent looks like 1h 30m, 45m or 2d"))?;
    let date = req["date"].as_str().and_then(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok()).unwrap_or_else(|| chrono::Utc::now().date_naive());
    let wid = new_id();
    let mut tx = state.db.begin().await?;
    sqlx::query("INSERT INTO worklogs (id, issue_id, user_id, minutes, work_date, note) VALUES ($1, $2, $3, $4, $5, $6)")
        .bind(wid)
        .bind(id)
        .bind(user.id)
        .bind(minutes)
        .bind(date)
        .bind(req["note"].as_str().map(str::trim).filter(|n| !n.is_empty()))
        .execute(&mut *tx)
        .await?;
    issues::record(
        &mut tx,
        id,
        project,
        Some(user.id),
        "worklog",
        Some(&Change { field: "worklog", old: Value::Null, new: json!({"minutes": minutes, "date": date}) }),
    )
    .await?;
    // remaining estimate goes down with the time spent (never below zero)
    if req["reduceEstimate"].as_bool().unwrap_or(false) {
        sqlx::query("UPDATE issues SET estimate_minutes = GREATEST(estimate_minutes - $2, 0) WHERE id = $1 AND estimate_minutes IS NOT NULL")
            .bind(id)
            .bind(minutes)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    refresh_spent(&state, id).await?;
    events::publish(&state, Event::new(user.workspace_id, "issue").project(project).actor(user.id)).await;
    Ok((StatusCode::CREATED, Json(json!({"id": wid, "minutes": minutes}))))
}

pub async fn delete_worklog(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    let row: Option<(Uuid, Option<Uuid>)> = sqlx::query_as("SELECT issue_id, user_id FROM worklogs WHERE id = $1").bind(id).fetch_optional(&state.db).await?;
    let (issue, owner) = row.ok_or(ApiError::NotFound("work log"))?;
    let (_, project, role) = issue_for(&state, &user, &issue.to_string(), Role::Member).await?;
    if owner != Some(user.id) && role < Role::Admin {
        return Err(ApiError::Denied("only the person who logged it or a project admin can delete it".into()));
    }
    sqlx::query("DELETE FROM worklogs WHERE id = $1").bind(id).execute(&state.db).await?;
    refresh_spent(&state, issue).await?;
    events::publish(&state, Event::new(user.workspace_id, "issue").project(project).actor(user.id)).await;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct SheetQuery {
    /// any day of the week to show (default: this week)
    from: Option<NaiveDate>,
    days: Option<i64>,
    user: Option<Uuid>,
}

/// Time logged per issue and day: a person's timesheet (yours, or anyone's for admins).
pub async fn timesheet(State(state): State<AppState>, CurrentUser(user): CurrentUser, Query(q): Query<SheetQuery>) -> ApiResult<Json<Value>> {
    let who = q.user.unwrap_or(user.id);
    if who != user.id && !user.is_admin() {
        return Err(ApiError::Denied("only admins see other people's timesheets".into()));
    }
    let today = chrono::Utc::now().date_naive();
    let from = q.from.unwrap_or_else(|| today - chrono::Duration::days(chrono::Datelike::weekday(&today).num_days_from_monday() as i64));
    let days = q.days.unwrap_or(7).clamp(1, 62);
    let to = from + chrono::Duration::days(days - 1);
    let visible = crate::access::visible_projects(&state, &user).await?;
    let rows: Vec<(Uuid, String, String, NaiveDate, i64)> = sqlx::query_as(
        "SELECT i.id, p.key || '-' || i.number, i.title, w.work_date, sum(w.minutes)::bigint FROM worklogs w JOIN issues i ON i.id = w.issue_id JOIN projects p ON p.id = i.project_id
          WHERE w.user_id = $1 AND w.work_date BETWEEN $2 AND $3 AND i.project_id = ANY($4)
          GROUP BY i.id, p.key, i.number, i.title, w.work_date ORDER BY p.key, i.number",
    )
    .bind(who)
    .bind(from)
    .bind(to)
    .bind(&visible)
    .fetch_all(&state.db)
    .await?;
    let mut issues: Vec<Value> = Vec::new();
    for (id, key, title, day, minutes) in rows {
        match issues.iter_mut().find(|i| i["id"] == json!(id)) {
            Some(i) => {
                i["days"][day.to_string()] = json!(minutes);
            }
            None => {
                let mut d = serde_json::Map::new();
                d.insert(day.to_string(), json!(minutes));
                issues.push(json!({"id": id, "key": key, "title": title, "days": d}));
            }
        }
    }
    Ok(Json(json!({"from": from, "to": to, "days": days, "issues": issues})))
}

#[cfg(test)]
mod tests {
    use super::parse_duration;

    #[test]
    fn durations() {
        assert_eq!(parse_duration("1h 30m"), Some(90));
        assert_eq!(parse_duration("45m"), Some(45));
        assert_eq!(parse_duration("1.5h"), Some(90));
        assert_eq!(parse_duration("2d"), Some(960));
        assert_eq!(parse_duration("30"), Some(30));
        assert_eq!(parse_duration("1,5h"), Some(90));
        assert_eq!(parse_duration("soon"), None);
        assert_eq!(parse_duration("3x"), None);
        assert_eq!(parse_duration("0m"), None);
    }
}
