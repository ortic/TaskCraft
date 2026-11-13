//! Personal data and how long things are kept.
//!
//! - [`export`]: everything TaskCraft holds about one person, as JSON (their own copy from their
//!   settings, or one an admin makes for them)
//! - [`purge`]: the workspace's retention rules (`settings.retention`): issues in the trash, the
//!   audit log, e-mail logs (sent e-mails and received ones) and read notifications older than so
//!   many days go away. Without a rule things are kept (e-mail logs: 30 and 90 days, as before).

use std::sync::Mutex;

use object_store::ObjectStoreExt;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use uuid::Uuid;

use crate::app::AppState;
use crate::error::{ApiError, ApiResult};

/// The retention settings a workspace may have, in days.
pub const RETENTION: [&str; 4] = ["trashDays", "auditDays", "mailDays", "notificationDays"];

/// Checks `settings.retention`: each a number of days (1–3650) or null.
pub fn check_retention(v: &Value) -> ApiResult<Value> {
    let o = v.as_object().ok_or_else(|| ApiError::bad("retention must be an object"))?;
    let mut out = serde_json::Map::new();
    for (k, val) in o {
        if !RETENTION.contains(&k.as_str()) {
            return Err(ApiError::bad(format!("unknown retention rule {k}")));
        }
        match val {
            Value::Null => {}
            v => {
                let days = v.as_i64().filter(|d| (1..=3650).contains(d)).ok_or_else(|| ApiError::bad(format!("{k} is a number of days, 1 to 3650")))?;
                out.insert(k.clone(), json!(days));
            }
        }
    }
    Ok(Value::Object(out))
}

/// Applies the retention rules (every replica may; at most once an hour each).
pub async fn purge(state: &AppState) -> anyhow::Result<()> {
    static LAST: Mutex<Option<Instant>> = Mutex::new(None);
    {
        let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
        if last.is_some_and(|t| t.elapsed() < Duration::from_secs(3600)) {
            return Ok(());
        }
        *last = Some(Instant::now());
    }
    purge_now(state).await
}

/// [`purge`] right away (and for tests).
pub async fn purge_now(state: &AppState) -> anyhow::Result<()> {
    let rules: Vec<(Uuid, Value)> = sqlx::query_as("SELECT id, COALESCE(settings->'retention', '{}') FROM workspaces").fetch_all(&state.db).await?;
    for (workspace, r) in &rules {
        let days = |k: &str| r[k].as_i64().map(|d| d as i32);
        // issues in the trash, with their files
        if let Some(d) = days("trashDays") {
            let keys: Vec<String> = sqlx::query_scalar(
                "SELECT a.storage_key FROM attachments a JOIN issues i ON i.id = a.issue_id
                  WHERE i.workspace_id = $1 AND i.deleted_at < now() - make_interval(days => $2)",
            )
            .bind(workspace)
            .bind(d)
            .fetch_all(&state.db)
            .await?;
            let gone = sqlx::query("DELETE FROM issues WHERE workspace_id = $1 AND deleted_at < now() - make_interval(days => $2)")
                .bind(workspace)
                .bind(d)
                .execute(&state.db)
                .await?
                .rows_affected();
            for k in keys {
                let _ = state.blobs.delete(&object_store::path::Path::from(k)).await;
            }
            if gone > 0 {
                tracing::info!(workspace = %workspace, issues = gone, "emptied the trash");
            }
        }
        if let Some(d) = days("auditDays") {
            sqlx::query("DELETE FROM audit_log WHERE workspace_id = $1 AND created_at < now() - make_interval(days => $2)")
                .bind(workspace)
                .bind(d)
                .execute(&state.db)
                .await?;
        }
        if let Some(d) = days("notificationDays") {
            sqlx::query("DELETE FROM notifications n USING users u WHERE u.id = n.user_id AND u.workspace_id = $1 AND n.read_at IS NOT NULL AND n.created_at < now() - make_interval(days => $2)")
                .bind(workspace)
                .bind(d)
                .execute(&state.db)
                .await?;
        }
    }
    // e-mail logs aren't kept per workspace: the shortest rule wins (else 30 days for sent
    // e-mails and 90 for received ones)
    let mail: Option<i32> = rules.iter().filter_map(|(_, r)| r["mailDays"].as_i64()).min().map(|d| d as i32);
    sqlx::query("DELETE FROM mail_outbox WHERE created_at < now() - make_interval(days => $1) AND (sent_at IS NOT NULL OR attempts >= $2)")
        .bind(mail.unwrap_or(30))
        .bind(crate::mail::MAX_ATTEMPTS)
        .execute(&state.db)
        .await?;
    sqlx::query("DELETE FROM mail_in WHERE received_at < now() - make_interval(days => $1)").bind(mail.unwrap_or(90)).execute(&state.db).await?;
    Ok(())
}

