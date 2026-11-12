//! Saved views: a TQL query plus how it is shown (list columns, grouping, layout), private or
//! shared with everyone who can see its project.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::access::Role;
use crate::app::{AppState, new_id};
use crate::auth::CurrentUser;
use crate::error::{ApiError, ApiResult};

type ViewRow = (Uuid, Option<Uuid>, Option<String>, Option<Uuid>, Option<String>, String, String, String, Value, bool, chrono::DateTime<chrono::Utc>, bool);

fn view_json(r: &ViewRow, me: Uuid) -> Value {
    let (id, owner, owner_name, project, project_key, name, query, layout, config, shared, updated, starred) = r;
    json!({
        "id": id, "ownerId": owner, "ownerName": owner_name, "projectId": project, "projectKey": project_key, "name": name, "query": query,
        "layout": layout, "config": config, "shared": shared, "updatedAt": updated, "starred": starred, "mine": *owner == Some(me)
    })
}

pub async fn list(State(state): State<AppState>, CurrentUser(user): CurrentUser) -> ApiResult<Json<Value>> {
    let visible = crate::access::visible_projects(&state, &user).await?;
    let rows: Vec<ViewRow> = sqlx::query_as(
        "SELECT v.id, v.owner_id, u.name, v.project_id, p.key, v.name, v.query, v.layout, v.config, v.shared, v.updated_at,
                EXISTS (SELECT 1 FROM view_stars s WHERE s.view_id = v.id AND s.user_id = $1)
           FROM views v LEFT JOIN users u ON u.id = v.owner_id LEFT JOIN projects p ON p.id = v.project_id
          WHERE v.workspace_id = $2 AND (v.owner_id = $1 OR (v.shared AND (v.project_id IS NULL OR v.project_id = ANY($3))))
          ORDER BY lower(v.name)",
    )
    .bind(user.id)
    .bind(user.workspace_id)
    .bind(&visible)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(json!(rows.iter().map(|r| view_json(r, user.id)).collect::<Vec<_>>())))
}

fn layout(v: &Value) -> ApiResult<&str> {
    match v.as_str() {
        None => Ok("list"),
        Some(l @ ("list" | "board" | "calendar" | "timeline")) => Ok(l),
        _ => Err(ApiError::bad("layout must be list, board, calendar or timeline")),
    }
}

pub async fn create(State(state): State<AppState>, CurrentUser(user): CurrentUser, Json(req): Json<Value>) -> ApiResult<(StatusCode, Json<Value>)> {
    let name = req["name"].as_str().map(str::trim).filter(|n| !n.is_empty()).ok_or_else(|| ApiError::bad("a view needs a name"))?;
    let query = req["query"].as_str().unwrap_or("");
    crate::query::parse(query)?;
    let project = match req["projectKey"].as_str().filter(|k| !k.is_empty()) {
        Some(k) => Some(super::projects::by_key(&state, &user, k, Role::Viewer).await?.0.id),
        None => None,
    };
    let id = new_id();
    sqlx::query("INSERT INTO views (id, workspace_id, owner_id, project_id, name, query, layout, config, shared) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)")
        .bind(id)
        .bind(user.workspace_id)
        .bind(user.id)
        .bind(project)
        .bind(name)
        .bind(query)
        .bind(layout(&req["layout"])?)
        .bind(if req["config"].is_object() { req["config"].clone() } else { json!({}) })
        .bind(req["shared"].as_bool().unwrap_or(false) && !user.is_guest())
        .execute(&state.db)
        .await?;
    sqlx::query("INSERT INTO view_stars (user_id, view_id) VALUES ($1, $2)").bind(user.id).bind(id).execute(&state.db).await?;
    Ok((StatusCode::CREATED, Json(json!({"id": id}))))
}

async fn own(state: &AppState, user: &crate::auth::User, id: Uuid) -> ApiResult<()> {
    let owner: Option<Option<Uuid>> =
        sqlx::query_scalar("SELECT owner_id FROM views WHERE id = $1 AND workspace_id = $2").bind(id).bind(user.workspace_id).fetch_optional(&state.db).await?;
    match owner {
        None => Err(ApiError::NotFound("view")),
        Some(o) if o == Some(user.id) || user.is_admin() => Ok(()),
        _ => Err(ApiError::Denied("only the view's owner can change it".into())),
    }
}

