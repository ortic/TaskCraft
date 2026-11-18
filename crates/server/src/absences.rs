//! Absences and public holidays: when people are away, so planning knows who is there. Everyone
//! sees that someone is away and until when; why (vacation, sick, …) only the person and
//! workspace admins see. A sprint's capacity is each person's working days in it (Monday to
//! Friday) minus the public holidays of their region calendar and their absences. While someone
//! is away, their substitute is told about new assignments and missed service-level goals.

use std::collections::HashMap;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use chrono::{Datelike, NaiveDate, Weekday};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::access::Role;
use crate::app::{AppState, audit, new_id};
use crate::auth::{CurrentUser, User};
use crate::error::{ApiError, ApiResult};

pub const KINDS: [&str; 4] = ["vacation", "sick", "training", "other"];

/// Today in the workspace's time zone.
pub async fn today(state: &AppState, workspace: Uuid) -> ApiResult<NaiveDate> {
    let zone: Option<String> = sqlx::query_scalar("SELECT settings->>'timeZone' FROM workspaces WHERE id = $1").bind(workspace).fetch_one(&state.db).await?;
    let tz: chrono_tz::Tz = zone.and_then(|z| z.parse().ok()).unwrap_or(chrono_tz::UTC);
    Ok(chrono::Utc::now().with_timezone(&tz).date_naive())
}

/// Who is away today: person → (back after this day, their substitute).
pub async fn away_now(state: &AppState, workspace: Uuid) -> ApiResult<HashMap<Uuid, (NaiveDate, Option<Uuid>)>> {
    let day = today(state, workspace).await?;
    let rows: Vec<(Uuid, NaiveDate, Option<Uuid>)> = sqlx::query_as(
        "SELECT user_id, end_date, substitute_id FROM absences WHERE workspace_id = $1 AND start_date <= $2 AND end_date >= $2 ORDER BY end_date",
    )
    .bind(workspace)
    .bind(day)
    .fetch_all(&state.db)
    .await?;
    let mut out = HashMap::new();
    for (u, until, sub) in rows {
        out.insert(u, (until, sub));
    }
    Ok(out)
}

/// The substitute of someone away today (and until when).
pub async fn substitute(state: &AppState, workspace: Uuid, user: Uuid) -> Option<(Uuid, NaiveDate)> {
    let day = today(state, workspace).await.ok()?;
    sqlx::query_as::<_, (Option<Uuid>, NaiveDate)>(
        "SELECT a.substitute_id, a.end_date FROM absences a JOIN users s ON s.id = a.substitute_id AND s.status = 'active'
          WHERE a.user_id = $1 AND a.start_date <= $2 AND a.end_date >= $2 ORDER BY a.end_date DESC LIMIT 1",
    )
    .bind(user)
    .bind(day)
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten()
    .and_then(|(s, until)| s.map(|s| (s, until)))
}

/// A person's days in a period: working days (Monday to Friday), public holidays among them,
/// days away (half days count half) and what's left.
#[derive(Debug, Clone, serde::Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Availability {
    pub user: Uuid,
    pub name: String,
    pub working_days: f64,
    pub holidays: f64,
    pub away: f64,
    pub available: f64,
}

/// The pure part: which of the working days are holidays and which are away.
pub fn count(from: NaiveDate, to: NaiveDate, holidays: &[NaiveDate], absences: &[(NaiveDate, NaiveDate, Option<String>)]) -> (f64, f64, f64) {
    let (mut work, mut hol, mut away) = (0.0, 0.0, 0.0);
    let mut d = from;
    while d <= to {
        if !matches!(d.weekday(), Weekday::Sat | Weekday::Sun) {
            work += 1.0;
            if holidays.contains(&d) {
                hol += 1.0;
            } else if let Some((_, _, half)) = absences.iter().find(|(s, e, _)| *s <= d && d <= *e) {
                away += if half.is_some() { 0.5 } else { 1.0 };
            }
        }
        match d.succ_opt() {
            Some(n) => d = n,
            None => break,
        }
    }
    (work, hol, away)
}

