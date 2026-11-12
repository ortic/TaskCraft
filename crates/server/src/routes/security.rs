//! Account security (`crate::mfa`): where you're signed in (list, sign out), two-factor
//! authentication with an authenticator app and recovery codes, the second step of a sign-in,
//! and for admins the workspace's "require two-factor authentication" and resetting someone's
//! second factor. Passkeys: `routes::passkeys`.
//!
//! Changes to sign-in methods take a browser session ([`SessionUser`]), never an API token.

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, patch, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::app::{AppState, audit};
use crate::auth::{CurrentUser, SessionUser, create_session, require_admin, session_token, token_hash};
use crate::error::{ApiError, ApiResult};
use crate::mfa;

use super::passkeys;

pub fn routes() -> Router<AppState> {
    Router::new()
        // the second step of a sign-in; passkeys without a password
        .route("/auth/mfa", post(login))
        .route("/auth/mfa/passkey", post(passkey_challenge))
        .route("/auth/passkey/options", post(passkeys::login_options))
        .route("/auth/passkey", post(passkeys::login))
        // the signed-in person
        .route("/me/sessions", get(sessions))
        .route("/me/sessions/revoke-others", post(revoke_others))
        .route("/me/sessions/{id}", delete(revoke_session))
        .route("/me/mfa", get(status))
        .route("/me/mfa/setup", post(setup))
        .route("/me/mfa/enable", post(enable))
        .route("/me/mfa/disable", post(disable))
        .route("/me/mfa/recovery-codes", post(new_recovery_codes))
        .route("/me/passkeys", get(passkeys::list).post(passkeys::add))
        .route("/me/passkeys/options", post(passkeys::add_options))
        .route("/me/passkeys/{id}", patch(passkeys::rename).delete(passkeys::delete))
        // admins
        .route("/admin/security", get(policy).put(put_policy))
        .route("/users/{id}/reset-mfa", post(admin_reset))
}

// ---- sessions --------------------------------------------------------------------------------

#[derive(Serialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct SessionRow {
    id: Uuid,
    created_at: chrono::DateTime<chrono::Utc>,
    last_seen_at: chrono::DateTime<chrono::Utc>,
    expires_at: chrono::DateTime<chrono::Utc>,
    user_agent: Option<String>,
    ip: Option<String>,
    current: bool,
}

/// `GET /me/sessions` — where you're signed in, this browser first.
pub async fn sessions(State(state): State<AppState>, CurrentUser(user): CurrentUser, headers: HeaderMap) -> ApiResult<Json<Vec<SessionRow>>> {
    let current = session_token(&headers).map(token_hash).unwrap_or_default();
    Ok(Json(
        sqlx::query_as::<_, SessionRow>(
            "SELECT id, created_at, last_seen_at, expires_at, user_agent, ip, token_hash = $2 AS current FROM sessions
             WHERE user_id = $1 AND expires_at > now() ORDER BY (token_hash = $2) DESC, last_seen_at DESC",
        )
        .bind(user.id)
        .bind(current)
        .fetch_all(&state.db)
        .await?,
    ))
}

