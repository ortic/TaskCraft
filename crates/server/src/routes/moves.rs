//! Moving issues to another project: a new key (the old one keeps working), the type and status
//! mapped by name or category, custom fields kept where the target has a field of the same name
//! and kind, project-specific fields (sprint, release, component) cleared; sub-tasks move along.

use std::collections::HashMap;

use axum::Json;
use axum::extract::{Path, Query, State};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use super::issues::issue_for;
use super::projects::by_key;
use crate::access::Role;
use crate::app::AppState;
use crate::auth::{CurrentUser, User};
use crate::error::{ApiError, ApiResult};
use crate::events::{self, Event};
use crate::issues::{self, Change, Summary};

#[derive(sqlx::FromRow, Clone)]
struct Field {
    id: Uuid,
    name: String,
    kind: String,
    options: Value,
}

async fn fields(tx: &mut Transaction<'_, Postgres>, project: Uuid) -> ApiResult<Vec<Field>> {
    Ok(sqlx::query_as("SELECT id, name, kind, options FROM custom_fields WHERE project_id = $1").bind(project).fetch_all(&mut **tx).await?)
}

fn option_label(f: &Field, id: &str) -> Option<String> {
    f.options.as_array()?.iter().find(|o| o["id"] == id).and_then(|o| o["label"].as_str()).map(str::to_lowercase)
}

fn option_id(f: &Field, label: &str) -> Option<String> {
    f.options.as_array()?.iter().find(|o| o["label"].as_str().is_some_and(|l| l.to_lowercase() == label)).and_then(|o| o["id"].as_str()).map(str::to_string)
}

/// Custom field values carried over (by field name and kind); returns them and the names of
/// fields whose values are lost.
fn map_custom(custom: &Value, from: &[Field], to: &[Field]) -> (Value, Vec<String>) {
    let mut out = Map::new();
    let mut dropped = Vec::new();
    for (fid, v) in custom.as_object().into_iter().flatten() {
        let Some(src) = from.iter().find(|f| f.id.to_string() == *fid) else { continue };
        let Some(dst) = to.iter().find(|f| f.name.to_lowercase() == src.name.to_lowercase() && f.kind == src.kind) else {
            dropped.push(src.name.clone());
            continue;
        };
        let mapped = match src.kind.as_str() {
            "select" => v.as_str().and_then(|o| option_label(src, o)).and_then(|l| option_id(dst, &l)).map(Value::String),
            "multiselect" => {
                let ids: Vec<Value> = v
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|o| o.as_str().and_then(|o| option_label(src, o)).and_then(|l| option_id(dst, &l)).map(Value::String))
                    .collect();
                (!ids.is_empty()).then_some(Value::Array(ids))
            }
            _ => Some(v.clone()),
        };
        match mapped {
            Some(m) => {
                out.insert(dst.id.to_string(), m);
            }
            None => dropped.push(src.name.clone()),
        }
    }
    (Value::Object(out), dropped)
}

/// The target type: the given one, else one of the same name, else one of the same level.
async fn target_type(tx: &mut Transaction<'_, Postgres>, target: Uuid, wanted: Option<Uuid>, name: &str, level: i16) -> ApiResult<Uuid> {
    if let Some(w) = wanted {
        let ok: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM issue_types WHERE id = $1 AND project_id = $2)").bind(w).bind(target).fetch_one(&mut **tx).await?;
        if !ok {
            return Err(ApiError::bad("that type isn't in the target project"));
        }
        return Ok(w);
    }
    let found: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM issue_types WHERE project_id = $1 ORDER BY lower(name) = lower($2) DESC, level = $3 DESC, level = 0 DESC, position LIMIT 1",
    )
    .bind(target)
    .bind(name)
    .bind(level)
    .fetch_optional(&mut **tx)
    .await?;
    found.ok_or_else(|| ApiError::bad("the target project has no issue types"))
}

