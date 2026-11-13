//! Outgoing e-mail over SMTP (`TASKCRAFT_SMTP_URL`): invitations, password resets and
//! notification e-mails.
//!
//! Mails go into the `mail_outbox` table and a worker on every replica sends them (rows are
//! leased with `FOR UPDATE SKIP LOCKED`, failures retried with backoff), so a request never waits
//! for the mail server and nothing is lost when a replica restarts.
//!
//! Notification e-mails follow each person's `preferences.email`:
//! - `instant`: bundled — once their oldest unmailed notification is two minutes old, everything
//!   pending goes out in one e-mail; what they've read, or got while using the app, is skipped;
//! - `digest` (the default): once a day after 08:00 in their time zone, what happened before;
//! - `off`.
//!
//! Muted kinds, deleted issues and issues they can no longer see are left out.

use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use lettre::message::header::{ContentType, HeaderName, HeaderValue};
use lettre::message::{Mailbox, MultiPart};
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::i18n::tr;

use crate::app::{AppState, new_id};
use crate::config::Config;

/// Attempts before an e-mail is given up (1, 2, 4, … minutes apart: about two hours).
pub const MAX_ATTEMPTS: i32 = 8;
/// Instant e-mails wait this long after a person's first unmailed notification, so a burst of
/// changes becomes one e-mail.
pub const BATCH_SECS: i64 = 120;
/// Instant e-mails are skipped for people who used the app this recently: they saw it live.
/// (Sessions are touched at most every 5 minutes; an open app polls every 2.)
pub const ACTIVE_SECS: i64 = 7 * 60;
/// Local hour of the daily digest.
pub const DIGEST_HOUR: i32 = 8;
/// Notifications not mailed within this time are dropped (e.g. mail was configured only now).
const STALE_HOURS: i32 = 72;
/// Advisory lock: one replica at a time turns notifications into e-mails.
const NOTIFY_LOCK: i64 = 0x7463_6d61_696c_0001;

// ---- SMTP ------------------------------------------------------------------------------------

/// Where mail goes, read from `TASKCRAFT_SMTP_URL` like the transport reads it (shown to admins;
/// never the password).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SmtpTarget {
    pub host: String,
    pub port: u16,
    /// `tls` (smtps://), `starttls` (`?tls=required`), `opportunistic` or `none`
    pub security: &'static str,
    pub user: Option<String>,
}

impl std::fmt::Display for SmtpTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let security = match self.security {
            "tls" => "TLS",
            "starttls" => "STARTTLS",
            "opportunistic" => "STARTTLS if offered",
            _ => "unencrypted",
        };
        write!(f, "{}:{} ({security})", self.host, self.port)
    }
}

/// Checks an SMTP URL: `smtps://user:pass@host[:465]`, `smtp://user:pass@host[:587]?tls=required`
/// (or `tls=opportunistic`) or `smtp://host[:25]`.
pub fn parse_smtp_url(url: &str) -> Result<SmtpTarget, String> {
    let u = reqwest::Url::parse(url.trim()).map_err(|e| format!("not a URL ({e})"))?;
    let tls = u.query_pairs().find(|(k, _)| k == "tls").map(|(_, v)| v.into_owned());
    let (security, default_port) = match (u.scheme(), tls.as_deref()) {
        ("smtps", _) => ("tls", 465),
        ("smtp", None) => ("none", 25),
        ("smtp", Some("required")) => ("starttls", 587),
        ("smtp", Some("opportunistic")) => ("opportunistic", 587),
        ("smtp", Some(other)) => return Err(format!("tls={other} isn't supported; use tls=required or tls=opportunistic")),
        (other, _) => return Err(format!("use smtps:// or smtp://, not {other}://")),
    };
    let host = u.host_str().filter(|h| !h.is_empty()).ok_or("the URL needs a host, e.g. smtps://user:password@mail.example.com")?;
    // credentials are used only together (as the transport does)
    let user = (u.password().is_some() && !u.username().is_empty()).then(|| u.username().to_string());
    Ok(SmtpTarget { host: host.to_string(), port: u.port().unwrap_or(default_port), security, user })
}

pub struct Mailer {
    transport: Option<AsyncSmtpTransport<Tokio1Executor>>,
    target: Option<SmtpTarget>,
    from: Mailbox,
}

impl Mailer {
    /// Without `TASKCRAFT_SMTP_URL` the mailer is disabled (nothing is queued or sent).
    pub fn new(config: &Config) -> anyhow::Result<Self> {
        let from: Mailbox = config.mail_from.parse().map_err(|e| anyhow::anyhow!("invalid TASKCRAFT_MAIL_FROM {:?}: {e}", config.mail_from))?;
        let Some(url) = config.smtp_url.as_deref().map(str::trim).filter(|u| !u.is_empty()) else {
            return Ok(Self { transport: None, target: None, from });
        };
        let target = parse_smtp_url(url).map_err(|e| anyhow::anyhow!("invalid TASKCRAFT_SMTP_URL: {e}"))?;
        let transport = AsyncSmtpTransport::<Tokio1Executor>::from_url(url)
            .map_err(|e| anyhow::anyhow!("invalid TASKCRAFT_SMTP_URL: {e}"))?
            .timeout(Some(Duration::from_secs(20)))
            .build();
        Ok(Self { transport: Some(transport), target: Some(target), from })
    }

    pub fn enabled(&self) -> bool {
        self.transport.is_some()
    }

    pub fn from_address(&self) -> String {
        self.from.to_string()
    }

    pub fn target(&self) -> Option<&SmtpTarget> {
        self.target.as_ref()
    }

    /// The address e-mails come from (lower case), e.g. to recognize our own e-mails coming back.
    pub fn from_email(&self) -> String {
        self.from.email.to_string().to_lowercase()
    }

