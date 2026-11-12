//! Service-level goals (mostly for trackers): time to first response and time to resolution,
//! in hours per priority, set in the project's settings:
//! `"sla": {"response": {"urgent": 1, "high": 4, "medium": 8, "low": 24}, "resolution": {…}}`.
//!
//! The response clock stops at the first move out of a "to do" status or the first comment by
//! someone other than the reporter; the resolution clock when the issue is done. Times are
//! calendar hours.

use std::collections::HashMap;

use chrono::{DateTime, Duration, Utc};
use serde::Serialize;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::app::AppState;
use crate::error::ApiResult;
use crate::query::priority_name;

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Goal {
    /// response | resolution
    pub goal: &'static str,
    pub target_minutes: i64,
    pub elapsed_minutes: i64,
    pub due_at: DateTime<Utc>,
    /// running | atRisk | breached | met
    pub state: &'static str,
    /// the clock stopped (responded / resolved)
    pub stopped: bool,
}

/// Hours for a goal and priority from the project settings (None = no goal).
fn target_hours(settings: &Value, goal: &str, priority: i16) -> Option<f64> {
    settings["sla"][goal][priority_name(priority)].as_f64().filter(|h| *h > 0.0)
}

pub fn evaluate(goal: &'static str, hours: f64, start: DateTime<Utc>, stop: Option<DateTime<Utc>>, now: DateTime<Utc>) -> Goal {
    let target = (hours * 60.0).round() as i64;
    let end = stop.unwrap_or(now);
    let elapsed = (end - start).num_minutes().max(0);
    let state = match (stop.is_some(), elapsed > target) {
        (_, true) => "breached",
        (true, false) => "met",
        (false, false) if target - elapsed <= target / 4 => "atRisk",
        _ => "running",
    };
    Goal { goal, target_minutes: target, elapsed_minutes: elapsed, due_at: start + Duration::minutes(target), state, stopped: stop.is_some() }
}

#[derive(sqlx::FromRow)]
struct Clock {
    id: Uuid,
    created_at: DateTime<Utc>,
    priority: i16,
    resolved_at: Option<DateTime<Utc>>,
    responded_at: Option<DateTime<Utc>>,
    settings: Value,
}

/// Goals of the given issues (issues of projects without goals are left out).
pub async fn for_issues(state: &AppState, ids: &[Uuid]) -> ApiResult<HashMap<Uuid, Vec<Goal>>> {
    let rows: Vec<Clock> = sqlx::query_as(
        "SELECT i.id, i.created_at, i.priority, i.resolved_at, p.settings,
                LEAST(
                  (SELECT min(e.created_at) FROM issue_events e WHERE e.issue_id = i.id AND e.field = 'status' AND e.new_value->>'category' <> 'todo'),
                  (SELECT min(c.created_at) FROM comments c WHERE c.issue_id = i.id AND c.author_id IS NOT NULL AND c.author_id IS DISTINCT FROM i.reporter_id)
                ) AS responded_at
           FROM issues i JOIN projects p ON p.id = i.project_id
          WHERE i.id = ANY($1) AND p.settings ? 'sla'",
    )
    .bind(ids)
    .fetch_all(&state.db)
    .await?;
    let now = Utc::now();
    let mut out = HashMap::new();
    for r in rows {
        let mut goals = Vec::new();
        if let Some(h) = target_hours(&r.settings, "response", r.priority) {
            // resolving also counts as a response
            goals.push(evaluate("response", h, r.created_at, r.responded_at.or(r.resolved_at), now));
        }
        if let Some(h) = target_hours(&r.settings, "resolution", r.priority) {
            goals.push(evaluate("resolution", h, r.created_at, r.resolved_at, now));
        }
        if !goals.is_empty() {
            out.insert(r.id, goals);
        }
    }
    Ok(out)
}

/// Tells the assignee (or the project lead) once when a goal is missed.
pub async fn alert(state: &AppState) -> anyhow::Result<()> {
    let open: Vec<(Uuid, Uuid, Option<Uuid>, Option<Uuid>)> = sqlx::query_as(
        "SELECT i.id, p.workspace_id, i.assignee_id, p.lead_id FROM issues i JOIN projects p ON p.id = i.project_id JOIN statuses s ON s.id = i.status_id
          WHERE p.settings ? 'sla' AND p.archived_at IS NULL AND i.deleted_at IS NULL AND s.category <> 'done' LIMIT 5000",
    )
    .fetch_all(&state.db)
    .await?;
    if open.is_empty() {
        return Ok(());
    }
    let ids: Vec<Uuid> = open.iter().map(|o| o.0).collect();
    let goals = for_issues(state, &ids).await?;
    for (id, workspace, assignee, lead) in open {
        for g in goals.get(&id).into_iter().flatten().filter(|g| g.state == "breached" && !g.stopped) {
            let fresh =
                sqlx::query("INSERT INTO sla_alerts (issue_id, goal) VALUES ($1, $2) ON CONFLICT DO NOTHING").bind(id).bind(g.goal).execute(&state.db).await?;
            if fresh.rows_affected() == 0 {
                continue;
            }
            let Some(who) = assignee.or(lead) else { continue };
            let s = crate::issues::summary(state, crate::issues::load(state, id).await?);
            crate::notify::notify(state, workspace, who, &s, None, "sla", json!({"goal": g.goal, "targetMinutes": g.target_minutes})).await;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn states() {
        let t0 = DateTime::parse_from_rfc3339("2026-11-12T08:00:00Z").unwrap().with_timezone(&Utc);
        let h = |n: i64| t0 + Duration::minutes(n);
        assert_eq!(evaluate("response", 4.0, t0, None, h(60)).state, "running");
        assert_eq!(evaluate("response", 4.0, t0, None, h(200)).state, "atRisk");
        assert_eq!(evaluate("response", 4.0, t0, None, h(241)).state, "breached");
        assert_eq!(evaluate("response", 4.0, t0, Some(h(30)), h(1000)).state, "met");
        let late = evaluate("resolution", 1.0, t0, Some(h(90)), h(1000));
        assert_eq!((late.state, late.elapsed_minutes, late.stopped), ("breached", 90, true));
        assert_eq!(late.due_at, h(60));
        let s = json!({"sla": {"response": {"urgent": 1, "low": 0}}});
        assert_eq!(target_hours(&s, "response", 4), Some(1.0));
        assert_eq!(target_hours(&s, "response", 1), None, "0 = no goal");
        assert_eq!(target_hours(&s, "resolution", 4), None);
    }
}
