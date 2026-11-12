//! Setup, sign-in, the signed-in person (profile, password, preferences, API tokens) and
//! one-time links (invitations, password resets).

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::app::{AppState, audit, new_id};
use crate::auth::{
    API_TOKEN_PREFIX, CurrentUser, MaybeUser, SessionUser, create_session, hash_password, random_token, session_cookie, session_token, token_hash,
    verify_password,
};
use crate::error::{ApiError, ApiResult};

pub async fn setup_status(State(state): State<AppState>) -> ApiResult<Json<Value>> {
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM workspaces").fetch_one(&state.db).await?;
    Ok(Json(json!({ "needsSetup": n == 0 })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetupRequest {
    workspace_name: String,
    name: String,
    email: String,
    password: String,
    /// fill the workspace with sample projects to explore
    #[serde(default)]
    demo: bool,
}

pub fn validate_password(p: &str) -> ApiResult<()> {
    if p.chars().count() < 8 {
        return Err(ApiError::bad("the password must be at least 8 characters"));
    }
    Ok(())
}

pub fn validate_email(e: &str) -> ApiResult<String> {
    let e = e.trim().to_lowercase();
    if e.len() > 254 || !e.contains('@') || e.starts_with('@') || e.ends_with('@') || e.contains(char::is_whitespace) {
        return Err(ApiError::bad("invalid e-mail address"));
    }
    Ok(e)
}

/// First-run setup: the workspace and its owner. Works once.
pub async fn setup(State(state): State<AppState>, headers: HeaderMap, Json(req): Json<SetupRequest>) -> ApiResult<Response> {
    validate_password(&req.password)?;
    let email = validate_email(&req.email)?;
    if req.workspace_name.trim().is_empty() || req.name.trim().is_empty() {
        return Err(ApiError::bad("workspace name and your name are required"));
    }
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM workspaces").fetch_one(&state.db).await?;
    if n > 0 {
        return Err(ApiError::Conflict("already set up".into()));
    }
    let hash = hash_password(req.password).await?;
    let mut tx = state.db.begin().await?;
    sqlx::query("LOCK TABLE workspaces IN EXCLUSIVE MODE").execute(&mut *tx).await?;
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM workspaces").fetch_one(&mut *tx).await?;
    if n > 0 {
        return Err(ApiError::Conflict("already set up".into()));
    }
    let ws = new_id();
    sqlx::query("INSERT INTO workspaces (id, name) VALUES ($1, $2)").bind(ws).bind(req.workspace_name.trim()).execute(&mut *tx).await?;
    let uid = new_id();
    sqlx::query("INSERT INTO users (id, workspace_id, email, name, password_hash, role) VALUES ($1, $2, $3, $4, $5, 'owner')")
        .bind(uid)
        .bind(ws)
        .bind(&email)
        .bind(req.name.trim())
        .bind(hash)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    audit(&state, ws, Some(uid), "workspace.setup", None, json!({})).await;
    if req.demo
        && let Err(e) = crate::demo::seed(&state, uid).await
    {
        tracing::warn!(error = ?e, "sample data");
    }
    let cookie = create_session(&state, uid, &headers).await?;
    Ok(([(header::SET_COOKIE, cookie)], Json(json!({ "ok": true }))).into_response())
}

#[derive(Deserialize)]
pub struct LoginRequest {
    email: String,
    password: String,
}

pub async fn login(State(state): State<AppState>, headers: HeaderMap, Json(req): Json<LoginRequest>) -> ApiResult<Response> {
    // single sign-on: local passwords may be off; directory (LDAP) accounts still sign in here
    if !crate::sso::password_form_enabled(&state) {
        return Err(ApiError::bad("password sign-in is turned off; use single sign-on"));
    }
    let email = req.email.trim().to_lowercase();
    if !crate::auth::login_allowed(&state, &email).await {
        return Err(ApiError::bad("too many failed attempts, try again in 15 minutes"));
    }
    let row: Option<(Uuid, Uuid, Option<String>, String)> =
        sqlx::query_as("SELECT id, workspace_id, password_hash, status FROM users WHERE lower(email) = $1").bind(&email).fetch_optional(&state.db).await?;
    static DUMMY: tokio::sync::OnceCell<String> = tokio::sync::OnceCell::const_new();
    let dummy = DUMMY.get_or_try_init(|| hash_password("taskcraft-timing-dummy".into())).await?.clone();
    let (ok, user) = match row {
        Some((id, ws, Some(hash), status)) if status == "active" && crate::sso::passwords_enabled(&state) => {
            (verify_password(req.password.clone(), hash).await, Some((id, ws)))
        }
        _ => {
            let _ = verify_password(req.password.clone(), dummy).await;
            (false, None)
        }
    };
    // single sign-on: directory accounts (LDAP / Active Directory)
    let (ok, user) = if ok { (ok, user) } else { crate::sso::ldap::login(&state, &req.email, &req.password).await?.map_or((false, user), |u| (true, Some(u))) };
    match (ok, user) {
        (true, Some((id, ws))) => {
            crate::auth::clear_login_failures(&state, &email).await;
            // two-factor authentication: no session yet, the second step first
            if let Some(step) = crate::mfa::second_step(&state, id).await? {
                return Ok(Json(step).into_response());
            }
            let cookie = create_session(&state, id, &headers).await?;
            audit(&state, ws, Some(id), "auth.login", None, json!({})).await;
            Ok(([(header::SET_COOKIE, cookie)], Json(json!({ "ok": true }))).into_response())
        }
        _ => {
            crate::auth::record_login_failure(&state, &email).await;
            Err(ApiError::Denied("wrong e-mail or password".into()))
        }
    }
}

pub async fn logout(State(state): State<AppState>, headers: HeaderMap) -> ApiResult<Response> {
    if let Some(t) = session_token(&headers) {
        sqlx::query("DELETE FROM sessions WHERE token_hash = $1").bind(token_hash(t)).execute(&state.db).await?;
    }
    Ok(([(header::SET_COOKIE, session_cookie("", 0, state.config.secure_cookies))], StatusCode::NO_CONTENT).into_response())
}

pub async fn me(
    State(state): State<AppState>,
    MaybeUser(user): MaybeUser,
    enroll: Option<axum::Extension<crate::mfa::MustEnrollTwoFactor>>,
) -> ApiResult<Json<Value>> {
    let Some(user) = user else {
        return Ok(Json(json!({ "user": null, "workspace": null })));
    };
    let (ws_name, settings): (String, Value) =
        sqlx::query_as("SELECT name, settings FROM workspaces WHERE id = $1").bind(user.workspace_id).fetch_one(&state.db).await?;
    let (title, locale, timezone, prefs): (Option<String>, Option<String>, Option<String>, Value) =
        sqlx::query_as("SELECT title, locale, timezone, preferences FROM users WHERE id = $1").bind(user.id).fetch_one(&state.db).await?;
    let wiki = crate::wikicraft::conn(&state, &user).await;
    let mut prefs = prefs;
    let personal_wiki = prefs.get("wikicraftToken").and_then(|v| v.as_str()).is_some_and(|t| !t.is_empty());
    if let Some(o) = prefs.as_object_mut() {
        o.remove("wikicraftToken");
    }
    Ok(Json(json!({
        "user": user,
        "profile": {"title": title, "locale": locale, "timezone": timezone},
        "preferences": prefs,
        "workspace": {
            "id": user.workspace_id, "name": ws_name,
            "accentColor": settings["accentColor"], "defaultLanguage": settings["defaultLanguage"], "weekStart": settings["weekStart"],
        },
        "wikicraft": {
            "url": crate::wikicraft::base_url(&state),
            "connected": wiki.is_some(),
            "personal": personal_wiki,
        },
        "baseUrl": state.config.base(),
        // the workspace requires two-factor authentication and this person hasn't set it up
        "mustSetUpTwoFactor": enroll.is_some(),
    })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileUpdate {
    name: Option<String>,
    title: Option<String>,
    locale: Option<String>,
    timezone: Option<String>,
}

pub async fn update_profile(State(state): State<AppState>, CurrentUser(user): CurrentUser, Json(req): Json<ProfileUpdate>) -> ApiResult<StatusCode> {
    if let Some(n) = &req.name {
        let n = n.trim();
        if n.is_empty() {
            return Err(ApiError::bad("the name can't be empty"));
        }
        sqlx::query("UPDATE users SET name = $2 WHERE id = $1").bind(user.id).bind(n.chars().take(100).collect::<String>()).execute(&state.db).await?;
    }
    let clean = |s: &Option<String>| s.as_ref().map(|s| s.trim().chars().take(100).collect::<String>()).filter(|s| !s.is_empty());
    if req.title.is_some() {
        sqlx::query("UPDATE users SET title = $2 WHERE id = $1").bind(user.id).bind(clean(&req.title)).execute(&state.db).await?;
    }
    if req.locale.is_some() {
        sqlx::query("UPDATE users SET locale = $2 WHERE id = $1").bind(user.id).bind(clean(&req.locale)).execute(&state.db).await?;
    }
    if req.timezone.is_some() {
        sqlx::query("UPDATE users SET timezone = $2 WHERE id = $1").bind(user.id).bind(clean(&req.timezone)).execute(&state.db).await?;
    }
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PasswordChange {
    current: String,
    new_password: String,
}

pub async fn change_password(
    State(state): State<AppState>,
    SessionUser(user): SessionUser,
    headers: HeaderMap,
    Json(req): Json<PasswordChange>,
) -> ApiResult<StatusCode> {
    if !crate::sso::passwords_enabled(&state) {
        return Err(ApiError::bad("password sign-in is turned off; use single sign-on"));
    }
    validate_password(&req.new_password)?;
    let hash: Option<String> = sqlx::query_scalar("SELECT password_hash FROM users WHERE id = $1").bind(user.id).fetch_one(&state.db).await?;
    let key = format!("password:{}", user.id);
    if !crate::auth::login_allowed(&state, &key).await {
        return Err(ApiError::bad("too many wrong passwords, try again in 15 minutes"));
    }
    if !match hash {
        Some(h) => verify_password(req.current, h).await,
        None => false,
    } {
        crate::auth::record_login_failure(&state, &key).await;
        return Err(ApiError::Denied("the current password isn't right".into()));
    }
    let new_hash = hash_password(req.new_password).await?;
    sqlx::query("UPDATE users SET password_hash = $2 WHERE id = $1").bind(user.id).bind(new_hash).execute(&state.db).await?;
    // other sessions end
    if let Some(t) = session_token(&headers) {
        sqlx::query("DELETE FROM sessions WHERE user_id = $1 AND token_hash <> $2").bind(user.id).bind(token_hash(t)).execute(&state.db).await?;
    }
    audit(&state, user.workspace_id, Some(user.id), "auth.password_changed", None, json!({})).await;
    Ok(StatusCode::NO_CONTENT)
}

/// Merges preferences (theme, muted notification kinds, board options, WikiCraft token).
pub async fn put_preferences(State(state): State<AppState>, CurrentUser(user): CurrentUser, Json(req): Json<Value>) -> ApiResult<StatusCode> {
    let obj = req.as_object().ok_or_else(|| ApiError::bad("preferences must be an object"))?;
    if serde_json::to_string(&req).map(|s| s.len()).unwrap_or(0) > 20_000 {
        return Err(ApiError::bad("too many preferences"));
    }
    if let Some(t) = obj.get("wikicraftToken").and_then(|v| v.as_str())
        && !t.is_empty()
        && !t.starts_with("wct_")
    {
        return Err(ApiError::bad("WikiCraft tokens start with wct_ (create one in WikiCraft under Profile → API tokens)"));
    }
    let mut cur: Value = sqlx::query_scalar("SELECT preferences FROM users WHERE id = $1").bind(user.id).fetch_one(&state.db).await?;
    if let Some(c) = cur.as_object_mut() {
        for (k, v) in obj {
            if v.is_null() {
                c.remove(k);
            } else {
                c.insert(k.clone(), v.clone());
            }
        }
    }
    sqlx::query("UPDATE users SET preferences = $2 WHERE id = $1").bind(user.id).bind(cur).execute(&state.db).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn list_tokens(State(state): State<AppState>, SessionUser(user): SessionUser) -> ApiResult<Json<Value>> {
    let rows: Vec<(Uuid, String, String, chrono::DateTime<chrono::Utc>, Option<chrono::DateTime<chrono::Utc>>, Option<chrono::DateTime<chrono::Utc>>)> =
        sqlx::query_as("SELECT id, name, scope, created_at, expires_at, last_used_at FROM api_tokens WHERE user_id = $1 ORDER BY created_at DESC")
            .bind(user.id)
            .fetch_all(&state.db)
            .await?;
    Ok(Json(json!(
        rows.into_iter()
            .map(|(id, name, scope, created, expires, used)| json!({"id": id, "name": name, "scope": scope, "createdAt": created, "expiresAt": expires, "lastUsedAt": used}))
            .collect::<Vec<_>>()
    )))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewToken {
    name: String,
    /// read | write
    scope: Option<String>,
    expires_in_days: Option<i64>,
}

pub async fn create_token(State(state): State<AppState>, SessionUser(user): SessionUser, Json(req): Json<NewToken>) -> ApiResult<(StatusCode, Json<Value>)> {
    let name = req.name.trim();
    if name.is_empty() {
        return Err(ApiError::bad("give the token a name"));
    }
    let scope = match req.scope.as_deref() {
        None | Some("write") => "write",
        Some("read") => "read",
        _ => return Err(ApiError::bad("scope must be read or write")),
    };
    let token = format!("{API_TOKEN_PREFIX}{}", random_token());
    let id = new_id();
    sqlx::query(
        "INSERT INTO api_tokens (id, user_id, name, token_hash, scope, expires_at) VALUES ($1, $2, $3, $4, $5, CASE WHEN $6::int IS NULL THEN NULL ELSE now() + make_interval(days => $6) END)",
    )
    .bind(id)
    .bind(user.id)
    .bind(name)
    .bind(token_hash(&token))
    .bind(scope)
    .bind(req.expires_in_days.map(|d| d.clamp(1, 3650) as i32))
    .execute(&state.db)
    .await?;
    audit(&state, user.workspace_id, Some(user.id), "token.created", Some(name.to_string()), json!({"scope": scope})).await;
    Ok((StatusCode::CREATED, Json(json!({ "id": id, "token": token }))))
}

pub async fn delete_token(State(state): State<AppState>, SessionUser(user): SessionUser, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    sqlx::query("DELETE FROM api_tokens WHERE id = $1 AND user_id = $2").bind(id).bind(user.id).execute(&state.db).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Creates a one-time link (invitation or password reset), valid for 7 days.
pub async fn user_link(state: &AppState, user: Uuid, kind: &str) -> ApiResult<String> {
    let token = random_token();
    sqlx::query("DELETE FROM user_links WHERE user_id = $1 AND kind = $2").bind(user).bind(kind).execute(&state.db).await?;
    sqlx::query("INSERT INTO user_links (token_hash, user_id, kind, expires_at) VALUES ($1, $2, $3, now() + interval '7 days')")
        .bind(token_hash(&token))
        .bind(user)
        .bind(kind)
        .execute(&state.db)
        .await?;
    Ok(format!("{}/welcome?token={token}", state.config.base()))
}

#[derive(Deserialize)]
pub struct LinkQuery {
    token: String,
}

pub async fn link_info(State(state): State<AppState>, Query(q): Query<LinkQuery>) -> ApiResult<Json<Value>> {
    let row: Option<(String, String, Option<String>, String)> = sqlx::query_as(
        "SELECT l.kind, u.name, u.email, w.name FROM user_links l JOIN users u ON u.id = l.user_id JOIN workspaces w ON w.id = u.workspace_id
          WHERE l.token_hash = $1 AND l.expires_at > now() AND u.status <> 'deactivated'",
    )
    .bind(token_hash(&q.token))
    .fetch_optional(&state.db)
    .await?;
    let (kind, name, email, ws) = row.ok_or_else(|| ApiError::bad("this link has expired or was already used"))?;
    Ok(Json(json!({"kind": kind, "name": name, "email": email, "workspace": ws})))
}

#[derive(Deserialize)]
pub struct LinkAccept {
    token: String,
    password: String,
    name: Option<String>,
}

pub async fn link_accept(State(state): State<AppState>, headers: HeaderMap, Json(req): Json<LinkAccept>) -> ApiResult<Response> {
    // single sign-on only: invited people join by signing in with their provider
    if !crate::sso::passwords_enabled(&state) {
        return Err(ApiError::bad("password sign-in is turned off; sign in with single sign-on instead"));
    }
    validate_password(&req.password)?;
    let row: Option<(Uuid, Uuid)> = sqlx::query_as(
        "DELETE FROM user_links l USING users u WHERE l.token_hash = $1 AND l.expires_at > now() AND u.id = l.user_id AND u.status <> 'deactivated'
         RETURNING u.id, u.workspace_id",
    )
    .bind(token_hash(&req.token))
    .fetch_optional(&state.db)
    .await?;
    let (uid, ws) = row.ok_or_else(|| ApiError::bad("this link has expired or was already used"))?;
    let hash = hash_password(req.password).await?;
    sqlx::query("UPDATE users SET password_hash = $2, status = 'active', name = COALESCE(NULLIF($3, ''), name) WHERE id = $1")
        .bind(uid)
        .bind(hash)
        .bind(req.name.as_deref().map(str::trim).unwrap_or(""))
        .execute(&state.db)
        .await?;
    sqlx::query("DELETE FROM sessions WHERE user_id = $1").bind(uid).execute(&state.db).await?;
    audit(&state, ws, Some(uid), "auth.link_used", None, json!({})).await;
    // a reset link alone doesn't get past two-factor authentication: the second step follows on /login?mfa=1
    if let Some(step) = crate::mfa::second_step_cookie(&state, uid, None).await? {
        return Ok(([(header::SET_COOKIE, step)], Json(json!({ "mfaRequired": true }))).into_response());
    }
    let cookie = create_session(&state, uid, &headers).await?;
    Ok(([(header::SET_COOKIE, cookie)], Json(json!({ "ok": true }))).into_response())
}
