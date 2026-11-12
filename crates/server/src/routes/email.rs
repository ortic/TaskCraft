//! E-mail flows: "forgot password", e-mailed invitation and reset links, and the admin's mail
//! status and test e-mail. Without SMTP, links are only shown to admins to pass on.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::app::{AppState, audit};
use crate::auth::{CurrentUser, User, require_admin};
use crate::error::{ApiError, ApiResult};
use crate::i18n::tr;
use crate::mail::Mail;

/// The language of an e-mail to an address in a workspace: its person's, else the workspace's
/// default, else (for someone new) the language of whoever sends it.
async fn lang_for(state: &AppState, workspace: Uuid, email: &str, sender: Option<Uuid>) -> &'static str {
    let row: (Option<String>, Option<String>, Option<String>) = sqlx::query_as(
        "SELECT (SELECT COALESCE(locale, preferences->>'browserLanguage') FROM users WHERE workspace_id = $1 AND lower(email) = lower($2) LIMIT 1),
                (SELECT settings->>'defaultLanguage' FROM workspaces WHERE id = $1),
                (SELECT locale FROM users WHERE id = $3)",
    )
    .bind(workspace)
    .bind(email)
    .bind(sender)
    .fetch_one(&state.db)
    .await
    .unwrap_or((None, None, None));
    crate::i18n::lang([row.0.as_deref(), row.1.as_deref(), row.2.as_deref()])
}

/// Self-service reset links are valid this long (admin-made links 7 days).
const RESET_HOURS: i32 = 1;
/// Reset e-mails per address and hour.
const RESETS_PER_HOUR: i64 = 3;

async fn workspace_name(state: &AppState, workspace: Uuid) -> String {
    sqlx::query_scalar("SELECT name FROM workspaces WHERE id = $1").bind(workspace).fetch_one(&state.db).await.unwrap_or_else(|_| "TaskCraft".into())
}

/// Queues an e-mail, logging failures: whether it was queued.
async fn queue(state: &AppState, mail: Mail) -> bool {
    match crate::mail::queue(state, &mail).await {
        Ok(queued) => queued,
        Err(e) => {
            tracing::warn!(purpose = mail.purpose, "queueing e-mail failed: {e:#}");
            false
        }
    }
}

/// E-mails an invitation link from `admin` (best effort; the admin sees the link anyway).
pub async fn send_invitation(state: &AppState, admin: &User, to: &str, name: &str, url: &str) -> bool {
    if !state.mailer.enabled() {
        return false;
    }
    let workspace = workspace_name(state, admin.workspace_id).await;
    let l = lang_for(state, admin.workspace_id, to, Some(admin.id)).await;
    let (subject, text, html) = crate::mail::invitation_mail(l, &admin.name, &workspace, name, url);
    queue(state, Mail { to: to.to_string(), subject, text, html, purpose: "invite" }).await
}

/// E-mails a password-reset link an admin made (best effort).
pub async fn send_reset_link(state: &AppState, admin: &User, to: &str, name: &str, url: &str) -> bool {
    if !state.mailer.enabled() {
        return false;
    }
    let l = lang_for(state, admin.workspace_id, to, None).await;
    let (subject, text, html) = crate::mail::reset_mail(l, name, to, url, Some(&admin.name));
    queue(state, Mail { to: to.to_string(), subject, text, html, purpose: "reset" }).await
}

#[derive(Deserialize)]
pub struct ForgotRequest {
    email: String,
}

