//! Share links: a read-only page of a project for people without an account — a list of issues
//! (a status page, from a TQL query) or the roadmap (epics and releases on a timeline). Project
//! admins make them, with an optional password and end date, and can revoke them at any time.
//!
//! Never shared: private issues (security levels), restricted comments, and people's names unless
//! the link says so. The workspace can turn links off (`features.sharing`): they stop working.

use axum::Json;
use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, NaiveDate, Utc};
use object_store::ObjectStoreExt;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::QueryBuilder;
use uuid::Uuid;

use crate::access::Role;
use crate::app::{AppState, audit, new_id};
use crate::auth::CurrentUser;
use crate::error::{ApiError, ApiResult};

const KINDS: [&str; 2] = ["issues", "roadmap"];
/// Issues a list shows at most.
const LIMIT: i64 = 500;

#[derive(sqlx::FromRow, Clone)]
struct Share {
    id: Uuid,
    workspace_id: Uuid,
    project_id: Uuid,
    token: String,
    kind: String,
    title: String,
    query: String,
    options: Value,
    password_hash: Option<String>,
    expires_at: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
    views: i64,
    last_viewed_at: Option<DateTime<Utc>>,
}

const COLS: &str = "id, workspace_id, project_id, token, kind, title, query, options, password_hash, expires_at, created_at, views, last_viewed_at";

impl Share {
    fn people(&self) -> bool {
        self.options["people"] == true
    }
    fn comments(&self) -> bool {
        self.options["comments"] == true
    }
    fn url(&self, state: &AppState) -> String {
        format!("{}/share/{}", state.config.base(), self.token)
    }
    /// For the project admins who manage it.
    fn json(&self, state: &AppState) -> Value {
        json!({
            "id": self.id, "kind": self.kind, "title": self.title, "query": self.query, "url": self.url(state),
            "people": self.people(), "comments": self.comments(), "password": self.password_hash.is_some(),
            "expiresAt": self.expires_at, "expired": self.expires_at.is_some_and(|e| e <= Utc::now()),
            "createdAt": self.created_at, "views": self.views, "lastViewedAt": self.last_viewed_at,
        })
    }
    /// What a password unlocks: changes with the password, so a new one locks people out again.
    fn key(&self) -> Option<String> {
        self.password_hash.as_ref().map(|h| hex::encode(Sha256::digest(format!("{}:{h}", self.token).as_bytes())))
    }
}

// ---- managing links (project admins) ----

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShareReq {
    kind: Option<String>,
    title: Option<String>,
    query: Option<String>,
    people: Option<bool>,
    comments: Option<bool>,
    /// set a password; empty or null with `clearPassword` removes it
    password: Option<String>,
    #[serde(default)]
    clear_password: bool,
    /// `null` clears it
    #[serde(default, deserialize_with = "double_option")]
    expires_at: Option<Option<DateTime<Utc>>>,
}

/// Left out: unchanged; `null`: cleared.
fn double_option<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Option<DateTime<Utc>>>, D::Error> {
    Ok(Some(Option::deserialize(d)?))
}

async fn project_admin(state: &AppState, user: &crate::auth::User, key: &str) -> ApiResult<Uuid> {
    crate::features::require(state, user.workspace_id, "sharing").await?;
    let (p, _) = crate::routes::projects::by_key(state, user, key, Role::Admin).await?;
    Ok(p.id)
}

pub async fn list(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(key): Path<String>) -> ApiResult<Json<Value>> {
    let (p, _) = crate::routes::projects::by_key(&state, &user, &key, Role::Admin).await?;
    let rows: Vec<Share> = sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT {COLS} FROM share_links WHERE project_id = $1 ORDER BY created_at DESC")))
        .bind(p.id)
        .fetch_all(&state.db)
        .await?;
    Ok(Json(json!(rows.iter().map(|s| s.json(&state)).collect::<Vec<_>>())))
}

