//! Service desk portal and public request forms.
//!
//! People without an account who opened a request — by e-mail ([`crate::mail_in`]) or on a
//! project's public request form — follow it on a small portal at `/portal` (the web app's
//! `src/portal`, the API in [`crate::routes::portal`]):
//!
//! - **Sign-in** by a link e-mailed to them: valid [`LINK_MINUTES`] minutes and used once. Asking
//!   for one answers the same whether the address has requests or not (the e-mail goes out in the
//!   background, and only to addresses with requests: at most a few per quarter hour); each IP
//!   address may ask a limited number of times. The portal session (cookie `tc_portal`, sent
//!   only to `/api/portal`) is the e-mail address, nothing else.
//! - **Requests**: the ones opened for that address, in any project. The requester sees the
//!   title, the description, the status in plain words, their own messages, the comments the team
//!   marked for them (with the author's name, never an address) and their own files; they answer
//!   (comments like answers by e-mail), add files and, where the project lets them, mark a request
//!   resolved.
//! - **Request forms** (`settings.portal.form`): a public page per project at
//!   `/portal/forms/<key or slug>`; a submission becomes an issue with the submitter as its
//!   requester and an acknowledgement e-mail with the request's portal link. Bots are kept out by
//!   a hidden field, a minimum fill time (the form carries a signed timestamp), rate limits per IP
//!   and e-mail address and size limits; no third-party service is involved.

use std::net::{IpAddr, SocketAddr};

use axum::extract::{ConnectInfo, FromRequestParts};
use axum::http::request::Parts;
use axum::http::{HeaderMap, Method};
use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::Sha256;
use uuid::Uuid;

use crate::app::AppState;
use crate::auth::{CSRF_HEADER, cookie_value, random_token, token_hash};
use crate::error::{ApiError, ApiResult};
use crate::mail::{Headers, Mail, esc};

/// Sign-in links work this long (and once).
pub const LINK_MINUTES: i32 = 30;
/// A portal session lasts this long.
pub const SESSION_DAYS: i32 = 7;
pub const COOKIE: &str = "tc_portal";
/// Sign-in links one address gets per quarter hour (more asks are answered alike, but not sent).
pub const LINKS_PER_EMAIL: i64 = 3;
/// Sign-in asks per IP address and quarter hour.
pub const SIGN_INS_PER_IP: i64 = 10;
/// Form submissions per IP address and hour.
pub const FORMS_PER_IP: i64 = 20;
/// Form submissions per e-mail address and hour.
pub const FORMS_PER_EMAIL: i64 = 5;
/// Answers a requester may write in the portal per hour.
pub const REPLIES_PER_HOUR: i64 = 30;
/// A form sent sooner after it was opened is refused (people take longer).
pub const MIN_FILL_SECS: i64 = 3;
/// A form opened longer ago than this is refused (reload it).
pub const FORM_HOURS: i64 = 24;
/// Files one answer may carry, and their size together (at most `TASKCRAFT_MAX_UPLOAD_MB`).
pub const MAX_FILES: usize = 5;
pub const MAX_FILES_MB: u64 = 20;
/// Lengths of a form's summary, texts and name.
pub const MAX_SUMMARY: usize = 200;
pub const MAX_TEXT: usize = 20_000;
pub const MAX_NAME: usize = 100;

// ---- settings ----------------------------------------------------------------------------------

/// Whether a form shows a field, and whether it must be filled.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Show {
    Off,
    #[default]
    Optional,
    Required,
}

/// A custom field on the form.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FormField {
    pub id: Uuid,
    #[serde(default)]
    pub required: bool,
}

/// A project's public request form. Texts left empty show the portal's own (translated) ones.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Form {
    pub enabled: bool,
    /// its address instead of the project key (`/portal/forms/<slug>`), lower case
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slug: Option<String>,
    /// shown on the portal's start page
    pub listed: bool,
    pub title: String,
    /// shown above the fields (plain text, paragraphs)
    pub intro: String,
    /// shown after sending
    pub success: String,
    pub description: Show,
    pub name: Show,
    /// issue types to choose from (none: no choice)
    pub types: Vec<Uuid>,
    /// the type when there's no choice (else the project's first)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub type_id: Option<Uuid>,
    /// a priority to choose (low, medium, high, urgent)
    pub priority: bool,
    pub custom: Vec<FormField>,
    /// a WikiCraft space whose pages are suggested while the summary is typed
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggest_space: Option<String>,
}

