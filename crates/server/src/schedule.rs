//! Schedules for automation rules and recurring issues, and the background loop that runs them
//! (scheduled rules, recurring issues, service-level alerts). Every replica runs the loop; due
//! rows are claimed with `FOR UPDATE SKIP LOCKED`.
//!
//! A schedule: `{"every": "hour" | "day" | "weekday" | "week" | "month", "at": "08:00",
//! "weekday": 1..7 (Monday = 1, for week), "day": 1..28 (for month), "tz": "Europe/Zurich"}`.
//! Times are local to `tz` (UTC when missing), so "weekdays at 08:00" stays 08:00 across
//! daylight-saving changes.

use chrono::{DateTime, Datelike, Duration, NaiveDate, NaiveTime, TimeZone, Utc, Weekday};
use chrono_tz::Tz;
use serde_json::{Map, Value, json};
use sqlx::QueryBuilder;
use uuid::Uuid;

use crate::app::AppState;
use crate::issues::{Actor, NewIssue};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Every {
    Hour,
    Day,
    Weekday,
    Week,
    Month,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Schedule {
    pub every: Every,
    pub at: NaiveTime,
    /// Monday = 1 … Sunday = 7
    pub weekday: u32,
    /// 1 … 28
    pub day: u32,
    pub tz: Tz,
}

pub fn parse(v: &Value) -> Result<Schedule, String> {
    let every = match v["every"].as_str() {
        Some("hour") => Every::Hour,
        Some("day") => Every::Day,
        Some("weekday") => Every::Weekday,
        Some("week") => Every::Week,
        Some("month") => Every::Month,
        _ => return Err("schedule.every must be hour, day, weekday, week or month".into()),
    };
    let at = NaiveTime::parse_from_str(v["at"].as_str().unwrap_or("08:00"), "%H:%M").map_err(|_| "schedule.at looks like 08:30".to_string())?;
    let weekday = v["weekday"].as_u64().unwrap_or(1) as u32;
    let day = v["day"].as_u64().unwrap_or(1) as u32;
    if !(1..=7).contains(&weekday) {
        return Err("schedule.weekday is 1 (Monday) to 7 (Sunday)".into());
    }
    if !(1..=28).contains(&day) {
        return Err("schedule.day is 1 to 28".into());
    }
    let tz = match v["tz"].as_str().filter(|s| !s.is_empty()) {
        Some(name) => name.parse::<Tz>().map_err(|_| format!("unknown time zone {name}"))?,
        None => Tz::UTC,
    };
    Ok(Schedule { every, at, weekday, day, tz })
}

/// A local date and time in `tz` as UTC (a time skipped by a daylight-saving change moves on
/// by an hour).
fn at(tz: Tz, d: NaiveDate, t: NaiveTime) -> DateTime<Utc> {
    let local = d.and_time(t);
    match tz.from_local_datetime(&local).earliest() {
        Some(x) => x.with_timezone(&Utc),
        None => {
            tz.from_local_datetime(&(local + Duration::hours(1))).earliest().map(|x| x.with_timezone(&Utc)).unwrap_or_else(|| Utc.from_utc_datetime(&local))
        }
    }
}

/// The first run strictly after `t`.
pub fn next_after(s: &Schedule, t: DateTime<Utc>) -> DateTime<Utc> {
    let today = t.with_timezone(&s.tz).date_naive();
    match s.every {
        Every::Hour => {
            let minute = chrono::Timelike::minute(&s.at);
            let base = t.date_naive().and_hms_opt(chrono::Timelike::hour(&t), minute, 0).map(|d| Utc.from_utc_datetime(&d)).unwrap_or(t);
            if base > t { base } else { base + Duration::hours(1) }
        }
        Every::Day | Every::Weekday | Every::Week => {
            for i in 0..15 {
                let d = today + Duration::days(i);
                let ok = match s.every {
                    Every::Weekday => !matches!(d.weekday(), Weekday::Sat | Weekday::Sun),
                    Every::Week => d.weekday().number_from_monday() == s.weekday,
                    _ => true,
                };
                let when = at(s.tz, d, s.at);
                if ok && when > t {
                    return when;
                }
            }
            t + Duration::days(1)
        }
        Every::Month => {
            let (mut y, mut m) = (today.year(), today.month());
            for _ in 0..3 {
                if let Some(d) = NaiveDate::from_ymd_opt(y, m, s.day) {
                    let when = at(s.tz, d, s.at);
                    if when > t {
                        return when;
                    }
                }
                if m == 12 {
                    y += 1;
                    m = 1;
                } else {
                    m += 1;
                }
            }
            t + Duration::days(30)
        }
    }
}

/// Fills `{date}`, `{week}` and `{month}` in a template title.
pub fn fill_title(title: &str, now: DateTime<Utc>) -> String {
    let iso = now.iso_week();
    title
        .replace("{date}", &now.format("%Y-%m-%d").to_string())
        .replace("{week}", &format!("{}-W{:02}", iso.year(), iso.week()))
        .replace("{month}", &now.format("%Y-%m").to_string())
}

/// Starts the loop (once a minute).
pub fn spawn(state: &AppState) {
    let state = state.clone();
    tokio::spawn(async move {
        loop {
            tick(&state).await;
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
        }
    });
}

pub async fn tick(state: &AppState) {
    if let Err(e) = run_rules(state).await {
        tracing::warn!(error = %e, "scheduled automation");
    }
    if let Err(e) = run_templates(state).await {
        tracing::warn!(error = %e, "recurring issues");
    }
    if let Err(e) = crate::sla::alert(state).await {
        tracing::warn!(error = %e, "service-level alerts");
    }
    if let Err(e) = run_subscriptions(state).await {
        tracing::warn!(error = %e, "filter subscriptions");
    }
    if let Err(e) = crate::privacy::purge(state).await {
        tracing::warn!(error = %e, "retention");
    }
    if let Err(e) = crate::extract::index_pending(state).await {
        tracing::warn!(error = %e, "reading files for search");
    }
}

/// Filter subscriptions: a saved filter's results e-mailed to its subscriber, run as them (they
/// see what they'd see in the app).
pub async fn run_subscriptions(state: &AppState) -> anyhow::Result<usize> {
    let mut sent = 0;
    if !state.mailer.enabled() {
        return Ok(0);
    }
    for _ in 0..50 {
        let mut tx = state.db.begin().await?;
        type Due = (Uuid, Uuid, Value, bool, String, String, Option<String>);
        let due: Option<Due> = sqlx::query_as(
            "SELECT s.view_id, s.user_id, s.schedule, s.skip_empty, v.name, v.query, p.key FROM view_subscriptions s
               JOIN views v ON v.id = s.view_id LEFT JOIN projects p ON p.id = v.project_id
              WHERE s.next_run_at <= now() ORDER BY s.next_run_at LIMIT 1 FOR UPDATE OF s SKIP LOCKED",
        )
        .fetch_optional(&mut *tx)
        .await?;
        let Some((view, user_id, schedule, skip_empty, name, query, project)) = due else { break };
        let next = parse(&schedule).map(|s| next_after(&s, Utc::now())).ok();
        sqlx::query("UPDATE view_subscriptions SET next_run_at = $3, last_run_at = now() WHERE view_id = $1 AND user_id = $2")
            .bind(view)
            .bind(user_id)
            .bind(next)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        let user: Option<crate::auth::User> =
            sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT {} FROM users WHERE id = $1 AND status = 'active'", crate::auth::USER_COLUMNS)))
                .bind(user_id)
                .fetch_optional(&state.db)
                .await?;
        let Some(user) = user.filter(|u| u.email.as_deref().is_some_and(|e| !e.is_empty())) else { continue };
        let found = match crate::routes::issues::run_search(state, &user, &query, project.as_deref(), 50, 0, "i.updated_at DESC").await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(view = %view, error = %e, "filter subscription query failed");
                continue;
            }
        };
        if found.total == 0 && skip_empty {
            continue;
        }
        let l = crate::i18n::of_user(&state.db, user.id).await;
        let base = state.config.base().to_string();
        let (subject, text, html) = crate::mail::subscription_mail(l, &base, &user.name, &name, view, found.total, &found.issues);
        let mail = crate::mail::Mail { to: user.email.clone().unwrap_or_default(), subject, text, html, purpose: "subscription" };
        if crate::mail::queue(state, &mail).await? {
            sent += 1;
        }
    }
    Ok(sent)
}

