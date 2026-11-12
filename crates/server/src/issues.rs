//! Issues: how they are read (one shape for lists, boards and the API), created and changed.
//! Every change is written to the issue's history (`issue_events`), which also feeds the
//! activity stream, notifications, automation, webhooks and the flow reports.

use chrono::{DateTime, NaiveDate, Utc};
use serde::Serialize;
use serde_json::{Map, Value, json};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::access::{self, Role};
use crate::app::{AppState, new_id};
use crate::auth::User;
use crate::doc;
use crate::error::{ApiError, ApiResult};
use crate::events::{self, Event};
use crate::query::{normalize_label, parse_priority, priority_name, split_key};

/// Columns of an issue summary over `issues i` with the joins below.
pub const SELECT: &str = "SELECT i.id, i.number, i.project_id, p.key AS project_key, p.name AS project_name, i.title,
       it.id AS type_id, it.name AS type_name, it.icon AS type_icon, it.color AS type_color, it.level AS type_level,
       s.id AS status_id, s.name AS status_name, s.category AS status_category, s.color AS status_color,
       i.priority, i.assignee_id, a.name AS assignee_name, i.reporter_id, r.name AS reporter_name,
       i.parent_id, (pp.key || '-' || par.number) AS parent_key, par.title AS parent_title, pt.color AS parent_color,
       i.sprint_id, sp.name AS sprint_name, sp.state AS sprint_state, i.version_id, v.name AS version_name,
       i.component_id, c.name AS component_name,
       i.story_points, i.estimate_minutes, i.spent_minutes, i.start_date, i.due_date, i.rank, i.custom,
       i.resolved_at, i.status_changed_at, i.created_at, i.updated_at,
       COALESCE((SELECT array_agg(l.label ORDER BY l.label) FROM issue_labels l WHERE l.issue_id = i.id), '{}') AS labels,
       (SELECT count(*) FROM issues ch WHERE ch.parent_id = i.id AND ch.deleted_at IS NULL) AS child_count,
       (SELECT count(*) FROM issues ch JOIN statuses chs ON chs.id = ch.status_id WHERE ch.parent_id = i.id AND ch.deleted_at IS NULL AND chs.category = 'done') AS child_done,
       (SELECT count(*) FROM comments cm WHERE cm.issue_id = i.id) AS comment_count,
       EXISTS (SELECT 1 FROM issue_links bl JOIN issues bi ON bi.id = bl.source_id JOIN statuses bs ON bs.id = bi.status_id
                WHERE bl.target_id = i.id AND bl.kind = 'blocks' AND bs.category <> 'done' AND bi.deleted_at IS NULL) AS blocked
  FROM issues i
  JOIN projects p ON p.id = i.project_id
  JOIN statuses s ON s.id = i.status_id
  JOIN issue_types it ON it.id = i.type_id
  LEFT JOIN users a ON a.id = i.assignee_id
  LEFT JOIN users r ON r.id = i.reporter_id
  LEFT JOIN issues par ON par.id = i.parent_id
  LEFT JOIN projects pp ON pp.id = par.project_id
  LEFT JOIN issue_types pt ON pt.id = par.type_id
  LEFT JOIN sprints sp ON sp.id = i.sprint_id
  LEFT JOIN versions v ON v.id = i.version_id
  LEFT JOIN components c ON c.id = i.component_id";

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Row {
    pub id: Uuid,
    pub number: i32,
    pub project_id: Uuid,
    pub project_key: String,
    pub project_name: String,
    pub title: String,
    pub type_id: Uuid,
    pub type_name: String,
    pub type_icon: String,
    pub type_color: String,
    pub type_level: i16,
    pub status_id: Uuid,
    pub status_name: String,
    pub status_category: String,
    pub status_color: Option<String>,
    pub priority: i16,
    pub assignee_id: Option<Uuid>,
    pub assignee_name: Option<String>,
    pub reporter_id: Option<Uuid>,
    pub reporter_name: Option<String>,
    pub parent_id: Option<Uuid>,
    pub parent_key: Option<String>,
    pub parent_title: Option<String>,
    pub parent_color: Option<String>,
    pub sprint_id: Option<Uuid>,
    pub sprint_name: Option<String>,
    pub sprint_state: Option<String>,
    pub version_id: Option<Uuid>,
    pub version_name: Option<String>,
    pub component_id: Option<Uuid>,
    pub component_name: Option<String>,
    pub story_points: Option<f64>,
    pub estimate_minutes: Option<i32>,
    pub spent_minutes: i32,
    pub start_date: Option<NaiveDate>,
    pub due_date: Option<NaiveDate>,
    pub rank: f64,
    pub custom: Value,
    pub resolved_at: Option<DateTime<Utc>>,
    pub status_changed_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub labels: Vec<String>,
    pub child_count: i64,
    pub child_done: i64,
    pub comment_count: i64,
    pub blocked: bool,
}

