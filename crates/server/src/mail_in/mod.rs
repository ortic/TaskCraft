//! E-mail in: issues and comments by e-mail, mostly for support trackers.
//!
//! Messages come from a mailbox TaskCraft reads every minute (`TASKCRAFT_IMAP_URL`, see
//! [`imap`]) or from a mail server or service posting them to
//! `POST /api/integrations/mail-in` (`TASKCRAFT_MAIL_IN_TOKEN`). Both end in [`receive`]:
//!
//! - **Routing**: a project takes e-mail when its settings say so (`settings.mailIn`), at its
//!   own address (`support@acme.com`) or at any address with `+KEY` (`tasks+SUP@acme.com`),
//!   matched against To, Cc, Delivered-To, X-Original-To and Envelope-To.
//! - **Replies** become comments: the issue is found by a Message-ID TaskCraft gave one of its
//!   e-mails (In-Reply-To / References), by an earlier e-mail of the same conversation, or by
//!   `[KEY-12]` in the subject. Quoted text and signatures are stripped.
//! - **Everything else** becomes an issue of the project's default type (subject → title, text
//!   → description; HTML is turned into text, never shown as HTML). Files become attachments.
//! - **Senders**: people with an account act as themselves, with their permissions (creating
//!   issues needs member access, commenting viewer access). Others only where the project lets
//!   anyone write: they become the issue's *requester*, get an acknowledgement with a link to
//!   the request in the portal ([`crate::portal`]), can answer by e-mail, and comments marked
//!   "visible to the requester" are sent to them (and shown in the portal).
//! - **Safety**: auto-replies, bulk mail and bounces are ignored, as are our own e-mails; a
//!   sender may send 30 messages an hour; messages over 40 MB are refused; each Message-ID is
//!   handled once; DMARC failures reported by the receiving server are refused. Every message
//!   is logged in `mail_in` with what became of it (shown in the project's e-mail settings).

pub mod imap;
pub mod parse;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::access::{self, Role};
use crate::app::{AppState, new_id};
use crate::auth::{USER_COLUMNS, User};
use crate::error::ApiError;
use crate::i18n::tr;
use crate::issues::{self, Actor, NewIssue, Summary};
use crate::mail::{Headers, Mail, esc};
use parse::Inbound;

/// Largest message accepted.
pub const MAX_MESSAGE_MB: usize = 40;
/// Messages one sender may send in an hour (loop protection).
pub const PER_SENDER_PER_HOUR: i64 = 30;
/// Acknowledgements one address gets in an hour.
const ACKS_PER_HOUR: i64 = 5;
/// A message whose handling failed this often is given up.
const MAX_FAILURES: i64 = 3;

/// Configuration (CLI flags or environment).
#[derive(Debug, Clone, Default, clap::Args)]
pub struct MailInConfig {
    /// Mailbox TaskCraft reads every minute for e-mail in:
    /// `imaps://user:password@imap.example.com/INBOX` (TLS, port 993),
    /// `imap://user:password@imap.example.com/INBOX?tls=starttls` (port 143), or `imap://…`
    /// unencrypted (local servers only). `?move=Folder` moves handled messages there instead of
    /// marking them read. URL-encode special characters in the password.
    #[arg(long, env = "TASKCRAFT_IMAP_URL", hide_env_values = true)]
    pub imap_url: Option<String>,

    /// Secret that mail servers and services send to `POST /api/integrations/mail-in`
    /// (`Authorization: Bearer …`, `X-TaskCraft-Token` or `?token=`). Unset: the endpoint is off.
    #[arg(long, env = "TASKCRAFT_MAIL_IN_TOKEN", hide_env_values = true)]
    pub mail_in_token: Option<String>,
}

impl MailInConfig {
    pub fn token(&self) -> Option<&str> {
        self.mail_in_token.as_deref().map(str::trim).filter(|t| !t.is_empty())
    }

    pub fn imap(&self) -> Option<&str> {
        self.imap_url.as_deref().map(str::trim).filter(|t| !t.is_empty())
    }
}

/// Who may e-mail a project.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Senders {
    /// people with an account and access to the project
    #[default]
    Members,
    /// anyone: unknown senders become requesters
    Anyone,
}

/// A project's e-mail settings (`projects.settings.mailIn`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    #[serde(default)]
    pub enabled: bool,
    /// its own inbound address (lower case)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
    #[serde(default)]
    pub senders: Senders,
}

impl Settings {
    pub fn of(project_settings: &Value) -> Settings {
        serde_json::from_value(project_settings.get("mailIn").cloned().unwrap_or(Value::Null)).unwrap_or_default()
    }
}

/// Where replies about a project's issues should go, when it takes e-mail: its own address, else
/// the mailbox's `+KEY` address (when the mailbox's user name is an e-mail address).
pub async fn reply_address(state: &AppState, project: Uuid) -> Option<String> {
    let (key, settings): (String, Value) =
        sqlx::query_as("SELECT key, settings FROM projects WHERE id = $1").bind(project).fetch_optional(&state.db).await.ok().flatten()?;
    let s = Settings::of(&settings);
    if !s.enabled {
        return None;
    }
    s.address.or_else(|| plus_address(state, &key))
}

