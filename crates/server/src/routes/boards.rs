//! Boards (columns of statuses, WIP limits, swimlanes, quick filters), sprints and the backlog.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use chrono::{DateTime, Duration, Utc};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use sqlx::QueryBuilder;
use uuid::Uuid;

use super::projects::by_key;
use crate::access::{self, Role};
use crate::app::{AppState, new_id};
use crate::auth::{CurrentUser, User};
use crate::error::{ApiError, ApiResult};
use crate::events::{self, Event};
use crate::issues::{self, Actor, Summary};

async fn board_row(state: &AppState, user: &User, id: Uuid, min: Role) -> ApiResult<(Uuid, String, String, Value, Role)> {
    let row: Option<(Uuid, String, String, Value)> = sqlx::query_as(
        "SELECT b.project_id, b.name, b.kind, b.config FROM boards b JOIN projects p ON p.id = b.project_id WHERE b.id = $1 AND p.workspace_id = $2",
    )
    .bind(id)
    .bind(user.workspace_id)
    .fetch_optional(&state.db)
    .await?;
    let (project, name, kind, config) = row.ok_or(ApiError::NotFound("board"))?;
    let role = access::require(state, user, project, min).await?;
    Ok((project, name, kind, config, role))
}

/// Issues of one project matching TQL `q` plus extra SQL conditions, in rank order.
async fn issues_where(state: &AppState, user: &User, project: Uuid, q: &str, extra: impl FnOnce(&mut QueryBuilder<sqlx::Postgres>)) -> ApiResult<Vec<Summary>> {
    let parsed = crate::query::parse(q)?;
    let mut ctx = crate::query::context(state, user, &parsed).await?;
    ctx.visible = vec![project];
    let mut qb: QueryBuilder<sqlx::Postgres> = QueryBuilder::new(
        "SELECT i.id FROM issues i JOIN projects p ON p.id = i.project_id JOIN statuses s ON s.id = i.status_id JOIN issue_types it ON it.id = i.type_id WHERE",
    );
    crate::query::push_where(&mut qb, &ctx, &parsed)?;
    extra(&mut qb);
    crate::query::push_order(&mut qb, &ctx, &parsed.order, "i.rank, i.id")?;
    qb.push(" LIMIT 2000");
    let ids: Vec<Uuid> = qb.build_query_scalar().fetch_all(&state.db).await?;
    let mut list = issues::load_many(state, &ids).await?;
    let pos: std::collections::HashMap<Uuid, usize> = ids.iter().enumerate().map(|(i, id)| (*id, i)).collect();
    list.sort_by_key(|s| pos.get(&s.id).copied().unwrap_or(usize::MAX));
    Ok(list)
}

fn and(a: &str, b: &str) -> String {
    match (a.trim().is_empty(), b.trim().is_empty()) {
        (true, true) => String::new(),
        (false, true) => a.to_string(),
        (true, false) => b.to_string(),
        (false, false) => format!("({a}) and ({b})"),
    }
}

#[derive(Deserialize)]
pub struct BoardQuery {
    /// extra TQL (quick filters, search box)
    q: Option<String>,
    sprint: Option<Uuid>,
}

pub async fn get(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(id): Path<Uuid>, Query(bq): Query<BoardQuery>) -> ApiResult<Json<Value>> {
    let (project, name, kind, config, role) = board_row(&state, &user, id, Role::Viewer).await?;
    let tql = and(config["filter"].as_str().unwrap_or(""), bq.q.as_deref().unwrap_or(""));
    let column_status: Vec<Uuid> = config["columns"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|c| c["statusIds"].as_array().cloned().unwrap_or_default())
        .filter_map(|s| s.as_str().and_then(|s| s.parse().ok()))
        .collect();
    let sprints: Vec<(Uuid, String, Option<String>, Option<DateTime<Utc>>, Option<DateTime<Utc>>)> =
        sqlx::query_as("SELECT id, name, goal, start_at, end_at FROM sprints WHERE project_id = $1 AND state = 'active' ORDER BY start_at")
            .bind(project)
            .fetch_all(&state.db)
            .await?;
    let (issues, sprint) = if kind == "scrum" {
        let chosen = bq.sprint.filter(|s| sprints.iter().any(|x| x.0 == *s)).or(sprints.first().map(|s| s.0));
        match chosen {
            None => (vec![], None),
            Some(sid) => {
                let list = issues_where(&state, &user, project, &tql, |qb| {
                    qb.push(" AND i.sprint_id = ");
                    qb.push_bind(sid);
                    qb.push(" AND it.level <= 0 AND i.status_id = ANY(");
                    qb.push_bind(column_status.clone());
                    qb.push(")");
                })
                .await?;
                (list, sprints.iter().find(|s| s.0 == sid).map(|(id, n, g, sa, ea)| json!({"id": id, "name": n, "goal": g, "startAt": sa, "endAt": ea})))
            }
        }
    } else {
        let days = config["hideDoneAfterDays"].as_i64().unwrap_or(14).clamp(0, 3650);
        let since = Utc::now() - Duration::days(days);
        let list = issues_where(&state, &user, project, &tql, |qb| {
            qb.push(" AND it.level <= 0 AND i.status_id = ANY(");
            qb.push_bind(column_status.clone());
            qb.push(") AND (s.category <> 'done' OR i.resolved_at IS NULL OR i.resolved_at >= ");
            qb.push_bind(since);
            qb.push(")");
        })
        .await?;
        (list, None)
    };
    Ok(Json(json!({
        "board": {"id": id, "name": name, "kind": kind, "config": config, "projectId": project},
        "canEdit": role >= Role::Member,
        "canConfigure": role >= Role::Admin,
        "sprint": sprint,
        "activeSprints": sprints.iter().map(|(id, n, _, _, ea)| json!({"id": id, "name": n, "endAt": ea})).collect::<Vec<_>>(),
        "issues": issues,
    })))
}