    /// The domain of Message-IDs: the sender's.
    fn id_domain(&self) -> &str {
        self.from.email.domain()
    }

    /// The thread of an issue: every e-mail about it references this id, and replies quoting it
    /// are matched back to the issue (`crate::mail_in`).
    pub fn thread_id(&self, issue: Uuid) -> String {
        format!("<tc.{issue}@{}>", self.id_domain())
    }

    /// A fresh Message-ID for an e-mail about an issue (it names the issue too).
    pub fn message_id(&self, issue: Uuid) -> String {
        format!("<tc.{issue}.{}@{}>", Uuid::new_v4().simple(), self.id_domain())
    }

    /// Sends right away (the outbox worker and the admin's test e-mail).
    pub async fn send_now(&self, to: &str, subject: &str, text: &str, html: Option<&str>) -> anyhow::Result<()> {
        self.send_with(to, subject, text, html, &Headers::default()).await
    }

    /// Sends right away, with threading and reply headers.
    pub async fn send_with(&self, to: &str, subject: &str, text: &str, html: Option<&str>, h: &Headers) -> anyhow::Result<()> {
        let transport = self.transport.as_ref().ok_or_else(|| anyhow::anyhow!("e-mail is not configured (TASKCRAFT_SMTP_URL)"))?;
        let to: Mailbox = to.parse().map_err(|e| anyhow::anyhow!("invalid recipient {to:?}: {e}"))?;
        let from = match h.from_name.as_deref().map(header_text).filter(|n| !n.is_empty()) {
            Some(name) => Mailbox::new(Some(name), self.from.email.clone()),
            None => self.from.clone(),
        };
        let mut builder = Message::builder().from(from).to(to).subject(header_text(subject));
        if let Some(id) = &h.message_id {
            builder = builder.message_id(Some(header_text(id)));
        }
        if let Some(id) = &h.in_reply_to {
            builder = builder.in_reply_to(header_text(id));
        }
        if let Some(ids) = &h.references {
            builder = builder.references(header_text(ids));
        }
        if let Some(r) = h.reply_to.as_deref().and_then(|r| r.parse::<Mailbox>().ok()) {
            builder = builder.reply_to(r);
        }
        if let Some(a) = &h.auto_submitted {
            builder = builder.raw_header(HeaderValue::new(HeaderName::new_from_ascii_str("Auto-Submitted"), header_text(a)));
        }
        let msg = match html {
            Some(h) => builder.multipart(MultiPart::alternative_plain_html(text.to_string(), h.to_string()))?,
            None => builder.header(ContentType::TEXT_PLAIN).body(text.to_string())?,
        };
        transport.send(msg).await?;
        Ok(())
    }
}

/// A header value on one line (titles and names come from users).
fn header_text(s: &str) -> String {
    s.chars().map(|c| if c.is_control() { ' ' } else { c }).collect::<String>().trim().to_string()
}

/// Threading and reply headers of an e-mail, kept with it in the outbox. Message-IDs are given
/// with their angle brackets.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Headers {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_reply_to: Option<String>,
    /// space-separated Message-IDs
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub references: Option<String>,
    /// where replies go (a project's inbound address)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<String>,
    /// shown as the sender's name instead of TASKCRAFT_MAIL_FROM's (e.g. the project's name)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_name: Option<String>,
    /// RFC 3834 `Auto-Submitted`: `auto-generated` (notifications) or `auto-replied`
    /// (acknowledgements), so auto-responders leave them alone
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_submitted: Option<String>,
}

impl Headers {
    /// An e-mail about one issue: a Message-ID naming the issue, in the issue's thread.
    pub fn about_issue(mailer: &Mailer, issue: Uuid) -> Headers {
        let thread = mailer.thread_id(issue);
        Headers { message_id: Some(mailer.message_id(issue)), in_reply_to: Some(thread.clone()), references: Some(thread), ..Default::default() }
    }
}

// ---- outbox ----------------------------------------------------------------------------------

/// An e-mail to queue: subject, plain text and HTML alternatives.
pub struct Mail {
    pub to: String,
    pub subject: String,
    pub text: String,
    pub html: String,
    /// invite | reset | notification | digest
    pub purpose: &'static str,
}

async fn insert<'e>(db: impl sqlx::PgExecutor<'e>, m: &Mail, headers: Option<&Headers>) -> sqlx::Result<()> {
    sqlx::query("INSERT INTO mail_outbox (id, to_address, subject, body_text, body_html, purpose, headers) VALUES ($1, $2, $3, $4, $5, $6, $7)")
        .bind(new_id())
        .bind(&m.to)
        .bind(header_text(&m.subject))
        .bind(&m.text)
        .bind(&m.html)
        .bind(m.purpose)
        .bind(headers.map(sqlx::types::Json))
        .execute(db)
        .await?;
    Ok(())
}

/// Queues an e-mail. `false` (logged) when mail isn't configured.
pub async fn queue(state: &AppState, mail: &Mail) -> anyhow::Result<bool> {
    queue_with(state, mail, None).await
}

/// Queues an e-mail with threading and reply headers. `false` when mail isn't configured.
pub async fn queue_with(state: &AppState, mail: &Mail, headers: Option<&Headers>) -> anyhow::Result<bool> {
    if !state.mailer.enabled() {
        tracing::info!(subject = %mail.subject, "e-mail not configured; not sending");
        return Ok(false);
    }
    insert(&state.db, mail, headers).await?;
    Ok(true)
}

