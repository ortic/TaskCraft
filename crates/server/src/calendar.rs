//! Calendar feeds: secret addresses calendar apps subscribe to (iCalendar, RFC 5545), and `.ics`
//! downloads of a search. A feed is read as its owner — it shows what they may see, never more —
//! and holds issues with due dates (from their start date when they have one), sprints and
//! releases. Only the token's hash is stored; the address is shown when the feed is made or
//! renewed (which revokes the old one). The workspace can turn feeds off.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, NaiveDate, Utc};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::app::{AppState, audit, new_id};
use crate::auth::{CurrentUser, User, random_token, token_hash};
use crate::error::{ApiError, ApiResult};
use crate::issues::Summary;

// ---- iCalendar ----

/// Text as iCalendar wants it: backslash, semicolon, comma and line breaks escaped.
pub fn escape(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => o.push_str("\\\\"),
            ';' => o.push_str("\\;"),
            ',' => o.push_str("\\,"),
            '\n' => o.push_str("\\n"),
            '\r' => {}
            c => o.push(c),
        }
    }
    o
}

/// One content line, folded at 75 octets (continuations start with a space), ending in CRLF.
pub fn line(out: &mut String, l: &str) {
    let mut width = 0;
    for c in l.chars() {
        let n = c.len_utf8();
        if width + n > 75 {
            out.push_str("\r\n ");
            width = 1;
        }
        out.push(c);
        width += n;
    }
    out.push_str("\r\n");
}

/// An all-day event from `start` to `end` (both days included).
pub struct Event {
    pub uid: String,
    pub start: NaiveDate,
    pub end: NaiveDate,
    pub summary: String,
    pub description: String,
    pub url: Option<String>,
    pub stamp: DateTime<Utc>,
    pub categories: &'static str,
}

pub fn render(name: &str, events: &[Event]) -> String {
    let mut o = String::new();
    for l in ["BEGIN:VCALENDAR", "VERSION:2.0", "PRODID:-//TaskCraft//Calendar feed//EN", "CALSCALE:GREGORIAN", "METHOD:PUBLISH"] {
        line(&mut o, l);
    }
    line(&mut o, &format!("X-WR-CALNAME:{}", escape(name)));
    for e in events {
        line(&mut o, "BEGIN:VEVENT");
        line(&mut o, &format!("UID:{}", e.uid));
        line(&mut o, &format!("DTSTAMP:{}", e.stamp.format("%Y%m%dT%H%M%SZ")));
        line(&mut o, &format!("DTSTART;VALUE=DATE:{}", e.start.format("%Y%m%d")));
        line(&mut o, &format!("DTEND;VALUE=DATE:{}", (e.end.max(e.start) + chrono::Duration::days(1)).format("%Y%m%d")));
        line(&mut o, &format!("SUMMARY:{}", escape(&e.summary)));
        if !e.description.is_empty() {
            line(&mut o, &format!("DESCRIPTION:{}", escape(&e.description)));
        }
        if let Some(u) = &e.url {
            line(&mut o, &format!("URL:{u}"));
        }
        line(&mut o, &format!("CATEGORIES:{}", e.categories));
        line(&mut o, "TRANSP:TRANSPARENT");
        line(&mut o, "END:VEVENT");
    }
    line(&mut o, "END:VCALENDAR");
    o
}

fn host(state: &AppState) -> String {
    let b = state.config.base();
    b.split("://").nth(1).unwrap_or(b).split('/').next().unwrap_or("taskcraft").to_string()
}

/// Issues with a due date as events: from their start date (when it's before) to the due date.
fn issue_events(state: &AppState, issues: &[Summary]) -> Vec<Event> {
    let host = host(state);
    issues
        .iter()
        .filter_map(|i| {
            let due = i.due_date?;
            let start = i.start_date.filter(|s| *s <= due && (due - *s).num_days() <= 90).unwrap_or(due);
            let mut desc = format!("{} · {}", i.status.name, i.project_name);
            if let Some(a) = &i.assignee {
                desc.push_str(&format!(" · {}", a.name));
            }
            let done = if i.status.category == "done" { " ✓" } else { "" };
            Some(Event {
                uid: format!("issue-{}@{host}", i.id),
                start,
                end: due,
                summary: format!("{} {}{done}", i.key, i.title),
                description: desc,
                url: Some(state.config.issue_url(&i.key)),
                stamp: i.updated_at,
                categories: "ISSUE",
            })
        })
        .collect()
}