/// Scheduled rules: every issue of the project matching the rule's condition gets the actions.
async fn run_rules(state: &AppState) -> anyhow::Result<()> {
    for _ in 0..50 {
        let mut tx = state.db.begin().await?;
        let due: Option<(Uuid, Uuid, Uuid, String, String, Value, Value)> = sqlx::query_as(
            "SELECT r.id, r.project_id, p.workspace_id, r.name, r.condition, r.trigger, r.actions FROM automation_rules r JOIN projects p ON p.id = r.project_id
              WHERE r.enabled AND r.next_run_at <= now() AND p.archived_at IS NULL ORDER BY r.next_run_at LIMIT 1 FOR UPDATE OF r SKIP LOCKED",
        )
        .fetch_optional(&mut *tx)
        .await?;
        let Some((id, project, workspace, name, condition, trigger, actions)) = due else { return Ok(()) };
        let next = parse(&trigger["schedule"]).map(|s| next_after(&s, Utc::now())).ok();
        sqlx::query("UPDATE automation_rules SET next_run_at = $2, last_run_at = now() WHERE id = $1").bind(id).bind(next).execute(&mut *tx).await?;
        tx.commit().await?;
        if !crate::features::on(state, workspace, "automation").await {
            continue;
        }
        let ids = matching(state, workspace, project, &condition, 200).await.unwrap_or_default();
        let bot = Actor { user: None, workspace, depth: 1 };
        let mut ran = 0;
        for issue in ids {
            let Ok(row) = crate::issues::load(state, issue).await else { continue };
            let s = crate::issues::summary(state, row);
            match crate::automation::execute(state, &bot, &s, &actions).await {
                Ok(()) => ran += 1,
                Err(e) => tracing::warn!(rule = %name, issue = %s.key, error = %e, "scheduled rule failed"),
            }
        }
        if ran > 0 {
            sqlx::query("UPDATE automation_rules SET runs = runs + $2 WHERE id = $1").bind(id).bind(ran).execute(&state.db).await?;
        }
    }
    Ok(())
}

