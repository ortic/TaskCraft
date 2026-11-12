//! Reports computed by replaying issue history (`issue_events`): sprint burndown, velocity,
//! cumulative flow, cycle and lead time, created vs resolved, workload, and a project overview.

use std::collections::{BTreeMap, HashMap};

use chrono::{DateTime, Duration, NaiveDate, Utc};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::app::AppState;
use crate::error::{ApiError, ApiResult};

#[derive(sqlx::FromRow, Clone)]
struct Ev {
    issue_id: Uuid,
    kind: String,
    field: Option<String>,
    new_value: Option<Value>,
    created_at: DateTime<Utc>,
}

#[derive(Default, Clone, Debug)]
struct IssueState {
    known: bool,
    sprint: Option<String>,
    done: bool,
    points: Option<f64>,
    status: Option<String>,
    category: Option<String>,
}

fn apply_event(st: &mut IssueState, e: &Ev) {
    let v = e.new_value.clone().unwrap_or(Value::Null);
    match (e.kind.as_str(), e.field.as_deref()) {
        ("created", _) => {
            st.known = true;
            st.sprint = v["sprint"]["id"].as_str().map(str::to_string);
            st.category = v["status"]["category"].as_str().map(str::to_string);
            st.status = v["status"]["id"].as_str().map(str::to_string);
            st.done = st.category.as_deref() == Some("done");
            st.points = v["points"].as_f64();
        }
        ("field", Some("status")) => {
            st.category = v["category"].as_str().map(str::to_string);
            st.status = v["id"].as_str().map(str::to_string);
            st.done = st.category.as_deref() == Some("done");
        }
        ("field", Some("sprint")) => st.sprint = v["id"].as_str().map(str::to_string),
        ("field", Some("points")) => st.points = v.as_f64(),
        _ => {}
    }
}

#[derive(sqlx::FromRow)]
struct SprintRow {
    id: Uuid,
    project_id: Uuid,
    name: String,
    state: String,
    goal: Option<String>,
    start_at: Option<DateTime<Utc>>,
    end_at: Option<DateTime<Utc>>,
    completed_at: Option<DateTime<Utc>>,
}

async fn sprint(state: &AppState, id: Uuid) -> ApiResult<SprintRow> {
    sqlx::query_as("SELECT id, project_id, name, state, goal, start_at, end_at, completed_at FROM sprints WHERE id = $1")
        .bind(id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(ApiError::NotFound("sprint"))
}

pub async fn sprint_project(state: &AppState, id: Uuid) -> ApiResult<Uuid> {
    Ok(sprint(state, id).await?.project_id)
}

/// The history events of the issues that were ever in a sprint.
async fn sprint_events(state: &AppState, sprint: Uuid, project: Uuid) -> ApiResult<Vec<Ev>> {
    let sid = sprint.to_string();
    Ok(sqlx::query_as(
        "WITH ids AS (
            SELECT id FROM issues WHERE sprint_id = $1 AND deleted_at IS NULL
            UNION SELECT issue_id FROM issue_events WHERE project_id = $3 AND (
                (field = 'sprint' AND (old_value->>'id' = $2 OR new_value->>'id' = $2)) OR (kind = 'created' AND new_value->'sprint'->>'id' = $2))
         )
         SELECT e.issue_id, e.kind, e.field, e.new_value, e.created_at FROM issue_events e JOIN ids ON ids.id = e.issue_id
          JOIN issues i ON i.id = e.issue_id AND i.deleted_at IS NULL
          WHERE e.kind = 'created' OR e.field IN ('status', 'sprint', 'points')
          ORDER BY e.created_at, e.id",
    )
    .bind(sprint)
    .bind(&sid)
    .bind(project)
    .fetch_all(&state.db)
    .await?)
}

struct Replay {
    committed_points: f64,
    committed_issues: i64,
    remaining_end: f64,
    completed_points: f64,
    completed_issues: i64,
    added: i64,
    removed: i64,
    steps: Vec<Value>,
    use_points: bool,
}