/// The target status: the given one, else one of the same name, else the first of the same category.
async fn target_status(tx: &mut Transaction<'_, Postgres>, target: Uuid, wanted: Option<Uuid>, name: &str, category: &str) -> ApiResult<(Uuid, String)> {
    let row: Option<(Uuid, String)> = match wanted {
        Some(w) => sqlx::query_as("SELECT id, category FROM statuses WHERE id = $1 AND project_id = $2").bind(w).bind(target).fetch_optional(&mut **tx).await?,
        None => {
            sqlx::query_as(
                "SELECT id, category FROM statuses WHERE project_id = $1 ORDER BY lower(name) = lower($2) DESC, category = $3 DESC, position LIMIT 1",
            )
            .bind(target)
            .bind(name)
            .bind(category)
            .fetch_optional(&mut **tx)
            .await?
        }
    };
    row.ok_or_else(|| ApiError::bad("that status isn't in the target project"))
}

#[derive(sqlx::FromRow)]
struct Moving {
    project_id: Uuid,
    project_key: String,
    number: i32,
    type_name: String,
    type_level: i16,
    status_id: Uuid,
    status_name: String,
    status_category: String,
    custom: Value,
}

async fn moving(tx: &mut Transaction<'_, Postgres>, id: Uuid) -> ApiResult<Moving> {
    Ok(sqlx::query_as(
        "SELECT i.project_id, p.key AS project_key, i.number, it.name AS type_name, it.level AS type_level, s.id AS status_id, s.name AS status_name,
                s.category AS status_category, i.custom
           FROM issues i JOIN projects p ON p.id = i.project_id JOIN issue_types it ON it.id = i.type_id JOIN statuses s ON s.id = i.status_id
          WHERE i.id = $1 FOR UPDATE OF i",
    )
    .bind(id)
    .fetch_one(&mut **tx)
    .await?)
}

/// The security level an issue gets in the target project: the one of the same name, else the
/// target's default. A private issue doesn't turn public by moving: without either, it stays put.
async fn target_level(tx: &mut Transaction<'_, Postgres>, id: Uuid, target: Uuid) -> ApiResult<Option<Uuid>> {
    let (to, from): (Option<Uuid>, Option<String>) = sqlx::query_as(
        "SELECT COALESCE((SELECT t.id FROM security_levels t WHERE t.project_id = $2 AND lower(t.name) = lower(s.name)),
                         (SELECT d.id FROM security_levels d WHERE d.project_id = $2 AND d.is_default LIMIT 1)), s.name
           FROM issues i LEFT JOIN security_levels s ON s.id = i.security_level_id WHERE i.id = $1",
    )
    .bind(id)
    .bind(target)
    .fetch_one(&mut **tx)
    .await?;
    match (to, from) {
        (None, Some(name)) => Err(ApiError::bad(format!(
            "the issue is private ({name}) and the target project has no security level of that name or a default one: add one there first"
        ))),
        (to, _) => Ok(to),
    }
}

