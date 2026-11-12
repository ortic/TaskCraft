//! Dashboards (widgets people arrange: issue lists, counts, breakdowns, reports, notes), the
//! grouped counts their charts use, and the portfolio across projects.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::QueryBuilder;
use uuid::Uuid;

use crate::app::{AppState, new_id};
use crate::auth::{CurrentUser, User};
use crate::error::{ApiError, ApiResult};
use crate::query::priority_name;

const WIDGETS: &[&str] = &["issues", "count", "breakdown", "burndown", "velocity", "flow", "created", "activity", "mywork", "note"];

fn clean_widgets(v: &Value) -> ApiResult<Value> {
    let list = v.as_array().ok_or_else(|| ApiError::bad("widgets must be a list"))?;
    if list.len() > 40 {
        return Err(ApiError::bad("at most 40 widgets"));
    }
    let mut out = Vec::new();
    for w in list {
        let kind = w["type"].as_str().filter(|k| WIDGETS.contains(k)).ok_or_else(|| ApiError::bad(format!("widget types: {}", WIDGETS.join(", "))))?;
        if let Some(q) = w["config"]["query"].as_str().filter(|q| !q.trim().is_empty()) {
            crate::query::parse(q)?;
        }
        out.push(json!({
            "id": w["id"].as_str().filter(|s| !s.is_empty()).map(str::to_string).unwrap_or_else(|| new_id().to_string()),
            "type": kind,
            "title": w["title"].as_str().unwrap_or("").chars().take(120).collect::<String>(),
            "config": if w["config"].is_object() { w["config"].clone() } else { json!({}) },
            "wide": w["wide"].as_bool().unwrap_or(false),
        }));
    }
    Ok(Value::Array(out))
}

type Row = (Uuid, Option<Uuid>, Option<String>, String, bool, Value, chrono::DateTime<chrono::Utc>);

fn dash_json(r: Row, me: Uuid) -> Value {
    let (id, owner, owner_name, name, shared, widgets, updated) = r;
    json!({"id": id, "ownerId": owner, "ownerName": owner_name, "name": name, "shared": shared, "widgets": widgets, "updatedAt": updated, "mine": owner == Some(me)})
}

const SELECT: &str = "SELECT d.id, d.owner_id, u.name, d.name, d.shared, d.widgets, d.updated_at FROM dashboards d LEFT JOIN users u ON u.id = d.owner_id";

pub async fn list(State(state): State<AppState>, CurrentUser(user): CurrentUser) -> ApiResult<Json<Value>> {
    let rows: Vec<Row> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "{SELECT} WHERE d.workspace_id = $1 AND (d.owner_id = $2 OR d.shared) ORDER BY d.owner_id = $2 DESC, lower(d.name)"
    )))
    .bind(user.workspace_id)
    .bind(user.id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(json!(rows.into_iter().map(|r| dash_json(r, user.id)).collect::<Vec<_>>())))
}

async fn find(state: &AppState, user: &User, id: Uuid) -> ApiResult<Row> {
    let row: Option<Row> = sqlx::query_as(sqlx::AssertSqlSafe(format!("{SELECT} WHERE d.id = $1 AND d.workspace_id = $2 AND (d.owner_id = $3 OR d.shared)")))
        .bind(id)
        .bind(user.workspace_id)
        .bind(user.id)
        .fetch_optional(&state.db)
        .await?;
    row.ok_or(ApiError::NotFound("dashboard"))
}

pub async fn get(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(id): Path<Uuid>) -> ApiResult<Json<Value>> {
    Ok(Json(dash_json(find(&state, &user, id).await?, user.id)))
}

pub async fn create(State(state): State<AppState>, CurrentUser(user): CurrentUser, Json(req): Json<Value>) -> ApiResult<(StatusCode, Json<Value>)> {
    let name = req["name"].as_str().map(str::trim).filter(|n| !n.is_empty()).ok_or_else(|| ApiError::bad("a dashboard needs a name"))?;
    let widgets = if req["widgets"].is_array() { clean_widgets(&req["widgets"])? } else { starter_widgets() };
    let id = new_id();
    sqlx::query("INSERT INTO dashboards (id, workspace_id, owner_id, name, shared, widgets) VALUES ($1, $2, $3, $4, $5, $6)")
        .bind(id)
        .bind(user.workspace_id)
        .bind(user.id)
        .bind(name)
        .bind(req["shared"].as_bool().unwrap_or(false) && !user.is_guest())
        .bind(widgets)
        .execute(&state.db)
        .await?;
    Ok((StatusCode::CREATED, Json(json!({"id": id}))))
}