impl Default for Form {
    fn default() -> Self {
        Form {
            enabled: false,
            slug: None,
            listed: true,
            title: String::new(),
            intro: String::new(),
            success: String::new(),
            description: Show::Optional,
            name: Show::Optional,
            types: Vec::new(),
            type_id: None,
            priority: false,
            custom: Vec::new(),
            suggest_space: None,
        }
    }
}

/// A project's portal settings (`projects.settings.portal`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    /// requesters may mark their requests resolved
    pub resolve: bool,
    /// the status resolved requests move to (a done status; else the first done one)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolve_status: Option<Uuid>,
    pub form: Form,
}

impl Settings {
    pub fn of(project_settings: &Value) -> Settings {
        serde_json::from_value(project_settings.get("portal").cloned().unwrap_or(Value::Null)).unwrap_or_default()
    }
}

/// Where a form lives: its slug, else the project key.
pub fn form_path(form: &Form, key: &str) -> String {
    form.slug.clone().unwrap_or_else(|| key.to_ascii_lowercase())
}

/// A request's page in the portal (for e-mails).
pub fn request_url(state: &AppState, key: &str) -> String {
    format!("{}/portal/requests/{key}", state.config.base())
}

/// A status category in the requester's words: open | in_progress | resolved.
pub fn plain_state(category: &str) -> &'static str {
    match category {
        "done" => "resolved",
        "in_progress" => "in_progress",
        _ => "open",
    }
}

// ---- who is asking -----------------------------------------------------------------------------

fn private(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_loopback() || v4.is_private() || v4.is_link_local() || v4.is_unspecified(),
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => private(IpAddr::V4(v4)),
            None => v6.is_loopback() || v6.is_unspecified() || (v6.segments()[0] & 0xfe00) == 0xfc00 || (v6.segments()[0] & 0xffc0) == 0xfe80,
        },
    }
}

/// The client's address for rate limits: the connection's peer, unless that is a proxy on a
/// private network — then the last public address the proxies saw (`X-Forwarded-For`, from the
/// right, else `X-Real-IP`). IPv6 clients count per /64 network.
pub fn client_ip(headers: &HeaderMap, peer: Option<IpAddr>) -> String {
    let forwarded = || {
        let chain: Vec<IpAddr> = headers
            .get_all("x-forwarded-for")
            .iter()
            .filter_map(|v| v.to_str().ok())
            .flat_map(|v| v.split(','))
            .filter_map(|s| s.trim().parse().ok())
            .collect();
        chain
            .iter()
            .rev()
            .find(|ip| !private(**ip))
            .or(chain.last())
            .copied()
            .or_else(|| headers.get("x-real-ip").and_then(|v| v.to_str().ok()).and_then(|v| v.trim().parse().ok()))
    };
    let ip = match peer {
        Some(p) if !private(p) => Some(p),
        _ => forwarded().or(peer),
    };
    match ip {
        Some(IpAddr::V6(v6)) if v6.to_ipv4_mapped().is_none() => {
            let s = v6.segments();
            format!("{:x}:{:x}:{:x}:{:x}::/64", s[0], s[1], s[2], s[3])
        }
        Some(IpAddr::V6(v6)) => v6.to_ipv4_mapped().map(|v4| v4.to_string()).unwrap_or_default(),
        Some(ip) => ip.to_string(),
        None => "unknown".into(),
    }
}

/// The client address of a request (see [`client_ip`]).
pub struct ClientIp(pub String);

impl<S: Send + Sync> FromRequestParts<S> for ClientIp {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        let peer = parts.extensions.get::<ConnectInfo<SocketAddr>>().map(|c| c.0.ip());
        Ok(ClientIp(client_ip(&parts.headers, peer)))
    }
}

/// Writes from the browser carry the CSRF header (like the app's).
pub fn csrf(method: &Method, headers: &HeaderMap) -> ApiResult<()> {
    if matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS) || headers.get(CSRF_HEADER).is_some() {
        Ok(())
    } else {
        Err(ApiError::BadRequest(format!("missing {CSRF_HEADER} header")))
    }
}

/// Whether `key` did `kind` fewer than `max` times in the last `secs` seconds.
pub async fn allowed(state: &AppState, kind: &str, key: &str, max: i64, secs: i32) -> ApiResult<bool> {
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM portal_attempts WHERE kind = $1 AND key = $2 AND at > now() - make_interval(secs => $3)")
        .bind(kind)
        .bind(key)
        .bind(secs as f64)
        .fetch_one(&state.db)
        .await?;
    Ok(n < max)
}