/// The mailbox's `+KEY` address for a project (see [`imap::Target::plus_address`]).
pub fn plus_address(state: &AppState, key: &str) -> Option<String> {
    state.config.mail_in.imap().and_then(|u| imap::parse_url(u).ok()).and_then(|t| t.plus_address(key))
}

// ---- outcome and log -------------------------------------------------------------------------

/// What became of a message. `status`: created | commented | ignored | rejected | duplicate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Outcome {
    pub status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// the issue it created or commented on
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issue: Option<String>,
}

impl Outcome {
    fn done(status: &'static str, issue: String, detail: Option<String>) -> Outcome {
        Outcome { status, reason: None, detail, issue: Some(issue) }
    }

    fn ignored(reason: &'static str) -> Outcome {
        Outcome { status: "ignored", reason: Some(reason), detail: None, issue: None }
    }

    fn rejected(reason: &'static str, detail: impl Into<Option<String>>) -> Outcome {
        Outcome { status: "rejected", reason: Some(reason), detail: detail.into(), issue: None }
    }
}

/// The log row of the message being handled.
struct Entry {
    id: Uuid,
    project: Option<Uuid>,
    issue: Option<Uuid>,
    to: Option<String>,
}

enum Claim {
    Mine,
    Duplicate,
    GaveUp,
}

/// Takes a message for handling, once per Message-ID.
async fn claim(state: &AppState, id: Uuid, source: &str, message_id: &str, m: Option<&Inbound>, size: usize) -> anyhow::Result<Claim> {
    // an attempt that died half-way (a restart) counts as failed
    sqlx::query("UPDATE mail_in SET status = 'failed', reason = 'interrupted' WHERE message_id = $1 AND status = 'processing' AND received_at < now() - interval '10 minutes'")
        .bind(message_id)
        .execute(&state.db)
        .await?;
    let failures: i64 =
        sqlx::query_scalar("SELECT count(*) FROM mail_in WHERE message_id = $1 AND status = 'failed'").bind(message_id).fetch_one(&state.db).await?;
    let status = if failures >= MAX_FAILURES { "rejected" } else { "processing" };
    let inserted = sqlx::query(
        "INSERT INTO mail_in (id, source, message_id, from_address, from_name, subject, size, status, reason)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
         ON CONFLICT (message_id) WHERE status NOT IN ('failed', 'duplicate') DO NOTHING",
    )
    .bind(id)
    .bind(source)
    .bind(message_id)
    .bind(m.and_then(|m| m.from.clone()).unwrap_or_default())
    .bind(m.and_then(|m| m.from_name.clone()))
    .bind(m.map(|m| m.subject.clone()).unwrap_or_default())
    .bind(size.min(i32::MAX as usize) as i32)
    .bind(status)
    .bind((status == "rejected").then_some("failing"))
    .execute(&state.db)
    .await?
    .rows_affected();
    if inserted == 0 {
        // seen before: noted, nothing else
        sqlx::query(
            "INSERT INTO mail_in (id, source, message_id, from_address, from_name, subject, size, status, reason) VALUES ($1, $2, $3, $4, $5, $6, $7, 'duplicate', 'duplicate')",
        )
        .bind(id)
        .bind(source)
        .bind(message_id)
        .bind(m.and_then(|m| m.from.clone()).unwrap_or_default())
        .bind(m.and_then(|m| m.from_name.clone()))
        .bind(m.map(|m| m.subject.clone()).unwrap_or_default())
        .bind(size.min(i32::MAX as usize) as i32)
        .execute(&state.db)
        .await?;
        return Ok(Claim::Duplicate);
    }
    Ok(if status == "rejected" { Claim::GaveUp } else { Claim::Mine })
}

async fn finish(state: &AppState, e: &Entry, o: &Outcome) -> anyhow::Result<()> {
    sqlx::query("UPDATE mail_in SET status = $2, reason = $3, detail = $4, project_id = $5, issue_id = $6, to_address = $7 WHERE id = $1")
        .bind(e.id)
        .bind(o.status)
        .bind(o.reason)
        .bind(o.detail.as_deref().map(|d| d.chars().take(1000).collect::<String>()))
        .bind(e.project)
        .bind(e.issue)
        .bind(&e.to)
        .execute(&state.db)
        .await?;
    Ok(())
}

/// Handles one received message (`raw`; only its beginning when `size` is over the limit).
/// `Ok` whatever became of it (the source may forget it); `Err` when it should be tried again
/// later (database or storage trouble).
pub async fn receive(state: &AppState, raw: &[u8], size: usize, source: &'static str) -> anyhow::Result<Outcome> {
    let inbound = Inbound::parse(raw);
    let message_id = inbound.as_ref().and_then(|m| m.message_id.clone()).unwrap_or_else(|| format!("sha256:{}", hex::encode(Sha256::digest(raw))));
    let id = new_id();
    match claim(state, id, source, &message_id, inbound.as_ref(), size).await? {
        Claim::Mine => {}
        Claim::Duplicate => return Ok(Outcome { status: "duplicate", reason: Some("duplicate"), detail: None, issue: None }),
        Claim::GaveUp => return Ok(Outcome::rejected("failing", None)),
    }
    let mut entry = Entry { id, project: None, issue: None, to: None };
    let result = match &inbound {
        None => Ok(Outcome::rejected("unreadable", None)),
        Some(m) => handle(state, m, size, &mut entry).await,
    };
    match result {
        Ok(outcome) => {
            // done either way: trying it again could make a second issue
            if let Err(e) = finish(state, &entry, &outcome).await {
                tracing::warn!("e-mail in: noting the outcome failed: {e:#}");
            }
            tracing::info!(source, status = outcome.status, reason = outcome.reason, issue = outcome.issue.as_deref(), "e-mail in");
            Ok(outcome)
        }
        Err(e) => {
            let _ = sqlx::query("UPDATE mail_in SET status = 'failed', reason = 'error', detail = $2, project_id = $3 WHERE id = $1")
                .bind(id)
                .bind(format!("{e:#}").chars().take(1000).collect::<String>())
                .bind(entry.project)
                .execute(&state.db)
                .await;
            Err(e)
        }
    }
}

