//! SCIM 2.0 provisioning (RFC 7643 / 7644) at `/scim/v2`: an identity provider creates, updates
//! and deactivates people and keeps groups — TaskCraft's teams — in step, without waiting for
//! anyone to sign in.
//!
//! Each identity provider gets a bearer token from an admin (`/api/admin/scim`). A token sees the
//! people and teams it provisions: accounts it created, and existing ones it took over by their
//! e-mail address. People are never deleted — `active: false` and `DELETE` deactivate them, which
//! ends their sessions and revokes their API tokens at once — so their history stays. Groups are
//! teams; a token tied to a single sign-on provider shares that provider's teams with the group
//! sync at sign-in (`sso:<provider>:<group>`). Every change lands in the audit log, and the token
//! remembers its last request and its last error for the admin page.

use axum::Json;
use axum::Router;
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::app::{AppState, audit, new_id};
use crate::auth::CurrentUser;
use crate::error::{ApiError, ApiResult};

const USER_SCHEMA: &str = "urn:ietf:params:scim:schemas:core:2.0:User";
const GROUP_SCHEMA: &str = "urn:ietf:params:scim:schemas:core:2.0:Group";
const LIST_SCHEMA: &str = "urn:ietf:params:scim:api:messages:2.0:ListResponse";
const ERROR_SCHEMA: &str = "urn:ietf:params:scim:api:messages:2.0:Error";
const MAX_PAGE: i64 = 200;
const TOKEN_PREFIX: &str = "tcscim_";

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/ServiceProviderConfig", get(service_provider_config))
        .route("/ResourceTypes", get(resource_types))
        .route("/Schemas", get(schemas))
        .route("/Users", get(list_users).post(create_user))
        .route("/Users/{id}", get(get_user).put(replace_user).patch(patch_user).delete(delete_user))
        .route("/Groups", get(list_groups).post(create_group))
        .route("/Groups/{id}", get(get_group).put(replace_group).patch(patch_group).delete(delete_group))
}

// ---- errors and the caller -------------------------------------------------------------------------

#[derive(Debug)]
struct ScimError {
    status: StatusCode,
    scim_type: Option<&'static str>,
    detail: String,
}

type ScimResult<T> = Result<T, ScimError>;

impl ScimError {
    fn new(status: StatusCode, scim_type: Option<&'static str>, detail: impl Into<String>) -> Self {
        ScimError { status, scim_type, detail: detail.into() }
    }
    fn bad(detail: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, Some("invalidValue"), detail)
    }
    fn not_found(what: &str) -> Self {
        Self::new(StatusCode::NOT_FOUND, None, format!("{what} not found"))
    }
    fn conflict(detail: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, Some("uniqueness"), detail)
    }
}

impl From<sqlx::Error> for ScimError {
    fn from(e: sqlx::Error) -> Self {
        if e.as_database_error().is_some_and(|d| d.is_unique_violation()) {
            return ScimError::conflict("it exists already");
        }
        tracing::error!(error = ?e, "scim");
        ScimError::new(StatusCode::INTERNAL_SERVER_ERROR, None, "internal error")
    }
}

fn scim_json(status: StatusCode, body: Value) -> Response {
    (status, [(header::CONTENT_TYPE, "application/scim+json")], body.to_string()).into_response()
}

impl IntoResponse for ScimError {
    fn into_response(self) -> Response {
        let mut body = json!({"schemas": [ERROR_SCHEMA], "status": self.status.as_u16().to_string(), "detail": self.detail});
        if let Some(t) = self.scim_type {
            body["scimType"] = json!(t);
        }
        scim_json(self.status, body)
    }
}

/// The identity provider calling, by its bearer token.
struct Caller {
    id: Uuid,
    workspace: Uuid,
    provider_id: Option<String>,
    default_role: String,
}

async fn caller(state: &AppState, headers: &HeaderMap) -> ScimResult<Caller> {
    let denied = || ScimError::new(StatusCode::UNAUTHORIZED, None, "a valid bearer token is required");
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer ").or_else(|| v.strip_prefix("bearer ")))
        .map(str::trim)
        .filter(|t| t.starts_with(TOKEN_PREFIX))
        .ok_or_else(denied)?;
    let row: Option<(Uuid, Uuid, Option<String>, String)> = sqlx::query_as(
        "UPDATE scim_tokens SET last_used_at = now(), requests = requests + 1 WHERE token_hash = $1 RETURNING id, workspace_id, provider_id, default_role",
    )
    .bind(crate::auth::token_hash(token))
    .fetch_optional(&state.db)
    .await?;
    let (id, workspace, provider_id, default_role) = row.ok_or_else(denied)?;
    Ok(Caller { id, workspace, provider_id, default_role })
}

/// The answer to a request; what went wrong is kept for the admin page (not found isn't an
/// error: identity providers ask for things to see whether they exist).
async fn respond(state: &AppState, c: &Caller, what: &str, r: ScimResult<Response>) -> Response {
    match r {
        Ok(r) => r,
        Err(e) => {
            if e.status != StatusCode::NOT_FOUND {
                let _ = sqlx::query("UPDATE scim_tokens SET last_error = $2, last_error_at = now() WHERE id = $1")
                    .bind(c.id)
                    .bind(format!("{what}: {} {}", e.status.as_u16(), e.detail).chars().take(500).collect::<String>())
                    .execute(&state.db)
                    .await;
            }
            e.into_response()
        }
    }
}

fn parse(body: &Bytes) -> ScimResult<Value> {
    serde_json::from_slice::<Value>(body)
        .ok()
        .filter(Value::is_object)
        .ok_or_else(|| ScimError::new(StatusCode::BAD_REQUEST, Some("invalidSyntax"), "the body must be a JSON object"))
}

fn text(v: &Value) -> Option<String> {
    v.as_str().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string)
}

/// `true` / `"True"` / `"false"` (Microsoft Entra sends strings).
fn truth(v: &Value) -> Option<bool> {
    match v {
        Value::Bool(b) => Some(*b),
        Value::String(s) if s.eq_ignore_ascii_case("true") => Some(true),
        Value::String(s) if s.eq_ignore_ascii_case("false") => Some(false),
        _ => None,
    }
}

// ---- discovery ----------------------------------------------------------------------------------------