impl Row {
    pub fn key(&self) -> String {
        format!("{}-{}", self.project_key, self.number)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Named {
    pub id: Uuid,
    pub name: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TypeRef {
    pub id: Uuid,
    pub name: String,
    pub icon: String,
    pub color: String,
    pub level: i16,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusRef {
    pub id: Uuid,
    pub name: String,
    pub category: String,
    pub color: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ParentRef {
    pub id: Uuid,
    pub key: String,
    pub title: String,
    pub color: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SprintRef {
    pub id: Uuid,
    pub name: String,
    pub state: String,
}

/// An issue as lists, boards and the API show it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Summary {
    pub id: Uuid,
    pub key: String,
    pub number: i32,
    pub project_id: Uuid,
    pub project_key: String,
    pub project_name: String,
    pub title: String,
    #[serde(rename = "type")]
    pub kind: TypeRef,
    pub status: StatusRef,
    pub priority: &'static str,
    pub assignee: Option<Named>,
    pub reporter: Option<Named>,
    pub parent: Option<ParentRef>,
    pub sprint: Option<SprintRef>,
    pub version: Option<Named>,
    pub component: Option<Named>,
    pub story_points: Option<f64>,
    pub estimate_minutes: Option<i32>,
    pub spent_minutes: i32,
    pub start_date: Option<NaiveDate>,
    pub due_date: Option<NaiveDate>,
    pub rank: f64,
    pub labels: Vec<String>,
    pub custom: Value,
    pub resolved_at: Option<DateTime<Utc>>,
    pub status_changed_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub child_count: i64,
    pub child_done: i64,
    pub comment_count: i64,
    pub blocked: bool,
    pub url: String,
}

pub fn summary(state: &AppState, r: Row) -> Summary {
    let key = r.key();
    Summary {
        url: state.config.issue_url(&key),
        key,
        id: r.id,
        number: r.number,
        project_id: r.project_id,
        project_key: r.project_key,
        project_name: r.project_name,
        title: r.title,
        kind: TypeRef { id: r.type_id, name: r.type_name, icon: r.type_icon, color: r.type_color, level: r.type_level },
        status: StatusRef { id: r.status_id, name: r.status_name, category: r.status_category, color: r.status_color },
        priority: priority_name(r.priority),
        assignee: r.assignee_id.map(|id| Named { id, name: r.assignee_name.unwrap_or_default() }),
        reporter: r.reporter_id.map(|id| Named { id, name: r.reporter_name.unwrap_or_default() }),
        parent: r.parent_id.map(|id| ParentRef { id, key: r.parent_key.unwrap_or_default(), title: r.parent_title.unwrap_or_default(), color: r.parent_color }),
        sprint: r.sprint_id.map(|id| SprintRef { id, name: r.sprint_name.unwrap_or_default(), state: r.sprint_state.unwrap_or_default() }),
        version: r.version_id.map(|id| Named { id, name: r.version_name.unwrap_or_default() }),
        component: r.component_id.map(|id| Named { id, name: r.component_name.unwrap_or_default() }),
        story_points: r.story_points,
        estimate_minutes: r.estimate_minutes,
        spent_minutes: r.spent_minutes,
        start_date: r.start_date,
        due_date: r.due_date,
        rank: r.rank,
        labels: r.labels,
        custom: r.custom,
        resolved_at: r.resolved_at,
        status_changed_at: r.status_changed_at,
        created_at: r.created_at,
        updated_at: r.updated_at,
        child_count: r.child_count,
        child_done: r.child_done,
        comment_count: r.comment_count,
        blocked: r.blocked,
    }
}

pub async fn load(state: &AppState, id: Uuid) -> ApiResult<Row> {
    sqlx::query_as::<_, Row>(sqlx::AssertSqlSafe(format!("{SELECT} WHERE i.id = $1 AND i.deleted_at IS NULL")))
        .bind(id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(ApiError::NotFound("issue"))
}

pub async fn load_many(state: &AppState, ids: &[Uuid]) -> ApiResult<Vec<Summary>> {
    let rows = sqlx::query_as::<_, Row>(sqlx::AssertSqlSafe(format!("{SELECT} WHERE i.id = ANY($1) AND i.deleted_at IS NULL ORDER BY i.rank")))
        .bind(ids)
        .fetch_all(&state.db)
        .await?;
    Ok(rows.into_iter().map(|r| summary(state, r)).collect())
}

/// Finds an issue by id or key (`WEB-12`, also under a project's former key) that `user` can
/// see, with the caller's role in its project. Deleted issues are found with `deleted`.
pub async fn resolve(state: &AppState, user: &User, id_or_key: &str, deleted: bool) -> ApiResult<(Uuid, Uuid, Role)> {
    let row: Option<(Uuid, Uuid)> = if let Ok(id) = id_or_key.parse::<Uuid>() {
        sqlx::query_as("SELECT i.id, i.project_id FROM issues i WHERE i.id = $1 AND i.workspace_id = $2 AND ($3 OR i.deleted_at IS NULL)")
            .bind(id)
            .bind(user.workspace_id)
            .bind(deleted)
            .fetch_optional(&state.db)
            .await?
    } else if let Some((k, n)) = split_key(id_or_key) {
        sqlx::query_as(
            "SELECT i.id, i.project_id FROM issues i JOIN projects p ON p.id = i.project_id
              WHERE p.workspace_id = $1 AND i.number = $3 AND ($4 OR i.deleted_at IS NULL)
                AND (upper(p.key) = $2 OR p.id IN (SELECT project_id FROM project_key_aliases WHERE workspace_id = $1 AND upper(key) = $2))",
        )
        .bind(user.workspace_id)
        .bind(k)
        .bind(n)
        .bind(deleted)
        .fetch_optional(&state.db)
        .await?
    } else {
        None
    };
    // a key the issue had before it moved to another project
    let row = match (row, split_key(id_or_key)) {
        (None, Some((k, n))) => {
            sqlx::query_as(
                "SELECT i.id, i.project_id FROM issue_key_aliases a JOIN issues i ON i.id = a.issue_id
              WHERE a.workspace_id = $1 AND upper(a.project_key) = $2 AND a.number = $3 AND ($4 OR i.deleted_at IS NULL)",
            )
            .bind(user.workspace_id)
            .bind(k)
            .bind(n)
            .bind(deleted)
            .fetch_optional(&state.db)
            .await?
        }
        (row, _) => row,
    };
    let (id, project) = row.ok_or(ApiError::NotFound("issue"))?;
    let role = access::role(state, user, project).await?;
    if role == Role::None {
        return Err(ApiError::NotFound("issue"));
    }
    Ok((id, project, role))
}

/// A change to one field, as the history keeps it.
#[derive(Debug, Clone)]
pub struct Change {
    pub field: &'static str,
    pub old: Value,
    pub new: Value,
}

/// Who made a change: a person, or an automation rule acting for the project.
#[derive(Debug, Clone)]
pub struct Actor {
    pub user: Option<Uuid>,
    pub workspace: Uuid,
    /// automation depth (rules triggering rules stop after a few rounds)
    pub depth: u8,
}

/// Depth of a quiet actor: imports change issues without notifications, webhooks, automation
/// or live events per issue.
pub const QUIET: u8 = 200;

impl Actor {
    pub fn of(u: &User) -> Self {
        Actor { user: Some(u.id), workspace: u.workspace_id, depth: 0 }
    }

    /// Changes in `u`'s name that tell nobody (imports).
    pub fn quiet(u: &User) -> Self {
        Actor { user: Some(u.id), workspace: u.workspace_id, depth: QUIET }
    }

    pub fn is_quiet(&self) -> bool {
        self.depth >= QUIET
    }
}

pub async fn record(tx: &mut Transaction<'_, Postgres>, issue: Uuid, project: Uuid, actor: Option<Uuid>, kind: &str, c: Option<&Change>) -> ApiResult<()> {
    sqlx::query("INSERT INTO issue_events (id, issue_id, project_id, actor_id, kind, field, old_value, new_value) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)")
        .bind(new_id())
        .bind(issue)
        .bind(project)
        .bind(actor)
        .bind(kind)
        .bind(c.map(|c| c.field))
        .bind(c.map(|c| c.old.clone()))
        .bind(c.map(|c| c.new.clone()))
        .execute(&mut **tx)
        .await?;
    Ok(())
}

pub async fn watch(tx: &mut Transaction<'_, Postgres>, issue: Uuid, user: Uuid) -> ApiResult<()> {
    sqlx::query("INSERT INTO issue_watchers (issue_id, user_id) VALUES ($1, $2) ON CONFLICT DO NOTHING").bind(issue).bind(user).execute(&mut **tx).await?;
    Ok(())
}

/// Rank for the bottom of a project's backlog.
pub async fn bottom_rank(tx: &mut Transaction<'_, Postgres>, project: Uuid) -> ApiResult<f64> {
    let max: Option<f64> = sqlx::query_scalar("SELECT max(rank) FROM issues WHERE project_id = $1").bind(project).fetch_one(&mut **tx).await?;
    Ok(max.unwrap_or(0.0) + 1024.0)
}

/// A rank between the issue `after` (above) and `before` (below); either may be missing.
/// Renumbers the project when the gap runs out.
pub async fn rank_between(tx: &mut Transaction<'_, Postgres>, project: Uuid, after: Option<Uuid>, before: Option<Uuid>, moving: Uuid) -> ApiResult<f64> {
    for _ in 0..2 {
        let rank_of = |id: Uuid| sqlx::query_scalar::<_, f64>("SELECT rank FROM issues WHERE id = $1 AND project_id = $2").bind(id).bind(project);
        let (lo, hi) = match (after, before) {
            (Some(a), _) => {
                let lo = rank_of(a).fetch_optional(&mut **tx).await?.ok_or_else(|| ApiError::bad("the issue to place after is in another project"))?;
                let hi: Option<f64> = sqlx::query_scalar("SELECT min(rank) FROM issues WHERE project_id = $1 AND rank > $2 AND id <> $3")
                    .bind(project)
                    .bind(lo)
                    .bind(moving)
                    .fetch_one(&mut **tx)
                    .await?;
                (Some(lo), hi)
            }
            (None, Some(b)) => {
                let hi = rank_of(b).fetch_optional(&mut **tx).await?.ok_or_else(|| ApiError::bad("the issue to place before is in another project"))?;
                let lo: Option<f64> = sqlx::query_scalar("SELECT max(rank) FROM issues WHERE project_id = $1 AND rank < $2 AND id <> $3")
                    .bind(project)
                    .bind(hi)
                    .bind(moving)
                    .fetch_one(&mut **tx)
                    .await?;
                (lo, Some(hi))
            }
            (None, None) => return bottom_rank(tx, project).await,
        };
        let r = match (lo, hi) {
            (Some(l), Some(h)) => (l + h) / 2.0,
            (Some(l), None) => l + 1024.0,
            (None, Some(h)) => h - 1024.0,
            (None, None) => 1024.0,
        };
        let tight = match (lo, hi) {
            (Some(l), Some(h)) => (h - l).abs() < 1e-6,
            _ => false,
        };
        if !tight {
            return Ok(r);
        }
        renumber(tx, project).await?;
    }
    bottom_rank(tx, project).await
}

/// Spreads a project's ranks out again (1024 apart, same order).
pub async fn renumber(tx: &mut Transaction<'_, Postgres>, project: Uuid) -> ApiResult<()> {
    sqlx::query(
        "UPDATE issues i SET rank = o.n * 1024.0 FROM (SELECT id, row_number() OVER (ORDER BY rank, created_at) AS n FROM issues WHERE project_id = $1) o
          WHERE i.id = o.id",
    )
    .bind(project)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// What a new issue starts with.
#[derive(Debug, Default)]
pub struct NewIssue {
    pub project: Uuid,
    pub title: String,
    pub description: Option<Value>,
    pub fields: Map<String, Value>,
}

async fn first_id(tx: &mut Transaction<'_, Postgres>, sql: &'static str, project: Uuid) -> ApiResult<Option<Uuid>> {
    Ok(sqlx::query_scalar(sql).bind(project).fetch_optional(&mut **tx).await?)
}

/// Creates an issue (the caller has checked the actor may). Further fields (`typeId`,
/// `statusId`, `assigneeId`, …) are applied like a change, so they are validated the same way.
pub async fn create(state: &AppState, actor: &Actor, mut n: NewIssue) -> ApiResult<Summary> {
    let title = clean_title(&n.title)?;
    let mut tx = state.db.begin().await?;
    let (seq, settings): (i32, Value) =
        sqlx::query_as("UPDATE projects SET issue_seq = issue_seq + 1 WHERE id = $1 AND archived_at IS NULL RETURNING issue_seq, settings")
            .bind(n.project)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(|| ApiError::bad("the project is archived"))?;
    let type_id = match n.fields.remove("typeId").and_then(|v| v.as_str().and_then(|s| s.parse::<Uuid>().ok())) {
        Some(t) => t,
        None => first_id(&mut tx, "SELECT id FROM issue_types WHERE project_id = $1 ORDER BY level = 0 DESC, position LIMIT 1", n.project)
            .await?
            .ok_or_else(|| ApiError::bad("the project has no issue types"))?,
    };
    let type_ok: Option<Value> = sqlx::query_scalar("SELECT template FROM issue_types WHERE id = $1 AND project_id = $2")
        .bind(type_id)
        .bind(n.project)
        .fetch_optional(&mut *tx)
        .await?
        .flatten()
        .map(Some)
        .unwrap_or(None);
    let exists: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM issue_types WHERE id = $1 AND project_id = $2)")
        .bind(type_id)
        .bind(n.project)
        .fetch_one(&mut *tx)
        .await?;
    if !exists {
        return Err(ApiError::bad("unknown issue type for this project"));
    }
    let status_id = first_id(&mut tx, "SELECT id FROM statuses WHERE project_id = $1 ORDER BY category <> 'todo', position LIMIT 1", n.project)
        .await?
        .ok_or_else(|| ApiError::bad("the project has no statuses"))?;
    let description = match n.description.take() {
        Some(d) if !d.is_null() => {
            doc::validate(&d).map_err(ApiError::BadRequest)?;
            Some(d)
        }
        _ => type_ok,
    };
    let text = description.as_ref().map(doc::text).unwrap_or_default();
    let rank = bottom_rank(&mut tx, n.project).await?;
    let id = new_id();
    let default_assignee = settings["defaultAssignee"].as_str().and_then(|s| s.parse::<Uuid>().ok());
    sqlx::query(
        "INSERT INTO issues (id, workspace_id, project_id, number, title, description, description_text, type_id, status_id, reporter_id, created_by, rank, assignee_id)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $10, $11, $12)",
    )
    .bind(id)
    .bind(actor.workspace)
    .bind(n.project)
    .bind(seq)
    .bind(&title)
    .bind(&description)
    .bind(&text)
    .bind(type_id)
    .bind(status_id)
    .bind(actor.user)
    .bind(rank)
    .bind(default_assignee)
    .execute(&mut *tx)
    .await?;
    if let Some(u) = actor.user {
        watch(&mut tx, id, u).await?;
    }
    // the remaining fields are set like any change, but without history entries of their own
    let changes = apply(state, &mut tx, actor, id, n.project, &n.fields, true).await?;
    let snapshot: (Uuid, String, String, Option<Uuid>, Option<String>, Option<f64>, Option<Uuid>) = sqlx::query_as(
        "SELECT s.id, s.name, s.category, i.sprint_id, sp.name, i.story_points, i.assignee_id FROM issues i JOIN statuses s ON s.id = i.status_id
           LEFT JOIN sprints sp ON sp.id = i.sprint_id WHERE i.id = $1",
    )
    .bind(id)
    .fetch_one(&mut *tx)
    .await?;
    let created = Change {
        field: "issue",
        old: Value::Null,
        new: json!({
            "status": {"id": snapshot.0, "name": snapshot.1, "category": snapshot.2},
            "sprint": snapshot.3.map(|id| json!({"id": id, "name": snapshot.4})),
            "points": snapshot.5,
            "assignee": snapshot.6,
        }),
    };
    record(&mut tx, id, n.project, actor.user, "created", Some(&created)).await?;
    tx.commit().await?;
    let row = load(state, id).await?;
    let s = summary(state, row);
    after_change(state, actor, &s, &changes, true).await;
    if let Some(d) = description.as_ref().filter(|_| !actor.is_quiet()) {
        crate::notify::mentions(state, actor, &s, d, None).await;
    }
    Ok(s)
}

fn clean_title(t: &str) -> ApiResult<String> {
    let t: String = t.split_whitespace().collect::<Vec<_>>().join(" ");
    if t.is_empty() {
        return Err(ApiError::bad("the title is empty"));
    }
    Ok(t.chars().take(500).collect())
}

fn opt_uuid(v: &Value, field: &str) -> ApiResult<Option<Uuid>> {
    match v {
        Value::Null => Ok(None),
        Value::String(s) if s.is_empty() => Ok(None),
        Value::String(s) => s.parse().map(Some).map_err(|_| ApiError::bad(format!("{field} must be an id"))),
        _ => Err(ApiError::bad(format!("{field} must be an id or null"))),
    }
}

fn opt_date(v: &Value, field: &str) -> ApiResult<Option<NaiveDate>> {
    match v {
        Value::Null => Ok(None),
        Value::String(s) if s.is_empty() => Ok(None),
        Value::String(s) => {
            NaiveDate::parse_from_str(s.get(..10).unwrap_or(s), "%Y-%m-%d").map(Some).map_err(|_| ApiError::bad(format!("{field} must be a date (YYYY-MM-DD)")))
        }
        _ => Err(ApiError::bad(format!("{field} must be a date or null"))),
    }
}

fn opt_f64(v: &Value, field: &str) -> ApiResult<Option<f64>> {
    match v {
        Value::Null => Ok(None),
        Value::Number(n) => {
            n.as_f64().filter(|n| n.is_finite() && *n >= 0.0 && *n < 1e6).map(Some).ok_or_else(|| ApiError::bad(format!("{field} is out of range")))
        }
        Value::String(s) if s.trim().is_empty() => Ok(None),
        Value::String(s) => {
            s.trim().parse::<f64>().ok().filter(|n| n.is_finite() && *n >= 0.0).map(Some).ok_or_else(|| ApiError::bad(format!("{field} must be a number")))
        }
        _ => Err(ApiError::bad(format!("{field} must be a number or null"))),
    }
}

/// `{"id": …, "name": …}` of a row, for the history.
async fn named(tx: &mut Transaction<'_, Postgres>, table: &str, id: Option<Uuid>) -> ApiResult<Value> {
    let Some(id) = id else { return Ok(Value::Null) };
    let name: Option<String> =
        sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT name FROM {table} WHERE id = $1"))).bind(id).fetch_optional(&mut **tx).await?;
    Ok(json!({"id": id, "name": name}))
}

/// Applies a change (`{"statusId": …, "assigneeId": …}`) inside a transaction and records
/// it (unless `quiet`, used while creating). Returns what actually changed.
pub async fn apply(
    state: &AppState,
    tx: &mut Transaction<'_, Postgres>,
    actor: &Actor,
    id: Uuid,
    project: Uuid,
    patch: &Map<String, Value>,
    quiet: bool,
) -> ApiResult<Vec<Change>> {
    #[derive(sqlx::FromRow)]
    struct Cur {
        title: String,
        type_id: Uuid,
        status_id: Uuid,
        priority: i16,
        assignee_id: Option<Uuid>,
        reporter_id: Option<Uuid>,
        parent_id: Option<Uuid>,
        sprint_id: Option<Uuid>,
        version_id: Option<Uuid>,
        component_id: Option<Uuid>,
        story_points: Option<f64>,
        estimate_minutes: Option<i32>,
        start_date: Option<NaiveDate>,
        due_date: Option<NaiveDate>,
        custom: Value,
        workspace_id: Uuid,
    }
    let cur: Cur = sqlx::query_as(
        "SELECT title, type_id, status_id, priority, assignee_id, reporter_id, parent_id, sprint_id, version_id, component_id, story_points,
                estimate_minutes, start_date, due_date, custom, workspace_id FROM issues WHERE id = $1 FOR UPDATE",
    )
    .bind(id)
    .fetch_one(&mut **tx)
    .await?;
    let mut changes = Vec::new();
    let mut sets: Vec<(&'static str, SqlVal)> = Vec::new();

    for (k, v) in patch {
        match k.as_str() {
            "title" => {
                let t = clean_title(v.as_str().ok_or_else(|| ApiError::bad("title must be text"))?)?;
                if t != cur.title {
                    changes.push(Change { field: "title", old: json!(cur.title), new: json!(t) });
                    sets.push(("title", SqlVal::Text(Some(t))));
                }
            }
            "description" | "descriptionMarkdown" => {
                let d = match (k.as_str(), v) {
                    (_, Value::Null) => None,
                    ("descriptionMarkdown", Value::String(md)) => Some(doc::from_markdown(md)),
                    ("description", Value::String(s)) => Some(doc::from_plain(s)),
                    (_, d) => {
                        doc::validate(d).map_err(ApiError::BadRequest)?;
                        Some(d.clone())
                    }
                };
                let text = d.as_ref().map(doc::text).unwrap_or_default();
                sqlx::query("UPDATE issues SET description = $2, description_text = $3 WHERE id = $1").bind(id).bind(&d).bind(&text).execute(&mut **tx).await?;
                changes.push(Change { field: "description", old: Value::Null, new: Value::Null });
            }
            "typeId" => {
                let t = opt_uuid(v, k)?.ok_or_else(|| ApiError::bad("an issue needs a type"))?;
                if t != cur.type_id {
                    let level: Option<i16> = sqlx::query_scalar("SELECT level FROM issue_types WHERE id = $1 AND project_id = $2")
                        .bind(t)
                        .bind(project)
                        .fetch_optional(&mut **tx)
                        .await?;
                    if level.is_none() {
                        return Err(ApiError::bad("unknown issue type for this project"));
                    }
                    changes.push(Change {
                        field: "type",
                        old: named(tx, "issue_types", Some(cur.type_id)).await?,
                        new: named(tx, "issue_types", Some(t)).await?,
                    });
                    sets.push(("type_id", SqlVal::Uuid(Some(t))));
                }
            }
            "statusId" => {
                let s = opt_uuid(v, k)?.ok_or_else(|| ApiError::bad("an issue needs a status"))?;
                if s != cur.status_id {
                    let row: Option<(String, String)> = sqlx::query_as("SELECT name, category FROM statuses WHERE id = $1 AND project_id = $2")
                        .bind(s)
                        .bind(project)
                        .fetch_optional(&mut **tx)
                        .await?;
                    let (name, category) = row.ok_or_else(|| ApiError::bad("unknown status for this project"))?;
                    check_transition(tx, project, cur.status_id, s, actor).await?;
                    let (old_name, old_cat): (String, String) =
                        sqlx::query_as("SELECT name, category FROM statuses WHERE id = $1").bind(cur.status_id).fetch_one(&mut **tx).await?;
                    changes.push(Change {
                        field: "status",
                        old: json!({"id": cur.status_id, "name": old_name, "category": old_cat}),
                        new: json!({"id": s, "name": name, "category": category}),
                    });
                    sets.push(("status_id", SqlVal::Uuid(Some(s))));
                    sqlx::query(
                        "UPDATE issues SET status_changed_at = now(),
                                resolved_at = CASE WHEN $2 = 'done' THEN COALESCE(resolved_at, now()) ELSE NULL END WHERE id = $1",
                    )
                    .bind(id)
                    .bind(&category)
                    .execute(&mut **tx)
                    .await?;
                }
            }
            "priority" => {
                let p = match v {
                    Value::Number(n) => n.as_i64().and_then(|n| i16::try_from(n).ok()).filter(|n| (0..=4).contains(n)),
                    Value::String(s) => parse_priority(s),
                    _ => None,
                }
                .ok_or_else(|| ApiError::bad("priority must be none, low, medium, high or urgent"))?;
                if p != cur.priority {
                    changes.push(Change { field: "priority", old: json!(priority_name(cur.priority)), new: json!(priority_name(p)) });
                    sets.push(("priority", SqlVal::Int(Some(p as i32))));
                }
            }
            "assigneeId" | "reporterId" => {
                let u = opt_uuid(v, k)?;
                let (old, col, field) =
                    if k == "assigneeId" { (cur.assignee_id, "assignee_id", "assignee") } else { (cur.reporter_id, "reporter_id", "reporter") };
                if u != old {
                    if let Some(u) = u {
                        let ok: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM users WHERE id = $1 AND workspace_id = $2 AND status = 'active')")
                            .bind(u)
                            .bind(cur.workspace_id)
                            .fetch_one(&mut **tx)
                            .await?;
                        if !ok || !access::can_view(state, u, project).await {
                            return Err(ApiError::bad(format!("that person can't be the {field}: they have no access to the project")));
                        }
                    }
                    changes.push(Change { field, old: named(tx, "users", old).await?, new: named(tx, "users", u).await? });
                    sets.push((col, SqlVal::Uuid(u)));
                }
            }
            "parentId" | "parentKey" => {
                let p = if k == "parentKey" {
                    match v.as_str().filter(|s| !s.is_empty()) {
                        None => None,
                        Some(key) => {
                            let (pk, pn) = split_key(key).ok_or_else(|| ApiError::bad("parentKey must be an issue key"))?;
                            Some(
                                sqlx::query_scalar::<_, Uuid>(
                                    "SELECT i.id FROM issues i JOIN projects p ON p.id = i.project_id WHERE p.workspace_id = $1 AND upper(p.key) = $2 AND i.number = $3 AND i.deleted_at IS NULL",
                                )
                                .bind(cur.workspace_id)
                                .bind(pk)
                                .bind(pn)
                                .fetch_optional(&mut **tx)
                                .await?
                                .ok_or(ApiError::NotFound("parent issue"))?,
                            )
                        }
                    }
                } else {
                    opt_uuid(v, k)?
                };
                if p != cur.parent_id {
                    if let Some(p) = p {
                        check_parent(state, tx, id, p, cur.workspace_id).await?;
                    }
                    let old = parent_ref(tx, cur.parent_id).await?;
                    let new = parent_ref(tx, p).await?;
                    changes.push(Change { field: "parent", old, new });
                    sets.push(("parent_id", SqlVal::Uuid(p)));
                }
            }
            "sprintId" => {
                let s = opt_uuid(v, k)?;
                if s != cur.sprint_id {
                    if let Some(s) = s {
                        let st: Option<String> = sqlx::query_scalar("SELECT state FROM sprints WHERE id = $1 AND project_id = $2")
                            .bind(s)
                            .bind(project)
                            .fetch_optional(&mut **tx)
                            .await?;
                        match st.as_deref() {
                            None => return Err(ApiError::bad("unknown sprint for this project")),
                            Some("closed") => return Err(ApiError::bad("the sprint is closed")),
                            _ => {}
                        }
                    }
                    changes.push(Change { field: "sprint", old: named(tx, "sprints", cur.sprint_id).await?, new: named(tx, "sprints", s).await? });
                    sets.push(("sprint_id", SqlVal::Uuid(s)));
                }
            }
            "versionId" | "componentId" => {
                let x = opt_uuid(v, k)?;
                let (old, table, col, field) = if k == "versionId" {
                    (cur.version_id, "versions", "version_id", "version")
                } else {
                    (cur.component_id, "components", "component_id", "component")
                };
                if x != old {
                    if let Some(x) = x {
                        let ok: bool =
                            sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT EXISTS (SELECT 1 FROM {table} WHERE id = $1 AND project_id = $2)")))
                                .bind(x)
                                .bind(project)
                                .fetch_one(&mut **tx)
                                .await?;
                        if !ok {
                            return Err(ApiError::bad(format!("unknown {field} for this project")));
                        }
                    }
                    changes.push(Change { field, old: named(tx, table, old).await?, new: named(tx, table, x).await? });
                    sets.push((col, SqlVal::Uuid(x)));
                }
            }
            "storyPoints" => {
                let p = opt_f64(v, k)?;
                if p != cur.story_points {
                    changes.push(Change { field: "points", old: json!(cur.story_points), new: json!(p) });
                    sets.push(("story_points", SqlVal::Float(p)));
                }
            }
            "estimateMinutes" => {
                let m = opt_f64(v, k)?.map(|m| m.round() as i32);
                if m != cur.estimate_minutes {
                    changes.push(Change { field: "estimate", old: json!(cur.estimate_minutes), new: json!(m) });
                    sets.push(("estimate_minutes", SqlVal::Int(m)));
                }
            }
            "startDate" | "dueDate" => {
                let d = opt_date(v, k)?;
                let (old, col, field) = if k == "startDate" { (cur.start_date, "start_date", "start") } else { (cur.due_date, "due_date", "due") };
                if d != old {
                    changes.push(Change { field, old: json!(old), new: json!(d) });
                    sets.push((col, SqlVal::Date(d)));
                }
            }
            "labels" => {
                let mut labels: Vec<String> = v
                    .as_array()
                    .ok_or_else(|| ApiError::bad("labels must be a list"))?
                    .iter()
                    .filter_map(|l| l.as_str())
                    .map(normalize_label)
                    .filter(|l| !l.is_empty())
                    .collect();
                labels.sort();
                labels.dedup();
                labels.truncate(30);
                let old: Vec<String> =
                    sqlx::query_scalar("SELECT label FROM issue_labels WHERE issue_id = $1 ORDER BY label").bind(id).fetch_all(&mut **tx).await?;
                if old != labels {
                    sqlx::query("DELETE FROM issue_labels WHERE issue_id = $1").bind(id).execute(&mut **tx).await?;
                    sqlx::query("INSERT INTO issue_labels (issue_id, label) SELECT $1, unnest($2::text[])").bind(id).bind(&labels).execute(&mut **tx).await?;
                    changes.push(Change { field: "labels", old: json!(old), new: json!(labels) });
                }
            }
            "custom" => {
                let obj = v.as_object().ok_or_else(|| ApiError::bad("custom must be an object of field id → value"))?;
                let mut merged = cur.custom.as_object().cloned().unwrap_or_default();
                for (fid, val) in obj {
                    let fid_uuid: Uuid = fid.parse().map_err(|_| ApiError::bad("custom field ids must be ids"))?;
                    let def: Option<(String, String, Value)> =
                        sqlx::query_as("SELECT name, kind, options FROM custom_fields WHERE id = $1 AND project_id = $2")
                            .bind(fid_uuid)
                            .bind(project)
                            .fetch_optional(&mut **tx)
                            .await?;
                    let (name, kind, options) = def.ok_or_else(|| ApiError::bad("unknown custom field for this project"))?;
                    let val = custom_value(&kind, &options, val, &name)?;
                    let old = merged.get(fid).cloned().unwrap_or(Value::Null);
                    if old != val {
                        changes.push(Change { field: "custom", old: json!({"field": name, "value": old}), new: json!({"field": name, "value": val}) });
                        if val.is_null() {
                            merged.remove(fid);
                        } else {
                            merged.insert(fid.clone(), val);
                        }
                    }
                }
                sqlx::query("UPDATE issues SET custom = $2 WHERE id = $1").bind(id).bind(Value::Object(merged)).execute(&mut **tx).await?;
            }
            // read-only or handled elsewhere: ignore what clients send back unchanged
            "id" | "key" | "number" | "projectId" | "projectKey" | "url" | "rank" | "createdAt" | "updatedAt" => {}
            other => return Err(ApiError::bad(format!("unknown field {other}"))),
        }
    }
    for (col, val) in &sets {
        let sql = sqlx::AssertSqlSafe(format!("UPDATE issues SET {col} = $2 WHERE id = $1"));
        let q = sqlx::query(sql).bind(id);
        let q = match val {
            SqlVal::Text(t) => q.bind(t.clone()),
            SqlVal::Uuid(u) => q.bind(*u),
            SqlVal::Int(n) => q.bind(*n),
            SqlVal::Float(f) => q.bind(*f),
            SqlVal::Date(d) => q.bind(*d),
        };
        q.execute(&mut **tx).await?;
    }
    if !changes.is_empty() {
        sqlx::query("UPDATE issues SET updated_at = now() WHERE id = $1").bind(id).execute(&mut **tx).await?;
        if !quiet {
            for c in &changes {
                record(tx, id, project, actor.user, "field", Some(c)).await?;
            }
        }
        // a new assignee follows the issue
        if let Some(c) = changes.iter().find(|c| c.field == "assignee")
            && let Some(u) = c.new["id"].as_str().and_then(|s| s.parse().ok())
        {
            watch(tx, id, u).await?;
        }
    }
    Ok(changes)
}

enum SqlVal {
    Text(Option<String>),
    Uuid(Option<Uuid>),
    Int(Option<i32>),
    Float(Option<f64>),
    Date(Option<NaiveDate>),
}

async fn parent_ref(tx: &mut Transaction<'_, Postgres>, id: Option<Uuid>) -> ApiResult<Value> {
    let Some(id) = id else { return Ok(Value::Null) };
    let r: Option<(String, i32, String)> =
        sqlx::query_as("SELECT p.key, i.number, i.title FROM issues i JOIN projects p ON p.id = i.project_id WHERE i.id = $1")
            .bind(id)
            .fetch_optional(&mut **tx)
            .await?;
    Ok(r.map(|(k, n, t)| json!({"id": id, "key": format!("{k}-{n}"), "name": t})).unwrap_or(Value::Null))
}

/// A parent must be in the workspace, visible, not a sub-task, and not the issue or below it.
async fn check_parent(_state: &AppState, tx: &mut Transaction<'_, Postgres>, id: Uuid, parent: Uuid, workspace: Uuid) -> ApiResult<()> {
    if parent == id {
        return Err(ApiError::bad("an issue can't be its own parent"));
    }
    let row: Option<(i16, Uuid)> = sqlx::query_as(
        "SELECT it.level, i.project_id FROM issues i JOIN issue_types it ON it.id = i.type_id WHERE i.id = $1 AND i.workspace_id = $2 AND i.deleted_at IS NULL",
    )
    .bind(parent)
    .bind(workspace)
    .fetch_optional(&mut **tx)
    .await?;
    let (level, _) = row.ok_or(ApiError::NotFound("parent issue"))?;
    if level < 0 {
        return Err(ApiError::bad("sub-tasks can't have children"));
    }
    let cycle: bool = sqlx::query_scalar(
        "WITH RECURSIVE up(id, parent_id, depth) AS (SELECT id, parent_id, 0 FROM issues WHERE id = $1
           UNION ALL SELECT i.id, i.parent_id, up.depth + 1 FROM issues i JOIN up ON i.id = up.parent_id WHERE up.depth < 50)
         SELECT EXISTS (SELECT 1 FROM up WHERE id = $2)",
    )
    .bind(parent)
    .bind(id)
    .fetch_one(&mut **tx)
    .await?;
    if cycle {
        return Err(ApiError::bad("that would make the issue its own ancestor"));
    }
    Ok(())
}

/// Restricted workflows only allow the configured moves (project admins may always move).
async fn check_transition(tx: &mut Transaction<'_, Postgres>, project: Uuid, from: Uuid, to: Uuid, actor: &Actor) -> ApiResult<()> {
    let restricted: bool =
        sqlx::query_scalar("SELECT settings->>'workflow' = 'restricted' FROM projects WHERE id = $1").bind(project).fetch_one(&mut **tx).await.unwrap_or(false);
    if !restricted || actor.user.is_none() {
        return Ok(());
    }
    let allowed: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM transitions WHERE from_status_id = $1 AND to_status_id = $2)")
        .bind(from)
        .bind(to)
        .fetch_one(&mut **tx)
        .await?;
    if allowed {
        return Ok(());
    }
    let names: (String, String) = sqlx::query_as("SELECT (SELECT name FROM statuses WHERE id = $1), (SELECT name FROM statuses WHERE id = $2)")
        .bind(from)
        .bind(to)
        .fetch_one(&mut **tx)
        .await?;
    Err(ApiError::BadRequest(format!("the workflow doesn't allow moving from {} to {}", names.0, names.1)))
}

/// Validates a custom field value for its kind (null clears it).
fn custom_value(kind: &str, options: &Value, v: &Value, name: &str) -> ApiResult<Value> {
    if v.is_null() || v.as_str() == Some("") {
        return Ok(Value::Null);
    }
    let option_ids: Vec<&str> = options.as_array().into_iter().flatten().filter_map(|o| o["id"].as_str()).collect();
    let err = |what: &str| ApiError::bad(format!("{name} takes {what}"));
    Ok(match kind {
        "text" => json!(v.as_str().ok_or_else(|| err("text"))?.chars().take(2000).collect::<String>()),
        "url" => {
            let s = v.as_str().ok_or_else(|| err("a web address"))?.trim();
            if !(s.starts_with("https://") || s.starts_with("http://")) {
                return Err(err("a web address (https://…)"));
            }
            json!(s)
        }
        "number" => json!(v.as_f64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok())).filter(|n| n.is_finite()).ok_or_else(|| err("a number"))?),
        "date" => json!(opt_date(v, name)?),
        "checkbox" => json!(v.as_bool().ok_or_else(|| err("true or false"))?),
        "user" => json!(v.as_str().and_then(|s| s.parse::<Uuid>().ok()).ok_or_else(|| err("a person"))?.to_string()),
        "select" => {
            let s = v.as_str().ok_or_else(|| err("one of its options"))?;
            if !option_ids.contains(&s) {
                return Err(err("one of its options"));
            }
            json!(s)
        }
        "multiselect" => {
            let list: Vec<&str> = v.as_array().ok_or_else(|| err("a list of its options"))?.iter().filter_map(|x| x.as_str()).collect();
            if list.iter().any(|s| !option_ids.contains(s)) {
                return Err(err("a list of its options"));
            }
            if list.is_empty() {
                return Ok(Value::Null);
            }
            json!(list)
        }
        _ => return Err(err("nothing (unknown field kind)")),
    })
}

/// Updates an issue on behalf of `actor` (permission already checked) and runs the follow-ups.
pub async fn update(state: &AppState, actor: &Actor, id: Uuid, patch: &Map<String, Value>) -> ApiResult<(Summary, Vec<Change>)> {
    let project: Uuid = sqlx::query_scalar("SELECT project_id FROM issues WHERE id = $1 AND deleted_at IS NULL")
        .bind(id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(ApiError::NotFound("issue"))?;
    let mut tx = state.db.begin().await?;
    let changes = apply(state, &mut tx, actor, id, project, patch, false).await?;
    tx.commit().await?;
    let s = summary(state, load(state, id).await?);
    if !changes.is_empty() {
        after_change(state, actor, &s, &changes, false).await;
        if !actor.is_quiet() && (patch.contains_key("description") || patch.contains_key("descriptionMarkdown")) {
            let d: Option<Value> = sqlx::query_scalar("SELECT description FROM issues WHERE id = $1").bind(id).fetch_one(&state.db).await?;
            if let Some(d) = d {
                crate::notify::mentions(state, actor, &s, &d, None).await;
            }
        }
    }
    Ok((s, changes))
}

/// Notifications, live update, webhooks and automation after an issue changed.
pub async fn after_change(state: &AppState, actor: &Actor, s: &Summary, changes: &[Change], created: bool) {
    if actor.is_quiet() {
        return;
    }
    crate::notify::changes(state, actor, s, changes, created).await;
    let mut e = Event::new(actor.workspace, "issue").project(s.project_id).issue(s.id, &s.key);
    e.actor_id = actor.user;
    events::publish(state, e).await;
    let kind = if created { "issue.created" } else { "issue.updated" };
    let fields: Vec<&str> = changes.iter().map(|c| c.field).collect();
    crate::webhooks::enqueue(state, actor.workspace, Some(s.project_id), kind, json!({"issue": s, "changed": fields})).await;
    crate::automation::run(state, actor, s, changes, created).await;
}

/// Moves an issue on a board or in a backlog: optionally a new status and sprint, and a place
/// between two neighbours.
pub async fn move_issue(
    state: &AppState,
    actor: &Actor,
    id: Uuid,
    status: Option<Uuid>,
    sprint: Option<Option<Uuid>>,
    after: Option<Uuid>,
    before: Option<Uuid>,
) -> ApiResult<Summary> {
    let project: Uuid = sqlx::query_scalar("SELECT project_id FROM issues WHERE id = $1 AND deleted_at IS NULL")
        .bind(id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(ApiError::NotFound("issue"))?;
    let mut patch = Map::new();
    if let Some(s) = status {
        patch.insert("statusId".into(), json!(s));
    }
    if let Some(sp) = sprint {
        patch.insert("sprintId".into(), json!(sp));
    }
    let mut tx = state.db.begin().await?;
    let changes = apply(state, &mut tx, actor, id, project, &patch, false).await?;
    if after.is_some() || before.is_some() {
        let r = rank_between(&mut tx, project, after.filter(|a| *a != id), before.filter(|b| *b != id), id).await?;
        sqlx::query("UPDATE issues SET rank = $2 WHERE id = $1").bind(id).bind(r).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    let s = summary(state, load(state, id).await?);
    if changes.is_empty() {
        // only the order changed: tell boards, nobody else
        let mut e = Event::new(actor.workspace, "issue").project(project).issue(id, &s.key);
        e.actor_id = actor.user;
        events::publish(state, e).await;
    } else {
        after_change(state, actor, &s, &changes, false).await;
    }
    Ok(s)
}
