//! People and teams; user administration.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::app::{AppState, audit, new_id};
use crate::auth::{CurrentUser, hash_password, require_admin};
use crate::error::{ApiError, ApiResult};

/// Everyone active in the workspace (people pickers, mentions).
pub async fn people(State(state): State<AppState>, CurrentUser(user): CurrentUser) -> ApiResult<Json<Value>> {
    let rows: Vec<(Uuid, String, Option<String>, String, Option<String>)> =
        sqlx::query_as("SELECT id, name, email, role, title FROM users WHERE workspace_id = $1 AND status = 'active' ORDER BY lower(name)")
            .bind(user.workspace_id)
            .fetch_all(&state.db)
            .await?;
    // guests don't see e-mail addresses
    let guest = user.is_guest();
    Ok(Json(json!(
        rows.into_iter()
            .map(|(id, name, email, role, title)| json!({"id": id, "name": name, "email": if guest { None } else { email }, "role": role, "title": title}))
            .collect::<Vec<_>>()
    )))
}

/// One person: profile and what they're working on.
pub async fn person(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(id): Path<Uuid>) -> ApiResult<Json<Value>> {
    let row: Option<(Uuid, String, Option<String>, String, Option<String>, String)> =
        sqlx::query_as("SELECT id, name, email, role, title, status FROM users WHERE id = $1 AND workspace_id = $2")
            .bind(id)
            .bind(user.workspace_id)
            .fetch_optional(&state.db)
            .await?;
    let (id, name, email, role, title, status) = row.ok_or(ApiError::NotFound("person"))?;
    let teams: Vec<(Uuid, String)> =
        sqlx::query_as("SELECT t.id, t.name FROM teams t JOIN team_members m ON m.team_id = t.id WHERE m.user_id = $1 ORDER BY t.name")
            .bind(id)
            .fetch_all(&state.db)
            .await?;
    Ok(Json(json!({
        "id": id, "name": name, "email": if user.is_guest() { None } else { email }, "role": role, "title": title, "status": status,
        "teams": teams.iter().map(|(id, n)| json!({"id": id, "name": n})).collect::<Vec<_>>(),
    })))
}

/// All accounts (admins).
pub async fn list(State(state): State<AppState>, CurrentUser(user): CurrentUser) -> ApiResult<Json<Value>> {
    require_admin(&user)?;
    type Row = (Uuid, String, Option<String>, String, String, Option<String>, chrono::DateTime<chrono::Utc>, Option<chrono::DateTime<chrono::Utc>>, bool);
    let rows: Vec<Row> =
        sqlx::query_as("SELECT id, name, email, role, status, title, created_at, last_login_at, preferences ? 'removed' FROM users WHERE workspace_id = $1 ORDER BY lower(name)")
            .bind(user.workspace_id)
            .fetch_all(&state.db)
            .await?;
    // how each person signs in: password and / or single sign-on providers
    let mut sign_in = crate::sso::sign_in_methods(&state, user.workspace_id).await?;
    Ok(Json(json!(
        rows.into_iter()
            .map(|(id, name, email, role, status, title, created, last, removed)| json!({
                "id": id, "name": name, "email": email, "role": role, "status": status, "title": title, "createdAt": created, "lastLoginAt": last,
                "removed": removed,
                "signIn": sign_in.remove(&id),
            }))
            .collect::<Vec<_>>()
    )))
}

#[derive(Deserialize)]
pub struct NewUser {
    name: String,
    email: String,
    role: Option<String>,
    /// set a password right away; without one an invitation link is returned (and e-mailed
    /// when mail is configured)
    password: Option<String>,
}

fn valid_role(r: &str) -> ApiResult<()> {
    if matches!(r, "admin" | "member" | "guest") { Ok(()) } else { Err(ApiError::bad("role must be admin, member or guest")) }
}