/// An `ApiError` from creating an issue or comment: a lasting problem with this message
/// (`Ok(text)`), or trouble worth trying again (`Err`).
fn lasting(e: ApiError) -> anyhow::Result<String> {
    match e {
        ApiError::Internal(e) => Err(e),
        other => Ok(other.to_string()),
    }
}

// ---- routing ---------------------------------------------------------------------------------

#[derive(Debug, Clone, sqlx::FromRow)]
struct Project {
    id: Uuid,
    workspace_id: Uuid,
    key: String,
    name: String,
    settings: Value,
    archived: bool,
}

const PROJECT: &str = "SELECT p.id, p.workspace_id, p.key, p.name, p.settings, p.archived_at IS NOT NULL AS archived FROM projects p";
const TAKES_MAIL: &str = "p.settings->'mailIn'->'enabled' = 'true'::jsonb";

/// The project a message is addressed to, with the address that matched: a project's own
/// address first, then `+KEY` in any address.
async fn route(state: &AppState, m: &Inbound) -> anyhow::Result<Option<(Project, String)>> {
    if m.recipients.is_empty() {
        return Ok(None);
    }
    let own: Vec<Project> = sqlx::query_as(sqlx::AssertSqlSafe(format!("{PROJECT} WHERE {TAKES_MAIL} AND lower(p.settings->'mailIn'->>'address') = ANY($1)")))
        .bind(&m.recipients)
        .fetch_all(&state.db)
        .await?;
    for r in &m.recipients {
        if let Some(p) = own.iter().find(|p| Settings::of(&p.settings).address.as_deref() == Some(r.as_str())) {
            return Ok(Some((p.clone(), r.clone())));
        }
    }
    for r in &m.recipients {
        let local = r.split('@').next().unwrap_or("");
        let Some((_, key)) = local.rsplit_once('+') else { continue };
        let key = key.to_ascii_uppercase();
        if key.is_empty() || key.len() > 20 {
            continue;
        }
        let p: Option<Project> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "{PROJECT} WHERE {TAKES_MAIL} AND (upper(p.key) = $1 OR p.id IN (SELECT project_id FROM project_key_aliases WHERE upper(key) = $1))
              ORDER BY upper(p.key) = $1 DESC LIMIT 1"
        )))
        .bind(&key)
        .fetch_optional(&state.db)
        .await?;
        if let Some(p) = p {
            return Ok(Some((p, r.clone())));
        }
    }
    Ok(None)
}

async fn project(state: &AppState, id: Uuid) -> anyhow::Result<Option<Project>> {
    Ok(sqlx::query_as(sqlx::AssertSqlSafe(format!("{PROJECT} WHERE p.id = $1"))).bind(id).fetch_optional(&state.db).await?)
}

/// The issue a message answers, in a project that takes e-mail: by the conversation
/// (Message-IDs of our e-mails or of earlier messages), else by `[KEY-12]` in the subject (in
/// the project it was sent to, if any).
async fn answered_issue(state: &AppState, m: &Inbound, routed: Option<&Project>) -> anyhow::Result<Option<(issues::Row, Project)>> {
    let mut candidates: Vec<Uuid> = parse::issue_ids(&m.references);
    if !m.references.is_empty() {
        let earlier: Vec<Uuid> = sqlx::query_scalar(
            "SELECT issue_id FROM mail_in WHERE message_id = ANY($1) AND issue_id IS NOT NULL AND status IN ('created', 'commented') ORDER BY received_at DESC",
        )
        .bind(&m.references)
        .fetch_all(&state.db)
        .await?;
        candidates.extend(earlier);
    }
    for id in candidates {
        if let Some(found) = open_issue(state, id).await? {
            return Ok(Some(found));
        }
    }
    for (key, number) in parse::subject_keys(&m.subject) {
        let id: Option<Uuid> = sqlx::query_scalar(
            "SELECT i.id FROM issues i JOIN projects p ON p.id = i.project_id
              WHERE i.number = $2 AND i.deleted_at IS NULL
                AND (upper(p.key) = $1 OR p.id IN (SELECT project_id FROM project_key_aliases WHERE upper(key) = $1))
             UNION ALL
             SELECT i.id FROM issue_key_aliases a JOIN issues i ON i.id = a.issue_id
              WHERE upper(a.project_key) = $1 AND a.number = $2 AND i.deleted_at IS NULL
             LIMIT 1",
        )
        .bind(&key)
        .bind(number)
        .fetch_optional(&state.db)
        .await?;
        if let Some(id) = id
            && let Some((row, p)) = open_issue(state, id).await?
            && routed.is_none_or(|r| r.id == p.id)
        {
            return Ok(Some((row, p)));
        }
    }
    Ok(None)
}

