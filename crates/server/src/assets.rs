//! Asset inventory: the devices, services, locations and other things issues are about.
//!
//! **Sets** (Hardware, Services, Locations) have their own permissions — what everyone in the
//! workspace may do, plus grants for people and teams (viewer, editor, admin; workspace admins
//! administer every set). A set has **types** (Laptop, Server) with an icon, a tag prefix and
//! fields of the usual kinds plus references to other assets, **categories** in a tree, and the
//! **assets**: a tag (`LT-0042`), a name, a status, an owner (a person, a team, or an address
//! without an account) and the type's field values. Assets **relate** to one another (depends
//! on, installed in), and **issues** name the assets they're about — an asset's page lists its
//! issues and its history, and what depends on it. CSV import and export, and **rules** that act
//! when an asset is created or changes, or some days before one of its dates.

use std::collections::{HashMap, HashSet};

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, NaiveDate, Utc};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use uuid::Uuid;

use crate::access::Role;
use crate::app::{AppState, audit, new_id};
use crate::auth::{CurrentUser, User};
use crate::error::{ApiError, ApiResult};
use crate::issues::Actor;

pub const STATUSES: [&str; 4] = ["in_use", "in_stock", "maintenance", "retired"];
const FIELD_KINDS: [&str; 8] = ["text", "number", "date", "select", "checkbox", "url", "user", "asset"];
const RELATIONS: [&str; 2] = ["depends_on", "installed_in"];

// ---- who may do what in a set ---------------------------------------------------------------------

/// The caller's rank in sets: 0 none, 1 viewer, 2 editor, 3 admin. `$1`: the user, `$2`: workspace
/// admin, `$3`: guest; `s` is the set.
const RANK_SQL: &str = "GREATEST(
    CASE WHEN $2 THEN 3 ELSE 0 END,
    CASE WHEN $3 THEN 0 ELSE CASE s.default_role WHEN 'editor' THEN 2 WHEN 'viewer' THEN 1 ELSE 0 END END,
    COALESCE((SELECT max(CASE m.role WHEN 'admin' THEN 3 WHEN 'editor' THEN 2 ELSE 1 END) FROM asset_set_members m
               WHERE m.set_id = s.id AND ((m.subject_type = 'user' AND m.subject_id = $1)
                  OR (m.subject_type = 'team' AND m.subject_id IN (SELECT team_id FROM team_members WHERE user_id = $1)))), 0))";

const VIEWER: i32 = 1;
const EDITOR: i32 = 2;
const ADMIN: i32 = 3;

async fn on(state: &AppState, user: &User) -> ApiResult<()> {
    crate::features::require(state, user.workspace_id, "assets").await
}

/// The caller's rank in a set; a set they can't see is "not found".
async fn rank_in(state: &AppState, user: &User, set: Uuid, min: i32) -> ApiResult<i32> {
    let rank: Option<i32> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT {RANK_SQL} FROM asset_sets s WHERE s.id = $4 AND s.workspace_id = $5")))
        .bind(user.id)
        .bind(user.is_admin())
        .bind(user.is_guest())
        .bind(set)
        .bind(user.workspace_id)
        .fetch_optional(&state.db)
        .await?;
    match rank {
        None | Some(0) => Err(ApiError::NotFound("asset set")),
        Some(r) if r < min => {
            Err(ApiError::Denied(if min >= ADMIN { "only the set's admins may do that".into() } else { "you can only view this set".into() }))
        }
        Some(r) => Ok(r),
    }
}

/// The sets the caller sees, with their rank.
async fn visible_sets(state: &AppState, user: &User) -> ApiResult<HashMap<Uuid, i32>> {
    let rows: Vec<(Uuid, i32)> = sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT s.id, {RANK_SQL} FROM asset_sets s WHERE s.workspace_id = $4")))
        .bind(user.id)
        .bind(user.is_admin())
        .bind(user.is_guest())
        .bind(user.workspace_id)
        .fetch_all(&state.db)
        .await?;
    Ok(rows.into_iter().filter(|(_, r)| *r > 0).collect())
}

fn rank_name(r: i32) -> &'static str {
    match r {
        3 => "admin",
        2 => "editor",
        1 => "viewer",
        _ => "none",
    }
}

fn name_of(v: &Value, what: &str, max: usize) -> ApiResult<String> {
    v.as_str()
        .map(str::trim)
        .filter(|n| !n.is_empty() && n.chars().count() <= max)
        .map(str::to_string)
        .ok_or_else(|| ApiError::bad(format!("{what} needs a name (up to {max} characters)")))
}

fn duplicate(what: &'static str) -> impl Fn(sqlx::Error) -> ApiError {
    move |e| {
        if e.as_database_error().is_some_and(|d| d.is_unique_violation()) {
            ApiError::Conflict(format!("there is {what} with that name already"))
        } else {
            e.into()
        }
    }
}

// ---- sets, types, categories ------------------------------------------------------------------------

/// `GET /asset-sets`: the sets the caller sees, each with its types, categories and their role.
pub async fn sets(State(state): State<AppState>, CurrentUser(user): CurrentUser) -> ApiResult<Json<Value>> {
    on(&state, &user).await?;
    let visible = visible_sets(&state, &user).await?;
    let ids: Vec<Uuid> = visible.keys().copied().collect();
    let rows: Vec<(Uuid, String, Option<String>, String, i64)> = sqlx::query_as(
        "SELECT s.id, s.name, s.description, s.default_role, (SELECT count(*) FROM assets a WHERE a.set_id = s.id) FROM asset_sets s WHERE s.id = ANY($1) ORDER BY lower(s.name)",
    )
    .bind(&ids)
    .fetch_all(&state.db)
    .await?;
    let types: Vec<(Uuid, Uuid, String, Option<String>, Option<String>, String, Value, i64)> = sqlx::query_as(
        "SELECT t.id, t.set_id, t.name, t.icon, t.color, t.tag_prefix, t.fields, (SELECT count(*) FROM assets a WHERE a.type_id = t.id) FROM asset_types t WHERE t.set_id = ANY($1) ORDER BY t.position, lower(t.name)",
    )
    .bind(&ids)
    .fetch_all(&state.db)
    .await?;
    let cats: Vec<(Uuid, Uuid, Option<Uuid>, String)> =
        sqlx::query_as("SELECT id, set_id, parent_id, name FROM asset_categories WHERE set_id = ANY($1) ORDER BY lower(name)")
            .bind(&ids)
            .fetch_all(&state.db)
            .await?;
    let members: Vec<(Uuid, String, Uuid, String, Option<String>)> = sqlx::query_as(
        "SELECT m.set_id, m.subject_type, m.subject_id, m.role, COALESCE(u.name, t.name) FROM asset_set_members m
           LEFT JOIN users u ON m.subject_type = 'user' AND u.id = m.subject_id LEFT JOIN teams t ON m.subject_type = 'team' AND t.id = m.subject_id WHERE m.set_id = ANY($1)",
    )
    .bind(&ids)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(json!({
        "canCreate": user.is_admin(),
        "sets": rows.iter().map(|(id, name, description, default_role, count)| json!({
            "id": id, "name": name, "description": description, "defaultRole": default_role, "assetCount": count, "role": rank_name(visible[id]),
            "types": types.iter().filter(|t| t.1 == *id).map(|(tid, _, n, icon, color, prefix, fields, c)| json!({
                "id": tid, "name": n, "icon": icon, "color": color, "tagPrefix": prefix, "fields": fields, "assetCount": c,
            })).collect::<Vec<_>>(),
            "categories": cats.iter().filter(|c| c.1 == *id).map(|(cid, _, parent, n)| json!({"id": cid, "parentId": parent, "name": n})).collect::<Vec<_>>(),
            // who else has a role: for the set's admins
            "members": if visible[id] >= ADMIN {
                json!(members.iter().filter(|m| m.0 == *id).map(|(_, st, sid, role, n)| json!({"subjectType": st, "subjectId": sid, "role": role, "name": n})).collect::<Vec<_>>())
            } else { Value::Null },
        })).collect::<Vec<_>>(),
    })))
}

fn default_role(v: &Value) -> ApiResult<&str> {
    match v.as_str() {
        None => Ok("viewer"),
        Some(r @ ("none" | "viewer" | "editor")) => Ok(r),
        Some(_) => Err(ApiError::bad("defaultRole is none, viewer or editor")),
    }
}

/// `POST /asset-sets` (workspace admins) `{name, description?, defaultRole?}`.
pub async fn create_set(State(state): State<AppState>, CurrentUser(user): CurrentUser, Json(req): Json<Value>) -> ApiResult<(StatusCode, Json<Value>)> {
    on(&state, &user).await?;
    crate::auth::require_admin(&user)?;
    let name = name_of(&req["name"], "a set", 80)?;
    let id = new_id();
    sqlx::query("INSERT INTO asset_sets (id, workspace_id, name, description, default_role) VALUES ($1, $2, $3, $4, $5)")
        .bind(id)
        .bind(user.workspace_id)
        .bind(&name)
        .bind(req["description"].as_str().map(str::trim).filter(|d| !d.is_empty()))
        .bind(default_role(&req["defaultRole"])?)
        .execute(&state.db)
        .await
        .map_err(duplicate("a set"))?;
    audit(&state, user.workspace_id, Some(user.id), "assets.set.created", Some(name), json!({})).await;
    Ok((StatusCode::CREATED, Json(json!({"id": id}))))
}