pub async fn availability(state: &AppState, workspace: Uuid, users: &[Uuid], from: NaiveDate, to: NaiveDate) -> ApiResult<Vec<Availability>> {
    let people: Vec<(Uuid, String, Option<String>)> =
        sqlx::query_as("SELECT id, name, holiday_region FROM users WHERE id = ANY($1) AND workspace_id = $2 AND status = 'active' ORDER BY lower(name)")
            .bind(users)
            .bind(workspace)
            .fetch_all(&state.db)
            .await?;
    let holidays: Vec<(String, NaiveDate)> = sqlx::query_as("SELECT region, day FROM holidays WHERE workspace_id = $1 AND day BETWEEN $2 AND $3")
        .bind(workspace)
        .bind(from)
        .bind(to)
        .fetch_all(&state.db)
        .await?;
    let absences: Vec<(Uuid, NaiveDate, NaiveDate, Option<String>)> =
        sqlx::query_as("SELECT user_id, start_date, end_date, half FROM absences WHERE user_id = ANY($1) AND start_date <= $3 AND end_date >= $2")
            .bind(users)
            .bind(from)
            .bind(to)
            .fetch_all(&state.db)
            .await?;
    Ok(people
        .into_iter()
        .map(|(id, name, region)| {
            let region = region.unwrap_or_default();
            let hol: Vec<NaiveDate> = holidays.iter().filter(|(r, _)| *r == region).map(|(_, d)| *d).collect();
            let abs: Vec<(NaiveDate, NaiveDate, Option<String>)> = absences.iter().filter(|a| a.0 == id).map(|a| (a.1, a.2, a.3.clone())).collect();
            let (work, h, away) = count(from, to, &hol, &abs);
            Availability { user: id, name, working_days: work, holidays: h, away, available: (work - h - away).max(0.0) }
        })
        .collect())
}

// ---- absences ----

#[derive(Deserialize)]
pub struct RangeQuery {
    from: Option<NaiveDate>,
    to: Option<NaiveDate>,
    user: Option<Uuid>,
    /// only the members of this project (people and teams)
    project: Option<String>,
}

/// `GET /absences?from=&to=&user=&project=`: absences in a period (default: the last 30 days to
/// the next 180), of one person or a project's members. Why someone is away only for themselves
/// and admins.
pub async fn list(State(state): State<AppState>, CurrentUser(user): CurrentUser, Query(q): Query<RangeQuery>) -> ApiResult<Json<Value>> {
    let day = today(&state, user.workspace_id).await?;
    let members: Option<Vec<Uuid>> = match q.project.as_deref().filter(|p| !p.is_empty()) {
        Some(key) => {
            let (p, _) = crate::routes::projects::by_key(&state, &user, key, Role::Viewer).await?;
            Some(
                sqlx::query_scalar(
                    "SELECT m.subject_id FROM project_members m WHERE m.project_id = $1 AND m.subject_type = 'user'
                     UNION SELECT tm.user_id FROM project_members m JOIN team_members tm ON tm.team_id = m.subject_id WHERE m.project_id = $1 AND m.subject_type = 'team'
                     UNION SELECT DISTINCT assignee_id FROM issues WHERE project_id = $1 AND assignee_id IS NOT NULL AND deleted_at IS NULL",
                )
                .bind(p.id)
                .fetch_all(&state.db)
                .await?,
            )
        }
        None => None,
    };
    let from = q.from.unwrap_or(day - chrono::Duration::days(30));
    let to = q.to.unwrap_or(day + chrono::Duration::days(180));
    type R = (Uuid, Uuid, String, NaiveDate, NaiveDate, Option<String>, String, Option<String>, Option<Uuid>, Option<String>);
    let rows: Vec<R> = sqlx::query_as(
        "SELECT a.id, a.user_id, u.name, a.start_date, a.end_date, a.half, a.kind, a.note, a.substitute_id, s.name
           FROM absences a JOIN users u ON u.id = a.user_id LEFT JOIN users s ON s.id = a.substitute_id
          WHERE a.workspace_id = $1 AND a.start_date <= $3 AND a.end_date >= $2 AND ($4::uuid IS NULL OR a.user_id = $4)
            AND ($5::uuid[] IS NULL OR a.user_id = ANY($5))
          ORDER BY a.start_date, lower(u.name) LIMIT 2000",
    )
    .bind(user.workspace_id)
    .bind(from)
    .bind(to)
    .bind(q.user)
    .bind(members)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(json!(
        rows.into_iter()
            .map(|(id, uid, name, start, end, half, kind, note, sub, sub_name)| {
                let mine = uid == user.id || user.is_admin();
                json!({
                    "id": id, "user": {"id": uid, "name": name}, "start": start, "end": end, "half": half,
                    "kind": if mine { Some(kind) } else { None }, "note": if mine { note } else { None },
                    "substitute": sub.map(|s| json!({"id": s, "name": sub_name})), "editable": mine,
                })
            })
            .collect::<Vec<_>>()
    )))
}

struct Fields {
    start: NaiveDate,
    end: NaiveDate,
    half: Option<String>,
    kind: String,
    note: Option<String>,
    substitute: Option<Uuid>,
}

