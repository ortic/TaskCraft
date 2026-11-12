//! E-mail in over HTTP (`POST /api/integrations/mail-in`, for mail servers and services) and a
//! project's e-mail settings with the messages it received. See `crate::mail_in`.

use axum::Json;
use axum::body::Body;
use axum::extract::{FromRequest, Multipart, Path, Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use futures::StreamExt;
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use super::projects::{ProjectRow, by_key};
use crate::access::Role;
use crate::app::{AppState, audit};
use crate::auth::{CurrentUser, User};
use crate::error::{ApiError, ApiResult};
use crate::events::{self, Event};
use crate::mail_in::{MAX_MESSAGE_MB, Senders, Settings, imap};

/// Of a message over the limit, this much is kept (its header, for the log).
const HEAD_BYTES: usize = 256 * 1024;

fn error(status: StatusCode, code: &str, message: &str) -> Response {
    (status, Json(json!({"error": code, "message": message}))).into_response()
}

/// The token a mail server sent: `Authorization: Bearer …`, `X-TaskCraft-Token` or `?token=`.
fn given_token<'a>(headers: &'a HeaderMap, query: Option<&'a str>) -> Option<String> {
    let h = |n: &str| headers.get(n).and_then(|v| v.to_str().ok());
    if let Some(t) = h("authorization").and_then(|a| a.strip_prefix("Bearer ")).or_else(|| h("x-taskcraft-token")) {
        return Some(t.trim().to_string());
    }
    query
        .and_then(|q| reqwest::Url::parse(&format!("http://x/?{q}")).ok())
        .and_then(|u| u.query_pairs().find(|(k, _)| k == "token").map(|(_, v)| v.into_owned()))
}

/// Reads at most `limit` bytes; past that only the beginning is kept. (bytes, size seen)
async fn read_limited(body: Body, limit: usize) -> Result<(Vec<u8>, usize), axum::Error> {
    let mut buf = Vec::new();
    let mut size = 0;
    let mut stream = body.into_data_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        size += chunk.len();
        if size > limit {
            break;
        }
        buf.extend_from_slice(&chunk);
    }
    if size > limit {
        buf.truncate(HEAD_BYTES);
    }
    Ok((buf, size))
}

/// `POST /api/integrations/mail-in` — one e-mail (raw RFC 822) from a mail server or service:
/// the request body itself, or a form field `email` (raw MIME as some services post it) or
/// `body-mime`. Answers 200 with what became of it (also when it was ignored or refused, so the
/// mail server doesn't bounce it), 401 for a wrong token, 503 when it should be sent again later.
pub async fn receive(State(state): State<AppState>, req: Request) -> Response {
    let Some(secret) = state.config.mail_in.token().map(str::to_string) else {
        return error(StatusCode::NOT_FOUND, "not_found", "e-mail in over HTTP is off (set TASKCRAFT_MAIL_IN_TOKEN)");
    };
    if !given_token(req.headers(), req.uri().query()).is_some_and(|t| crate::git::token_valid(&secret, &t)) {
        return error(StatusCode::UNAUTHORIZED, "unauthorized", "a valid token is required (Authorization: Bearer …, X-TaskCraft-Token or ?token=)");
    }
    let limit = MAX_MESSAGE_MB * 1024 * 1024;
    let form = req.headers().get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).is_some_and(|c| c.to_ascii_lowercase().starts_with("multipart/form-data"));
    let read = if form {
        let mut mp = match Multipart::from_request(req, &state).await {
            Ok(m) => m,
            Err(e) => return error(StatusCode::BAD_REQUEST, "bad_request", &format!("form: {e}")),
        };
        let mut found = None;
        loop {
            match mp.next_field().await {
                Ok(Some(mut field)) if matches!(field.name(), Some("email" | "body-mime" | "message" | "mime")) => {
                    let (mut buf, mut size) = (Vec::new(), 0);
                    loop {
                        match field.chunk().await {
                            Ok(Some(c)) => {
                                size += c.len();
                                if size > limit {
                                    break;
                                }
                                buf.extend_from_slice(&c);
                            }
                            Ok(None) => break,
                            Err(e) => return error(StatusCode::BAD_REQUEST, "bad_request", &format!("form: {e}")),
                        }
                    }
                    if size > limit {
                        buf.truncate(HEAD_BYTES);
                    }
                    found = Some((buf, size));
                    break;
                }
                Ok(Some(_)) => continue,
                Ok(None) => break,
                Err(e) => return error(StatusCode::BAD_REQUEST, "bad_request", &format!("form: {e}")),
            }
        }
        match found {
            Some(f) => f,
            None => return error(StatusCode::BAD_REQUEST, "bad_request", "no message: send it as the request body, or as the form field email"),
        }
    } else {
        match read_limited(req.into_body(), limit).await {
            Ok(r) => r,
            Err(e) => return error(StatusCode::BAD_REQUEST, "bad_request", &format!("reading the message: {e}")),
        }
    };
    let (raw, size) = read;
    if raw.is_empty() {
        return error(StatusCode::BAD_REQUEST, "bad_request", "the message is empty");
    }
    match crate::mail_in::receive(&state, &raw, size, "http").await {
        Ok(outcome) => (StatusCode::OK, Json(outcome)).into_response(),
        Err(e) => {
            tracing::warn!("e-mail in over HTTP: {e:#}");
            error(StatusCode::SERVICE_UNAVAILABLE, "unavailable", "the message couldn't be handled right now; send it again later")
        }
    }
}

