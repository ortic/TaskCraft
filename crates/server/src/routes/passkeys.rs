//! Passkeys (`crate::webauthn`): people add them under Profile & settings → Security and use them
//! * as the second step after the password (instead of a code), and
//! * to sign in without a password, when the authenticator verified the person (PIN, fingerprint,
//!   face): that is two factors in one, so no second step follows.
//!
//! Adding or removing a passkey needs a recent sign-in (15 minutes) or the password, so a stolen
//! session can't quietly add the thief's own passkey. A passkey counts for the workspace's
//! "require two-factor authentication". Routes: `routes::security::routes`.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::app::{AppState, audit, new_id};
use crate::auth::{CurrentUser, SessionUser, create_session};
use crate::error::{ApiError, ApiResult};
use crate::mfa;
use crate::webauthn::{self, ALGORITHMS, RelyingParty, b64, unb64};

use super::security::Confirm;

const TIMEOUT_MS: u64 = 300_000;
/// passkeys per person at most
const MAX_PASSKEYS: i64 = 20;

fn rp(state: &AppState) -> ApiResult<RelyingParty> {
    RelyingParty::from_base_url(&state.config.base_url).map_err(ApiError::Internal)
}

#[derive(Serialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct PasskeyRow {
    id: Uuid,
    name: String,
    backed_up: bool,
    created_at: chrono::DateTime<chrono::Utc>,
    last_used_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// `GET /me/passkeys`
pub async fn list(State(state): State<AppState>, CurrentUser(user): CurrentUser) -> ApiResult<Json<Vec<PasskeyRow>>> {
    Ok(Json(
        sqlx::query_as("SELECT id, name, backed_up, created_at, last_used_at FROM passkeys WHERE user_id = $1 ORDER BY created_at")
            .bind(user.id)
            .fetch_all(&state.db)
            .await?,
    ))
}

/// `POST /me/passkeys/options {password?}` — what `navigator.credentials.create()` needs, and a
/// token for `POST /me/passkeys`.
pub async fn add_options(
    State(state): State<AppState>,
    SessionUser(user): SessionUser,
    headers: HeaderMap,
    body: Option<Json<Confirm>>,
) -> ApiResult<Json<Value>> {
    let password = body.and_then(|Json(c)| c.password);
    mfa::confirm_recent(&state, &user, &headers, password.as_deref()).await?;
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM passkeys WHERE user_id = $1").bind(user.id).fetch_one(&state.db).await?;
    if count >= MAX_PASSKEYS {
        return Err(ApiError::bad(format!("at most {MAX_PASSKEYS} passkeys; remove one first")));
    }
    let rp = rp(&state)?;
    let challenge = webauthn::challenge();
    let token = mfa::put_step(&state, mfa::PASSKEY_ADD, Some(user.id), Some(&challenge), None).await?;
    let existing: Vec<(Vec<u8>, Vec<String>)> =
        sqlx::query_as("SELECT credential_id, transports FROM passkeys WHERE user_id = $1").bind(user.id).fetch_all(&state.db).await?;
    let workspace: String = sqlx::query_scalar("SELECT name FROM workspaces WHERE id = $1").bind(user.workspace_id).fetch_one(&state.db).await?;
    Ok(Json(json!({
        "token": token,
        "publicKey": {
            "rp": {"id": rp.id, "name": format!("{workspace} · TaskCraft")},
            "user": {"id": b64(user.id.as_bytes()), "name": user.email.clone().unwrap_or_else(|| user.name.clone()), "displayName": user.name},
            "challenge": b64(&challenge),
            "pubKeyCredParams": ALGORITHMS.iter().map(|a| json!({"type": "public-key", "alg": a})).collect::<Vec<_>>(),
            "timeout": TIMEOUT_MS,
            "excludeCredentials": existing.iter().map(|(id, t)| json!({"type": "public-key", "id": b64(id), "transports": t})).collect::<Vec<_>>(),
            "authenticatorSelection": {"residentKey": "preferred", "requireResidentKey": false, "userVerification": "preferred"},
            "attestation": "none",
        }
    })))
}

/// Field names as in `PublicKeyCredential.toJSON()`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Attestation {
    #[serde(rename = "clientDataJSON")]
    client_data_json: String,
    attestation_object: String,
    #[serde(default)]
    transports: Vec<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Registration {
    raw_id: String,
    response: Attestation,
}

#[derive(Deserialize)]
pub struct AddRequest {
    token: String,
    name: Option<String>,
    credential: Registration,
}