async fn fields(state: &AppState, user: &User, owner: Uuid, req: &Value, old: Option<&Fields>) -> ApiResult<Fields> {
    let date = |k: &str, cur: Option<NaiveDate>| -> ApiResult<NaiveDate> {
        match &req[k] {
            Value::Null => cur.ok_or_else(|| ApiError::bad(format!("{k} is a date like 2026-12-24"))),
            v => v.as_str().and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok()).ok_or_else(|| ApiError::bad(format!("{k} is a date like 2026-12-24"))),
        }
    };
    let start = date("start", old.map(|o| o.start))?;
    let end = match (&req["end"], old) {
        (Value::Null, Some(o)) if req.get("start").is_none() => o.end,
        (Value::Null, _) => start,
        _ => date("end", None)?,
    };
    if end < start {
        return Err(ApiError::bad("an absence ends on or after the day it starts"));
    }
    if (end - start).num_days() > 366 {
        return Err(ApiError::bad("an absence is at most a year long"));
    }
    let half = match req.get("half") {
        None => old.and_then(|o| o.half.clone()),
        Some(Value::Null) => None,
        Some(v) => Some(v.as_str().filter(|h| ["am", "pm"].contains(h)).ok_or_else(|| ApiError::bad("half is am, pm or null"))?.to_string()),
    };
    if half.is_some() && start != end {
        return Err(ApiError::bad("a half day is a single day"));
    }
    let kind = match req.get("kind") {
        None | Some(Value::Null) => old.map(|o| o.kind.clone()).unwrap_or_else(|| "vacation".into()),
        Some(v) => v.as_str().filter(|k| KINDS.contains(k)).ok_or_else(|| ApiError::bad("kind is vacation, sick, training or other"))?.to_string(),
    };
    let note = match req.get("note") {
        None => old.and_then(|o| o.note.clone()),
        Some(v) => v.as_str().map(str::trim).filter(|n| !n.is_empty()).map(|n| n.chars().take(500).collect()),
    };
    let substitute = match req.get("substituteId") {
        None => old.and_then(|o| o.substitute),
        Some(Value::Null) => None,
        Some(v) => {
            let s: Uuid = v.as_str().and_then(|s| s.parse().ok()).ok_or_else(|| ApiError::bad("substituteId is a person's id"))?;
            let ok: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM users WHERE id = $1 AND workspace_id = $2 AND status = 'active')")
                .bind(s)
                .bind(user.workspace_id)
                .fetch_one(&state.db)
                .await?;
            if !ok || s == owner {
                return Err(ApiError::bad("the substitute is someone else in this workspace"));
            }
            Some(s)
        }
    };
    Ok(Fields { start, end, half, kind, note, substitute })
}

/// `POST /absences` `{start, end?, half?, kind?, note?, substituteId?, userId?}`: one's own
/// absence; admins enter them for anyone (`userId`).
pub async fn create(State(state): State<AppState>, CurrentUser(user): CurrentUser, Json(req): Json<Value>) -> ApiResult<(StatusCode, Json<Value>)> {
    let owner: Uuid = match req["userId"].as_str().filter(|s| !s.is_empty()) {
        Some(u) => {
            let u: Uuid = u.parse().map_err(|_| ApiError::bad("userId is a person's id"))?;
            if u != user.id && !user.is_admin() {
                return Err(ApiError::Denied("only workspace admins enter absences for others".into()));
            }
            let ok: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM users WHERE id = $1 AND workspace_id = $2)")
                .bind(u)
                .bind(user.workspace_id)
                .fetch_one(&state.db)
                .await?;
            if !ok {
                return Err(ApiError::NotFound("person"));
            }
            u
        }
        None => user.id,
    };
    let f = fields(&state, &user, owner, &req, None).await?;
    let id = new_id();
    sqlx::query(
        "INSERT INTO absences (id, workspace_id, user_id, start_date, end_date, half, kind, note, substitute_id, created_by)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
    )
    .bind(id)
    .bind(user.workspace_id)
    .bind(owner)
    .bind(f.start)
    .bind(f.end)
    .bind(&f.half)
    .bind(&f.kind)
    .bind(&f.note)
    .bind(f.substitute)
    .bind(user.id)
    .execute(&state.db)
    .await?;
    if owner != user.id {
        audit(&state, user.workspace_id, Some(user.id), "absence.created", Some(owner.to_string()), json!({"start": f.start, "end": f.end})).await;
    }
    Ok((StatusCode::CREATED, Json(json!({"id": id}))))
}