/// Moves one issue (and, recursively, its sub-tasks) inside a transaction; returns the new keys.
async fn move_one(
    tx: &mut Transaction<'_, Postgres>,
    actor: &User,
    id: Uuid,
    target: Uuid,
    target_key: &str,
    type_override: Option<Uuid>,
    status_override: Option<Uuid>,
    depth: u8,
) -> ApiResult<Vec<(Uuid, String, Uuid)>> {
    let m = moving(tx, id).await?;
    if m.project_id == target {
        return Err(ApiError::bad("the issue is already in that project"));
    }
    let seq: i32 =
        sqlx::query_scalar("UPDATE projects SET issue_seq = issue_seq + 1 WHERE id = $1 RETURNING issue_seq").bind(target).fetch_one(&mut **tx).await?;
    let type_id = target_type(tx, target, type_override, &m.type_name, m.type_level).await?;
    let (status_id, category) = target_status(tx, target, status_override, &m.status_name, &m.status_category).await?;
    let (custom, _) = map_custom(&m.custom, &fields(tx, m.project_id).await?, &fields(tx, target).await?);
    let rank = issues::bottom_rank(tx, target).await?;
    let level = target_level(tx, id, target).await?;
    sqlx::query(
        "UPDATE issues SET project_id = $2, number = $3, type_id = $4, status_id = $5, custom = $6, rank = $7, sprint_id = NULL,
                updated_at = now(), security_level_id = $9,
                status_changed_at = CASE WHEN status_id <> $5 THEN now() ELSE status_changed_at END,
                resolved_at = CASE WHEN $8 = 'done' THEN COALESCE(resolved_at, now()) ELSE NULL END
          WHERE id = $1",
    )
    .bind(id)
    .bind(target)
    .bind(seq)
    .bind(type_id)
    .bind(status_id)
    .bind(&custom)
    .bind(rank)
    .bind(&category)
    .bind(level)
    .execute(&mut **tx)
    .await?;
    // releases and components belong to the project it leaves
    sqlx::query("DELETE FROM issue_versions WHERE issue_id = $1").bind(id).execute(&mut **tx).await?;
    sqlx::query("DELETE FROM issue_components WHERE issue_id = $1").bind(id).execute(&mut **tx).await?;
    sqlx::query("INSERT INTO issue_key_aliases (workspace_id, project_key, number, issue_id) VALUES ($1, $2, $3, $4) ON CONFLICT DO NOTHING")
        .bind(actor.workspace_id)
        .bind(&m.project_key)
        .bind(m.number)
        .bind(id)
        .execute(&mut **tx)
        .await?;
    // the history moves with the issue (reports of the new project include its past)
    sqlx::query("UPDATE issue_events SET project_id = $2 WHERE issue_id = $1").bind(id).bind(target).execute(&mut **tx).await?;
    let old_key = format!("{}-{}", m.project_key, m.number);
    let new_key = format!("{target_key}-{seq}");
    let c = Change { field: "project", old: json!({"key": old_key, "id": m.project_id}), new: json!({"key": new_key, "id": target}) };
    issues::record(tx, id, target, Some(actor.id), "moved", Some(&c)).await?;
    if m.status_id != status_id {
        let name: String = sqlx::query_scalar("SELECT name FROM statuses WHERE id = $1").bind(status_id).fetch_one(&mut **tx).await?;
        let sc = Change {
            field: "status",
            old: json!({"id": m.status_id, "name": m.status_name, "category": m.status_category}),
            new: json!({"id": status_id, "name": name, "category": category}),
        };
        issues::record(tx, id, target, Some(actor.id), "field", Some(&sc)).await?;
    }
    let mut moved = vec![(id, new_key, m.project_id)];
    if depth < 3 {
        let subtasks: Vec<Uuid> = sqlx::query_scalar(
            "SELECT i.id FROM issues i JOIN issue_types it ON it.id = i.type_id WHERE i.parent_id = $1 AND it.level < 0 AND i.project_id = $2 AND i.deleted_at IS NULL",
        )
        .bind(id)
        .bind(m.project_id)
        .fetch_all(&mut **tx)
        .await?;
        for sub in subtasks {
            moved.extend(Box::pin(move_one(tx, actor, sub, target, target_key, None, None, depth + 1)).await?);
        }
    }
    Ok(moved)
}

#[derive(Deserialize)]
pub struct PreviewQuery {
    to: String,
}