async fn service_provider_config(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(e) = caller(&state, &headers).await {
        return e.into_response();
    }
    scim_json(
        StatusCode::OK,
        json!({
            "schemas": ["urn:ietf:params:scim:schemas:core:2.0:ServiceProviderConfig"],
            "documentationUri": "https://github.com/ortic/TaskCraft/blob/master/docs/admin-guide.md",
            "patch": {"supported": true},
            "bulk": {"supported": false, "maxOperations": 0, "maxPayloadSize": 0},
            "filter": {"supported": true, "maxResults": MAX_PAGE},
            "changePassword": {"supported": false},
            "sort": {"supported": false},
            "etag": {"supported": false},
            "authenticationSchemes": [{"type": "oauthbearertoken", "name": "Bearer token", "description": "A token made in Administration → Provisioning", "primary": true}],
            "meta": {"resourceType": "ServiceProviderConfig", "location": format!("{}/scim/v2/ServiceProviderConfig", state.config.base())},
        }),
    )
}

async fn resource_types(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(e) = caller(&state, &headers).await {
        return e.into_response();
    }
    let base = state.config.base();
    let one = |id: &str, endpoint: &str, schema: &str| {
        json!({"schemas": ["urn:ietf:params:scim:schemas:core:2.0:ResourceType"], "id": id, "name": id, "endpoint": endpoint, "schema": schema,
               "meta": {"resourceType": "ResourceType", "location": format!("{base}/scim/v2/ResourceTypes/{id}")}})
    };
    let all = vec![one("User", "/Users", USER_SCHEMA), one("Group", "/Groups", GROUP_SCHEMA)];
    scim_json(StatusCode::OK, json!({"schemas": [LIST_SCHEMA], "totalResults": all.len(), "startIndex": 1, "itemsPerPage": all.len(), "Resources": all}))
}

async fn schemas(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(e) = caller(&state, &headers).await {
        return e.into_response();
    }
    let attr = |name: &str, kind: &str, required: bool, multi: bool| json!({"name": name, "type": kind, "multiValued": multi, "required": required, "caseExact": false, "mutability": "readWrite", "returned": "default", "uniqueness": if name == "userName" { "server" } else { "none" }});
    let all = vec![
        json!({"schemas": ["urn:ietf:params:scim:schemas:core:2.0:Schema"], "id": USER_SCHEMA, "name": "User", "description": "A person",
               "attributes": [attr("userName", "string", true, false), attr("name", "complex", false, false), attr("displayName", "string", false, false),
                              attr("emails", "complex", false, true), attr("active", "boolean", false, false), attr("externalId", "string", false, false)]}),
        json!({"schemas": ["urn:ietf:params:scim:schemas:core:2.0:Schema"], "id": GROUP_SCHEMA, "name": "Group", "description": "A team",
               "attributes": [attr("displayName", "string", true, false), attr("members", "complex", false, true), attr("externalId", "string", false, false)]}),
    ];
    scim_json(StatusCode::OK, json!({"schemas": [LIST_SCHEMA], "totalResults": all.len(), "startIndex": 1, "itemsPerPage": all.len(), "Resources": all}))
}

// ---- lists and filters --------------------------------------------------------------------------------

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct ListQuery {
    filter: Option<String>,
    start_index: Option<i64>,
    count: Option<i64>,
    excluded_attributes: Option<String>,
}

/// `attribute eq "value"`: the one kind of filter identity providers use to find a resource.
fn eq_filter(filter: &str) -> ScimResult<(String, String)> {
    let unsupported = || ScimError::new(StatusCode::BAD_REQUEST, Some("invalidFilter"), "only filters of the form attribute eq \"value\" are supported");
    let lower = filter.to_ascii_lowercase();
    let at = lower.rfind(" eq ").ok_or_else(unsupported)?;
    let attr = lower[..at].trim().to_string();
    let value = filter[at + 4..].trim();
    let value = value.strip_prefix('"').and_then(|v| v.strip_suffix('"')).unwrap_or(value).replace("\\\"", "\"");
    if attr.is_empty() {
        return Err(unsupported());
    }
    Ok((attr, value))
}

fn page(q: &ListQuery) -> (i64, i64) {
    (q.start_index.unwrap_or(1).max(1), q.count.unwrap_or(100).clamp(0, MAX_PAGE))
}

fn list_response(total: i64, start: i64, resources: Vec<Value>) -> Response {
    scim_json(
        StatusCode::OK,
        json!({"schemas": [LIST_SCHEMA], "totalResults": total, "startIndex": start, "itemsPerPage": resources.len(), "Resources": resources}),
    )
}

// ---- users --------------------------------------------------------------------------------------------

#[derive(sqlx::FromRow)]
struct UserRow {
    id: Uuid,
    email: Option<String>,
    name: String,
    status: String,
    role: String,
    created_at: DateTime<Utc>,
    external_id: Option<String>,
    user_name: String,
    given_name: Option<String>,
    family_name: Option<String>,
    updated_at: DateTime<Utc>,
}

const USER_SQL: &str = "SELECT u.id, u.email, u.name, u.status, u.role, u.created_at, s.external_id, s.user_name, s.given_name, s.family_name, s.updated_at
                          FROM scim_users s JOIN users u ON u.id = s.user_id WHERE s.token_id = $1";

async fn user_json(state: &AppState, u: &UserRow) -> ScimResult<Value> {
    let groups: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT t.id, t.name FROM team_members m JOIN teams t ON t.id = m.team_id JOIN scim_groups g ON g.team_id = t.id WHERE m.user_id = $1 ORDER BY lower(t.name)",
    )
    .bind(u.id)
    .fetch_all(&state.db)
    .await?;
    let mut name = json!({"formatted": u.name});
    if let Some(g) = &u.given_name {
        name["givenName"] = json!(g);
    }
    if let Some(f) = &u.family_name {
        name["familyName"] = json!(f);
    }
    let mut v = json!({
        "schemas": [USER_SCHEMA], "id": u.id, "userName": u.user_name, "name": name, "displayName": u.name,
        "active": u.status != "deactivated",
        "emails": u.email.iter().map(|e| json!({"value": e, "type": "work", "primary": true})).collect::<Vec<_>>(),
        "groups": groups.iter().map(|(id, n)| json!({"value": id, "display": n})).collect::<Vec<_>>(),
        "meta": {"resourceType": "User", "created": u.created_at, "lastModified": u.updated_at, "location": format!("{}/scim/v2/Users/{}", state.config.base(), u.id)},
    });
    if let Some(e) = &u.external_id {
        v["externalId"] = json!(e);
    }
    Ok(v)
}

async fn load_user(state: &AppState, c: &Caller, id: &str) -> ScimResult<UserRow> {
    let id: Uuid = id.parse().map_err(|_| ScimError::not_found("user"))?;
    sqlx::query_as(sqlx::AssertSqlSafe(format!("{USER_SQL} AND u.id = $2")))
        .bind(c.id)
        .bind(id)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| ScimError::not_found("user"))
}