/// Notes that `key` did `kind` (for [`allowed`]).
pub async fn note(state: &AppState, kind: &str, key: &str) -> ApiResult<()> {
    sqlx::query("INSERT INTO portal_attempts (kind, key) VALUES ($1, $2)").bind(kind).bind(key).execute(&state.db).await?;
    Ok(())
}

/// Notes an attempt of `kind` by `key` if it is within `max` per `secs` (counting this one).
pub async fn attempt(state: &AppState, kind: &str, key: &str, max: i64, secs: i32) -> ApiResult<bool> {
    if !allowed(state, kind, key, max, secs).await? {
        return Ok(false);
    }
    note(state, kind, key).await?;
    Ok(true)
}

// ---- sessions ----------------------------------------------------------------------------------

pub fn cookie(token: &str, max_age: i64, secure: bool) -> String {
    format!("{COOKIE}={token}; Path=/api/portal; HttpOnly; SameSite=Lax; Max-Age={max_age}{}", if secure { "; Secure" } else { "" })
}

/// Starts a portal session for an address; the `Set-Cookie` value.
pub async fn start_session(state: &AppState, email: &str) -> ApiResult<String> {
    let token = random_token();
    sqlx::query("INSERT INTO portal_sessions (token_hash, email, expires_at) VALUES ($1, $2, now() + make_interval(days => $3))")
        .bind(token_hash(&token))
        .bind(email)
        .bind(SESSION_DAYS)
        .execute(&state.db)
        .await?;
    Ok(cookie(&token, SESSION_DAYS as i64 * 86400, state.config.secure_cookies))
}

/// The address signed in to the portal, if any.
pub async fn session_email(state: &AppState, headers: &HeaderMap) -> ApiResult<Option<String>> {
    let Some(token) = cookie_value(headers, COOKIE).filter(|t| !t.is_empty()) else { return Ok(None) };
    let hash = token_hash(token);
    let email: Option<String> =
        sqlx::query_scalar("SELECT email FROM portal_sessions WHERE token_hash = $1 AND expires_at > now()").bind(&hash).fetch_optional(&state.db).await?;
    if email.is_some() {
        let _ = sqlx::query("UPDATE portal_sessions SET last_seen_at = now() WHERE token_hash = $1 AND last_seen_at < now() - interval '5 minutes'")
            .bind(&hash)
            .execute(&state.db)
            .await;
    }
    Ok(email)
}

pub async fn end_session(state: &AppState, headers: &HeaderMap) -> ApiResult<()> {
    if let Some(token) = cookie_value(headers, COOKIE) {
        sqlx::query("DELETE FROM portal_sessions WHERE token_hash = $1").bind(token_hash(token)).execute(&state.db).await?;
    }
    Ok(())
}

/// A requester signed in to the portal: their e-mail address. Rejects others with 401, and
/// writes without the CSRF header.
pub struct Requester(pub String);

impl FromRequestParts<AppState> for Requester {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        csrf(&parts.method, &parts.headers)?;
        session_email(state, &parts.headers).await?.map(Requester).ok_or(ApiError::Unauthorized)
    }
}

/// The address signed in to the portal, or none.
pub struct MaybeRequester(pub Option<String>);

impl FromRequestParts<AppState> for MaybeRequester {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        Ok(MaybeRequester(session_email(state, &parts.headers).await?))
    }
}

// ---- sign-in links -----------------------------------------------------------------------------

/// The workspace's name (the portal's and its e-mails' sender name).
pub async fn workspace_name(state: &AppState) -> String {
    sqlx::query_scalar("SELECT name FROM workspaces ORDER BY created_at LIMIT 1").fetch_optional(&state.db).await.ok().flatten().unwrap_or_default()
}

