//! Timers: start one on an issue, pause, stop. A stopped timer becomes a **time draft** (only its
//! owner sees it; reports leave it out) that is booked as a time log — right away when stopping
//! with `book`, or later from the draft. The timer is kept on the server, so every tab and device
//! shows the same one; changes reach the person's browsers live.
//!
//! Forgotten timers: a reminder after the workspace's `timer.reminderHours` (default 8), and at
//! 24 hours the timer stops by itself into a draft.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use chrono::{DateTime, NaiveDate, Utc};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::access::Role;
use crate::app::{AppState, new_id};
use crate::auth::{CurrentUser, User};
use crate::error::{ApiError, ApiResult};
use crate::events::{self, Event};

/// The longest a timer runs (and a draft from it lasts).
pub const MAX_SECONDS: i64 = 24 * 3600;

/// What the workspace says about timers: (round up to minutes, remind after hours; 0: never).
pub fn settings(ws: &Value) -> (i32, i32) {
    let t = &ws["timer"];
    (t["rounding"].as_i64().unwrap_or(1).clamp(1, 60) as i32, t["reminderHours"].as_i64().unwrap_or(8).clamp(0, 24) as i32)
}

/// Checks `timer` in the workspace settings: `{rounding: 1|5|6|10|15|30, reminderHours: 0…23}`.
pub fn check_settings(v: &Value) -> ApiResult<Value> {
    let rounding = match &v["rounding"] {
        Value::Null => 1,
        r => r.as_i64().filter(|r| [1, 5, 6, 10, 15, 30].contains(r)).ok_or_else(|| ApiError::bad("timer rounding is 1, 5, 6, 10, 15 or 30 minutes"))?,
    };
    let reminder = match &v["reminderHours"] {
        Value::Null => 8,
        h => h.as_i64().filter(|h| (0..24).contains(h)).ok_or_else(|| ApiError::bad("the timer reminder is after 1 to 23 hours, or 0 for none"))?,
    };
    Ok(json!({"rounding": rounding, "reminderHours": reminder}))
}

/// Minutes to book for a timer's seconds: at least one, rounded up to `rounding`, at most 24 hours.
pub fn minutes(seconds: i64, rounding: i32) -> i32 {
    let m = ((seconds.clamp(0, MAX_SECONDS) + 59) / 60).max(1) as i32;
    let r = rounding.max(1);
    ((m + r - 1) / r * r).min((MAX_SECONDS / 60) as i32)
}

async fn changed(state: &AppState, user: &User) {
    let mut e = Event::new(user.workspace_id, "timer");
    e.user_id = Some(user.id);
    events::publish(state, e).await;
}

type TimerRow = (Uuid, DateTime<Utc>, Option<DateTime<Utc>>, i64, Option<String>);

async fn running(state: &AppState, user: Uuid) -> ApiResult<Option<TimerRow>> {
    Ok(sqlx::query_as(
        "SELECT issue_id, started_at, paused_at,
                GREATEST(EXTRACT(EPOCH FROM (COALESCE(paused_at, now()) - started_at))::bigint - paused_seconds, 0), note
           FROM timers WHERE user_id = $1",
    )
    .bind(user)
    .fetch_optional(&state.db)
    .await?)
}

/// `GET /me/timer`: the running timer (`timer: null` when there's none) and how many drafts wait.
pub async fn get(State(state): State<AppState>, CurrentUser(user): CurrentUser) -> ApiResult<Json<Value>> {
    let drafts: i64 = sqlx::query_scalar("SELECT count(*) FROM time_drafts WHERE user_id = $1").bind(user.id).fetch_one(&state.db).await?;
    let Some((issue, started, paused, elapsed, note)) = running(&state, user.id).await? else {
        return Ok(Json(json!({"timer": null, "drafts": drafts})));
    };
    let summary = crate::issues::load_many_for(&state, user.id, &[issue]).await?.pop();
    Ok(Json(json!({
        "timer": {"issue": summary, "startedAt": started, "pausedAt": paused, "elapsedSeconds": elapsed, "note": note},
        "drafts": drafts,
    })))
}

