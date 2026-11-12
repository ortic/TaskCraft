//! Issue templates (and recurring issues: templates with a schedule) and the service-level
//! queue of a project.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use chrono::Utc;
use serde_json::{Map, Value, json};
use uuid::Uuid;

use super::projects::by_key;
use crate::access::{self, Role};
use crate::app::{AppState, new_id};
use crate::auth::{CurrentUser, User};
use crate::error::{ApiError, ApiResult};
use crate::issues::{Actor, Summary};
use crate::schedule;

type Row = (Uuid, String, String, Option<Value>, Value, Value, Option<Value>, Option<chrono::DateTime<Utc>>, Option<chrono::DateTime<Utc>>, Option<String>);

fn template_json(r: Row) -> Value {
    let (id, name, title, description, fields, subtasks, sched, next, last, last_key) = r;
    json!({"id": id, "name": name, "title": title, "description": description, "fields": fields, "subtasks": subtasks,
           "schedule": sched, "nextRunAt": next, "lastRunAt": last, "lastIssueKey": last_key})
}

const SELECT: &str = "SELECT t.id, t.name, t.title, t.description, t.fields, t.subtasks, t.schedule, t.next_run_at, t.last_run_at,
       (SELECT p.key || '-' || i.number FROM issues i JOIN projects p ON p.id = i.project_id WHERE i.id = t.last_issue_id)
  FROM issue_templates t";

pub async fn list(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(key): Path<String>) -> ApiResult<Json<Value>> {
    let (p, _) = by_key(&state, &user, &key, Role::Viewer).await?;
    let rows: Vec<Row> = sqlx::query_as(sqlx::AssertSqlSafe(format!("{SELECT} WHERE t.project_id = $1 ORDER BY t.position, lower(t.name)")))
        .bind(p.id)
        .fetch_all(&state.db)
        .await?;
    Ok(Json(json!(rows.into_iter().map(template_json).collect::<Vec<_>>())))
}

/// Checks and normalizes a template body.
fn clean(req: &Value) -> ApiResult<(String, String, Option<Value>, Value, Value, Option<Value>)> {
    let name = req["name"].as_str().map(str::trim).filter(|n| !n.is_empty()).ok_or_else(|| ApiError::bad("a template needs a name"))?.to_string();
    let title = req["title"].as_str().unwrap_or("").trim().chars().take(500).collect::<String>();
    let description = match &req["description"] {
        Value::String(md) if !md.trim().is_empty() => Some(crate::doc::from_markdown(md)),
        d @ Value::Object(_) => {
            crate::doc::validate(d).map_err(ApiError::BadRequest)?;
            Some(d.clone())
        }
        _ => None,
    };
    let mut fields = req["fields"].as_object().cloned().unwrap_or_default();
    fields.retain(|k, _| {
        matches!(
            k.as_str(),
            "typeId"
                | "priority"
                | "labels"
                | "assigneeId"
                | "storyPoints"
                | "estimateMinutes"
                | "parentId"
                | "componentId"
                | "versionId"
                | "dueInDays"
                | "sprint"
                | "custom"
        )
    });
    let subtasks: Vec<Value> = req["subtasks"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|s| s.as_str())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .take(30)
        .map(|s| json!(s))
        .collect();
    let schedule = match &req["schedule"] {
        Value::Null => None,
        s => {
            schedule::parse(s).map_err(ApiError::BadRequest)?;
            if title.is_empty() {
                return Err(ApiError::bad("a recurring template needs a title for the issues it creates"));
            }
            Some(s.clone())
        }
    };
    Ok((name, title, description, Value::Object(fields), Value::Array(subtasks), schedule))
}

fn next(schedule: &Option<Value>) -> Option<chrono::DateTime<Utc>> {
    schedule.as_ref().and_then(|s| schedule::parse(s).ok()).map(|s| schedule::next_after(&s, Utc::now()))
}

pub async fn create(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Path(key): Path<String>,
    Json(req): Json<Value>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let (p, _) = by_key(&state, &user, &key, Role::Admin).await?;
    let (name, title, description, fields, subtasks, sched) = clean(&req)?;
    let id = new_id();
    sqlx::query(
        "INSERT INTO issue_templates (id, project_id, name, title, description, fields, subtasks, schedule, next_run_at, created_by, position)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, (SELECT COALESCE(max(position), 0) + 1 FROM issue_templates WHERE project_id = $2))",
    )
    .bind(id)
    .bind(p.id)
    .bind(name)
    .bind(title)
    .bind(description)
    .bind(fields)
    .bind(subtasks)
    .bind(&sched)
    .bind(next(&sched))
    .bind(user.id)
    .execute(&state.db)
    .await?;
    Ok((StatusCode::CREATED, Json(json!({"id": id}))))
}