/// E-mails a sign-in link to `email` when it has requests, at most [`LINKS_PER_EMAIL`] per
/// quarter hour; `request` (a key) is where the link leads. Whether one was queued.
pub async fn send_link(state: &AppState, email: &str, request: Option<&str>) -> anyhow::Result<bool> {
    let has: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM issue_requesters r JOIN issues i ON i.id = r.issue_id WHERE r.email = $1 AND i.deleted_at IS NULL)")
            .bind(email)
            .fetch_one(&state.db)
            .await?;
    if !has || !attempt(state, "sign-in-email", email, LINKS_PER_EMAIL, 15 * 60).await.map_err(|e| anyhow::anyhow!("{e:#}"))? {
        return Ok(false);
    }
    let token = random_token();
    sqlx::query("INSERT INTO portal_links (token_hash, email, expires_at) VALUES ($1, $2, now() + make_interval(mins => $3))")
        .bind(token_hash(&token))
        .bind(email)
        .bind(LINK_MINUTES)
        .execute(&state.db)
        .await?;
    let mut url = format!("{}/portal/sign-in?token={token}", state.config.base());
    if let Some(key) = request.filter(|k| crate::query::split_key(k).is_some()) {
        url.push_str(&format!("&request={}", key.to_ascii_uppercase()));
    }
    let workspace = workspace_name(state).await;
    let default: Option<String> = sqlx::query_scalar("SELECT settings->>'defaultLanguage' FROM workspaces ORDER BY created_at LIMIT 1")
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten()
        .flatten();
    let (subject, text, html) = sign_in_mail(crate::i18n::lang([default.as_deref()]), &url, &workspace);
    let from_name = (!workspace.is_empty()).then_some(workspace);
    let headers = Headers { from_name, ..Default::default() };
    crate::mail::queue_with(state, &Mail { to: email.to_string(), subject, text, html, purpose: "portal-link" }, Some(&headers)).await
}

/// (subject, text, html) of a sign-in link e-mail (no TaskCraft branding: requesters don't
/// know it).
pub fn sign_in_mail(l: &str, url: &str, workspace: &str) -> (String, String, String) {
    use crate::i18n::tr;
    let subject = tr(l, "Your sign-in link", &[]);
    let intro = tr(l, "Here is your link to sign in and follow your requests:", &[]);
    let outro = tr(l, "It works once, within {minutes} minutes. If you didn't ask for it, ignore this e-mail.", &[("minutes", &LINK_MINUTES.to_string())]);
    let hi = tr(l, "Hi,", &[]);
    let signature = if workspace.is_empty() { String::new() } else { format!("\n— {workspace}\n") };
    let text = format!("{hi}\n\n{intro}\n{url}\n\n{outro}\n{signature}");
    let html = format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width\"><title>{subject}</title></head>\
         <body style=\"margin:0;padding:16px;font-family:-apple-system,'Segoe UI',Roboto,Helvetica,Arial,sans-serif;color:#1f2328;font-size:15px;line-height:1.55\">\
         <div style=\"max-width:640px\"><p style=\"margin:0 0 12px\">{hi}</p><p style=\"margin:0 0 12px\">{intro}</p>\
         <p style=\"margin:16px 0\"><a href=\"{url}\" style=\"background:#4f5fe0;color:#ffffff;padding:9px 16px;border-radius:8px;text-decoration:none;font-weight:600;display:inline-block\">{sign_in}</a></p>\
         <p style=\"color:#57606a;font-size:13px;margin:0 0 12px\">{or}<br><a href=\"{url}\" style=\"color:#3241b5;word-break:break-all\">{url}</a></p>\
         <p style=\"color:#6b7280;font-size:13px;margin:24px 0 0\">{outro}{sig}</p></div></body></html>",
        subject = esc(&subject),
        hi = esc(&hi),
        sign_in = esc(&tr(l, "Sign in", &[])),
        or = esc(&tr(l, "Or open this link:", &[])),
        intro = esc(&intro),
        url = esc(url),
        outro = esc(&outro),
        sig = if workspace.is_empty() { String::new() } else { format!("<br>— {}", esc(workspace)) },
    );
    (subject, text, html)
}

/// Uses a sign-in link: the address it was for, once and while it's valid.
pub async fn redeem(state: &AppState, token: &str) -> ApiResult<Option<String>> {
    Ok(sqlx::query_scalar("DELETE FROM portal_links WHERE token_hash = $1 AND expires_at > now() RETURNING email")
        .bind(token_hash(token))
        .fetch_optional(&state.db)
        .await?)
}

// ---- forms: the fill-time stamp ----------------------------------------------------------------

async fn secret(state: &AppState) -> ApiResult<String> {
    if let Some(s) = sqlx::query_scalar("SELECT secret FROM portal_secret").fetch_optional(&state.db).await? {
        return Ok(s);
    }
    sqlx::query("INSERT INTO portal_secret (id, secret) VALUES (true, $1) ON CONFLICT (id) DO NOTHING").bind(random_token()).execute(&state.db).await?;
    Ok(sqlx::query_scalar("SELECT secret FROM portal_secret").fetch_one(&state.db).await?)
}