/// An issue (not deleted) whose project takes e-mail.
async fn open_issue(state: &AppState, id: Uuid) -> anyhow::Result<Option<(issues::Row, Project)>> {
    let row = match issues::load(state, id).await {
        Ok(r) => r,
        Err(ApiError::NotFound(_)) => return Ok(None),
        Err(e) => return Err(anyhow::anyhow!("{e:#}")),
    };
    let Some(p) = project(state, row.project_id).await? else { return Ok(None) };
    Ok(Settings::of(&p.settings).enabled.then_some((row, p)))
}

/// An active person of the workspace with this e-mail address.
async fn person(state: &AppState, workspace: Uuid, email: &str) -> anyhow::Result<Option<User>> {
    Ok(sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {USER_COLUMNS} FROM users WHERE workspace_id = $1 AND lower(email) = $2 AND status = 'active' ORDER BY created_at LIMIT 1"
    )))
    .bind(workspace)
    .bind(email)
    .fetch_optional(&state.db)
    .await?)
}

/// Mail from TaskCraft itself or from a project's inbound address: never handled (loops).
async fn from_us(state: &AppState, from: &str) -> anyhow::Result<bool> {
    if state.mailer.from_email() == from {
        return Ok(true);
    }
    Ok(sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM projects WHERE lower(settings->'mailIn'->>'address') = $1)").bind(from).fetch_one(&state.db).await?)
}

async fn handle(state: &AppState, m: &Inbound, size: usize, entry: &mut Entry) -> anyhow::Result<Outcome> {
    // the project it's for, first: so even what's refused shows up in that project's log
    let routed = route(state, m).await?;
    entry.to = routed.as_ref().map(|r| r.1.clone());
    entry.project = routed.as_ref().map(|r| r.0.id);
    if size > MAX_MESSAGE_MB * 1024 * 1024 {
        return Ok(Outcome::rejected("too-large", format!("{} MB", size.div_ceil(1024 * 1024))));
    }
    if let Some(why) = m.automatic {
        return Ok(Outcome::ignored(why));
    }
    let Some(from) = m.from.clone() else { return Ok(Outcome::rejected("no-sender", None)) };
    if from_us(state, &from).await? {
        return Ok(Outcome::ignored("loop"));
    }
    if m.dmarc_fail {
        return Ok(Outcome::rejected("forged", None));
    }
    let recent: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM mail_in WHERE from_address = $1 AND received_at > now() - interval '1 hour' AND status <> 'duplicate' AND id <> $2",
    )
    .bind(&from)
    .bind(entry.id)
    .fetch_one(&state.db)
    .await?;
    if recent >= PER_SENDER_PER_HOUR {
        return Ok(Outcome::rejected("rate-limit", None));
    }
    if let Some((issue, project)) = answered_issue(state, m, routed.as_ref().map(|r| &r.0)).await? {
        entry.project = Some(project.id);
        entry.issue = Some(issue.id);
        return reply(state, m, &from, &issue, &project).await;
    }
    let Some((project, address)) = routed else {
        return Ok(Outcome::rejected("no-project", (!m.recipients.is_empty()).then(|| m.recipients.join(", "))));
    };
    entry.project = Some(project.id);
    new_issue(state, m, &from, &project, &address, entry).await
}

// ---- issues and comments ---------------------------------------------------------------------

/// The person outside the workspace an issue was opened for (by e-mail, on the request form or
/// in the portal).
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Requester {
    pub email: String,
    pub name: Option<String>,
    pub reply_to: Option<String>,
    pub last_message_id: Option<String>,
    /// email | form | portal
    pub source: String,
}

impl Requester {
    /// "Jane Doe" or the address.
    pub fn display(&self) -> String {
        self.name.clone().unwrap_or_else(|| self.email.clone())
    }
}

pub async fn requester(state: &AppState, issue: Uuid) -> sqlx::Result<Option<Requester>> {
    sqlx::query_as("SELECT email, name, reply_to, last_message_id, source FROM issue_requesters WHERE issue_id = $1")
        .bind(issue)
        .fetch_optional(&state.db)
        .await
}

/// Stores a message's files on an issue (as large as uploads may be); notes what was left out.
async fn attach(state: &AppState, m: &Inbound, issue: &Summary, uploader: Option<Uuid>, by: Option<&str>) -> Vec<String> {
    let max = (state.config.max_upload_mb as usize).saturating_mul(1024 * 1024);
    let mut notes = Vec::new();
    let mut stored = 0;
    for a in &m.attachments {
        if a.data.len() > max {
            notes.push(format!("{} is larger than {} MB and was left out", a.name, state.config.max_upload_mb));
            continue;
        }
        match crate::routes::attachments::store(state, issue.id, issue.project_id, uploader, by, &a.name, &a.mime, a.data.clone()).await {
            Ok(_) => stored += 1,
            Err(e) => {
                tracing::warn!(file = %a.name, "e-mail in: storing a file failed: {e:#}");
                notes.push(format!("{} couldn't be stored", a.name));
            }
        }
    }
    if m.skipped_files > 0 {
        notes.push(format!("{} more files were left out", m.skipped_files));
    }
    if stored > 0 {
        let workspace: Option<Uuid> =
            sqlx::query_scalar("SELECT workspace_id FROM projects WHERE id = $1").bind(issue.project_id).fetch_optional(&state.db).await.ok().flatten();
        if let Some(w) = workspace {
            crate::events::publish(state, crate::events::Event::new(w, "issue").project(issue.project_id).issue(issue.id, &issue.key)).await;
        }
    }
    notes
}