/// Sprints (with dates) and releases (with a release date) of these projects.
async fn plan_events(state: &AppState, projects: &[Uuid], sprints: bool, releases: bool) -> ApiResult<Vec<Event>> {
    let host = host(state);
    let mut out = Vec::new();
    if sprints {
        type S = (Uuid, String, String, Option<String>, DateTime<Utc>, DateTime<Utc>, String);
        let rows: Vec<S> = sqlx::query_as(
            "SELECT s.id, s.name, p.key, s.goal, s.start_at, s.end_at, s.state FROM sprints s JOIN projects p ON p.id = s.project_id
              WHERE s.project_id = ANY($1) AND s.start_at IS NOT NULL AND s.end_at IS NOT NULL AND s.end_at > now() - interval '180 days'
              ORDER BY s.start_at LIMIT 500",
        )
        .bind(projects)
        .fetch_all(&state.db)
        .await?;
        for (id, name, key, goal, start, end, st) in rows {
            out.push(Event {
                uid: format!("sprint-{id}@{host}"),
                start: start.date_naive(),
                end: end.date_naive(),
                summary: format!("{name} ({key})"),
                description: [goal.unwrap_or_default(), st].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" · "),
                url: Some(format!("{}/projects/{key}/backlog", state.config.base())),
                stamp: start,
                categories: "SPRINT",
            });
        }
    }
    if releases {
        type R = (Uuid, String, String, NaiveDate, String, Option<String>);
        let rows: Vec<R> = sqlx::query_as(
            "SELECT v.id, v.name, p.key, v.release_date, v.status, v.description FROM versions v JOIN projects p ON p.id = v.project_id
              WHERE v.project_id = ANY($1) AND v.release_date IS NOT NULL AND v.status <> 'archived' ORDER BY v.release_date LIMIT 500",
        )
        .bind(projects)
        .fetch_all(&state.db)
        .await?;
        for (id, name, key, date, status, description) in rows {
            out.push(Event {
                uid: format!("release-{id}@{host}"),
                start: date,
                end: date,
                summary: format!("{key} {name}{}", if status == "released" { " ✓" } else { "" }),
                description: description.unwrap_or_default(),
                url: Some(format!("{}/projects/{key}/releases", state.config.base())),
                stamp: date.and_hms_opt(0, 0, 0).map(|d| d.and_utc()).unwrap_or_else(Utc::now),
                categories: "RELEASE",
            });
        }
    }
    Ok(out)
}

/// Absences of these people (from 30 days back): "away" for others; one's own say why.
async fn absence_events(state: &AppState, people: &[Uuid], me: Uuid) -> ApiResult<Vec<Event>> {
    let host = host(state);
    type A = (Uuid, Uuid, String, NaiveDate, NaiveDate, Option<String>, String, DateTime<Utc>);
    let rows: Vec<A> = sqlx::query_as(
        "SELECT a.id, a.user_id, u.name, a.start_date, a.end_date, a.half, a.kind, a.created_at FROM absences a JOIN users u ON u.id = a.user_id
          WHERE a.user_id = ANY($1) AND a.end_date >= now()::date - 30 ORDER BY a.start_date LIMIT 1000",
    )
    .bind(people)
    .fetch_all(&state.db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(id, uid, name, start, end, half, kind, at)| {
            let half = match half.as_deref() {
                Some("am") => " (morning)",
                Some("pm") => " (afternoon)",
                _ => "",
            };
            let summary = if uid == me { format!("Away: {kind}{half}") } else { format!("{name} away{half}") };
            Event { uid: format!("absence-{id}@{host}"), start, end, summary, description: String::new(), url: None, stamp: at, categories: "ABSENCE" }
        })
        .collect())
}

fn ics(name: &str, body: String) -> Response {
    let file = format!("{}.ics", name.chars().map(|c| if c.is_alphanumeric() || c == '-' { c } else { '-' }).collect::<String>());
    (
        [
            (header::CONTENT_TYPE, "text/calendar; charset=utf-8".to_string()),
            (header::CONTENT_DISPOSITION, format!("inline; filename=\"{file}\"")),
            (header::CACHE_CONTROL, "private, max-age=300".to_string()),
        ],
        body,
    )
        .into_response()
}