fn check_query(q: &str) -> ApiResult<String> {
    let q = q.trim();
    if q.len() > 2000 {
        return Err(ApiError::bad("the query is 2000 characters at most"));
    }
    crate::query::parse(q)?;
    Ok(q.to_string())
}

pub async fn create(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Path(key): Path<String>,
    Json(req): Json<ShareReq>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let project = project_admin(&state, &user, &key).await?;
    let kind = req.kind.as_deref().unwrap_or("issues");
    if !KINDS.contains(&kind) {
        return Err(ApiError::bad("kind is issues or roadmap"));
    }
    let title: String =
        req.title.as_deref().map(str::trim).filter(|t| !t.is_empty()).ok_or_else(|| ApiError::bad("the link needs a title"))?.chars().take(120).collect();
    let query = check_query(req.query.as_deref().unwrap_or(""))?;
    let hash = match req.password.as_deref().filter(|p| !p.is_empty()) {
        Some(p) if p.chars().count() < 8 => return Err(ApiError::bad("the password has at least 8 characters")),
        Some(p) => Some(crate::auth::hash_password(p.to_string()).await.map_err(ApiError::Internal)?),
        None => None,
    };
    let expires = req.expires_at.flatten();
    if expires.is_some_and(|e| e <= Utc::now()) {
        return Err(ApiError::bad("the end lies in the past"));
    }
    let id = new_id();
    let s: Share = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "INSERT INTO share_links (id, workspace_id, project_id, token, kind, title, query, options, password_hash, expires_at, created_by)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11) RETURNING {COLS}"
    )))
    .bind(id)
    .bind(user.workspace_id)
    .bind(project)
    .bind(crate::auth::random_token())
    .bind(kind)
    .bind(&title)
    .bind(&query)
    .bind(json!({"people": req.people.unwrap_or(false), "comments": req.comments.unwrap_or(false)}))
    .bind(hash)
    .bind(expires)
    .bind(user.id)
    .fetch_one(&state.db)
    .await?;
    audit(&state, user.workspace_id, Some(user.id), "share.created", Some(key.to_uppercase()), json!({"id": id, "kind": kind, "title": title})).await;
    Ok((StatusCode::CREATED, Json(s.json(&state))))
}