pub async fn create(State(state): State<AppState>, CurrentUser(user): CurrentUser, Json(req): Json<NewUser>) -> ApiResult<(StatusCode, Json<Value>)> {
    require_admin(&user)?;
    let email = super::auth::validate_email(&req.email)?;
    let role = req.role.unwrap_or_else(|| "member".into());
    valid_role(&role)?;
    let name = req.name.trim();
    if name.is_empty() {
        return Err(ApiError::bad("a name is required"));
    }
    if req.password.is_some() && !crate::sso::passwords_enabled(&state) {
        return Err(ApiError::bad("password sign-in is turned off; invite people to sign in through single sign-on"));
    }
    let hash = match &req.password {
        Some(p) if !p.is_empty() => {
            super::auth::validate_password(p)?;
            Some(hash_password(p.clone()).await?)
        }
        _ => None,
    };
    let id = new_id();
    sqlx::query("INSERT INTO users (id, workspace_id, email, name, role, password_hash, status) VALUES ($1, $2, $3, $4, $5, $6, $7)")
        .bind(id)
        .bind(user.workspace_id)
        .bind(&email)
        .bind(name)
        .bind(&role)
        .bind(&hash)
        .bind(if hash.is_some() { "active" } else { "invited" })
        .execute(&state.db)
        .await
        .map_err(|e| match ApiError::from(e) {
            ApiError::Conflict(_) => ApiError::Conflict("someone with this e-mail address already has an account".into()),
            other => other,
        })?;
    let (invite, emailed) = if hash.is_none() {
        let url = super::auth::user_link(&state, id, "invite").await?;
        let emailed = super::email::send_invitation(&state, &user, &email, name, &url).await;
        (Some(url), emailed)
    } else {
        (None, false)
    };
    audit(&state, user.workspace_id, Some(user.id), "user.created", Some(email), json!({"role": role})).await;
    Ok((StatusCode::CREATED, Json(json!({"id": id, "inviteUrl": invite, "emailed": emailed}))))
}

#[derive(Deserialize)]
pub struct UserUpdate {
    name: Option<String>,
    role: Option<String>,
    /// active | deactivated
    status: Option<String>,
}

/// `GET /users/{id}/export` — everything about one person (workspace admins), as a download.
pub async fn export(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(id): Path<Uuid>) -> ApiResult<axum::response::Response> {
    crate::auth::require_admin(&user)?;
    let ws: Option<Uuid> = sqlx::query_scalar("SELECT workspace_id FROM users WHERE id = $1").bind(id).fetch_optional(&state.db).await?;
    if ws != Some(user.workspace_id) {
        return Err(ApiError::NotFound("person"));
    }
    crate::app::audit(&state, user.workspace_id, Some(user.id), "user.exported", Some(id.to_string()), serde_json::json!({})).await;
    download(crate::privacy::export(&state, id).await?)
}

/// `GET /me/export` — everything TaskCraft holds about the caller, as a download.
pub async fn export_me(State(state): State<AppState>, CurrentUser(user): CurrentUser) -> ApiResult<axum::response::Response> {
    crate::app::audit(&state, user.workspace_id, Some(user.id), "user.exported", Some(user.id.to_string()), serde_json::json!({"self": true})).await;
    download(crate::privacy::export(&state, user.id).await?)
}

fn download(v: serde_json::Value) -> ApiResult<axum::response::Response> {
    use axum::response::IntoResponse;
    let body = serde_json::to_vec_pretty(&v)?;
    Ok((
        [
            (axum::http::header::CONTENT_TYPE, "application/json".to_string()),
            (axum::http::header::CONTENT_DISPOSITION, "attachment; filename=\"taskcraft-personal-data.json\"".to_string()),
        ],
        body,
    )
        .into_response())
}