const LIMIT: i64 = 1000;

async fn due_issues(state: &AppState, user: &User, q: &str, project: Option<&str>) -> ApiResult<Vec<Summary>> {
    let r = crate::routes::issues::run_search_in(state, user, q, project, false, LIMIT, 0, "i.due_date, i.id").await?;
    Ok(r.issues.into_iter().filter(|i| i.due_date.is_some()).collect())
}

/// `GET /search.ics?q=&project=`: the issues of a search that have a due date, as a calendar file.
pub async fn search_ics(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> ApiResult<Response> {
    let query = q.get("q").cloned().unwrap_or_default();
    let project = q.get("project").map(String::as_str);
    let issues = due_issues(&state, &user, &query, project).await?;
    let name = project.map(|p| format!("{p} due dates")).unwrap_or_else(|| "TaskCraft due dates".into());
    Ok(ics(&name, render(&name, &issue_events(&state, &issues))))
}

// ---- feeds ----

fn parts(v: &Value) -> (bool, bool, bool, bool) {
    let on = |k: &str| v.get(k).and_then(Value::as_bool).unwrap_or(true);
    (on("issues"), on("sprints"), on("releases"), on("absences"))
}

fn feed_url(state: &AppState, token: &str) -> String {
    format!("{}/api/calendar/{token}.ics", state.config.base())
}

/// The projects a person belongs to: as a member, through a team, or leading them.
async fn my_projects(state: &AppState, user: &User) -> ApiResult<Vec<Uuid>> {
    Ok(sqlx::query_scalar(
        "SELECT p.id FROM projects p WHERE p.workspace_id = $1 AND p.archived_at IS NULL AND (
            p.lead_id = $2
            OR EXISTS (SELECT 1 FROM project_members m WHERE m.project_id = p.id AND m.subject_type = 'user' AND m.subject_id = $2)
            OR EXISTS (SELECT 1 FROM project_members m JOIN team_members tm ON tm.team_id = m.subject_id
                        WHERE m.project_id = p.id AND m.subject_type = 'team' AND tm.user_id = $2))",
    )
    .bind(user.workspace_id)
    .bind(user.id)
    .fetch_all(&state.db)
    .await?)
}

/// A feed's name, events and owner, read as its owner.
async fn build(state: &AppState, user: &User, kind: &str, view: Option<Uuid>, project: Option<Uuid>, p: &Value) -> ApiResult<(String, Vec<Event>)> {
    let (issues, sprints, releases, absences) = parts(p);
    let visible = crate::access::visible_projects(state, user).await?;
    match kind {
        "mine" => {
            let mut ev = if issues { issue_events(state, &due_issues(state, user, "assignee = me() and category != done", None).await?) } else { vec![] };
            let mine: Vec<Uuid> = my_projects(state, user).await?.into_iter().filter(|p| visible.contains(p)).collect();
            ev.extend(plan_events(state, &mine, sprints, releases).await?);
            if absences {
                ev.extend(absence_events(state, &[user.id], user.id).await?);
            }
            Ok((format!("TaskCraft · {}", user.name), ev))
        }
        "filter" => {
            let row: Option<(String, String, Option<String>)> = sqlx::query_as(
                "SELECT v.name, v.query, p.key FROM views v LEFT JOIN projects p ON p.id = v.project_id
                  WHERE v.id = $1 AND v.workspace_id = $2 AND (v.owner_id = $3 OR v.shared)",
            )
            .bind(view)
            .bind(user.workspace_id)
            .bind(user.id)
            .fetch_optional(&state.db)
            .await?;
            let (name, q, key) = row.ok_or(ApiError::NotFound("saved filter"))?;
            Ok((name, issue_events(state, &due_issues(state, user, &q, key.as_deref()).await?)))
        }
        "project" => {
            let pid = project.filter(|p| visible.contains(p)).ok_or(ApiError::NotFound("project"))?;
            let (key, name): (String, String) = sqlx::query_as("SELECT key, name FROM projects WHERE id = $1").bind(pid).fetch_one(&state.db).await?;
            let mut ev = if issues { issue_events(state, &due_issues(state, user, "due is not empty", Some(&key)).await?) } else { vec![] };
            ev.extend(plan_events(state, &[pid], sprints, releases).await?);
            if absences {
                let members: Vec<Uuid> = sqlx::query_scalar(
                    "SELECT m.subject_id FROM project_members m WHERE m.project_id = $1 AND m.subject_type = 'user'
                     UNION SELECT tm.user_id FROM project_members m JOIN team_members tm ON tm.team_id = m.subject_id WHERE m.project_id = $1 AND m.subject_type = 'team'",
                )
                .bind(pid)
                .fetch_all(&state.db)
                .await?;
                ev.extend(absence_events(state, &members, user.id).await?);
            }
            Ok((name, ev))
        }
        _ => Err(ApiError::NotFound("calendar feed")),
    }
}