pub async fn create(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Path(key): Path<String>,
    Json(req): Json<Value>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let (p, _) = by_key(&state, &user, &key, Role::Admin).await?;
    let kind = req["kind"].as_str().filter(|k| matches!(*k, "kanban" | "scrum")).unwrap_or("kanban");
    let statuses: Vec<(Uuid, String)> =
        sqlx::query_as("SELECT id, name FROM statuses WHERE project_id = $1 ORDER BY position").bind(p.id).fetch_all(&state.db).await?;
    let config = json!({
        "columns": statuses.iter().map(|(id, n)| json!({"id": new_id(), "name": n, "statusIds": [id], "wipLimit": null})).collect::<Vec<_>>(),
        "swimlanes": "none",
        "quickFilters": [{"name": "Only my issues", "query": "assignee = me()"}],
        "cardFields": ["priority", "assignee", "points", "due", "labels"],
        "hideDoneAfterDays": 14,
        "filter": req["filter"].as_str().unwrap_or(""),
    });
    let id = new_id();
    sqlx::query("INSERT INTO boards (id, project_id, name, kind, config, position) VALUES ($1, $2, $3, $4, $5, (SELECT COALESCE(max(position), 0) + 1 FROM boards WHERE project_id = $2))")
        .bind(id)
        .bind(p.id)
        .bind(req["name"].as_str().map(str::trim).filter(|n| !n.is_empty()).unwrap_or("Board"))
        .bind(kind)
        .bind(config)
        .execute(&state.db)
        .await?;
    events::publish(&state, Event::new(user.workspace_id, "project").project(p.id).actor(user.id)).await;
    Ok((StatusCode::CREATED, Json(json!({"id": id}))))
}

fn check_config(c: &Value) -> ApiResult<()> {
    let cols = c["columns"].as_array().ok_or_else(|| ApiError::bad("a board needs columns"))?;
    if cols.is_empty() || cols.len() > 30 {
        return Err(ApiError::bad("a board has 1–30 columns"));
    }
    for col in cols {
        if col["name"].as_str().is_none_or(|n| n.trim().is_empty()) || !col["statusIds"].is_array() {
            return Err(ApiError::bad("every column needs a name and statusIds"));
        }
    }
    if let Some(f) = c["filter"].as_str().filter(|f| !f.trim().is_empty()) {
        crate::query::parse(f)?;
    }
    for q in c["quickFilters"].as_array().into_iter().flatten() {
        crate::query::parse(q["query"].as_str().unwrap_or(""))?;
    }
    if !matches!(c["swimlanes"].as_str(), None | Some("none" | "assignee" | "epic" | "priority" | "type")) {
        return Err(ApiError::bad("swimlanes are none, assignee, epic, priority or type"));
    }
    Ok(())
}