pub async fn update(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(id): Path<Uuid>, Json(req): Json<UserUpdate>) -> ApiResult<StatusCode> {
    require_admin(&user)?;
    let (target_role, removed): (String, bool) = sqlx::query_as("SELECT role, preferences ? 'removed' FROM users WHERE id = $1 AND workspace_id = $2")
        .bind(id)
        .bind(user.workspace_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(ApiError::NotFound("user"))?;
    if removed {
        return Err(ApiError::bad("this person was removed"));
    }
    if target_role == "owner" && (req.role.is_some() || req.status.as_deref() == Some("deactivated")) {
        return Err(ApiError::bad("the owner's role can't be changed"));
    }
    if let Some(r) = &req.role {
        valid_role(r)?;
        sqlx::query("UPDATE users SET role = $2 WHERE id = $1").bind(id).bind(r).execute(&state.db).await?;
    }
    if let Some(s) = &req.status {
        if !matches!(s.as_str(), "active" | "deactivated") {
            return Err(ApiError::bad("status must be active or deactivated"));
        }
        if id == user.id {
            return Err(ApiError::bad("you can't deactivate yourself"));
        }
        sqlx::query("UPDATE users SET status = $2 WHERE id = $1 AND (status <> 'invited' OR $2 = 'deactivated')").bind(id).bind(s).execute(&state.db).await?;
        if s == "deactivated" {
            sqlx::query("DELETE FROM sessions WHERE user_id = $1").bind(id).execute(&state.db).await?;
            sqlx::query("DELETE FROM api_tokens WHERE user_id = $1").bind(id).execute(&state.db).await?;
        }
    }
    if let Some(n) = req.name.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
        sqlx::query("UPDATE users SET name = $2 WHERE id = $1").bind(id).bind(n).execute(&state.db).await?;
    }
    audit(&state, user.workspace_id, Some(user.id), "user.updated", Some(id.to_string()), json!({"role": req.role, "status": req.status})).await;
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /users/{id}`: removes a person for good. Their comments, time logs and history stay —
/// attributed to "Former user …" — while everything personal goes: name, e-mail address, password,
/// second factors, sign-in links, sessions and tokens, notifications, private filters and
/// dashboards; their open issues become unassigned.
pub async fn remove(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    require_admin(&user)?;
    let (role, email, removed): (String, Option<String>, bool) =
        sqlx::query_as("SELECT role, email, preferences ? 'removed' FROM users WHERE id = $1 AND workspace_id = $2")
            .bind(id)
            .bind(user.workspace_id)
            .fetch_optional(&state.db)
            .await?
            .ok_or(ApiError::NotFound("user"))?;
    if id == user.id {
        return Err(ApiError::bad("you can't remove yourself"));
    }
    if removed {
        return Ok(StatusCode::NO_CONTENT);
    }
    if role == "owner" {
        return Err(ApiError::bad("the workspace owner can't be removed"));
    }
    if role == "admin" && user.role != "owner" {
        return Err(ApiError::Denied("only the owner can remove administrators".into()));
    }
    let name = format!("Former user {}", &id.simple().to_string()[24..]);
    let mut tx = state.db.begin().await?;
    sqlx::query(
        "UPDATE users SET name = $2, email = NULL, password_hash = NULL, title = NULL, locale = NULL, timezone = NULL, preferences = jsonb_build_object('removed', true),
                role = 'member', status = 'deactivated', totp_secret = NULL, totp_pending = NULL, totp_enabled_at = NULL, totp_last_step = NULL,
                recovery_codes = '{}'
          WHERE id = $1",
    )
    .bind(id)
    .bind(&name)
    .execute(&mut *tx)
    .await?;
    for sql in [
        "DELETE FROM sessions WHERE user_id = $1",
        "DELETE FROM api_tokens WHERE user_id = $1",
        "DELETE FROM user_links WHERE user_id = $1",
        "DELETE FROM identities WHERE user_id = $1",
        "DELETE FROM passkeys WHERE user_id = $1",
        "DELETE FROM auth_challenges WHERE user_id = $1",
        "DELETE FROM notifications WHERE user_id = $1",
        "DELETE FROM recent_issues WHERE user_id = $1",
        "DELETE FROM project_stars WHERE user_id = $1",
        "DELETE FROM view_stars WHERE user_id = $1",
        "DELETE FROM issue_watchers WHERE user_id = $1",
        "DELETE FROM team_members WHERE user_id = $1",
        "DELETE FROM project_members WHERE subject_type = 'user' AND subject_id = $1",
        "DELETE FROM views WHERE owner_id = $1 AND NOT shared",
        "DELETE FROM dashboards WHERE owner_id = $1 AND NOT shared",
        "UPDATE projects SET lead_id = NULL WHERE lead_id = $1",
        "UPDATE components SET lead_id = NULL WHERE lead_id = $1",
        "UPDATE issues i SET assignee_id = NULL FROM statuses s WHERE s.id = i.status_id AND s.category <> 'done' AND i.assignee_id = $1",
    ] {
        sqlx::query(sql).bind(id).execute(&mut *tx).await?;
    }
    if let Some(email) = email {
        sqlx::query("DELETE FROM mail_outbox WHERE sent_at IS NULL AND lower(to_address) = lower($1)").bind(email).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    audit(&state, user.workspace_id, Some(user.id), "user.removed", Some(id.to_string()), json!({"name": name})).await;
    Ok(StatusCode::NO_CONTENT)
}

/// A new invitation (invited people) or password-reset link (everyone else), for the admin to
/// pass on; also e-mailed to the person when mail is configured.
pub async fn reset_link(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(id): Path<Uuid>) -> ApiResult<Json<Value>> {
    require_admin(&user)?;
    let (status, name, email): (String, String, Option<String>) = sqlx::query_as("SELECT status, name, email FROM users WHERE id = $1 AND workspace_id = $2")
        .bind(id)
        .bind(user.workspace_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(ApiError::NotFound("user"))?;
    let invite = status == "invited";
    if !invite && !crate::sso::passwords_enabled(&state) {
        return Err(ApiError::bad("password sign-in is turned off; people sign in through single sign-on"));
    }
    let url = super::auth::user_link(&state, id, if invite { "invite" } else { "reset" }).await?;
    let emailed = match email.as_deref().filter(|_| status != "deactivated") {
        Some(to) if invite => super::email::send_invitation(&state, &user, to, &name, &url).await,
        Some(to) => super::email::send_reset_link(&state, &user, to, &name, &url).await,
        None => false,
    };
    Ok(Json(json!({"url": url, "emailed": emailed})))
}

pub async fn teams(State(state): State<AppState>, CurrentUser(user): CurrentUser) -> ApiResult<Json<Value>> {
    let rows: Vec<(Uuid, String, Option<String>, Option<String>, Vec<Uuid>)> = sqlx::query_as(
        "SELECT t.id, t.name, t.description, t.color, COALESCE(array_agg(m.user_id) FILTER (WHERE m.user_id IS NOT NULL), '{}')
           FROM teams t LEFT JOIN team_members m ON m.team_id = t.id WHERE t.workspace_id = $1 GROUP BY t.id ORDER BY lower(t.name)",
    )
    .bind(user.workspace_id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(json!(
        rows.into_iter()
            .map(|(id, name, d, c, members)| json!({"id": id, "name": name, "description": d, "color": c, "memberIds": members}))
            .collect::<Vec<_>>()
    )))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamInput {
    name: Option<String>,
    description: Option<String>,
    color: Option<String>,
    member_ids: Option<Vec<Uuid>>,
}

pub async fn create_team(State(state): State<AppState>, CurrentUser(user): CurrentUser, Json(req): Json<TeamInput>) -> ApiResult<(StatusCode, Json<Value>)> {
    if user.is_guest() {
        return Err(ApiError::Forbidden);
    }
    let name = req.name.as_deref().map(str::trim).filter(|n| !n.is_empty()).ok_or_else(|| ApiError::bad("a team needs a name"))?;
    let id = new_id();
    sqlx::query("INSERT INTO teams (id, workspace_id, name, description, color) VALUES ($1, $2, $3, $4, $5)")
        .bind(id)
        .bind(user.workspace_id)
        .bind(name)
        .bind(&req.description)
        .bind(&req.color)
        .execute(&state.db)
        .await?;
    set_members(&state, user.workspace_id, id, req.member_ids.unwrap_or_else(|| vec![user.id])).await?;
    Ok((StatusCode::CREATED, Json(json!({"id": id}))))
}

async fn set_members(state: &AppState, ws: Uuid, team: Uuid, members: Vec<Uuid>) -> ApiResult<()> {
    let mut tx = state.db.begin().await?;
    sqlx::query("DELETE FROM team_members WHERE team_id = $1").bind(team).execute(&mut *tx).await?;
    sqlx::query("INSERT INTO team_members (team_id, user_id) SELECT $1, id FROM users WHERE id = ANY($2) AND workspace_id = $3")
        .bind(team)
        .bind(&members)
        .bind(ws)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

pub async fn update_team(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<Uuid>,
    Json(req): Json<TeamInput>,
) -> ApiResult<StatusCode> {
    if user.is_guest() {
        return Err(ApiError::Forbidden);
    }
    let exists: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM teams WHERE id = $1 AND workspace_id = $2)")
        .bind(id)
        .bind(user.workspace_id)
        .fetch_one(&state.db)
        .await?;
    if !exists {
        return Err(ApiError::NotFound("team"));
    }
    if let Some(n) = req.name.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
        sqlx::query("UPDATE teams SET name = $2 WHERE id = $1").bind(id).bind(n).execute(&state.db).await?;
    }
    if req.description.is_some() {
        sqlx::query("UPDATE teams SET description = $2 WHERE id = $1").bind(id).bind(&req.description).execute(&state.db).await?;
    }
    if req.color.is_some() {
        sqlx::query("UPDATE teams SET color = $2 WHERE id = $1").bind(id).bind(&req.color).execute(&state.db).await?;
    }
    if let Some(m) = req.member_ids {
        set_members(&state, user.workspace_id, id, m).await?;
    }
    Ok(StatusCode::NO_CONTENT)
}

pub async fn delete_team(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    require_admin(&user)?;
    sqlx::query("DELETE FROM teams WHERE id = $1 AND workspace_id = $2").bind(id).bind(user.workspace_id).execute(&state.db).await?;
    sqlx::query("DELETE FROM project_members WHERE subject_type = 'team' AND subject_id = $1").bind(id).execute(&state.db).await?;
    Ok(StatusCode::NO_CONTENT)
}