fn replay(events: &[Ev], sprint_id: Uuid, t0: DateTime<Utc>, t1: DateTime<Utc>, keys: &HashMap<Uuid, String>) -> Replay {
    let sid = sprint_id.to_string();
    let mut states: HashMap<Uuid, IssueState> = HashMap::new();
    let use_points = events.iter().any(|e| {
        (e.kind == "created" && e.new_value.as_ref().is_some_and(|v| v["points"].as_f64().is_some()))
            || (e.field.as_deref() == Some("points") && e.new_value.as_ref().is_some_and(|v| v.as_f64().is_some()))
    });
    let weight = |s: &IssueState| if use_points { s.points.unwrap_or(0.0) } else { 1.0 };
    let remaining = |states: &HashMap<Uuid, IssueState>| -> f64 { states.values().filter(|s| s.sprint.as_deref() == Some(&sid) && !s.done).map(weight).sum() };
    let mut i = 0;
    while i < events.len() && events[i].created_at <= t0 {
        apply_event(states.entry(events[i].issue_id).or_default(), &events[i]);
        i += 1;
    }
    let in_sprint = |states: &HashMap<Uuid, IssueState>| -> Vec<Uuid> { states.iter().filter(|(_, s)| s.sprint.as_deref() == Some(&sid)).map(|(k, _)| *k).collect() };
    let start_members = in_sprint(&states);
    let committed_points: f64 = start_members.iter().filter_map(|id| states.get(id)).map(weight).sum();
    let committed_issues = start_members.len() as i64;
    let mut steps = vec![json!({"at": t0, "remaining": remaining(&states), "event": "start"})];
    let mut added = 0;
    let mut removed = 0;
    while i < events.len() && events[i].created_at <= t1 {
        let e = &events[i];
        let before_in = states.get(&e.issue_id).is_some_and(|s| s.sprint.as_deref() == Some(&sid));
        let before = remaining(&states);
        apply_event(states.entry(e.issue_id).or_default(), e);
        let after_in = states.get(&e.issue_id).is_some_and(|s| s.sprint.as_deref() == Some(&sid));
        if !before_in && after_in {
            added += 1;
        }
        if before_in && !after_in {
            removed += 1;
        }
        let after = remaining(&states);
        if (after - before).abs() > f64::EPSILON {
            let what = match (before_in, after_in, e.field.as_deref()) {
                (false, true, _) => "added",
                (true, false, _) => "removed",
                (_, _, Some("status")) if after < before => "completed",
                (_, _, Some("status")) => "reopened",
                (_, _, Some("points")) => "estimate",
                _ => "changed",
            };
            steps.push(json!({"at": e.created_at, "remaining": after, "event": what, "key": keys.get(&e.issue_id)}));
        }
        i += 1;
    }
    let end_members = in_sprint(&states);
    let done: Vec<&IssueState> = end_members.iter().filter_map(|id| states.get(id)).filter(|s| s.done).collect();
    Replay {
        committed_points,
        committed_issues,
        remaining_end: remaining(&states),
        completed_points: done.iter().map(|s| weight(s)).sum(),
        completed_issues: done.len() as i64,
        added,
        removed,
        steps,
        use_points,
    }
}

async fn keys(state: &AppState, project: Uuid) -> ApiResult<HashMap<Uuid, String>> {
    let rows: Vec<(Uuid, String)> =
        sqlx::query_as("SELECT i.id, p.key || '-' || i.number FROM issues i JOIN projects p ON p.id = i.project_id WHERE i.project_id = $1").bind(project).fetch_all(&state.db).await?;
    Ok(rows.into_iter().collect())
}

