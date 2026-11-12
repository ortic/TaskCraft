//! TQL → SQL. The parsed query becomes a WHERE clause over `issues i` (joined with
//! `projects p`, `statuses s`, `issue_types it`) and an ORDER BY; every value is a bind
//! parameter. Leaves are wrapped in COALESCE(…, false) so `not` behaves on empty fields.
//! The caller's visible projects are always ANDed in.

use std::collections::HashMap;

use chrono::{DateTime, Datelike, Duration, NaiveDate, TimeZone, Utc};
use serde_json::Value as Json;
use sqlx::{Postgres, QueryBuilder};
use taskcraft_tql::{Expr, Op, Order, Query, Value};
use uuid::Uuid;

use crate::app::AppState;
use crate::auth::User;
use crate::error::{ApiError, ApiResult};

/// A custom field known to the query.
#[derive(Debug, Clone)]
pub struct FieldDef {
    pub ids: Vec<String>,
    pub kind: String,
    /// select options: label (lower case) → option ids
    pub options: HashMap<String, Vec<String>>,
}

/// What a query is compiled against.
pub struct Ctx {
    pub user: Uuid,
    pub workspace: Uuid,
    pub visible: Vec<Uuid>,
    pub now: DateTime<Utc>,
    pub custom: HashMap<String, FieldDef>,
}

pub const PRIORITIES: [&str; 5] = ["none", "low", "medium", "high", "urgent"];

pub fn priority_name(p: i16) -> &'static str {
    PRIORITIES.get(p.clamp(0, 4) as usize).copied().unwrap_or("medium")
}

pub fn parse_priority(s: &str) -> Option<i16> {
    let l = s.trim().to_ascii_lowercase();
    if let Ok(n) = l.parse::<i16>()
        && (0..=4).contains(&n)
    {
        return Some(n);
    }
    Some(match l.as_str() {
        "none" | "no priority" => 0,
        "low" | "lowest" | "minor" => 1,
        "medium" | "normal" => 2,
        "high" | "major" => 3,
        "urgent" | "highest" | "critical" | "blocker" => 4,
        _ => return None,
    })
}

pub fn parse_category(s: &str) -> Option<&'static str> {
    Some(match s.trim().to_ascii_lowercase().replace(['-', '_'], " ").as_str() {
        "todo" | "to do" | "open" | "new" | "backlog" => "todo",
        "in progress" | "inprogress" | "doing" | "started" | "active" => "in_progress",
        "done" | "closed" | "resolved" | "complete" | "completed" => "done",
        _ => return None,
    })
}

/// `WEB-12` → ("WEB", 12)
pub fn split_key(s: &str) -> Option<(String, i32)> {
    let (k, n) = s.trim().rsplit_once('-')?;
    let n: i32 = n.parse().ok().filter(|n| *n > 0)?;
    (!k.is_empty() && k.len() <= 20 && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')).then(|| (k.to_ascii_uppercase(), n))
}

/// Loads the custom fields a query names (by name, in the caller's visible projects).
pub async fn context(state: &AppState, user: &User, q: &Query) -> ApiResult<Ctx> {
    let visible = crate::access::visible_projects(state, user).await?;
    context_with(state, user.workspace_id, user.id, visible, q).await
}

/// Like [`context`] for a given set of projects (automation rules: their own project).
pub async fn context_with(state: &AppState, workspace: Uuid, user: Uuid, visible: Vec<Uuid>, q: &Query) -> ApiResult<Ctx> {
    let mut custom = HashMap::new();
    let unknown: Vec<String> = taskcraft_tql::fields(q).into_iter().filter(|f| !is_builtin(f)).collect();
    if !unknown.is_empty() {
        let rows: Vec<(Uuid, String, String, Json)> =
            sqlx::query_as("SELECT id, lower(name), kind, options FROM custom_fields WHERE project_id = ANY($1) AND lower(name) = ANY($2)")
                .bind(&visible)
                .bind(&unknown)
                .fetch_all(&state.db)
                .await?;
        for (id, name, kind, options) in rows {
            let def = custom.entry(name).or_insert_with(|| FieldDef { ids: vec![], kind: kind.clone(), options: HashMap::new() });
            def.ids.push(id.to_string());
            for o in options.as_array().into_iter().flatten() {
                if let (Some(oid), Some(label)) = (o["id"].as_str(), o["label"].as_str()) {
                    def.options.entry(label.to_lowercase()).or_default().push(oid.to_string());
                }
            }
        }
    }
    Ok(Ctx { user, workspace, visible, now: Utc::now(), custom })
}

const BUILTIN: &[&str] = &[
    "project",
    "key",
    "issue",
    "id",
    "title",
    "summary",
    "description",
    "text",
    "comment",
    "comments",
    "type",
    "issuetype",
    "status",
    "category",
    "statuscategory",
    "priority",
    "assignee",
    "reporter",
    "creator",
    "labels",
    "label",
    "sprint",
    "version",
    "fixversion",
    "release",
    "component",
    "parent",
    "epic",
    "points",
    "storypoints",
    "estimate",
    "remaining",
    "spent",
    "created",
    "updated",
    "resolved",
    "resolution",
    "security",
    "due",
    "duedate",
    "start",
    "startdate",
    "watcher",
    "watchers",
    "blocked",
    "rank",
];

fn is_builtin(f: &str) -> bool {
    BUILTIN.contains(&f)
}

/// The field names TQL knows (for autocompletion).
pub fn builtin_fields() -> &'static [&'static str] {
    BUILTIN
}

type Qb = QueryBuilder<Postgres>;

fn bad<T>(msg: impl Into<String>) -> ApiResult<T> {
    Err(ApiError::BadRequest(msg.into()))
}

