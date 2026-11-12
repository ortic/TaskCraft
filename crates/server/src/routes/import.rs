//! CSV import into a project: columns are mapped to fields (suggested from the header names,
//! adjustable), values are matched by name (statuses, types, people, sprints, releases,
//! components, custom field options), and a dry run reports what would fail before anything is
//! written. Keys from the source system become aliases, so references like `OLD-12` keep
//! working. Imports tell nobody (no notifications, webhooks or automation).

use std::collections::HashMap;

use axum::Json;
use axum::extract::{Path, State};
use chrono::{DateTime, NaiveDate, NaiveDateTime, Utc};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use uuid::Uuid;

use super::projects::by_key;
use crate::access::Role;
use crate::app::{AppState, audit, new_id};
use crate::auth::CurrentUser;
use crate::error::{ApiError, ApiResult};
use crate::events::{self, Event};
use crate::issues::{self, Actor, NewIssue};
use crate::query::{parse_category, parse_priority, split_key};

const MAX_ROWS: usize = 5000;

/// The fields a column can go to (plus `custom:<id>` and `ignore`).
const FIELDS: &[&str] = &[
    "key",
    "title",
    "description",
    "type",
    "status",
    "priority",
    "assignee",
    "reporter",
    "labels",
    "sprint",
    "version",
    "component",
    "parent",
    "points",
    "estimate",
    "spent",
    "start",
    "due",
    "created",
    "resolved",
];

/// A field for a header name (case and punctuation don't matter).
pub fn suggest(header: &str) -> Option<&'static str> {
    let h: String = header.to_lowercase().chars().filter(|c| c.is_alphanumeric()).collect();
    Some(match h.as_str() {
        "key" | "issuekey" | "id" | "issueid" | "number" => "key",
        "title" | "summary" | "subject" | "name" | "task" => "title",
        "description" | "body" | "details" | "notes" => "description",
        "type" | "issuetype" | "tracker" | "kind" => "type",
        "status" | "state" | "column" | "stage" => "status",
        "priority" | "severityprio" => "priority",
        "assignee" | "assignedto" | "owner" | "responsible" => "assignee",
        "reporter" | "author" | "createdby" | "requester" => "reporter",
        "labels" | "label" | "tags" | "tag" | "keywords" => "labels",
        "sprint" | "iteration" => "sprint",
        "version" | "fixversion" | "fixversions" | "release" | "milestone" | "targetversion" => "version",
        "component" | "components" | "category" | "area" => "component",
        "parent" | "parentkey" | "parentid" | "epic" | "epiclink" | "parenttask" => "parent",
        "points" | "storypoints" | "storypointestimate" | "estimatepoints" => "points",
        "estimate" | "estimateh" | "estimatedhours" | "originalestimate" | "estimatedtime" => "estimate",
        "spent" | "spenth" | "timespent" | "spenthours" | "loggedhours" => "spent",
        "start" | "startdate" | "begin" => "start",
        "due" | "duedate" | "deadline" | "enddate" => "due",
        "created" | "createdat" | "createddate" | "opened" => "created",
        "resolved" | "resolvedat" | "closed" | "closedat" | "completed" | "donedate" => "resolved",
        _ => return None,
    })
}

/// Splits CSV text (comma, semicolon or tab, chosen by the header line; BOM removed).
fn read_csv(text: &str) -> ApiResult<(Vec<String>, Vec<Vec<String>>)> {
    let text = text.trim_start_matches('\u{feff}');
    let first = text.lines().next().unwrap_or("");
    let delim = [b',', b';', b'\t'].into_iter().max_by_key(|d| first.matches(*d as char).count()).unwrap_or(b',');
    let mut r = csv::ReaderBuilder::new().delimiter(delim).flexible(true).from_reader(text.as_bytes());
    let headers: Vec<String> = r.headers().map_err(|e| ApiError::bad(format!("not a CSV file: {e}")))?.iter().map(|h| h.trim().to_string()).collect();
    if headers.iter().all(|h| h.is_empty()) {
        return Err(ApiError::bad("the first line must name the columns"));
    }
    let mut rows = Vec::new();
    for rec in r.records() {
        let rec = rec.map_err(|e| ApiError::bad(format!("CSV problem: {e}")))?;
        if rec.iter().all(|c| c.trim().is_empty()) {
            continue;
        }
        rows.push(rec.iter().map(|c| c.to_string()).collect());
        if rows.len() > MAX_ROWS {
            return Err(ApiError::bad(format!("at most {MAX_ROWS} rows per import")));
        }
    }
    Ok((headers, rows))
}