pub async fn update(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(id): Path<Uuid>, Json(req): Json<Value>) -> ApiResult<StatusCode> {
    let (project, _, _, mut config, role) = board_row(&state, &user, id, Role::Member).await?;
    if let Some(n) = req["name"].as_str().map(str::trim).filter(|n| !n.is_empty()) {
        if role < Role::Admin {
            return Err(ApiError::Forbidden);
        }
        sqlx::query("UPDATE boards SET name = $2 WHERE id = $1").bind(id).bind(n).execute(&state.db).await?;
    }
    if let Some(c) = req.get("config").and_then(|c| c.as_object()) {
        // members may change how the board looks (swimlanes); columns and filters are admin settings
        for (k, v) in c {
            if k != "swimlanes" && role < Role::Admin {
                return Err(ApiError::Denied("only project admins can configure boards".into()));
            }
            config[k] = v.clone();
        }
        check_config(&config)?;
        sqlx::query("UPDATE boards SET config = $2 WHERE id = $1").bind(id).bind(&config).execute(&state.db).await?;
    }
    events::publish(&state, Event::new(user.workspace_id, "board").project(project).actor(user.id)).await;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn delete(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    let (project, _, _, _, _) = board_row(&state, &user, id, Role::Admin).await?;
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM boards WHERE project_id = $1").bind(project).fetch_one(&state.db).await?;
    if n <= 1 {
        return Err(ApiError::bad("a project keeps at least one board"));
    }
    sqlx::query("DELETE FROM boards WHERE id = $1").bind(id).execute(&state.db).await?;
    events::publish(&state, Event::new(user.workspace_id, "project").project(project).actor(user.id)).await;
    Ok(StatusCode::NO_CONTENT)
}

// ---- sprints and backlog ----

#[derive(sqlx::FromRow)]
struct SprintRow {
    id: Uuid,
    project_id: Uuid,
    name: String,
    goal: Option<String>,
    state: String,
    start_at: Option<DateTime<Utc>>,
    end_at: Option<DateTime<Utc>>,
    completed_at: Option<DateTime<Utc>>,
    stats: Value,
}

fn sprint_json(s: &SprintRow) -> Value {
    json!({"id": s.id, "name": s.name, "goal": s.goal, "state": s.state, "startAt": s.start_at, "endAt": s.end_at, "completedAt": s.completed_at, "stats": s.stats})
}

const SPRINT_COLS: &str = "id, project_id, name, goal, state, start_at, end_at, completed_at, stats";

async fn sprint_for(state: &AppState, user: &User, id: Uuid, min: Role) -> ApiResult<SprintRow> {
    let s: Option<SprintRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {SPRINT_COLS} FROM sprints WHERE id = $1 AND project_id IN (SELECT id FROM projects WHERE workspace_id = $2)"
    )))
    .bind(id)
    .bind(user.workspace_id)
    .fetch_optional(&state.db)
    .await?;
    let s = s.ok_or(ApiError::NotFound("sprint"))?;
    access::require(state, user, s.project_id, min).await?;
    Ok(s)
}

/// Sprints (open ones with their issues) and the backlog, in rank order.
pub async fn backlog(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Path(key): Path<String>,
    Query(bq): Query<BoardQuery>,
) -> ApiResult<Json<Value>> {
    let (p, role) = by_key(&state, &user, &key, Role::Viewer).await?;
    let sprints: Vec<SprintRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {SPRINT_COLS} FROM sprints WHERE project_id = $1 AND state <> 'closed' ORDER BY state = 'active' DESC, position, created_at"
    )))
    .bind(p.id)
    .fetch_all(&state.db)
    .await?;
    let tql = bq.q.unwrap_or_default();
    let all = issues_where(&state, &user, p.id, &tql, |qb| {
        qb.push(" AND it.level = 0 AND (i.sprint_id IS NOT NULL AND i.sprint_id IN (SELECT id FROM sprints WHERE state <> 'closed') OR (i.sprint_id IS NULL AND s.category <> 'done'))");
    })
    .await?;
    let epics = issues_where(&state, &user, p.id, "", |qb| {
        qb.push(" AND it.level = 1");
    })
    .await?;
    let mut by_sprint: std::collections::HashMap<Option<Uuid>, Vec<&Summary>> = Default::default();
    for s in &all {
        by_sprint.entry(s.sprint.as_ref().map(|x| x.id)).or_default().push(s);
    }
    Ok(Json(json!({
        "canEdit": role >= Role::Member,
        "sprints": sprints.iter().map(|s| {
            let mut v = sprint_json(s);
            v["issues"] = json!(by_sprint.get(&Some(s.id)).cloned().unwrap_or_default());
            v
        }).collect::<Vec<_>>(),
        "backlog": by_sprint.get(&None).cloned().unwrap_or_default(),
        "epics": epics,
    })))
}