/// Issues of a project matching TQL (all open ones when empty).
pub async fn matching(state: &AppState, workspace: Uuid, project: Uuid, condition: &str, limit: i64) -> crate::error::ApiResult<Vec<Uuid>> {
    let q = crate::query::parse(if condition.trim().is_empty() { "category != done" } else { condition })?;
    let ctx = crate::query::context_with(state, workspace, Uuid::nil(), vec![project], &q).await?;
    let mut qb: QueryBuilder<sqlx::Postgres> = QueryBuilder::new(
        "SELECT i.id FROM issues i JOIN projects p ON p.id = i.project_id JOIN statuses s ON s.id = i.status_id JOIN issue_types it ON it.id = i.type_id WHERE",
    );
    crate::query::push_where(&mut qb, &ctx, &q)?;
    qb.push(" ORDER BY i.rank LIMIT ");
    qb.push_bind(limit);
    Ok(qb.build_query_scalar().fetch_all(&state.db).await?)
}

/// Recurring issues: templates with a schedule create an issue when due.
async fn run_templates(state: &AppState) -> anyhow::Result<()> {
    for _ in 0..50 {
        let mut tx = state.db.begin().await?;
        let due: Option<(Uuid, Value, Option<Uuid>, Uuid)> = sqlx::query_as(
            "SELECT t.id, t.schedule, t.created_by, p.workspace_id FROM issue_templates t JOIN projects p ON p.id = t.project_id
              WHERE t.next_run_at <= now() AND p.archived_at IS NULL ORDER BY t.next_run_at LIMIT 1 FOR UPDATE OF t SKIP LOCKED",
        )
        .fetch_optional(&mut *tx)
        .await?;
        let Some((id, schedule, creator, workspace)) = due else { return Ok(()) };
        let next = parse(&schedule).map(|s| next_after(&s, Utc::now())).ok();
        sqlx::query("UPDATE issue_templates SET next_run_at = $2, last_run_at = now() WHERE id = $1").bind(id).bind(next).execute(&mut *tx).await?;
        tx.commit().await?;
        // created in the name of the template's author (reporter), like they'd have done it
        let actor = Actor { user: creator, workspace, depth: 1 };
        match instantiate(state, &actor, id, Map::new()).await {
            Ok(s) => {
                sqlx::query("UPDATE issue_templates SET last_issue_id = $2 WHERE id = $1").bind(id).bind(s.id).execute(&state.db).await?;
            }
            Err(e) => tracing::warn!(template = %id, error = %e, "recurring issue failed"),
        }
    }
    Ok(())
}