/// `POST /issues/{key}/timer` `{note?, replace?}`: starts a timer on an issue (project members).
/// A timer on another issue is answered with 409, unless `replace`: then it stops into a draft.
pub async fn start(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Path(key): Path<String>,
    Json(req): Json<Value>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    crate::features::require(&state, user.workspace_id, "timeTracking").await?;
    let (id, _, _) = crate::routes::issues::issue_for(&state, &user, &key, Role::Member).await?;
    let mut draft = Value::Null;
    if let Some((other, ..)) = running(&state, user.id).await? {
        if other == id {
            return Err(ApiError::Conflict("a timer is already running on this issue".into()));
        }
        if req["replace"].as_bool() != Some(true) {
            let k = crate::issues::load_many_for(&state, user.id, &[other]).await?.pop().map(|s| s.key).unwrap_or_default();
            return Err(ApiError::Conflict(format!("a timer is running on {k}: stop it first, or start with replace to keep its time as a draft")));
        }
        draft = stop_into_draft(&state, &user).await?.map(|d| json!(d)).unwrap_or(Value::Null);
    }
    let note = req["note"].as_str().map(str::trim).filter(|n| !n.is_empty()).map(|n| n.chars().take(2000).collect::<String>());
    sqlx::query("INSERT INTO timers (user_id, issue_id, note) VALUES ($1, $2, $3) ON CONFLICT (user_id) DO NOTHING")
        .bind(user.id)
        .bind(id)
        .bind(note)
        .execute(&state.db)
        .await?;
    changed(&state, &user).await;
    Ok((StatusCode::CREATED, Json(json!({"started": key, "draft": draft}))))
}

/// `POST /me/timer/pause` and `/resume`.
pub async fn pause(State(state): State<AppState>, CurrentUser(user): CurrentUser) -> ApiResult<StatusCode> {
    let n = sqlx::query("UPDATE timers SET paused_at = now() WHERE user_id = $1 AND paused_at IS NULL").bind(user.id).execute(&state.db).await?.rows_affected();
    if n == 0 && running(&state, user.id).await?.is_none() {
        return Err(ApiError::NotFound("timer"));
    }
    changed(&state, &user).await;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn resume(State(state): State<AppState>, CurrentUser(user): CurrentUser) -> ApiResult<StatusCode> {
    let n = sqlx::query(
        "UPDATE timers SET paused_seconds = paused_seconds + EXTRACT(EPOCH FROM (now() - paused_at))::int, paused_at = NULL
          WHERE user_id = $1 AND paused_at IS NOT NULL",
    )
    .bind(user.id)
    .execute(&state.db)
    .await?
    .rows_affected();
    if n == 0 && running(&state, user.id).await?.is_none() {
        return Err(ApiError::NotFound("timer"));
    }
    changed(&state, &user).await;
    Ok(StatusCode::NO_CONTENT)
}

/// A time draft as the person sees it.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Draft {
    pub id: Uuid,
    pub key: String,
    pub title: String,
    pub minutes: i32,
    pub date: NaiveDate,
    pub started_at: Option<DateTime<Utc>>,
    pub note: Option<String>,
    pub created_at: DateTime<Utc>,
}