#[derive(sqlx::FromRow)]
struct Received {
    id: Uuid,
    received_at: chrono::DateTime<chrono::Utc>,
    source: String,
    from_address: String,
    from_name: Option<String>,
    to_address: Option<String>,
    subject: String,
    status: String,
    reason: Option<String>,
    detail: Option<String>,
    issue_key: Option<String>,
    routed: bool,
}

/// The settings, how this server receives mail, and the latest messages (for workspace admins
/// also those that reached no project).
async fn overview(state: &AppState, user: &User, p: &ProjectRow) -> ApiResult<Value> {
    let settings = Settings::of(&p.settings);
    let imap = state.config.mail_in.imap().and_then(|u| imap::parse_url(u).ok());
    let checked: Option<(chrono::DateTime<chrono::Utc>, Option<chrono::DateTime<chrono::Utc>>, Option<String>)> =
        sqlx::query_as("SELECT checked_at, ok_at, error FROM mail_in_sources WHERE source = 'imap'").fetch_optional(&state.db).await?;
    let messages: Vec<Received> = sqlx::query_as(
        "SELECT m.id, m.received_at, m.source, m.from_address, m.from_name, m.to_address, m.subject, m.status, m.reason, m.detail,
                ip.key || '-' || i.number AS issue_key, m.project_id IS NOT NULL AS routed
           FROM mail_in m LEFT JOIN issues i ON i.id = m.issue_id LEFT JOIN projects ip ON ip.id = i.project_id
          WHERE m.project_id = $1 OR ($2 AND m.project_id IS NULL)
          ORDER BY m.received_at DESC LIMIT 50",
    )
    .bind(p.id)
    .bind(user.is_admin())
    .fetch_all(&state.db)
    .await?;
    Ok(json!({
        "settings": settings,
        "imap": imap.as_ref().map(|t| json!({
            "mailbox": t.to_string(),
            "checkedAt": checked.as_ref().map(|c| c.0),
            "okAt": checked.as_ref().and_then(|c| c.1),
            "error": checked.as_ref().and_then(|c| c.2.clone()),
        })),
        "http": state.config.mail_in.token().is_some(),
        "endpoint": format!("{}/api/integrations/mail-in", state.config.base()),
        "smtp": state.mailer.enabled(),
        "plusAddress": imap.as_ref().and_then(|t| t.plus_address(&p.key)),
        "messages": messages.into_iter().map(|m| json!({
            "id": m.id, "receivedAt": m.received_at, "source": m.source, "from": m.from_address, "fromName": m.from_name, "to": m.to_address,
            "subject": m.subject, "status": m.status, "reason": m.reason, "detail": m.detail, "issueKey": m.issue_key, "routed": m.routed,
        })).collect::<Vec<_>>(),
    }))
}

/// `GET /api/projects/{key}/mail-in` (project admins).
pub async fn settings(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(key): Path<String>) -> ApiResult<Json<Value>> {
    let (p, _) = by_key(&state, &user, &key, Role::Admin).await?;
    Ok(Json(overview(&state, &user, &p).await?))
}

#[derive(Deserialize)]
pub struct SettingsReq {
    enabled: bool,
    #[serde(default)]
    address: Option<String>,
    #[serde(default)]
    senders: Senders,
}

/// `PUT /api/projects/{key}/mail-in` — `{enabled, address, senders: members | anyone}`.
pub async fn put_settings(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Path(key): Path<String>,
    Json(req): Json<SettingsReq>,
) -> ApiResult<Json<Value>> {
    let (p, _) = by_key(&state, &user, &key, Role::Admin).await?;
    let address = match req.address.as_deref().map(str::trim).filter(|a| !a.is_empty()) {
        Some(a) => Some(super::auth::validate_email(a)?),
        None => None,
    };
    if let Some(a) = &address {
        if *a == state.mailer.from_email() {
            return Err(ApiError::bad("that's the address TaskCraft sends e-mail from: mail to it would go round in circles"));
        }
        let taken: Option<String> = sqlx::query_scalar("SELECT key FROM projects WHERE lower(settings->'mailIn'->>'address') = $1 AND id <> $2 LIMIT 1")
            .bind(a)
            .bind(p.id)
            .fetch_optional(&state.db)
            .await?;
        if let Some(k) = taken {
            return Err(ApiError::Conflict(format!("{a} is already the address of {k}")));
        }
    }
    let settings = Settings { enabled: req.enabled, address, senders: req.senders };
    sqlx::query("UPDATE projects SET settings = jsonb_set(settings, '{mailIn}', $2), updated_at = now() WHERE id = $1")
        .bind(p.id)
        .bind(serde_json::to_value(&settings)?)
        .execute(&state.db)
        .await?;
    audit(&state, user.workspace_id, Some(user.id), "project.mail_in", Some(p.key.clone()), serde_json::to_value(&settings)?).await;
    events::publish(&state, Event::new(user.workspace_id, "project").project(p.id).actor(user.id)).await;
    let (p, _) = by_key(&state, &user, &p.id.to_string(), Role::Admin).await?;
    Ok(Json(overview(&state, &user, &p).await?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens() {
        let mut h = HeaderMap::new();
        assert_eq!(given_token(&h, Some("a=1&token=s%2Bt")), Some("s+t".into()));
        assert_eq!(given_token(&h, None), None);
        h.insert("x-taskcraft-token", " abc ".parse().unwrap());
        assert_eq!(given_token(&h, Some("token=zzz")), Some("abc".into()));
        h.insert("authorization", "Bearer xyz".parse().unwrap());
        assert_eq!(given_token(&h, None), Some("xyz".into()));
    }
}