/// Starts this replica's mail worker (outbox and notification e-mails); does nothing without SMTP.
pub fn spawn_worker(state: &AppState) {
    if !state.mailer.enabled() {
        return;
    }
    let state = state.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(5));
        let mut n: u64 = 0;
        loop {
            tick.tick().await;
            if state.shutting_down.load(std::sync::atomic::Ordering::Relaxed) {
                return;
            }
            if n.is_multiple_of(4)
                && let Err(e) = notification_mails(&state).await
            {
                tracing::warn!("notification e-mails: {e:#}");
            }
            n += 1;
            if let Err(e) = drain(&state).await {
                tracing::warn!("mail outbox: {e:#}");
            }
        }
    });
}

#[derive(sqlx::FromRow)]
struct OutboxRow {
    id: Uuid,
    to_address: String,
    subject: String,
    body_text: String,
    body_html: Option<String>,
    attempts: i32,
    headers: Option<sqlx::types::Json<Headers>>,
}

/// Sends what's due in the outbox; returns how many went out. Rows are leased (not locked in an
/// open transaction) while the SMTP conversation runs, so a slow mail server doesn't hold
/// database connections.
pub async fn drain(state: &AppState) -> anyhow::Result<usize> {
    let mut sent = 0;
    loop {
        let rows = sqlx::query_as::<_, OutboxRow>(
            "WITH batch AS (
                SELECT id FROM mail_outbox WHERE sent_at IS NULL AND attempts < $1 AND next_attempt_at <= now()
                 ORDER BY created_at LIMIT 10 FOR UPDATE SKIP LOCKED)
             UPDATE mail_outbox o SET next_attempt_at = now() + interval '2 minutes' FROM batch WHERE o.id = batch.id
             RETURNING o.id, o.to_address, o.subject, o.body_text, o.body_html, o.attempts, o.headers",
        )
        .bind(MAX_ATTEMPTS)
        .fetch_all(&state.db)
        .await?;
        if rows.is_empty() {
            return Ok(sent);
        }
        for r in rows {
            let headers = r.headers.map(|h| h.0).unwrap_or_default();
            match state.mailer.send_with(&r.to_address, &r.subject, &r.body_text, r.body_html.as_deref(), &headers).await {
                Ok(()) => {
                    sqlx::query("UPDATE mail_outbox SET sent_at = now(), attempts = attempts + 1, last_error = NULL WHERE id = $1")
                        .bind(r.id)
                        .execute(&state.db)
                        .await?;
                    sent += 1;
                }
                Err(e) => {
                    tracing::warn!(to = %r.to_address, attempt = r.attempts + 1, "sending e-mail failed: {e:#}");
                    sqlx::query(
                        "UPDATE mail_outbox SET attempts = attempts + 1, last_error = $2, next_attempt_at = now() + make_interval(mins => $3) WHERE id = $1",
                    )
                    .bind(r.id)
                    .bind(format!("{e:#}"))
                    .bind(1i32 << r.attempts.clamp(0, 10))
                    .execute(&state.db)
                    .await?;
                }
            }
        }
    }
}

// ---- notification e-mails --------------------------------------------------------------------

/// Claims the notifications whose e-mail is due (or that are skipped) and returns them with
/// what the e-mail needs. $1 batch seconds, $2 stale hours, $3 digest hour, $4 active seconds.
const CLAIM: &str = "
WITH pending AS (
    SELECT n.id, n.user_id, n.created_at,
           CASE WHEN u.preferences->>'email' IN ('off', 'instant') THEN u.preferences->>'email' ELSE 'digest' END AS mode,
           COALESCE(z.name, 'UTC') AS tz
      FROM notifications n
      JOIN users u ON u.id = n.user_id
      LEFT JOIN workspaces w ON w.id = u.workspace_id
      -- the person's time zone, else the workspace's
      LEFT JOIN pg_timezone_names z ON z.name = COALESCE(NULLIF(u.timezone, ''), w.settings->>'timeZone')
     WHERE n.emailed_at IS NULL
),
due AS (
    SELECT p.* FROM pending p
     WHERE p.mode = 'off'
        OR p.created_at < now() - make_interval(hours => $2)
        OR (p.mode = 'instant' AND EXISTS (SELECT 1 FROM pending q WHERE q.user_id = p.user_id AND q.created_at < now() - make_interval(secs => $1)))
        OR (p.mode = 'digest'
            AND p.created_at < (date_trunc('day', (now() AT TIME ZONE p.tz) - make_interval(hours => $3)) + make_interval(hours => $3)) AT TIME ZONE p.tz)
),
claimed AS (
    UPDATE notifications n SET emailed_at = now() FROM due WHERE n.id = due.id
    RETURNING n.id, n.user_id, n.issue_id, n.actor_id, n.kind, n.data, n.created_at, n.read_at
)
SELECT c.user_id, d.mode, c.kind, c.data, c.read_at IS NOT NULL AS read,
       c.created_at < now() - make_interval(hours => $2) AS stale,
       (c.created_at AT TIME ZONE d.tz) AS at_local,
       a.name AS actor_name,
       i.id AS issue_id, i.project_id, pr.key || '-' || i.number AS issue_key, i.title AS issue_title,
       i.deleted_at IS NOT NULL AS deleted,
       left(cm.body_text, 400) AS comment_text,
       COALESCE((u.preferences->'muted') ? c.kind, false) AS muted,
       u.email, u.name AS user_name, u.status, w.name AS workspace, u.locale, w.settings->>'defaultLanguage' AS default_language, u.preferences->>'browserLanguage' AS browser_language,
       EXISTS (SELECT 1 FROM sessions s WHERE s.user_id = u.id AND s.expires_at > now() AND s.last_seen_at > now() - make_interval(secs => $4)) AS active
  FROM claimed c
  JOIN due d ON d.id = c.id
  JOIN users u ON u.id = c.user_id
  JOIN workspaces w ON w.id = u.workspace_id
  LEFT JOIN users a ON a.id = c.actor_id
  LEFT JOIN issues i ON i.id = c.issue_id
  LEFT JOIN projects pr ON pr.id = i.project_id
  LEFT JOIN comments cm ON cm.id = CASE WHEN c.data->>'commentId' ~* '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$'
                                        THEN (c.data->>'commentId')::uuid END
 ORDER BY c.user_id, c.created_at";