/// What a request says about a person (absent: not said).
#[derive(Default, Debug)]
struct UserIn {
    user_name: Option<String>,
    external_id: Option<Option<String>>,
    name: Option<String>,
    given: Option<Option<String>>,
    family: Option<Option<String>>,
    email: Option<Option<String>>,
    active: Option<bool>,
}

impl UserIn {
    /// Reads a resource, or the value of a PATCH operation without a path (whose keys may be
    /// paths themselves: `"name.givenName"`).
    fn read(&mut self, v: &Value) -> ScimResult<()> {
        for (k, val) in v.as_object().into_iter().flatten() {
            self.set(k, val)?;
        }
        Ok(())
    }

    fn set(&mut self, path: &str, v: &Value) -> ScimResult<()> {
        let p = path.to_ascii_lowercase();
        let p = p.strip_prefix(&format!("{}:", USER_SCHEMA.to_ascii_lowercase())).unwrap_or(&p);
        match p {
            "username" => self.user_name = Some(text(v).ok_or_else(|| ScimError::bad("userName can't be empty"))?),
            "externalid" => self.external_id = Some(text(v)),
            "displayname" | "name.formatted" => {
                if let Some(n) = text(v) {
                    self.name = Some(n);
                }
            }
            "name.givenname" => self.given = Some(text(v)),
            "name.familyname" => self.family = Some(text(v)),
            "name" => {
                if let Some(n) = text(&v["formatted"]) {
                    self.name = Some(n);
                }
                if !v["givenName"].is_null() {
                    self.given = Some(text(&v["givenName"]));
                }
                if !v["familyName"].is_null() {
                    self.family = Some(text(&v["familyName"]));
                }
            }
            "active" => self.active = Some(truth(v).ok_or_else(|| ScimError::bad("active is true or false"))?),
            "emails" => {
                let list = v.as_array().cloned().unwrap_or_default();
                let pick = list.iter().find(|e| truth(&e["primary"]) == Some(true)).or_else(|| list.first());
                self.email = Some(pick.and_then(|e| text(&e["value"])));
            }
            // emails[type eq "work"].value, emails[primary eq true].value
            p if p.starts_with("emails[") && p.ends_with(".value") => self.email = Some(text(v)),
            _ => {}
        }
        Ok(())
    }

    fn unset(&mut self, path: &str) {
        match path.to_ascii_lowercase().as_str() {
            "externalid" => self.external_id = Some(None),
            "name.givenname" => self.given = Some(None),
            "name.familyname" => self.family = Some(None),
            p if p.starts_with("emails") => self.email = Some(None),
            _ => {}
        }
    }
}

fn clean_email(e: &str) -> ScimResult<String> {
    let e = e.trim().to_lowercase();
    if e.len() > 254 || !e.contains('@') || e.contains(char::is_whitespace) {
        return Err(ScimError::bad(format!("{e} isn't an e-mail address")));
    }
    Ok(e)
}

fn clean_name(n: &str) -> String {
    n.trim().chars().take(100).collect()
}

/// Deactivates (ends sessions, revokes API tokens) or reactivates a person.
async fn set_active(state: &AppState, c: &Caller, u: &UserRow, active: bool) -> ScimResult<()> {
    if active == (u.status != "deactivated") {
        return Ok(());
    }
    if !active && u.role == "owner" {
        return Err(ScimError::new(StatusCode::FORBIDDEN, None, "the workspace owner can't be deactivated by provisioning"));
    }
    if active {
        sqlx::query("UPDATE users SET status = 'active' WHERE id = $1 AND status = 'deactivated'").bind(u.id).execute(&state.db).await?;
    } else {
        let mut tx = state.db.begin().await?;
        sqlx::query("UPDATE users SET status = 'deactivated' WHERE id = $1").bind(u.id).execute(&mut *tx).await?;
        sqlx::query("DELETE FROM sessions WHERE user_id = $1").bind(u.id).execute(&mut *tx).await?;
        sqlx::query("DELETE FROM api_tokens WHERE user_id = $1").bind(u.id).execute(&mut *tx).await?;
        sqlx::query("DELETE FROM user_links WHERE user_id = $1").bind(u.id).execute(&mut *tx).await?;
        tx.commit().await?;
    }
    audit(
        state,
        c.workspace,
        None,
        if active { "scim.user.reactivated" } else { "scim.user.deactivated" },
        Some(u.id.to_string()),
        json!({"token": c.id, "name": u.name}),
    )
    .await;
    Ok(())
}

/// Applies what a request says to a provisioned person.
async fn apply_user(state: &AppState, c: &Caller, u: &UserRow, i: UserIn) -> ScimResult<()> {
    let given = i.given.clone().unwrap_or_else(|| u.given_name.clone());
    let family = i.family.clone().unwrap_or_else(|| u.family_name.clone());
    let name = match &i.name {
        Some(n) => Some(clean_name(n)),
        None if i.given.is_some() || i.family.is_some() => {
            Some(clean_name(&[given.as_deref(), family.as_deref()].into_iter().flatten().collect::<Vec<_>>().join(" "))).filter(|n| !n.is_empty())
        }
        None => None,
    };
    let email = match &i.email {
        Some(Some(e)) => Some(Some(clean_email(e)?)),
        Some(None) => Some(None),
        None => None,
    };
    let mut changed = Vec::new();
    if let Some(n) = name.filter(|n| *n != u.name) {
        sqlx::query("UPDATE users SET name = $2 WHERE id = $1").bind(u.id).bind(&n).execute(&state.db).await?;
        changed.push("name");
    }
    if let Some(e) = email.filter(|e| *e != u.email) {
        sqlx::query("UPDATE users SET email = $2 WHERE id = $1").bind(u.id).bind(&e).execute(&state.db).await.map_err(|e| match ScimError::from(e) {
            x if x.status == StatusCode::CONFLICT => ScimError::conflict("someone else has this e-mail address"),
            x => x,
        })?;
        changed.push("email");
    }
    let user_name = i.user_name.clone().unwrap_or_else(|| u.user_name.clone());
    let external = i.external_id.clone().unwrap_or_else(|| u.external_id.clone());
    sqlx::query("UPDATE scim_users SET user_name = $2, external_id = $3, given_name = $4, family_name = $5, updated_at = now() WHERE user_id = $1")
        .bind(u.id)
        .bind(&user_name)
        .bind(&external)
        .bind(&given)
        .bind(&family)
        .execute(&state.db)
        .await
        .map_err(|e| match ScimError::from(e) {
            x if x.status == StatusCode::CONFLICT => ScimError::conflict(format!("userName {user_name} is taken")),
            x => x,
        })?;
    if user_name != u.user_name {
        changed.push("userName");
    }
    if !changed.is_empty() {
        audit(state, c.workspace, None, "scim.user.updated", Some(u.id.to_string()), json!({"token": c.id, "changed": changed})).await;
    }
    if let Some(a) = i.active {
        set_active(state, c, u, a).await?;
    }
    Ok(())
}