/// Sprint burndown: remaining work over the sprint, with the ideal line and scope changes.
pub async fn burndown(state: &AppState, sprint_id: Uuid) -> ApiResult<Value> {
    let s = sprint(state, sprint_id).await?;
    let now = Utc::now();
    let t0 = s.start_at.unwrap_or(now);
    let t_end = s.completed_at.or(s.end_at).unwrap_or(t0 + Duration::days(14));
    let t1 = s.completed_at.unwrap_or(now).min(t_end.max(now));
    let events = sprint_events(state, sprint_id, s.project_id).await?;
    let r = replay(&events, sprint_id, t0, t1, &keys(state, s.project_id).await?);
    Ok(json!({
        "sprint": {"id": s.id, "name": s.name, "state": s.state, "goal": s.goal, "startAt": s.start_at, "endAt": s.end_at, "completedAt": s.completed_at},
        "unit": if r.use_points { "points" } else { "issues" },
        "committed": r.committed_points,
        "committedIssues": r.committed_issues,
        "remaining": r.remaining_end,
        "completed": r.completed_points,
        "completedIssues": r.completed_issues,
        "added": r.added,
        "removed": r.removed,
        "start": t0,
        "end": t_end,
        "now": now,
        "steps": r.steps,
    }))
}

/// Committed vs completed per closed sprint (the latest `count`).
pub async fn velocity(state: &AppState, project: Uuid, count: i64) -> ApiResult<Value> {
    let sprints: Vec<SprintRow> = sqlx::query_as(
        "SELECT id, project_id, name, state, goal, start_at, end_at, completed_at FROM sprints WHERE project_id = $1 AND state = 'closed'
          ORDER BY completed_at DESC NULLS LAST LIMIT $2",
    )
    .bind(project)
    .bind(count.clamp(1, 24))
    .fetch_all(&state.db)
    .await?;
    let keys = keys(state, project).await?;
    let mut out = Vec::new();
    let mut any_points = false;
    for s in sprints.into_iter().rev() {
        let events = sprint_events(state, s.id, project).await?;
        let t0 = s.start_at.unwrap_or_else(Utc::now);
        let t1 = s.completed_at.unwrap_or_else(Utc::now);
        let r = replay(&events, s.id, t0, t1, &keys);
        any_points |= r.use_points;
        out.push(json!({
            "id": s.id, "name": s.name, "startAt": s.start_at, "completedAt": s.completed_at,
            "committed": r.committed_points, "completed": r.completed_points,
            "committedIssues": r.committed_issues, "completedIssues": r.completed_issues, "unit": if r.use_points { "points" } else { "issues" }
        }));
    }
    let completed: Vec<f64> = out.iter().filter_map(|s| s["completed"].as_f64()).collect();
    let avg = if completed.is_empty() { 0.0 } else { completed.iter().sum::<f64>() / completed.len() as f64 };
    Ok(json!({"sprints": out, "average": avg, "unit": if any_points { "points" } else { "issues" }}))
}

/// Figures kept on the sprint when it closes (shown in sprint history).
pub async fn sprint_stats(state: &AppState, sprint_id: Uuid) -> ApiResult<Value> {
    let b = burndown(state, sprint_id).await?;
    Ok(json!({
        "unit": b["unit"], "committed": b["committed"], "completed": b["completed"],
        "committedIssues": b["committedIssues"], "completedIssues": b["completedIssues"], "added": b["added"], "removed": b["removed"]
    }))
}

async fn project_events(state: &AppState, project: Uuid, fields: &[&str]) -> ApiResult<Vec<Ev>> {
    let fields: Vec<String> = fields.iter().map(|s| s.to_string()).collect();
    Ok(sqlx::query_as(
        "SELECT e.issue_id, e.kind, e.field, e.new_value, e.created_at FROM issue_events e JOIN issues i ON i.id = e.issue_id AND i.deleted_at IS NULL
          WHERE e.project_id = $1 AND (e.kind = 'created' OR e.field = ANY($2)) ORDER BY e.created_at, e.id",
    )
    .bind(project)
    .bind(&fields)
    .fetch_all(&state.db)
    .await?)
}

fn day_end(d: NaiveDate) -> DateTime<Utc> {
    DateTime::from_naive_utc_and_offset(d.and_hms_opt(23, 59, 59).unwrap_or_default(), Utc)
}