/// What a new dashboard starts with.
fn starter_widgets() -> Value {
    json!([
        {"id": new_id(), "type": "count", "title": "Assigned to me", "config": {"query": "assignee = me() and category != done"}, "wide": false},
        {"id": new_id(), "type": "count", "title": "Overdue", "config": {"query": "due < today() and category != done", "tone": "danger"}, "wide": false},
        {"id": new_id(), "type": "mywork", "title": "", "config": {}, "wide": true},
        {"id": new_id(), "type": "breakdown", "title": "Open issues by status", "config": {"query": "category != done", "by": "status"}, "wide": false},
        {"id": new_id(), "type": "activity", "title": "", "config": {}, "wide": false}
    ])
}

pub async fn update(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(id): Path<Uuid>, Json(req): Json<Value>) -> ApiResult<StatusCode> {
    let (_, owner, ..) = find(&state, &user, id).await?;
    if owner != Some(user.id) && !user.is_admin() {
        return Err(ApiError::Denied("only the dashboard's owner can change it".into()));
    }
    if let Some(n) = req["name"].as_str().map(str::trim).filter(|n| !n.is_empty()) {
        sqlx::query("UPDATE dashboards SET name = $2 WHERE id = $1").bind(id).bind(n).execute(&state.db).await?;
    }
    if let Some(s) = req["shared"].as_bool() {
        sqlx::query("UPDATE dashboards SET shared = $2 WHERE id = $1").bind(id).bind(s && !user.is_guest()).execute(&state.db).await?;
    }
    if req.get("widgets").is_some() {
        sqlx::query("UPDATE dashboards SET widgets = $2 WHERE id = $1").bind(id).bind(clean_widgets(&req["widgets"])?).execute(&state.db).await?;
    }
    sqlx::query("UPDATE dashboards SET updated_at = now() WHERE id = $1").bind(id).execute(&state.db).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn delete(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    let (_, owner, ..) = find(&state, &user, id).await?;
    if owner != Some(user.id) && !user.is_admin() {
        return Err(ApiError::Denied("only the dashboard's owner can delete it".into()));
    }
    sqlx::query("DELETE FROM dashboards WHERE id = $1").bind(id).execute(&state.db).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct GroupQuery {
    #[serde(default)]
    q: String,
    by: String,
}

/// Issues matching a query, counted per status / person / priority / type / project / label /
/// sprint / category.
pub async fn groups(State(state): State<AppState>, CurrentUser(user): CurrentUser, Query(g): Query<GroupQuery>) -> ApiResult<Json<Value>> {
    let parsed = crate::query::parse(&g.q)?;
    let ctx = crate::query::context(&state, &user, &parsed).await?;
    let (key, label, extra, join) = match g.by.as_str() {
        "status" => ("lower(s.name)", "min(s.name)", "min(s.color), min(s.category)", ""),
        "category" => ("s.category", "min(s.category)", "NULL::text, min(s.category)", ""),
        "assignee" => ("COALESCE(a.id::text, '')", "min(a.name)", "NULL::text, NULL::text", "LEFT JOIN users a ON a.id = i.assignee_id"),
        "priority" => ("i.priority::text", "min(i.priority)::text", "NULL::text, NULL::text", ""),
        "type" => ("lower(it.name)", "min(it.name)", "min(it.color), min(it.icon)", ""),
        "project" => ("p.key", "min(p.name)", "min(p.color), min(p.icon)", ""),
        "labels" => ("COALESCE(l.label, '')", "min(l.label)", "NULL::text, NULL::text", "LEFT JOIN issue_labels l ON l.issue_id = i.id"),
        "sprint" => ("COALESCE(sp.id::text, '')", "min(sp.name)", "NULL::text, min(sp.state)", "LEFT JOIN sprints sp ON sp.id = i.sprint_id"),
        _ => return Err(ApiError::bad("by is status, category, assignee, priority, type, project, labels or sprint")),
    };
    let mut qb: QueryBuilder<sqlx::Postgres> = QueryBuilder::new(format!(
        "SELECT {key} AS k, {label} AS label, count(DISTINCT i.id), COALESCE(sum(i.story_points), 0)::float8, {extra}
           FROM issues i JOIN projects p ON p.id = i.project_id JOIN statuses s ON s.id = i.status_id JOIN issue_types it ON it.id = i.type_id {join} WHERE"
    ));
    crate::query::push_where(&mut qb, &ctx, &parsed)?;
    qb.push(format!(" GROUP BY {key} ORDER BY count(DISTINCT i.id) DESC LIMIT 50"));
    let rows: Vec<(String, Option<String>, i64, f64, Option<String>, Option<String>)> = qb.build_query_as().fetch_all(&state.db).await?;
    let total: i64 = rows.iter().map(|r| r.2).sum();
    Ok(Json(json!({
        "by": g.by,
        "total": total,
        "groups": rows.into_iter().map(|(k, label, count, points, color, more)| {
            let label = match (g.by.as_str(), label) {
                ("priority", Some(p)) => Some(priority_name(p.parse().unwrap_or(2)).to_string()),
                (_, l) => l,
            };
            json!({"key": k, "label": label, "count": count, "points": points, "color": color, "extra": more})
        }).collect::<Vec<_>>()
    })))
}

#[derive(Deserialize)]
pub struct PortfolioQuery {
    /// comma-separated project keys (default: starred projects, else all)
    projects: Option<String>,
}

/// Epics and releases of several projects on one timeline, and how loaded each team is.
pub async fn portfolio(State(state): State<AppState>, CurrentUser(user): CurrentUser, Query(q): Query<PortfolioQuery>) -> ApiResult<Json<Value>> {
    let visible = crate::access::visible_projects(&state, &user).await?;
    let wanted: Vec<String> = q.projects.as_deref().unwrap_or("").split(',').map(|k| k.trim().to_uppercase()).filter(|k| !k.is_empty()).collect();
    let projects: Vec<(Uuid, String, String, Option<String>, Option<String>, String)> = if wanted.is_empty() {
        let starred: Vec<(Uuid, String, String, Option<String>, Option<String>, String)> = sqlx::query_as(
            "SELECT p.id, p.key, p.name, p.color, p.icon, p.kind FROM projects p JOIN project_stars s ON s.project_id = p.id AND s.user_id = $2
              WHERE p.id = ANY($1) AND p.archived_at IS NULL ORDER BY lower(p.name)",
        )
        .bind(&visible)
        .bind(user.id)
        .fetch_all(&state.db)
        .await?;
        if starred.is_empty() {
            sqlx::query_as("SELECT id, key, name, color, icon, kind FROM projects WHERE id = ANY($1) AND archived_at IS NULL ORDER BY lower(name)")
                .bind(&visible)
                .fetch_all(&state.db)
                .await?
        } else {
            starred
        }
    } else {
        sqlx::query_as("SELECT id, key, name, color, icon, kind FROM projects WHERE id = ANY($1) AND upper(key) = ANY($2) ORDER BY lower(name)")
            .bind(&visible)
            .bind(&wanted)
            .fetch_all(&state.db)
            .await?
    };
    let ids: Vec<Uuid> = projects.iter().map(|p| p.0).collect();
    // epics: open ones and those finished in the last 60 days, with their children's progress
    type Epic = (
        Uuid,
        Uuid,
        String,
        String,
        Option<String>,
        Option<chrono::NaiveDate>,
        Option<chrono::NaiveDate>,
        String,
        i64,
        i64,
        f64,
        f64,
        Option<chrono::NaiveDate>,
        Option<chrono::NaiveDate>,
    );
    let epics: Vec<Epic> = sqlx::query_as(
        "SELECT i.id, i.project_id, p.key || '-' || i.number, i.title, it.color, i.start_date, i.due_date, s.category,
                (SELECT count(*) FROM issues c WHERE c.parent_id = i.id AND c.deleted_at IS NULL),
                (SELECT count(*) FROM issues c JOIN statuses cs ON cs.id = c.status_id WHERE c.parent_id = i.id AND c.deleted_at IS NULL AND cs.category = 'done'),
                COALESCE((SELECT sum(c.story_points) FROM issues c WHERE c.parent_id = i.id AND c.deleted_at IS NULL), 0)::float8,
                COALESCE((SELECT sum(c.story_points) FROM issues c JOIN statuses cs ON cs.id = c.status_id WHERE c.parent_id = i.id AND c.deleted_at IS NULL AND cs.category = 'done'), 0)::float8,
                (SELECT min(COALESCE(c.start_date, c.due_date)) FROM issues c WHERE c.parent_id = i.id AND c.deleted_at IS NULL),
                (SELECT max(COALESCE(c.due_date, c.start_date)) FROM issues c WHERE c.parent_id = i.id AND c.deleted_at IS NULL)
           FROM issues i JOIN projects p ON p.id = i.project_id JOIN issue_types it ON it.id = i.type_id JOIN statuses s ON s.id = i.status_id
          WHERE i.project_id = ANY($1) AND i.deleted_at IS NULL AND it.level = 1 AND (s.category <> 'done' OR i.resolved_at > now() - interval '60 days')
          ORDER BY COALESCE(i.start_date, i.due_date) NULLS LAST, i.rank",
    )
    .bind(&ids)
    .fetch_all(&state.db)
    .await?;
    type Ver = (Uuid, Uuid, String, String, Option<chrono::NaiveDate>, Option<chrono::NaiveDate>, i64, i64);
    let versions: Vec<Ver> = sqlx::query_as(
        "SELECT v.id, v.project_id, v.name, v.status, v.start_date, v.release_date,
                (SELECT count(*) FROM issues i WHERE i.version_id = v.id AND i.deleted_at IS NULL),
                (SELECT count(*) FROM issues i JOIN statuses s ON s.id = i.status_id WHERE i.version_id = v.id AND i.deleted_at IS NULL AND s.category = 'done')
           FROM versions v WHERE v.project_id = ANY($1) AND (v.status = 'unreleased' OR v.released_at > now() - interval '60 days')
          ORDER BY v.release_date NULLS LAST",
    )
    .bind(&ids)
    .fetch_all(&state.db)
    .await?;
    // teams: everyone's open work in these projects
    let load: Vec<(Uuid, String, Uuid, String, i64, i64, f64, i64, i64)> = sqlx::query_as(
        "SELECT t.id, t.name, u.id, u.name,
                count(i.id) FILTER (WHERE s.category = 'todo'), count(i.id) FILTER (WHERE s.category = 'in_progress'),
                COALESCE(sum(i.story_points) FILTER (WHERE s.category <> 'done'), 0)::float8,
                count(i.id) FILTER (WHERE s.category <> 'done' AND i.due_date < current_date),
                count(i.id) FILTER (WHERE s.category <> 'done' AND i.due_date BETWEEN current_date AND current_date + 7)
           FROM teams t JOIN team_members m ON m.team_id = t.id JOIN users u ON u.id = m.user_id AND u.status = 'active'
           LEFT JOIN issues i ON i.assignee_id = u.id AND i.deleted_at IS NULL AND i.project_id = ANY($2)
           LEFT JOIN statuses s ON s.id = i.status_id
          WHERE t.workspace_id = $1 GROUP BY t.id, t.name, u.id, u.name ORDER BY lower(t.name), lower(u.name)",
    )
    .bind(user.workspace_id)
    .bind(&ids)
    .fetch_all(&state.db)
    .await?;
    let mut teams: Vec<Value> = Vec::new();
    for (tid, tname, uid, uname, todo, doing, points, overdue, soon) in load {
        let member = json!({"id": uid, "name": uname, "todo": todo, "inProgress": doing, "points": points, "overdue": overdue, "dueThisWeek": soon});
        match teams.iter_mut().find(|t| t["id"] == json!(tid)) {
            Some(t) => t["members"].as_array_mut().expect("members").push(member),
            None => teams.push(json!({"id": tid, "name": tname, "members": [member]})),
        }
    }
    Ok(Json(json!({
        "projects": projects.iter().map(|(id, key, name, color, icon, kind)| json!({
            "id": id, "key": key, "name": name, "color": color, "icon": icon, "kind": kind,
            "epics": epics.iter().filter(|e| e.1 == *id).map(|e| json!({
                "id": e.0, "key": e.2, "title": e.3, "color": e.4, "startDate": e.5, "dueDate": e.6, "category": e.7,
                "childCount": e.8, "childDone": e.9, "points": e.10, "pointsDone": e.11, "childStart": e.12, "childEnd": e.13,
            })).collect::<Vec<_>>(),
            "versions": versions.iter().filter(|v| v.1 == *id).map(|v| json!({
                "id": v.0, "name": v.2, "status": v.3, "startDate": v.4, "releaseDate": v.5, "total": v.6, "done": v.7,
            })).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
        "teams": teams,
    })))
}