async fn template_project(state: &AppState, user: &User, id: Uuid, min: Role) -> ApiResult<Uuid> {
    let p: Option<Uuid> =
        sqlx::query_scalar("SELECT t.project_id FROM issue_templates t JOIN projects p ON p.id = t.project_id WHERE t.id = $1 AND p.workspace_id = $2")
            .bind(id)
            .bind(user.workspace_id)
            .fetch_optional(&state.db)
            .await?;
    let p = p.ok_or(ApiError::NotFound("template"))?;
    access::require(state, user, p, min).await?;
    Ok(p)
}

pub async fn update(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(id): Path<Uuid>, Json(req): Json<Value>) -> ApiResult<StatusCode> {
    template_project(&state, &user, id, Role::Admin).await?;
    let (name, title, description, fields, subtasks, sched) = clean(&req)?;
    sqlx::query(
        "UPDATE issue_templates SET name = $2, title = $3, description = $4, fields = $5, subtasks = $6, schedule = $7, next_run_at = $8 WHERE id = $1",
    )
    .bind(id)
    .bind(name)
    .bind(title)
    .bind(description)
    .bind(fields)
    .bind(subtasks)
    .bind(&sched)
    .bind(next(&sched))
    .execute(&state.db)
    .await?;
    // the creator of recurring issues becomes whoever changed the template last
    sqlx::query("UPDATE issue_templates SET created_by = $2 WHERE id = $1 AND schedule IS NOT NULL").bind(id).bind(user.id).execute(&state.db).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn delete(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    template_project(&state, &user, id, Role::Admin).await?;
    sqlx::query("DELETE FROM issue_templates WHERE id = $1").bind(id).execute(&state.db).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Creates an issue from a template right away (fields in the body win).
pub async fn instantiate(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<Uuid>,
    Json(req): Json<Value>,
) -> ApiResult<(StatusCode, Json<Summary>)> {
    template_project(&state, &user, id, Role::Member).await?;
    let overrides: Map<String, Value> = req.as_object().cloned().unwrap_or_default();
    let s = schedule::instantiate(&state, &Actor::of(&user), id, overrides).await?;
    Ok((StatusCode::CREATED, Json(s)))
}

/// Open issues with service-level goals, the most urgent first.
pub async fn sla_queue(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(key): Path<String>) -> ApiResult<Json<Value>> {
    let (p, _) = by_key(&state, &user, &key, Role::Viewer).await?;
    let ids: Vec<Uuid> = sqlx::query_scalar(
        "SELECT i.id FROM issues i JOIN statuses s ON s.id = i.status_id WHERE i.project_id = $1 AND i.deleted_at IS NULL AND s.category <> 'done' LIMIT 2000",
    )
    .bind(p.id)
    .fetch_all(&state.db)
    .await?;
    let goals = crate::sla::for_issues(&state, &ids).await?;
    let issues = crate::issues::load_many(&state, &goals.keys().copied().collect::<Vec<_>>()).await?;
    let mut rows: Vec<(chrono::DateTime<Utc>, Value)> = issues
        .into_iter()
        .filter_map(|s| {
            let g = goals.get(&s.id)?;
            let running: Vec<&crate::sla::Goal> = g.iter().filter(|g| !g.stopped).collect();
            let due = running.iter().map(|g| g.due_at).min()?;
            Some((due, json!({"issue": s, "sla": g})))
        })
        .collect();
    rows.sort_by_key(|r| r.0);
    let counts = goals.values().flatten().filter(|g| !g.stopped).fold(json!({"breached": 0, "atRisk": 0, "running": 0}), |mut acc, g| {
        let k = g.state;
        acc[k] = json!(acc[k].as_i64().unwrap_or(0) + 1);
        acc
    });
    Ok(Json(json!({"goals": p.settings["sla"], "counts": counts, "issues": rows.into_iter().map(|r| r.1).collect::<Vec<_>>()})))
}