/// Cumulative flow: issues per status at the end of each day.
pub async fn cumulative_flow(state: &AppState, project: Uuid, days: i64) -> ApiResult<Value> {
    let days = days.clamp(7, 365);
    let statuses: Vec<(Uuid, String, String, Option<String>)> =
        sqlx::query_as("SELECT id, name, category, color FROM statuses WHERE project_id = $1 ORDER BY position").bind(project).fetch_all(&state.db).await?;
    let events = project_events(state, project, &["status"]).await?;
    let today = Utc::now().date_naive();
    let first = today - Duration::days(days - 1);
    let mut states: HashMap<Uuid, IssueState> = HashMap::new();
    let mut i = 0;
    let mut series = Vec::new();
    let mut d = first;
    while d <= today {
        let end = day_end(d);
        while i < events.len() && events[i].created_at <= end {
            apply_event(states.entry(events[i].issue_id).or_default(), &events[i]);
            i += 1;
        }
        let mut counts: BTreeMap<String, i64> = BTreeMap::new();
        for s in states.values() {
            if let Some(st) = &s.status {
                *counts.entry(st.clone()).or_default() += 1;
            }
        }
        series.push(json!({"date": d, "counts": counts}));
        d += Duration::days(1);
    }
    Ok(json!({
        "statuses": statuses.iter().map(|(id, name, cat, color)| json!({"id": id, "name": name, "category": cat, "color": color})).collect::<Vec<_>>(),
        "days": series,
    }))
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let idx = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

/// Cycle time (first "in progress" → done) and lead time (created → done) of issues finished
/// in the last `days` days.
pub async fn cycle_time(state: &AppState, project: Uuid, days: i64) -> ApiResult<Value> {
    let since = Utc::now() - Duration::days(days.clamp(7, 730));
    let done: Vec<(Uuid, String, String, DateTime<Utc>, DateTime<Utc>, String)> = sqlx::query_as(
        "SELECT i.id, p.key || '-' || i.number, i.title, i.created_at, i.resolved_at, it.name FROM issues i JOIN projects p ON p.id = i.project_id
           JOIN issue_types it ON it.id = i.type_id
          WHERE i.project_id = $1 AND i.deleted_at IS NULL AND i.resolved_at >= $2 ORDER BY i.resolved_at",
    )
    .bind(project)
    .bind(since)
    .fetch_all(&state.db)
    .await?;
    let ids: Vec<Uuid> = done.iter().map(|d| d.0).collect();
    let started: Vec<(Uuid, DateTime<Utc>)> = sqlx::query_as(
        "SELECT issue_id, min(created_at) FROM issue_events WHERE issue_id = ANY($1) AND field = 'status' AND new_value->>'category' IN ('in_progress', 'done')
          GROUP BY issue_id",
    )
    .bind(&ids)
    .fetch_all(&state.db)
    .await?;
    let started: HashMap<Uuid, DateTime<Utc>> = started.into_iter().collect();
    let to_days = |d: Duration| (d.num_minutes() as f64 / 1440.0 * 10.0).round() / 10.0;
    let mut points = Vec::new();
    let mut cycles = Vec::new();
    let mut leads = Vec::new();
    for (id, key, title, created, resolved, kind) in done {
        let start = started.get(&id).copied().unwrap_or(created).min(resolved);
        let cycle = to_days(resolved - start);
        let lead = to_days(resolved - created);
        cycles.push(cycle);
        leads.push(lead);
        points.push(json!({"key": key, "title": title, "type": kind, "resolvedAt": resolved, "cycleDays": cycle, "leadDays": lead}));
    }
    cycles.sort_by(|a, b| a.total_cmp(b));
    leads.sort_by(|a, b| a.total_cmp(b));
    let avg = |v: &[f64]| if v.is_empty() { 0.0 } else { (v.iter().sum::<f64>() / v.len() as f64 * 10.0).round() / 10.0 };
    Ok(json!({
        "issues": points,
        "cycle": {"average": avg(&cycles), "median": percentile(&cycles, 0.5), "p85": percentile(&cycles, 0.85)},
        "lead": {"average": avg(&leads), "median": percentile(&leads, 0.5), "p85": percentile(&leads, 0.85)},
    }))
}

/// Issues created and resolved per day.
pub async fn created_resolved(state: &AppState, project: Uuid, days: i64) -> ApiResult<Value> {
    let days = days.clamp(7, 365);
    let rows: Vec<(NaiveDate, i64, i64)> = sqlx::query_as(
        "WITH d AS (SELECT generate_series(current_date - ($2::int - 1), current_date, interval '1 day')::date AS day)
         SELECT d.day,
                (SELECT count(*) FROM issues i WHERE i.project_id = $1 AND i.deleted_at IS NULL AND i.created_at::date = d.day),
                (SELECT count(*) FROM issues i WHERE i.project_id = $1 AND i.deleted_at IS NULL AND i.resolved_at::date = d.day)
           FROM d ORDER BY d.day",
    )
    .bind(project)
    .bind(days as i32)
    .fetch_all(&state.db)
    .await?;
    Ok(json!({"days": rows.iter().map(|(d, c, r)| json!({"date": d, "created": c, "resolved": r})).collect::<Vec<_>>()}))
}

/// Open work per person.
pub async fn workload(state: &AppState, project: Uuid) -> ApiResult<Value> {
    let rows: Vec<(Option<Uuid>, Option<String>, i64, i64, f64, i64)> = sqlx::query_as(
        "SELECT i.assignee_id, u.name, count(*) FILTER (WHERE s.category = 'todo'), count(*) FILTER (WHERE s.category = 'in_progress'),
                COALESCE(sum(i.story_points), 0)::float8, count(*) FILTER (WHERE i.due_date < current_date)
           FROM issues i JOIN statuses s ON s.id = i.status_id LEFT JOIN users u ON u.id = i.assignee_id
          WHERE i.project_id = $1 AND i.deleted_at IS NULL AND s.category <> 'done'
          GROUP BY i.assignee_id, u.name ORDER BY count(*) DESC",
    )
    .bind(project)
    .fetch_all(&state.db)
    .await?;
    Ok(json!({"people": rows.iter().map(|(id, name, todo, doing, pts, overdue)| json!({
        "id": id, "name": name, "todo": todo, "inProgress": doing, "points": pts, "overdue": overdue
    })).collect::<Vec<_>>()}))
}

/// Counts for the project overview.
pub async fn overview(state: &AppState, project: Uuid) -> ApiResult<Value> {
    let by_cat: Vec<(String, i64)> = sqlx::query_as(
        "SELECT s.category, count(*) FROM issues i JOIN statuses s ON s.id = i.status_id WHERE i.project_id = $1 AND i.deleted_at IS NULL GROUP BY s.category",
    )
    .bind(project)
    .fetch_all(&state.db)
    .await?;
    let by_status: Vec<(Uuid, String, String, Option<String>, i64)> = sqlx::query_as(
        "SELECT s.id, s.name, s.category, s.color, count(i.id) FROM statuses s LEFT JOIN issues i ON i.status_id = s.id AND i.deleted_at IS NULL
          WHERE s.project_id = $1 GROUP BY s.id ORDER BY s.position",
    )
    .bind(project)
    .fetch_all(&state.db)
    .await?;
    let by_type: Vec<(String, String, String, i64)> = sqlx::query_as(
        "SELECT it.name, it.icon, it.color, count(i.id) FROM issue_types it LEFT JOIN issues i ON i.type_id = it.id AND i.deleted_at IS NULL
           AND i.status_id IN (SELECT id FROM statuses WHERE category <> 'done')
          WHERE it.project_id = $1 GROUP BY it.id ORDER BY it.position",
    )
    .bind(project)
    .fetch_all(&state.db)
    .await?;
    let by_priority: Vec<(i16, i64)> = sqlx::query_as(
        "SELECT i.priority, count(*) FROM issues i JOIN statuses s ON s.id = i.status_id WHERE i.project_id = $1 AND i.deleted_at IS NULL AND s.category <> 'done'
          GROUP BY i.priority ORDER BY i.priority DESC",
    )
    .bind(project)
    .fetch_all(&state.db)
    .await?;
    let (overdue, unassigned, created7, resolved7, stale): (i64, i64, i64, i64, i64) = sqlx::query_as(
        "SELECT count(*) FILTER (WHERE s.category <> 'done' AND i.due_date < current_date),
                count(*) FILTER (WHERE s.category <> 'done' AND i.assignee_id IS NULL),
                count(*) FILTER (WHERE i.created_at > now() - interval '7 days'),
                count(*) FILTER (WHERE i.resolved_at > now() - interval '7 days'),
                count(*) FILTER (WHERE s.category = 'in_progress' AND i.status_changed_at < now() - interval '7 days')
           FROM issues i JOIN statuses s ON s.id = i.status_id WHERE i.project_id = $1 AND i.deleted_at IS NULL",
    )
    .bind(project)
    .fetch_one(&state.db)
    .await?;
    Ok(json!({
        "categories": by_cat.into_iter().collect::<HashMap<_, _>>(),
        "statuses": by_status.iter().map(|(id, n, c, col, k)| json!({"id": id, "name": n, "category": c, "color": col, "count": k})).collect::<Vec<_>>(),
        "types": by_type.iter().map(|(n, i, c, k)| json!({"name": n, "icon": i, "color": c, "count": k})).collect::<Vec<_>>(),
        "priorities": by_priority.iter().map(|(p, k)| json!({"priority": crate::query::priority_name(*p), "count": k})).collect::<Vec<_>>(),
        "overdue": overdue, "unassigned": unassigned, "createdLastWeek": created7, "resolvedLastWeek": resolved7, "stale": stale,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(issue: Uuid, kind: &str, field: Option<&str>, v: Value, h: i64) -> Ev {
        Ev {
            issue_id: issue,
            kind: kind.into(),
            field: field.map(str::to_string),
            new_value: Some(v),
            created_at: DateTime::from_timestamp(1_700_000_000 + h * 3600, 0).unwrap(),
        }
    }

    #[test]
    fn burndown_replay() {
        let sprint = Uuid::now_v7();
        let (a, b, c) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
        let sid = sprint.to_string();
        let events = vec![
            ev(a, "created", None, json!({"status": {"category": "todo"}, "sprint": {"id": sid}, "points": 3.0}), 0),
            ev(b, "created", None, json!({"status": {"category": "todo"}, "sprint": null, "points": 5.0}), 0),
            ev(b, "field", Some("sprint"), json!({"id": sid}), 1),
            // sprint starts at hour 2
            ev(a, "field", Some("status"), json!({"category": "done"}), 5),
            ev(c, "created", None, json!({"status": {"category": "todo"}, "sprint": {"id": sid}, "points": 2.0}), 6),
            ev(b, "field", Some("points"), json!(8.0), 7),
        ];
        let t = |h: i64| DateTime::from_timestamp(1_700_000_000 + h * 3600, 0).unwrap();
        let r = replay(&events, sprint, t(2), t(10), &HashMap::new());
        assert!(r.use_points);
        assert_eq!(r.committed_points, 8.0);
        assert_eq!(r.committed_issues, 2);
        assert_eq!(r.remaining_end, 10.0); // b (8) + c (2)
        assert_eq!(r.completed_points, 3.0);
        assert_eq!(r.added, 1);
        let remaining: Vec<f64> = r.steps.iter().map(|s| s["remaining"].as_f64().unwrap()).collect();
        assert_eq!(remaining, vec![8.0, 5.0, 7.0, 10.0]);
    }

    #[test]
    fn percentiles() {
        let v = [1.0, 2.0, 3.0, 4.0, 10.0];
        assert_eq!(percentile(&v, 0.5), 3.0);
        assert_eq!(percentile(&v, 0.85), 4.0);
        assert_eq!(percentile(&[], 0.5), 0.0);
    }
}