async fn draft(state: &AppState, id: Uuid) -> ApiResult<Option<Draft>> {
    type R = (Uuid, String, String, i32, NaiveDate, Option<DateTime<Utc>>, Option<String>, DateTime<Utc>);
    let r: Option<R> = sqlx::query_as(
        "SELECT d.id, p.key || '-' || i.number, i.title, d.minutes, d.work_date, d.started_at, d.note, d.created_at
           FROM time_drafts d JOIN issues i ON i.id = d.issue_id JOIN projects p ON p.id = i.project_id WHERE d.id = $1",
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await?;
    Ok(r.map(|(id, key, title, minutes, date, started_at, note, created_at)| Draft { id, key, title, minutes, date, started_at, note, created_at }))
}

/// Ends the running timer as a draft.
async fn stop_into_draft(state: &AppState, user: &User) -> ApiResult<Option<Draft>> {
    match stop_user(state, user.id).await? {
        Some(id) => draft(state, id).await,
        None => Ok(None),
    }
}

/// Stops a person's timer into a draft (one transaction, so a retried stop finds nothing to do):
/// the minutes as the workspace rounds them, on the day it started in the person's time zone.
/// Returns the draft's id (None: no timer ran).
pub async fn stop_user(state: &AppState, user: Uuid) -> ApiResult<Option<Uuid>> {
    let mut tx = state.db.begin().await?;
    type R = (Uuid, DateTime<Utc>, i64, Option<String>, Value, Option<String>);
    let row: Option<R> = sqlx::query_as(
        "WITH t AS (DELETE FROM timers WHERE user_id = $1 RETURNING *)
         SELECT t.issue_id, t.started_at, GREATEST(EXTRACT(EPOCH FROM (COALESCE(t.paused_at, now()) - t.started_at))::bigint - t.paused_seconds, 0),
                t.note, w.settings, COALESCE(NULLIF(u.timezone, ''), w.settings->>'timeZone')
           FROM t JOIN users u ON u.id = t.user_id JOIN workspaces w ON w.id = u.workspace_id",
    )
    .bind(user)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((issue, started, secs, note, ws, zone)) = row else {
        return Ok(None);
    };
    let tz: chrono_tz::Tz = zone.and_then(|z| z.parse().ok()).unwrap_or(chrono_tz::UTC);
    let id = new_id();
    sqlx::query("INSERT INTO time_drafts (id, user_id, issue_id, minutes, work_date, started_at, note) VALUES ($1, $2, $3, $4, $5, $6, $7)")
        .bind(id)
        .bind(user)
        .bind(issue)
        .bind(minutes(secs, settings(&ws).0))
        .bind(started.with_timezone(&tz).date_naive())
        .bind(started)
        .bind(note)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(Some(id))
}

/// `POST /me/timer/stop` `{book?, time?, note?, categoryId?, billable?, remaining?}`: stops the
/// timer into a draft and, with `book`, logs it right away (the time as timed unless `time`
/// says otherwise). A booking that fails (a missing category, say) leaves the draft.
pub async fn stop(State(state): State<AppState>, CurrentUser(user): CurrentUser, Json(req): Json<Value>) -> ApiResult<Json<Value>> {
    let Some(d) = stop_into_draft(&state, &user).await? else {
        return Err(ApiError::Conflict("no timer is running".into()));
    };
    changed(&state, &user).await;
    if req["book"].as_bool() != Some(true) {
        return Ok(Json(json!({"draft": d, "worklog": null})));
    }
    let mut body = req.clone();
    body["draftId"] = json!(d.id);
    if body["time"].as_str().is_none_or(|t| t.trim().is_empty()) && !body["minutes"].is_number() {
        body["minutes"] = json!(d.minutes);
    }
    if body["date"].is_null() {
        body["date"] = json!(d.date);
    }
    if body["note"].is_null() {
        body["note"] = json!(d.note);
    }
    let key = d.key.clone();
    let (_, Json(w)) = crate::routes::comments::log_work(State(state.clone()), CurrentUser(user), Path(key), Json(body)).await?;
    Ok(Json(json!({"draft": null, "worklog": w, "issue": d.key})))
}

/// `DELETE /me/timer`: stops without keeping the time.
pub async fn discard(State(state): State<AppState>, CurrentUser(user): CurrentUser) -> ApiResult<StatusCode> {
    let n = sqlx::query("DELETE FROM timers WHERE user_id = $1").bind(user.id).execute(&state.db).await?.rows_affected();
    if n == 0 {
        return Err(ApiError::NotFound("timer"));
    }
    changed(&state, &user).await;
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /me/time-drafts`: the caller's drafts, newest first.
pub async fn drafts(State(state): State<AppState>, CurrentUser(user): CurrentUser) -> ApiResult<Json<Value>> {
    let ids: Vec<Uuid> =
        sqlx::query_scalar("SELECT id FROM time_drafts WHERE user_id = $1 ORDER BY created_at DESC LIMIT 100").bind(user.id).fetch_all(&state.db).await?;
    let mut out = Vec::new();
    for id in ids {
        if let Some(d) = draft(&state, id).await? {
            out.push(d);
        }
    }
    Ok(Json(json!(out)))
}

/// `DELETE /me/time-drafts/{id}`: lets the time go.
pub async fn delete_draft(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    let n = sqlx::query("DELETE FROM time_drafts WHERE id = $1 AND user_id = $2").bind(id).bind(user.id).execute(&state.db).await?.rows_affected();
    if n == 0 {
        return Err(ApiError::NotFound("time draft"));
    }
    changed(&state, &user).await;
    Ok(StatusCode::NO_CONTENT)
}

/// A draft being booked (inside the time log's transaction): it must be the caller's, on this
/// issue, and still there — a second try of the same booking is refused instead of logged twice.
pub async fn take_draft(tx: &mut sqlx::PgConnection, user: Uuid, issue: Uuid, v: &Value) -> ApiResult<bool> {
    let Some(s) = v.as_str().filter(|s| !s.is_empty()) else {
        return Ok(false);
    };
    let id: Uuid = s.parse().map_err(|_| ApiError::bad("draftId must be a time draft's id"))?;
    let n = sqlx::query("DELETE FROM time_drafts WHERE id = $1 AND user_id = $2 AND issue_id = $3")
        .bind(id)
        .bind(user)
        .bind(issue)
        .execute(tx)
        .await?
        .rows_affected();
    if n == 0 {
        return Err(ApiError::Conflict("this time draft is booked or gone already".into()));
    }
    Ok(true)
}

/// Once a minute: reminders for timers that run long, and timers at 24 hours stop into drafts.
pub async fn housekeeping(state: &AppState) -> anyhow::Result<()> {
    type Long = (Uuid, Uuid, Uuid, i64);
    let long: Vec<Long> = sqlx::query_as(
        "SELECT t.user_id, u.workspace_id, t.issue_id, EXTRACT(EPOCH FROM (COALESCE(t.paused_at, now()) - t.started_at))::bigint - t.paused_seconds
           FROM timers t JOIN users u ON u.id = t.user_id JOIN workspaces w ON w.id = u.workspace_id
          WHERE t.reminded_at IS NULL AND COALESCE((w.settings->'timer'->>'reminderHours')::int, 8) > 0
            AND EXTRACT(EPOCH FROM (COALESCE(t.paused_at, now()) - t.started_at)) - t.paused_seconds >= COALESCE((w.settings->'timer'->>'reminderHours')::int, 8) * 3600
          LIMIT 200",
    )
    .fetch_all(&state.db)
    .await?;
    for (user, ws, issue, secs) in long {
        let n = sqlx::query("UPDATE timers SET reminded_at = now() WHERE user_id = $1 AND reminded_at IS NULL")
            .bind(user)
            .execute(&state.db)
            .await?
            .rows_affected();
        if n == 1
            && let Some(s) = crate::issues::load_many_for(state, user, &[issue]).await?.pop()
        {
            crate::notify::notify(state, ws, user, &s, None, "timer", json!({"hours": secs / 3600})).await;
        }
    }
    let full: Vec<(Uuid, Uuid, Uuid)> = sqlx::query_as(
        "SELECT t.user_id, u.workspace_id, t.issue_id FROM timers t JOIN users u ON u.id = t.user_id
          WHERE EXTRACT(EPOCH FROM (COALESCE(t.paused_at, now()) - t.started_at)) - t.paused_seconds >= $1 LIMIT 200",
    )
    .bind(MAX_SECONDS)
    .fetch_all(&state.db)
    .await?;
    for (user, ws, issue) in full {
        if stop_user(state, user).await.map_err(|e| anyhow::anyhow!("{e}"))?.is_some() {
            if let Some(s) = crate::issues::load_many_for(state, user, &[issue]).await?.pop() {
                crate::notify::notify(state, ws, user, &s, None, "timer", json!({"stopped": true})).await;
            }
            let mut e = Event::new(ws, "timer");
            e.user_id = Some(user);
            events::publish(state, e).await;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rounds_up_and_caps() {
        assert_eq!(minutes(0, 1), 1, "at least a minute");
        assert_eq!(minutes(61, 1), 2);
        assert_eq!(minutes(600, 15), 15);
        assert_eq!(minutes(16 * 60, 15), 30);
        assert_eq!(minutes(30 * 3600, 1), 24 * 60, "at most a day");
        assert_eq!(settings(&json!({})), (1, 8));
        assert_eq!(settings(&json!({"timer": {"rounding": 15, "reminderHours": 0}})), (15, 0));
        assert!(check_settings(&json!({"rounding": 7})).is_err());
        assert_eq!(check_settings(&json!({"rounding": 6, "reminderHours": 4})).unwrap(), json!({"rounding": 6, "reminderHours": 4}));
    }
}