/// `POST /me/passkeys {token, name, credential}` — stores the passkey `navigator.credentials.create()` made.
pub async fn add(State(state): State<AppState>, SessionUser(user): SessionUser, Json(req): Json<AddRequest>) -> ApiResult<(StatusCode, Json<PasskeyRow>)> {
    let (owner, challenge) = mfa::take_step(&state, mfa::PASSKEY_ADD, &req.token).await?;
    if owner != Some(user.id) {
        return Err(ApiError::bad("this passkey setup belongs to another sign-in"));
    }
    let challenge = challenge.ok_or_else(|| ApiError::bad("invalid setup"))?;
    let bad = |e: anyhow::Error| ApiError::bad(format!("the passkey couldn't be checked: {e}"));
    let client = unb64(&req.credential.response.client_data_json).map_err(bad)?;
    let att = unb64(&req.credential.response.attestation_object).map_err(bad)?;
    let cred = webauthn::verify_registration(&rp(&state)?, &challenge, &client, &att).map_err(bad)?;
    if unb64(&req.credential.raw_id).map_err(bad)? != cred.id {
        return Err(ApiError::bad("the passkey's id doesn't match its data"));
    }
    let name: String = req.name.as_deref().map(str::trim).filter(|n| !n.is_empty()).unwrap_or("Passkey").chars().take(80).collect();
    let transports: Vec<String> = req.credential.response.transports.into_iter().filter(|t| t.len() <= 20).take(8).collect();
    let row = sqlx::query_as::<_, PasskeyRow>(
        "INSERT INTO passkeys (id, user_id, credential_id, public_key, algorithm, sign_count, name, transports, backed_up)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) ON CONFLICT (credential_id) DO NOTHING
         RETURNING id, name, backed_up, created_at, last_used_at",
    )
    .bind(new_id())
    .bind(user.id)
    .bind(&cred.id)
    .bind(&cred.public_key)
    .bind(cred.algorithm as i32)
    .bind(i64::from(cred.sign_count))
    .bind(&name)
    .bind(&transports)
    .bind(cred.backed_up)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| ApiError::Conflict("this passkey is already registered".into()))?;
    audit(&state, user.workspace_id, Some(user.id), "auth.passkey_added", Some(name.clone()), json!({"passkey": row.id})).await;
    Ok((StatusCode::CREATED, Json(row)))
}

#[derive(Deserialize)]
pub struct Rename {
    name: String,
}

/// `PATCH /me/passkeys/{id} {name}`
pub async fn rename(
    State(state): State<AppState>,
    SessionUser(user): SessionUser,
    Path(id): Path<Uuid>,
    Json(req): Json<Rename>,
) -> ApiResult<Json<PasskeyRow>> {
    let name: String = req.name.trim().chars().take(80).collect();
    if name.is_empty() {
        return Err(ApiError::bad("a passkey needs a name"));
    }
    Ok(Json(
        sqlx::query_as("UPDATE passkeys SET name = $3 WHERE id = $1 AND user_id = $2 RETURNING id, name, backed_up, created_at, last_used_at")
            .bind(id)
            .bind(user.id)
            .bind(&name)
            .fetch_optional(&state.db)
            .await?
            .ok_or(ApiError::NotFound("passkey"))?,
    ))
}

/// `DELETE /me/passkeys/{id} {password?}` — needs a recent sign-in or the password (a stolen
/// session must not take a factor away).
pub async fn delete(
    State(state): State<AppState>,
    SessionUser(user): SessionUser,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    body: Option<Json<Confirm>>,
) -> ApiResult<StatusCode> {
    let password = body.and_then(|Json(c)| c.password);
    mfa::confirm_recent(&state, &user, &headers, password.as_deref()).await?;
    let (exists, others, totp): (bool, i64, bool) = sqlx::query_as(
        "SELECT EXISTS (SELECT 1 FROM passkeys WHERE id = $1 AND user_id = $2), (SELECT count(*) FROM passkeys WHERE user_id = $2 AND id <> $1),
                (SELECT totp_enabled_at IS NOT NULL FROM users WHERE id = $2)",
    )
    .bind(id)
    .bind(user.id)
    .fetch_one(&state.db)
    .await?;
    if !exists {
        return Err(ApiError::NotFound("passkey"));
    }
    if others == 0 && !totp && mfa::policy_applies(&state, user.id).await? {
        return Err(ApiError::bad("two-factor authentication is required in this workspace: set up another passkey or an authenticator app first"));
    }
    sqlx::query("DELETE FROM passkeys WHERE id = $1 AND user_id = $2").bind(id).bind(user.id).execute(&state.db).await?;
    audit(&state, user.workspace_id, Some(user.id), "auth.passkey_removed", None, json!({"passkey": id})).await;
    Ok(StatusCode::NO_CONTENT)
}

/// Whether the person has a passkey.
pub async fn has_passkey(state: &AppState, user: Uuid) -> ApiResult<bool> {
    Ok(sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM passkeys WHERE user_id = $1)").bind(user).fetch_one(&state.db).await?)
}