async fn list_users(State(state): State<AppState>, headers: HeaderMap, Query(q): Query<ListQuery>) -> Response {
    let c = match caller(&state, &headers).await {
        Ok(c) => c,
        Err(e) => return e.into_response(),
    };
    let r = async {
        let (start, count) = page(&q);
        let (cond, value) = match q.filter.as_deref().map(str::trim).filter(|f| !f.is_empty()) {
            None => ("TRUE", String::new()),
            Some(f) => {
                let (attr, value) = eq_filter(f)?;
                let cond = match attr.as_str() {
                    "username" => "lower(s.user_name) = lower($2)",
                    "externalid" => "s.external_id = $2",
                    "id" => "u.id::text = lower($2)",
                    "displayname" | "name.formatted" => "lower(u.name) = lower($2)",
                    a if a == "emails.value" || (a.starts_with("emails[") && a.ends_with(".value")) || a == "emails" => "lower(u.email) = lower($2)",
                    _ => return Err(ScimError::new(StatusCode::BAD_REQUEST, Some("invalidFilter"), format!("can't filter people by {attr}"))),
                };
                (cond, value)
            }
        };
        let total: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT count(*) FROM scim_users s JOIN users u ON u.id = s.user_id WHERE s.token_id = $1 AND ($2 = $2) AND {cond}"
        )))
        .bind(c.id)
        .bind(&value)
        .fetch_one(&state.db)
        .await?;
        let rows: Vec<UserRow> =
            sqlx::query_as(sqlx::AssertSqlSafe(format!("{USER_SQL} AND ($2 = $2) AND {cond} ORDER BY s.created_at, u.id OFFSET $3 LIMIT $4")))
                .bind(c.id)
                .bind(&value)
                .bind(start - 1)
                .bind(count)
                .fetch_all(&state.db)
                .await?;
        let mut out = Vec::with_capacity(rows.len());
        for u in &rows {
            out.push(user_json(&state, u).await?);
        }
        Ok(list_response(total, start, out))
    }
    .await;
    respond(&state, &c, "GET /Users", r).await
}

async fn get_user(State(state): State<AppState>, headers: HeaderMap, Path(id): Path<String>) -> Response {
    let c = match caller(&state, &headers).await {
        Ok(c) => c,
        Err(e) => return e.into_response(),
    };
    let r = async { Ok(scim_json(StatusCode::OK, user_json(&state, &load_user(&state, &c, &id).await?).await?)) }.await;
    respond(&state, &c, "GET /Users/{id}", r).await
}

async fn create_user(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    let c = match caller(&state, &headers).await {
        Ok(c) => c,
        Err(e) => return e.into_response(),
    };
    let r = async {
        let v = parse(&body)?;
        let mut i = UserIn::default();
        i.read(&v)?;
        let user_name = i.user_name.clone().ok_or_else(|| ScimError::bad("userName is required"))?;
        // the address: the primary e-mail, else a userName that is one
        let email = match i.email.clone().flatten() {
            Some(e) => Some(clean_email(&e)?),
            None if user_name.contains('@') => Some(clean_email(&user_name)?),
            None => None,
        };
        let taken: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM scim_users WHERE token_id = $1 AND lower(user_name) = lower($2))")
            .bind(c.id)
            .bind(&user_name)
            .fetch_one(&state.db)
            .await?;
        if taken {
            return Err(ScimError::conflict(format!("userName {user_name} exists already")));
        }
        let name = i
            .name
            .clone()
            .or_else(|| Some([i.given.clone().flatten(), i.family.clone().flatten()].into_iter().flatten().collect::<Vec<_>>().join(" ")).filter(|n| !n.is_empty()))
            .map(|n| clean_name(&n))
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| clean_name(&user_name));
        // an account with this address is taken over, not doubled (never one that was removed)
        let existing: Option<(Uuid, bool, bool)> = match &email {
            Some(e) => {
                sqlx::query_as(
                    "SELECT u.id, u.preferences ? 'removed', EXISTS (SELECT 1 FROM scim_users s WHERE s.user_id = u.id) FROM users u WHERE u.workspace_id = $1 AND lower(u.email) = $2",
                )
                .bind(c.workspace)
                .bind(e)
                .fetch_optional(&state.db)
                .await?
            }
            None => None,
        };
        let (id, created) = match existing {
            Some((_, true, _)) => return Err(ScimError::conflict("this person was removed from the workspace")),
            Some((_, _, true)) => return Err(ScimError::conflict("this person is provisioned by another directory already")),
            Some((id, _, _)) => (id, false),
            None => {
                let id = new_id();
                sqlx::query("INSERT INTO users (id, workspace_id, email, name, role, status) VALUES ($1, $2, $3, $4, $5, 'active')")
                    .bind(id)
                    .bind(c.workspace)
                    .bind(&email)
                    .bind(&name)
                    .bind(&c.default_role)
                    .execute(&state.db)
                    .await?;
                (id, true)
            }
        };
        sqlx::query("INSERT INTO scim_users (user_id, token_id, external_id, user_name, given_name, family_name) VALUES ($1, $2, $3, $4, $5, $6)")
            .bind(id)
            .bind(c.id)
            .bind(i.external_id.clone().flatten())
            .bind(&user_name)
            .bind(i.given.clone().flatten())
            .bind(i.family.clone().flatten())
            .execute(&state.db)
            .await?;
        audit(&state, c.workspace, None, if created { "scim.user.created" } else { "scim.user.linked" }, Some(id.to_string()), json!({"token": c.id, "userName": user_name, "email": email}))
            .await;
        let u = load_user(&state, &c, &id.to_string()).await?;
        if !created {
            // the directory's name for an account that was there
            apply_user(&state, &c, &u, UserIn { name: Some(name), active: i.active, ..Default::default() }).await?;
        } else if i.active == Some(false) {
            set_active(&state, &c, &u, false).await?;
        }
        let u = load_user(&state, &c, &id.to_string()).await?;
        let mut res = scim_json(StatusCode::CREATED, user_json(&state, &u).await?);
        if let Ok(loc) = format!("{}/scim/v2/Users/{}", state.config.base(), u.id).parse() {
            res.headers_mut().insert(header::LOCATION, loc);
        }
        Ok(res)
    }
    .await;
    respond(&state, &c, "POST /Users", r).await
}