/// Appends `AND <filter>` for `q` (visibility included) to a query over `issues i`.
pub fn push_where(qb: &mut Qb, ctx: &Ctx, q: &Query) -> ApiResult<()> {
    qb.push(" i.deleted_at IS NULL AND i.project_id = ANY(");
    qb.push_bind(ctx.visible.clone());
    qb.push(")");
    // private issues: for people (automation and schedules act for the project and see them all)
    if !ctx.user.is_nil() {
        qb.push(" AND (i.security_level_id IS NULL OR i.reporter_id = ");
        qb.push_bind(ctx.user);
        qb.push(" OR i.assignee_id = ");
        qb.push_bind(ctx.user);
        qb.push(" OR i.created_by = ");
        qb.push_bind(ctx.user);
        qb.push(" OR i.security_level_id IN (SELECT tc_visible_levels(");
        qb.push_bind(ctx.user);
        qb.push(")))");
    }
    if let Some(f) = &q.filter {
        qb.push(" AND ");
        expr(qb, ctx, f)?;
    }
    Ok(())
}

fn expr(qb: &mut Qb, ctx: &Ctx, e: &Expr) -> ApiResult<()> {
    match e {
        Expr::And { items } | Expr::Or { items } => {
            let joiner = if matches!(e, Expr::And { .. }) { " AND " } else { " OR " };
            qb.push("(");
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    qb.push(joiner);
                }
                expr(qb, ctx, item)?;
            }
            qb.push(")");
        }
        Expr::Not { item } => {
            qb.push("NOT ");
            expr(qb, ctx, item)?;
        }
        Expr::Text { text } => {
            qb.push("COALESCE(");
            text_search(qb, text);
            qb.push(", false)");
        }
        Expr::Compare { field, op, value } => {
            qb.push("COALESCE(");
            compare(qb, ctx, &field.to_lowercase(), *op, std::slice::from_ref(value), false)?;
            qb.push(", false)");
        }
        Expr::In { field, values, negated } => {
            qb.push("COALESCE(");
            compare(qb, ctx, &field.to_lowercase(), if *negated { Op::Ne } else { Op::Eq }, values, true)?;
            qb.push(", false)");
        }
        Expr::Empty { field, negated } => {
            qb.push("COALESCE(");
            empty(qb, ctx, &field.to_lowercase(), *negated)?;
            qb.push(", false)");
        }
    }
    Ok(())
}

/// Words for a prefix search: `log in` → `log:* & in:*`.
fn tsquery(text: &str) -> Option<String> {
    let words: Vec<String> = text.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).take(12).map(|w| format!("{}:*", w.to_lowercase())).collect();
    (!words.is_empty()).then(|| words.join(" & "))
}