/// `POST /api/auth/forgot` — e-mails a link to choose a new password. Answers 204 whether or not
/// the address has an account (no enumeration); at most three e-mails per address and hour.
pub async fn forgot(State(state): State<AppState>, Json(req): Json<ForgotRequest>) -> ApiResult<StatusCode> {
    if !state.mailer.enabled() {
        return Err(ApiError::bad("e-mail isn't set up here; ask an administrator for a password-reset link"));
    }
    if !crate::sso::passwords_enabled(&state) {
        return Err(ApiError::bad("password sign-in is turned off; use single sign-on"));
    }
    let Ok(email) = super::auth::validate_email(&req.email) else { return Ok(StatusCode::NO_CONTENT) };
    let user: Option<(Uuid, Uuid, String, String)> = sqlx::query_as(
        "SELECT id, workspace_id, name, email FROM users WHERE lower(email) = $1 AND status = 'active' AND password_hash IS NOT NULL ORDER BY created_at LIMIT 1",
    )
    .bind(&email)
    .fetch_optional(&state.db)
    .await?;
    let Some((id, workspace, name, to)) = user else { return Ok(StatusCode::NO_CONTENT) };
    let recent: i64 =
        sqlx::query_scalar("SELECT count(*) FROM mail_outbox WHERE to_address = $1 AND purpose = 'reset' AND created_at > now() - interval '1 hour'")
            .bind(&to)
            .fetch_one(&state.db)
            .await?;
    if recent >= RESETS_PER_HOUR {
        tracing::info!(user = %id, "password-reset e-mail throttled");
        return Ok(StatusCode::NO_CONTENT);
    }
    let url = super::auth::user_link_valid(&state, id, "reset", RESET_HOURS).await?;
    let l = crate::i18n::of_user(&state.db, id).await;
    let (subject, text, html) = crate::mail::reset_mail(l, &name, &to, &url, None);
    crate::mail::queue(&state, &Mail { to, subject, text, html, purpose: "reset" }).await?;
    audit(&state, workspace, Some(id), "auth.reset_requested", None, json!({})).await;
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /api/admin/mail` — whether mail is configured, and how the outbox is doing.
pub async fn status(State(state): State<AppState>, CurrentUser(user): CurrentUser) -> ApiResult<Json<Value>> {
    require_admin(&user)?;
    let (pending, failed, sent): (i64, i64, i64) = sqlx::query_as(
        "SELECT count(*) FILTER (WHERE sent_at IS NULL AND attempts < $1),
                count(*) FILTER (WHERE sent_at IS NULL AND attempts >= $1 AND created_at > now() - interval '7 days'),
                count(*) FILTER (WHERE sent_at > now() - interval '1 day')
           FROM mail_outbox",
    )
    .bind(crate::mail::MAX_ATTEMPTS)
    .fetch_one(&state.db)
    .await?;
    let last_error: Option<(String, chrono::DateTime<chrono::Utc>)> = sqlx::query_as(
        "SELECT last_error, created_at FROM mail_outbox WHERE last_error IS NOT NULL AND sent_at IS NULL AND created_at > now() - interval '7 days' ORDER BY created_at DESC LIMIT 1",
    )
    .fetch_optional(&state.db)
    .await?;
    Ok(Json(json!({
        "enabled": state.mailer.enabled(),
        "from": state.mailer.from_address(),
        "server": state.mailer.target().map(|t| t.to_string()),
        "pending": pending,
        "failed": failed,
        "sentLastDay": sent,
        "lastError": last_error.as_ref().map(|e| &e.0),
        "lastErrorAt": last_error.as_ref().map(|e| e.1),
    })))
}

/// `POST /api/admin/mail/test` — sends a test e-mail to the admin right away and reports what
/// the mail server said.
pub async fn test(State(state): State<AppState>, CurrentUser(user): CurrentUser) -> ApiResult<Json<Value>> {
    require_admin(&user)?;
    if !state.mailer.enabled() {
        return Err(ApiError::bad("e-mail isn't configured: set TASKCRAFT_SMTP_URL"));
    }
    let to = user.email.clone().filter(|e| !e.is_empty()).ok_or_else(|| ApiError::bad("your account has no e-mail address"))?;
    let l = crate::i18n::of_user(&state.db, user.id).await;
    let hi = tr(l, "Hi {name},", &[("name", &user.name)]);
    let works = tr(l, "E-mail from TaskCraft works: this test was sent from {address}.", &[("address", state.config.base())]);
    let text = format!("{hi}\n\n{works}\n");
    let subject = tr(l, "TaskCraft test e-mail", &[]);
    let html = crate::mail::layout(
        &subject,
        &format!("<p style=\"margin:0 0 12px\">{}</p><p style=\"margin:0\">{}</p>", crate::mail::esc(&hi), crate::mail::esc(&works)),
        &crate::mail::esc(&tr(l, "Sent from Administration → Overview.", &[])),
    );
    state.mailer.send_now(&to, &subject, &text, Some(&html)).await.map_err(|e| ApiError::bad(format!("sending failed: {e:#}")))?;
    audit(&state, user.workspace_id, Some(user.id), "mail.test", Some(to.clone()), json!({})).await;
    Ok(Json(json!({ "ok": true, "to": to })))
}