async fn replace_user(State(state): State<AppState>, headers: HeaderMap, Path(id): Path<String>, body: Bytes) -> Response {
    let c = match caller(&state, &headers).await {
        Ok(c) => c,
        Err(e) => return e.into_response(),
    };
    let r = async {
        let u = load_user(&state, &c, &id).await?;
        let v = parse(&body)?;
        let mut i = UserIn::default();
        i.read(&v)?;
        // a replacement: what isn't said is empty (but a person keeps a name, and their state)
        if i.external_id.is_none() {
            i.external_id = Some(None);
        }
        if i.given.is_none() {
            i.given = Some(None);
        }
        if i.family.is_none() {
            i.family = Some(None);
        }
        if i.name.is_none() && i.given == Some(None) && i.family == Some(None) {
            i.name = Some(u.name.clone());
        }
        apply_user(&state, &c, &u, i).await?;
        Ok(scim_json(StatusCode::OK, user_json(&state, &load_user(&state, &c, &id).await?).await?))
    }
    .await;
    respond(&state, &c, "PUT /Users/{id}", r).await
}

/// The operations of a PATCH: (op, path, value).
fn operations(v: &Value) -> ScimResult<Vec<(String, Option<String>, Value)>> {
    let ops = v["Operations"]
        .as_array()
        .or_else(|| v["operations"].as_array())
        .ok_or_else(|| ScimError::new(StatusCode::BAD_REQUEST, Some("invalidSyntax"), "Operations is required"))?;
    ops.iter()
        .map(|o| {
            let op = o["op"]
                .as_str()
                .map(str::to_ascii_lowercase)
                .filter(|op| matches!(op.as_str(), "add" | "replace" | "remove"))
                .ok_or_else(|| ScimError::bad("op is add, replace or remove"))?;
            Ok((op, text(&o["path"]), o["value"].clone()))
        })
        .collect()
}

async fn patch_user(State(state): State<AppState>, headers: HeaderMap, Path(id): Path<String>, body: Bytes) -> Response {
    let c = match caller(&state, &headers).await {
        Ok(c) => c,
        Err(e) => return e.into_response(),
    };
    let r = async {
        let u = load_user(&state, &c, &id).await?;
        let mut i = UserIn::default();
        for (op, path, value) in operations(&parse(&body)?)? {
            match (op.as_str(), path) {
                ("remove", Some(p)) => i.unset(&p),
                ("remove", None) => return Err(ScimError::new(StatusCode::BAD_REQUEST, Some("noTarget"), "remove needs a path")),
                (_, Some(p)) => i.set(&p, &value)?,
                (_, None) => i.read(&value)?,
            }
        }
        apply_user(&state, &c, &u, i).await?;
        Ok(scim_json(StatusCode::OK, user_json(&state, &load_user(&state, &c, &id).await?).await?))
    }
    .await;
    respond(&state, &c, "PATCH /Users/{id}", r).await
}

/// `DELETE /Users/{id}`: the person is deactivated and leaves this directory's list; the account
/// and its history stay.
async fn delete_user(State(state): State<AppState>, headers: HeaderMap, Path(id): Path<String>) -> Response {
    let c = match caller(&state, &headers).await {
        Ok(c) => c,
        Err(e) => return e.into_response(),
    };
    let r = async {
        let u = load_user(&state, &c, &id).await?;
        set_active(&state, &c, &u, false).await?;
        sqlx::query("DELETE FROM scim_users WHERE user_id = $1").bind(u.id).execute(&state.db).await?;
        sqlx::query("DELETE FROM team_members m USING scim_groups g WHERE g.team_id = m.team_id AND g.token_id = $2 AND m.user_id = $1")
            .bind(u.id)
            .bind(c.id)
            .execute(&state.db)
            .await?;
        Ok(StatusCode::NO_CONTENT.into_response())
    }
    .await;
    respond(&state, &c, "DELETE /Users/{id}", r).await
}

// ---- groups (teams) ---------------------------------------------------------------------------------

#[derive(sqlx::FromRow)]
struct GroupRow {
    id: Uuid,
    name: String,
    created_at: DateTime<Utc>,
    external_id: Option<String>,
    updated_at: DateTime<Utc>,
}

const GROUP_SQL: &str =
    "SELECT t.id, t.name, t.created_at, g.external_id, g.updated_at FROM scim_groups g JOIN teams t ON t.id = g.team_id WHERE g.token_id = $1";

async fn group_json(state: &AppState, g: &GroupRow, members: bool) -> ScimResult<Value> {
    let mut v = json!({
        "schemas": [GROUP_SCHEMA], "id": g.id, "displayName": g.name,
        "meta": {"resourceType": "Group", "created": g.created_at, "lastModified": g.updated_at, "location": format!("{}/scim/v2/Groups/{}", state.config.base(), g.id)},
    });
    if let Some(e) = &g.external_id {
        v["externalId"] = json!(e);
    }
    if members {
        let list: Vec<(Uuid, String)> =
            sqlx::query_as("SELECT u.id, u.name FROM team_members m JOIN users u ON u.id = m.user_id WHERE m.team_id = $1 ORDER BY lower(u.name)")
                .bind(g.id)
                .fetch_all(&state.db)
                .await?;
        v["members"] = json!(list.iter().map(|(id, n)| json!({"value": id, "display": n, "type": "User"})).collect::<Vec<_>>());
    }
    Ok(v)
}

async fn load_group(state: &AppState, c: &Caller, id: &str) -> ScimResult<GroupRow> {
    let id: Uuid = id.parse().map_err(|_| ScimError::not_found("group"))?;
    sqlx::query_as(sqlx::AssertSqlSafe(format!("{GROUP_SQL} AND t.id = $2")))
        .bind(c.id)
        .bind(id)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| ScimError::not_found("group"))
}

/// How the team of a group is known: the same reference the sign-in group sync uses when the
/// token belongs to a single sign-on provider.
fn team_ref(c: &Caller, name: &str) -> String {
    match &c.provider_id {
        Some(p) => format!("sso:{p}:{name}"),
        None => format!("scim:{}:{name}", c.id),
    }
}

fn group_name(v: &Value) -> ScimResult<String> {
    text(v).filter(|n| n.chars().count() <= 200).ok_or_else(|| ScimError::bad("displayName is required (up to 200 characters)"))
}