fn like(text: &str) -> String {
    format!("%{}%", text.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_"))
}

fn text_search(qb: &mut Qb, text: &str) {
    qb.push("(i.title ILIKE ");
    qb.push_bind(like(text));
    if let Some(tq) = tsquery(text) {
        qb.push(" OR i.search @@ to_tsquery('simple', ");
        qb.push_bind(tq);
        qb.push(")");
    }
    if let Some((k, n)) = split_key(text) {
        qb.push(" OR (upper(p.key) = ");
        qb.push_bind(k);
        qb.push(" AND i.number = ");
        qb.push_bind(n);
        qb.push(")");
    }
    qb.push(")");
}

/// Text values of a list (functions are rejected with the field's name).
fn texts(field: &str, values: &[Value]) -> ApiResult<Vec<String>> {
    values.iter().map(|v| v.as_text().ok_or_else(|| ApiError::bad(format!("{field} takes names, not functions")))).collect()
}

/// People by name, e-mail, id or me(): (ids, lower-case names).
fn people(ctx: &Ctx, field: &str, values: &[Value]) -> ApiResult<(Vec<Uuid>, Vec<String>)> {
    let mut ids = Vec::new();
    let mut names = Vec::new();
    for v in values {
        match v {
            Value::Function { name, .. } if matches!(name.to_lowercase().as_str(), "me" | "currentuser") => ids.push(ctx.user),
            Value::Function { name, .. } => return bad(format!("{field} doesn't know the function {name}()")),
            other => {
                let t = other.as_text().unwrap_or_default();
                match t.parse::<Uuid>() {
                    Ok(id) => ids.push(id),
                    Err(_) => names.push(t.to_lowercase()),
                }
            }
        }
    }
    Ok((ids, names))
}

fn only_eq(field: &str, op: Op) -> ApiResult<bool> {
    match op {
        Op::Eq => Ok(false),
        Op::Ne => Ok(true),
        _ => bad(format!("{field} can be compared with =, !=, in and not in")),
    }
}

/// `col IN (SELECT id FROM <table> WHERE <cond>)`, with empty values included for `!=`.
fn member_of(qb: &mut Qb, col: &str, negated: bool, sub: impl FnOnce(&mut Qb)) {
    if negated {
        qb.push(format!("({col} IS NULL OR {col} NOT IN ("));
    } else {
        qb.push(format!("{col} IN ("));
    }
    sub(qb);
    qb.push(if negated { "))" } else { ")" });
}

fn compare(qb: &mut Qb, ctx: &Ctx, field: &str, op: Op, values: &[Value], _list: bool) -> ApiResult<()> {
    match field {
        "project" => {
            let neg = only_eq(field, op)?;
            let names: Vec<String> = texts(field, values)?.into_iter().map(|s| s.to_lowercase()).collect();
            qb.push(if neg { "NOT " } else { "" });
            qb.push("(lower(p.key) = ANY(");
            qb.push_bind(names.clone());
            qb.push(") OR lower(p.name) = ANY(");
            qb.push_bind(names.clone());
            qb.push(") OR i.project_id IN (SELECT project_id FROM project_key_aliases WHERE workspace_id = ");
            qb.push_bind(ctx.workspace);
            qb.push(" AND lower(key) = ANY(");
            qb.push_bind(names);
            qb.push(")))");
        }
        "key" | "issue" => {
            let neg = only_eq(field, op)?;
            let keys = texts(field, values)?;
            let mut parts: Vec<(String, i32)> = Vec::new();
            for k in &keys {
                parts.push(split_key(k).ok_or_else(|| ApiError::bad(format!("{k} isn't an issue key (like WEB-12)")))?);
            }
            qb.push(if neg { "NOT (" } else { "(" });
            for (i, (k, n)) in parts.into_iter().enumerate() {
                if i > 0 {
                    qb.push(" OR ");
                }
                qb.push("(upper(p.key) = ");
                qb.push_bind(k);
                qb.push(" AND i.number = ");
                qb.push_bind(n);
                qb.push(")");
            }
            qb.push(")");
        }
        "id" => {
            let neg = only_eq(field, op)?;
            let ids: Vec<Uuid> = texts(field, values)?.iter().filter_map(|s| s.parse().ok()).collect();
            qb.push(if neg { "NOT (i.id = ANY(" } else { "(i.id = ANY(" });
            qb.push_bind(ids);
            qb.push("))");
        }
        "title" | "summary" | "description" | "text" | "comment" | "comments" => {
            let t = texts(field, values)?.join(" ");
            let neg = match op {
                Op::Contains | Op::Eq => false,
                Op::NotContains | Op::Ne => true,
                _ => return bad(format!("{field} can be searched with ~ (contains) or !~")),
            };
            qb.push(if neg { "NOT " } else { "" });
            match field {
                "title" | "summary" => {
                    qb.push("(i.title ILIKE ");
                    qb.push_bind(like(&t));
                    qb.push(")");
                }
                "description" => {
                    qb.push("(i.description_text ILIKE ");
                    qb.push_bind(like(&t));
                    qb.push(")");
                }
                "comment" | "comments" => {
                    // restricted comments are searched by their authors only (others may not read them)
                    qb.push("EXISTS (SELECT 1 FROM comments c WHERE c.issue_id = i.id AND (c.visibility IS NULL OR c.author_id = ");
                    qb.push_bind(ctx.user);
                    qb.push(") AND c.body_text ILIKE ");
                    qb.push_bind(like(&t));
                    qb.push(")");
                }
                _ => text_search(qb, &t),
            }
        }
        "type" | "issuetype" => {
            let neg = only_eq(field, op)?;
            let names: Vec<String> = texts(field, values)?.into_iter().map(|s| s.to_lowercase()).collect();
            qb.push(if neg { "NOT (lower(it.name) = ANY(" } else { "(lower(it.name) = ANY(" });
            qb.push_bind(names);
            qb.push("))");
        }
        "status" => {
            let neg = only_eq(field, op)?;
            let names: Vec<String> = texts(field, values)?.into_iter().map(|s| s.to_lowercase()).collect();
            qb.push(if neg { "NOT (lower(s.name) = ANY(" } else { "(lower(s.name) = ANY(" });
            qb.push_bind(names);
            qb.push("))");
        }
        "resolution" => {
            let neg = only_eq(field, op)?;
            let names: Vec<String> = texts(field, values)?.into_iter().map(|s| s.to_lowercase()).collect();
            qb.push(if neg { "NOT (lower(i.resolution) = ANY(" } else { "(lower(i.resolution) = ANY(" });
            qb.push_bind(names);
            qb.push("))");
        }
        "security" => {
            let neg = only_eq(field, op)?;
            let names: Vec<String> = texts(field, values)?.into_iter().map(|s| s.to_lowercase()).collect();
            let visible = ctx.visible.clone();
            member_of(qb, "i.security_level_id", neg, |qb| {
                qb.push("SELECT id FROM security_levels WHERE project_id = ANY(");
                qb.push_bind(visible);
                qb.push(") AND lower(name) = ANY(");
                qb.push_bind(names);
                qb.push(")");
            });
        }
        "category" | "statuscategory" => {
            let neg = only_eq(field, op)?;
            let mut cats = Vec::new();
            for t in texts(field, values)? {
                cats.push(parse_category(&t).ok_or_else(|| ApiError::bad(format!("{t} isn't a category: use todo, in_progress or done")))?.to_string());
            }
            qb.push(if neg { "NOT (s.category = ANY(" } else { "(s.category = ANY(" });
            qb.push_bind(cats);
            qb.push("))");
        }
        "priority" => {
            let mut nums = Vec::new();
            for v in values {
                let t = v.as_text().ok_or_else(|| ApiError::bad("priority takes a name: none, low, medium, high, urgent"))?;
                nums.push(parse_priority(&t).ok_or_else(|| ApiError::bad(format!("{t} isn't a priority: none, low, medium, high, urgent")))?);
            }
            match (op, nums.as_slice()) {
                (Op::Eq, _) | (Op::Ne, _) => {
                    qb.push(if op == Op::Ne { "NOT (i.priority = ANY(" } else { "(i.priority = ANY(" });
                    qb.push_bind(nums);
                    qb.push("))");
                }
                (Op::Gt | Op::Ge | Op::Lt | Op::Le, [n]) => {
                    qb.push(format!("i.priority {} ", op.as_str()));
                    qb.push_bind(*n);
                }
                _ => return bad("priority can be compared with =, !=, >, >=, <, <="),
            }
        }
        "assignee" | "reporter" | "creator" => {
            let neg = only_eq(field, op)?;
            let (ids, names) = people(ctx, field, values)?;
            let col = match field {
                "assignee" => "i.assignee_id",
                "reporter" => "i.reporter_id",
                _ => "i.created_by",
            };
            let ws = ctx.workspace;
            member_of(qb, col, neg, |qb| {
                qb.push("SELECT id FROM users WHERE workspace_id = ");
                qb.push_bind(ws);
                qb.push(" AND (id = ANY(");
                qb.push_bind(ids);
                qb.push(") OR lower(name) = ANY(");
                qb.push_bind(names.clone());
                qb.push(") OR lower(email) = ANY(");
                qb.push_bind(names);
                qb.push("))");
            });
        }
        "watcher" | "watchers" => {
            let neg = only_eq(field, op)?;
            let (ids, names) = people(ctx, field, values)?;
            qb.push(if neg { "NOT " } else { "" });
            qb.push("EXISTS (SELECT 1 FROM issue_watchers w JOIN users u ON u.id = w.user_id WHERE w.issue_id = i.id AND (u.id = ANY(");
            qb.push_bind(ids);
            qb.push(") OR lower(u.name) = ANY(");
            qb.push_bind(names.clone());
            qb.push(") OR lower(u.email) = ANY(");
            qb.push_bind(names);
            qb.push(")))");
        }
        "labels" | "label" => {
            let (neg, contains) = match op {
                Op::Eq => (false, false),
                Op::Ne => (true, false),
                Op::Contains => (false, true),
                Op::NotContains => (true, true),
                _ => return bad("labels can be compared with =, !=, ~, in"),
            };
            let labels: Vec<String> = texts(field, values)?.into_iter().map(|s| normalize_label(&s)).collect();
            qb.push(if neg { "NOT " } else { "" });
            qb.push("EXISTS (SELECT 1 FROM issue_labels l WHERE l.issue_id = i.id AND ");
            if contains {
                qb.push("l.label ILIKE ");
                qb.push_bind(like(&labels.join(" ")));
            } else {
                qb.push("l.label = ANY(");
                qb.push_bind(labels);
                qb.push(")");
            }
            qb.push(")");
        }
        "sprint" => {
            let neg = only_eq(field, op)?;
            let mut states = Vec::new();
            let mut names = Vec::new();
            for v in values {
                match v {
                    Value::Function { name, .. } => states.push(
                        match name.to_lowercase().as_str() {
                            "opensprints" | "activesprint" | "activesprints" | "currentsprint" => "active",
                            "futuresprints" | "nextsprint" => "future",
                            "closedsprints" => "closed",
                            other => return bad(format!("sprint doesn't know {other}(): use openSprints(), futureSprints() or closedSprints()")),
                        }
                        .to_string(),
                    ),
                    other => names.push(other.as_text().unwrap_or_default().to_lowercase()),
                }
            }
            let visible = ctx.visible.clone();
            member_of(qb, "i.sprint_id", neg, |qb| {
                qb.push("SELECT id FROM sprints WHERE project_id = ANY(");
                qb.push_bind(visible);
                qb.push(") AND (state = ANY(");
                qb.push_bind(states);
                qb.push(") OR lower(name) = ANY(");
                qb.push_bind(names);
                qb.push("))");
            });
        }
        "version" | "fixversion" | "release" => {
            let neg = only_eq(field, op)?;
            let mut states = Vec::new();
            let mut names = Vec::new();
            for v in values {
                match v {
                    Value::Function { name, .. } => states.push(
                        match name.to_lowercase().as_str() {
                            "releasedversions" => "released",
                            "unreleasedversions" => "unreleased",
                            other => return bad(format!("version doesn't know {other}(): use releasedVersions() or unreleasedVersions()")),
                        }
                        .to_string(),
                    ),
                    other => names.push(other.as_text().unwrap_or_default().to_lowercase()),
                }
            }
            let visible = ctx.visible.clone();
            member_of(qb, "i.version_id", neg, |qb| {
                qb.push("SELECT id FROM versions WHERE project_id = ANY(");
                qb.push_bind(visible);
                qb.push(") AND (status = ANY(");
                qb.push_bind(states);
                qb.push(") OR lower(name) = ANY(");
                qb.push_bind(names);
                qb.push("))");
            });
        }
        "component" => {
            let neg = only_eq(field, op)?;
            let names: Vec<String> = texts(field, values)?.into_iter().map(|s| s.to_lowercase()).collect();
            let visible = ctx.visible.clone();
            member_of(qb, "i.component_id", neg, |qb| {
                qb.push("SELECT id FROM components WHERE project_id = ANY(");
                qb.push_bind(visible);
                qb.push(") AND lower(name) = ANY(");
                qb.push_bind(names);
                qb.push(")");
            });
        }
        "parent" | "epic" => {
            let neg = only_eq(field, op)?;
            let mut keys: Vec<String> = Vec::new();
            let mut nums: Vec<i32> = Vec::new();
            for t in texts(field, values)? {
                let (k, n) = split_key(&t).ok_or_else(|| ApiError::bad(format!("{field} takes issue keys (like WEB-12), not {t}")))?;
                keys.push(k);
                nums.push(n);
            }
            member_of(qb, "i.parent_id", neg, |qb| {
                qb.push("SELECT pi.id FROM issues pi JOIN projects pp ON pp.id = pi.project_id JOIN unnest(");
                qb.push_bind(keys);
                qb.push("::text[], ");
                qb.push_bind(nums);
                qb.push("::int[]) AS want(k, n) ON upper(pp.key) = want.k AND pi.number = want.n");
            });
        }
        "points" | "storypoints" | "estimate" | "spent" | "remaining" => {
            let (col, scale) = match field {
                "estimate" => ("i.estimate_minutes", 60.0),
                "remaining" => ("COALESCE(i.remaining_minutes, GREATEST(i.estimate_minutes - i.spent_minutes, 0))", 60.0),
                "spent" => ("i.spent_minutes", 60.0),
                _ => ("i.story_points", 1.0),
            };
            numeric(qb, field, col, op, values, scale)?;
        }
        "created" | "updated" | "resolved" => {
            let col = match field {
                "created" => "i.created_at",
                "updated" => "i.updated_at",
                _ => "i.resolved_at",
            };
            time_compare(qb, ctx, field, col, op, values, false)?;
        }
        "due" | "duedate" | "start" | "startdate" => {
            let col = if field.starts_with("due") { "i.due_date" } else { "i.start_date" };
            time_compare(qb, ctx, field, col, op, values, true)?;
        }
        "blocked" => {
            let neg = only_eq(field, op)?;
            let want = texts(field, values)?.first().map(|s| matches!(s.to_lowercase().as_str(), "true" | "yes" | "1")).unwrap_or(true);
            qb.push(if want != neg { "" } else { "NOT " });
            qb.push(BLOCKED_SQL);
        }
        "rank" => return bad("rank can only be used to sort (order by rank)"),
        other => custom_compare(qb, ctx, other, op, values)?,
    }
    Ok(())
}

/// Whether an open issue blocks `i`.
pub const BLOCKED_SQL: &str = "EXISTS (SELECT 1 FROM issue_links bl JOIN issues bi ON bi.id = bl.source_id JOIN statuses bs ON bs.id = bi.status_id \
                               WHERE bl.target_id = i.id AND bl.kind = 'blocks' AND bs.category <> 'done' AND bi.deleted_at IS NULL)";

pub fn normalize_label(s: &str) -> String {
    s.trim().to_lowercase().split_whitespace().collect::<Vec<_>>().join("-").chars().take(60).collect()
}

fn numeric(qb: &mut Qb, field: &str, col: &str, op: Op, values: &[Value], scale: f64) -> ApiResult<()> {
    let nums: Vec<f64> = values
        .iter()
        .map(|v| match v {
            Value::Number { value } => Ok(*value * scale),
            other => other.as_text().and_then(|t| t.parse::<f64>().ok()).map(|n| n * scale).ok_or_else(|| ApiError::bad(format!("{field} takes numbers"))),
        })
        .collect::<ApiResult<_>>()?;
    match op {
        Op::Eq | Op::Ne => {
            qb.push(format!("{}({col}::float8 = ANY(", if op == Op::Ne { "NOT " } else { "" }));
            qb.push_bind(nums);
            qb.push("))");
        }
        Op::Gt | Op::Ge | Op::Lt | Op::Le if nums.len() == 1 => {
            qb.push(format!("{col}::float8 {} ", op.as_str()));
            qb.push_bind(nums[0]);
        }
        _ => return bad(format!("{field} can be compared with =, !=, >, >=, <, <=")),
    }
    Ok(())
}

/// A moment: `2026-11-01`, `2026-11-01 14:00`, `-7d`, `+2w`, `now()`, `startOfWeek()`, …
/// Returns the moment and whether it names a whole day.
pub fn moment(ctx_now: DateTime<Utc>, v: &Value) -> Result<(DateTime<Utc>, bool), String> {
    let start_of_day = |t: DateTime<Utc>| Utc.from_utc_datetime(&t.date_naive().and_hms_opt(0, 0, 0).unwrap_or_default());
    match v {
        Value::Function { name, args } => {
            let shift = match args.first() {
                Some(a) => relative(ctx_now, &a.as_text().unwrap_or_default()).ok_or_else(|| format!("{name}() takes an offset like \"-7d\""))? - ctx_now,
                None => Duration::zero(),
            };
            let today = start_of_day(ctx_now);
            let weekday = ctx_now.weekday().num_days_from_monday() as i64;
            let first_of_month = Utc.with_ymd_and_hms(ctx_now.year(), ctx_now.month(), 1, 0, 0, 0).single().unwrap_or(today);
            let next_month = if ctx_now.month() == 12 {
                Utc.with_ymd_and_hms(ctx_now.year() + 1, 1, 1, 0, 0, 0).single()
            } else {
                Utc.with_ymd_and_hms(ctx_now.year(), ctx_now.month() + 1, 1, 0, 0, 0).single()
            }
            .unwrap_or(today);
            let (t, day) = match name.to_lowercase().as_str() {
                "now" => (ctx_now, false),
                "today" | "startofday" => (today, true),
                "endofday" => (today + Duration::days(1) - Duration::seconds(1), true),
                "startofweek" => (today - Duration::days(weekday), true),
                "endofweek" => (today + Duration::days(6 - weekday + 1) - Duration::seconds(1), true),
                "startofmonth" => (first_of_month, true),
                "endofmonth" => (next_month - Duration::seconds(1), true),
                "startofyear" => (Utc.with_ymd_and_hms(ctx_now.year(), 1, 1, 0, 0, 0).single().unwrap_or(today), true),
                "endofyear" => (Utc.with_ymd_and_hms(ctx_now.year() + 1, 1, 1, 0, 0, 0).single().unwrap_or(today) - Duration::seconds(1), true),
                other => return Err(format!("unknown date function {other}()")),
            };
            Ok((t + shift, day))
        }
        Value::Number { .. } => Err("dates look like 2026-11-01 or -7d".into()),
        Value::Text { text } => {
            if let Some(t) = relative(ctx_now, text) {
                // whole days (-7d) count from the start of that day
                let day = text.ends_with(['d', 'w', 'm', 'y']);
                return Ok((if day { start_of_day(t) } else { t }, day));
            }
            if let Ok(d) = NaiveDate::parse_from_str(text.trim(), "%Y-%m-%d") {
                return Ok((Utc.from_utc_datetime(&d.and_hms_opt(0, 0, 0).unwrap_or_default()), true));
            }
            for f in ["%Y-%m-%d %H:%M", "%Y-%m-%dT%H:%M", "%Y-%m-%dT%H:%M:%S"] {
                if let Ok(t) = chrono::NaiveDateTime::parse_from_str(text.trim(), f) {
                    return Ok((Utc.from_utc_datetime(&t), false));
                }
            }
            if let Ok(t) = DateTime::parse_from_rfc3339(text.trim()) {
                return Ok((t.with_timezone(&Utc), false));
            }
            Err(format!("{text} isn't a date: use 2026-11-01, -7d, +2w or a function like startOfWeek()"))
        }
    }
}

/// `-7d`, `+2w`, `3h`, `-1m` (months), `1y` relative to now.
fn relative(now: DateTime<Utc>, s: &str) -> Option<DateTime<Utc>> {
    let s = s.trim();
    let (idx, _) = s.char_indices().last()?;
    let (num, unit) = s.split_at(idx);
    let n: i64 = num.trim_start_matches('+').parse().ok()?;
    if n.abs() > 100_000 {
        return None;
    }
    Some(match unit {
        "m" => {
            let months = now.year() as i64 * 12 + now.month0() as i64 + n;
            let (y, m) = (months.div_euclid(12) as i32, months.rem_euclid(12) as u32 + 1);
            let day = now.day().min(28);
            Utc.with_ymd_and_hms(y, m, day, now.hour_(), 0, 0).single()?
        }
        "h" => now + Duration::hours(n),
        "d" => now + Duration::days(n),
        "w" => now + Duration::weeks(n),
        "y" => now + Duration::days(365 * n),
        _ => return None,
    })
}

trait HourExt {
    fn hour_(&self) -> u32;
}

impl HourExt for DateTime<Utc> {
    fn hour_(&self) -> u32 {
        chrono::Timelike::hour(self)
    }
}

fn time_compare(qb: &mut Qb, ctx: &Ctx, field: &str, col: &str, op: Op, values: &[Value], date_col: bool) -> ApiResult<()> {
    let mut moments = Vec::new();
    for v in values {
        moments.push(moment(ctx.now, v).map_err(ApiError::BadRequest)?);
    }
    let bind_t = |qb: &mut Qb, t: DateTime<Utc>| {
        if date_col {
            qb.push_bind(t.date_naive());
        } else {
            qb.push_bind(t);
        }
    };
    match (op, moments.as_slice()) {
        (Op::Eq | Op::Ne, list) => {
            qb.push(if op == Op::Ne { "NOT (" } else { "(" });
            for (i, (t, day)) in list.iter().enumerate() {
                if i > 0 {
                    qb.push(" OR ");
                }
                if date_col {
                    qb.push(format!("{col} = "));
                    qb.push_bind(t.date_naive());
                } else if *day {
                    qb.push(format!("({col} >= "));
                    qb.push_bind(*t);
                    qb.push(format!(" AND {col} < "));
                    qb.push_bind(*t + Duration::days(1));
                    qb.push(")");
                } else {
                    qb.push(format!("{col} = "));
                    qb.push_bind(*t);
                }
            }
            qb.push(")");
        }
        (Op::Gt | Op::Ge | Op::Lt | Op::Le, [(t, day)]) => {
            // `created <= 2026-11-01` includes that day; `created > 2026-11-01` starts after it
            let (op_s, t) = match (op, day, date_col) {
                (Op::Le, true, false) => ("<", *t + Duration::days(1)),
                (Op::Gt, true, false) => (">=", *t + Duration::days(1)),
                _ => (op.as_str(), *t),
            };
            qb.push(format!("{col} {op_s} "));
            bind_t(qb, t);
        }
        _ => return bad(format!("{field} can be compared with =, !=, >, >=, <, <=")),
    }
    Ok(())
}

fn custom_def<'a>(ctx: &'a Ctx, field: &str) -> ApiResult<&'a FieldDef> {
    ctx.custom.get(field).ok_or_else(|| ApiError::bad(format!("unknown field {field}")))
}