#[derive(sqlx::FromRow)]
struct Claimed {
    user_id: Uuid,
    mode: String,
    kind: String,
    data: Value,
    read: bool,
    stale: bool,
    at_local: chrono::NaiveDateTime,
    actor_name: Option<String>,
    issue_id: Option<Uuid>,
    project_id: Option<Uuid>,
    issue_key: Option<String>,
    issue_title: Option<String>,
    deleted: bool,
    comment_text: Option<String>,
    muted: bool,
    email: Option<String>,
    user_name: String,
    status: String,
    workspace: String,
    locale: Option<String>,
    default_language: Option<String>,
    browser_language: Option<String>,
    active: bool,
}

/// Turns due notifications into e-mails (one per person); returns how many were queued.
/// Runs on one replica at a time; the claim and the queued e-mails commit together.
pub async fn notification_mails(state: &AppState) -> anyhow::Result<usize> {
    if !state.mailer.enabled() {
        return Ok(0);
    }
    let any: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM notifications WHERE emailed_at IS NULL)").fetch_one(&state.db).await?;
    if !any {
        return Ok(0);
    }
    let mut tx = state.db.begin().await?;
    let locked: bool = sqlx::query_scalar("SELECT pg_try_advisory_xact_lock($1)").bind(NOTIFY_LOCK).fetch_one(&mut *tx).await?;
    if !locked {
        return Ok(0);
    }
    let rows: Vec<Claimed> =
        sqlx::query_as(CLAIM).bind(BATCH_SECS as f64).bind(STALE_HOURS).bind(DIGEST_HOUR).bind(ACTIVE_SECS as f64).fetch_all(&mut *tx).await?;
    let mut per_user: BTreeMap<Uuid, Vec<Claimed>> = BTreeMap::new();
    for r in rows {
        let skip = r.mode == "off"
            || r.stale
            || r.read
            || r.muted
            || r.deleted
            || r.issue_id.is_none()
            || r.status != "active"
            || r.email.as_deref().is_none_or(str::is_empty)
            || (r.mode == "instant" && r.active);
        if !skip {
            per_user.entry(r.user_id).or_default().push(r);
        }
    }
    let base = state.config.base().to_string();
    let mut access: HashMap<(Uuid, Uuid), bool> = HashMap::new();
    let mut mails = 0;
    for (user, items) in per_user {
        let l = crate::i18n::lang([items[0].locale.as_deref(), items[0].default_language.as_deref(), items[0].browser_language.as_deref()]);
        // access may have changed since the notification was created
        let mut issues: Vec<IssueUpdates> = Vec::new();
        let mut projects: HashMap<Uuid, Uuid> = HashMap::new();
        for n in &items {
            let (Some(issue), Some(project)) = (n.issue_id, n.project_id) else { continue };
            // the issue may have turned private since
            let visible = match access.get(&(user, issue)) {
                Some(v) => *v,
                None => {
                    let v = crate::access::sees(state, user, issue).await;
                    access.insert((user, issue), v);
                    v
                }
            };
            if !visible {
                continue;
            }
            let key = n.issue_key.clone().unwrap_or_default();
            projects.insert(issue, project);
            // people without an account (requesters writing by e-mail) are named in the data
            let who = n.actor_name.clone().or_else(|| n.data.get("by").and_then(Value::as_str).map(String::from)).unwrap_or_else(|| tr(l, "Automation", &[]));
            let excerpt = match n.kind.as_str() {
                "commented" | "mentioned" => n
                    .comment_text
                    .clone()
                    .or_else(|| n.data.get("excerpt").and_then(Value::as_str).map(String::from))
                    .map(|t| excerpt(&t, 200))
                    .filter(|t| !t.is_empty()),
                _ => None,
            };
            let update = Update { line: describe(l, &n.kind, &n.data, &who), excerpt, at: crate::i18n::day_time(l, n.at_local) };
            match issues.iter_mut().find(|i| i.id == issue) {
                Some(i) => i.items.push(update),
                None => issues.push(IssueUpdates {
                    id: issue,
                    url: format!("{base}/issues/{key}"),
                    key,
                    title: n.issue_title.clone().unwrap_or_default(),
                    items: vec![update],
                }),
            }
        }
        if issues.is_empty() {
            continue;
        }
        let first = &items[0];
        let cadence = if first.mode == "instant" { Cadence::Instant } else { Cadence::Digest };
        let (subject, text, html) = notification_mail(l, &base, cadence, &first.user_name, &first.workspace, &issues);
        let mail = Mail {
            to: first.email.clone().unwrap_or_default(),
            subject,
            text,
            html,
            purpose: if cadence == Cadence::Instant { "notification" } else { "digest" },
        };
        // an e-mail about one issue is part of its thread; replies to it become comments when
        // the project takes e-mail (`crate::mail_in`)
        let mut headers = match issues.as_slice() {
            [one] => {
                let mut h = Headers::about_issue(&state.mailer, one.id);
                if let Some(project) = projects.get(&one.id) {
                    h.reply_to = crate::mail_in::reply_address(state, *project).await;
                }
                h
            }
            _ => Headers::default(),
        };
        headers.auto_submitted = Some("auto-generated".into());
        insert(&mut *tx, &mail, Some(&headers)).await?;
        mails += 1;
    }
    tx.commit().await?;
    Ok(mails)
}