/// The people named as members: `[{"value": id}]` (also one object, or bare ids).
async fn member_ids(state: &AppState, c: &Caller, v: &Value) -> ScimResult<Vec<Uuid>> {
    let items: Vec<Value> = match v {
        Value::Array(a) => a.clone(),
        Value::Null => vec![],
        one => vec![one.clone()],
    };
    let mut ids = Vec::new();
    for m in &items {
        let raw = m["value"].as_str().or_else(|| m.as_str()).ok_or_else(|| ScimError::bad("a member needs a value (the user's id)"))?;
        ids.push(raw.parse::<Uuid>().map_err(|_| ScimError::bad(format!("{raw} isn't a user id")))?);
    }
    let known: Vec<Uuid> =
        sqlx::query_scalar("SELECT id FROM users WHERE id = ANY($1) AND workspace_id = $2").bind(&ids).bind(c.workspace).fetch_all(&state.db).await?;
    if let Some(missing) = ids.iter().find(|i| !known.contains(i)) {
        return Err(ScimError::bad(format!("there is no user {missing}")));
    }
    ids.sort();
    ids.dedup();
    Ok(ids)
}

async fn add_members(state: &AppState, team: Uuid, ids: &[Uuid]) -> ScimResult<()> {
    sqlx::query("INSERT INTO team_members (team_id, user_id) SELECT $1, unnest($2::uuid[]) ON CONFLICT DO NOTHING")
        .bind(team)
        .bind(ids)
        .execute(&state.db)
        .await?;
    Ok(())
}

async fn set_members(state: &AppState, team: Uuid, ids: &[Uuid]) -> ScimResult<()> {
    sqlx::query("DELETE FROM team_members WHERE team_id = $1 AND NOT (user_id = ANY($2))").bind(team).bind(ids).execute(&state.db).await?;
    add_members(state, team, ids).await
}

async fn rename_group(state: &AppState, c: &Caller, g: &GroupRow, name: &str) -> ScimResult<()> {
    if name == g.name {
        return Ok(());
    }
    sqlx::query("UPDATE teams SET name = $2, external_ref = $3 WHERE id = $1").bind(g.id).bind(name).bind(team_ref(c, name)).execute(&state.db).await.map_err(
        |e| match ScimError::from(e) {
            x if x.status == StatusCode::CONFLICT => ScimError::conflict(format!("there is a team {name} already")),
            x => x,
        },
    )?;
    Ok(())
}

async fn touch_group(state: &AppState, c: &Caller, g: &GroupRow, action: &str) -> ScimResult<()> {
    sqlx::query("UPDATE scim_groups SET updated_at = now() WHERE team_id = $1").bind(g.id).execute(&state.db).await?;
    audit(state, c.workspace, None, action, Some(g.id.to_string()), json!({"token": c.id, "name": g.name})).await;
    Ok(())
}

async fn list_groups(State(state): State<AppState>, headers: HeaderMap, Query(q): Query<ListQuery>) -> Response {
    let c = match caller(&state, &headers).await {
        Ok(c) => c,
        Err(e) => return e.into_response(),
    };
    let r = async {
        let (start, count) = page(&q);
        let (cond, value) = match q.filter.as_deref().map(str::trim).filter(|f| !f.is_empty()) {
            None => ("TRUE", String::new()),
            Some(f) => {
                let (attr, value) = eq_filter(f)?;
                let cond = match attr.as_str() {
                    "displayname" => "lower(t.name) = lower($2)",
                    "externalid" => "g.external_id = $2",
                    "id" => "t.id::text = lower($2)",
                    "members.value" | "members" => "EXISTS (SELECT 1 FROM team_members m WHERE m.team_id = t.id AND m.user_id::text = lower($2))",
                    _ => return Err(ScimError::new(StatusCode::BAD_REQUEST, Some("invalidFilter"), format!("can't filter groups by {attr}"))),
                };
                (cond, value)
            }
        };
        let total: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT count(*) FROM scim_groups g JOIN teams t ON t.id = g.team_id WHERE g.token_id = $1 AND ($2 = $2) AND {cond}"
        )))
        .bind(c.id)
        .bind(&value)
        .fetch_one(&state.db)
        .await?;
        let rows: Vec<GroupRow> =
            sqlx::query_as(sqlx::AssertSqlSafe(format!("{GROUP_SQL} AND ($2 = $2) AND {cond} ORDER BY g.created_at, t.id OFFSET $3 LIMIT $4")))
                .bind(c.id)
                .bind(&value)
                .bind(start - 1)
                .bind(count)
                .fetch_all(&state.db)
                .await?;
        let members = !q.excluded_attributes.as_deref().unwrap_or("").split(',').any(|a| a.trim().eq_ignore_ascii_case("members"));
        let mut out = Vec::with_capacity(rows.len());
        for g in &rows {
            out.push(group_json(&state, g, members).await?);
        }
        Ok(list_response(total, start, out))
    }
    .await;
    respond(&state, &c, "GET /Groups", r).await
}

async fn get_group(State(state): State<AppState>, headers: HeaderMap, Path(id): Path<String>, Query(q): Query<ListQuery>) -> Response {
    let c = match caller(&state, &headers).await {
        Ok(c) => c,
        Err(e) => return e.into_response(),
    };
    let r = async {
        let members = !q.excluded_attributes.as_deref().unwrap_or("").split(',').any(|a| a.trim().eq_ignore_ascii_case("members"));
        Ok(scim_json(StatusCode::OK, group_json(&state, &load_group(&state, &c, &id).await?, members).await?))
    }
    .await;
    respond(&state, &c, "GET /Groups/{id}", r).await
}

async fn create_group(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    let c = match caller(&state, &headers).await {
        Ok(c) => c,
        Err(e) => return e.into_response(),
    };
    let r = async {
        let v = parse(&body)?;
        let name = group_name(&v["displayName"])?;
        let members = member_ids(&state, &c, &v["members"]).await?;
        let ext_ref = team_ref(&c, &name);
        // the team the sign-in sync made for this group already, else a new one; a local team
        // (or another directory's) of the same name is never taken over: ours gets a suffix
        let mut team: Option<Uuid> = sqlx::query_scalar("SELECT id FROM teams WHERE workspace_id = $1 AND external_ref = $2")
            .bind(c.workspace)
            .bind(&ext_ref)
            .fetch_optional(&state.db)
            .await?;
        if let Some(t) = team {
            let taken: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM scim_groups WHERE team_id = $1)").bind(t).fetch_one(&state.db).await?;
            if taken {
                return Err(ScimError::conflict(format!("group {name} exists already")));
            }
        }
        let suffix = c.provider_id.clone().unwrap_or_else(|| "SCIM".into());
        for candidate in [name.clone(), format!("{name} ({suffix})")] {
            if team.is_some() {
                break;
            }
            team = sqlx::query_scalar("INSERT INTO teams (id, workspace_id, name, external_ref) VALUES ($1, $2, $3, $4) ON CONFLICT DO NOTHING RETURNING id")
                .bind(new_id())
                .bind(c.workspace)
                .bind(&candidate)
                .bind(&ext_ref)
                .fetch_optional(&state.db)
                .await?;
        }
        let team = team.ok_or_else(|| ScimError::conflict(format!("there is a team {name} already")))?;
        sqlx::query("INSERT INTO scim_groups (team_id, token_id, external_id) VALUES ($1, $2, $3)")
            .bind(team)
            .bind(c.id)
            .bind(text(&v["externalId"]))
            .execute(&state.db)
            .await?;
        add_members(&state, team, &members).await?;
        let g = load_group(&state, &c, &team.to_string()).await?;
        audit(&state, c.workspace, None, "scim.group.created", Some(team.to_string()), json!({"token": c.id, "name": g.name, "members": members.len()})).await;
        let mut res = scim_json(StatusCode::CREATED, group_json(&state, &g, true).await?);
        if let Ok(loc) = format!("{}/scim/v2/Groups/{team}", state.config.base()).parse() {
            res.headers_mut().insert(header::LOCATION, loc);
        }
        Ok(res)
    }
    .await;
    respond(&state, &c, "POST /Groups", r).await
}