/// `(i.custom->>id1, i.custom->>id2, …)` reduced to the first that is set.
fn custom_col(qb: &mut Qb, def: &FieldDef, json: bool) {
    qb.push("COALESCE(");
    for (n, id) in def.ids.iter().enumerate() {
        if n > 0 {
            qb.push(", ");
        }
        qb.push(if json { "i.custom->" } else { "i.custom->>" });
        qb.push_bind(id.clone());
    }
    qb.push(")");
}

fn custom_compare(qb: &mut Qb, ctx: &Ctx, field: &str, op: Op, values: &[Value]) -> ApiResult<()> {
    let def = custom_def(ctx, field)?.clone();
    match def.kind.as_str() {
        "number" => {
            let nums: Vec<f64> = values.iter().filter_map(|v| v.as_text().and_then(|t| t.parse().ok())).collect();
            if nums.len() != values.len() {
                return bad(format!("{field} takes numbers"));
            }
            qb.push("(");
            custom_col(qb, &def, false);
            match op {
                Op::Eq | Op::Ne => {
                    qb.push(")::float8 = ANY(");
                    qb.push_bind(nums);
                    qb.push(")");
                    if op == Op::Ne {
                        return bad(format!("use not ({field} = …) for numbers"));
                    }
                }
                Op::Gt | Op::Ge | Op::Lt | Op::Le if nums.len() == 1 => {
                    qb.push(format!(")::float8 {} ", op.as_str()));
                    qb.push_bind(nums[0]);
                }
                _ => return bad(format!("{field} can be compared with =, >, >=, <, <=")),
            }
        }
        "date" => {
            qb.push("(");
            custom_col(qb, &def, false);
            qb.push(")::date");
            let (t, _) = moment(ctx.now, values.first().ok_or_else(|| ApiError::bad("a date is missing"))?).map_err(ApiError::BadRequest)?;
            match op {
                Op::Contains | Op::NotContains => return bad(format!("{field} can't use ~")),
                _ => {
                    qb.push(format!(" {} ", op.as_str()));
                    qb.push_bind(t.date_naive());
                }
            }
        }
        "checkbox" => {
            let want = values.first().and_then(|v| v.as_text()).map(|t| matches!(t.to_lowercase().as_str(), "true" | "yes" | "1" | "checked")).unwrap_or(true);
            let neg = only_eq(field, op)?;
            qb.push("COALESCE((");
            custom_col(qb, &def, false);
            qb.push(")::boolean, false) = ");
            qb.push_bind(want != neg);
        }
        "select" | "multiselect" => {
            let neg = only_eq(field, op)?;
            let mut ids: Vec<String> = Vec::new();
            for t in texts(field, values)? {
                match def.options.get(&t.to_lowercase()) {
                    Some(o) => ids.extend(o.iter().cloned()),
                    None => ids.push(t),
                }
            }
            qb.push(if neg { "NOT (" } else { "(" });
            if def.kind == "select" {
                custom_col(qb, &def, false);
                qb.push(" = ANY(");
                qb.push_bind(ids);
                qb.push(")");
            } else {
                qb.push("EXISTS (SELECT 1 FROM jsonb_array_elements_text(CASE WHEN jsonb_typeof(");
                custom_col(qb, &def, true);
                qb.push(") = 'array' THEN ");
                custom_col(qb, &def, true);
                qb.push(" ELSE '[]'::jsonb END) AS o(v) WHERE o.v = ANY(");
                qb.push_bind(ids);
                qb.push("))");
            }
            qb.push(")");
        }
        "user" => {
            let neg = only_eq(field, op)?;
            let (ids, names) = people(ctx, field, values)?;
            let ws = ctx.workspace;
            qb.push(if neg { "NOT (" } else { "(" });
            custom_col(qb, &def, false);
            qb.push(" IN (SELECT id::text FROM users WHERE workspace_id = ");
            qb.push_bind(ws);
            qb.push(" AND (id = ANY(");
            qb.push_bind(ids);
            qb.push(") OR lower(name) = ANY(");
            qb.push_bind(names.clone());
            qb.push(") OR lower(email) = ANY(");
            qb.push_bind(names);
            qb.push("))))");
        }
        _ => {
            let t = texts(field, values)?;
            match op {
                Op::Contains | Op::NotContains => {
                    qb.push(if op == Op::NotContains { "NOT (" } else { "(" });
                    custom_col(qb, &def, false);
                    qb.push(" ILIKE ");
                    qb.push_bind(like(&t.join(" ")));
                    qb.push(")");
                }
                Op::Eq | Op::Ne => {
                    qb.push(if op == Op::Ne { "NOT (lower(" } else { "(lower(" });
                    custom_col(qb, &def, false);
                    qb.push(") = ANY(");
                    qb.push_bind(t.into_iter().map(|s| s.to_lowercase()).collect::<Vec<_>>());
                    qb.push("))");
                }
                _ => return bad(format!("{field} can be compared with =, !=, ~")),
            }
        }
    }
    Ok(())
}