/// `DELETE /me/sessions/{id}` — signs one session out.
pub async fn revoke_session(State(state): State<AppState>, SessionUser(user): SessionUser, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    let n = sqlx::query("DELETE FROM sessions WHERE id = $1 AND user_id = $2").bind(id).bind(user.id).execute(&state.db).await?.rows_affected();
    if n == 0 {
        return Err(ApiError::NotFound("session"));
    }
    audit(&state, user.workspace_id, Some(user.id), "auth.session_revoked", None, json!({})).await;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /me/sessions/revoke-others` — signs out everywhere else.
pub async fn revoke_others(State(state): State<AppState>, SessionUser(user): SessionUser, headers: HeaderMap) -> ApiResult<Json<Value>> {
    let current = session_token(&headers).map(token_hash).unwrap_or_default();
    let n = sqlx::query("DELETE FROM sessions WHERE user_id = $1 AND token_hash <> $2").bind(user.id).bind(current).execute(&state.db).await?.rows_affected();
    audit(&state, user.workspace_id, Some(user.id), "auth.sessions_revoked", None, json!({"count": n})).await;
    Ok(Json(json!({ "revoked": n })))
}

// ---- two-factor authentication ---------------------------------------------------------------

/// `GET /me/mfa`
pub async fn status(State(state): State<AppState>, CurrentUser(user): CurrentUser) -> ApiResult<Json<Value>> {
    let (enabled, codes, has_password): (Option<chrono::DateTime<chrono::Utc>>, i32, bool) =
        sqlx::query_as("SELECT totp_enabled_at, cardinality(recovery_codes), password_hash IS NOT NULL FROM users WHERE id = $1")
            .bind(user.id)
            .fetch_one(&state.db)
            .await?;
    let passkeys: i64 = sqlx::query_scalar("SELECT count(*) FROM passkeys WHERE user_id = $1").bind(user.id).fetch_one(&state.db).await?;
    Ok(Json(json!({
        "enabled": enabled.is_some(),
        "enabledAt": enabled,
        "recoveryCodesLeft": codes,
        "hasPassword": has_password,
        "passkeys": passkeys,
        // the workspace requires a second factor of this person: the last one can't be removed
        "required": mfa::policy_applies(&state, user.id).await?,
    })))
}

/// An optional password confirming a change to sign-in methods.
#[derive(Deserialize, Default)]
pub struct Confirm {
    #[serde(default)]
    pub password: Option<String>,
}

/// `POST /me/mfa/setup {password?}` — a new pending secret (active once confirmed with a code).
/// Needs a recent sign-in or the password.
pub async fn setup(State(state): State<AppState>, SessionUser(user): SessionUser, headers: HeaderMap, body: Option<Json<Confirm>>) -> ApiResult<Json<Value>> {
    // replacing an active app needs turning it off (with the password) first
    let on: bool = sqlx::query_scalar("SELECT totp_enabled_at IS NOT NULL FROM users WHERE id = $1").bind(user.id).fetch_one(&state.db).await?;
    if on {
        return Err(ApiError::bad("two-factor authentication is already on; turn it off first"));
    }
    let password = body.and_then(|Json(c)| c.password);
    mfa::confirm_recent(&state, &user, &headers, password.as_deref()).await?;
    let secret = mfa::new_secret();
    sqlx::query("UPDATE users SET totp_pending = $2 WHERE id = $1").bind(user.id).bind(&secret).execute(&state.db).await?;
    let account = user.email.clone().unwrap_or_else(|| user.name.clone());
    let uri = mfa::otpauth_uri(&account, &secret);
    let svg = mfa::qr_svg(&uri).map_err(ApiError::Internal)?;
    Ok(Json(json!({ "secret": secret, "uri": uri, "qrSvg": svg })))
}

#[derive(Deserialize)]
pub struct CodeRequest {
    code: String,
}

/// `POST /me/mfa/enable {code}` — turns the pending secret on; returns the recovery codes
/// (shown once).
pub async fn enable(State(state): State<AppState>, SessionUser(user): SessionUser, Json(req): Json<CodeRequest>) -> ApiResult<Json<Value>> {
    let pending: Option<String> = sqlx::query_scalar("SELECT totp_pending FROM users WHERE id = $1").bind(user.id).fetch_one(&state.db).await?;
    let pending = pending.ok_or_else(|| ApiError::bad("start the setup first"))?;
    let step =
        mfa::matching_step(&pending, &req.code).ok_or_else(|| ApiError::bad("that code doesn't match — check the time on your phone and try the next one"))?;
    let (codes, hashes) = mfa::recovery_codes();
    sqlx::query(
        "UPDATE users SET totp_secret = totp_pending, totp_pending = NULL, totp_enabled_at = now(), totp_last_step = $2, recovery_codes = $3 WHERE id = $1",
    )
    .bind(user.id)
    .bind(step as i64)
    .bind(&hashes)
    .execute(&state.db)
    .await?;
    audit(&state, user.workspace_id, Some(user.id), "auth.mfa_enabled", None, json!({})).await;
    Ok(Json(json!({ "recoveryCodes": codes })))
}

#[derive(Deserialize)]
pub struct DisableRequest {
    password: Option<String>,
    code: Option<String>,
}

/// `POST /me/mfa/disable {password}` — needs the password (accounts without one: a code).
pub async fn disable(State(state): State<AppState>, SessionUser(user): SessionUser, Json(req): Json<DisableRequest>) -> ApiResult<StatusCode> {
    let has_password: bool = sqlx::query_scalar("SELECT password_hash IS NOT NULL FROM users WHERE id = $1").bind(user.id).fetch_one(&state.db).await?;
    let ok = match (has_password, req.password.as_deref().filter(|p| !p.is_empty()), req.code.as_deref()) {
        (true, Some(pw), _) => mfa::check_password(&state, user.id, pw).await?,
        (false, _, Some(code)) => mfa::verify(&state, user.id, code).await?,
        _ => false,
    };
    if !ok {
        return Err(ApiError::bad(if has_password { "that password isn't right" } else { "that code doesn't match" }));
    }
    if mfa::policy_applies(&state, user.id).await? && !passkeys::has_passkey(&state, user.id).await? {
        return Err(ApiError::bad("two-factor authentication is required in this workspace: add a passkey before turning off the authenticator app"));
    }
    clear_totp(&state, user.id).await?;
    audit(&state, user.workspace_id, Some(user.id), "auth.mfa_disabled", None, json!({})).await;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /me/mfa/recovery-codes {code}` — replaces the recovery codes (a current code confirms).
pub async fn new_recovery_codes(State(state): State<AppState>, SessionUser(user): SessionUser, Json(req): Json<CodeRequest>) -> ApiResult<Json<Value>> {
    if !mfa::verify(&state, user.id, &req.code).await? {
        return Err(ApiError::bad("that code doesn't match"));
    }
    let (codes, hashes) = mfa::recovery_codes();
    sqlx::query("UPDATE users SET recovery_codes = $2 WHERE id = $1").bind(user.id).bind(&hashes).execute(&state.db).await?;
    audit(&state, user.workspace_id, Some(user.id), "auth.recovery_codes_renewed", None, json!({})).await;
    Ok(Json(json!({ "recoveryCodes": codes })))
}

async fn clear_totp(state: &AppState, user: Uuid) -> ApiResult<()> {
    sqlx::query("UPDATE users SET totp_secret = NULL, totp_pending = NULL, totp_enabled_at = NULL, totp_last_step = NULL, recovery_codes = '{}' WHERE id = $1")
        .bind(user)
        .execute(&state.db)
        .await?;
    Ok(())
}

// ---- the second step of a sign-in ------------------------------------------------------------

fn step_token(token: Option<&str>, headers: &HeaderMap) -> ApiResult<String> {
    token
        .filter(|t| !t.is_empty())
        .map(String::from)
        .or_else(|| crate::auth::cookie_value(headers, mfa::MFA_COOKIE).filter(|t| !t.is_empty()).map(String::from))
        .ok_or_else(|| ApiError::bad("the sign-in expired, please log in again"))
}

const EXPIRED: &str = "the sign-in expired, please log in again";

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PasskeyChallenge {
    /// from the login response; sign-ins that arrive by redirect use the `tc_mfa` cookie
    mfa_token: Option<String>,
}

/// `POST /auth/mfa/passkey {mfaToken?}` — starts the second step with a passkey: options for
/// `navigator.credentials.get()` (null when the person has none), and the methods they have.
pub async fn passkey_challenge(State(state): State<AppState>, headers: HeaderMap, Json(req): Json<PasskeyChallenge>) -> ApiResult<Json<Value>> {
    let token = step_token(req.mfa_token.as_deref(), &headers)?;
    let challenge = crate::webauthn::challenge();
    let user: Option<Option<Uuid>> =
        sqlx::query_scalar("UPDATE auth_challenges SET challenge = $3 WHERE token_hash = $1 AND kind = $2 AND expires_at > now() RETURNING user_id")
            .bind(token_hash(&token))
            .bind(mfa::SECOND_STEP)
            .bind(&challenge)
            .fetch_optional(&state.db)
            .await?;
    let user = user.flatten().ok_or_else(|| ApiError::bad(EXPIRED))?;
    let methods = mfa::methods(&state, user).await?;
    let options = if methods.contains(&"passkey") { Some(passkeys::request_options(&state, Some(user), &challenge).await?) } else { None };
    Ok(Json(json!({ "methods": methods, "publicKey": options })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SecondStep {
    /// from the login response; sign-ins that arrive by redirect use the `tc_mfa` cookie
    mfa_token: Option<String>,
    /// a code from the authenticator app or a recovery code …
    #[serde(default)]
    code: String,
    /// … or a passkey's answer (after `POST /auth/mfa/passkey`)
    #[serde(default)]
    passkey: Option<passkeys::Assertion>,
}

/// `POST /auth/mfa {mfaToken?, code | passkey}` — completes a sign-in: five tries per sign-in
/// (counted atomically), plus the per-person limit of `mfa::verify`.
pub async fn login(State(state): State<AppState>, headers: HeaderMap, Json(req): Json<SecondStep>) -> ApiResult<Response> {
    let token = step_token(req.mfa_token.as_deref(), &headers)?;
    let key = token_hash(&token);
    let row: Option<(Option<Uuid>, i32, Option<Vec<u8>>, Option<String>)> = sqlx::query_as(
        "UPDATE auth_challenges SET attempts = attempts + 1 WHERE token_hash = $1 AND kind = $2 AND expires_at > now()
         RETURNING user_id, attempts, challenge, next",
    )
    .bind(&key)
    .bind(mfa::SECOND_STEP)
    .fetch_optional(&state.db)
    .await?;
    let Some((Some(user), attempts, challenge, next)) = row else { return Err(ApiError::bad(EXPIRED)) };
    let forget = || sqlx::query("DELETE FROM auth_challenges WHERE token_hash = $1").bind(&key).execute(&state.db);
    if attempts > 5 {
        forget().await?;
        return Err(ApiError::bad("too many wrong codes, please log in again"));
    }
    let ok = match &req.passkey {
        Some(answer) => {
            let challenge = challenge.ok_or_else(|| ApiError::bad("start the passkey step first"))?;
            match passkeys::verify(&state, &challenge, answer, Some(user)).await {
                Ok(_) => true,
                Err(ApiError::BadRequest(m)) => {
                    tracing::info!(%user, "passkey second step refused: {m}");
                    false
                }
                Err(e) => return Err(e),
            }
        }
        None => mfa::verify(&state, user, &req.code).await?,
    };
    if !ok {
        if attempts >= 5 {
            forget().await?;
            return Err(ApiError::bad("too many wrong codes, please log in again"));
        }
        return Err(ApiError::bad(if req.passkey.is_some() { "that passkey didn't work" } else { "that code doesn't match" }));
    }
    forget().await?;
    let ws: Uuid = sqlx::query_scalar("SELECT workspace_id FROM users WHERE id = $1 AND status = 'active'")
        .bind(user)
        .fetch_optional(&state.db)
        .await?
        .ok_or(ApiError::Forbidden)?;
    let cookie = create_session(&state, user, &headers).await?;
    audit(&state, ws, Some(user), "auth.login", None, json!({"secondFactor": if req.passkey.is_some() { "passkey" } else { "code" }})).await;
    let mut resp = ([(header::SET_COOKIE, cookie)], Json(json!({ "ok": true, "next": next }))).into_response();
    if crate::auth::cookie_value(&headers, mfa::MFA_COOKIE).is_some() {
        resp.headers_mut().append(header::SET_COOKIE, mfa::mfa_cookie(&state, "", 0).parse().map_err(|e| ApiError::Internal(anyhow::anyhow!("{e}")))?);
    }
    Ok(resp)
}

// ---- administration --------------------------------------------------------------------------

async fn policy_json(state: &AppState, workspace: Uuid) -> ApiResult<Value> {
    let policy: Option<String> =
        sqlx::query_scalar("SELECT settings->>'requireTwoFactor' FROM workspaces WHERE id = $1").bind(workspace).fetch_one(&state.db).await?;
    let rows: Vec<(Uuid, bool, bool, bool)> = sqlx::query_as(
        "SELECT u.id, u.status = 'active', u.role IN ('owner', 'admin'),
                u.totp_enabled_at IS NOT NULL OR EXISTS (SELECT 1 FROM passkeys p WHERE p.user_id = u.id)
           FROM users u WHERE u.workspace_id = $1",
    )
    .bind(workspace)
    .fetch_all(&state.db)
    .await?;
    let count = |admins: bool, enrolled: bool| rows.iter().filter(|r| r.1 && (!admins || r.2) && (!enrolled || r.3)).count();
    Ok(json!({
        "requireTwoFactor": policy.filter(|p| mfa::POLICIES.contains(&p.as_str())).unwrap_or_else(|| "off".into()),
        // everyone with a second factor (the Users list marks them)
        "enrolled": rows.iter().filter(|r| r.3).map(|r| r.0).collect::<Vec<_>>(),
        // active people: all and admins, and how many of them have a second factor
        "people": {"total": count(false, false), "enrolled": count(false, true)},
        "admins": {"total": count(true, false), "enrolled": count(true, true)},
    }))
}

/// `GET /admin/security` — the workspace's two-factor policy and who has a second factor.
pub async fn policy(State(state): State<AppState>, CurrentUser(user): CurrentUser) -> ApiResult<Json<Value>> {
    require_admin(&user)?;
    Ok(Json(policy_json(&state, user.workspace_id).await?))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PolicyUpdate {
    require_two_factor: String,
}

/// `PUT /admin/security {requireTwoFactor: off | admins | everyone}` — people it applies to who
/// have no second factor yet see only the setup until they add one. Browser sessions only.
pub async fn put_policy(State(state): State<AppState>, SessionUser(user): SessionUser, Json(req): Json<PolicyUpdate>) -> ApiResult<Json<Value>> {
    require_admin(&user)?;
    let v = req.require_two_factor.as_str();
    if !mfa::POLICIES.contains(&v) {
        return Err(ApiError::bad("requireTwoFactor must be off, admins or everyone"));
    }
    sqlx::query("UPDATE workspaces SET settings = settings || jsonb_build_object('requireTwoFactor', $2::text) WHERE id = $1")
        .bind(user.workspace_id)
        .bind(v)
        .execute(&state.db)
        .await?;
    audit(&state, user.workspace_id, Some(user.id), "workspace.two_factor_policy", None, json!({"requireTwoFactor": v})).await;
    Ok(Json(policy_json(&state, user.workspace_id).await?))
}

/// `POST /users/{id}/reset-mfa` (admins) — for people who lost their phone and recovery codes:
/// removes the authenticator app and passkeys, signs them out everywhere and deletes their API
/// tokens. Only the owner resets admins; your own is managed under Profile & settings.
pub async fn admin_reset(State(state): State<AppState>, SessionUser(admin): SessionUser, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    require_admin(&admin)?;
    let role: String = sqlx::query_scalar("SELECT role FROM users WHERE id = $1 AND workspace_id = $2")
        .bind(id)
        .bind(admin.workspace_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(ApiError::NotFound("user"))?;
    if id == admin.id {
        return Err(ApiError::bad("manage your own sign-in methods under Profile & settings → Security"));
    }
    if matches!(role.as_str(), "owner" | "admin") && admin.role != "owner" {
        return Err(ApiError::Denied("only the workspace owner can reset an admin's second factor".into()));
    }
    let mut tx = state.db.begin().await?;
    sqlx::query("UPDATE users SET totp_secret = NULL, totp_pending = NULL, totp_enabled_at = NULL, totp_last_step = NULL, recovery_codes = '{}' WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM passkeys WHERE user_id = $1").bind(id).execute(&mut *tx).await?;
    sqlx::query("DELETE FROM auth_challenges WHERE user_id = $1").bind(id).execute(&mut *tx).await?;
    sqlx::query("DELETE FROM sessions WHERE user_id = $1").bind(id).execute(&mut *tx).await?;
    sqlx::query("DELETE FROM api_tokens WHERE user_id = $1").bind(id).execute(&mut *tx).await?;
    tx.commit().await?;
    audit(&state, admin.workspace_id, Some(admin.id), "auth.mfa_reset", Some(id.to_string()), json!({})).await;
    Ok(StatusCode::NO_CONTENT)
}