/// Creates an issue from a template; `overrides` (fields, title, description) win over it.
pub async fn instantiate(
    state: &AppState,
    actor: &Actor,
    template: Uuid,
    mut overrides: Map<String, Value>,
) -> crate::error::ApiResult<crate::issues::Summary> {
    let (project, title, description, fields, subtasks): (Uuid, String, Option<Value>, Value, Value) =
        sqlx::query_as("SELECT project_id, title, description, fields, subtasks FROM issue_templates WHERE id = $1")
            .bind(template)
            .fetch_one(&state.db)
            .await?;
    let now = Utc::now();
    let mut f: Map<String, Value> = fields.as_object().cloned().unwrap_or_default();
    if let Some(days) = f.remove("dueInDays").and_then(|v| v.as_i64()) {
        f.insert("dueDate".into(), json!((now + Duration::days(days.clamp(0, 3650))).date_naive()));
    }
    if f.get("sprint").and_then(|v| v.as_str()) == Some("active") {
        f.remove("sprint");
        let active: Option<Uuid> = sqlx::query_scalar("SELECT id FROM sprints WHERE project_id = $1 AND state = 'active' ORDER BY start_at LIMIT 1")
            .bind(project)
            .fetch_optional(&state.db)
            .await?;
        if let Some(a) = active {
            f.insert("sprintId".into(), json!(a));
        }
    }
    f.remove("sprint");
    let title = overrides.remove("title").and_then(|v| v.as_str().map(str::to_string)).unwrap_or_else(|| fill_title(&title, now));
    let description = match overrides.remove("description") {
        Some(d) if !d.is_null() => Some(d),
        _ => description,
    };
    for (k, v) in overrides {
        f.insert(k, v);
    }
    let s = crate::issues::create(state, actor, NewIssue { project, title, description, fields: f }).await?;
    // sub-tasks with the project's sub-task type
    let sub_type: Option<Uuid> = sqlx::query_scalar("SELECT id FROM issue_types WHERE project_id = $1 AND level < 0 ORDER BY position LIMIT 1")
        .bind(project)
        .fetch_optional(&state.db)
        .await?;
    if let Some(st) = sub_type
        && s.kind.level >= 0
    {
        for t in subtasks.as_array().into_iter().flatten().filter_map(|t| t.as_str()).filter(|t| !t.trim().is_empty()).take(30) {
            let mut sf = Map::new();
            sf.insert("typeId".into(), json!(st));
            sf.insert("parentId".into(), json!(s.id));
            if let Some(sp) = &s.sprint {
                sf.insert("sprintId".into(), json!(sp.id));
            }
            crate::issues::create(state, actor, NewIssue { project, title: fill_title(t, now), description: None, fields: sf }).await?;
        }
    }
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn next_runs() {
        // 2026-11-12 is a Thursday
        let now = t("2026-11-12T10:30:00Z");
        let day = parse(&json!({"every": "day", "at": "08:00"})).unwrap();
        assert_eq!(next_after(&day, now), t("2026-11-13T08:00:00Z"));
        let late = parse(&json!({"every": "day", "at": "11:00"})).unwrap();
        assert_eq!(next_after(&late, now), t("2026-11-12T11:00:00Z"));
        let weekday = parse(&json!({"every": "weekday", "at": "08:00"})).unwrap();
        assert_eq!(next_after(&weekday, t("2026-11-13T09:00:00Z")), t("2026-11-16T08:00:00Z"), "Friday after 8 → Monday");
        let monday = parse(&json!({"every": "week", "at": "09:15", "weekday": 1})).unwrap();
        assert_eq!(next_after(&monday, now), t("2026-11-16T09:15:00Z"));
        let thursday = parse(&json!({"every": "week", "at": "12:00", "weekday": 4})).unwrap();
        assert_eq!(next_after(&thursday, now), t("2026-11-12T12:00:00Z"));
        let monthly = parse(&json!({"every": "month", "at": "07:00", "day": 1})).unwrap();
        assert_eq!(next_after(&monthly, now), t("2026-12-01T07:00:00Z"));
        assert_eq!(next_after(&monthly, t("2026-12-15T00:00:00Z")), t("2027-01-01T07:00:00Z"));
        let hourly = parse(&json!({"every": "hour", "at": "00:45"})).unwrap();
        assert_eq!(next_after(&hourly, now), t("2026-11-12T10:45:00Z"));
        assert_eq!(next_after(&hourly, t("2026-11-12T10:50:00Z")), t("2026-11-12T11:45:00Z"));
        assert!(parse(&json!({"every": "fortnight"})).is_err());
        // local times: 08:00 in Zurich is 07:00 UTC in winter
        let zurich = parse(&json!({"every": "weekday", "at": "08:00", "tz": "Europe/Zurich"})).unwrap();
        assert_eq!(next_after(&zurich, now), t("2026-11-13T07:00:00Z"));
        // 00:30 local on Mondays in Zurich is Sunday 23:30 UTC
        let early = parse(&json!({"every": "week", "at": "00:30", "weekday": 1, "tz": "Europe/Zurich"})).unwrap();
        assert_eq!(next_after(&early, now), t("2026-11-15T23:30:00Z"));
        assert!(parse(&json!({"every": "day", "tz": "Mars/Base"})).is_err());
        assert!(parse(&json!({"every": "month", "day": 31})).is_err());
    }

    #[test]
    fn titles() {
        let now = t("2026-11-12T10:30:00Z");
        assert_eq!(fill_title("Weekly report {week} ({date})", now), "Weekly report 2026-W46 (2026-11-12)");
        assert_eq!(fill_title("Invoices {month}", now), "Invoices 2026-11");
    }
}