fn empty(qb: &mut Qb, ctx: &Ctx, field: &str, negated: bool) -> ApiResult<()> {
    let col = match field {
        "assignee" => "i.assignee_id",
        "reporter" => "i.reporter_id",
        "sprint" => "i.sprint_id",
        "version" | "fixversion" | "release" => "i.version_id",
        "component" => "i.component_id",
        "parent" | "epic" => "i.parent_id",
        "points" | "storypoints" => "i.story_points",
        "estimate" => "i.estimate_minutes",
        "remaining" => "COALESCE(i.remaining_minutes, GREATEST(i.estimate_minutes - i.spent_minutes, 0))",
        "due" | "duedate" => "i.due_date",
        "start" | "startdate" => "i.start_date",
        "resolved" => "i.resolved_at",
        "resolution" => "i.resolution",
        "security" => "i.security_level_id",
        "labels" | "label" => {
            qb.push(if negated { "EXISTS" } else { "NOT EXISTS" });
            qb.push(" (SELECT 1 FROM issue_labels l WHERE l.issue_id = i.id)");
            return Ok(());
        }
        "description" => {
            qb.push(if negated { "i.description_text <> ''" } else { "i.description_text = ''" });
            return Ok(());
        }
        "watcher" | "watchers" => {
            qb.push(if negated { "EXISTS" } else { "NOT EXISTS" });
            qb.push(" (SELECT 1 FROM issue_watchers w WHERE w.issue_id = i.id)");
            return Ok(());
        }
        other if !is_builtin(other) => {
            let def = custom_def(ctx, other)?.clone();
            qb.push("(NULLIF(NULLIF(");
            custom_col(qb, &def, false);
            qb.push(if negated { ", ''), '[]') IS NOT NULL)" } else { ", ''), '[]') IS NULL)" });
            return Ok(());
        }
        other => return bad(format!("{other} is never empty")),
    };
    qb.push(format!("{col} IS {}NULL", if negated { "NOT " } else { "" }));
    Ok(())
}