/// The names of the files that come along, for the comment's text.
fn files_line(m: &Inbound, max_bytes: usize) -> Option<String> {
    let names: Vec<&str> = m.attachments.iter().filter(|a| a.data.len() <= max_bytes).map(|a| a.name.as_str()).collect();
    (!names.is_empty()).then(|| format!("📎 {}", names.join(" · ")))
}

/// A reply to an issue: a comment by the sender, if they may.
async fn reply(state: &AppState, m: &Inbound, from: &str, issue: &issues::Row, project: &Project) -> anyhow::Result<Outcome> {
    let key = issue.key();
    let user = person(state, project.workspace_id, from).await?;
    let role = match &user {
        // a private issue they don't see is as good as not there
        Some(u) if !access::sees(state, u.id, issue.id).await => Role::None,
        Some(u) => access::role(state, u, project.id).await.map_err(|e| anyhow::anyhow!("{e:#}"))?,
        None => Role::None,
    };
    let req = requester(state, issue.id).await?;
    let is_requester = req.as_ref().is_some_and(|r| r.email == from);
    let (actor, external, files_ok) = match user {
        Some(u) if role >= Role::Viewer => (Actor::of(&u), None, role >= Role::Member),
        _ if is_requester => (
            Actor { user: None, workspace: project.workspace_id, depth: 0 },
            Some((from.to_string(), m.from_name.clone().or_else(|| req.as_ref().and_then(|r| r.name.clone())))),
            true,
        ),
        _ => return Ok(Outcome::rejected("not-allowed", format!("{from} → {key}"))),
    };
    let max = (state.config.max_upload_mb as usize).saturating_mul(1024 * 1024);
    let mut text = parse::strip_reply(&m.text);
    let files = if files_ok { files_line(m, max) } else { None };
    if let Some(f) = &files {
        text = if text.is_empty() { f.clone() } else { format!("{text}\n\n{f}") };
    }
    if text.trim().is_empty() {
        return Ok(Outcome::ignored("empty"));
    }
    let by = external.as_ref().map(|(e, n)| n.clone().unwrap_or_else(|| e.clone()));
    // the requester's own messages are part of their conversation in the portal
    let opts = crate::routes::comments::CommentOpts { via_email: true, for_requester: external.is_some(), external: external.clone(), ..Default::default() };
    if let Err(e) = crate::routes::comments::add_with(state, &actor, issue.id, parse::text_to_doc(&text), &opts).await {
        return Ok(Outcome::rejected("invalid", lasting(e)?));
    }
    // the comment exists: from here on, trouble is noted rather than tried again
    let notes = if files_ok { attach(state, m, &issues::summary(state, issue.clone()), actor.user, by.as_deref()).await } else { Vec::new() };
    if is_requester && actor.user.is_none() {
        // e-mails to them continue from their latest one
        let _ = sqlx::query("UPDATE issue_requesters SET last_message_id = COALESCE($2, last_message_id), name = COALESCE(name, $3) WHERE issue_id = $1")
            .bind(issue.id)
            .bind(&m.message_id)
            .bind(&m.from_name)
            .execute(&state.db)
            .await;
    }
    Ok(Outcome::done("commented", key, (!notes.is_empty()).then(|| notes.join("; "))))
}

/// Who an issue was opened for (someone without an account).
pub struct NewRequester<'a> {
    pub email: &'a str,
    pub name: Option<&'a str>,
    /// the address replies to them name as Reply-To (where they wrote to)
    pub reply_to: Option<&'a str>,
    /// Message-ID of their e-mail
    pub message_id: Option<&'a str>,
    /// email | form | portal
    pub source: &'static str,
}

/// Notes who an issue was opened for, names them in its history and has someone follow it (the
/// assignee, else the project lead), so their answers reach the team.
pub async fn add_requester(state: &AppState, s: &Summary, r: &NewRequester<'_>) -> anyhow::Result<()> {
    sqlx::query("INSERT INTO issue_requesters (issue_id, email, name, reply_to, last_message_id, source) VALUES ($1, $2, $3, $4, $5, $6)")
        .bind(s.id)
        .bind(r.email)
        .bind(r.name)
        .bind(r.reply_to)
        .bind(r.message_id)
        .bind(r.source)
        .execute(&state.db)
        .await?;
    sqlx::query("UPDATE issue_events SET new_value = new_value || jsonb_build_object('by', $2::text) WHERE issue_id = $1 AND kind = 'created'")
        .bind(s.id)
        .bind(r.name.unwrap_or(r.email))
        .execute(&state.db)
        .await?;
    sqlx::query(
        "INSERT INTO issue_watchers (issue_id, user_id)
         SELECT i.id, COALESCE(i.assignee_id, p.lead_id) FROM issues i JOIN projects p ON p.id = i.project_id
          WHERE i.id = $1 AND COALESCE(i.assignee_id, p.lead_id) IS NOT NULL
         ON CONFLICT DO NOTHING",
    )
    .bind(s.id)
    .execute(&state.db)
    .await?;
    Ok(())
}