async fn replace_group(State(state): State<AppState>, headers: HeaderMap, Path(id): Path<String>, body: Bytes) -> Response {
    let c = match caller(&state, &headers).await {
        Ok(c) => c,
        Err(e) => return e.into_response(),
    };
    let r = async {
        let g = load_group(&state, &c, &id).await?;
        let v = parse(&body)?;
        let name = group_name(&v["displayName"])?;
        let members = member_ids(&state, &c, &v["members"]).await?;
        rename_group(&state, &c, &g, &name).await?;
        set_members(&state, g.id, &members).await?;
        sqlx::query("UPDATE scim_groups SET external_id = $2 WHERE team_id = $1").bind(g.id).bind(text(&v["externalId"])).execute(&state.db).await?;
        touch_group(&state, &c, &g, "scim.group.updated").await?;
        Ok(scim_json(StatusCode::OK, group_json(&state, &load_group(&state, &c, &id).await?, true).await?))
    }
    .await;
    respond(&state, &c, "PUT /Groups/{id}", r).await
}

/// The user id in `members[value eq "…"]`.
fn member_in_path(path: &str) -> Option<Uuid> {
    let lower = path.to_ascii_lowercase();
    let rest = lower.strip_prefix("members[")?;
    let at = rest.find(" eq ")?;
    rest[at + 4..].trim().trim_end_matches(']').trim().trim_matches('"').parse().ok()
}

async fn patch_group(State(state): State<AppState>, headers: HeaderMap, Path(id): Path<String>, body: Bytes) -> Response {
    let c = match caller(&state, &headers).await {
        Ok(c) => c,
        Err(e) => return e.into_response(),
    };
    let r = async {
        let g = load_group(&state, &c, &id).await?;
        for (op, path, value) in operations(&parse(&body)?)? {
            let p = path.as_deref().map(str::to_ascii_lowercase);
            match (op.as_str(), p.as_deref()) {
                ("add", Some("members")) => add_members(&state, g.id, &member_ids(&state, &c, &value).await?).await?,
                ("replace", Some("members")) => set_members(&state, g.id, &member_ids(&state, &c, &value).await?).await?,
                ("remove", Some("members")) if value.is_null() => set_members(&state, g.id, &[]).await?,
                ("remove", Some("members")) => {
                    let ids = member_ids(&state, &c, &value).await?;
                    sqlx::query("DELETE FROM team_members WHERE team_id = $1 AND user_id = ANY($2)").bind(g.id).bind(&ids).execute(&state.db).await?;
                }
                ("remove", Some(p)) if p.starts_with("members[") => {
                    let user = member_in_path(p)
                        .ok_or_else(|| ScimError::new(StatusCode::BAD_REQUEST, Some("invalidPath"), "members[value eq \"<user id>\"] is expected"))?;
                    sqlx::query("DELETE FROM team_members WHERE team_id = $1 AND user_id = $2").bind(g.id).bind(user).execute(&state.db).await?;
                }
                ("add" | "replace", Some("displayname")) => rename_group(&state, &c, &g, &group_name(&value)?).await?,
                ("add" | "replace", Some("externalid")) => {
                    sqlx::query("UPDATE scim_groups SET external_id = $2 WHERE team_id = $1").bind(g.id).bind(text(&value)).execute(&state.db).await?;
                }
                ("add" | "replace", None) => {
                    if !value["displayName"].is_null() {
                        rename_group(&state, &c, &g, &group_name(&value["displayName"])?).await?;
                    }
                    if !value["externalId"].is_null() {
                        sqlx::query("UPDATE scim_groups SET external_id = $2 WHERE team_id = $1")
                            .bind(g.id)
                            .bind(text(&value["externalId"]))
                            .execute(&state.db)
                            .await?;
                    }
                    if !value["members"].is_null() {
                        let ids = member_ids(&state, &c, &value["members"]).await?;
                        if op == "add" { add_members(&state, g.id, &ids).await? } else { set_members(&state, g.id, &ids).await? }
                    }
                }
                ("remove", None) => return Err(ScimError::new(StatusCode::BAD_REQUEST, Some("noTarget"), "remove needs a path")),
                // attributes TaskCraft doesn't keep
                _ => {}
            }
        }
        touch_group(&state, &c, &g, "scim.group.updated").await?;
        Ok(scim_json(StatusCode::OK, group_json(&state, &load_group(&state, &c, &id).await?, true).await?))
    }
    .await;
    respond(&state, &c, "PATCH /Groups/{id}", r).await
}

/// `DELETE /Groups/{id}`: the team goes, with its project grants (its people stay).
async fn delete_group(State(state): State<AppState>, headers: HeaderMap, Path(id): Path<String>) -> Response {
    let c = match caller(&state, &headers).await {
        Ok(c) => c,
        Err(e) => return e.into_response(),
    };
    let r = async {
        let g = load_group(&state, &c, &id).await?;
        sqlx::query("DELETE FROM project_members WHERE subject_type = 'team' AND subject_id = $1").bind(g.id).execute(&state.db).await?;
        sqlx::query("DELETE FROM teams WHERE id = $1").bind(g.id).execute(&state.db).await?;
        audit(&state, c.workspace, None, "scim.group.deleted", Some(g.id.to_string()), json!({"token": c.id, "name": g.name})).await;
        Ok(StatusCode::NO_CONTENT.into_response())
    }
    .await;
    respond(&state, &c, "DELETE /Groups/{id}", r).await
}