pub async fn update(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(id): Path<Uuid>, Json(req): Json<Value>) -> ApiResult<StatusCode> {
    own(&state, &user, id).await?;
    if let Some(n) = req["name"].as_str().map(str::trim).filter(|n| !n.is_empty()) {
        sqlx::query("UPDATE views SET name = $2 WHERE id = $1").bind(id).bind(n).execute(&state.db).await?;
    }
    if let Some(q) = req["query"].as_str() {
        crate::query::parse(q)?;
        sqlx::query("UPDATE views SET query = $2 WHERE id = $1").bind(id).bind(q).execute(&state.db).await?;
    }
    if req.get("layout").is_some() {
        sqlx::query("UPDATE views SET layout = $2 WHERE id = $1").bind(id).bind(layout(&req["layout"])?).execute(&state.db).await?;
    }
    if req["config"].is_object() {
        sqlx::query("UPDATE views SET config = $2 WHERE id = $1").bind(id).bind(&req["config"]).execute(&state.db).await?;
    }
    if let Some(s) = req["shared"].as_bool() {
        sqlx::query("UPDATE views SET shared = $2 WHERE id = $1").bind(id).bind(s && !user.is_guest()).execute(&state.db).await?;
    }
    sqlx::query("UPDATE views SET updated_at = now() WHERE id = $1").bind(id).execute(&state.db).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn delete(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    own(&state, &user, id).await?;
    sqlx::query("DELETE FROM views WHERE id = $1").bind(id).execute(&state.db).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn star(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(id): Path<Uuid>, Json(req): Json<Value>) -> ApiResult<StatusCode> {
    if req["starred"].as_bool().unwrap_or(true) {
        sqlx::query("INSERT INTO view_stars (user_id, view_id) SELECT $1, id FROM views WHERE id = $2 AND workspace_id = $3 ON CONFLICT DO NOTHING")
            .bind(user.id)
            .bind(id)
            .bind(user.workspace_id)
            .execute(&state.db)
            .await?;
    } else {
        sqlx::query("DELETE FROM view_stars WHERE user_id = $1 AND view_id = $2").bind(user.id).bind(id).execute(&state.db).await?;
    }
    Ok(StatusCode::NO_CONTENT)
}

// ---- notifications ----

pub async fn notifications(State(state): State<AppState>, CurrentUser(user): CurrentUser) -> ApiResult<Json<Value>> {
    let rows: Vec<(Uuid, Option<Uuid>, Option<String>, String, Value, chrono::DateTime<chrono::Utc>, Option<chrono::DateTime<chrono::Utc>>)> = sqlx::query_as(
        "SELECT n.id, n.actor_id, u.name, n.kind, n.data, n.created_at, n.read_at FROM notifications n LEFT JOIN users u ON u.id = n.actor_id
          WHERE n.user_id = $1 ORDER BY n.created_at DESC LIMIT 100",
    )
    .bind(user.id)
    .fetch_all(&state.db)
    .await?;
    let unread: i64 =
        sqlx::query_scalar("SELECT count(*) FROM notifications WHERE user_id = $1 AND read_at IS NULL").bind(user.id).fetch_one(&state.db).await?;
    Ok(Json(json!({
        "unread": unread,
        "items": rows.into_iter().map(|(id, aid, an, kind, data, at, read)| json!({
            "id": id, "actor": aid.map(|a| json!({"id": a, "name": an})), "kind": kind, "data": data, "createdAt": at, "read": read.is_some()
        })).collect::<Vec<_>>()
    })))
}

/// Marks notifications read: `{"ids": [...]}` or `{"all": true}`.
pub async fn read_notifications(State(state): State<AppState>, CurrentUser(user): CurrentUser, Json(req): Json<Value>) -> ApiResult<StatusCode> {
    if req["all"].as_bool() == Some(true) {
        sqlx::query("UPDATE notifications SET read_at = now() WHERE user_id = $1 AND read_at IS NULL").bind(user.id).execute(&state.db).await?;
    } else {
        let ids: Vec<Uuid> = serde_json::from_value(req["ids"].clone()).unwrap_or_default();
        sqlx::query("UPDATE notifications SET read_at = now() WHERE user_id = $1 AND id = ANY($2) AND read_at IS NULL")
            .bind(user.id)
            .bind(&ids)
            .execute(&state.db)
            .await?;
    }
    Ok(StatusCode::NO_CONTENT)
}