/// A new message to a project: an issue, if the sender may.
async fn new_issue(state: &AppState, m: &Inbound, from: &str, project: &Project, address: &str, entry: &mut Entry) -> anyhow::Result<Outcome> {
    if project.archived {
        return Ok(Outcome::rejected("archived", None));
    }
    let settings = Settings::of(&project.settings);
    let user = person(state, project.workspace_id, from).await?;
    let role = match &user {
        Some(u) => access::role(state, u, project.id).await.map_err(|e| anyhow::anyhow!("{e:#}"))?,
        None => Role::None,
    };
    let (actor, external) = match user {
        Some(u) if role >= Role::Member => (Actor::of(&u), false),
        _ if settings.senders == Senders::Anyone => (Actor { user: None, workspace: project.workspace_id, depth: 0 }, true),
        _ => return Ok(Outcome::rejected("not-allowed", format!("{from} → {}", project.key))),
    };
    let title = if m.topic.trim().is_empty() { format!("E-mail from {}", m.from_name.as_deref().unwrap_or(from)) } else { m.topic.clone() };
    let description = (!m.text.trim().is_empty()).then(|| parse::text_to_doc(&m.text));
    let s = match issues::create(state, &actor, NewIssue { project: project.id, title, description, fields: Map::new() }).await {
        Ok(s) => s,
        Err(e) => return Ok(Outcome::rejected("invalid", lasting(e)?)),
    };
    entry.issue = Some(s.id);
    let by = external.then(|| m.from_name.clone().unwrap_or_else(|| from.to_string()));
    // the issue exists: from here on, trouble is noted rather than tried again (that would make
    // a second issue)
    let mut notes = Vec::new();
    let r = NewRequester { email: from, name: m.from_name.as_deref(), reply_to: Some(address), message_id: m.message_id.as_deref(), source: "email" };
    if external && let Err(e) = add_requester(state, &s, &r).await {
        tracing::warn!(issue = %s.key, "e-mail in: noting the requester failed: {e:#}");
        notes.push("the requester couldn't be noted".to_string());
    }
    notes.extend(attach(state, m, &s, actor.user, by.as_deref()).await);
    if external {
        match acknowledge(state, &s, &project.name, from, m.from_name.as_deref(), Some(address), m.message_id.as_deref()).await {
            Ok(true) => {}
            Ok(false) => notes.push("no acknowledgement sent".into()),
            Err(e) => {
                tracing::warn!(issue = %s.key, "e-mail in: acknowledgement: {e:#}");
                notes.push("the acknowledgement couldn't be queued".into());
            }
        }
    }
    Ok(Outcome::done("created", s.key, (!notes.is_empty()).then(|| notes.join("; "))))
}

// ---- e-mails to requesters -------------------------------------------------------------------

/// A plain conversation e-mail (no TaskCraft branding: requesters don't know it).
fn conversation_html(title: &str, body: &str, footer: &str) -> String {
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width\"><title>{}</title></head>\
         <body style=\"margin:0;padding:16px;font-family:-apple-system,'Segoe UI',Roboto,Helvetica,Arial,sans-serif;color:#1f2328;font-size:15px;line-height:1.55\">\
         <div style=\"max-width:640px\">{body}<p style=\"color:#6b7280;font-size:13px;margin:24px 0 0\">{footer}</p></div></body></html>",
        esc(title)
    )
}

/// Headers of an e-mail to an issue's requester: in their conversation, from the project,
/// answers going to the project's address.
fn requester_headers(state: &AppState, issue: Uuid, project_name: &str, reply_to: Option<String>, their_id: Option<&str>) -> Headers {
    let thread = state.mailer.thread_id(issue);
    let theirs = their_id.map(|id| format!("<{}>", id.trim_matches(|c| c == '<' || c == '>')));
    Headers {
        message_id: Some(state.mailer.message_id(issue)),
        in_reply_to: Some(theirs.clone().unwrap_or_else(|| thread.clone())),
        references: Some(match theirs {
            Some(t) => format!("{t} {thread}"),
            None => thread,
        }),
        reply_to,
        from_name: Some(project_name.to_string()),
        auto_submitted: None,
    }
}