/// `GET /calendar/{token}.ics` (no sign-in: the address is the secret).
pub async fn feed(State(state): State<AppState>, Path(token): Path<String>) -> ApiResult<Response> {
    let token = token.trim_end_matches(".ics");
    type F = (Uuid, Uuid, String, Option<Uuid>, Option<Uuid>, Value);
    let row: Option<F> = sqlx::query_as("SELECT id, user_id, kind, view_id, project_id, parts FROM calendar_feeds WHERE token_hash = $1")
        .bind(token_hash(token))
        .fetch_optional(&state.db)
        .await?;
    let (id, owner, kind, view, project, p) = row.ok_or(ApiError::NotFound("calendar feed"))?;
    let user: Option<User> = sqlx::query_as("SELECT id, workspace_id, email, name, role, status FROM users WHERE id = $1 AND status = 'active'")
        .bind(owner)
        .fetch_optional(&state.db)
        .await?;
    let user = user.ok_or(ApiError::NotFound("calendar feed"))?;
    if !crate::features::on(&state, user.workspace_id, "calendarFeeds").await {
        return Err(ApiError::NotFound("calendar feed"));
    }
    let (name, events) = build(&state, &user, &kind, view, project, &p).await?;
    let _ =
        sqlx::query("UPDATE calendar_feeds SET last_used_at = now() WHERE id = $1 AND (last_used_at IS NULL OR last_used_at < now() - interval '5 minutes')")
            .bind(id)
            .execute(&state.db)
            .await;
    Ok(ics(&name, render(&name, &events)))
}

/// `GET /me/calendar-feeds`: the caller's feeds (their addresses aren't kept, only shown once).
pub async fn list(State(state): State<AppState>, CurrentUser(user): CurrentUser) -> ApiResult<Json<Value>> {
    type F = (Uuid, String, Option<Uuid>, Option<String>, Option<String>, Value, DateTime<Utc>, Option<DateTime<Utc>>);
    let rows: Vec<F> = sqlx::query_as(
        "SELECT f.id, f.kind, f.view_id, v.name, p.key, f.parts, f.created_at, f.last_used_at FROM calendar_feeds f
           LEFT JOIN views v ON v.id = f.view_id LEFT JOIN projects p ON p.id = f.project_id
          WHERE f.user_id = $1 ORDER BY f.created_at",
    )
    .bind(user.id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(json!(
        rows.into_iter()
            .map(|(id, kind, view, vname, pkey, p, at, used)| json!({
                "id": id, "kind": kind, "viewId": view, "viewName": vname, "projectKey": pkey, "parts": p, "createdAt": at, "lastUsedAt": used,
            }))
            .collect::<Vec<_>>()
    )))
}