// ---- administration (/api/admin/scim) ---------------------------------------------------------------

/// `GET /admin/scim`: the address identity providers call, the tokens with their last request and
/// last error, and the single sign-on providers a token can belong to.
pub async fn admin_list(State(state): State<AppState>, CurrentUser(user): CurrentUser) -> ApiResult<Json<Value>> {
    crate::auth::require_admin(&user)?;
    type Row = (Uuid, String, Option<String>, String, DateTime<Utc>, Option<DateTime<Utc>>, i64, Option<String>, Option<DateTime<Utc>>, i64, i64, i64);
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT t.id, t.name, t.provider_id, t.default_role, t.created_at, t.last_used_at, t.requests, t.last_error, t.last_error_at,
                (SELECT count(*) FROM scim_users s WHERE s.token_id = t.id),
                (SELECT count(*) FROM scim_users s JOIN users u ON u.id = s.user_id WHERE s.token_id = t.id AND u.status = 'deactivated'),
                (SELECT count(*) FROM scim_groups g WHERE g.token_id = t.id)
           FROM scim_tokens t WHERE t.workspace_id = $1 ORDER BY t.created_at",
    )
    .bind(user.workspace_id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(json!({
        "baseUrl": format!("{}/scim/v2", state.config.base()),
        "tokens": rows.iter().map(|(id, name, provider, role, created, used, requests, error, error_at, users, inactive, groups)| json!({
            "id": id, "name": name, "providerId": provider, "defaultRole": role, "createdAt": created, "lastUsedAt": used, "requests": requests,
            "lastError": error, "lastErrorAt": error_at, "users": users, "deactivated": inactive, "groups": groups,
        })).collect::<Vec<_>>(),
        "providers": state.providers.iter().map(|p| json!({"id": p.id(), "name": p.display_name()})).collect::<Vec<_>>(),
    })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewToken {
    name: String,
    provider_id: Option<String>,
    default_role: Option<String>,
}

/// `POST /admin/scim`: a token for an identity provider; shown this once.
pub async fn admin_create(State(state): State<AppState>, CurrentUser(user): CurrentUser, Json(req): Json<NewToken>) -> ApiResult<(StatusCode, Json<Value>)> {
    crate::auth::require_admin(&user)?;
    let name = req.name.trim();
    if name.is_empty() || name.chars().count() > 80 {
        return Err(ApiError::bad("give the token a name (up to 80 characters), e.g. the identity provider's"));
    }
    let provider = req.provider_id.as_deref().map(str::trim).filter(|p| !p.is_empty());
    if let Some(p) = provider
        && crate::sso::find(&state, p).is_none()
    {
        return Err(ApiError::bad(format!("there is no single sign-on provider {p}")));
    }
    let role = match req.default_role.as_deref() {
        None | Some("member") => "member",
        Some("guest") => "guest",
        Some(_) => return Err(ApiError::bad("defaultRole is member or guest")),
    };
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM scim_tokens WHERE workspace_id = $1").bind(user.workspace_id).fetch_one(&state.db).await?;
    if count >= 20 {
        return Err(ApiError::bad("there are 20 provisioning tokens already: delete one you don't use"));
    }
    let token = format!("{TOKEN_PREFIX}{}", crate::auth::random_token());
    let id = new_id();
    sqlx::query("INSERT INTO scim_tokens (id, workspace_id, name, token_hash, provider_id, default_role, created_by) VALUES ($1, $2, $3, $4, $5, $6, $7)")
        .bind(id)
        .bind(user.workspace_id)
        .bind(name)
        .bind(crate::auth::token_hash(&token))
        .bind(provider)
        .bind(role)
        .bind(user.id)
        .execute(&state.db)
        .await?;
    audit(&state, user.workspace_id, Some(user.id), "scim.token.created", Some(id.to_string()), json!({"name": name, "provider": provider})).await;
    Ok((StatusCode::CREATED, Json(json!({"id": id, "token": token}))))
}

/// `DELETE /admin/scim/{id}`: the identity provider can't call any more. The people and teams it
/// provisioned stay as they are.
pub async fn admin_delete(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    crate::auth::require_admin(&user)?;
    let name: Option<String> = sqlx::query_scalar("DELETE FROM scim_tokens WHERE id = $1 AND workspace_id = $2 RETURNING name")
        .bind(id)
        .bind(user.workspace_id)
        .fetch_optional(&state.db)
        .await?;
    let name = name.ok_or(ApiError::NotFound("token"))?;
    audit(&state, user.workspace_id, Some(user.id), "scim.token.deleted", Some(id.to_string()), json!({"name": name})).await;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_and_paths() {
        assert_eq!(eq_filter("userName eq \"mia@example.com\"").unwrap(), ("username".into(), "mia@example.com".into()));
        assert_eq!(eq_filter("emails[type eq \"work\"].value eq \"a b@c\"").unwrap().0, "emails[type eq \"work\"].value");
        assert_eq!(eq_filter("displayName Eq \"Dev \\\"Ops\\\"\"").unwrap().1, "Dev \"Ops\"");
        assert!(eq_filter("userName sw \"mia\"").is_err());
        let id = Uuid::new_v4();
        assert_eq!(member_in_path(&format!("members[value eq \"{id}\"]")), Some(id));
        assert_eq!(member_in_path("members[value eq \"x\"]"), None);
        assert_eq!(truth(&json!("False")), Some(false));
        assert_eq!(truth(&json!(true)), Some(true));
        assert_eq!(truth(&json!(1)), None);
    }

    #[test]
    fn what_a_request_says() {
        let mut i = UserIn::default();
        i.read(&json!({"userName": "mia@example.com", "name": {"givenName": "Mia", "familyName": "Member"}, "emails": [{"value": "x@example.com"}, {"value": "mia@example.com", "primary": true}], "active": "False"}))
            .unwrap();
        assert_eq!(i.user_name.as_deref(), Some("mia@example.com"));
        assert_eq!(i.email, Some(Some("mia@example.com".into())));
        assert_eq!((i.given.clone().flatten().as_deref(), i.family.clone().flatten().as_deref()), (Some("Mia"), Some("Member")));
        assert_eq!(i.active, Some(false));
        let mut p = UserIn::default();
        p.read(&json!({"name.givenName": "Maria", "active": true})).unwrap();
        p.set("emails[type eq \"work\"].value", &json!("maria@example.com")).unwrap();
        assert_eq!((p.given.flatten().as_deref(), p.active, p.email.flatten().as_deref()), (Some("Maria"), Some(true), Some("maria@example.com")));
    }
}