/// Everything about one person: profile, access, their work and what they wrote, sign-in
/// devices and tokens (never secrets), notifications and their actions in the audit log. Issue
/// titles only of issues they still see.
pub async fn export(state: &AppState, user: Uuid) -> ApiResult<Value> {
    let db = &state.db;
    #[derive(sqlx::FromRow)]
    struct Profile {
        id: Uuid,
        workspace_id: Uuid,
        email: Option<String>,
        name: String,
        title: Option<String>,
        role: String,
        status: String,
        locale: Option<String>,
        timezone: Option<String>,
        preferences: Value,
        created_at: chrono::DateTime<chrono::Utc>,
        last_login_at: Option<chrono::DateTime<chrono::Utc>>,
    }
    let p: Profile = sqlx::query_as(
        "SELECT id, workspace_id, email, name, title, role, status, locale, timezone, preferences, created_at, last_login_at FROM users WHERE id = $1",
    )
    .bind(user)
    .fetch_optional(db)
    .await?
    .ok_or(ApiError::NotFound("person"))?;
    let mut prefs = p.preferences.clone();
    if let Some(o) = prefs.as_object_mut() {
        o.remove("wikicraftToken");
    }
    // keys of the issues they see, to name the rest by key only
    let visible = |ids: Vec<Uuid>| async move { crate::access::visible_issues(state, user, &ids).await.unwrap_or_default() };
    let rows = |sql: &'static str| sqlx::query_as::<_, (Value,)>(sql).bind(user);
    let list = |v: Vec<(Value,)>| Value::Array(v.into_iter().map(|r| r.0).collect());

    let issues: Vec<(Uuid, String, String, String, bool, bool, bool, chrono::DateTime<chrono::Utc>)> = sqlx::query_as(
        "SELECT i.id, p.key || '-' || i.number, i.title, s.name, COALESCE(i.reporter_id = $1, false), COALESCE(i.created_by = $1, false),
                COALESCE(i.assignee_id = $1, false), i.created_at
           FROM issues i JOIN projects p ON p.id = i.project_id JOIN statuses s ON s.id = i.status_id
          WHERE (i.reporter_id = $1 OR i.created_by = $1 OR i.assignee_id = $1) AND i.deleted_at IS NULL ORDER BY i.created_at",
    )
    .bind(user)
    .fetch_all(db)
    .await?;
    let seen = visible(issues.iter().map(|i| i.0).collect()).await;
    let issues: Vec<Value> = issues
        .into_iter()
        .map(|(id, key, title, status, reported, created, assigned, at)| {
            json!({"key": key, "title": seen.contains(&id).then_some(title), "status": status, "reporter": reported, "creator": created, "assignee": assigned, "createdAt": at})
        })
        .collect();

    let comments = rows(
        "SELECT json_build_object('issue', p.key || '-' || i.number, 'createdAt', c.created_at, 'updatedAt', c.updated_at, 'text', c.body_text, 'visibility', c.visibility)
           FROM comments c JOIN issues i ON i.id = c.issue_id JOIN projects p ON p.id = i.project_id WHERE c.author_id = $1 ORDER BY c.created_at",
    )
    .fetch_all(db)
    .await?;
    let worklogs = rows(
        "SELECT json_build_object('issue', p.key || '-' || i.number, 'date', w.work_date, 'minutes', w.minutes, 'note', w.note, 'billable', w.billable)
           FROM worklogs w JOIN issues i ON i.id = w.issue_id JOIN projects p ON p.id = i.project_id WHERE w.user_id = $1 ORDER BY w.work_date",
    )
    .fetch_all(db)
    .await?;
    let attachments = rows(
        "SELECT json_build_object('issue', p.key || '-' || i.number, 'name', a.name, 'size', a.size, 'uploadedAt', a.created_at)
           FROM attachments a JOIN issues i ON i.id = a.issue_id JOIN projects p ON p.id = i.project_id WHERE a.uploaded_by = $1 ORDER BY a.created_at",
    )
    .fetch_all(db)
    .await?;
    let watching = rows(
        "SELECT to_json(p.key || '-' || i.number) FROM issue_watchers w JOIN issues i ON i.id = w.issue_id JOIN projects p ON p.id = i.project_id WHERE w.user_id = $1",
    )
    .fetch_all(db)
    .await?;
    let votes =
        rows("SELECT to_json(p.key || '-' || i.number) FROM issue_votes v JOIN issues i ON i.id = v.issue_id JOIN projects p ON p.id = i.project_id WHERE v.user_id = $1")
            .fetch_all(db)
            .await?;
    let teams =
        rows("SELECT to_json(t.name) FROM team_members m JOIN teams t ON t.id = m.team_id WHERE m.user_id = $1 ORDER BY lower(t.name)").fetch_all(db).await?;
    let access = rows(
        "SELECT json_build_object('project', p.key, 'role', m.role) FROM project_members m JOIN projects p ON p.id = m.project_id
          WHERE m.subject_type = 'user' AND m.subject_id = $1 ORDER BY p.key",
    )
    .fetch_all(db)
    .await?;
    let filters = rows(
        "SELECT json_build_object('name', v.name, 'query', v.query, 'shared', v.shared, 'subscribed', EXISTS (SELECT 1 FROM view_subscriptions s WHERE s.view_id = v.id AND s.user_id = $1))
           FROM views v WHERE v.owner_id = $1 ORDER BY lower(v.name)",
    )
    .fetch_all(db)
    .await?;
    let notifications = rows(
        "SELECT json_build_object('kind', n.kind, 'issue', p.key || '-' || i.number, 'createdAt', n.created_at, 'readAt', n.read_at)
           FROM notifications n LEFT JOIN issues i ON i.id = n.issue_id LEFT JOIN projects p ON p.id = i.project_id WHERE n.user_id = $1 ORDER BY n.created_at",
    )
    .fetch_all(db)
    .await?;
    let sessions = rows(
        "SELECT json_build_object('createdAt', created_at, 'lastSeenAt', last_seen_at, 'expiresAt', expires_at, 'userAgent', user_agent, 'ip', ip)
           FROM sessions WHERE user_id = $1 ORDER BY created_at",
    )
    .fetch_all(db)
    .await?;
    let tokens = rows(
        "SELECT json_build_object('name', name, 'scope', scope, 'createdAt', created_at, 'expiresAt', expires_at, 'lastUsedAt', last_used_at)
           FROM api_tokens WHERE user_id = $1 ORDER BY created_at",
    )
    .fetch_all(db)
    .await?;
    let passkeys =
        rows("SELECT json_build_object('name', name, 'createdAt', created_at) FROM passkeys WHERE user_id = $1 ORDER BY created_at").fetch_all(db).await?;
    let identities = rows(
        "SELECT json_build_object('provider', provider_id, 'email', email, 'createdAt', created_at) FROM identities WHERE user_id = $1 ORDER BY created_at",
    )
    .fetch_all(db)
    .await?;
    let audit = rows("SELECT json_build_object('action', action, 'target', target, 'at', created_at) FROM audit_log WHERE actor_id = $1 ORDER BY created_at")
        .fetch_all(db)
        .await?;
    Ok(json!({
        "exportedAt": chrono::Utc::now(),
        "profile": {
            "id": p.id, "workspaceId": p.workspace_id, "name": p.name, "email": p.email, "title": p.title, "role": p.role, "status": p.status,
            "language": p.locale, "timeZone": p.timezone, "preferences": prefs, "createdAt": p.created_at, "lastSignInAt": p.last_login_at,
        },
        "teams": list(teams),
        "projectAccess": list(access),
        "issues": issues,
        "comments": list(comments),
        "timeLogs": list(worklogs),
        "attachments": list(attachments),
        "watching": list(watching),
        "votes": list(votes),
        "savedFilters": list(filters),
        "notifications": list(notifications),
        "sessions": list(sessions),
        "apiTokens": list(tokens),
        "passkeys": list(passkeys),
        "signInIdentities": list(identities),
        "auditLog": list(audit),
    }))
}