/// `PATCH /asset-sets/{id}` (the set's admins) `{name?, description?, defaultRole?, members?: [{subjectType, subjectId, role}]}`.
pub async fn update_set(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(id): Path<Uuid>, Json(req): Json<Value>) -> ApiResult<StatusCode> {
    on(&state, &user).await?;
    rank_in(&state, &user, id, ADMIN).await?;
    let mut tx = state.db.begin().await?;
    if !req["name"].is_null() {
        sqlx::query("UPDATE asset_sets SET name = $2 WHERE id = $1")
            .bind(id)
            .bind(name_of(&req["name"], "a set", 80)?)
            .execute(&mut *tx)
            .await
            .map_err(duplicate("a set"))?;
    }
    if req.get("description").is_some() {
        sqlx::query("UPDATE asset_sets SET description = $2 WHERE id = $1")
            .bind(id)
            .bind(req["description"].as_str().map(str::trim).filter(|d| !d.is_empty()))
            .execute(&mut *tx)
            .await?;
    }
    if !req["defaultRole"].is_null() {
        sqlx::query("UPDATE asset_sets SET default_role = $2 WHERE id = $1").bind(id).bind(default_role(&req["defaultRole"])?).execute(&mut *tx).await?;
    }
    if let Some(list) = req["members"].as_array() {
        sqlx::query("DELETE FROM asset_set_members WHERE set_id = $1").bind(id).execute(&mut *tx).await?;
        for m in list.iter().take(200) {
            let kind = m["subjectType"].as_str().filter(|k| matches!(*k, "user" | "team")).ok_or_else(|| ApiError::bad("subjectType is user or team"))?;
            let subject: Uuid = m["subjectId"].as_str().and_then(|s| s.parse().ok()).ok_or_else(|| ApiError::bad("subjectId is an id"))?;
            let role =
                m["role"].as_str().filter(|r| matches!(*r, "viewer" | "editor" | "admin")).ok_or_else(|| ApiError::bad("role is viewer, editor or admin"))?;
            let table = if kind == "user" { "users" } else { "teams" };
            let ok: bool = sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT EXISTS (SELECT 1 FROM {table} WHERE id = $1 AND workspace_id = $2)")))
                .bind(subject)
                .bind(user.workspace_id)
                .fetch_one(&mut *tx)
                .await?;
            if !ok {
                return Err(ApiError::bad("a member isn't a person or team of this workspace"));
            }
            sqlx::query("INSERT INTO asset_set_members (set_id, subject_type, subject_id, role) VALUES ($1, $2, $3, $4) ON CONFLICT (set_id, subject_type, subject_id) DO UPDATE SET role = EXCLUDED.role")
                .bind(id)
                .bind(kind)
                .bind(subject)
                .bind(role)
                .execute(&mut *tx)
                .await?;
        }
    }
    tx.commit().await?;
    audit(&state, user.workspace_id, Some(user.id), "assets.set.updated", Some(id.to_string()), json!({"members": req["members"].is_array()})).await;
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /asset-sets/{id}` (workspace admins): the set with everything in it.
pub async fn delete_set(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    on(&state, &user).await?;
    crate::auth::require_admin(&user)?;
    rank_in(&state, &user, id, ADMIN).await?;
    let mut tx = state.db.begin().await?;
    sqlx::query("DELETE FROM assets WHERE set_id = $1").bind(id).execute(&mut *tx).await?;
    let name: String = sqlx::query_scalar("DELETE FROM asset_sets WHERE id = $1 RETURNING name").bind(id).fetch_one(&mut *tx).await?;
    tx.commit().await?;
    audit(&state, user.workspace_id, Some(user.id), "assets.set.deleted", Some(name), json!({})).await;
    Ok(StatusCode::NO_CONTENT)
}

/// Checks and tidies a type's fields; ids are kept (or made) so values stay with their field.
fn check_fields(v: &Value) -> ApiResult<Value> {
    let list = match v {
        Value::Null => return Ok(json!([])),
        Value::Array(a) if a.len() <= 40 => a,
        _ => return Err(ApiError::bad("fields is a list of up to 40")),
    };
    let mut names = HashSet::new();
    let mut out = Vec::new();
    for f in list {
        let name = name_of(&f["name"], "a field", 60)?;
        if !names.insert(name.to_lowercase()) || matches!(name.to_lowercase().as_str(), "name" | "tag" | "status" | "type" | "category" | "owner") {
            return Err(ApiError::bad(format!("the field name {name} is taken")));
        }
        let kind = f["kind"]
            .as_str()
            .filter(|k| FIELD_KINDS.contains(k))
            .ok_or_else(|| ApiError::bad(format!("a field's kind is one of: {}", FIELD_KINDS.join(", "))))?;
        let id = f["id"].as_str().filter(|i| i.parse::<Uuid>().is_ok()).map(str::to_string).unwrap_or_else(|| new_id().to_string());
        let mut field = json!({"id": id, "name": name, "kind": kind, "required": f["required"].as_bool().unwrap_or(false)});
        if kind == "select" {
            let options: Vec<Value> = f["options"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|o| {
                    let label = o["label"].as_str().or_else(|| o.as_str())?.trim();
                    (!label.is_empty()).then(|| json!({"id": o["id"].as_str().map(str::to_string).unwrap_or_else(|| new_id().to_string()), "label": label.chars().take(80).collect::<String>()}))
                })
                .take(100)
                .collect();
            if options.is_empty() {
                return Err(ApiError::bad(format!("{name}: a select field needs options")));
            }
            field["options"] = json!(options);
        }
        if kind == "asset"
            && let Some(t) = f["typeId"].as_str().filter(|t| t.parse::<Uuid>().is_ok())
        {
            field["typeId"] = json!(t);
        }
        out.push(field);
    }
    Ok(Value::Array(out))
}

fn prefix_of(v: &Value, name: &str) -> ApiResult<String> {
    let p = match v.as_str().map(str::trim).filter(|p| !p.is_empty()) {
        Some(p) => p.to_uppercase(),
        None => name.chars().filter(|c| c.is_ascii_alphabetic()).take(3).collect::<String>().to_uppercase(),
    };
    if p.is_empty() || p.len() > 8 || !p.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Err(ApiError::bad("a tag prefix is 1 to 8 letters and digits (LT gives LT-0001)"));
    }
    Ok(p)
}

/// `POST /asset-sets/{id}/types` (the set's admins) `{name, icon?, color?, tagPrefix?, fields?}`.
pub async fn create_type(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Path(set): Path<Uuid>,
    Json(req): Json<Value>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    on(&state, &user).await?;
    rank_in(&state, &user, set, ADMIN).await?;
    let name = name_of(&req["name"], "a type", 60)?;
    let id = new_id();
    sqlx::query("INSERT INTO asset_types (id, set_id, name, icon, color, tag_prefix, fields, position) VALUES ($1, $2, $3, $4, $5, $6, $7, (SELECT COALESCE(max(position), 0) + 1 FROM asset_types WHERE set_id = $2))")
        .bind(id)
        .bind(set)
        .bind(&name)
        .bind(req["icon"].as_str())
        .bind(req["color"].as_str())
        .bind(prefix_of(&req["tagPrefix"], &name)?)
        .bind(check_fields(&req["fields"])?)
        .execute(&state.db)
        .await
        .map_err(duplicate("a type"))?;
    Ok((StatusCode::CREATED, Json(json!({"id": id}))))
}

async fn type_set(state: &AppState, user: &User, id: Uuid, min: i32) -> ApiResult<Uuid> {
    let set: Uuid = sqlx::query_scalar("SELECT t.set_id FROM asset_types t JOIN asset_sets s ON s.id = t.set_id WHERE t.id = $1 AND s.workspace_id = $2")
        .bind(id)
        .bind(user.workspace_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(ApiError::NotFound("asset type"))?;
    rank_in(state, user, set, min).await?;
    Ok(set)
}

/// `PATCH /asset-types/{id}` (the set's admins).
pub async fn update_type(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(id): Path<Uuid>, Json(req): Json<Value>) -> ApiResult<StatusCode> {
    on(&state, &user).await?;
    type_set(&state, &user, id, ADMIN).await?;
    if !req["name"].is_null() {
        sqlx::query("UPDATE asset_types SET name = $2 WHERE id = $1")
            .bind(id)
            .bind(name_of(&req["name"], "a type", 60)?)
            .execute(&state.db)
            .await
            .map_err(duplicate("a type"))?;
    }
    for (key, col) in [("icon", "icon"), ("color", "color")] {
        if req.get(key).is_some() {
            sqlx::query(sqlx::AssertSqlSafe(format!("UPDATE asset_types SET {col} = $2 WHERE id = $1")))
                .bind(id)
                .bind(req[key].as_str())
                .execute(&state.db)
                .await?;
        }
    }
    if !req["tagPrefix"].is_null() {
        sqlx::query("UPDATE asset_types SET tag_prefix = $2 WHERE id = $1").bind(id).bind(prefix_of(&req["tagPrefix"], "")?).execute(&state.db).await?;
    }
    if req.get("fields").is_some() {
        sqlx::query("UPDATE asset_types SET fields = $2 WHERE id = $1").bind(id).bind(check_fields(&req["fields"])?).execute(&state.db).await?;
    }
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /asset-types/{id}` (the set's admins): only a type without assets.
pub async fn delete_type(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    on(&state, &user).await?;
    type_set(&state, &user, id, ADMIN).await?;
    let used: i64 = sqlx::query_scalar("SELECT count(*) FROM assets WHERE type_id = $1").bind(id).fetch_one(&state.db).await?;
    if used > 0 {
        return Err(ApiError::Conflict(format!("{used} assets have this type: change or delete them first")));
    }
    sqlx::query("DELETE FROM asset_types WHERE id = $1").bind(id).execute(&state.db).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /asset-sets/{id}/categories` (the set's admins) `{name, parentId?}`.
pub async fn create_category(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Path(set): Path<Uuid>,
    Json(req): Json<Value>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    on(&state, &user).await?;
    rank_in(&state, &user, set, ADMIN).await?;
    let parent: Option<Uuid> = match &req["parentId"] {
        Value::Null => None,
        v => {
            let p: Uuid = v.as_str().and_then(|s| s.parse().ok()).ok_or_else(|| ApiError::bad("parentId is a category's id"))?;
            let ok: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM asset_categories WHERE id = $1 AND set_id = $2)")
                .bind(p)
                .bind(set)
                .fetch_one(&state.db)
                .await?;
            if !ok {
                return Err(ApiError::bad("the parent is a category of this set"));
            }
            Some(p)
        }
    };
    let id = new_id();
    sqlx::query("INSERT INTO asset_categories (id, set_id, parent_id, name) VALUES ($1, $2, $3, $4)")
        .bind(id)
        .bind(set)
        .bind(parent)
        .bind(name_of(&req["name"], "a category", 80)?)
        .execute(&state.db)
        .await?;
    Ok((StatusCode::CREATED, Json(json!({"id": id}))))
}

/// `DELETE /asset-categories/{id}` (the set's admins): with the categories below it; their assets
/// have no category then.
pub async fn delete_category(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    on(&state, &user).await?;
    let set: Uuid = sqlx::query_scalar("SELECT c.set_id FROM asset_categories c JOIN asset_sets s ON s.id = c.set_id WHERE c.id = $1 AND s.workspace_id = $2")
        .bind(id)
        .bind(user.workspace_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(ApiError::NotFound("category"))?;
    rank_in(&state, &user, set, ADMIN).await?;
    sqlx::query("DELETE FROM asset_categories WHERE id = $1").bind(id).execute(&state.db).await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---- assets -------------------------------------------------------------------------------------------

#[derive(sqlx::FromRow, Clone)]
struct Row {
    id: Uuid,
    set_id: Uuid,
    type_id: Uuid,
    type_name: String,
    type_icon: Option<String>,
    type_color: Option<String>,
    category_id: Option<Uuid>,
    category_name: Option<String>,
    tag: String,
    name: String,
    status: String,
    owner_id: Option<Uuid>,
    owner_name: Option<String>,
    owner_team_id: Option<Uuid>,
    owner_team: Option<String>,
    owner_email: Option<String>,
    values: Value,
    fields: Value,
    updated_at: DateTime<Utc>,
}

const ROW_SQL: &str = "SELECT a.id, a.set_id, a.type_id, t.name AS type_name, t.icon AS type_icon, t.color AS type_color, a.category_id, c.name AS category_name, a.tag, a.name, a.status,
                              a.owner_id, u.name AS owner_name, a.owner_team_id, tm.name AS owner_team, a.owner_email, a.\"values\" AS values, t.fields, a.updated_at
                         FROM assets a JOIN asset_types t ON t.id = a.type_id LEFT JOIN asset_categories c ON c.id = a.category_id
                         LEFT JOIN users u ON u.id = a.owner_id LEFT JOIN teams tm ON tm.id = a.owner_team_id";

fn owner_json(r: &Row) -> Value {
    match (&r.owner_id, &r.owner_team_id, &r.owner_email) {
        (Some(id), _, _) => json!({"kind": "user", "id": id, "name": r.owner_name}),
        (_, Some(id), _) => json!({"kind": "team", "id": id, "name": r.owner_team}),
        (_, _, Some(e)) => json!({"kind": "email", "name": e}),
        _ => Value::Null,
    }
}

fn row_json(r: &Row) -> Value {
    json!({
        "id": r.id, "setId": r.set_id, "tag": r.tag, "name": r.name, "status": r.status,
        "type": {"id": r.type_id, "name": r.type_name, "icon": r.type_icon, "color": r.type_color},
        "category": r.category_id.map(|id| json!({"id": id, "name": r.category_name})),
        "owner": owner_json(r), "updatedAt": r.updated_at,
    })
}

/// An asset by id or tag, when the caller sees its set: (the row, their rank).
async fn find(state: &AppState, user: &User, id_or_tag: &str, min: i32) -> ApiResult<(Row, i32)> {
    let id = id_or_tag.parse::<Uuid>().ok();
    let row: Option<Row> = sqlx::query_as(sqlx::AssertSqlSafe(format!("{ROW_SQL} WHERE a.workspace_id = $1 AND (a.id = $2 OR upper(a.tag) = upper($3))")))
        .bind(user.workspace_id)
        .bind(id)
        .bind(id_or_tag.trim())
        .fetch_optional(&state.db)
        .await?;
    let row = row.ok_or(ApiError::NotFound("asset"))?;
    let rank = rank_in(state, user, row.set_id, min).await.map_err(|e| match e {
        ApiError::NotFound(_) => ApiError::NotFound("asset"),
        e => e,
    })?;
    Ok((row, rank))
}

#[derive(Deserialize, Default)]
pub struct ListQuery {
    pub set: Option<Uuid>,
    pub r#type: Option<Uuid>,
    pub category: Option<Uuid>,
    pub status: Option<String>,
    /// a person's id, or `me`
    pub owner: Option<String>,
    pub q: Option<String>,
    pub limit: Option<i64>,
}

/// `GET /assets`: the assets the caller sees, filtered.
pub async fn list(State(state): State<AppState>, CurrentUser(user): CurrentUser, Query(q): Query<ListQuery>) -> ApiResult<Json<Value>> {
    on(&state, &user).await?;
    let visible: Vec<Uuid> = visible_sets(&state, &user).await?.into_keys().collect();
    let owner: Option<Uuid> = match q.owner.as_deref() {
        Some("me") => Some(user.id),
        Some(o) => Some(o.parse().map_err(|_| ApiError::bad("owner is a person's id or me"))?),
        None => None,
    };
    let text = q.q.as_deref().map(str::trim).filter(|t| !t.is_empty()).map(|t| format!("%{}%", t.to_lowercase().replace('%', "\\%").replace('_', "\\_")));
    let limit = q.limit.unwrap_or(200).clamp(1, 1000);
    let rows: Vec<Row> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "{ROW_SQL} WHERE a.set_id = ANY($1) AND ($2::uuid IS NULL OR a.set_id = $2) AND ($3::uuid IS NULL OR a.type_id = $3)
            AND ($4::uuid IS NULL OR a.category_id IN (WITH RECURSIVE sub AS (SELECT $4::uuid AS id UNION ALL SELECT c2.id FROM asset_categories c2 JOIN sub ON c2.parent_id = sub.id) SELECT id FROM sub))
            AND ($5::text IS NULL OR a.status = $5) AND ($6::uuid IS NULL OR a.owner_id = $6)
            AND ($7::text IS NULL OR lower(a.name) LIKE $7 OR lower(a.tag) LIKE $7 OR lower(a.\"values\"::text) LIKE $7)
          ORDER BY upper(a.tag) LIMIT $8"
    )))
    .bind(&visible)
    .bind(q.set)
    .bind(q.r#type)
    .bind(q.category)
    .bind(q.status.as_deref().filter(|s| !s.is_empty()))
    .bind(owner)
    .bind(&text)
    .bind(limit + 1)
    .fetch_all(&state.db)
    .await?;
    let more = rows.len() as i64 > limit;
    Ok(Json(json!({"assets": rows.iter().take(limit as usize).map(row_json).collect::<Vec<_>>(), "more": more})))
}

/// A value for a field of a type (by the field's kind); null clears it.
async fn field_value(state: &AppState, workspace: Uuid, f: &Value, v: &Value) -> ApiResult<Value> {
    let name = f["name"].as_str().unwrap_or("the field");
    let err = |what: &str| ApiError::bad(format!("{name} takes {what}"));
    if v.is_null() || v.as_str() == Some("") {
        return Ok(Value::Null);
    }
    Ok(match f["kind"].as_str().unwrap_or("text") {
        "number" => json!(
            v.as_f64().or_else(|| v.as_str().and_then(|s| s.trim().replace(',', ".").parse().ok())).filter(|n| n.is_finite()).ok_or_else(|| err("a number"))?
        ),
        "date" => {
            let s = v.as_str().ok_or_else(|| err("a date (YYYY-MM-DD)"))?;
            json!(NaiveDate::parse_from_str(s.get(..10).unwrap_or(s), "%Y-%m-%d").map_err(|_| err("a date (YYYY-MM-DD)"))?.to_string())
        }
        "checkbox" => match v {
            Value::Bool(b) => json!(b),
            Value::String(s) if matches!(s.to_lowercase().as_str(), "true" | "yes" | "1" | "x") => json!(true),
            Value::String(s) if matches!(s.to_lowercase().as_str(), "false" | "no" | "0") => json!(false),
            _ => return Err(err("true or false")),
        },
        "url" => {
            let s = v.as_str().ok_or_else(|| err("a web address"))?.trim();
            if !(s.starts_with("https://") || s.starts_with("http://")) {
                return Err(err("a web address (https://…)"));
            }
            json!(s)
        }
        "select" => {
            // an option by its id or its label
            let s = v.as_str().ok_or_else(|| err("one of its options"))?.trim();
            let found = f["options"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|o| o["id"].as_str() == Some(s) || o["label"].as_str().is_some_and(|l| l.eq_ignore_ascii_case(s)));
            found.ok_or_else(|| err("one of its options"))?["id"].clone()
        }
        "user" => {
            let s = v.as_str().ok_or_else(|| err("a person"))?.trim();
            let id: Option<Uuid> = sqlx::query_scalar(
                "SELECT id FROM users WHERE workspace_id = $1 AND (id::text = $2 OR lower(email) = lower($2) OR lower(name) = lower($2)) LIMIT 1",
            )
            .bind(workspace)
            .bind(s)
            .fetch_optional(&state.db)
            .await?;
            json!(id.ok_or_else(|| err("a person of the workspace"))?)
        }
        "asset" => {
            // another asset by its id or tag (of the field's type, when it names one)
            let s = v.as_str().ok_or_else(|| err("an asset (its tag)"))?.trim();
            let found: Option<(Uuid, Uuid)> =
                sqlx::query_as("SELECT id, type_id FROM assets WHERE workspace_id = $1 AND (id::text = $2 OR upper(tag) = upper($2))")
                    .bind(workspace)
                    .bind(s)
                    .fetch_optional(&state.db)
                    .await?;
            let (id, kind) = found.ok_or_else(|| err("an asset (its tag)"))?;
            if f["typeId"].as_str().is_some_and(|t| t != kind.to_string()) {
                return Err(err("an asset of the field's type"));
            }
            json!(id)
        }
        _ => json!(v.as_str().ok_or_else(|| err("text"))?.chars().take(2000).collect::<String>()),
    })
}

/// The values a request gives (by field id or name), checked; with `all`, required ones must be there.
async fn check_values(
    state: &AppState,
    workspace: Uuid,
    fields: &Value,
    input: &Value,
    mut current: Map<String, Value>,
    all: bool,
) -> ApiResult<Map<String, Value>> {
    let defs = fields.as_array().cloned().unwrap_or_default();
    for (key, v) in input.as_object().into_iter().flatten() {
        let def = defs
            .iter()
            .find(|f| f["id"].as_str() == Some(key) || f["name"].as_str().is_some_and(|n| n.eq_ignore_ascii_case(key)))
            .ok_or_else(|| ApiError::bad(format!("the type has no field {key}")))?;
        let id = def["id"].as_str().unwrap_or_default().to_string();
        match field_value(state, workspace, def, v).await? {
            Value::Null => {
                current.remove(&id);
            }
            value => {
                current.insert(id, value);
            }
        }
    }
    // values of fields the type no longer has go
    current.retain(|k, _| defs.iter().any(|f| f["id"].as_str() == Some(k)));
    if all && let Some(missing) = defs.iter().find(|f| f["required"] == true && !current.contains_key(f["id"].as_str().unwrap_or_default())) {
        return Err(ApiError::bad(format!("{} is required", missing["name"].as_str().unwrap_or("a field"))));
    }
    Ok(current)
}

/// The owner a request names: `ownerId` (a person), `ownerTeamId`, `ownerEmail` — one of them.
async fn owner_of(state: &AppState, workspace: Uuid, req: &Value) -> ApiResult<(Option<Uuid>, Option<Uuid>, Option<String>)> {
    let id_in = |v: &Value| v.as_str().and_then(|s| s.parse::<Uuid>().ok());
    if let Some(u) = id_in(&req["ownerId"]) {
        let ok: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM users WHERE id = $1 AND workspace_id = $2)").bind(u).bind(workspace).fetch_one(&state.db).await?;
        return if ok { Ok((Some(u), None, None)) } else { Err(ApiError::bad("the owner isn't a person of this workspace")) };
    }
    if let Some(t) = id_in(&req["ownerTeamId"]) {
        let ok: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM teams WHERE id = $1 AND workspace_id = $2)").bind(t).bind(workspace).fetch_one(&state.db).await?;
        return if ok { Ok((None, Some(t), None)) } else { Err(ApiError::bad("the owner isn't a team of this workspace")) };
    }
    if let Some(e) = req["ownerEmail"].as_str().map(str::trim).filter(|e| !e.is_empty()) {
        if !e.contains('@') || e.len() > 254 {
            return Err(ApiError::bad("ownerEmail is an e-mail address"));
        }
        // someone with an account owns it as themselves
        let known: Option<Uuid> = sqlx::query_scalar("SELECT id FROM users WHERE workspace_id = $1 AND lower(email) = lower($2)")
            .bind(workspace)
            .bind(e)
            .fetch_optional(&state.db)
            .await?;
        return Ok(match known {
            Some(u) => (Some(u), None, None),
            None => (None, None, Some(e.to_lowercase())),
        });
    }
    Ok((None, None, None))
}

async fn category_in(state: &AppState, set: Uuid, v: &Value) -> ApiResult<Option<Uuid>> {
    match v {
        Value::Null => Ok(None),
        v => {
            let s = v.as_str().unwrap_or("").trim();
            let found: Option<Uuid> =
                sqlx::query_scalar("SELECT id FROM asset_categories WHERE set_id = $1 AND (id::text = $2 OR lower(name) = lower($2)) LIMIT 1")
                    .bind(set)
                    .bind(s)
                    .fetch_optional(&state.db)
                    .await?;
            found.map(Some).ok_or_else(|| ApiError::bad(format!("the set has no category {s}")))
        }
    }
}

fn status_in(v: &Value) -> ApiResult<Option<String>> {
    match v.as_str().map(|s| s.trim().to_lowercase().replace([' ', '-'], "_")) {
        None => Ok(None),
        Some(s) if s.is_empty() => Ok(None),
        Some(s) if STATUSES.contains(&s.as_str()) => Ok(Some(s)),
        Some(s) => Err(ApiError::bad(format!("status is one of: {} (not {s})", STATUSES.join(", ")))),
    }
}

async fn event(state: &AppState, asset: Uuid, actor: Option<Uuid>, kind: &str, data: Value) {
    let _ = sqlx::query("INSERT INTO asset_events (id, asset_id, actor_id, kind, data) VALUES ($1, $2, $3, $4, $5)")
        .bind(new_id())
        .bind(asset)
        .bind(actor)
        .bind(kind)
        .bind(data)
        .execute(&state.db)
        .await;
}

/// Creates an asset (permission checked by the caller); its id and tag.
async fn insert(state: &AppState, workspace: Uuid, actor: Option<Uuid>, type_id: Uuid, req: &Value) -> ApiResult<(Uuid, String)> {
    let (set, fields): (Uuid, Value) = sqlx::query_as("SELECT set_id, fields FROM asset_types WHERE id = $1").bind(type_id).fetch_one(&state.db).await?;
    let name = name_of(&req["name"], "an asset", 200)?;
    let values = check_values(state, workspace, &fields, &req["values"], Map::new(), true).await?;
    let (owner, team, email) = owner_of(state, workspace, req).await?;
    let category = category_in(state, set, &req["categoryId"]).await?;
    let status = status_in(&req["status"])?.unwrap_or_else(|| "in_use".into());
    let id = new_id();
    let mut tx = state.db.begin().await?;
    let tag = match req["tag"].as_str().map(str::trim).filter(|t| !t.is_empty()) {
        Some(t) if t.len() <= 40 && t.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')) => t.to_uppercase(),
        Some(_) => return Err(ApiError::bad("a tag is up to 40 letters, digits, - _ and .")),
        None => {
            // the type's next number that is free
            loop {
                let (prefix, n): (String, i32) =
                    sqlx::query_as("UPDATE asset_types SET next_number = next_number + 1 WHERE id = $1 RETURNING tag_prefix, next_number - 1")
                        .bind(type_id)
                        .fetch_one(&mut *tx)
                        .await?;
                let tag = format!("{prefix}-{n:04}");
                let taken: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM assets WHERE workspace_id = $1 AND upper(tag) = $2)")
                    .bind(workspace)
                    .bind(&tag)
                    .fetch_one(&mut *tx)
                    .await?;
                if !taken {
                    break tag;
                }
            }
        }
    };
    sqlx::query("INSERT INTO assets (id, workspace_id, set_id, type_id, category_id, tag, name, status, owner_id, owner_team_id, owner_email, \"values\", created_by) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)")
        .bind(id)
        .bind(workspace)
        .bind(set)
        .bind(type_id)
        .bind(category)
        .bind(&tag)
        .bind(&name)
        .bind(&status)
        .bind(owner)
        .bind(team)
        .bind(&email)
        .bind(Value::Object(values))
        .bind(actor)
        .execute(&mut *tx)
        .await
        .map_err(|e| if e.as_database_error().is_some_and(|d| d.is_unique_violation()) { ApiError::Conflict(format!("the tag {tag} is taken")) } else { e.into() })?;
    // a tag given by hand that has the type's form moves the counter past it (LT-0042 → next LT-0043)
    sqlx::query(
        "UPDATE asset_types SET next_number = GREATEST(next_number, substring($2 from '-(\\d{1,8})$')::int + 1)
          WHERE id = $1 AND upper($2) ~ ('^' || upper(tag_prefix) || '-\\d{1,8}$')",
    )
    .bind(type_id)
    .bind(&tag)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    event(state, id, actor, "created", json!({"name": name})).await;
    Ok((id, tag))
}

/// `POST /assets` (the set's editors) `{typeId, name, tag?, status?, categoryId?, ownerId? | ownerTeamId? | ownerEmail?, values?}`.
pub async fn create(State(state): State<AppState>, CurrentUser(user): CurrentUser, Json(req): Json<Value>) -> ApiResult<(StatusCode, Json<Value>)> {
    on(&state, &user).await?;
    let type_id: Uuid = req["typeId"].as_str().and_then(|s| s.parse().ok()).ok_or_else(|| ApiError::bad("typeId says what kind of asset it is"))?;
    let set = type_set(&state, &user, type_id, EDITOR).await?;
    let (id, tag) = insert(&state, user.workspace_id, Some(user.id), type_id, &req).await?;
    run_rules(&state, set, id, "created").await;
    Ok((StatusCode::CREATED, Json(json!({"id": id, "tag": tag}))))
}

/// The values with what they point at spelled out: `[{id, name, kind, value, label}]`.
async fn values_json(state: &AppState, r: &Row) -> ApiResult<Vec<Value>> {
    let mut out = Vec::new();
    for f in r.fields.as_array().into_iter().flatten() {
        let id = f["id"].as_str().unwrap_or_default();
        let v = r.values.get(id).cloned().unwrap_or(Value::Null);
        let label: Value = match (f["kind"].as_str(), v.as_str()) {
            (Some("select"), Some(s)) => {
                f["options"].as_array().into_iter().flatten().find(|o| o["id"].as_str() == Some(s)).map(|o| o["label"].clone()).unwrap_or(Value::Null)
            }
            (Some("user"), Some(s)) => {
                json!(sqlx::query_scalar::<_, String>("SELECT name FROM users WHERE id::text = $1").bind(s).fetch_optional(&state.db).await?)
            }
            (Some("asset"), Some(s)) => {
                json!(sqlx::query_scalar::<_, String>("SELECT tag || ' ' || name FROM assets WHERE id::text = $1").bind(s).fetch_optional(&state.db).await?)
            }
            _ => Value::Null,
        };
        out.push(json!({"id": id, "name": f["name"], "kind": f["kind"], "required": f["required"], "options": f["options"], "typeId": f["typeId"], "value": v, "label": label}));
    }
    Ok(out)
}

/// `GET /assets/{id or tag}`: the asset with its fields, relations (both ways), the issues about
/// it that the caller sees, and its history.
pub async fn get(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    on(&state, &user).await?;
    let (r, rank) = find(&state, &user, &id, VIEWER).await?;
    let visible: Vec<Uuid> = visible_sets(&state, &user).await?.into_keys().collect();
    let relations: Vec<(Uuid, String, bool, Uuid, String, String, String)> = sqlx::query_as(
        "SELECT r.id, r.kind, r.source_id = $1, o.id, o.tag, o.name, o.status FROM asset_relations r JOIN assets o ON o.id = CASE WHEN r.source_id = $1 THEN r.target_id ELSE r.source_id END
          WHERE (r.source_id = $1 OR r.target_id = $1) AND o.set_id = ANY($2) ORDER BY r.kind, upper(o.tag)",
    )
    .bind(r.id)
    .bind(&visible)
    .fetch_all(&state.db)
    .await?;
    let issue_ids: Vec<Uuid> = sqlx::query_scalar("SELECT ia.issue_id FROM issue_assets ia JOIN issues i ON i.id = ia.issue_id WHERE ia.asset_id = $1 AND i.deleted_at IS NULL ORDER BY i.created_at DESC LIMIT 200").bind(r.id).fetch_all(&state.db).await?;
    let issues = crate::issues::load_many_for(&state, user.id, &issue_ids).await?;
    let events: Vec<(DateTime<Utc>, Option<String>, String, Value)> =
        sqlx::query_as("SELECT e.at, u.name, e.kind, e.data FROM asset_events e LEFT JOIN users u ON u.id = e.actor_id WHERE e.asset_id = $1 ORDER BY e.at DESC, e.id DESC LIMIT 100").bind(r.id).fetch_all(&state.db).await?;
    let mut v = row_json(&r);
    v["fields"] = json!(values_json(&state, &r).await?);
    v["role"] = json!(rank_name(rank));
    // outward: what this asset depends on / is installed in; inward: what depends on it / is installed in it
    v["relations"] = json!(relations.iter().map(|(rid, kind, outward, oid, tag, name, status)| json!({"id": rid, "kind": kind, "outward": outward, "asset": {"id": oid, "tag": tag, "name": name, "status": status}})).collect::<Vec<_>>());
    v["issues"] = json!(issues);
    v["history"] = json!(events.iter().map(|(at, who, kind, data)| json!({"at": at, "actor": who, "kind": kind, "data": data})).collect::<Vec<_>>());
    Ok(Json(v))
}

/// `PATCH /assets/{id}` (the set's editors): any of the fields of creating one, but the type.
pub async fn update(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(id): Path<String>, Json(req): Json<Value>) -> ApiResult<StatusCode> {
    on(&state, &user).await?;
    let (r, _) = find(&state, &user, &id, EDITOR).await?;
    apply_update(&state, user.workspace_id, Some(user.id), &r, &req).await?;
    run_rules(&state, r.set_id, r.id, "changed").await;
    Ok(StatusCode::NO_CONTENT)
}

async fn apply_update(state: &AppState, workspace: Uuid, actor: Option<Uuid>, r: &Row, req: &Value) -> ApiResult<()> {
    let mut changed = Map::new();
    if !req["name"].is_null() {
        let name = name_of(&req["name"], "an asset", 200)?;
        if name != r.name {
            sqlx::query("UPDATE assets SET name = $2 WHERE id = $1").bind(r.id).bind(&name).execute(&state.db).await?;
            changed.insert("name".into(), json!(name));
        }
    }
    if let Some(s) = status_in(&req["status"])?.filter(|s| *s != r.status) {
        sqlx::query("UPDATE assets SET status = $2 WHERE id = $1").bind(r.id).bind(&s).execute(&state.db).await?;
        changed.insert("status".into(), json!({"from": r.status, "to": s}));
    }
    if req.get("categoryId").is_some() {
        let c = category_in(state, r.set_id, &req["categoryId"]).await?;
        if c != r.category_id {
            sqlx::query("UPDATE assets SET category_id = $2 WHERE id = $1").bind(r.id).bind(c).execute(&state.db).await?;
            changed.insert("category".into(), json!(true));
        }
    }
    if req.get("ownerId").is_some() || req.get("ownerTeamId").is_some() || req.get("ownerEmail").is_some() {
        let (owner, team, email) = owner_of(state, workspace, req).await?;
        if (owner, team, &email) != (r.owner_id, r.owner_team_id, &r.owner_email) {
            sqlx::query("UPDATE assets SET owner_id = $2, owner_team_id = $3, owner_email = $4 WHERE id = $1")
                .bind(r.id)
                .bind(owner)
                .bind(team)
                .bind(&email)
                .execute(&state.db)
                .await?;
            let name: Option<String> =
                sqlx::query_scalar("SELECT COALESCE((SELECT name FROM users WHERE id = $1), (SELECT name FROM teams WHERE id = $2), $3)")
                    .bind(owner)
                    .bind(team)
                    .bind(&email)
                    .fetch_one(&state.db)
                    .await?;
            changed.insert("owner".into(), json!(name));
        }
    }
    if req["values"].is_object() {
        let before = r.values.as_object().cloned().unwrap_or_default();
        let after = check_values(state, workspace, &r.fields, &req["values"], before.clone(), false).await?;
        if after != before {
            let names: Vec<&str> = r
                .fields
                .as_array()
                .into_iter()
                .flatten()
                .filter(|f| before.get(f["id"].as_str().unwrap_or_default()) != after.get(f["id"].as_str().unwrap_or_default()))
                .filter_map(|f| f["name"].as_str())
                .collect();
            sqlx::query("UPDATE assets SET \"values\" = $2 WHERE id = $1").bind(r.id).bind(Value::Object(after.clone())).execute(&state.db).await?;
            changed.insert("fields".into(), json!(names));
        }
    }
    if !changed.is_empty() {
        sqlx::query("UPDATE assets SET updated_at = now() WHERE id = $1").bind(r.id).execute(&state.db).await?;
        event(state, r.id, actor, "changed", Value::Object(changed)).await;
    }
    Ok(())
}

/// `DELETE /assets/{id}` (the set's admins).
pub async fn delete(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(id): Path<String>) -> ApiResult<StatusCode> {
    on(&state, &user).await?;
    let (r, _) = find(&state, &user, &id, ADMIN).await?;
    sqlx::query("DELETE FROM assets WHERE id = $1").bind(r.id).execute(&state.db).await?;
    audit(&state, user.workspace_id, Some(user.id), "assets.deleted", Some(r.tag), json!({"name": r.name})).await;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /assets/{id}/relations` (editors of this asset's set) `{kind: depends_on | installed_in, target}`:
/// this asset depends on / is installed in the target (an id or tag the caller sees).
pub async fn relate(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<String>,
    Json(req): Json<Value>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    on(&state, &user).await?;
    let (r, _) = find(&state, &user, &id, EDITOR).await?;
    let kind = req["kind"].as_str().filter(|k| RELATIONS.contains(k)).ok_or_else(|| ApiError::bad("kind is depends_on or installed_in"))?;
    let (target, _) = find(&state, &user, req["target"].as_str().unwrap_or(""), VIEWER).await?;
    if target.id == r.id {
        return Err(ApiError::bad("an asset doesn't relate to itself"));
    }
    let rid = new_id();
    let made = sqlx::query("INSERT INTO asset_relations (id, source_id, target_id, kind) VALUES ($1, $2, $3, $4) ON CONFLICT DO NOTHING")
        .bind(rid)
        .bind(r.id)
        .bind(target.id)
        .bind(kind)
        .execute(&state.db)
        .await?;
    if made.rows_affected() == 0 {
        return Err(ApiError::Conflict("they're related like that already".into()));
    }
    event(&state, r.id, Some(user.id), "related", json!({"kind": kind, "tag": target.tag, "name": target.name})).await;
    Ok((StatusCode::CREATED, Json(json!({"id": rid}))))
}

/// `DELETE /asset-relations/{id}` (editors of the source's set).
pub async fn unrelate(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    on(&state, &user).await?;
    let row: Option<(Uuid, String, String)> =
        sqlx::query_as("SELECT r.source_id, r.kind, t.tag FROM asset_relations r JOIN assets t ON t.id = r.target_id WHERE r.id = $1")
            .bind(id)
            .fetch_optional(&state.db)
            .await?;
    let (source, kind, tag) = row.ok_or(ApiError::NotFound("relation"))?;
    find(&state, &user, &source.to_string(), EDITOR).await?;
    sqlx::query("DELETE FROM asset_relations WHERE id = $1").bind(id).execute(&state.db).await?;
    event(&state, source, Some(user.id), "unrelated", json!({"kind": kind, "tag": tag})).await;
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /assets/{id}/graph`: the asset with what it depends on and what depends on it, three
/// steps out — when something fails, you see what's affected.
pub async fn graph(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    on(&state, &user).await?;
    let (r, _) = find(&state, &user, &id, VIEWER).await?;
    let visible: Vec<Uuid> = visible_sets(&state, &user).await?.into_keys().collect();
    let edges: Vec<(Uuid, Uuid, String)> = sqlx::query_as(
        "WITH RECURSIVE near AS (
             SELECT $1::uuid AS id, 0 AS depth
             UNION SELECT CASE WHEN r.source_id = near.id THEN r.target_id ELSE r.source_id END, near.depth + 1
               FROM asset_relations r JOIN near ON near.id IN (r.source_id, r.target_id) WHERE near.depth < 3)
         SELECT DISTINCT r.source_id, r.target_id, r.kind FROM asset_relations r
          WHERE r.source_id IN (SELECT id FROM near) AND r.target_id IN (SELECT id FROM near) LIMIT 500",
    )
    .bind(r.id)
    .fetch_all(&state.db)
    .await?;
    let mut ids: Vec<Uuid> = edges.iter().flat_map(|e| [e.0, e.1]).collect();
    ids.push(r.id);
    ids.sort();
    ids.dedup();
    let nodes: Vec<Row> = sqlx::query_as(sqlx::AssertSqlSafe(format!("{ROW_SQL} WHERE a.id = ANY($1) AND a.set_id = ANY($2)")))
        .bind(&ids)
        .bind(&visible)
        .fetch_all(&state.db)
        .await?;
    let seen: HashSet<Uuid> = nodes.iter().map(|n| n.id).collect();
    // what fails with it: everything that depends on it or is installed in it, directly or through others
    let affected: Vec<Uuid> = sqlx::query_scalar(
        "WITH RECURSIVE up AS (SELECT source_id AS id FROM asset_relations WHERE target_id = $1 UNION SELECT r.source_id FROM asset_relations r JOIN up ON r.target_id = up.id) SELECT id FROM up LIMIT 500",
    )
    .bind(r.id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(json!({
        "root": r.id,
        "nodes": nodes.iter().map(row_json).collect::<Vec<_>>(),
        "edges": edges.iter().filter(|e| seen.contains(&e.0) && seen.contains(&e.1)).map(|(s, t, k)| json!({"source": s, "target": t, "kind": k})).collect::<Vec<_>>(),
        "affected": affected.into_iter().filter(|a| seen.contains(a)).collect::<Vec<_>>(),
    })))
}

// ---- issues -------------------------------------------------------------------------------------------

/// The assets an issue is about, as the caller may see them.
pub async fn of_issue(state: &AppState, user: &User, issue: Uuid) -> ApiResult<Vec<Value>> {
    if !crate::features::on(state, user.workspace_id, "assets").await {
        return Ok(vec![]);
    }
    let visible: Vec<Uuid> = visible_sets(state, user).await?.into_keys().collect();
    let rows: Vec<Row> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "{ROW_SQL} JOIN issue_assets ia ON ia.asset_id = a.id WHERE ia.issue_id = $1 AND a.set_id = ANY($2) ORDER BY upper(a.tag)"
    )))
    .bind(issue)
    .bind(&visible)
    .fetch_all(&state.db)
    .await?;
    Ok(rows.iter().map(row_json).collect())
}

/// `PUT /issues/{key}/assets` (project members) `{assets: [id or tag, …]}`: the assets the issue
/// is about (of the ones the caller sees; others it has stay).
pub async fn set_for_issue(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Path(key): Path<String>,
    Json(req): Json<Value>,
) -> ApiResult<Json<Value>> {
    on(&state, &user).await?;
    let (issue, project, _) = crate::routes::issues::issue_for(&state, &user, &key, Role::Member).await?;
    let mut wanted: Vec<Row> = Vec::new();
    for a in req["assets"].as_array().ok_or_else(|| ApiError::bad("assets is a list of ids or tags"))?.iter().take(50) {
        let (r, _) = find(&state, &user, a.as_str().unwrap_or(""), VIEWER).await?;
        if !wanted.iter().any(|w| w.id == r.id) {
            wanted.push(r);
        }
    }
    link_issue(&state, &user, issue, project, &key, &wanted, true).await?;
    Ok(Json(json!(of_issue(&state, &user, issue).await?)))
}

/// Sets (or, without `replace`, adds to) an issue's assets, with history on both sides.
async fn link_issue(state: &AppState, user: &User, issue: Uuid, project: Uuid, key: &str, wanted: &[Row], replace: bool) -> ApiResult<()> {
    let visible: Vec<Uuid> = visible_sets(state, user).await?.into_keys().collect();
    let current: Vec<(Uuid, String)> =
        sqlx::query_as("SELECT a.id, a.tag FROM issue_assets ia JOIN assets a ON a.id = ia.asset_id WHERE ia.issue_id = $1 AND a.set_id = ANY($2)")
            .bind(issue)
            .bind(&visible)
            .fetch_all(&state.db)
            .await?;
    let (mut added, mut removed) = (Vec::new(), Vec::new());
    let mut tx = state.db.begin().await?;
    for w in wanted.iter().filter(|w| !current.iter().any(|c| c.0 == w.id)) {
        sqlx::query("INSERT INTO issue_assets (issue_id, asset_id) VALUES ($1, $2) ON CONFLICT DO NOTHING").bind(issue).bind(w.id).execute(&mut *tx).await?;
        added.push(w.tag.clone());
    }
    if replace {
        for (id, tag) in current.iter().filter(|c| !wanted.iter().any(|w| w.id == c.0)) {
            sqlx::query("DELETE FROM issue_assets WHERE issue_id = $1 AND asset_id = $2").bind(issue).bind(id).execute(&mut *tx).await?;
            removed.push(tag.clone());
        }
    }
    if !added.is_empty() || !removed.is_empty() {
        let c = crate::issues::Change { field: "assets", old: json!(removed), new: json!(added) };
        crate::issues::record(&mut tx, issue, project, Some(user.id), "assets", Some(&c)).await?;
    }
    tx.commit().await?;
    for w in wanted.iter().filter(|w| added.contains(&w.tag)) {
        event(state, w.id, Some(user.id), "issue", json!({"key": key.to_uppercase(), "linked": true})).await;
    }
    if !added.is_empty() || !removed.is_empty() {
        crate::events::publish(state, crate::events::Event::new(user.workspace_id, "issue").project(project).actor(user.id)).await;
    }
    Ok(())
}

/// The assets a requester owns (by their address): what the portal's request form offers.
pub async fn of_requester(state: &AppState, workspace: Uuid, email: &str) -> ApiResult<Vec<Value>> {
    if !crate::features::on(state, workspace, "assets").await {
        return Ok(vec![]);
    }
    let rows: Vec<(Uuid, String, String, String)> = sqlx::query_as(
        "SELECT a.id, a.tag, a.name, t.name FROM assets a JOIN asset_types t ON t.id = a.type_id
          WHERE a.workspace_id = $1 AND a.status <> 'retired' AND (lower(a.owner_email) = lower($2) OR a.owner_id IN (SELECT id FROM users WHERE lower(email) = lower($2)))
          ORDER BY upper(a.tag) LIMIT 100",
    )
    .bind(workspace)
    .bind(email)
    .fetch_all(&state.db)
    .await?;
    Ok(rows.into_iter().map(|(id, tag, name, kind)| json!({"id": id, "tag": tag, "name": name, "type": kind})).collect())
}

/// A new request names assets of its requester's: linked when they are theirs, ignored otherwise.
pub async fn link_request(state: &AppState, workspace: Uuid, issue: Uuid, email: &str, ids: &[String]) -> ApiResult<()> {
    let mine = of_requester(state, workspace, email).await?;
    for id in ids.iter().take(20) {
        if let Some(a) = mine.iter().find(|a| a["id"].as_str() == Some(id)) {
            let asset: Uuid = id.parse().map_err(|_| ApiError::bad("an asset's id"))?;
            sqlx::query("INSERT INTO issue_assets (issue_id, asset_id) VALUES ($1, $2) ON CONFLICT DO NOTHING")
                .bind(issue)
                .bind(asset)
                .execute(&state.db)
                .await?;
            event(state, asset, None, "issue", json!({"linked": true, "by": email, "tag": a["tag"]})).await;
        }
    }
    Ok(())
}

// ---- CSV ----------------------------------------------------------------------------------------------

fn csv_safe(t: &str) -> String {
    if t.starts_with(['=', '+', '-', '@']) { format!("'{t}") } else { t.to_string() }
}

/// `GET /asset-sets/{id}/export.csv`: every asset of the set with its fields.
pub async fn export(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(set): Path<Uuid>) -> ApiResult<Response> {
    on(&state, &user).await?;
    rank_in(&state, &user, set, VIEWER).await?;
    let rows: Vec<Row> = sqlx::query_as(sqlx::AssertSqlSafe(format!("{ROW_SQL} WHERE a.set_id = $1 ORDER BY t.position, upper(a.tag) LIMIT 50000")))
        .bind(set)
        .fetch_all(&state.db)
        .await?;
    // one column per field name across the set's types
    let mut columns: Vec<String> = Vec::new();
    for r in &rows {
        for f in r.fields.as_array().into_iter().flatten() {
            if let Some(n) = f["name"].as_str().filter(|n| !columns.iter().any(|c| c.eq_ignore_ascii_case(n))) {
                columns.push(n.to_string());
            }
        }
    }
    let mut w = csv::Writer::from_writer(Vec::new());
    let mut head = vec!["Tag".to_string(), "Name".into(), "Type".into(), "Status".into(), "Category".into(), "Owner".into()];
    head.extend(columns.iter().cloned());
    let _ = w.write_record(&head);
    for r in &rows {
        let values = values_json(&state, r).await?;
        let owner = r.owner_name.clone().or_else(|| r.owner_team.clone()).or_else(|| r.owner_email.clone()).unwrap_or_default();
        let mut rec =
            vec![r.tag.clone(), csv_safe(&r.name), r.type_name.clone(), r.status.clone(), r.category_name.clone().unwrap_or_default(), csv_safe(&owner)];
        for c in &columns {
            let cell = values.iter().find(|v| v["name"].as_str().is_some_and(|n| n.eq_ignore_ascii_case(c))).map(|v| match (&v["label"], &v["value"]) {
                (Value::String(l), _) => l.clone(),
                (_, Value::String(s)) => s.clone(),
                (_, Value::Null) => String::new(),
                (_, other) => other.to_string(),
            });
            rec.push(csv_safe(&cell.unwrap_or_default()));
        }
        let _ = w.write_record(&rec);
    }
    let body = w.into_inner().map_err(|e| ApiError::Internal(anyhow::anyhow!("csv: {e}")))?;
    Ok(([(header::CONTENT_TYPE, "text/csv; charset=utf-8"), (header::CONTENT_DISPOSITION, "attachment; filename=\"assets.csv\"")], body).into_response())
}

/// `POST /asset-types/{id}/import` (the set's editors) `{csv, mapping?: {column: name | tag | status
/// | category | owner | <field name or id> | ""}, dryRun?}`: one asset per row; a row whose tag
/// exists updates that asset. Without a mapping, columns are matched by their headers.
pub async fn import(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Path(type_id): Path<Uuid>,
    Json(req): Json<Value>,
) -> ApiResult<Json<Value>> {
    on(&state, &user).await?;
    let set = type_set(&state, &user, type_id, EDITOR).await?;
    let fields: Value = sqlx::query_scalar("SELECT fields FROM asset_types WHERE id = $1").bind(type_id).fetch_one(&state.db).await?;
    let text = req["csv"].as_str().ok_or_else(|| ApiError::bad("csv is the file's text"))?;
    if text.len() > 5 * 1024 * 1024 {
        return Err(ApiError::TooLarge);
    }
    let dry = req["dryRun"].as_bool().unwrap_or(false);
    let mut reader = csv::ReaderBuilder::new().flexible(true).from_reader(text.as_bytes());
    let headers: Vec<String> = reader
        .headers()
        .map_err(|e| ApiError::bad(format!("the CSV can't be read: {e}")))?
        .iter()
        .map(|h| h.trim().trim_start_matches('\u{feff}').to_string())
        .collect();
    // where each column goes
    let target = |h: &str| -> Option<String> {
        if let Some(m) = req["mapping"].as_object() {
            return m.get(h).and_then(Value::as_str).filter(|t| !t.is_empty()).map(str::to_string);
        }
        let l = h.to_lowercase();
        if matches!(l.as_str(), "name" | "tag" | "status" | "category" | "owner") {
            return Some(l);
        }
        fields
            .as_array()
            .into_iter()
            .flatten()
            .find(|f| f["name"].as_str().is_some_and(|n| n.eq_ignore_ascii_case(h)))
            .and_then(|f| f["name"].as_str())
            .map(str::to_string)
    };
    let columns: Vec<Option<String>> = headers.iter().map(|h| target(h)).collect();
    if !columns.iter().flatten().any(|c| c == "name") {
        return Err(ApiError::bad("one column must be the asset's name"));
    }
    let (mut created, mut updated, mut errors) = (0, 0, Vec::new());
    for (n, rec) in reader.records().enumerate() {
        if n >= 5000 {
            errors.push(json!({"row": n + 2, "message": "only the first 5000 rows are imported"}));
            break;
        }
        let rec = match rec {
            Ok(r) => r,
            Err(e) => {
                errors.push(json!({"row": n + 2, "message": e.to_string()}));
                continue;
            }
        };
        let mut body = json!({"values": {}});
        for (i, cell) in rec.iter().enumerate() {
            let cell = cell.trim();
            match columns.get(i).and_then(|c| c.as_deref()) {
                Some("name") => body["name"] = json!(cell),
                Some("tag") if !cell.is_empty() => body["tag"] = json!(cell),
                Some("status") if !cell.is_empty() => body["status"] = json!(cell),
                Some("category") if !cell.is_empty() => body["categoryId"] = json!(cell),
                Some("owner") if !cell.is_empty() => {
                    // a person by name or address, a team by name, else an address without an account
                    let person: Option<Uuid> =
                        sqlx::query_scalar("SELECT id FROM users WHERE workspace_id = $1 AND (lower(email) = lower($2) OR lower(name) = lower($2)) LIMIT 1")
                            .bind(user.workspace_id)
                            .bind(cell)
                            .fetch_optional(&state.db)
                            .await?;
                    let team: Option<Uuid> = sqlx::query_scalar("SELECT id FROM teams WHERE workspace_id = $1 AND lower(name) = lower($2)")
                        .bind(user.workspace_id)
                        .bind(cell)
                        .fetch_optional(&state.db)
                        .await?;
                    match (person, team) {
                        (Some(p), _) => body["ownerId"] = json!(p),
                        (_, Some(t)) => body["ownerTeamId"] = json!(t),
                        _ => body["ownerEmail"] = json!(cell),
                    }
                }
                Some(field) if !matches!(field, "tag" | "status" | "category" | "owner") && !cell.is_empty() => body["values"][field] = json!(cell),
                _ => {}
            }
        }
        let existing: Option<Row> = match body["tag"].as_str() {
            Some(t) => {
                sqlx::query_as(sqlx::AssertSqlSafe(format!("{ROW_SQL} WHERE a.workspace_id = $1 AND upper(a.tag) = upper($2)")))
                    .bind(user.workspace_id)
                    .bind(t)
                    .fetch_optional(&state.db)
                    .await?
            }
            None => None,
        };
        let result = match &existing {
            Some(r) if r.type_id != type_id => Err(ApiError::bad(format!("{} is an asset of another type", r.tag))),
            Some(r) if dry => {
                check_values(&state, user.workspace_id, &fields, &body["values"], r.values.as_object().cloned().unwrap_or_default(), false).await.map(|_| false)
            }
            Some(r) => apply_update(&state, user.workspace_id, Some(user.id), r, &body).await.map(|_| false),
            None if dry => {
                let checked = async {
                    name_of(&body["name"], "an asset", 200)?;
                    status_in(&body["status"])?;
                    category_in(&state, set, &body["categoryId"]).await?;
                    check_values(&state, user.workspace_id, &fields, &body["values"], Map::new(), true).await
                };
                checked.await.map(|_| true)
            }
            None => insert(&state, user.workspace_id, Some(user.id), type_id, &body).await.map(|_| true),
        };
        match result {
            Ok(true) => created += 1,
            Ok(false) => updated += 1,
            Err(e) => errors.push(json!({"row": n + 2, "message": e.to_string()})),
        }
    }
    if !dry {
        audit(
            &state,
            user.workspace_id,
            Some(user.id),
            "assets.imported",
            Some(type_id.to_string()),
            json!({"created": created, "updated": updated, "errors": errors.len()}),
        )
        .await;
    }
    Ok(Json(
        json!({"dryRun": dry, "created": created, "updated": updated, "errors": errors, "columns": headers.iter().zip(&columns).map(|(h, c)| json!({"header": h, "target": c})).collect::<Vec<_>>()}),
    ))
}

// ---- rules --------------------------------------------------------------------------------------------

fn check_rule(req: &Value) -> ApiResult<()> {
    let t = &req["trigger"];
    match t["event"].as_str() {
        Some("created" | "changed") => {}
        Some("date") => {
            if t["fieldId"].as_str().is_none() {
                return Err(ApiError::bad("a date rule names the date field it watches (fieldId)"));
            }
            if !t["days"].as_i64().is_some_and(|d| (0..=3650).contains(&d)) {
                return Err(ApiError::bad("days says how long before the date the rule acts (0 to 3650)"));
            }
        }
        _ => return Err(ApiError::bad("trigger.event is created, changed or date")),
    }
    if !t["status"].is_null() {
        status_in(&t["status"])?;
    }
    let actions = req["actions"].as_array().filter(|a| !a.is_empty() && a.len() <= 5).ok_or_else(|| ApiError::bad("a rule has 1 to 5 actions"))?;
    for a in actions {
        match a["action"].as_str() {
            Some("createIssue") => {
                if a["projectKey"].as_str().is_none_or(str::is_empty) || a["title"].as_str().is_none_or(|t| t.trim().is_empty()) {
                    return Err(ApiError::bad("createIssue needs a projectKey and a title"));
                }
            }
            Some("notifyOwner") => {}
            Some("setStatus") => {
                status_in(&a["status"])?.ok_or_else(|| ApiError::bad("setStatus needs a status"))?;
            }
            _ => return Err(ApiError::bad("an action is createIssue, notifyOwner or setStatus")),
        }
    }
    Ok(())
}

/// `GET /asset-sets/{id}/rules` (the set's admins).
pub async fn rules(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(set): Path<Uuid>) -> ApiResult<Json<Value>> {
    on(&state, &user).await?;
    rank_in(&state, &user, set, ADMIN).await?;
    let rows: Vec<(Uuid, String, bool, Value, Value, i32, Option<DateTime<Utc>>, Option<String>)> =
        sqlx::query_as("SELECT id, name, enabled, trigger, actions, runs, last_run_at, last_error FROM asset_rules WHERE set_id = $1 ORDER BY created_at")
            .bind(set)
            .fetch_all(&state.db)
            .await?;
    Ok(Json(json!(
        rows.iter()
            .map(|(id, name, enabled, trigger, actions, runs, last, error)| json!({
                "id": id, "name": name, "enabled": enabled, "trigger": trigger, "actions": actions, "runs": runs, "lastRunAt": last, "lastError": error,
            }))
            .collect::<Vec<_>>()
    )))
}

/// `POST /asset-sets/{id}/rules` (the set's admins) `{name, trigger, actions, enabled?}`.
pub async fn create_rule(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Path(set): Path<Uuid>,
    Json(req): Json<Value>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    on(&state, &user).await?;
    rank_in(&state, &user, set, ADMIN).await?;
    check_rule(&req)?;
    let id = new_id();
    sqlx::query("INSERT INTO asset_rules (id, set_id, name, enabled, trigger, actions) VALUES ($1, $2, $3, $4, $5, $6)")
        .bind(id)
        .bind(set)
        .bind(name_of(&req["name"], "a rule", 120)?)
        .bind(req["enabled"].as_bool().unwrap_or(true))
        .bind(&req["trigger"])
        .bind(&req["actions"])
        .execute(&state.db)
        .await?;
    Ok((StatusCode::CREATED, Json(json!({"id": id}))))
}

/// `PATCH /asset-rules/{id}` `{name?, enabled?, trigger + actions?}`; `DELETE` removes it.
pub async fn update_rule(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(id): Path<Uuid>, Json(req): Json<Value>) -> ApiResult<StatusCode> {
    on(&state, &user).await?;
    let set: Uuid =
        sqlx::query_scalar("SELECT set_id FROM asset_rules WHERE id = $1").bind(id).fetch_optional(&state.db).await?.ok_or(ApiError::NotFound("rule"))?;
    rank_in(&state, &user, set, ADMIN).await?;
    if req.get("trigger").is_some() || req.get("actions").is_some() {
        check_rule(&req)?;
        sqlx::query("UPDATE asset_rules SET trigger = $2, actions = $3, last_error = NULL WHERE id = $1")
            .bind(id)
            .bind(&req["trigger"])
            .bind(&req["actions"])
            .execute(&state.db)
            .await?;
    }
    if !req["name"].is_null() {
        sqlx::query("UPDATE asset_rules SET name = $2 WHERE id = $1").bind(id).bind(name_of(&req["name"], "a rule", 120)?).execute(&state.db).await?;
    }
    if let Some(e) = req["enabled"].as_bool() {
        sqlx::query("UPDATE asset_rules SET enabled = $2 WHERE id = $1").bind(id).bind(e).execute(&state.db).await?;
    }
    Ok(StatusCode::NO_CONTENT)
}

pub async fn delete_rule(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    on(&state, &user).await?;
    let set: Uuid =
        sqlx::query_scalar("SELECT set_id FROM asset_rules WHERE id = $1").bind(id).fetch_optional(&state.db).await?.ok_or(ApiError::NotFound("rule"))?;
    rank_in(&state, &user, set, ADMIN).await?;
    sqlx::query("DELETE FROM asset_rules WHERE id = $1").bind(id).execute(&state.db).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `{tag}`, `{name}`, `{type}`, `{status}`, `{owner}`, `{date}` and `{field:Name}` in a rule's text.
fn fill(template: &str, r: &Row, date: Option<&str>) -> String {
    let owner = r.owner_name.clone().or_else(|| r.owner_team.clone()).or_else(|| r.owner_email.clone()).unwrap_or_default();
    let mut out = template
        .replace("{tag}", &r.tag)
        .replace("{name}", &r.name)
        .replace("{type}", &r.type_name)
        .replace("{status}", &r.status)
        .replace("{owner}", &owner)
        .replace("{date}", date.unwrap_or(""));
    for f in r.fields.as_array().into_iter().flatten() {
        if let (Some(name), Some(id)) = (f["name"].as_str(), f["id"].as_str()) {
            let v = match r.values.get(id) {
                Some(Value::String(s)) => s.clone(),
                Some(Value::Null) | None => String::new(),
                Some(other) => other.to_string(),
            };
            out = out.replace(&format!("{{field:{name}}}"), &v);
        }
    }
    out
}

async fn act(state: &AppState, workspace: Uuid, r: &Row, actions: &Value, date: Option<&str>) -> ApiResult<()> {
    let bot = Actor { user: None, workspace, depth: 1 };
    for a in actions.as_array().into_iter().flatten() {
        match a["action"].as_str().unwrap_or("") {
            "createIssue" => {
                let key = a["projectKey"].as_str().unwrap_or("").to_uppercase();
                let project: Uuid = sqlx::query_scalar("SELECT id FROM projects WHERE workspace_id = $1 AND upper(key) = $2 AND archived_at IS NULL")
                    .bind(workspace)
                    .bind(&key)
                    .fetch_optional(&state.db)
                    .await?
                    .ok_or_else(|| ApiError::bad(format!("there is no project {key}")))?;
                let mut fields = Map::new();
                if a["assignOwner"].as_bool() == Some(true)
                    && let Some(o) = r.owner_id
                {
                    fields.insert("assigneeId".into(), json!(o));
                }
                let description = a["description"].as_str().filter(|d| !d.trim().is_empty()).map(|d| crate::doc::from_markdown(&fill(d, r, date)));
                let s = crate::issues::create(
                    state,
                    &bot,
                    crate::issues::NewIssue { project, title: fill(a["title"].as_str().unwrap_or("{tag} {name}"), r, date), description, fields },
                )
                .await?;
                sqlx::query("INSERT INTO issue_assets (issue_id, asset_id) VALUES ($1, $2) ON CONFLICT DO NOTHING")
                    .bind(s.id)
                    .bind(r.id)
                    .execute(&state.db)
                    .await?;
                event(state, r.id, None, "issue", json!({"key": s.key, "linked": true, "rule": true})).await;
            }
            "notifyOwner" => {
                // about an issue of the asset, when there is one to point at
                let issue: Option<Uuid> = sqlx::query_scalar("SELECT ia.issue_id FROM issue_assets ia JOIN issues i ON i.id = ia.issue_id WHERE ia.asset_id = $1 AND i.deleted_at IS NULL ORDER BY i.created_at DESC LIMIT 1").bind(r.id).fetch_optional(&state.db).await?;
                if let (Some(owner), Some(issue)) = (r.owner_id, issue) {
                    let s = crate::issues::summary(state, crate::issues::load(state, issue).await?);
                    let message: String = fill(a["message"].as_str().unwrap_or("{tag} {name} needs your attention"), r, date).chars().take(300).collect();
                    crate::notify::notify(state, workspace, owner, &s, None, "automation", json!({"message": message})).await;
                }
            }
            "setStatus" => {
                if let Some(s) = status_in(&a["status"])? {
                    apply_update(state, workspace, None, r, &json!({"status": s})).await?;
                }
            }
            _ => {}
        }
    }
    Ok(())
}

async fn ran(state: &AppState, rule: Uuid, result: ApiResult<()>) {
    let _ = match result {
        Ok(()) => {
            sqlx::query("UPDATE asset_rules SET runs = runs + 1, last_run_at = now(), last_error = NULL WHERE id = $1").bind(rule).execute(&state.db).await
        }
        Err(e) => {
            sqlx::query("UPDATE asset_rules SET last_run_at = now(), last_error = $2 WHERE id = $1")
                .bind(rule)
                .bind(e.to_string().chars().take(500).collect::<String>())
                .execute(&state.db)
                .await
        }
    };
}

/// After an asset was created or changed: the set's rules for that.
async fn run_rules(state: &AppState, set: Uuid, asset: Uuid, what: &str) {
    let rules: Vec<(Uuid, Value, Value)> =
        sqlx::query_as("SELECT id, trigger, actions FROM asset_rules WHERE set_id = $1 AND enabled AND trigger->>'event' = $2 ORDER BY created_at")
            .bind(set)
            .bind(what)
            .fetch_all(&state.db)
            .await
            .unwrap_or_default();
    if rules.is_empty() {
        return;
    }
    let Ok(Some(r)) = sqlx::query_as::<_, Row>(sqlx::AssertSqlSafe(format!("{ROW_SQL} WHERE a.id = $1"))).bind(asset).fetch_optional(&state.db).await else {
        return;
    };
    for (rule, trigger, actions) in rules {
        if trigger["typeId"].as_str().is_some_and(|t| t != r.type_id.to_string()) || trigger["status"].as_str().is_some_and(|s| s != r.status) {
            continue;
        }
        let result = act(state, r_workspace(state, &r).await, &r, &actions, None).await;
        ran(state, rule, result).await;
    }
}

async fn r_workspace(state: &AppState, r: &Row) -> Uuid {
    sqlx::query_scalar("SELECT workspace_id FROM assets WHERE id = $1").bind(r.id).fetch_one(&state.db).await.unwrap_or_default()
}

/// Once a minute: date rules — assets whose date comes up within the rule's days, once per asset
/// and date.
pub async fn tick(state: &AppState) -> anyhow::Result<()> {
    let rules: Vec<(Uuid, Uuid, Uuid, Value, Value)> = sqlx::query_as(
        "SELECT r.id, r.set_id, s.workspace_id, r.trigger, r.actions FROM asset_rules r JOIN asset_sets s ON s.id = r.set_id WHERE r.enabled AND r.trigger->>'event' = 'date'",
    )
    .fetch_all(&state.db)
    .await?;
    for (rule, set, workspace, trigger, actions) in rules {
        if !crate::features::on(state, workspace, "assets").await {
            continue;
        }
        let (Some(field), Some(days)) = (trigger["fieldId"].as_str(), trigger["days"].as_i64()) else { continue };
        // dates as text sort and compare as dates (YYYY-MM-DD)
        let due: Vec<(Uuid, String)> = sqlx::query_as(
            "SELECT a.id, a.\"values\"->>$2 FROM assets a
              WHERE a.set_id = $1 AND a.status <> 'retired' AND a.\"values\"->>$2 ~ '^\\d{4}-\\d{2}-\\d{2}$'
                AND (a.\"values\"->>$2)::date BETWEEN current_date AND current_date + $3::int
                AND ($4::text IS NULL OR a.type_id::text = $4) AND ($5::text IS NULL OR a.status = $5)
                AND NOT EXISTS (SELECT 1 FROM asset_rule_fires f WHERE f.rule_id = $6 AND f.asset_id = a.id AND f.due = (a.\"values\"->>$2)::date)
              LIMIT 200",
        )
        .bind(set)
        .bind(field)
        .bind(days as i32)
        .bind(trigger["typeId"].as_str())
        .bind(trigger["status"].as_str())
        .bind(rule)
        .fetch_all(&state.db)
        .await?;
        for (asset, date) in due {
            // claimed first: two replicas don't both act
            let claimed = sqlx::query("INSERT INTO asset_rule_fires (rule_id, asset_id, due) VALUES ($1, $2, $3::date) ON CONFLICT DO NOTHING")
                .bind(rule)
                .bind(asset)
                .bind(&date)
                .execute(&state.db)
                .await?;
            if claimed.rows_affected() == 0 {
                continue;
            }
            let Some(r) = sqlx::query_as::<_, Row>(sqlx::AssertSqlSafe(format!("{ROW_SQL} WHERE a.id = $1"))).bind(asset).fetch_optional(&state.db).await?
            else {
                continue;
            };
            let result = act(state, workspace, &r, &actions, Some(&date)).await;
            ran(state, rule, result).await;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fields_and_prefixes() {
        let f = check_fields(&json!([{"name": "Serial", "kind": "text", "required": true}, {"name": "State", "kind": "select", "options": ["New", {"label": "Used"}]}, {"name": "Location", "kind": "asset"}])).unwrap();
        assert_eq!(f.as_array().unwrap().len(), 3);
        assert!(f[0]["id"].as_str().unwrap().parse::<Uuid>().is_ok() && f[0]["required"] == true);
        assert_eq!(f[1]["options"][1]["label"], "Used");
        for bad in [
            json!([{"name": "A", "kind": "blob"}]),
            json!([{"name": "A", "kind": "select"}]),
            json!([{"name": "Status", "kind": "text"}]),
            json!([{"name": "A", "kind": "text"}, {"name": "a", "kind": "text"}]),
        ] {
            assert!(check_fields(&bad).is_err(), "{bad}");
        }
        assert_eq!(prefix_of(&Value::Null, "Laptop").unwrap(), "LAP");
        assert_eq!(prefix_of(&json!("lt"), "Laptop").unwrap(), "LT");
        assert!(prefix_of(&json!("too-long-prefix"), "x").is_err());
        assert_eq!(status_in(&json!("In stock")).unwrap().as_deref(), Some("in_stock"));
        assert!(status_in(&json!("lost")).is_err());
    }
}