pub async fn list_sprints(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(key): Path<String>) -> ApiResult<Json<Value>> {
    let (p, _) = by_key(&state, &user, &key, Role::Viewer).await?;
    let sprints: Vec<SprintRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {SPRINT_COLS} FROM sprints WHERE project_id = $1 ORDER BY CASE state WHEN 'active' THEN 0 WHEN 'future' THEN 1 ELSE 2 END, COALESCE(completed_at, start_at, created_at) DESC"
    )))
    .bind(p.id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(json!(sprints.iter().map(sprint_json).collect::<Vec<_>>())))
}

fn ts(v: &Value) -> Option<DateTime<Utc>> {
    let s = v.as_str()?;
    DateTime::parse_from_rfc3339(s).map(|d| d.with_timezone(&Utc)).ok().or_else(|| {
        chrono::NaiveDate::parse_from_str(s.get(..10).unwrap_or(s), "%Y-%m-%d")
            .ok()
            .map(|d| DateTime::from_naive_utc_and_offset(d.and_hms_opt(9, 0, 0).unwrap_or_default(), Utc))
    })
}

pub async fn create_sprint(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Path(key): Path<String>,
    Json(req): Json<Value>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let (p, _) = by_key(&state, &user, &key, Role::Member).await?;
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM sprints WHERE project_id = $1").bind(p.id).fetch_one(&state.db).await?;
    let name = req["name"].as_str().map(str::trim).filter(|n| !n.is_empty()).map(str::to_string).unwrap_or_else(|| format!("{} Sprint {}", p.key, n + 1));
    let id = new_id();
    sqlx::query("INSERT INTO sprints (id, project_id, name, goal, start_at, end_at, position) VALUES ($1, $2, $3, $4, $5, $6, (SELECT COALESCE(max(position), 0) + 1 FROM sprints WHERE project_id = $2))")
        .bind(id)
        .bind(p.id)
        .bind(&name)
        .bind(req["goal"].as_str())
        .bind(ts(&req["startAt"]))
        .bind(ts(&req["endAt"]))
        .execute(&state.db)
        .await?;
    events::publish(&state, Event::new(user.workspace_id, "sprint").project(p.id).actor(user.id)).await;
    Ok((StatusCode::CREATED, Json(json!({"id": id, "name": name}))))
}

pub async fn update_sprint(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<Uuid>,
    Json(req): Json<Value>,
) -> ApiResult<StatusCode> {
    let s = sprint_for(&state, &user, id, Role::Member).await?;
    if let Some(n) = req["name"].as_str().map(str::trim).filter(|n| !n.is_empty()) {
        sqlx::query("UPDATE sprints SET name = $2 WHERE id = $1").bind(id).bind(n).execute(&state.db).await?;
    }
    if req.get("goal").is_some() {
        sqlx::query("UPDATE sprints SET goal = $2 WHERE id = $1")
            .bind(id)
            .bind(req["goal"].as_str().filter(|g| !g.trim().is_empty()))
            .execute(&state.db)
            .await?;
    }
    if req.get("startAt").is_some() {
        sqlx::query("UPDATE sprints SET start_at = $2 WHERE id = $1").bind(id).bind(ts(&req["startAt"])).execute(&state.db).await?;
    }
    if req.get("endAt").is_some() {
        sqlx::query("UPDATE sprints SET end_at = $2 WHERE id = $1").bind(id).bind(ts(&req["endAt"])).execute(&state.db).await?;
    }
    events::publish(&state, Event::new(user.workspace_id, "sprint").project(s.project_id).actor(user.id)).await;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn start_sprint(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<Uuid>,
    Json(req): Json<Value>,
) -> ApiResult<Json<Value>> {
    let s = sprint_for(&state, &user, id, Role::Member).await?;
    if s.state != "future" {
        return Err(ApiError::bad("only planned sprints can start"));
    }
    let active: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM sprints WHERE project_id = $1 AND state = 'active')").bind(s.project_id).fetch_one(&state.db).await?;
    if active {
        return Err(ApiError::bad("another sprint is running: complete it first"));
    }
    let start = ts(&req["startAt"]).or(s.start_at).unwrap_or_else(Utc::now);
    let weeks = req["weeks"].as_i64().unwrap_or(2).clamp(1, 8);
    let end = ts(&req["endAt"]).or(s.end_at).filter(|e| *e > start).unwrap_or(start + Duration::weeks(weeks));
    sqlx::query("UPDATE sprints SET state = 'active', start_at = $2, end_at = $3, goal = COALESCE($4, goal) WHERE id = $1")
        .bind(id)
        .bind(start.min(Utc::now()))
        .bind(end)
        .bind(req["goal"].as_str().filter(|g| !g.trim().is_empty()))
        .execute(&state.db)
        .await?;
    events::publish(&state, Event::new(user.workspace_id, "sprint").project(s.project_id).actor(user.id)).await;
    crate::webhooks::enqueue(&state, user.workspace_id, Some(s.project_id), "sprint.started", json!({"sprint": {"id": id, "name": s.name}})).await;
    Ok(Json(json!({"startAt": start.min(Utc::now()), "endAt": end})))
}