async fn managed(state: &AppState, user: &crate::auth::User, id: Uuid) -> ApiResult<Share> {
    let s: Share = sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT {COLS} FROM share_links WHERE id = $1 AND workspace_id = $2")))
        .bind(id)
        .bind(user.workspace_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(ApiError::NotFound("link"))?;
    crate::access::require(state, user, s.project_id, Role::Admin).await?;
    Ok(s)
}

pub async fn update(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(id): Path<Uuid>, Json(req): Json<ShareReq>) -> ApiResult<Json<Value>> {
    let mut s = managed(&state, &user, id).await?;
    if let Some(t) = req.title.as_deref().map(str::trim) {
        if t.is_empty() {
            return Err(ApiError::bad("the link needs a title"));
        }
        s.title = t.chars().take(120).collect();
    }
    if let Some(q) = req.query.as_deref() {
        s.query = check_query(q)?;
    }
    if let Some(p) = req.people {
        s.options["people"] = json!(p);
    }
    if let Some(c) = req.comments {
        s.options["comments"] = json!(c);
    }
    if let Some(p) = req.password.as_deref().filter(|p| !p.is_empty()) {
        if p.chars().count() < 8 {
            return Err(ApiError::bad("the password has at least 8 characters"));
        }
        s.password_hash = Some(crate::auth::hash_password(p.to_string()).await.map_err(ApiError::Internal)?);
    } else if req.clear_password {
        s.password_hash = None;
    }
    if let Some(e) = req.expires_at {
        s.expires_at = e;
    }
    sqlx::query("UPDATE share_links SET title = $2, query = $3, options = $4, password_hash = $5, expires_at = $6 WHERE id = $1")
        .bind(id)
        .bind(&s.title)
        .bind(&s.query)
        .bind(&s.options)
        .bind(&s.password_hash)
        .bind(s.expires_at)
        .execute(&state.db)
        .await?;
    Ok(Json(s.json(&state)))
}

pub async fn delete(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    let s = managed(&state, &user, id).await?;
    sqlx::query("DELETE FROM share_links WHERE id = $1").bind(id).execute(&state.db).await?;
    audit(&state, user.workspace_id, Some(user.id), "share.revoked", None, json!({"id": id, "title": s.title})).await;
    Ok(StatusCode::NO_CONTENT)
}

// ---- the shared page (no account) ----

/// The link behind a token, when it works: known, not over, sharing on, and unlocked when it has
/// a password (`x-share-key`).
async fn open(state: &AppState, token: &str, headers: &HeaderMap) -> ApiResult<Share> {
    let given = headers.get("x-share-key").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
    open_with(state, token, &given).await
}

async fn open_with(state: &AppState, token: &str, given: &str) -> ApiResult<Share> {
    let s: Share = sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT {COLS} FROM share_links WHERE token = $1")))
        .bind(token)
        .fetch_optional(&state.db)
        .await?
        .ok_or(ApiError::NotFound("link"))?;
    if s.expires_at.is_some_and(|e| e <= Utc::now()) || !crate::features::on(state, s.workspace_id, "sharing").await {
        return Err(ApiError::NotFound("link"));
    }
    if let Some(key) = s.key()
        && !constant_eq(given.as_bytes(), key.as_bytes())
    {
        return Err(ApiError::Unauthorized);
    }
    Ok(s)
}

fn constant_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// `GET /share/{token}`: what the page is (and whether it asks for a password).
pub async fn info(State(state): State<AppState>, Path(token): Path<String>, headers: HeaderMap) -> ApiResult<Json<Value>> {
    let locked = match open(&state, &token, &headers).await {
        Ok(_) => false,
        Err(ApiError::Unauthorized) => true,
        Err(e) => return Err(e),
    };
    let s: Share = sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT {COLS} FROM share_links WHERE token = $1"))).bind(&token).fetch_one(&state.db).await?;
    let (project, key, color, icon, workspace, settings): (String, String, Option<String>, Option<String>, String, Value) =
        sqlx::query_as("SELECT p.name, p.key, p.color, p.icon, w.name, w.settings FROM projects p JOIN workspaces w ON w.id = p.workspace_id WHERE p.id = $1")
            .bind(s.project_id)
            .fetch_one(&state.db)
            .await?;
    if !locked {
        sqlx::query("UPDATE share_links SET views = views + 1, last_viewed_at = now() WHERE id = $1").bind(s.id).execute(&state.db).await?;
    }
    Ok(Json(json!({
        "title": s.title, "kind": s.kind, "locked": locked,
        "project": {"name": project, "key": key, "color": color, "icon": icon},
        "workspace": {"name": workspace, "accentColor": settings["accentColor"]},
        "people": s.people(), "comments": s.comments(), "expiresAt": s.expires_at,
    })))
}

#[derive(Deserialize)]
pub struct Unlock {
    password: String,
}

/// `POST /share/{token}/unlock`: the key the page sends with its requests (10 tries per quarter hour).
pub async fn unlock(State(state): State<AppState>, Path(token): Path<String>, Json(req): Json<Unlock>) -> ApiResult<Json<Value>> {
    let s: Share = sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT {COLS} FROM share_links WHERE token = $1")))
        .bind(&token)
        .fetch_optional(&state.db)
        .await?
        .ok_or(ApiError::NotFound("link"))?;
    if s.expires_at.is_some_and(|e| e <= Utc::now()) || !crate::features::on(&state, s.workspace_id, "sharing").await {
        return Err(ApiError::NotFound("link"));
    }
    let Some(hash) = s.password_hash.clone() else { return Ok(Json(json!({"key": null}))) };
    let throttle = format!("share:{}", s.id);
    if !crate::auth::login_allowed(&state, &throttle).await {
        return Err(ApiError::TooMany("too many wrong passwords: try again in a quarter of an hour".into()));
    }
    if !crate::auth::verify_password(req.password, hash).await {
        crate::auth::record_login_failure(&state, &throttle).await;
        return Err(ApiError::bad("that's not the password"));
    }
    crate::auth::clear_login_failures(&state, &throttle).await;
    Ok(Json(json!({"key": s.key()})))
}

/// The share's issues: its query within the project, never private ones.
async fn issue_ids(state: &AppState, s: &Share, extra: &str, limit: i64) -> ApiResult<Vec<Uuid>> {
    let parsed = crate::query::parse(&s.query)?;
    let ctx = crate::query::context_with(state, s.workspace_id, Uuid::nil(), vec![s.project_id], &parsed).await?;
    let mut qb: QueryBuilder<sqlx::Postgres> = QueryBuilder::new(
        "SELECT i.id FROM issues i JOIN projects p ON p.id = i.project_id JOIN statuses s ON s.id = i.status_id JOIN issue_types it ON it.id = i.type_id WHERE",
    );
    crate::query::push_where(&mut qb, &ctx, &parsed)?;
    qb.push(" AND i.security_level_id IS NULL");
    qb.push(extra);
    crate::query::push_order(&mut qb, &ctx, &parsed.order, "i.updated_at DESC")?;
    qb.push(" LIMIT ");
    qb.push_bind(limit);
    Ok(qb.build_query_scalar().fetch_all(&state.db).await?)
}

fn issue_json(s: &Share, i: &crate::issues::Summary) -> Value {
    json!({
        "key": i.key, "title": i.title,
        "type": {"name": i.kind.name, "icon": i.kind.icon, "color": i.kind.color, "level": i.kind.level},
        "status": {"name": i.status.name, "category": i.status.category, "color": i.status.color},
        "priority": i.priority,
        "assignee": if s.people() { i.assignee.as_ref().map(|a| json!({"name": a.name})) } else { None },
        "startDate": i.start_date, "dueDate": i.due_date, "updatedAt": i.updated_at, "resolvedAt": i.resolved_at, "createdAt": i.created_at,
        "labels": i.labels, "versions": i.versions.iter().map(|v| &v.name).collect::<Vec<_>>(),
        "childCount": i.child_count, "childDone": i.child_done,
    })
}

#[derive(Deserialize)]
pub struct ListQ {
    q: Option<String>,
}

/// `GET /share/{token}/issues`: the list (issue links; text filter `q`).
pub async fn issues(State(state): State<AppState>, Path(token): Path<String>, Query(lq): Query<ListQ>, headers: HeaderMap) -> ApiResult<Json<Value>> {
    let s = open(&state, &token, &headers).await?;
    if s.kind != "issues" {
        return Err(ApiError::NotFound("list"));
    }
    let ids = issue_ids(&state, &s, "", LIMIT + 1).await?;
    let more = ids.len() as i64 > LIMIT;
    let ids = &ids[..ids.len().min(LIMIT as usize)];
    let mut list = crate::issues::load_many(&state, ids).await?;
    let pos: std::collections::HashMap<Uuid, usize> = ids.iter().enumerate().map(|(i, id)| (*id, i)).collect();
    list.sort_by_key(|i| pos.get(&i.id).copied().unwrap_or(usize::MAX));
    let needle = lq.q.as_deref().map(str::trim).filter(|q| !q.is_empty()).map(str::to_lowercase);
    let shown: Vec<Value> = list
        .iter()
        .filter(|i| needle.as_ref().is_none_or(|n| i.title.to_lowercase().contains(n) || i.key.to_lowercase().contains(n)))
        .map(|i| issue_json(&s, i))
        .collect();
    Ok(Json(json!({"issues": shown, "more": more})))
}

/// `GET /share/{token}/roadmap`: epics with their dates and progress, and the releases.
pub async fn roadmap(State(state): State<AppState>, Path(token): Path<String>, headers: HeaderMap) -> ApiResult<Json<Value>> {
    let s = open(&state, &token, &headers).await?;
    if s.kind != "roadmap" {
        return Err(ApiError::NotFound("roadmap"));
    }
    let ids = issue_ids(&state, &s, " AND it.level = 1", LIMIT).await?;
    let mut epics = crate::issues::load_many(&state, &ids).await?;
    epics.sort_by_key(|e| (e.start_date.or(e.due_date).unwrap_or(NaiveDate::MAX), e.key.clone()));
    type V = (String, String, Option<NaiveDate>, Option<NaiveDate>, Option<String>);
    let releases: Vec<V> = sqlx::query_as(
        "SELECT name, status, start_date, release_date, description FROM versions WHERE project_id = $1 AND status <> 'archived' ORDER BY position, created_at",
    )
    .bind(s.project_id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(json!({
        "epics": epics.iter().map(|e| issue_json(&s, e)).collect::<Vec<_>>(),
        "releases": releases.into_iter().map(|(name, status, start, date, description)| json!({
            "name": name, "released": status == "released", "startDate": start, "releaseDate": date, "description": description,
        })).collect::<Vec<_>>(),
    })))
}

/// One shared issue: in the share's list (or an epic of the roadmap) and never private.
async fn shared_issue(state: &AppState, s: &Share, key: &str) -> ApiResult<Uuid> {
    let (k, n) = key.rsplit_once('-').ok_or(ApiError::NotFound("issue"))?;
    let n: i32 = n.parse().map_err(|_| ApiError::NotFound("issue"))?;
    let extra = if s.kind == "roadmap" { " AND it.level = 1" } else { "" };
    let id: Option<Uuid> = sqlx::query_scalar(
        "SELECT i.id FROM issues i JOIN projects p ON p.id = i.project_id WHERE i.project_id = $1 AND upper(p.key) = upper($2) AND i.number = $3
            AND i.deleted_at IS NULL AND i.security_level_id IS NULL",
    )
    .bind(s.project_id)
    .bind(k)
    .bind(n)
    .fetch_optional(&state.db)
    .await?;
    let id = id.ok_or(ApiError::NotFound("issue"))?;
    // in the share's query
    let mut extra = extra.to_string();
    extra.push_str(&format!(" AND i.id = '{id}'"));
    let ids = issue_ids(state, s, &extra, 1).await?;
    ids.first().copied().ok_or(ApiError::NotFound("issue"))
}

/// A document as the shared page shows it: images and file links through the share, people's
/// names only when the link shows them.
fn public_doc(mut v: Value, token: &str, people: bool) -> Value {
    fn walk(n: &mut Value, token: &str, people: bool) {
        if n["type"] == "mention" && n["attrs"]["mentionSuggestionChar"] != "#" {
            let label = if people { n["attrs"]["label"].as_str().unwrap_or("someone").to_string() } else { "someone".into() };
            *n = json!({"type": "text", "text": format!("@{label}")});
            return;
        }
        let fix = |s: &str| s.strip_prefix("/api/attachments/").map(|id| format!("/api/share/{token}/files/{id}"));
        if let Some(src) = n.pointer("/attrs/src").and_then(Value::as_str).and_then(fix) {
            n["attrs"]["src"] = json!(src);
        }
        if let Some(marks) = n["marks"].as_array_mut() {
            for m in marks {
                if let Some(href) = m.pointer("/attrs/href").and_then(Value::as_str).and_then(fix) {
                    m["attrs"]["href"] = json!(href);
                }
            }
        }
        if let Some(c) = n["content"].as_array_mut() {
            for child in c {
                walk(child, token, people);
            }
        }
    }
    walk(&mut v, token, people);
    v
}

/// `GET /share/{token}/issues/{key}`: the issue's description, sub-issues and (when the link
/// shows them) its comments for everyone.
pub async fn issue(State(state): State<AppState>, Path((token, key)): Path<(String, String)>, headers: HeaderMap) -> ApiResult<Json<Value>> {
    let s = open(&state, &token, &headers).await?;
    let id = shared_issue(&state, &s, &key).await?;
    let i = crate::issues::load_many(&state, &[id]).await?.pop().ok_or(ApiError::NotFound("issue"))?;
    let description: Option<Value> = sqlx::query_scalar("SELECT description FROM issues WHERE id = $1").bind(id).fetch_one(&state.db).await?;
    let children: Vec<Uuid> =
        sqlx::query_scalar("SELECT id FROM issues WHERE parent_id = $1 AND deleted_at IS NULL AND security_level_id IS NULL ORDER BY rank")
            .bind(id)
            .fetch_all(&state.db)
            .await?;
    let children = crate::issues::load_many(&state, &children).await?;
    let comments: Vec<Value> = if s.comments() {
        let rows: Vec<(Value, Option<String>, DateTime<Utc>)> = sqlx::query_as(
            "SELECT c.body, u.name, c.created_at FROM comments c LEFT JOIN users u ON u.id = c.author_id
              WHERE c.issue_id = $1 AND c.visibility IS NULL ORDER BY c.created_at",
        )
        .bind(id)
        .fetch_all(&state.db)
        .await?;
        rows.into_iter()
            .map(|(body, author, at)| json!({"body": public_doc(body, &token, s.people()), "author": if s.people() { author } else { None }, "createdAt": at}))
            .collect()
    } else {
        vec![]
    };
    let mut v = issue_json(&s, &i);
    v["description"] = description.map(|d| public_doc(d, &token, s.people())).unwrap_or(Value::Null);
    v["children"] = json!(children.iter().map(|c| issue_json(&s, c)).collect::<Vec<_>>());
    v["comments"] = json!(comments);
    Ok(Json(v))
}

#[derive(Deserialize)]
pub struct FileQ {
    key: Option<String>,
}

/// `GET /share/{token}/files/{id}`: a file of a shared issue (images in its description).
pub async fn file(State(state): State<AppState>, Path((token, id)): Path<(String, Uuid)>, Query(fq): Query<FileQ>, headers: HeaderMap) -> ApiResult<Response> {
    // images load without the page's header: the key may come as ?key=…
    let given = headers.get("x-share-key").and_then(|v| v.to_str().ok()).map(str::to_string).or(fq.key).unwrap_or_default();
    let s = open_with(&state, &token, &given).await?;
    let row: Option<(String, String, String, Uuid)> =
        sqlx::query_as("SELECT a.name, a.mime, a.storage_key, a.issue_id FROM attachments a WHERE a.id = $1").bind(id).fetch_optional(&state.db).await?;
    let (name, mime, storage_key, issue) = row.ok_or(ApiError::NotFound("file"))?;
    let (key, number): (String, i32) =
        sqlx::query_as("SELECT p.key, i.number FROM issues i JOIN projects p ON p.id = i.project_id WHERE i.id = $1").bind(issue).fetch_one(&state.db).await?;
    shared_issue(&state, &s, &format!("{key}-{number}")).await?;
    let got = state.blobs.get(&object_store::path::Path::from(storage_key)).await.map_err(|_| ApiError::NotFound("file"))?;
    let bytes = got.bytes().await.map_err(|e| ApiError::Internal(e.into()))?;
    // shown inline only when it's an image the browser can't run code from
    let inline = matches!(mime.as_str(), "image/png" | "image/jpeg" | "image/gif" | "image/webp");
    let disposition = if inline { "inline".to_string() } else { format!("attachment; filename=\"{}\"", name.replace(['"', '\\', '\r', '\n'], "_")) };
    Ok((
        [
            (header::CONTENT_TYPE, if inline { mime } else { "application/octet-stream".into() }),
            (header::CONTENT_DISPOSITION, disposition),
            (header::CACHE_CONTROL, "private, max-age=3600".into()),
        ],
        Body::from(bytes),
    )
        .into_response())
}