/// Dates in the shapes exports use: 2026-11-12, 12.11.2026, 11/12/2026, 12/Nov/26 10:30 AM,
/// RFC 3339.
pub fn parse_date(s: &str) -> Option<DateTime<Utc>> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if let Ok(t) = DateTime::parse_from_rfc3339(s) {
        return Some(t.with_timezone(&Utc));
    }
    for f in ["%Y-%m-%d %H:%M:%S", "%Y-%m-%d %H:%M", "%d.%m.%Y %H:%M", "%d/%b/%y %I:%M %p", "%d/%b/%Y %I:%M %p", "%m/%d/%Y %H:%M", "%Y-%m-%dT%H:%M:%S"] {
        if let Ok(t) = NaiveDateTime::parse_from_str(s, f) {
            return Some(t.and_utc());
        }
    }
    for f in ["%Y-%m-%d", "%d.%m.%Y", "%m/%d/%Y", "%d/%b/%y", "%d/%b/%Y", "%Y/%m/%d", "%d %b %Y", "%b %d, %Y"] {
        if let Ok(d) = NaiveDate::parse_from_str(s, f) {
            return d.and_hms_opt(0, 0, 0).map(|t| t.and_utc());
        }
    }
    None
}

fn number(s: &str) -> Option<f64> {
    let s = s.trim().replace(',', ".");
    s.parse::<f64>().ok().filter(|n| n.is_finite() && *n >= 0.0)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportReq {
    csv: String,
    /// column header → field (`title`, `status`, …, `custom:<id>`, `ignore`); missing = suggested
    #[serde(default)]
    mapping: HashMap<String, String>,
    #[serde(default)]
    dry_run: bool,
    /// create sprints, releases and components that don't exist yet
    #[serde(default = "yes")]
    create_missing: bool,
}

fn yes() -> bool {
    true
}

/// Headers, a few rows and the suggested mapping.
pub async fn preview(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(key): Path<String>, Json(req): Json<Value>) -> ApiResult<Json<Value>> {
    let (p, _) = by_key(&state, &user, &key, Role::Admin).await?;
    let (headers, rows) = read_csv(req["csv"].as_str().unwrap_or(""))?;
    let fields: Vec<(Uuid, String)> = sqlx::query_as("SELECT id, name FROM custom_fields WHERE project_id = $1").bind(p.id).fetch_all(&state.db).await?;
    let mut used = std::collections::HashSet::new();
    let mapping: Map<String, Value> = headers
        .iter()
        .map(|h| {
            let custom = fields.iter().find(|(_, n)| n.eq_ignore_ascii_case(h.trim())).map(|(id, _)| format!("custom:{id}"));
            let f = custom.or_else(|| suggest(h).map(str::to_string)).filter(|f| used.insert(f.clone())).unwrap_or_else(|| "ignore".into());
            (h.clone(), json!(f))
        })
        .collect();
    Ok(Json(json!({"headers": headers, "rows": rows.iter().take(5).collect::<Vec<_>>(), "total": rows.len(), "mapping": mapping, "fields": FIELDS})))
}

struct Lookup {
    statuses: Vec<(Uuid, String, String)>,
    types: Vec<(Uuid, String, i16)>,
    people: Vec<(Uuid, String, Option<String>)>,
    sprints: HashMap<String, Uuid>,
    versions: HashMap<String, Uuid>,
    components: HashMap<String, Uuid>,
    fields: HashMap<Uuid, (String, String, Value)>,
}

impl Lookup {
    fn status(&self, v: &str) -> Option<Uuid> {
        let l = v.trim().to_lowercase();
        if let Some(s) = self.statuses.iter().find(|s| s.1.to_lowercase() == l) {
            return Some(s.0);
        }
        // closed / resolved / in progress: the first status of that category
        let cat = parse_category(&l).or_else(|| {
            if ["resolved", "closed", "fixed", "won't fix", "wontfix", "rejected", "cancelled", "canceled", "verified"].contains(&l.as_str()) {
                Some("done")
            } else if l.contains("progress") || l.contains("review") || l.contains("testing") || l.contains("doing") {
                Some("in_progress")
            } else {
                None
            }
        })?;
        self.statuses.iter().find(|s| s.2 == cat).map(|s| s.0)
    }

    fn type_id(&self, v: &str) -> Option<Uuid> {
        let l = v.trim().to_lowercase();
        let alias = match l.as_str() {
            "subtask" | "sub-task" | "sub task" => Some(-1),
            "epic" | "milestone" | "initiative" => Some(1),
            _ => None,
        };
        self.types
            .iter()
            .find(|t| t.1.to_lowercase() == l)
            .or_else(|| alias.and_then(|lv| self.types.iter().find(|t| t.2 == lv)))
            .or_else(|| match l.as_str() {
                "defect" | "incident" | "problem" => self.types.iter().find(|t| t.1.eq_ignore_ascii_case("bug")),
                "feature" | "improvement" | "user story" | "enhancement" | "new feature" => {
                    self.types.iter().find(|t| t.1.eq_ignore_ascii_case("story") || t.1.eq_ignore_ascii_case("feature request"))
                }
                _ => None,
            })
            .map(|t| t.0)
    }

    fn person(&self, v: &str) -> Option<Uuid> {
        let l = v.trim().to_lowercase();
        if l.is_empty() {
            return None;
        }
        self.people.iter().find(|p| p.2.as_deref().is_some_and(|e| e.to_lowercase() == l) || p.1.to_lowercase() == l).map(|p| p.0)
    }
}

async fn ensure(state: &AppState, table: &str, project: Uuid, map: &mut HashMap<String, Uuid>, name: &str, create: bool, dry: bool) -> ApiResult<Option<Uuid>> {
    let l = name.trim().to_lowercase();
    if l.is_empty() {
        return Ok(None);
    }
    if let Some(id) = map.get(&l) {
        return Ok(Some(*id));
    }
    if !create {
        return Ok(None);
    }
    let id = new_id();
    if !dry {
        let sql = match table {
            "sprints" => "INSERT INTO sprints (id, project_id, name, state) VALUES ($1, $2, $3, 'future')",
            "versions" => "INSERT INTO versions (id, project_id, name) VALUES ($1, $2, $3)",
            _ => "INSERT INTO components (id, project_id, name) VALUES ($1, $2, $3)",
        };
        sqlx::query(sql).bind(id).bind(project).bind(name.trim()).execute(&state.db).await?;
    }
    map.insert(l, id);
    Ok(Some(id))
}

/// Imports (or with `dryRun` checks) the rows.
pub async fn run(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(key): Path<String>, Json(req): Json<ImportReq>) -> ApiResult<Json<Value>> {
    let (p, _) = by_key(&state, &user, &key, Role::Admin).await?;
    if p.archived_at.is_some() {
        return Err(ApiError::bad("the project is archived"));
    }
    let (headers, rows) = read_csv(&req.csv)?;
    let mapping: Vec<String> =
        headers.iter().map(|h| req.mapping.get(h).cloned().unwrap_or_else(|| suggest(h).map(str::to_string).unwrap_or_else(|| "ignore".into()))).collect();
    for m in &mapping {
        if m != "ignore" && !FIELDS.contains(&m.as_str()) && !m.starts_with("custom:") {
            return Err(ApiError::bad(format!("unknown field {m}")));
        }
    }
    if !mapping.iter().any(|m| m == "title") {
        return Err(ApiError::bad("map a column to the title"));
    }
    let pairs = |table: &'static str| {
        let state = state.clone();
        async move {
            let rows: Vec<(Uuid, String)> =
                sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT id, name FROM {table} WHERE project_id = $1"))).bind(p.id).fetch_all(&state.db).await?;
            Ok::<_, ApiError>(rows.into_iter().map(|(id, n)| (n.to_lowercase(), id)).collect::<HashMap<_, _>>())
        }
    };
    let mut lookup = Lookup {
        statuses: sqlx::query_as("SELECT id, name, category FROM statuses WHERE project_id = $1 ORDER BY position").bind(p.id).fetch_all(&state.db).await?,
        types: sqlx::query_as("SELECT id, name, level FROM issue_types WHERE project_id = $1 ORDER BY position").bind(p.id).fetch_all(&state.db).await?,
        people: sqlx::query_as("SELECT id, name, email FROM users WHERE workspace_id = $1 AND status <> 'deactivated'")
            .bind(user.workspace_id)
            .fetch_all(&state.db)
            .await?,
        sprints: pairs("sprints").await?,
        versions: pairs("versions").await?,
        components: pairs("components").await?,
        fields: sqlx::query_as::<_, (Uuid, String, String, Value)>("SELECT id, name, kind, options FROM custom_fields WHERE project_id = $1")
            .bind(p.id)
            .fetch_all(&state.db)
            .await?
            .into_iter()
            .map(|(id, n, k, o)| (id, (n, k, o)))
            .collect(),
    };
    let actor = Actor::quiet(&user);
    let dry = req.dry_run;
    let mut errors: Vec<Value> = Vec::new();
    let mut warnings: Vec<Value> = Vec::new();
    let mut created: Vec<(Uuid, String)> = Vec::new();
    // source key → new issue (for parents and aliases)
    let mut by_source: HashMap<String, Uuid> = HashMap::new();
    let mut parents: Vec<(Uuid, String, usize)> = Vec::new();
    let col = |row: &Vec<String>, f: &str| -> Vec<String> {
        mapping
            .iter()
            .enumerate()
            .filter(|(_, m)| m.as_str() == f)
            .filter_map(|(i, _)| row.get(i).map(|v| v.trim().to_string()))
            .filter(|v| !v.is_empty())
            .collect()
    };
    for (n, row) in rows.iter().enumerate() {
        let line = n + 2;
        let one = |f: &str| col(row, f).into_iter().next();
        let Some(title) = one("title") else {
            errors.push(json!({"row": line, "message": "no title"}));
            continue;
        };
        let mut fields = Map::new();
        let mut warn = |m: String| warnings.push(json!({"row": line, "message": m}));
        if let Some(v) = one("type") {
            match lookup.type_id(&v) {
                Some(id) => {
                    fields.insert("typeId".into(), json!(id));
                }
                None => warn(format!("unknown type “{v}”: the default type is used")),
            }
        }
        let status = one("status").map(|v| (lookup.status(&v), v));
        if let Some((None, v)) = &status {
            warn(format!("unknown status “{v}”: the first status is used"));
        }
        if let Some(v) = one("priority") {
            match parse_priority(&v) {
                Some(pr) => {
                    fields.insert("priority".into(), json!(pr));
                }
                None => warn(format!("unknown priority “{v}”")),
            }
        }
        for (f, k) in [("assignee", "assigneeId"), ("reporter", "reporterId")] {
            if let Some(v) = one(f) {
                match lookup.person(&v) {
                    Some(id) => {
                        fields.insert(k.into(), json!(id));
                    }
                    None => warn(format!("no one called “{v}” in this workspace ({f} left empty)")),
                }
            }
        }
        let labels: Vec<String> = col(row, "labels")
            .iter()
            .flat_map(|v| v.split([',', ';', ' ', '|']).map(str::to_string).collect::<Vec<_>>())
            .filter(|l| !l.trim().is_empty())
            .collect();
        if !labels.is_empty() {
            fields.insert("labels".into(), json!(labels));
        }
        if let Some(v) = one("sprint")
            && let Some(id) = ensure(&state, "sprints", p.id, &mut lookup.sprints, &v, req.create_missing, dry).await?
        {
            fields.insert("sprintId".into(), json!(id));
        }
        if let Some(v) = one("version")
            && let Some(id) = ensure(&state, "versions", p.id, &mut lookup.versions, &v, req.create_missing, dry).await?
        {
            fields.insert("versionId".into(), json!(id));
        }
        if let Some(v) = one("component")
            && let Some(id) = ensure(&state, "components", p.id, &mut lookup.components, &v, req.create_missing, dry).await?
        {
            fields.insert("componentId".into(), json!(id));
        }
        for (f, k, scale) in [("points", "storyPoints", 1.0), ("estimate", "estimateMinutes", 60.0)] {
            if let Some(v) = one(f) {
                match number(&v) {
                    Some(x) => {
                        fields.insert(k.into(), json!(x * scale));
                    }
                    None => warn(format!("{f} “{v}” isn't a number")),
                }
            }
        }
        for (f, k) in [("start", "startDate"), ("due", "dueDate")] {
            if let Some(v) = one(f) {
                match parse_date(&v) {
                    Some(d) => {
                        fields.insert(k.into(), json!(d.date_naive()));
                    }
                    None => warn(format!("{f} date “{v}” isn't a date")),
                }
            }
        }
        let mut custom = Map::new();
        for (i, m) in mapping.iter().enumerate() {
            let Some(fid) = m.strip_prefix("custom:").and_then(|s| s.parse::<Uuid>().ok()) else { continue };
            let Some((name, kind, options)) = lookup.fields.get(&fid) else { continue };
            let v = row.get(i).map(|s| s.trim()).unwrap_or("");
            if v.is_empty() {
                continue;
            }
            let opt = |label: &str| {
                options
                    .as_array()
                    .and_then(|o| o.iter().find(|x| x["label"].as_str().is_some_and(|l| l.eq_ignore_ascii_case(label.trim()))))
                    .and_then(|x| x["id"].as_str())
                    .map(str::to_string)
            };
            let val = match kind.as_str() {
                "number" => number(v).map(|x| json!(x)),
                "date" => parse_date(v).map(|d| json!(d.date_naive())),
                "checkbox" => Some(json!(matches!(v.to_lowercase().as_str(), "true" | "yes" | "1" | "x" | "ja" | "y"))),
                "select" => opt(v).map(|o| json!(o)),
                "multiselect" => Some(json!(v.split([',', ';']).filter_map(opt).collect::<Vec<_>>())),
                "user" => lookup.person(v).map(|u| json!(u.to_string())),
                _ => Some(json!(v)),
            };
            match val {
                Some(x) => {
                    custom.insert(fid.to_string(), x);
                }
                None => warn(format!("“{v}” doesn't fit the field {name}")),
            }
        }
        if !custom.is_empty() {
            fields.insert("custom".into(), Value::Object(custom));
        }
        let description = one("description").map(|d| crate::doc::from_markdown(&d));
        let source_key = one("key");
        if dry {
            if let Some(k) = &source_key {
                by_source.insert(k.to_uppercase(), Uuid::nil());
            }
            if let Some(parent) = one("parent") {
                parents.push((Uuid::nil(), parent.to_uppercase(), line));
            }
            continue;
        }
        let s = match issues::create(&state, &actor, NewIssue { project: p.id, title, description, fields }).await {
            Ok(s) => s,
            Err(e) => {
                errors.push(json!({"row": line, "message": e.to_string()}));
                continue;
            }
        };
        // status, dates and time spent as the source had them
        if let Some((Some(sid), _)) = status {
            let mut patch = Map::new();
            patch.insert("statusId".into(), json!(sid));
            if let Err(e) = issues::update(&state, &actor, s.id, &patch).await {
                warnings.push(json!({"row": line, "message": e.to_string()}));
            }
        }
        if let Some(c) = one("created").and_then(|v| parse_date(&v)) {
            sqlx::query("UPDATE issues SET created_at = $2 WHERE id = $1").bind(s.id).bind(c).execute(&state.db).await?;
            sqlx::query("UPDATE issue_events SET created_at = $2 WHERE issue_id = $1 AND kind = 'created'").bind(s.id).bind(c).execute(&state.db).await?;
        }
        if let Some(r) = one("resolved").and_then(|v| parse_date(&v)) {
            sqlx::query("UPDATE issues SET resolved_at = $2 WHERE id = $1 AND resolved_at IS NOT NULL").bind(s.id).bind(r).execute(&state.db).await?;
        }
        if let Some(h) = one("spent").and_then(|v| number(&v)).filter(|h| *h > 0.0) {
            sqlx::query("INSERT INTO worklogs (id, issue_id, user_id, minutes, work_date, note) VALUES ($1, $2, $3, $4, current_date, 'Imported')")
                .bind(new_id())
                .bind(s.id)
                .bind(user.id)
                .bind((h * 60.0).round() as i32)
                .execute(&state.db)
                .await?;
            sqlx::query("UPDATE issues SET spent_minutes = (SELECT COALESCE(sum(minutes), 0) FROM worklogs WHERE issue_id = $1) WHERE id = $1")
                .bind(s.id)
                .execute(&state.db)
                .await?;
        }
        if let Some(k) = source_key {
            let k = k.to_uppercase();
            if let Some((pk, num)) = split_key(&k)
                && pk != p.key.to_uppercase()
            {
                sqlx::query("INSERT INTO issue_key_aliases (workspace_id, project_key, number, issue_id) VALUES ($1, $2, $3, $4) ON CONFLICT DO NOTHING")
                    .bind(user.workspace_id)
                    .bind(&pk)
                    .bind(num)
                    .bind(s.id)
                    .execute(&state.db)
                    .await?;
            }
            by_source.insert(k, s.id);
        }
        if let Some(parent) = one("parent") {
            parents.push((s.id, parent.to_uppercase(), line));
        }
        created.push((s.id, s.key));
    }
    // parents once everything exists: a key from the file, else an issue already in TaskCraft
    for (child, parent, line) in parents.iter().filter(|_| !dry) {
        let target = match by_source.get(parent) {
            Some(id) => Some(*id),
            None => issues::resolve(&state, &user, parent, false).await.ok().map(|r| r.0),
        };
        match target {
            Some(t) => {
                let mut patch = Map::new();
                patch.insert("parentId".into(), json!(t));
                if let Err(e) = issues::update(&state, &actor, *child, &patch).await {
                    warnings.push(json!({"row": line, "message": format!("parent {parent}: {e}")}));
                }
            }
            None => warnings.push(json!({"row": line, "message": format!("parent {parent} not found")})),
        }
    }
    if dry {
        for (_, parent, line) in &parents {
            if !by_source.contains_key(parent) && issues::resolve(&state, &user, parent, false).await.is_err() {
                warnings.push(json!({"row": line, "message": format!("parent {parent} not found")}));
            }
        }
    } else if !created.is_empty() {
        audit(&state, user.workspace_id, Some(user.id), "project.import", Some(p.key.clone()), json!({"created": created.len(), "errors": errors.len()})).await;
        events::publish(&state, Event::new(user.workspace_id, "project").project(p.id).actor(user.id)).await;
        events::publish(&state, Event::new(user.workspace_id, "issue").project(p.id).actor(user.id)).await;
    }
    let count = if dry { rows.len() - errors.len() } else { created.len() };
    Ok(Json(json!({
        "dryRun": dry, "rows": rows.len(), "created": count, "errors": errors, "warnings": warnings,
        "keys": created.iter().map(|c| c.1.clone()).collect::<Vec<_>>(),
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suggestions_and_dates() {
        assert_eq!(suggest("Summary"), Some("title"));
        assert_eq!(suggest("Issue key"), Some("key"));
        assert_eq!(suggest("Story Points"), Some("points"));
        assert_eq!(suggest("Fix Version/s"), Some("version"));
        assert_eq!(suggest("Due date"), Some("due"));
        assert_eq!(suggest("Estimate (h)"), Some("estimate"));
        assert_eq!(suggest("Something else"), None);
        let d = |s: &str| parse_date(s).map(|d| d.format("%Y-%m-%d %H:%M").to_string());
        assert_eq!(d("2026-11-12").as_deref(), Some("2026-11-12 00:00"));
        assert_eq!(d("12.11.2026").as_deref(), Some("2026-11-12 00:00"));
        assert_eq!(d("11/12/2026").as_deref(), Some("2026-11-12 00:00"));
        assert_eq!(d("12/Nov/26 2:30 PM").as_deref(), Some("2026-11-12 14:30"));
        assert_eq!(d("2026-11-12T08:00:00Z").as_deref(), Some("2026-11-12 08:00"));
        assert_eq!(d("2026-11-12 08:15").as_deref(), Some("2026-11-12 08:15"));
        assert_eq!(d("soon"), None);
    }

    #[test]
    fn csv_shapes() {
        let (h, rows) = read_csv("\u{feff}Summary;Status;Labels\nFix login;Done;a b\n;;\n\"Multi\nline\";Open;\n").unwrap();
        assert_eq!(h, vec!["Summary", "Status", "Labels"]);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1][0], "Multi\nline");
        assert!(read_csv("").is_err());
    }
}