/// `POST /me/calendar-feeds` `{kind: mine|filter|project, viewId?, projectKey?, parts?}`: a new
/// feed; its address comes back once.
pub async fn create(State(state): State<AppState>, CurrentUser(user): CurrentUser, Json(req): Json<Value>) -> ApiResult<(StatusCode, Json<Value>)> {
    crate::features::require(&state, user.workspace_id, "calendarFeeds").await?;
    let kind = req["kind"].as_str().unwrap_or("mine");
    let mut view = None;
    let mut project = None;
    match kind {
        "mine" => {}
        "filter" => {
            view = Some(req["viewId"].as_str().and_then(|v| v.parse::<Uuid>().ok()).ok_or_else(|| ApiError::bad("a filter feed needs viewId"))?);
        }
        "project" => {
            let key = req["projectKey"].as_str().ok_or_else(|| ApiError::bad("a project feed needs projectKey"))?;
            project = Some(crate::routes::projects::by_key(&state, &user, key, crate::access::Role::Viewer).await?.0.id);
        }
        _ => return Err(ApiError::bad("kind is mine, filter or project")),
    }
    let p = match &req["parts"] {
        Value::Null => json!({}),
        Value::Object(o) => {
            let mut out = serde_json::Map::new();
            for k in ["issues", "sprints", "releases", "absences"] {
                if let Some(v) = o.get(k) {
                    out.insert(k.into(), json!(v.as_bool().ok_or_else(|| ApiError::bad(format!("parts.{k} is true or false")))?));
                }
            }
            Value::Object(out)
        }
        _ => return Err(ApiError::bad("parts is an object like {\"sprints\": false}")),
    };
    // it must be readable now (a saved filter they may see, a project they're in)
    build(&state, &user, kind, view, project, &p).await?;
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM calendar_feeds WHERE user_id = $1").bind(user.id).fetch_one(&state.db).await?;
    if n >= 50 {
        return Err(ApiError::bad("that's 50 calendar feeds already: delete one first"));
    }
    let token = random_token();
    let id = new_id();
    sqlx::query("INSERT INTO calendar_feeds (id, user_id, token_hash, kind, view_id, project_id, parts) VALUES ($1, $2, $3, $4, $5, $6, $7)")
        .bind(id)
        .bind(user.id)
        .bind(token_hash(&token))
        .bind(kind)
        .bind(view)
        .bind(project)
        .bind(&p)
        .execute(&state.db)
        .await?;
    audit(&state, user.workspace_id, Some(user.id), "calendar.feed", Some(kind.to_string()), json!({"created": true})).await;
    Ok((StatusCode::CREATED, Json(json!({"id": id, "url": feed_url(&state, &token)}))))
}

/// `POST /me/calendar-feeds/{id}/renew`: a new address; the old one stops working.
pub async fn renew(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(id): Path<Uuid>) -> ApiResult<Json<Value>> {
    let token = random_token();
    let n = sqlx::query("UPDATE calendar_feeds SET token_hash = $3, last_used_at = NULL WHERE id = $1 AND user_id = $2")
        .bind(id)
        .bind(user.id)
        .bind(token_hash(&token))
        .execute(&state.db)
        .await?
        .rows_affected();
    if n == 0 {
        return Err(ApiError::NotFound("calendar feed"));
    }
    Ok(Json(json!({"id": id, "url": feed_url(&state, &token)})))
}

pub async fn delete(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    let n = sqlx::query("DELETE FROM calendar_feeds WHERE id = $1 AND user_id = $2").bind(id).bind(user.id).execute(&state.db).await?.rows_affected();
    if n == 0 {
        return Err(ApiError::NotFound("calendar feed"));
    }
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_and_folds() {
        assert_eq!(escape("a,b;c\\d\ne"), "a\\,b\\;c\\\\d\\ne");
        let mut o = String::new();
        line(&mut o, &format!("SUMMARY:{}", "ä".repeat(60)));
        let lines: Vec<&str> = o.split("\r\n").filter(|l| !l.is_empty()).collect();
        assert!(lines.iter().all(|l| l.len() <= 75), "{lines:?}");
        assert!(lines[1].starts_with(' '));
        assert_eq!(o.replace("\r\n ", "").trim_end(), format!("SUMMARY:{}", "ä".repeat(60)));
    }

    #[test]
    fn all_day_events() {
        let d = NaiveDate::from_ymd_opt(2026, 11, 20).unwrap();
        let s = render(
            "Web, mine",
            &[Event {
                uid: "issue-1@x".into(),
                start: d,
                end: d,
                summary: "WEB-1 Fix it".into(),
                description: String::new(),
                url: None,
                stamp: Utc::now(),
                categories: "ISSUE",
            }],
        );
        assert!(s.starts_with("BEGIN:VCALENDAR\r\n"));
        assert!(s.contains("X-WR-CALNAME:Web\\, mine\r\n"));
        assert!(s.contains("DTSTART;VALUE=DATE:20261120\r\nDTEND;VALUE=DATE:20261121\r\n"));
        assert!(s.ends_with("END:VCALENDAR\r\n"));
    }
}