/// Appends ` ORDER BY …` (falls back to `default` when the query has no order).
pub fn push_order(qb: &mut Qb, ctx: &Ctx, order: &[Order], default: &str) -> ApiResult<()> {
    qb.push(" ORDER BY ");
    if order.is_empty() {
        qb.push(default);
        return Ok(());
    }
    for (n, o) in order.iter().enumerate() {
        if n > 0 {
            qb.push(", ");
        }
        let dir = if o.desc { " DESC NULLS LAST" } else { " ASC NULLS LAST" };
        let col = match o.field.to_lowercase().as_str() {
            "rank" => "i.rank",
            "key" | "issue" => "p.key, i.number",
            "created" => "i.created_at",
            "updated" => "i.updated_at",
            "resolved" => "i.resolved_at",
            "resolution" => "lower(i.resolution)",
            "due" | "duedate" => "i.due_date",
            "start" | "startdate" => "i.start_date",
            "priority" => "i.priority",
            "status" => "s.position",
            "category" | "statuscategory" => "CASE s.category WHEN 'todo' THEN 0 WHEN 'in_progress' THEN 1 ELSE 2 END",
            "type" | "issuetype" => "it.position",
            "assignee" => "(SELECT name FROM users WHERE id = i.assignee_id)",
            "reporter" => "(SELECT name FROM users WHERE id = i.reporter_id)",
            "title" | "summary" => "lower(i.title)",
            "points" | "storypoints" => "i.story_points",
            "estimate" => "i.estimate_minutes",
            "remaining" => "COALESCE(i.remaining_minutes, GREATEST(i.estimate_minutes - i.spent_minutes, 0))",
            "spent" => "i.spent_minutes",
            "project" => "p.key",
            other => {
                let def = custom_def(ctx, other)?.clone();
                custom_col(qb, &def, false);
                qb.push(dir);
                continue;
            }
        };
        if col.contains(", ") {
            qb.push(col.replace(", ", &format!("{dir}, ")));
        } else {
            qb.push(col);
        }
        qb.push(dir);
    }
    qb.push(", i.id");
    Ok(())
}