/// Options for `navigator.credentials.get()`: for one person's passkeys (second step), or any
/// passkey of this TaskCraft with the person verified (sign-in without a password).
pub async fn request_options(state: &AppState, user: Option<Uuid>, challenge: &[u8]) -> ApiResult<Value> {
    let allow: Vec<(Vec<u8>, Vec<String>)> = match user {
        Some(u) => sqlx::query_as("SELECT credential_id, transports FROM passkeys WHERE user_id = $1").bind(u).fetch_all(&state.db).await?,
        None => vec![],
    };
    Ok(json!({
        "challenge": b64(challenge),
        "rpId": rp(state)?.id,
        "timeout": TIMEOUT_MS,
        "userVerification": if user.is_some() { "preferred" } else { "required" },
        "allowCredentials": allow.iter().map(|(id, t)| json!({"type": "public-key", "id": b64(id), "transports": t})).collect::<Vec<_>>(),
    }))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssertionResponse {
    #[serde(rename = "clientDataJSON")]
    client_data_json: String,
    authenticator_data: String,
    signature: String,
    #[serde(default)]
    user_handle: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Assertion {
    raw_id: String,
    response: AssertionResponse,
}

/// Checks a passkey's answer; returns (user, whether the person was verified). `for_user`: the
/// passkey must be theirs (second step).
pub async fn verify(state: &AppState, challenge: &[u8], a: &Assertion, for_user: Option<Uuid>) -> ApiResult<(Uuid, bool)> {
    let bad = |e: anyhow::Error| ApiError::bad(format!("the passkey couldn't be checked: {e}"));
    let id = unb64(&a.raw_id).map_err(bad)?;
    let row: Option<(Uuid, Uuid, Vec<u8>, i64)> =
        sqlx::query_as("SELECT id, user_id, public_key, sign_count FROM passkeys WHERE credential_id = $1").bind(&id).fetch_optional(&state.db).await?;
    let Some((passkey, user, key, count)) = row else { return Err(ApiError::bad("this passkey isn't registered here")) };
    if for_user.is_some_and(|u| u != user) {
        return Err(ApiError::bad("this passkey belongs to someone else"));
    }
    if let Some(h) = a.response.user_handle.as_deref().filter(|h| !h.is_empty())
        && unb64(h).map_err(bad)? != user.as_bytes()
    {
        return Err(ApiError::bad("the passkey's user doesn't match"));
    }
    let client = unb64(&a.response.client_data_json).map_err(bad)?;
    let data = unb64(&a.response.authenticator_data).map_err(bad)?;
    let sig = unb64(&a.response.signature).map_err(bad)?;
    let v = webauthn::verify_assertion(&rp(state)?, challenge, &client, &data, &sig, &key, u32::try_from(count).unwrap_or(u32::MAX)).map_err(bad)?;
    // the counter moves forward atomically (two sign-ins with the same signature can't both pass)
    let n = sqlx::query("UPDATE passkeys SET sign_count = $2, backed_up = $3, last_used_at = now() WHERE id = $1 AND (sign_count < $2 OR $2 = 0)")
        .bind(passkey)
        .bind(i64::from(v.sign_count))
        .bind(v.backed_up)
        .execute(&state.db)
        .await?
        .rows_affected();
    if n == 0 {
        return Err(ApiError::bad("the passkey's signature counter went backwards"));
    }
    Ok((user, v.user_verified))
}

/// `POST /auth/passkey/options` — starts a sign-in without a password.
/// Signing in with a passkey alone stands in for the password, so it's off when sign-in goes
/// through single sign-on only (passkeys still work as the second step there).
fn passkey_login_allowed(state: &AppState) -> ApiResult<()> {
    if crate::sso::passwords_enabled(state) { Ok(()) } else { Err(ApiError::bad("sign-in with a passkey alone is turned off; use single sign-on")) }
}

pub async fn login_options(State(state): State<AppState>) -> ApiResult<Json<Value>> {
    passkey_login_allowed(&state)?;
    let challenge = webauthn::challenge();
    let token = mfa::put_step(&state, mfa::PASSKEY_LOGIN, None, Some(&challenge), None).await?;
    Ok(Json(json!({"token": token, "publicKey": request_options(&state, None, &challenge).await?})))
}

#[derive(Deserialize)]
pub struct PasskeyLogin {
    token: String,
    credential: Assertion,
}

/// `POST /auth/passkey {token, credential}` — signs in with a passkey that verified the person.
pub async fn login(State(state): State<AppState>, headers: HeaderMap, Json(req): Json<PasskeyLogin>) -> ApiResult<Response> {
    passkey_login_allowed(&state)?;
    let (_, challenge) = mfa::take_step(&state, mfa::PASSKEY_LOGIN, &req.token).await?;
    let challenge = challenge.ok_or_else(|| ApiError::bad("invalid sign-in"))?;
    let (user, verified) = verify(&state, &challenge, &req.credential, None).await?;
    if !verified {
        return Err(ApiError::bad(
            "this passkey didn't verify you (PIN, fingerprint or face): log in with your password and use the passkey as the second step",
        ));
    }
    let ws: Uuid = sqlx::query_scalar("SELECT workspace_id FROM users WHERE id = $1 AND status = 'active'")
        .bind(user)
        .fetch_optional(&state.db)
        .await?
        .ok_or(ApiError::Forbidden)?;
    let cookie = create_session(&state, user, &headers).await?;
    audit(&state, ws, Some(user), "auth.login", None, json!({"passkey": true})).await;
    Ok(([(header::SET_COOKIE, cookie)], Json(json!({ "ok": true }))).into_response())
}