fn stamp_mac(secret: &str, project: Uuid, at: i64) -> Hmac<Sha256> {
    let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(secret.as_bytes()).expect("any key length");
    mac.update(format!("form:{project}:{at}").as_bytes());
    mac
}

/// The stamp a form carries: when it was opened (`at`, Unix seconds), signed.
pub async fn form_stamp(state: &AppState, project: Uuid, at: i64) -> ApiResult<String> {
    let mac = stamp_mac(&secret(state).await?, project, at);
    Ok(format!("{at}.{}", hex::encode(mac.finalize().into_bytes())))
}

/// How a form's stamp checks out.
#[derive(Debug, PartialEq, Eq)]
pub enum Stamp {
    Ok,
    /// sent sooner than [`MIN_FILL_SECS`] after opening
    TooFast,
    /// missing, forged or older than [`FORM_HOURS`]
    Invalid,
}

pub async fn check_stamp(state: &AppState, project: Uuid, stamp: &str) -> ApiResult<Stamp> {
    let Some((at, sig)) = stamp.split_once('.') else { return Ok(Stamp::Invalid) };
    let (Ok(at), Ok(sig)) = (at.parse::<i64>(), hex::decode(sig)) else { return Ok(Stamp::Invalid) };
    if stamp_mac(&secret(state).await?, project, at).verify_slice(&sig).is_err() {
        return Ok(Stamp::Invalid);
    }
    let age = chrono::Utc::now().timestamp() - at;
    // a minute's leeway for replicas whose clocks differ a little
    Ok(if !(-60..=FORM_HOURS * 3600).contains(&age) {
        Stamp::Invalid
    } else if age < MIN_FILL_SECS {
        Stamp::TooFast
    } else {
        Stamp::Ok
    })
}

// ---- what requesters see -----------------------------------------------------------------------

/// A document as a requester sees it: mentions and issue references as plain text, no images
/// (they live in the workspace), links only to the web and e-mail.
pub fn public_doc(v: &Value) -> Value {
    fn node(n: &Value) -> Option<Value> {
        match n["type"].as_str()? {
            "mention" => {
                let ch = n["attrs"]["mentionSuggestionChar"].as_str().unwrap_or("@");
                let label = n["attrs"]["label"].as_str().or(n["attrs"]["id"].as_str()).unwrap_or("");
                Some(json!({"type": "text", "text": if ch == "#" { label.to_string() } else { format!("{ch}{label}") }}))
            }
            "image" => None,
            "text" => {
                let marks: Vec<Value> = n["marks"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|m| match m["type"].as_str()? {
                        "link" => {
                            let href = m["attrs"]["href"].as_str().unwrap_or("");
                            let ok = href.starts_with("https://") || href.starts_with("http://") || href.starts_with("mailto:");
                            ok.then(|| json!({"type": "link", "attrs": {"href": href}}))
                        }
                        t @ ("bold" | "italic" | "strike" | "code" | "underline") => Some(json!({"type": t})),
                        _ => None,
                    })
                    .collect();
                let mut t = json!({"type": "text", "text": n["text"].as_str().unwrap_or("")});
                if !marks.is_empty() {
                    t["marks"] = json!(marks);
                }
                Some(t)
            }
            t => {
                let mut out = json!({"type": t});
                if let Some(level) = n["attrs"]["level"].as_i64() {
                    out["attrs"] = json!({"level": level});
                }
                if let Some(checked) = n["attrs"]["checked"].as_bool() {
                    out["attrs"] = json!({"checked": checked});
                }
                if let Some(start) = n["attrs"]["start"].as_i64() {
                    out["attrs"] = json!({"start": start});
                }
                if let Some(c) = n["content"].as_array() {
                    out["content"] = json!(c.iter().filter_map(node).collect::<Vec<_>>());
                }
                Some(out)
            }
        }
    }
    node(v).unwrap_or_else(crate::doc::empty)
}

// ---- housekeeping ------------------------------------------------------------------------------