/// Completes the active sprint; unfinished issues move to `moveTo` (a planned sprint id,
/// `new` for a new sprint, or the backlog).
pub async fn complete_sprint(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<Uuid>,
    Json(req): Json<Value>,
) -> ApiResult<Json<Value>> {
    let s = sprint_for(&state, &user, id, Role::Member).await?;
    if s.state != "active" {
        return Err(ApiError::bad("only the running sprint can be completed"));
    }
    let target: Option<Uuid> = match req["moveTo"].as_str() {
        Some("new") => {
            let n: i64 = sqlx::query_scalar("SELECT count(*) FROM sprints WHERE project_id = $1").bind(s.project_id).fetch_one(&state.db).await?;
            let key: String = sqlx::query_scalar("SELECT key FROM projects WHERE id = $1").bind(s.project_id).fetch_one(&state.db).await?;
            let nid = new_id();
            sqlx::query("INSERT INTO sprints (id, project_id, name, position) VALUES ($1, $2, $3, (SELECT COALESCE(max(position), 0) + 1 FROM sprints WHERE project_id = $2))")
                .bind(nid)
                .bind(s.project_id)
                .bind(format!("{key} Sprint {}", n + 1))
                .execute(&state.db)
                .await?;
            Some(nid)
        }
        Some(x) if x != "backlog" => {
            let t: Uuid = x.parse().map_err(|_| ApiError::bad("moveTo is a sprint id, new or backlog"))?;
            let ok: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM sprints WHERE id = $1 AND project_id = $2 AND state = 'future')")
                .bind(t)
                .bind(s.project_id)
                .fetch_one(&state.db)
                .await?;
            if !ok {
                return Err(ApiError::bad("unfinished issues can only move to a planned sprint"));
            }
            Some(t)
        }
        _ => None,
    };
    let unfinished: Vec<Uuid> = sqlx::query_scalar(
        "SELECT i.id FROM issues i JOIN statuses st ON st.id = i.status_id WHERE i.sprint_id = $1 AND i.deleted_at IS NULL AND st.category <> 'done' ORDER BY i.rank",
    )
    .bind(id)
    .fetch_all(&state.db)
    .await?;
    let actor = Actor::of(&user);
    for issue in &unfinished {
        let mut patch = Map::new();
        patch.insert("sprintId".into(), json!(target));
        issues::update(&state, &actor, *issue, &patch).await?;
    }
    // close it, then keep its figures
    sqlx::query("UPDATE sprints SET state = 'closed', completed_at = now() WHERE id = $1").bind(id).execute(&state.db).await?;
    let mut stats = crate::reports::sprint_stats(&state, id).await.unwrap_or(json!({}));
    stats["moved"] = json!(unfinished.len());
    sqlx::query("UPDATE sprints SET stats = $2 WHERE id = $1").bind(id).bind(&stats).execute(&state.db).await?;
    events::publish(&state, Event::new(user.workspace_id, "sprint").project(s.project_id).actor(user.id)).await;
    crate::webhooks::enqueue(&state, user.workspace_id, Some(s.project_id), "sprint.completed", json!({"sprint": {"id": id, "name": s.name}, "stats": stats}))
        .await;
    Ok(Json(json!({"moved": unfinished.len(), "movedTo": target, "stats": stats})))
}

pub async fn delete_sprint(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    let s = sprint_for(&state, &user, id, Role::Member).await?;
    if s.state != "future" {
        return Err(ApiError::bad("only planned sprints can be deleted"));
    }
    let actor = Actor::of(&user);
    let members: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM issues WHERE sprint_id = $1 AND deleted_at IS NULL").bind(id).fetch_all(&state.db).await?;
    for issue in members {
        let mut patch = Map::new();
        patch.insert("sprintId".into(), Value::Null);
        issues::update(&state, &actor, issue, &patch).await?;
    }
    sqlx::query("DELETE FROM sprints WHERE id = $1").bind(id).execute(&state.db).await?;
    events::publish(&state, Event::new(user.workspace_id, "sprint").project(s.project_id).actor(user.id)).await;
    Ok(StatusCode::NO_CONTENT)
}