/// Parses TQL into a 400 with the position on errors.
pub fn parse(q: &str) -> ApiResult<Query> {
    if q.len() > 4000 {
        return bad("the query is too long");
    }
    taskcraft_tql::parse(q).map_err(|e| ApiError::BadRequest(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> Ctx {
        Ctx { user: Uuid::nil(), workspace: Uuid::nil(), visible: vec![], now: Utc.with_ymd_and_hms(2026, 11, 12, 10, 30, 0).unwrap(), custom: HashMap::new() }
    }

    fn sql(q: &str) -> String {
        let q = parse(q).unwrap();
        let mut qb: Qb = QueryBuilder::new("SELECT 1 FROM issues i WHERE");
        push_where(&mut qb, &ctx(), &q).unwrap();
        push_order(&mut qb, &ctx(), &q.order, "i.rank").unwrap();
        qb.sql().as_str().to_string()
    }

    #[test]
    fn compiles_common_queries() {
        let s = sql("project = WEB and status != Done and assignee = me() order by priority desc");
        assert!(s.contains("lower(p.key) = ANY($2)"), "{s}");
        assert!(s.contains("NOT (lower(s.name) = ANY("), "{s}");
        assert!(s.contains("ORDER BY i.priority DESC NULLS LAST, i.id"), "{s}");
        let s = sql("login crash");
        assert!(s.contains("to_tsquery('simple'"), "{s}");
        let s = sql("labels is empty or sprint in openSprints()");
        assert!(s.contains("NOT EXISTS (SELECT 1 FROM issue_labels"), "{s}");
        assert!(s.contains("i.sprint_id IN (SELECT id FROM sprints"), "{s}");
        let s = sql("assignee != me()");
        assert!(s.contains("i.assignee_id IS NULL OR i.assignee_id NOT IN"), "{s}");
        let s = sql("order by key");
        assert!(s.contains("p.key ASC NULLS LAST, i.number ASC NULLS LAST"), "{s}");
    }

    #[test]
    fn rejects_nonsense() {
        let q = parse("status > Done").unwrap();
        let mut qb: Qb = QueryBuilder::new("");
        assert!(push_where(&mut qb, &ctx(), &q).is_err());
        let q = parse("frobnicate = 3").unwrap();
        assert!(push_where(&mut QueryBuilder::new(""), &ctx(), &q).is_err());
        let q = parse("priority = sometimes").unwrap();
        assert!(push_where(&mut QueryBuilder::new(""), &ctx(), &q).is_err());
    }

    #[test]
    fn moments() {
        let now = ctx().now;
        let (t, day) = moment(now, &Value::text("-7d")).unwrap();
        assert!(day);
        assert_eq!(t, Utc.with_ymd_and_hms(2026, 11, 5, 0, 0, 0).unwrap());
        let (t, _) = moment(now, &Value::Function { name: "startOfWeek".into(), args: vec![] }).unwrap();
        assert_eq!(t, Utc.with_ymd_and_hms(2026, 11, 9, 0, 0, 0).unwrap()); // a Monday
        let (t, _) = moment(now, &Value::Function { name: "endOfMonth".into(), args: vec![] }).unwrap();
        assert_eq!(t, Utc.with_ymd_and_hms(2026, 11, 30, 23, 59, 59).unwrap());
        let (t, _) = moment(now, &Value::text("-1m")).unwrap();
        assert_eq!(t.month(), 10);
        let (t, day) = moment(now, &Value::text("2026-01-02")).unwrap();
        assert!(day);
        assert_eq!(t.day(), 2);
        assert!(moment(now, &Value::text("soon")).is_err());
    }

    #[test]
    fn keys_and_priorities() {
        assert_eq!(split_key("web-12"), Some(("WEB".into(), 12)));
        assert_eq!(split_key("WEB-0"), None);
        assert_eq!(split_key("nope"), None);
        assert_eq!(parse_priority("High"), Some(3));
        assert_eq!(parse_priority("4"), Some(4));
        assert_eq!(priority_name(1), "low");
        assert_eq!(parse_category("In progress"), Some("in_progress"));
        assert_eq!(normalize_label("  Needs Review "), "needs-review");
    }
}