/// What a move would do: mapped type and status, fields that would be lost, sub-tasks.
pub async fn preview(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Path(key): Path<String>,
    Query(q): Query<PreviewQuery>,
) -> ApiResult<Json<Value>> {
    let (id, source, role) = issue_for(&state, &user, &key, Role::Member).await?;
    crate::access::require_action(&state, source, role, "moveIssues").await?;
    let (target, target_role) = by_key(&state, &user, &q.to, Role::Member).await?;
    crate::access::require_action(&state, target.id, target_role, "moveIssues").await?;
    let mut tx = state.db.begin().await?;
    let m = moving(&mut tx, id).await?;
    let type_id = target_type(&mut tx, target.id, None, &m.type_name, m.type_level).await?;
    let (status_id, _) = target_status(&mut tx, target.id, None, &m.status_name, &m.status_category).await?;
    let (_, dropped) = map_custom(&m.custom, &fields(&mut tx, m.project_id).await?, &fields(&mut tx, target.id).await?);
    let level = target_level(&mut tx, id, target.id).await?;
    let level: Option<String> = match level {
        Some(l) => sqlx::query_scalar("SELECT name FROM security_levels WHERE id = $1").bind(l).fetch_optional(&mut *tx).await?,
        None => None,
    };
    let cleared: (bool, bool, bool) =
        sqlx::query_as(
            "SELECT sprint_id IS NOT NULL, EXISTS (SELECT 1 FROM issue_versions WHERE issue_id = i.id), EXISTS (SELECT 1 FROM issue_components WHERE issue_id = i.id)
               FROM issues i WHERE id = $1",
        )
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
    let subtasks: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM issues i JOIN issue_types it ON it.id = i.type_id WHERE i.parent_id = $1 AND it.level < 0 AND i.deleted_at IS NULL",
    )
    .bind(id)
    .fetch_one(&mut *tx)
    .await?;
    tx.rollback().await?;
    let mut lost: Vec<&str> = Vec::new();
    if cleared.0 {
        lost.push("sprint");
    }
    if cleared.1 {
        lost.push("release");
    }
    if cleared.2 {
        lost.push("component");
    }
    Ok(Json(json!({
        "target": {"key": target.key, "name": target.name},
        "typeId": type_id, "statusId": status_id,
        "droppedFields": dropped, "cleared": lost, "subtasks": subtasks, "securityLevel": level,
    })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MoveReq {
    project_key: String,
    type_id: Option<Uuid>,
    status_id: Option<Uuid>,
}

pub async fn move_to_project(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Path(key): Path<String>,
    Json(req): Json<MoveReq>,
) -> ApiResult<Json<Summary>> {
    let (id, source, role) = issue_for(&state, &user, &key, Role::Member).await?;
    crate::access::require_action(&state, source, role, "moveIssues").await?;
    let (target, target_role) = by_key(&state, &user, &req.project_key, Role::Member).await?;
    crate::access::require_action(&state, target.id, target_role, "moveIssues").await?;
    if target.archived_at.is_some() {
        return Err(ApiError::bad("the target project is archived"));
    }
    let mut tx = state.db.begin().await?;
    let moved = move_one(&mut tx, &user, id, target.id, &target.key, req.type_id, req.status_id, 0).await?;
    tx.commit().await?;
    let s = issues::summary(&state, issues::load(&state, id).await?);
    let mut sources: HashMap<Uuid, ()> = HashMap::new();
    sources.insert(source, ());
    for (mid, mkey, from) in &moved {
        sources.insert(*from, ());
        events::publish(&state, Event::new(user.workspace_id, "issue").project(target.id).issue(*mid, mkey).actor(user.id)).await;
    }
    for p in sources.keys() {
        events::publish(&state, Event::new(user.workspace_id, "issue").project(*p).actor(user.id)).await;
    }
    crate::webhooks::enqueue(
        &state,
        user.workspace_id,
        Some(target.id),
        "issue.updated",
        json!({"issue": s, "changed": ["project"], "previousKey": key.to_uppercase()}),
    )
    .await;
    Ok(Json(s))
}

/// Changes a time log (the person who logged it or a project admin).
pub async fn update_worklog(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<Uuid>,
    Json(req): Json<Value>,
) -> ApiResult<Json<Value>> {
    let row: Option<(Uuid, Option<Uuid>)> = sqlx::query_as("SELECT issue_id, user_id FROM worklogs WHERE id = $1").bind(id).fetch_optional(&state.db).await?;
    let (issue, owner) = row.ok_or(ApiError::NotFound("work log"))?;
    let (_, project, role) = issue_for(&state, &user, &issue.to_string(), Role::Member).await?;
    if owner != Some(user.id) {
        crate::access::require_action(&state, project, role, "logTimeForOthers").await?;
    }
    let minutes = match (&req["minutes"], req["time"].as_str()) {
        (Value::Number(n), _) => {
            Some(n.as_f64().map(|m| m.round() as i32).filter(|m| *m > 0 && *m <= 24 * 60 * 7).ok_or_else(|| ApiError::bad("minutes out of range"))?)
        }
        (_, Some(t)) => {
            Some(super::comments::parse_duration(t).filter(|m| *m <= 24 * 60 * 7).ok_or_else(|| ApiError::bad("time spent looks like 1h 30m, 45m or 2d"))?)
        }
        _ => None,
    };
    if let Some(m) = minutes {
        sqlx::query("UPDATE worklogs SET minutes = $2 WHERE id = $1").bind(id).bind(m).execute(&state.db).await?;
    }
    if let Some(d) = req["date"].as_str() {
        let d = chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").map_err(|_| ApiError::bad("date must be YYYY-MM-DD"))?;
        sqlx::query("UPDATE worklogs SET work_date = $2 WHERE id = $1").bind(id).bind(d).execute(&state.db).await?;
    }
    if req.get("categoryId").is_some() {
        let c = super::time::category_of(&state, user.workspace_id, &req["categoryId"]).await?;
        sqlx::query("UPDATE worklogs SET category_id = $2 WHERE id = $1").bind(id).bind(c.map(|c| c.0)).execute(&state.db).await?;
    }
    if let Some(b) = req.get("billable") {
        let b = b.as_bool().ok_or_else(|| ApiError::bad("billable must be true or false"))?;
        sqlx::query("UPDATE worklogs SET billable = $2 WHERE id = $1").bind(id).bind(b).execute(&state.db).await?;
    }
    if req.get("note").is_some() {
        sqlx::query("UPDATE worklogs SET note = $2 WHERE id = $1")
            .bind(id)
            .bind(req["note"].as_str().map(str::trim).filter(|n| !n.is_empty()))
            .execute(&state.db)
            .await?;
    }
    sqlx::query("UPDATE issues SET spent_minutes = COALESCE((SELECT sum(minutes) FROM worklogs WHERE issue_id = $1), 0), updated_at = now() WHERE id = $1")
        .bind(issue)
        .execute(&state.db)
        .await?;
    events::publish(&state, Event::new(user.workspace_id, "issue").project(project).actor(user.id)).await;
    let (minutes, date, note, category, billable): (i32, chrono::NaiveDate, Option<String>, Option<Uuid>, bool) =
        sqlx::query_as("SELECT minutes, work_date, note, category_id, billable FROM worklogs WHERE id = $1").bind(id).fetch_one(&state.db).await?;
    Ok(Json(json!({"id": id, "minutes": minutes, "date": date, "note": note, "categoryId": category, "billable": billable})))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(name: &str, kind: &str, options: Value) -> Field {
        Field { id: Uuid::now_v7(), name: name.into(), kind: kind.into(), options }
    }

    #[test]
    fn custom_values_follow_by_name_and_option_label() {
        let src_sel = field("Severity", "select", json!([{"id": "a", "label": "Critical"}, {"id": "b", "label": "Minor"}]));
        let src_env = field("Environment", "text", json!([]));
        let src_budget = field("Budget", "number", json!([]));
        let dst_sel = field("severity", "select", json!([{"id": "x", "label": "minor"}, {"id": "y", "label": "Critical"}]));
        let dst_budget = field("Budget", "text", json!([]));
        let custom = json!({src_sel.id.to_string(): "a", src_env.id.to_string(): "prod", src_budget.id.to_string(): 5});
        let (out, mut dropped) = map_custom(&custom, &[src_sel, src_env, src_budget], &[dst_sel.clone(), dst_budget]);
        dropped.sort();
        assert_eq!(out, json!({dst_sel.id.to_string(): "y"}));
        assert_eq!(dropped, vec!["Budget", "Environment"], "other kind or no such field");
    }
}