/// The acknowledgement for a new issue from a requester (at most a few per hour and address).
/// `reply_to`: the project's address when it takes e-mail (they may answer by e-mail).
pub async fn acknowledge(
    state: &AppState,
    s: &Summary,
    project_name: &str,
    to: &str,
    name: Option<&str>,
    reply_to: Option<&str>,
    their_id: Option<&str>,
) -> anyhow::Result<bool> {
    if !state.mailer.enabled() {
        return Ok(false);
    }
    let recent: i64 =
        sqlx::query_scalar("SELECT count(*) FROM mail_outbox WHERE to_address = $1 AND purpose = 'mail-in-ack' AND created_at > now() - interval '1 hour'")
            .bind(to)
            .fetch_one(&state.db)
            .await?;
    if recent >= ACKS_PER_HOUR {
        return Ok(false);
    }
    let follow = FollowUp { portal: &crate::portal::request_url(state, &s.key), by_mail: reply_to.is_some() };
    let l = crate::i18n::of_project(&state.db, s.project_id).await;
    let (subject, text, html) = acknowledgement_mail(l, &s.key, &s.title, name, project_name, &follow);
    let mut headers = requester_headers(state, s.id, project_name, reply_to.map(str::to_string), their_id);
    headers.auto_submitted = Some("auto-replied".into());
    crate::mail::queue_with(state, &Mail { to: to.to_string(), subject, text, html, purpose: "mail-in-ack" }, Some(&headers)).await
}

/// How a requester follows up on a request: in the portal, and by e-mail when the project
/// takes e-mail.
pub struct FollowUp<'a> {
    /// the request's page in the portal
    pub portal: &'a str,
    /// answers by e-mail reach the project
    pub by_mail: bool,
}

/// A link styled as a button, for e-mails to requesters.
fn button(url: &str, label: &str) -> String {
    format!(
        "<p style=\"margin:16px 0\"><a href=\"{url}\" style=\"background:#4f5fe0;color:#ffffff;padding:9px 16px;border-radius:8px;text-decoration:none;font-weight:600;display:inline-block\">{}</a></p>",
        esc(label),
        url = esc(url)
    )
}

/// (subject, text, html) of an acknowledgement.
pub fn acknowledgement_mail(l: &str, key: &str, title: &str, name: Option<&str>, project: &str, follow: &FollowUp) -> (String, String, String) {
    let subject = format!("[{key}] {title}");
    let greeting = match name {
        Some(n) => tr(l, "Hi {name},", &[("name", n)]),
        None => tr(l, "Hi,", &[]),
    };
    let intro = tr(l, "thanks for your message. It's filed as {key}, and we'll get back to you.", &[("key", key)]);
    let online = tr(l, "Follow your request and answer online:", &[]);
    let how = if follow.by_mail {
        tr(l, "To add anything, you can also reply to this e-mail and keep [{key}] in the subject.", &[("key", key)])
    } else {
        tr(l, "Replies to this e-mail don't reach us: please answer on that page.", &[])
    };
    let text = format!("{greeting}\n\n{intro}\n\n{online}\n{}\n\n{how}\n\n— {project}\n", follow.portal);
    let p = |s: &str| format!("<p style=\"margin:0 0 12px\">{}</p>", esc(s));
    let html = conversation_html(
        &subject,
        &format!("{}{}{}{}", p(&greeting), p(&intro), button(follow.portal, &tr(l, "View your request", &[])), p(&how)),
        &format!("— {}", esc(project)),
    );
    (subject, text, html)
}

/// (subject, text, html) of a comment e-mailed to the requester; the comment as Markdown.
pub fn reply_mail(l: &str, key: &str, title: &str, markdown: &str, author: &str, project: &str, follow: &FollowUp) -> (String, String, String) {
    let subject = format!("Re: [{key}] {title}");
    let how = if follow.by_mail {
        tr(l, "Reply to this e-mail to answer; please keep [{key}] in the subject. Or answer online: {address}", &[("key", key), ("address", follow.portal)])
    } else {
        tr(l, "Answer online: {address} (replies to this e-mail don't reach us)", &[("address", follow.portal)])
    };
    let text = format!("{}\n\n— {author}, {project}\n\n{how}\n", markdown.trim());
    let mut options = comrak::Options::default();
    options.render.r#unsafe = false;
    options.extension.autolink = true;
    let body = comrak::markdown_to_html(markdown, &options);
    let footer = if follow.by_mail {
        esc(&tr(l, "Reply to this e-mail to answer; please keep [{key}] in the subject.", &[("key", key)]))
    } else {
        esc(&tr(l, "Replies to this e-mail don't reach us: please answer online.", &[]))
    };
    let html = conversation_html(
        &subject,
        &format!("{body}<p style=\"margin:16px 0 0\">— {}, {}</p>{}", esc(author), esc(project), button(follow.portal, &tr(l, "View your request", &[]))),
        &footer,
    );
    (subject, text, html)
}

/// E-mails a comment to the issue's requester; whether it was queued.
pub async fn mail_requester(state: &AppState, issue: Uuid, comment: Uuid, body: &Value, author: &str) -> anyhow::Result<bool> {
    let Some(r) = requester(state, issue).await? else { return Ok(false) };
    if !state.mailer.enabled() {
        return Ok(false);
    }
    let s = issues::summary(state, issues::load(state, issue).await.map_err(|e| anyhow::anyhow!("{e:#}"))?);
    let Some(p) = project(state, s.project_id).await? else { return Ok(false) };
    let markdown = crate::doc::to_markdown(body);
    // answers by e-mail reach the project only while it takes e-mail
    let settings = Settings::of(&p.settings);
    let reply_to = if settings.enabled { settings.address.or(r.reply_to.clone()).or_else(|| plus_address(state, &p.key)) } else { None };
    let follow = FollowUp { portal: &crate::portal::request_url(state, &s.key), by_mail: reply_to.is_some() };
    let l = crate::i18n::of_workspace(&state.db, p.workspace_id).await;
    let (subject, text, html) = reply_mail(l, &s.key, &s.title, &markdown, author, &p.name, &follow);
    let headers = requester_headers(state, issue, &p.name, reply_to, r.last_message_id.as_deref());
    let queued = crate::mail::queue_with(state, &Mail { to: r.email.clone(), subject, text, html, purpose: "requester" }, Some(&headers)).await?;
    if queued {
        sqlx::query("UPDATE comments SET to_requester_at = now() WHERE id = $1").bind(comment).execute(&state.db).await?;
    }
    Ok(queued)
}