/// Removes used-up links, sessions and old attempts (every hour; any replica).
pub fn spawn(state: &AppState) {
    let state = state.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(3600));
        loop {
            tick.tick().await;
            if state.shutting_down.load(std::sync::atomic::Ordering::Relaxed) {
                return;
            }
            let _ = sqlx::query("DELETE FROM portal_links WHERE expires_at < now()").execute(&state.db).await;
            let _ = sqlx::query("DELETE FROM portal_sessions WHERE expires_at < now()").execute(&state.db).await;
            let _ = sqlx::query("DELETE FROM portal_attempts WHERE at < now() - interval '1 day'").execute(&state.db).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_and_addresses() {
        assert_eq!(Settings::of(&json!({})), Settings::default());
        assert!(Settings::of(&json!({})).form.listed, "forms are listed unless switched off");
        let s = Settings::of(
            &json!({"portal": {"resolve": true, "form": {"enabled": true, "slug": "help", "description": "required", "custom": [{"id": Uuid::nil()}]}}}),
        );
        assert!(s.resolve && s.form.enabled && s.form.description == Show::Required && s.form.name == Show::Optional);
        assert_eq!(s.form.custom, vec![FormField { id: Uuid::nil(), required: false }]);
        assert_eq!(form_path(&s.form, "SUP"), "help");
        assert_eq!(form_path(&Form::default(), "SUP"), "sup");
        assert_eq!(Settings::of(&json!({"portal": {"form": {"description": "sometimes"}}})), Settings::default(), "invalid settings mean off");

        let h = |xff: &str| {
            let mut h = HeaderMap::new();
            h.insert("x-forwarded-for", xff.parse().unwrap());
            h
        };
        let public: IpAddr = "203.0.113.9".parse().unwrap();
        let proxy: IpAddr = "10.0.0.2".parse().unwrap();
        assert_eq!(client_ip(&h("1.2.3.4"), Some(public)), "203.0.113.9", "a public peer is the client: its headers are made up");
        assert_eq!(client_ip(&h("6.6.6.6, 198.51.100.7, 10.1.1.1"), Some(proxy)), "198.51.100.7", "behind proxies: the last public address");
        assert_eq!(client_ip(&HeaderMap::new(), Some(proxy)), "10.0.0.2");
        assert_eq!(client_ip(&HeaderMap::new(), None), "unknown");
        assert_eq!(client_ip(&h("2001:db8:1:2:3:4:5:6"), None), "2001:db8:1:2::/64");
        assert_eq!(plain_state("done"), "resolved");
        assert_eq!(plain_state("todo"), "open");
    }

    #[test]
    fn requesters_see_plain_documents() {
        let doc = json!({"type": "doc", "content": [
            {"type": "paragraph", "content": [
                {"type": "mention", "attrs": {"id": "0190f0e4-0000-7000-8000-000000000000", "label": "Max Member"}},
                {"type": "text", "text": " looks at "},
                {"type": "mention", "attrs": {"id": "WEB-3", "label": "WEB-3", "mentionSuggestionChar": "#"}},
                {"type": "text", "text": "here", "marks": [{"type": "link", "attrs": {"href": "javascript:alert(1)"}}, {"type": "bold"}]},
                {"type": "text", "text": "docs", "marks": [{"type": "link", "attrs": {"href": "https://example.com", "target": "_blank"}}]}
            ]},
            {"type": "image", "attrs": {"src": "/api/attachments/x"}},
            {"type": "heading", "attrs": {"level": 2, "id": "x"}, "content": [{"type": "text", "text": "Steps"}]}
        ]});
        let out = public_doc(&doc);
        assert_eq!(out["content"].as_array().unwrap().len(), 2, "no images");
        let p = &out["content"][0]["content"];
        assert_eq!(p[0], json!({"type": "text", "text": "@Max Member"}));
        assert_eq!(p[2], json!({"type": "text", "text": "WEB-3"}));
        assert_eq!(p[3], json!({"type": "text", "text": "here", "marks": [{"type": "bold"}]}));
        assert_eq!(p[4]["marks"], json!([{"type": "link", "attrs": {"href": "https://example.com"}}]));
        assert_eq!(out["content"][1]["attrs"], json!({"level": 2}));
        assert!(!out.to_string().contains("0190f0e4"));

        let (subject, text, html) = sign_in_mail("en", "https://t.example/portal/sign-in?token=a&request=SUP-1", "Acme & Co");
        assert_eq!(subject, "Your sign-in link");
        assert!(text.contains("https://t.example/portal/sign-in?token=a&request=SUP-1\n") && text.contains("— Acme & Co"), "{text}");
        assert!(html.contains("token=a&amp;request=SUP-1") && html.contains("Acme &amp; Co"), "{html}");
    }
}
