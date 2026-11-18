//! Signing in to the command-line client (`tc login`) through the browser.
//!
//! The client asks for a sign-in (`POST /cli/logins`) and gets a short code, a secret verifier and
//! the page to open (`/cli/authorize?code=…`). The person — signed in to the app as usual, second
//! factor and single sign-on included — sees which computer asks and approves. The client, which
//! polls `POST /cli/token` with its verifier, then receives an API token made for it (named
//! "tc on <computer>", listed and revocable under *Profile & settings → API tokens*). Codes live
//! ten minutes and work once; nothing is created before the approval.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use rand::RngExt;
use serde_json::{Value, json};

use crate::app::{AppState, audit, new_id};
use crate::auth::{API_TOKEN_PREFIX, SessionUser, random_token, token_hash};
use crate::error::{ApiError, ApiResult};

const MINUTES: i32 = 10;
/// Sign-ins waiting at once (they're made without an account: the table mustn't grow without end).
const MAX_PENDING: i64 = 500;

/// `ABCD-EFGH`: letters and digits that aren't mistaken for one another.
fn new_code() -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    let mut rng = rand::rng();
    let mut s = String::new();
    for i in 0..8 {
        if i == 4 {
            s.push('-');
        }
        s.push(ALPHABET[rng.random_range(0..ALPHABET.len())] as char);
    }
    s
}

fn clean_code(code: &str) -> String {
    code.trim().to_ascii_uppercase()
}

/// `POST /cli/logins` `{name?}` (no account needed): starts a sign-in.
pub async fn start(State(state): State<AppState>, Json(req): Json<Value>) -> ApiResult<(StatusCode, Json<Value>)> {
    sqlx::query("DELETE FROM cli_logins WHERE expires_at < now()").execute(&state.db).await?;
    let pending: i64 = sqlx::query_scalar("SELECT count(*) FROM cli_logins").fetch_one(&state.db).await?;
    if pending >= MAX_PENDING {
        return Err(ApiError::TooMany("too many sign-ins are waiting: try again in a few minutes".into()));
    }
    let name: String =
        req["name"].as_str().map(str::trim).filter(|n| !n.is_empty()).unwrap_or("a computer").chars().filter(|c| !c.is_control()).take(60).collect();
    let code = new_code();
    let verifier = random_token();
    sqlx::query("INSERT INTO cli_logins (code_hash, verifier_hash, name, expires_at) VALUES ($1, $2, $3, now() + make_interval(mins => $4))")
        .bind(token_hash(&code))
        .bind(token_hash(&verifier))
        .bind(&name)
        .bind(MINUTES)
        .execute(&state.db)
        .await?;
    Ok((
        StatusCode::CREATED,
        Json(
            json!({"code": code, "verifier": verifier, "url": format!("{}/cli/authorize?code={code}", state.config.base()), "expiresIn": MINUTES * 60, "interval": 2}),
        ),
    ))
}

/// `GET /cli/logins/{code}` (signed in with a session): which client asks.
pub async fn show(State(state): State<AppState>, SessionUser(_): SessionUser, Path(code): Path<String>) -> ApiResult<Json<Value>> {
    let row: Option<(String, chrono::DateTime<chrono::Utc>, bool)> =
        sqlx::query_as("SELECT name, created_at, user_id IS NOT NULL FROM cli_logins WHERE code_hash = $1 AND expires_at > now()")
            .bind(token_hash(&clean_code(&code)))
            .fetch_optional(&state.db)
            .await?;
    let (name, at, approved) = row.ok_or(ApiError::NotFound("sign-in"))?;
    Ok(Json(json!({"name": name, "createdAt": at, "approved": approved})))
}

/// `POST /cli/logins/{code}/approve` (a session, not a token: tokens don't make tokens): the
/// client may act as the caller.
pub async fn approve(State(state): State<AppState>, SessionUser(user): SessionUser, Path(code): Path<String>) -> ApiResult<StatusCode> {
    let name: Option<String> =
        sqlx::query_scalar("UPDATE cli_logins SET user_id = $2 WHERE code_hash = $1 AND expires_at > now() AND user_id IS NULL RETURNING name")
            .bind(token_hash(&clean_code(&code)))
            .bind(user.id)
            .fetch_optional(&state.db)
            .await?;
    let name = name.ok_or(ApiError::NotFound("sign-in"))?;
    audit(&state, user.workspace_id, Some(user.id), "cli.approved", Some(name), json!({})).await;
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /cli/logins/{code}` (a session): "that wasn't me".
pub async fn deny(State(state): State<AppState>, SessionUser(_): SessionUser, Path(code): Path<String>) -> ApiResult<StatusCode> {
    sqlx::query("DELETE FROM cli_logins WHERE code_hash = $1 AND user_id IS NULL").bind(token_hash(&clean_code(&code))).execute(&state.db).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /cli/token` `{verifier}` (no account needed): 202 while the person hasn't approved, then
/// — once — the API token; 404 when the sign-in ran out or was refused.
pub async fn token(State(state): State<AppState>, Json(req): Json<Value>) -> ApiResult<(StatusCode, Json<Value>)> {
    let verifier = req["verifier"].as_str().ok_or_else(|| ApiError::bad("verifier is required"))?;
    let hash = token_hash(verifier);
    let mut tx = state.db.begin().await?;
    let row: Option<(Option<uuid::Uuid>, String)> =
        sqlx::query_as("SELECT user_id, name FROM cli_logins WHERE verifier_hash = $1 AND expires_at > now() FOR UPDATE")
            .bind(&hash)
            .fetch_optional(&mut *tx)
            .await?;
    let (user, name) = row.ok_or(ApiError::NotFound("sign-in"))?;
    let Some(user) = user else {
        return Ok((StatusCode::ACCEPTED, Json(json!({"pending": true}))));
    };
    sqlx::query("DELETE FROM cli_logins WHERE verifier_hash = $1").bind(&hash).execute(&mut *tx).await?;
    let who: Option<(String, Option<String>, uuid::Uuid)> =
        sqlx::query_as("SELECT name, email, workspace_id FROM users WHERE id = $1 AND status = 'active'").bind(user).fetch_optional(&mut *tx).await?;
    let (user_name, email, workspace) = who.ok_or(ApiError::NotFound("sign-in"))?;
    let token = format!("{API_TOKEN_PREFIX}{}", random_token());
    let token_name = format!("tc on {name}");
    sqlx::query("INSERT INTO api_tokens (id, user_id, name, token_hash, scope) VALUES ($1, $2, $3, $4, 'write')")
        .bind(new_id())
        .bind(user)
        .bind(&token_name)
        .bind(token_hash(&token))
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    audit(&state, workspace, Some(user), "token.created", Some(token_name), json!({"scope": "write", "by": "cli"})).await;
    Ok((StatusCode::OK, Json(json!({"token": token, "user": {"name": user_name, "email": email}}))))
}