// ---- templates -------------------------------------------------------------------------------

/// HTML-escapes text (element content and attribute values).
pub fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// One line of text, at most `max` characters (with an ellipsis when cut).
pub fn excerpt(s: &str, max: usize) -> String {
    let flat = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        return flat;
    }
    let cut: String = flat.chars().take(max.saturating_sub(1)).collect();
    format!("{}…", cut.trim_end())
}

const ACCENT: &str = "#4f5fe0";

/// The HTML frame of every e-mail: light, inline styles, readable without images.
pub fn layout(title: &str, body: &str, footer: &str) -> String {
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width\"><title>{t}</title></head>\
         <body style=\"margin:0;padding:0;background:#f5f6f8;font-family:-apple-system,'Segoe UI',Roboto,Helvetica,Arial,sans-serif;color:#1f2328;font-size:15px\">\
         <div style=\"max-width:600px;margin:0 auto;padding:24px 16px\">\
         <div style=\"font-weight:700;font-size:17px;margin:0 0 16px\"><span style=\"display:inline-block;width:22px;height:22px;border-radius:6px;background:{ACCENT};color:#fff;text-align:center;line-height:22px;font-size:13px;margin-right:8px;vertical-align:middle\">T</span>TaskCraft</div>\
         <div style=\"background:#ffffff;border:1px solid #e3e5e8;border-radius:10px;padding:24px;line-height:1.55\">{body}</div>\
         <p style=\"color:#6b7280;font-size:12px;line-height:1.5;margin:16px 4px 0\">{footer}</p>\
         </div></body></html>",
        t = esc(title)
    )
}

/// An e-mail with one call to action (invitation, password reset): (text, html).
pub fn action_mail(l: &str, greeting: &str, intro: &str, button: &str, url: &str, outro: &str) -> (String, String) {
    let text = format!("{greeting}\n\n{intro}\n\n{button}:\n{url}\n\n{outro}\n");
    let html = layout(
        button,
        &format!(
            "<p style=\"margin:0 0 12px\">{greeting}</p><p style=\"margin:0 0 12px\">{intro}</p>\
             <p style=\"margin:24px 0\"><a href=\"{url}\" style=\"background:{ACCENT};color:#ffffff;padding:10px 18px;border-radius:8px;text-decoration:none;font-weight:600;display:inline-block\">{button}</a></p>\
             <p style=\"color:#57606a;font-size:13px;margin:0\">{or}<br><a href=\"{url}\" style=\"color:{ACCENT};word-break:break-all\">{url}</a></p>",
            or = esc(&tr(l, "Or open this link:", &[])),
            greeting = esc(greeting),
            intro = esc(intro),
            url = esc(url),
            button = esc(button),
        ),
        &esc(outro),
    );
    (text, html)
}

/// The invitation for someone an admin added: (subject, text, html).
pub fn invitation_mail(l: &str, inviter: &str, workspace: &str, name: &str, url: &str) -> (String, String, String) {
    let args = [("inviter", inviter), ("workspace", workspace), ("name", name)];
    let subject = tr(l, "{inviter} invited you to {workspace} on TaskCraft", &args);
    let (text, html) = action_mail(
        l,
        &tr(l, "Hi {name},", &args),
        &tr(l, "{inviter} invited you to join {workspace} on TaskCraft, where your team plans and tracks its work. Choose a password to get started.", &args),
        &tr(l, "Accept the invitation", &[]),
        url,
        &tr(l, "The link works once and expires in 7 days.", &[]),
    );
    (subject, text, html)
}

/// A link to choose a new password, asked for by the person (`by_admin` None, valid for an
/// hour) or made by an admin (valid for 7 days): (subject, text, html).
pub fn reset_mail(l: &str, name: &str, email: &str, url: &str, by_admin: Option<&str>) -> (String, String, String) {
    let (subject, intro, outro) = match by_admin {
        None => (
            tr(l, "Reset your TaskCraft password", &[]),
            tr(l, "Someone (hopefully you) asked to reset the password of your TaskCraft account {email}.", &[("email", email)]),
            tr(l, "The link works once and expires in an hour. If you didn't ask for this, ignore this e-mail: your password stays the same.", &[]),
        ),
        Some(admin) => (
            tr(l, "Choose a new TaskCraft password", &[]),
            tr(l, "{admin} sent you a link to choose a new password for your TaskCraft account {email}.", &[("admin", admin), ("email", email)]),
            tr(l, "The link works once and expires in 7 days. Choosing a new password signs you out on your other devices.", &[]),
        ),
    };
    let (text, html) = action_mail(l, &tr(l, "Hi {name},", &[("name", name)]), &intro, &tr(l, "Choose a new password", &[]), url, &outro);
    (subject, text, html)
}