// ---- worker ----------------------------------------------------------------------------------

/// Advisory lock: one replica at a time reads the mailbox.
const IMAP_LOCK: i64 = 0x7463_6d61_696c_0002;

/// Reads the mailbox every minute (one replica at a time) and trims the log.
pub fn spawn(state: &AppState) {
    let state = state.clone();
    let target = match state.config.mail_in.imap().map(imap::parse_url) {
        None => None,
        Some(Ok(t)) => Some(t),
        Some(Err(e)) => {
            tracing::error!("TASKCRAFT_IMAP_URL: {e}; not reading mail");
            None
        }
    };
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(60));
        loop {
            tick.tick().await;
            if state.shutting_down.load(std::sync::atomic::Ordering::Relaxed) {
                return;
            }
            if let Some(t) = &target
                && let Err(e) = poll_once(&state, t).await
            {
                tracing::warn!("reading the mailbox: {e:#}");
            }
        }
    });
}

/// Reads the mailbox once, if no other replica is at it; notes how it went.
pub async fn poll_once(state: &AppState, target: &imap::Target) -> anyhow::Result<usize> {
    let mut tx = state.db.begin().await?;
    let locked: bool = sqlx::query_scalar("SELECT pg_try_advisory_xact_lock($1)").bind(IMAP_LOCK).fetch_one(&mut *tx).await?;
    if !locked {
        return Ok(0);
    }
    let result = match tokio::time::timeout(std::time::Duration::from_secs(300), imap::poll(state, target)).await {
        Ok(r) => r,
        Err(_) => Err(anyhow::anyhow!("timed out")),
    };
    let error = result.as_ref().err().map(|e| format!("{e:#}").chars().take(500).collect::<String>());
    sqlx::query(
        "INSERT INTO mail_in_sources (source, checked_at, ok_at, error) VALUES ('imap', now(), CASE WHEN $1::text IS NULL THEN now() END, $1)
         ON CONFLICT (source) DO UPDATE SET checked_at = now(), ok_at = CASE WHEN $1::text IS NULL THEN now() ELSE mail_in_sources.ok_at END, error = $1",
    )
    .bind(&error)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn settings_and_mails() {
        assert_eq!(Settings::of(&json!({})), Settings::default());
        let s = Settings::of(&json!({"mailIn": {"enabled": true, "address": "support@acme.com", "senders": "anyone"}}));
        assert_eq!(s, Settings { enabled: true, address: Some("support@acme.com".into()), senders: Senders::Anyone });
        assert_eq!(Settings::of(&json!({"mailIn": {"senders": "nobody"}})), Settings::default(), "invalid settings mean off");

        let url = "https://tasks.acme.com/portal/requests/SUP-12";
        let by_mail = FollowUp { portal: url, by_mail: true };
        let (subject, text, html) = acknowledgement_mail("en", "SUP-12", "Printer <on> fire", Some("Jane & Co"), "Customer Support", &by_mail);
        assert_eq!(subject, "[SUP-12] Printer <on> fire");
        assert!(text.starts_with("Hi Jane & Co,\n\nthanks for your message. It's filed as SUP-12"), "{text}");
        assert!(text.contains("keep [SUP-12] in the subject") && text.ends_with("— Customer Support\n"));
        assert!(text.contains(&format!("answer online:\n{url}\n")), "{text}");
        assert!(html.contains("Hi Jane &amp; Co,") && html.contains("Printer &lt;on&gt; fire") && !html.contains("<on>"), "{html}");
        assert!(html.contains(&format!("href=\"{url}\"")), "{html}");
        let online = FollowUp { portal: url, by_mail: false };
        let (_, text, _) = acknowledgement_mail("en", "SUP-12", "Printer", None, "Customer Support", &online);
        assert!(text.starts_with("Hi,\n\n") && !text.contains("keep [SUP-12]") && text.contains("don't reach us"), "{text}");

        let (subject, text, html) = reply_mail("en", "SUP-12", "Printer", "We **restarted** it. <script>x</script>", "Mira", "Customer Support", &by_mail);
        assert_eq!(subject, "Re: [SUP-12] Printer");
        assert!(text.starts_with("We **restarted** it."), "{text}");
        assert!(text.contains("— Mira, Customer Support") && text.contains(url));
        assert!(html.contains("<strong>restarted</strong>") && !html.contains("<script>") && html.contains(url), "{html}");
        let (_, text, _) = reply_mail("en", "SUP-12", "Printer", "Done.", "Mira", "Customer Support", &online);
        assert!(text.contains(&format!("Answer online: {url}")) && !text.contains("Reply to this e-mail"), "{text}");
    }
}