async fn editable(state: &AppState, user: &User, id: Uuid) -> ApiResult<(Uuid, Fields)> {
    type R = (Uuid, NaiveDate, NaiveDate, Option<String>, String, Option<String>, Option<Uuid>);
    let r: Option<R> =
        sqlx::query_as("SELECT user_id, start_date, end_date, half, kind, note, substitute_id FROM absences WHERE id = $1 AND workspace_id = $2")
            .bind(id)
            .bind(user.workspace_id)
            .fetch_optional(&state.db)
            .await?;
    let (owner, start, end, half, kind, note, substitute) = r.ok_or(ApiError::NotFound("absence"))?;
    if owner != user.id && !user.is_admin() {
        return Err(ApiError::NotFound("absence"));
    }
    Ok((owner, Fields { start, end, half, kind, note, substitute }))
}

pub async fn update(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(id): Path<Uuid>, Json(req): Json<Value>) -> ApiResult<StatusCode> {
    let (owner, old) = editable(&state, &user, id).await?;
    let f = fields(&state, &user, owner, &req, Some(&old)).await?;
    sqlx::query("UPDATE absences SET start_date = $2, end_date = $3, half = $4, kind = $5, note = $6, substitute_id = $7 WHERE id = $1")
        .bind(id)
        .bind(f.start)
        .bind(f.end)
        .bind(&f.half)
        .bind(&f.kind)
        .bind(&f.note)
        .bind(f.substitute)
        .execute(&state.db)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn delete(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    editable(&state, &user, id).await?;
    sqlx::query("DELETE FROM absences WHERE id = $1").bind(id).execute(&state.db).await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---- public holidays ----

/// `GET /holidays`: the workspace's region calendars and their holidays (last year to two years ahead).
pub async fn holidays(State(state): State<AppState>, CurrentUser(user): CurrentUser) -> ApiResult<Json<Value>> {
    let rows: Vec<(Uuid, String, NaiveDate, String)> = sqlx::query_as(
        "SELECT id, region, day, name FROM holidays WHERE workspace_id = $1 AND day >= now()::date - 366 AND day <= now()::date + 731 ORDER BY day, region",
    )
    .bind(user.workspace_id)
    .fetch_all(&state.db)
    .await?;
    let mut regions: Vec<String> =
        sqlx::query_scalar("SELECT DISTINCT region FROM holidays WHERE workspace_id = $1 ORDER BY region").bind(user.workspace_id).fetch_all(&state.db).await?;
    if !regions.contains(&String::new()) {
        regions.insert(0, String::new());
    }
    let mine: Option<String> = sqlx::query_scalar("SELECT holiday_region FROM users WHERE id = $1").bind(user.id).fetch_one(&state.db).await?;
    Ok(Json(json!({
        "regions": regions,
        "myRegion": mine.unwrap_or_default(),
        "holidays": rows.into_iter().map(|(id, region, day, name)| json!({"id": id, "region": region, "day": day, "name": name})).collect::<Vec<_>>(),
    })))
}

fn admin(user: &User) -> ApiResult<()> {
    if user.is_admin() { Ok(()) } else { Err(ApiError::Denied("only workspace admins keep public holidays".into())) }
}

fn region(v: &Value) -> ApiResult<String> {
    let r = v.as_str().map(str::trim).unwrap_or("").to_string();
    if r.chars().count() > 60 {
        return Err(ApiError::bad("region names are at most 60 characters"));
    }
    Ok(r)
}

/// `POST /holidays` `{day, name, region?}` or `{holidays: [{day, name, region?}, …]}` (admins);
/// a day already in that calendar gets the new name.
pub async fn add_holidays(State(state): State<AppState>, CurrentUser(user): CurrentUser, Json(req): Json<Value>) -> ApiResult<(StatusCode, Json<Value>)> {
    admin(&user)?;
    let list: Vec<Value> = match req.get("holidays") {
        Some(Value::Array(a)) => a.clone(),
        Some(_) => return Err(ApiError::bad("holidays is a list")),
        None => vec![req.clone()],
    };
    if list.len() > 500 {
        return Err(ApiError::bad("at most 500 holidays at once"));
    }
    let mut tx = state.db.begin().await?;
    for h in &list {
        let day =
            h["day"].as_str().and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok()).ok_or_else(|| ApiError::bad("day is a date like 2026-12-25"))?;
        let name = h["name"]
            .as_str()
            .map(str::trim)
            .filter(|n| !n.is_empty() && n.chars().count() <= 100)
            .ok_or_else(|| ApiError::bad("a holiday has a name (up to 100 characters)"))?;
        sqlx::query(
            "INSERT INTO holidays (id, workspace_id, region, day, name) VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT (workspace_id, region, day) DO UPDATE SET name = EXCLUDED.name",
        )
        .bind(new_id())
        .bind(user.workspace_id)
        .bind(region(&h["region"])?)
        .bind(day)
        .bind(name)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    audit(&state, user.workspace_id, Some(user.id), "holidays.added", None, json!({"count": list.len()})).await;
    Ok((StatusCode::CREATED, Json(json!({"added": list.len()}))))
}

pub async fn delete_holiday(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    admin(&user)?;
    let n = sqlx::query("DELETE FROM holidays WHERE id = $1 AND workspace_id = $2").bind(id).bind(user.workspace_id).execute(&state.db).await?.rows_affected();
    if n == 0 {
        return Err(ApiError::NotFound("holiday"));
    }
    Ok(StatusCode::NO_CONTENT)
}

/// `PUT /me/holiday-region` `{region}`: whose public holidays apply to the caller ('' or null:
/// the workspace's default calendar). Admins set it for others with `userId`.
pub async fn set_region(State(state): State<AppState>, CurrentUser(user): CurrentUser, Json(req): Json<Value>) -> ApiResult<StatusCode> {
    let who: Uuid = match req["userId"].as_str() {
        Some(u) if !u.is_empty() => {
            admin(&user)?;
            u.parse().map_err(|_| ApiError::bad("userId is a person's id"))?
        }
        _ => user.id,
    };
    let r = region(&req["region"])?;
    sqlx::query("UPDATE users SET holiday_region = NULLIF($2, '') WHERE id = $1 AND workspace_id = $3")
        .bind(who)
        .bind(&r)
        .bind(user.workspace_id)
        .execute(&state.db)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---- planning ----

/// `GET /sprints/{id}/capacity`: each person's days in the sprint — the people with issues in it
/// and the project's members — and the team's share of its working days that's available.
pub async fn sprint_capacity(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(id): Path<Uuid>) -> ApiResult<Json<Value>> {
    let row: Option<(Uuid, Option<chrono::DateTime<chrono::Utc>>, Option<chrono::DateTime<chrono::Utc>>)> =
        sqlx::query_as("SELECT project_id, start_at, end_at FROM sprints WHERE id = $1").bind(id).fetch_optional(&state.db).await?;
    let (project, start, end) = row.ok_or(ApiError::NotFound("sprint"))?;
    crate::routes::projects::by_key(&state, &user, &project.to_string(), Role::Viewer).await?;
    let (Some(start), Some(end)) = (start, end) else {
        return Ok(Json(json!({"start": null, "end": null, "people": [], "workingDays": 0, "available": 0, "share": null})));
    };
    let people: Vec<Uuid> = sqlx::query_scalar(
        "SELECT assignee_id FROM issues WHERE sprint_id = $1 AND assignee_id IS NOT NULL AND deleted_at IS NULL
         UNION SELECT m.subject_id FROM project_members m WHERE m.project_id = $2 AND m.subject_type = 'user'
         UNION SELECT tm.user_id FROM project_members m JOIN team_members tm ON tm.team_id = m.subject_id WHERE m.project_id = $2 AND m.subject_type = 'team'",
    )
    .bind(id)
    .bind(project)
    .fetch_all(&state.db)
    .await?;
    let list = availability(&state, user.workspace_id, &people, start.date_naive(), end.date_naive()).await?;
    let work: f64 = list.iter().map(|a| a.working_days - a.holidays).sum();
    let avail: f64 = list.iter().map(|a| a.available).sum();
    Ok(Json(json!({
        "start": start.date_naive(), "end": end.date_naive(), "people": list,
        "workingDays": work, "available": avail, "share": if work > 0.0 { Some(avail / work) } else { None },
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    #[test]
    fn working_days_holidays_and_absences() {
        // Mon 2026-12-21 to Sun 2027-01-03: 10 working days; Christmas and Boxing Day are
        // Friday and Saturday; away the 28th to the 30th and half of the 31st
        let (work, hol, away) = count(
            d("2026-12-21"),
            d("2027-01-03"),
            &[d("2026-12-25"), d("2026-12-26")],
            &[(d("2026-12-28"), d("2026-12-30"), None), (d("2026-12-31"), d("2026-12-31"), Some("pm".into()))],
        );
        assert_eq!((work, hol, away), (10.0, 1.0, 3.5));
        // an absence over a holiday doesn't count the holiday twice
        let (_, hol, away) = count(d("2026-12-24"), d("2026-12-25"), &[d("2026-12-25")], &[(d("2026-12-24"), d("2026-12-25"), None)]);
        assert_eq!((hol, away), (1.0, 1.0));
    }
}