/// A saved filter's results for its subscriber: (subject, text, html). Lists the first issues
/// (most recently changed first) and links to the filter.
pub fn subscription_mail(l: &str, base: &str, name: &str, filter: &str, view: Uuid, total: i64, issues: &[crate::issues::Summary]) -> (String, String, String) {
    let count = if total == 1 { tr(l, "1 issue", &[]) } else { tr(l, "{n} issues", &[("n", &total.to_string())]) };
    let subject = tr(l, "{filter}: {count}", &[("filter", filter), ("count", &count)]);
    let url = format!("{base}/search?view={view}");
    let hi = tr(l, "Hi {name},", &[("name", name)]);
    let intro = if total == 0 {
        tr(l, "Nothing matches your filter “{filter}” right now.", &[("filter", filter)])
    } else {
        tr(l, "This is what your filter “{filter}” finds now ({count}):", &[("filter", filter), ("count", &count)])
    };
    let more = (total > issues.len() as i64).then(|| tr(l, "and {n} more", &[("n", &(total - issues.len() as i64).to_string())]));
    let open = tr(l, "Open the filter", &[]);
    let why = tr(l, "You subscribed to this filter in TaskCraft. Change or stop it on the filter's page.", &[]);
    let line = |s: &crate::issues::Summary| {
        let who = s.assignee.as_ref().map(|a| a.name.clone()).unwrap_or_else(|| tr(l, "unassigned", &[]));
        format!("{} · {} · {}", s.status.name, s.priority, who)
    };
    let mut text = format!("{hi}\n\n{intro}\n");
    for s in issues {
        text.push_str(&format!("\n[{}] {}\n  {}\n  {}\n", s.key, s.title, line(s), s.url));
    }
    if let Some(m) = &more {
        text.push_str(&format!("\n… {m}\n"));
    }
    text.push_str(&format!("\n{open}: {url}\n\n--\n{why}\n"));
    let mut rows = String::new();
    for s in issues {
        rows.push_str(&format!(
            "<tr><td style=\"padding:6px 8px 6px 0;vertical-align:top;white-space:nowrap\"><a href=\"{u}\" style=\"color:{ACCENT};text-decoration:none;font-family:ui-monospace,SFMono-Regular,Menlo,Consolas,monospace;font-size:13px\">{k}</a></td>\
             <td style=\"padding:6px 0;vertical-align:top\"><a href=\"{u}\" style=\"color:#1f2328;text-decoration:none\">{t}</a><div style=\"color:#6b7280;font-size:13px\">{d}</div></td></tr>",
            u = esc(&s.url),
            k = esc(&s.key),
            t = esc(&s.title),
            d = esc(&line(s)),
        ));
    }
    let body = format!(
        "<p style=\"margin:0 0 6px\">{}</p><p style=\"margin:0 0 10px\">{}</p><table style=\"border-collapse:collapse;width:100%\">{rows}</table>{}\
         <p style=\"margin:20px 0 0\"><a href=\"{}\" style=\"background:{ACCENT};color:#ffffff;padding:9px 16px;border-radius:8px;text-decoration:none;font-weight:600;display:inline-block\">{}</a></p>",
        esc(&hi),
        esc(&intro),
        more.as_ref().map(|m| format!("<p style=\"color:#6b7280;margin:8px 0 0\">… {}</p>", esc(m))).unwrap_or_default(),
        esc(&url),
        esc(&open),
    );
    let html = layout(&subject, &body, &esc(&why));
    (subject, text, html)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cadence {
    /// a couple of minutes after it happened
    Instant,
    /// once a day
    Digest,
}

/// What happened on one issue.
pub struct IssueUpdates {
    pub id: Uuid,
    pub key: String,
    pub title: String,
    pub url: String,
    pub items: Vec<Update>,
}

pub struct Update {
    /// "Mira Meyer commented"
    pub line: String,
    /// the comment, shortened
    pub excerpt: Option<String>,
    /// local time, e.g. "Mon 14:05" (shown in digests)
    pub at: String,
}

/// One notification as a sentence, `who` being the person who did it.
pub fn describe(l: &str, kind: &str, data: &Value, who: &str) -> String {
    let s = |k: &str| data.get(k).and_then(Value::as_str).filter(|v| !v.is_empty());
    let w = [("who", who)];
    match kind {
        "assigned" => tr(l, "{who} assigned it to you", &w),
        "mentioned" if data.get("commentId").is_some_and(|c| !c.is_null()) => tr(l, "{who} mentioned you in a comment", &w),
        "mentioned" => tr(l, "{who} mentioned you in the description", &w),
        "commented" => tr(l, "{who} commented", &w),
        "status" => match (s("from"), s("to")) {
            (Some(from), Some(to)) => tr(l, "{who} changed the status from {from} to {to}", &[("who", who), ("from", from), ("to", to)]),
            (None, Some(to)) => tr(l, "{who} changed the status to {to}", &[("who", who), ("to", to)]),
            _ => tr(l, "{who} changed the status", &w),
        },
        "sla" if s("goal") == Some("response") => tr(l, "The goal for the first response was missed", &[]),
        "sla" => tr(l, "The goal for resolving it was missed", &[]),
        "automation" => match s("message") {
            Some(m) => tr(l, "Automation: {message}", &[("message", m)]),
            None => tr(l, "{who} updated it", &w),
        },
        _ => tr(l, "{who} updated it", &w),
    }
}

/// A notification e-mail (instant bundle or daily digest), grouped by issue:
/// (subject, text, html).
pub fn notification_mail(l: &str, base: &str, cadence: Cadence, name: &str, workspace: &str, issues: &[IssueUpdates]) -> (String, String, String) {
    let updates: usize = issues.iter().map(|i| i.items.len()).sum();
    let title = |i: &IssueUpdates| excerpt(&i.title, 80);
    let n_updates = |n: usize| if n == 1 { tr(l, "1 update", &[]) } else { tr(l, "{n} updates", &[("n", &n.to_string())]) };
    let n_issues = |n: usize| if n == 1 { tr(l, "1 issue", &[]) } else { tr(l, "{n} issues", &[("n", &n.to_string())]) };
    let subject = match cadence {
        Cadence::Digest => tr(l, "TaskCraft daily summary: {updates} on {issues}", &[("updates", &n_updates(updates)), ("issues", &n_issues(issues.len()))]),
        Cadence::Instant if issues.len() == 1 => {
            let i = &issues[0];
            let what = if updates == 1 { i.items[0].line.clone() } else { n_updates(updates) };
            format!("[{}] {} — {what}", i.key, title(i))
        }
        Cadence::Instant if issues.len() <= 3 => {
            let keys = issues.iter().map(|i| i.key.as_str()).collect::<Vec<_>>().join(", ");
            tr(l, "{updates} on {issues}", &[("updates", &n_updates(updates)), ("issues", &keys)])
        }
        Cadence::Instant => tr(l, "{updates} on {issues}", &[("updates", &n_updates(updates)), ("issues", &n_issues(issues.len()))]),
    };
    let intro = match cadence {
        Cadence::Digest => tr(l, "Here's what happened on your issues in {workspace}.", &[("workspace", workspace)]),
        Cadence::Instant => tr(l, "New on your issues in {workspace}:", &[("workspace", workspace)]),
    };
    let settings = format!("{base}/settings#notifications");
    let why = match cadence {
        Cadence::Digest => tr(l, "You get a daily summary of your TaskCraft notifications.", &[]),
        Cadence::Instant => tr(l, "You get an e-mail for each TaskCraft notification you haven't seen in the app.", &[]),
    };
    let change = tr(l, "Change this or turn e-mails off", &[]);
    let hi = tr(l, "Hi {name},", &[("name", name)]);

    let mut text = format!("{hi}\n\n{intro}\n");
    let mut html = format!("<p style=\"margin:0 0 6px\">{}</p><p style=\"margin:0 0 8px\">{}</p>", esc(&hi), esc(&intro));
    for i in issues {
        text.push_str(&format!("\n[{}] {}\n{}\n", i.key, i.title, i.url));
        html.push_str(&format!(
            "<div style=\"border-top:1px solid #e3e5e8;padding:14px 0 4px;margin-top:10px\">\
             <a href=\"{url}\" style=\"color:#1f2328;text-decoration:none;font-weight:600;font-size:15px\">\
             <span style=\"color:{ACCENT};font-family:ui-monospace,SFMono-Regular,Menlo,Consolas,monospace;font-size:13px;margin-right:6px\">{key}</span>{title}</a>\
             <ul style=\"margin:8px 0 0;padding:0 0 0 18px;color:#1f2328\">",
            url = esc(&i.url),
            key = esc(&i.key),
            title = esc(&i.title),
        ));
        for u in &i.items {
            let at = if cadence == Cadence::Digest && !u.at.is_empty() { format!(" ({})", u.at) } else { String::new() };
            text.push_str(&format!("  • {}{at}\n", u.line));
            html.push_str(&format!(
                "<li style=\"margin:0 0 6px\">{}{}",
                esc(&u.line),
                if at.is_empty() { String::new() } else { format!(" <span style=\"color:#6b7280;font-size:13px\">{}</span>", esc(&u.at)) }
            ));
            if let Some(e) = &u.excerpt {
                text.push_str(&format!("    “{e}”\n"));
                html.push_str(&format!("<div style=\"color:#57606a;border-left:3px solid #e3e5e8;padding:2px 0 2px 10px;margin:4px 0 0\">{}</div>", esc(e)));
            }
            html.push_str("</li>");
        }
        html.push_str(&format!(
            "</ul><p style=\"margin:4px 0 0\"><a href=\"{}\" style=\"color:{ACCENT};text-decoration:none;font-size:13px\">{} →</a></p></div>",
            esc(&i.url),
            esc(&tr(l, "Open {key}", &[("key", &i.key)]))
        ));
    }
    text.push_str(&format!("\n--\n{why} {change}: {settings}\n"));
    let footer = format!("{} <a href=\"{}\" style=\"color:#6b7280\">{}</a>.", esc(&why), esc(&settings), esc(&change));
    let html = layout(&subject, &html, &footer);
    (subject, text, html)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn smtp_urls() {
        let t = parse_smtp_url("smtps://mailer:s3cr%40t@mail.example.com").unwrap();
        assert_eq!(t, SmtpTarget { host: "mail.example.com".into(), port: 465, security: "tls", user: Some("mailer".into()) });
        assert_eq!(t.to_string(), "mail.example.com:465 (TLS)");
        let t = parse_smtp_url("smtp://u:p@mail.example.com?tls=required").unwrap();
        assert_eq!((t.port, t.security), (587, "starttls"));
        let t = parse_smtp_url("smtp://u:p@mail.example.com:2525?tls=opportunistic").unwrap();
        assert_eq!((t.port, t.security), (2525, "opportunistic"));
        let t = parse_smtp_url(" smtp://relay.internal:25 ").unwrap();
        assert_eq!(t, SmtpTarget { host: "relay.internal".into(), port: 25, security: "none", user: None });
        assert_eq!(parse_smtp_url("smtp://localhost").unwrap().port, 25);
        assert!(parse_smtp_url("https://mail.example.com").unwrap_err().contains("smtps://"));
        assert!(parse_smtp_url("smtp://mail.example.com?tls=always").unwrap_err().contains("tls=always"));
        assert!(parse_smtp_url("mail.example.com").is_err());
        assert!(parse_smtp_url("smtps://").is_err());
    }

    #[tokio::test]
    async fn mailer_from_config() {
        #[derive(clap::Parser)]
        struct Cli {
            #[command(flatten)]
            config: Config,
        }
        let mut config = <Cli as clap::Parser>::parse_from(["taskcraft", "--mail-from", "TaskCraft <taskcraft@localhost>"]).config;
        config.smtp_url = None;
        let m = Mailer::new(&config).unwrap();
        assert!(!m.enabled());
        assert_eq!(m.from_address(), "TaskCraft <taskcraft@localhost>");
        config.smtp_url = Some("smtp://user:pw@mail.example.com:587?tls=required".into());
        let m = Mailer::new(&config).unwrap();
        assert!(m.enabled());
        assert_eq!(m.target().unwrap().to_string(), "mail.example.com:587 (STARTTLS)");
        config.smtp_url = Some("imap://mail.example.com".into());
        assert!(Mailer::new(&config).is_err());
        config.smtp_url = None;
        config.mail_from = "not an address".into();
        assert!(Mailer::new(&config).is_err());
    }

    #[test]
    fn escaping_and_excerpts() {
        assert_eq!(esc(r#"<a href="x">Tom & 'Jerry'</a>"#), "&lt;a href=&quot;x&quot;&gt;Tom &amp; &#39;Jerry&#39;&lt;/a&gt;");
        assert_eq!(excerpt("  one\n two  ", 20), "one two");
        assert_eq!(excerpt("abcdefghij", 5), "abcd…");
        assert_eq!(header_text("Title\r\nBcc: x@y"), "Title  Bcc: x@y");
    }

    #[test]
    fn action_mails() {
        let (subject, text, html) = invitation_mail("en", "Olivia <Owner>", "Acme & Co", "Max", "http://tc.test/welcome?token=a&b");
        assert_eq!(subject, "Olivia <Owner> invited you to Acme & Co on TaskCraft");
        assert!(text.contains("Hi Max,") && text.contains("http://tc.test/welcome?token=a&b"), "{text}");
        assert!(html.contains("Olivia &lt;Owner&gt; invited you to join Acme &amp; Co"), "{html}");
        assert!(html.contains("href=\"http://tc.test/welcome?token=a&amp;b\""));
        assert!(!html.contains("<Owner>"));
        let (subject, text, _) = reset_mail("en", "Max", "max@example.com", "http://tc.test/welcome?token=r", None);
        assert_eq!(subject, "Reset your TaskCraft password");
        assert!(text.contains("expires in an hour") && text.contains("max@example.com"));
        let (subject, text, _) = reset_mail("en", "Max", "max@example.com", "http://tc.test/welcome?token=r", Some("Olivia"));
        assert_eq!(subject, "Choose a new TaskCraft password");
        assert!(text.contains("Olivia sent you a link"));
    }

    fn issue(key: &str, title: &str, items: Vec<Update>) -> IssueUpdates {
        IssueUpdates { id: Uuid::nil(), key: key.into(), title: title.into(), url: format!("http://tc.test/issues/{key}"), items }
    }

    fn update(line: &str, excerpt: Option<&str>) -> Update {
        Update { line: line.into(), excerpt: excerpt.map(String::from), at: "Mon 14:05".into() }
    }

    #[test]
    fn notification_mails() {
        assert_eq!(describe("en", "commented", &json!({}), "Mira"), "Mira commented");
        assert_eq!(describe("en", "status", &json!({"from": "To do", "to": "Done"}), "Mira"), "Mira changed the status from To do to Done");
        assert_eq!(describe("en", "mentioned", &json!({"commentId": null}), "Mira"), "Mira mentioned you in the description");
        assert_eq!(describe("en", "mentioned", &json!({"commentId": "0190"}), "Mira"), "Mira mentioned you in a comment");
        assert_eq!(describe("en", "assigned", &json!({}), "Automation"), "Automation assigned it to you");

        // one update: the subject says what happened
        let one = [issue("WEB-12", "Login <times> out", vec![update("Mira commented", Some("Looks like the cookie & the <proxy>"))])];
        let (subject, text, html) = notification_mail("en", "http://tc.test", Cadence::Instant, "Max", "Acme", &one);
        assert_eq!(subject, "[WEB-12] Login <times> out — Mira commented");
        assert!(
            text.contains("[WEB-12] Login <times> out\nhttp://tc.test/issues/WEB-12\n  • Mira commented\n    “Looks like the cookie & the <proxy>”"),
            "{text}"
        );
        assert!(text.contains("http://tc.test/settings#notifications"));
        assert!(!text.contains("Mon 14:05"), "instant mails don't need times");
        assert!(html.contains("Login &lt;times&gt; out") && html.contains("the cookie &amp; the &lt;proxy&gt;"), "{html}");
        assert!(!html.contains("<times>") && !html.contains("<proxy>"));
        assert!(html.contains("href=\"http://tc.test/settings#notifications\""));

        // a burst on one issue, then several issues
        let burst = [issue("WEB-12", "Login", vec![update("Mira commented", None), update("Mira changed the status to Done", None)])];
        assert_eq!(notification_mail("en", "http://tc.test", Cadence::Instant, "Max", "Acme", &burst).0, "[WEB-12] Login — 2 updates");
        let several = [issue("WEB-12", "Login", vec![update("a", None), update("b", None)]), issue("WEB-14", "Signup", vec![update("c", None)])];
        assert_eq!(notification_mail("en", "http://tc.test", Cadence::Instant, "Max", "Acme", &several).0, "3 updates on WEB-12, WEB-14");

        // the digest lists everything grouped by issue, with times
        let (subject, text, html) = notification_mail("en", "http://tc.test", Cadence::Digest, "Max", "Acme", &several);
        assert_eq!(subject, "TaskCraft daily summary: 3 updates on 2 issues");
        assert!(text.contains("Here's what happened on your issues in Acme."));
        let (a, b) = (text.find("[WEB-12] Login").unwrap(), text.find("[WEB-14] Signup").unwrap());
        assert!(a < b && text[a..b].contains("  • a (Mon 14:05)\n  • b (Mon 14:05)"), "{text}");
        assert!(html.contains("Open WEB-14"));
        let single = [issue("WEB-1", "x", vec![update("a", None)])];
        assert_eq!(notification_mail("en", "http://tc.test", Cadence::Digest, "Max", "Acme", &single).0, "TaskCraft daily summary: 1 update on 1 issue");
    }
}
