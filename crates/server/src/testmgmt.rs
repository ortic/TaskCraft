//! Test management: what was checked before a release ships.
//!
//! A project has **test cases** (`WEB-T12`): a title, preconditions, numbered steps with an
//! expected result each — or a Given / When / Then scenario —, a priority, labels and a folder.
//! Cases are versioned: every change of what a tester does makes a new version, and a run
//! records the version it executed. A case **covers** issues; an issue shows its cases and
//! their latest results. **Test plans** are named sets of cases, picked by hand or by a filter,
//! optionally for a release; **run templates** add an environment and testers. A **test run**
//! executes cases: each step gets passed / failed / blocked / skipped with a comment and files,
//! and a failure becomes a bug in one click. Results are also kept as history
//! (`test_result_events`), which the reports replay: pass rate over time, flaky cases, and the
//! coverage of a release.
//!
//! Who may: project viewers read; `executeTests` and `manageTests` (project permissions, members
//! by default) run tests and change cases, plans and runs.

use std::collections::{BTreeMap, HashMap, HashSet};

use axum::Json;
use axum::extract::{Multipart, Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, NaiveDate, Utc};
use object_store::ObjectStoreExt;
use object_store::path::Path as ObjPath;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use uuid::Uuid;

use crate::access::{self, Role};
use crate::app::{AppState, new_id};
use crate::auth::{CurrentUser, User};
use crate::error::{ApiError, ApiResult};
use crate::events::{self, Event};
use crate::issues::Actor;
use crate::routes::projects::{ProjectRow, by_key};

pub const RESULTS: [&str; 5] = ["untested", "passed", "failed", "blocked", "skipped"];
const PRIORITIES: [&str; 4] = ["low", "medium", "high", "critical"];

// ---- who may ----------------------------------------------------------------------------------------

async fn on(state: &AppState, user: &User) -> ApiResult<()> {
    crate::features::require(state, user.workspace_id, "tests").await
}

/// A project of the caller's, while the workspace uses test management.
async fn project(state: &AppState, user: &User, key: &str) -> ApiResult<(ProjectRow, Role)> {
    on(state, user).await?;
    by_key(state, user, key, Role::Viewer).await
}

/// The caller's role in a project they see, with the feature on.
async fn role_in(state: &AppState, user: &User, project: Uuid) -> ApiResult<Role> {
    on(state, user).await?;
    access::require(state, user, project, Role::Viewer).await
}

async fn may(state: &AppState, project: Uuid, role: Role, action: &str) -> bool {
    access::require_action(state, project, role, action).await.is_ok()
}

async fn manage(state: &AppState, project: Uuid, role: Role) -> ApiResult<()> {
    access::require_action(state, project, role, "manageTests").await
}

async fn execute(state: &AppState, project: Uuid, role: Role) -> ApiResult<()> {
    access::require_action(state, project, role, "executeTests").await
}

fn changed(state: &AppState, user: &User, project: Uuid) -> impl Future<Output = ()> {
    events::publish(state, Event::new(user.workspace_id, "tests").project(project).actor(user.id))
}

// ---- test cases -------------------------------------------------------------------------------------

#[derive(sqlx::FromRow)]
struct CaseRow {
    id: Uuid,
    project_id: Uuid,
    project_key: String,
    number: i32,
    folder_id: Option<Uuid>,
    title: String,
    preconditions: Option<String>,
    kind: String,
    steps: Value,
    scenario: Option<String>,
    priority: String,
    labels: Vec<String>,
    version: i32,
    archived_at: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

const CASE_SQL: &str = "SELECT c.id, c.project_id, p.key AS project_key, c.number, c.folder_id, c.title, c.preconditions, c.kind, c.steps, c.scenario,
        c.priority, c.labels, c.version, c.archived_at, c.created_at, c.updated_at
   FROM test_cases c JOIN projects p ON p.id = c.project_id";

fn case_key(project: &str, number: i32) -> String {
    format!("{project}-T{number}")
}

/// A test case by id or key (`WEB-T12`) in a project the caller sees.
async fn case_for(state: &AppState, user: &User, r: &str) -> ApiResult<(CaseRow, Role)> {
    on(state, user).await?;
    let row: Option<CaseRow> = if let Ok(id) = r.parse::<Uuid>() {
        sqlx::query_as(sqlx::AssertSqlSafe(format!("{CASE_SQL} WHERE c.id = $1 AND p.workspace_id = $2")))
            .bind(id)
            .bind(user.workspace_id)
            .fetch_optional(&state.db)
            .await?
    } else {
        let upper = r.trim().to_uppercase();
        let Some((key, number)) = upper.rsplit_once("-T").and_then(|(k, n)| Some((k.to_string(), n.parse::<i32>().ok()?))) else {
            return Err(ApiError::NotFound("test case"));
        };
        sqlx::query_as(sqlx::AssertSqlSafe(format!("{CASE_SQL} WHERE upper(p.key) = $1 AND c.number = $2 AND p.workspace_id = $3")))
            .bind(key)
            .bind(number)
            .bind(user.workspace_id)
            .fetch_optional(&state.db)
            .await?
    };
    let row = row.ok_or(ApiError::NotFound("test case"))?;
    let role = access::role(state, user, row.project_id).await?;
    if role == Role::None {
        return Err(ApiError::NotFound("test case"));
    }
    Ok((row, role))
}

fn text(v: &Value, what: &str, max: usize) -> ApiResult<Option<String>> {
    match v {
        Value::Null => Ok(None),
        Value::String(s) if s.chars().count() <= max => Ok(Some(s.trim().to_string()).filter(|s| !s.is_empty())),
        Value::String(_) => Err(ApiError::bad(format!("{what} is at most {max} characters"))),
        _ => Err(ApiError::bad(format!("{what} must be text"))),
    }
}

/// `[{action, expected}]`, tidied.
fn clean_steps(v: &Value) -> ApiResult<Value> {
    let list = v.as_array().ok_or_else(|| ApiError::bad("steps is a list of {action, expected}"))?;
    if list.len() > 100 {
        return Err(ApiError::bad("a test case has at most 100 steps"));
    }
    let mut out = Vec::new();
    for s in list {
        let action = text(&s["action"], "a step", 5000)?.ok_or_else(|| ApiError::bad("every step says what to do (action)"))?;
        let expected = text(&s["expected"], "an expected result", 5000)?.unwrap_or_default();
        out.push(json!({"action": action, "expected": expected}));
    }
    Ok(json!(out))
}

/// The steps a tester goes through: the case's steps, or the scenario's Given / When / Then lines.
fn steps_of(kind: &str, steps: &Value, scenario: Option<&str>) -> Vec<Value> {
    if kind == "scenario" {
        scenario
            .unwrap_or("")
            .lines()
            .map(str::trim)
            .filter(|l| matches!(l.split_whitespace().next().unwrap_or("").to_lowercase().as_str(), "given" | "when" | "then" | "and" | "but"))
            .map(|l| json!({"action": l, "expected": ""}))
            .collect()
    } else {
        steps.as_array().cloned().unwrap_or_default()
    }
}

/// The Markdown of a case as rich text, for showing: `{preconditions, steps: [{action, expected}]}`.
fn rendered(preconditions: Option<&str>, steps: &[Value]) -> Value {
    let doc = |v: &Value| v.as_str().filter(|s| !s.is_empty()).map(crate::doc::from_markdown);
    json!({
        "preconditions": preconditions.map(crate::doc::from_markdown),
        "steps": steps.iter().map(|s| json!({"action": doc(&s["action"]), "expected": doc(&s["expected"])})).collect::<Vec<_>>(),
    })
}

fn clean_labels(v: &Value) -> ApiResult<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for l in v.as_array().ok_or_else(|| ApiError::bad("labels is a list of names"))? {
        let l = l.as_str().unwrap_or("").trim().to_string();
        if l.is_empty() || l.chars().count() > 40 || l.contains(',') {
            return Err(ApiError::bad("a label is 1 to 40 characters without commas"));
        }
        if !out.iter().any(|o| o.eq_ignore_ascii_case(&l)) {
            out.push(l);
        }
    }
    if out.len() > 20 {
        return Err(ApiError::bad("at most 20 labels"));
    }
    Ok(out)
}

fn clean_priority(v: &Value) -> ApiResult<String> {
    let p = v.as_str().unwrap_or("").to_lowercase();
    if !PRIORITIES.contains(&p.as_str()) {
        return Err(ApiError::bad("priority is low, medium, high or critical"));
    }
    Ok(p)
}

async fn folder_in(state: &AppState, project: Uuid, v: &Value) -> ApiResult<Option<Uuid>> {
    if v.is_null() {
        return Ok(None);
    }
    let id = v.as_str().and_then(|s| s.parse::<Uuid>().ok()).ok_or_else(|| ApiError::bad("folderId must be a folder's id"))?;
    let found: Option<Uuid> =
        sqlx::query_scalar("SELECT id FROM test_folders WHERE id = $1 AND project_id = $2").bind(id).bind(project).fetch_optional(&state.db).await?;
    Ok(Some(found.ok_or_else(|| ApiError::bad("there is no such folder in this project"))?))
}

/// What a test case holds, checked: (title, preconditions, kind, steps, scenario).
struct Content {
    title: String,
    preconditions: Option<String>,
    kind: String,
    steps: Value,
    scenario: Option<String>,
}

fn content(req: &Map<String, Value>, old: Option<&CaseRow>) -> ApiResult<Content> {
    let title = match req.get("title") {
        Some(v) => text(v, "the title", 300)?.ok_or_else(|| ApiError::bad("a test case needs a title"))?,
        None => old.map(|o| o.title.clone()).ok_or_else(|| ApiError::bad("a test case needs a title"))?,
    };
    let preconditions = match req.get("preconditions") {
        Some(v) => text(v, "the preconditions", 20_000)?,
        None => old.and_then(|o| o.preconditions.clone()),
    };
    let scenario = match req.get("scenario") {
        Some(v) => text(v, "the scenario", 20_000)?,
        None => old.and_then(|o| o.scenario.clone()),
    };
    let steps = match req.get("steps") {
        Some(v) => clean_steps(v)?,
        None => old.map(|o| o.steps.clone()).unwrap_or_else(|| json!([])),
    };
    let kind = match req.get("kind").map(|k| k.as_str().unwrap_or("")) {
        Some(k @ ("steps" | "scenario")) => k.to_string(),
        Some(_) => return Err(ApiError::bad("kind is steps or scenario")),
        None => old
            .map(|o| o.kind.clone())
            .unwrap_or_else(|| if scenario.is_some() && steps.as_array().is_none_or(Vec::is_empty) { "scenario".into() } else { "steps".into() }),
    };
    if kind == "scenario" && steps_of(&kind, &steps, scenario.as_deref()).is_empty() {
        return Err(ApiError::bad("a scenario has lines that start with Given, When or Then"));
    }
    Ok(Content { title, preconditions, kind, steps, scenario })
}

/// Creates a test case (the caller checked who may).
pub async fn new_case(state: &AppState, user: Option<Uuid>, project: Uuid, req: &Map<String, Value>) -> ApiResult<(Uuid, i32)> {
    let c = content(req, None)?;
    let priority = match req.get("priority") {
        Some(v) if !v.is_null() => clean_priority(v)?,
        _ => "medium".into(),
    };
    let labels = match req.get("labels") {
        Some(v) if !v.is_null() => clean_labels(v)?,
        _ => vec![],
    };
    let folder = folder_in(state, project, req.get("folderId").unwrap_or(&Value::Null)).await?;
    let mut tx = state.db.begin().await?;
    let number: i32 = sqlx::query_scalar("UPDATE projects SET next_test_number = next_test_number + 1 WHERE id = $1 RETURNING next_test_number - 1")
        .bind(project)
        .fetch_one(&mut *tx)
        .await?;
    let id = new_id();
    sqlx::query(
        "INSERT INTO test_cases (id, project_id, number, folder_id, title, preconditions, kind, steps, scenario, priority, labels, created_by)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
    )
    .bind(id)
    .bind(project)
    .bind(number)
    .bind(folder)
    .bind(&c.title)
    .bind(&c.preconditions)
    .bind(&c.kind)
    .bind(&c.steps)
    .bind(&c.scenario)
    .bind(&priority)
    .bind(&labels)
    .bind(user)
    .execute(&mut *tx)
    .await?;
    version_row(&mut tx, id, 1, &c, user).await?;
    tx.commit().await?;
    Ok((id, number))
}

async fn version_row(tx: &mut sqlx::Transaction<'_, sqlx::Postgres>, case: Uuid, version: i32, c: &Content, user: Option<Uuid>) -> ApiResult<()> {
    sqlx::query(
        "INSERT INTO test_case_versions (case_id, version, title, preconditions, kind, steps, scenario, created_by) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(case)
    .bind(version)
    .bind(&c.title)
    .bind(&c.preconditions)
    .bind(&c.kind)
    .bind(&c.steps)
    .bind(&c.scenario)
    .bind(user)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Sets the issues a case covers (ids or keys the caller sees).
async fn set_coverage(state: &AppState, user: &User, case: Uuid, v: &Value) -> ApiResult<()> {
    let ids = coverage_ids(state, user, v).await?;
    let mut tx = state.db.begin().await?;
    // the ones the caller can't see stay as they are
    let kept: Vec<Uuid> = sqlx::query_scalar("SELECT issue_id FROM test_case_issues WHERE case_id = $1").bind(case).fetch_all(&mut *tx).await?;
    let visible = access::visible_issues(state, user.id, &kept).await?;
    let gone: Vec<Uuid> = kept.into_iter().filter(|i| visible.contains(i) && !ids.contains(i)).collect();
    sqlx::query("DELETE FROM test_case_issues WHERE case_id = $1 AND issue_id = ANY($2)").bind(case).bind(&gone).execute(&mut *tx).await?;
    sqlx::query("INSERT INTO test_case_issues (case_id, issue_id) SELECT $1, unnest($2::uuid[]) ON CONFLICT DO NOTHING")
        .bind(case)
        .bind(&ids)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

/// The issues named in a coverage list (ids or keys the caller sees).
async fn coverage_ids(state: &AppState, user: &User, v: &Value) -> ApiResult<Vec<Uuid>> {
    let list = v.as_array().ok_or_else(|| ApiError::bad("issues is a list of issue keys"))?;
    if list.len() > 200 {
        return Err(ApiError::bad("a test case covers at most 200 issues"));
    }
    let mut ids = Vec::new();
    for k in list {
        let key = k.as_str().ok_or_else(|| ApiError::bad("issues is a list of issue keys"))?;
        let (id, _, _) = crate::issues::resolve(state, user, key.trim(), false).await.map_err(|_| ApiError::bad(format!("there is no issue {key}")))?;
        ids.push(id);
    }
    Ok(ids)
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct CaseFilter {
    /// a folder, with the ones below it
    pub folder: Option<Uuid>,
    /// comma-separated: any of them
    pub label: Option<String>,
    pub priority: Option<String>,
    /// text in the title, the key or the steps
    pub q: Option<String>,
    /// also archived cases
    pub archived: Option<bool>,
}

impl CaseFilter {
    /// A plan's stored filter: `{folderId, labels: [], priority, text}`.
    fn from_plan(v: &Value) -> CaseFilter {
        CaseFilter {
            folder: v["folderId"].as_str().and_then(|s| s.parse().ok()),
            label: v["labels"].as_array().map(|l| l.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(",")).filter(|s| !s.is_empty()),
            priority: v["priority"].as_str().map(str::to_string),
            q: v["text"].as_str().map(str::to_string),
            archived: None,
        }
    }
}

#[derive(sqlx::FromRow)]
struct CaseLine {
    id: Uuid,
    number: i32,
    title: String,
    folder_id: Option<Uuid>,
    priority: String,
    labels: Vec<String>,
    version: i32,
    kind: String,
    archived_at: Option<DateTime<Utc>>,
    updated_at: DateTime<Utc>,
    issues: i64,
    last_result: Option<String>,
    last_at: Option<DateTime<Utc>>,
}

const LATEST: &str = "LEFT JOIN LATERAL (SELECT rc.result, rc.executed_at, rc.run_id FROM test_run_cases rc
        WHERE rc.case_id = c.id AND rc.result <> 'untested' ORDER BY rc.executed_at DESC NULLS LAST LIMIT 1) l ON true";

async fn lines(state: &AppState, project: Uuid, f: &CaseFilter, only: Option<&[Uuid]>) -> ApiResult<Vec<CaseLine>> {
    let labels: Option<Vec<String>> =
        f.label.as_deref().map(|l| l.split(',').map(|s| s.trim().to_lowercase()).filter(|s| !s.is_empty()).collect::<Vec<_>>()).filter(|l| !l.is_empty());
    let q = f.q.as_deref().map(|s| s.trim().to_lowercase()).filter(|s| !s.is_empty());
    Ok(sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT c.id, c.number, c.title, c.folder_id, c.priority, c.labels, c.version, c.kind, c.archived_at, c.updated_at,
                (SELECT count(*) FROM test_case_issues ci WHERE ci.case_id = c.id) AS issues, l.result AS last_result, l.executed_at AS last_at
           FROM test_cases c JOIN projects p ON p.id = c.project_id {LATEST}
          WHERE c.project_id = $1
            AND ($2::uuid IS NULL OR c.folder_id IN (WITH RECURSIVE f AS (SELECT $2::uuid AS id UNION ALL SELECT tf.id FROM test_folders tf JOIN f ON tf.parent_id = f.id) SELECT id FROM f))
            AND ($3::text[] IS NULL OR EXISTS (SELECT 1 FROM unnest(c.labels) x WHERE lower(x) = ANY($3)))
            AND ($4::text IS NULL OR c.priority = $4)
            AND ($5::text IS NULL OR position($5 in lower(c.title)) > 0 OR position($5 in lower(c.steps::text)) > 0 OR position($5 in lower(COALESCE(c.scenario, ''))) > 0
                 OR lower(p.key || '-T' || c.number) = $5)
            AND ($6 OR c.archived_at IS NULL)
            AND ($7::uuid[] IS NULL OR c.id = ANY($7))
          ORDER BY c.number LIMIT 5000"
    )))
    .bind(project)
    .bind(f.folder)
    .bind(labels)
    .bind(f.priority.as_deref().map(str::to_lowercase))
    .bind(q)
    .bind(f.archived.unwrap_or(false) || only.is_some())
    .bind(only)
    .fetch_all(&state.db)
    .await?)
}

fn line_json(key: &str, c: &CaseLine) -> Value {
    json!({
        "id": c.id, "key": case_key(key, c.number), "title": c.title, "folderId": c.folder_id, "priority": c.priority, "labels": c.labels,
        "version": c.version, "kind": c.kind, "archived": c.archived_at.is_some(), "updatedAt": c.updated_at, "issueCount": c.issues,
        "lastResult": c.last_result, "lastRunAt": c.last_at,
    })
}

/// `GET /projects/{key}/tests`: the project's folders and test cases.
pub async fn list(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Path(key): Path<String>,
    Query(f): Query<CaseFilter>,
) -> ApiResult<Json<Value>> {
    let (p, role) = project(&state, &user, &key).await?;
    let folders: Vec<(Uuid, Option<Uuid>, String)> =
        sqlx::query_as("SELECT id, parent_id, name FROM test_folders WHERE project_id = $1 ORDER BY position, lower(name)")
            .bind(p.id)
            .fetch_all(&state.db)
            .await?;
    let cases = lines(&state, p.id, &f, None).await?;
    Ok(Json(json!({
        "canManage": may(&state, p.id, role, "manageTests").await,
        "canExecute": may(&state, p.id, role, "executeTests").await,
        "folders": folders.into_iter().map(|(id, parent, name)| json!({"id": id, "parentId": parent, "name": name})).collect::<Vec<_>>(),
        "cases": cases.iter().map(|c| line_json(&p.key, c)).collect::<Vec<_>>(),
    })))
}

/// `POST /projects/{key}/tests` `{title, preconditions?, steps? | scenario?, priority?, labels?, folderId?, issues?}`.
pub async fn create(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Path(key): Path<String>,
    Json(req): Json<Map<String, Value>>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let (p, role) = project(&state, &user, &key).await?;
    manage(&state, p.id, role).await?;
    if let Some(issues) = req.get("issues").filter(|v| !v.is_null()) {
        coverage_ids(&state, &user, issues).await?;
    }
    let (id, number) = new_case(&state, Some(user.id), p.id, &req).await?;
    if let Some(issues) = req.get("issues").filter(|v| !v.is_null()) {
        set_coverage(&state, &user, id, issues).await?;
    }
    changed(&state, &user, p.id).await;
    Ok((StatusCode::CREATED, Json(json!({"id": id, "key": case_key(&p.key, number)}))))
}

async fn issue_lines(state: &AppState, user: &User, sql_where: &str, id: Uuid) -> ApiResult<Vec<Value>> {
    let rows: Vec<(Uuid, String, String, String, String)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT i.id, p.key || '-' || i.number, i.title, s.name, s.category
           FROM issues i JOIN projects p ON p.id = i.project_id JOIN statuses s ON s.id = i.status_id
           JOIN tc_project_roles($2) r ON r.project_id = i.project_id AND r.rank >= 1
          WHERE i.deleted_at IS NULL AND {} AND {sql_where} ORDER BY p.key, i.number LIMIT 500",
        access::sees_issue("i", "$2")
    )))
    .bind(id)
    .bind(user.id)
    .fetch_all(&state.db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(id, key, title, status, category)| json!({"id": id, "key": key, "title": title, "status": {"name": status, "category": category}}))
        .collect())
}

/// `GET /tests/{id}`: a test case with its steps, the issues it covers, its versions and its
/// latest results.
pub async fn get(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(r): Path<String>) -> ApiResult<Json<Value>> {
    let (c, role) = case_for(&state, &user, &r).await?;
    let issues = issue_lines(&state, &user, "i.id IN (SELECT issue_id FROM test_case_issues WHERE case_id = $1)", c.id).await?;
    let versions: Vec<(i32, DateTime<Utc>, Option<String>)> = sqlx::query_as(
        "SELECT v.version, v.created_at, u.name FROM test_case_versions v LEFT JOIN users u ON u.id = v.created_by WHERE v.case_id = $1 ORDER BY v.version DESC",
    )
    .bind(c.id)
    .fetch_all(&state.db)
    .await?;
    let results: Vec<(Uuid, i32, String, String, i32, Option<DateTime<Utc>>, Option<String>)> = sqlx::query_as(
        "SELECT r.id, r.number, r.name, rc.result, rc.case_version, rc.executed_at, u.name
           FROM test_run_cases rc JOIN test_runs r ON r.id = rc.run_id LEFT JOIN users u ON u.id = rc.executed_by
          WHERE rc.case_id = $1 ORDER BY COALESCE(rc.executed_at, r.created_at) DESC LIMIT 30",
    )
    .bind(c.id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(json!({
        "id": c.id, "key": case_key(&c.project_key, c.number), "projectKey": c.project_key, "title": c.title, "preconditions": c.preconditions,
        "kind": c.kind, "steps": c.steps, "scenario": c.scenario,
        "rendered": rendered(c.preconditions.as_deref(), &steps_of(&c.kind, &c.steps, c.scenario.as_deref())), "priority": c.priority, "labels": c.labels, "folderId": c.folder_id,
        "version": c.version, "archived": c.archived_at.is_some(), "createdAt": c.created_at, "updatedAt": c.updated_at,
        "canManage": may(&state, c.project_id, role, "manageTests").await,
        "issues": issues,
        "versions": versions.into_iter().map(|(v, at, by)| json!({"version": v, "at": at, "by": by})).collect::<Vec<_>>(),
        "results": results.into_iter().map(|(run, n, name, result, v, at, by)| json!({"run": {"id": run, "number": n, "name": name}, "result": result, "version": v, "at": at, "by": by})).collect::<Vec<_>>(),
    })))
}

/// `GET /tests/{id}/versions/{version}`: what the case said then.
pub async fn version(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path((r, n)): Path<(String, i32)>) -> ApiResult<Json<Value>> {
    let (c, _) = case_for(&state, &user, &r).await?;
    let v: Option<(String, Option<String>, String, Value, Option<String>, DateTime<Utc>)> =
        sqlx::query_as("SELECT title, preconditions, kind, steps, scenario, created_at FROM test_case_versions WHERE case_id = $1 AND version = $2")
            .bind(c.id)
            .bind(n)
            .fetch_optional(&state.db)
            .await?;
    let (title, preconditions, kind, steps, scenario, at) = v.ok_or(ApiError::NotFound("version"))?;
    Ok(Json(json!({"version": n, "title": title, "preconditions": preconditions, "kind": kind, "steps": steps, "scenario": scenario, "at": at})))
}

/// `PATCH /tests/{id}`: changes a case. A change of the title, the preconditions, the steps or
/// the scenario makes a new version; runs keep the version they executed.
pub async fn update(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Path(r): Path<String>,
    Json(req): Json<Map<String, Value>>,
) -> ApiResult<Json<Value>> {
    let (c, role) = case_for(&state, &user, &r).await?;
    manage(&state, c.project_id, role).await?;
    let new = content(&req, Some(&c))?;
    if let Some(issues) = req.get("issues").filter(|v| !v.is_null()) {
        coverage_ids(&state, &user, issues).await?;
    }
    let priority = match req.get("priority") {
        Some(v) => clean_priority(v)?,
        None => c.priority.clone(),
    };
    let labels = match req.get("labels") {
        Some(v) => clean_labels(v)?,
        None => c.labels.clone(),
    };
    let folder = match req.get("folderId") {
        Some(v) => folder_in(&state, c.project_id, v).await?,
        None => c.folder_id,
    };
    let archived = match req.get("archived").and_then(Value::as_bool) {
        Some(true) => Some(c.archived_at.unwrap_or_else(Utc::now)),
        Some(false) => None,
        None => c.archived_at,
    };
    let differs = new.title != c.title || new.preconditions != c.preconditions || new.kind != c.kind || new.steps != c.steps || new.scenario != c.scenario;
    let version = c.version + i32::from(differs);
    let mut tx = state.db.begin().await?;
    sqlx::query(
        "UPDATE test_cases SET title = $2, preconditions = $3, kind = $4, steps = $5, scenario = $6, priority = $7, labels = $8, folder_id = $9,
                archived_at = $10, version = $11, updated_at = now() WHERE id = $1",
    )
    .bind(c.id)
    .bind(&new.title)
    .bind(&new.preconditions)
    .bind(&new.kind)
    .bind(&new.steps)
    .bind(&new.scenario)
    .bind(&priority)
    .bind(&labels)
    .bind(folder)
    .bind(archived)
    .bind(version)
    .execute(&mut *tx)
    .await?;
    if differs {
        version_row(&mut tx, c.id, version, &new, Some(user.id)).await?;
    }
    tx.commit().await?;
    if let Some(issues) = req.get("issues").filter(|v| !v.is_null()) {
        set_coverage(&state, &user, c.id, issues).await?;
    }
    changed(&state, &user, c.project_id).await;
    Ok(Json(json!({"version": version})))
}

/// `DELETE /tests/{id}`: deletes a case with its results (archive it to keep them).
pub async fn delete(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(r): Path<String>) -> ApiResult<StatusCode> {
    let (c, role) = case_for(&state, &user, &r).await?;
    manage(&state, c.project_id, role).await?;
    let keys: Vec<String> = sqlx::query_scalar("SELECT f.storage_key FROM test_files f JOIN test_run_cases rc ON rc.id = f.run_case_id WHERE rc.case_id = $1")
        .bind(c.id)
        .fetch_all(&state.db)
        .await?;
    sqlx::query("DELETE FROM test_cases WHERE id = $1").bind(c.id).execute(&state.db).await?;
    for k in keys {
        let _ = state.blobs.delete(&ObjPath::from(k)).await;
    }
    changed(&state, &user, c.project_id).await;
    Ok(StatusCode::NO_CONTENT)
}

// ---- folders ----------------------------------------------------------------------------------------

/// `POST /projects/{key}/test-folders` `{name, parentId?}`.
pub async fn create_folder(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Path(key): Path<String>,
    Json(req): Json<Value>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let (p, role) = project(&state, &user, &key).await?;
    manage(&state, p.id, role).await?;
    let name = text(&req["name"], "the name", 80)?.ok_or_else(|| ApiError::bad("a folder needs a name"))?;
    let parent = folder_in(&state, p.id, &req["parentId"]).await?;
    let id = new_id();
    sqlx::query("INSERT INTO test_folders (id, project_id, parent_id, name, position) VALUES ($1, $2, $3, $4, (SELECT COALESCE(max(position), 0) + 1 FROM test_folders WHERE project_id = $2))")
        .bind(id)
        .bind(p.id)
        .bind(parent)
        .bind(&name)
        .execute(&state.db)
        .await?;
    Ok((StatusCode::CREATED, Json(json!({"id": id}))))
}

async fn folder_project(state: &AppState, user: &User, id: Uuid) -> ApiResult<Uuid> {
    let p: Option<Uuid> =
        sqlx::query_scalar("SELECT f.project_id FROM test_folders f JOIN projects p ON p.id = f.project_id WHERE f.id = $1 AND p.workspace_id = $2")
            .bind(id)
            .bind(user.workspace_id)
            .fetch_optional(&state.db)
            .await?;
    let p = p.ok_or(ApiError::NotFound("folder"))?;
    let role = role_in(state, user, p).await?;
    manage(state, p, role).await?;
    Ok(p)
}

/// `PATCH /test-folders/{id}` `{name}`.
pub async fn update_folder(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<Uuid>,
    Json(req): Json<Value>,
) -> ApiResult<StatusCode> {
    folder_project(&state, &user, id).await?;
    let name = text(&req["name"], "the name", 80)?.ok_or_else(|| ApiError::bad("a folder needs a name"))?;
    sqlx::query("UPDATE test_folders SET name = $2 WHERE id = $1").bind(id).bind(name).execute(&state.db).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /test-folders/{id}`: the folder and the ones below it go; their cases stay, without a folder.
pub async fn delete_folder(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    folder_project(&state, &user, id).await?;
    sqlx::query("DELETE FROM test_folders WHERE id = $1").bind(id).execute(&state.db).await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---- coverage: an issue's test cases ----------------------------------------------------------------

/// The test cases that cover an issue, with their latest results (cases in projects the caller sees).
pub async fn of_issue(state: &AppState, user: &User, issue: Uuid) -> ApiResult<Vec<Value>> {
    let rows: Vec<(Uuid, String, i32, String, String, Option<String>, Option<DateTime<Utc>>, Option<Uuid>, Option<String>)> =
        sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT c.id, p.key, c.number, c.title, c.priority, l.result, l.executed_at, l.run_id, (SELECT name FROM test_runs WHERE id = l.run_id)
           FROM test_case_issues ci JOIN test_cases c ON c.id = ci.case_id JOIN projects p ON p.id = c.project_id
           JOIN tc_project_roles($2) r ON r.project_id = c.project_id AND r.rank >= 1 {LATEST}
          WHERE ci.issue_id = $1 AND c.archived_at IS NULL ORDER BY p.key, c.number"
        )))
        .bind(issue)
        .bind(user.id)
        .fetch_all(&state.db)
        .await?;
    Ok(rows
        .into_iter()
        .map(|(id, key, number, title, priority, result, at, run, run_name)| {
            json!({"id": id, "key": case_key(&key, number), "title": title, "priority": priority, "lastResult": result, "lastRunAt": at,
                   "lastRun": run.map(|r| json!({"id": r, "name": run_name}))})
        })
        .collect())
}

/// `GET /issues/{key}/tests`.
pub async fn issue_tests(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(key): Path<String>) -> ApiResult<Json<Value>> {
    on(&state, &user).await?;
    let (issue, project, role) = crate::issues::resolve(&state, &user, &key, false).await?;
    Ok(Json(json!({"canManage": may(&state, project, role, "manageTests").await, "tests": of_issue(&state, &user, issue).await?})))
}

/// `POST /issues/{key}/tests` `{test}`: a test case (id or key) covers the issue.
pub async fn cover(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(key): Path<String>, Json(req): Json<Value>) -> ApiResult<StatusCode> {
    let (issue, _, _) = crate::issues::resolve(&state, &user, &key, false).await?;
    let (c, role) = case_for(&state, &user, req["test"].as_str().unwrap_or("")).await.map_err(|_| ApiError::bad("there is no such test case"))?;
    manage(&state, c.project_id, role).await?;
    sqlx::query("INSERT INTO test_case_issues (case_id, issue_id) VALUES ($1, $2) ON CONFLICT DO NOTHING").bind(c.id).bind(issue).execute(&state.db).await?;
    changed(&state, &user, c.project_id).await;
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /issues/{key}/tests/{test}`.
pub async fn uncover(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path((key, r)): Path<(String, String)>) -> ApiResult<StatusCode> {
    let (issue, _, _) = crate::issues::resolve(&state, &user, &key, false).await?;
    let (c, role) = case_for(&state, &user, &r).await?;
    manage(&state, c.project_id, role).await?;
    sqlx::query("DELETE FROM test_case_issues WHERE case_id = $1 AND issue_id = $2").bind(c.id).bind(issue).execute(&state.db).await?;
    changed(&state, &user, c.project_id).await;
    Ok(StatusCode::NO_CONTENT)
}

// ---- test plans -------------------------------------------------------------------------------------

async fn version_in(state: &AppState, project: Uuid, v: &Value) -> ApiResult<Option<Uuid>> {
    if v.is_null() {
        return Ok(None);
    }
    let id = v.as_str().and_then(|s| s.parse::<Uuid>().ok()).ok_or_else(|| ApiError::bad("versionId must be a release's id"))?;
    let found: Option<Uuid> =
        sqlx::query_scalar("SELECT id FROM versions WHERE id = $1 AND project_id = $2").bind(id).bind(project).fetch_optional(&state.db).await?;
    Ok(Some(found.ok_or_else(|| ApiError::bad("there is no such release in this project"))?))
}

fn clean_filter(v: &Value) -> ApiResult<Option<Value>> {
    if v.is_null() {
        return Ok(None);
    }
    let o = v.as_object().ok_or_else(|| ApiError::bad("filter is {folderId, labels, priority, text}"))?;
    let mut out = Map::new();
    for (k, val) in o {
        if val.is_null() {
            continue;
        }
        match k.as_str() {
            "folderId" => {
                val.as_str().and_then(|s| s.parse::<Uuid>().ok()).ok_or_else(|| ApiError::bad("filter.folderId must be a folder's id"))?;
                out.insert(k.clone(), val.clone());
            }
            "labels" => {
                let l = clean_labels(val)?;
                if !l.is_empty() {
                    out.insert(k.clone(), json!(l));
                }
            }
            "priority" => {
                out.insert(k.clone(), json!(clean_priority(val)?));
            }
            "text" => {
                if let Some(t) = text(val, "filter.text", 200)? {
                    out.insert(k.clone(), json!(t));
                }
            }
            other => return Err(ApiError::bad(format!("unknown filter {other}"))),
        }
    }
    Ok(if out.is_empty() { None } else { Some(Value::Object(out)) })
}

/// The ids of cases among `refs` (ids or keys) that belong to the project.
async fn case_ids(state: &AppState, project: Uuid, v: &Value) -> ApiResult<Vec<Uuid>> {
    let list = v.as_array().ok_or_else(|| ApiError::bad("caseIds is a list of test cases"))?;
    if list.len() > 2000 {
        return Err(ApiError::bad("at most 2000 test cases"));
    }
    let known: Vec<(Uuid, i32)> = sqlx::query_as("SELECT id, number FROM test_cases WHERE project_id = $1").bind(project).fetch_all(&state.db).await?;
    let mut out = Vec::new();
    for r in list {
        let s = r.as_str().unwrap_or("").trim().to_uppercase();
        let found = match s.parse::<Uuid>() {
            Ok(id) => known.iter().find(|k| k.0 == id),
            Err(_) => s.rsplit_once("-T").and_then(|(_, n)| n.parse::<i32>().ok()).and_then(|n| known.iter().find(|k| k.1 == n)),
        };
        let id = found.ok_or_else(|| ApiError::bad(format!("there is no test case {s} in this project")))?.0;
        if !out.contains(&id) {
            out.push(id);
        }
    }
    Ok(out)
}

/// A plan's cases: the ones picked by hand in their order, then the ones its filter finds.
async fn plan_cases(state: &AppState, project: Uuid, plan: Uuid, filter: Option<&Value>) -> ApiResult<Vec<Uuid>> {
    let mut ids: Vec<Uuid> = sqlx::query_scalar(
        "SELECT pc.case_id FROM test_plan_cases pc JOIN test_cases c ON c.id = pc.case_id WHERE pc.plan_id = $1 AND c.archived_at IS NULL ORDER BY pc.position, c.number",
    )
    .bind(plan)
    .fetch_all(&state.db)
    .await?;
    if let Some(f) = filter {
        for c in lines(state, project, &CaseFilter::from_plan(f), None).await? {
            if !ids.contains(&c.id) {
                ids.push(c.id);
            }
        }
    }
    Ok(ids)
}

#[derive(sqlx::FromRow)]
struct PlanRow {
    id: Uuid,
    project_id: Uuid,
    name: String,
    description: Option<String>,
    version_id: Option<Uuid>,
    version_name: Option<String>,
    filter: Option<Value>,
    created_at: DateTime<Utc>,
}

const PLAN_SQL: &str = "SELECT tp.id, tp.project_id, tp.name, tp.description, tp.version_id, v.name AS version_name, tp.filter, tp.created_at
   FROM test_plans tp JOIN projects p ON p.id = tp.project_id LEFT JOIN versions v ON v.id = tp.version_id";

async fn plan_for(state: &AppState, user: &User, id: Uuid) -> ApiResult<(PlanRow, Role)> {
    let row: Option<PlanRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!("{PLAN_SQL} WHERE tp.id = $1 AND p.workspace_id = $2")))
        .bind(id)
        .bind(user.workspace_id)
        .fetch_optional(&state.db)
        .await?;
    let row = row.ok_or(ApiError::NotFound("test plan"))?;
    let role = role_in(state, user, row.project_id).await.map_err(|e| if matches!(e, ApiError::NotFound(_)) { ApiError::NotFound("test plan") } else { e })?;
    Ok((row, role))
}

fn plan_json(p: &PlanRow, count: usize) -> Value {
    json!({"id": p.id, "name": p.name, "description": p.description, "version": p.version_id.map(|id| json!({"id": id, "name": p.version_name})),
           "filter": p.filter, "caseCount": count, "createdAt": p.created_at})
}

/// `GET /projects/{key}/test-plans`: the plans and the run templates.
pub async fn plans(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(key): Path<String>) -> ApiResult<Json<Value>> {
    let (p, _) = project(&state, &user, &key).await?;
    let rows: Vec<PlanRow> =
        sqlx::query_as(sqlx::AssertSqlSafe(format!("{PLAN_SQL} WHERE tp.project_id = $1 ORDER BY lower(tp.name)"))).bind(p.id).fetch_all(&state.db).await?;
    let mut out = Vec::new();
    for r in &rows {
        out.push(plan_json(r, plan_cases(&state, p.id, r.id, r.filter.as_ref()).await?.len()));
    }
    let templates: Vec<(Uuid, String, Uuid, String, Option<String>, Vec<Uuid>)> = sqlx::query_as(
        "SELECT t.id, t.name, t.plan_id, tp.name, t.environment, t.tester_ids FROM test_run_templates t JOIN test_plans tp ON tp.id = t.plan_id WHERE t.project_id = $1 ORDER BY lower(t.name)",
    )
    .bind(p.id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(json!({
        "plans": out,
        "templates": templates.into_iter().map(|(id, name, plan, plan_name, env, testers)| json!({"id": id, "name": name, "plan": {"id": plan, "name": plan_name}, "environment": env, "testerIds": testers})).collect::<Vec<_>>(),
    })))
}

/// `POST /projects/{key}/test-plans` `{name, description?, versionId?, caseIds?, filter?}`.
pub async fn create_plan(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Path(key): Path<String>,
    Json(req): Json<Value>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let (p, role) = project(&state, &user, &key).await?;
    manage(&state, p.id, role).await?;
    let name = text(&req["name"], "the name", 120)?.ok_or_else(|| ApiError::bad("a test plan needs a name"))?;
    let description = text(&req["description"], "the description", 5000)?;
    let version = version_in(&state, p.id, &req["versionId"]).await?;
    let filter = clean_filter(&req["filter"])?;
    let cases = if req["caseIds"].is_null() { vec![] } else { case_ids(&state, p.id, &req["caseIds"]).await? };
    let id = new_id();
    let mut tx = state.db.begin().await?;
    sqlx::query("INSERT INTO test_plans (id, project_id, name, description, version_id, filter, created_by) VALUES ($1, $2, $3, $4, $5, $6, $7)")
        .bind(id)
        .bind(p.id)
        .bind(&name)
        .bind(&description)
        .bind(version)
        .bind(&filter)
        .bind(user.id)
        .execute(&mut *tx)
        .await
        .map_err(|e| unique(e, "there is already a test plan with this name"))?;
    put_plan_cases(&mut tx, id, &cases).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(json!({"id": id}))))
}

fn unique(e: sqlx::Error, msg: &str) -> ApiError {
    match &e {
        sqlx::Error::Database(d) if d.is_unique_violation() => ApiError::Conflict(msg.into()),
        _ => e.into(),
    }
}

async fn put_plan_cases(tx: &mut sqlx::Transaction<'_, sqlx::Postgres>, plan: Uuid, cases: &[Uuid]) -> ApiResult<()> {
    sqlx::query("DELETE FROM test_plan_cases WHERE plan_id = $1").bind(plan).execute(&mut **tx).await?;
    let positions: Vec<i32> = (0..cases.len() as i32).collect();
    sqlx::query("INSERT INTO test_plan_cases (plan_id, case_id, position) SELECT $1, * FROM unnest($2::uuid[], $3::int[])")
        .bind(plan)
        .bind(cases)
        .bind(&positions)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// `GET /test-plans/{id}`: a plan with its cases.
pub async fn plan(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(id): Path<Uuid>) -> ApiResult<Json<Value>> {
    let (p, _) = plan_for(&state, &user, id).await?;
    let key: String = sqlx::query_scalar("SELECT key FROM projects WHERE id = $1").bind(p.project_id).fetch_one(&state.db).await?;
    let ids = plan_cases(&state, p.project_id, p.id, p.filter.as_ref()).await?;
    let picked: Vec<Uuid> =
        sqlx::query_scalar("SELECT case_id FROM test_plan_cases WHERE plan_id = $1 ORDER BY position").bind(id).fetch_all(&state.db).await?;
    let mut found = lines(&state, p.project_id, &CaseFilter::default(), Some(&ids)).await?;
    found.sort_by_key(|c| ids.iter().position(|i| *i == c.id));
    let mut v = plan_json(&p, ids.len());
    v["caseIds"] = json!(picked);
    v["cases"] = json!(found.iter().map(|c| line_json(&key, c)).collect::<Vec<_>>());
    Ok(Json(v))
}

/// `PATCH /test-plans/{id}` `{name?, description?, versionId?, caseIds?, filter?}`.
pub async fn update_plan(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<Uuid>,
    Json(req): Json<Map<String, Value>>,
) -> ApiResult<StatusCode> {
    let (p, role) = plan_for(&state, &user, id).await?;
    manage(&state, p.project_id, role).await?;
    let name = match req.get("name") {
        Some(v) => text(v, "the name", 120)?.ok_or_else(|| ApiError::bad("a test plan needs a name"))?,
        None => p.name.clone(),
    };
    let description = match req.get("description") {
        Some(v) => text(v, "the description", 5000)?,
        None => p.description.clone(),
    };
    let version = match req.get("versionId") {
        Some(v) => version_in(&state, p.project_id, v).await?,
        None => p.version_id,
    };
    let filter = match req.get("filter") {
        Some(v) => clean_filter(v)?,
        None => p.filter.clone(),
    };
    let cases = match req.get("caseIds") {
        Some(v) => Some(case_ids(&state, p.project_id, v).await?),
        None => None,
    };
    let mut tx = state.db.begin().await?;
    sqlx::query("UPDATE test_plans SET name = $2, description = $3, version_id = $4, filter = $5 WHERE id = $1")
        .bind(id)
        .bind(&name)
        .bind(&description)
        .bind(version)
        .bind(&filter)
        .execute(&mut *tx)
        .await
        .map_err(|e| unique(e, "there is already a test plan with this name"))?;
    if let Some(cases) = cases {
        put_plan_cases(&mut tx, id, &cases).await?;
    }
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /test-plans/{id}`: its runs stay.
pub async fn delete_plan(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    let (p, role) = plan_for(&state, &user, id).await?;
    manage(&state, p.project_id, role).await?;
    sqlx::query("DELETE FROM test_plans WHERE id = $1").bind(id).execute(&state.db).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// People of the workspace among `v` (ids).
async fn testers(state: &AppState, workspace: Uuid, v: &Value) -> ApiResult<Vec<Uuid>> {
    if v.is_null() {
        return Ok(vec![]);
    }
    let ids: Vec<Uuid> = v
        .as_array()
        .ok_or_else(|| ApiError::bad("testerIds is a list of people"))?
        .iter()
        .map(|x| x.as_str().and_then(|s| s.parse().ok()).ok_or_else(|| ApiError::bad("testerIds is a list of people's ids")))
        .collect::<ApiResult<_>>()?;
    let known: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM users WHERE workspace_id = $1 AND id = ANY($2) AND status <> 'deactivated'")
        .bind(workspace)
        .bind(&ids)
        .fetch_all(&state.db)
        .await?;
    if known.len() != ids.iter().collect::<HashSet<_>>().len() {
        return Err(ApiError::bad("a tester isn't an active person of this workspace"));
    }
    Ok(ids)
}

/// `POST /projects/{key}/test-run-templates` `{name, planId, environment?, testerIds?}`.
pub async fn create_template(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Path(key): Path<String>,
    Json(req): Json<Value>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let (p, role) = project(&state, &user, &key).await?;
    manage(&state, p.id, role).await?;
    let name = text(&req["name"], "the name", 120)?.ok_or_else(|| ApiError::bad("a run template needs a name"))?;
    let plan: Option<Uuid> = sqlx::query_scalar("SELECT id FROM test_plans WHERE id = $1 AND project_id = $2")
        .bind(req["planId"].as_str().and_then(|s| s.parse::<Uuid>().ok()))
        .bind(p.id)
        .fetch_optional(&state.db)
        .await?;
    let plan = plan.ok_or_else(|| ApiError::bad("a run template names a test plan of this project (planId)"))?;
    let environment = text(&req["environment"], "the environment", 200)?;
    let who = testers(&state, user.workspace_id, &req["testerIds"]).await?;
    let id = new_id();
    sqlx::query("INSERT INTO test_run_templates (id, project_id, name, plan_id, environment, tester_ids) VALUES ($1, $2, $3, $4, $5, $6)")
        .bind(id)
        .bind(p.id)
        .bind(&name)
        .bind(plan)
        .bind(&environment)
        .bind(&who)
        .execute(&state.db)
        .await
        .map_err(|e| unique(e, "there is already a run template with this name"))?;
    Ok((StatusCode::CREATED, Json(json!({"id": id}))))
}

/// `DELETE /test-run-templates/{id}`.
pub async fn delete_template(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    let p: Option<Uuid> =
        sqlx::query_scalar("SELECT t.project_id FROM test_run_templates t JOIN projects p ON p.id = t.project_id WHERE t.id = $1 AND p.workspace_id = $2")
            .bind(id)
            .bind(user.workspace_id)
            .fetch_optional(&state.db)
            .await?;
    let p = p.ok_or(ApiError::NotFound("run template"))?;
    let role = role_in(&state, &user, p).await?;
    manage(&state, p, role).await?;
    sqlx::query("DELETE FROM test_run_templates WHERE id = $1").bind(id).execute(&state.db).await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---- test runs --------------------------------------------------------------------------------------

#[derive(sqlx::FromRow)]
struct RunRow {
    id: Uuid,
    project_id: Uuid,
    project_key: String,
    number: i32,
    name: String,
    plan_id: Option<Uuid>,
    plan_name: Option<String>,
    version_id: Option<Uuid>,
    version_name: Option<String>,
    environment: Option<String>,
    tester_ids: Vec<Uuid>,
    created_at: DateTime<Utc>,
    finished_at: Option<DateTime<Utc>>,
    total: i64,
    passed: i64,
    failed: i64,
    blocked: i64,
    skipped: i64,
}

const RUN_SQL: &str =
    "SELECT r.id, r.project_id, p.key AS project_key, r.number, r.name, r.plan_id, tp.name AS plan_name, r.version_id, v.name AS version_name,
        r.environment, r.tester_ids, r.created_at, r.finished_at,
        (SELECT count(*) FROM test_run_cases rc WHERE rc.run_id = r.id) AS total,
        (SELECT count(*) FROM test_run_cases rc WHERE rc.run_id = r.id AND rc.result = 'passed') AS passed,
        (SELECT count(*) FROM test_run_cases rc WHERE rc.run_id = r.id AND rc.result = 'failed') AS failed,
        (SELECT count(*) FROM test_run_cases rc WHERE rc.run_id = r.id AND rc.result = 'blocked') AS blocked,
        (SELECT count(*) FROM test_run_cases rc WHERE rc.run_id = r.id AND rc.result = 'skipped') AS skipped
   FROM test_runs r JOIN projects p ON p.id = r.project_id LEFT JOIN test_plans tp ON tp.id = r.plan_id LEFT JOIN versions v ON v.id = r.version_id";

fn run_json(r: &RunRow) -> Value {
    let done = r.passed + r.failed + r.blocked + r.skipped;
    json!({
        "id": r.id, "key": format!("{}-R{}", r.project_key, r.number), "number": r.number, "name": r.name, "projectKey": r.project_key,
        "plan": r.plan_id.map(|id| json!({"id": id, "name": r.plan_name})), "version": r.version_id.map(|id| json!({"id": id, "name": r.version_name})),
        "environment": r.environment, "testerIds": r.tester_ids, "createdAt": r.created_at, "finishedAt": r.finished_at,
        "counts": {"total": r.total, "passed": r.passed, "failed": r.failed, "blocked": r.blocked, "skipped": r.skipped, "untested": r.total - done},
    })
}

async fn run_for(state: &AppState, user: &User, id: Uuid) -> ApiResult<(RunRow, Role)> {
    let row: Option<RunRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!("{RUN_SQL} WHERE r.id = $1 AND p.workspace_id = $2")))
        .bind(id)
        .bind(user.workspace_id)
        .fetch_optional(&state.db)
        .await?;
    let row = row.ok_or(ApiError::NotFound("test run"))?;
    let role = role_in(state, user, row.project_id).await.map_err(|e| if matches!(e, ApiError::NotFound(_)) { ApiError::NotFound("test run") } else { e })?;
    Ok((row, role))
}

#[derive(Deserialize, Default)]
pub struct RunsQuery {
    /// only the runs of a release
    version: Option<Uuid>,
}

/// `GET /projects/{key}/test-runs`: newest first.
pub async fn runs(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Path(key): Path<String>,
    Query(q): Query<RunsQuery>,
) -> ApiResult<Json<Value>> {
    let (p, _) = project(&state, &user, &key).await?;
    let rows: Vec<RunRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "{RUN_SQL} WHERE r.project_id = $1 AND ($2::uuid IS NULL OR r.version_id = $2) ORDER BY r.number DESC LIMIT 500"
    )))
    .bind(p.id)
    .bind(q.version)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(json!(rows.iter().map(run_json).collect::<Vec<_>>())))
}

/// `POST /projects/{key}/test-runs` `{name?, planId? | templateId? | caseIds?, environment?, versionId?, testerIds?}`:
/// starts a run of a plan's cases (or of the given ones), each at its current version.
pub async fn create_run(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Path(key): Path<String>,
    Json(req): Json<Value>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let (p, role) = project(&state, &user, &key).await?;
    execute(&state, p.id, role).await?;
    let mut environment = text(&req["environment"], "the environment", 200)?;
    let mut who = testers(&state, user.workspace_id, &req["testerIds"]).await?;
    let mut plan_id = req["planId"].as_str().and_then(|s| s.parse::<Uuid>().ok());
    if let Some(t) = req["templateId"].as_str() {
        let row: Option<(Uuid, Option<String>, Vec<Uuid>)> =
            sqlx::query_as("SELECT plan_id, environment, tester_ids FROM test_run_templates WHERE id = $1 AND project_id = $2")
                .bind(t.parse::<Uuid>().ok())
                .bind(p.id)
                .fetch_optional(&state.db)
                .await?;
        let (plan, env, testers) = row.ok_or_else(|| ApiError::bad("there is no such run template in this project"))?;
        plan_id = Some(plan);
        environment = environment.or(env);
        if who.is_empty() {
            who = testers;
        }
    }
    let plan: Option<PlanRow> = match plan_id {
        Some(id) => Some(
            sqlx::query_as(sqlx::AssertSqlSafe(format!("{PLAN_SQL} WHERE tp.id = $1 AND tp.project_id = $2")))
                .bind(id)
                .bind(p.id)
                .fetch_optional(&state.db)
                .await?
                .ok_or_else(|| ApiError::bad("there is no such test plan in this project"))?,
        ),
        None => None,
    };
    let cases = match (&plan, req["caseIds"].is_null()) {
        (_, false) => case_ids(&state, p.id, &req["caseIds"]).await?,
        (Some(pl), true) => plan_cases(&state, p.id, pl.id, pl.filter.as_ref()).await?,
        (None, true) => return Err(ApiError::bad("a run needs a test plan, a run template or test cases")),
    };
    if cases.is_empty() {
        return Err(ApiError::bad("there are no test cases to run"));
    }
    let version = match (&req["versionId"], &plan) {
        (Value::Null, Some(pl)) => pl.version_id,
        (v, _) => version_in(&state, p.id, v).await?,
    };
    let name = match text(&req["name"], "the name", 160)? {
        Some(n) => n,
        None => format!("{} {}", plan.as_ref().map(|pl| pl.name.as_str()).unwrap_or("Test run"), Utc::now().date_naive()),
    };
    let id = new_id();
    let mut tx = state.db.begin().await?;
    let number: i32 =
        sqlx::query_scalar("UPDATE projects SET next_test_run_number = next_test_run_number + 1 WHERE id = $1 RETURNING next_test_run_number - 1")
            .bind(p.id)
            .fetch_one(&mut *tx)
            .await?;
    sqlx::query("INSERT INTO test_runs (id, project_id, number, name, plan_id, version_id, environment, tester_ids, created_by) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)")
        .bind(id)
        .bind(p.id)
        .bind(number)
        .bind(&name)
        .bind(plan.as_ref().map(|pl| pl.id))
        .bind(version)
        .bind(&environment)
        .bind(&who)
        .bind(user.id)
        .execute(&mut *tx)
        .await?;
    let ids: Vec<Uuid> = cases.iter().map(|_| new_id()).collect();
    let positions: Vec<i32> = (0..cases.len() as i32).collect();
    sqlx::query(
        "INSERT INTO test_run_cases (id, run_id, case_id, case_version, position)
         SELECT x.id, $1, x.case_id, c.version, x.position FROM unnest($2::uuid[], $3::uuid[], $4::int[]) AS x(id, case_id, position) JOIN test_cases c ON c.id = x.case_id",
    )
    .bind(id)
    .bind(&ids)
    .bind(&cases)
    .bind(&positions)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    changed(&state, &user, p.id).await;
    Ok((StatusCode::CREATED, Json(json!({"id": id, "key": format!("{}-R{number}", p.key), "name": name}))))
}

#[derive(sqlx::FromRow)]
struct RunCase {
    id: Uuid,
    case_id: Uuid,
    number: i32,
    priority: String,
    current: i32,
    case_version: i32,
    title: String,
    preconditions: Option<String>,
    kind: String,
    case_steps: Value,
    scenario: Option<String>,
    assignee_id: Option<Uuid>,
    assignee: Option<String>,
    result: String,
    comment: Option<String>,
    steps: Value,
    executed_by: Option<String>,
    executed_at: Option<DateTime<Utc>>,
}

const RUN_CASE_SQL: &str = "SELECT rc.id, rc.case_id, c.number, c.priority, c.version AS current, rc.case_version, cv.title, cv.preconditions, cv.kind, cv.steps AS case_steps, cv.scenario,
        rc.assignee_id, au.name AS assignee, rc.result, rc.comment, rc.steps, eu.name AS executed_by, rc.executed_at
   FROM test_run_cases rc JOIN test_cases c ON c.id = rc.case_id
   JOIN test_case_versions cv ON cv.case_id = rc.case_id AND cv.version = rc.case_version
   LEFT JOIN users au ON au.id = rc.assignee_id LEFT JOIN users eu ON eu.id = rc.executed_by";

/// The steps of a run's case with their results: `[{action, expected, result, comment}]`.
fn step_results(rc: &RunCase) -> Vec<Value> {
    steps_of(&rc.kind, &rc.case_steps, rc.scenario.as_deref())
        .into_iter()
        .enumerate()
        .map(|(i, s)| {
            let r = &rc.steps[i];
            json!({"action": s["action"], "expected": s["expected"], "result": r["result"].as_str().unwrap_or("untested"), "comment": r["comment"]})
        })
        .collect()
}

/// `GET /test-runs/{id}`: a run with its cases, their steps and results, files and bugs.
pub async fn run(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(id): Path<Uuid>) -> ApiResult<Json<Value>> {
    let (r, role) = run_for(&state, &user, id).await?;
    let cases: Vec<RunCase> = sqlx::query_as(sqlx::AssertSqlSafe(format!("{RUN_CASE_SQL} WHERE rc.run_id = $1 ORDER BY rc.position, c.number")))
        .bind(id)
        .fetch_all(&state.db)
        .await?;
    let files: Vec<(Uuid, Uuid, Option<i32>, String, String, i64)> =
        sqlx::query_as("SELECT f.id, f.run_case_id, f.step, f.name, f.mime, f.size FROM test_files f JOIN test_run_cases rc ON rc.id = f.run_case_id WHERE rc.run_id = $1 ORDER BY f.created_at")
            .bind(id)
            .fetch_all(&state.db)
            .await?;
    let bugs: Vec<(Uuid, Uuid, String, String, String, String)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT b.run_case_id, i.id, p.key || '-' || i.number, i.title, s.name, s.category
           FROM test_bugs b JOIN test_run_cases rc ON rc.id = b.run_case_id JOIN issues i ON i.id = b.issue_id JOIN projects p ON p.id = i.project_id
           JOIN statuses s ON s.id = i.status_id JOIN tc_project_roles($2) pr ON pr.project_id = i.project_id AND pr.rank >= 1
          WHERE rc.run_id = $1 AND i.deleted_at IS NULL AND {}",
        access::sees_issue("i", "$2")
    )))
    .bind(id)
    .bind(user.id)
    .fetch_all(&state.db)
    .await?;
    let people: Vec<(Uuid, String)> = sqlx::query_as("SELECT id, name FROM users WHERE id = ANY($1)").bind(&r.tester_ids).fetch_all(&state.db).await?;
    let mut v = run_json(&r);
    v["testers"] = json!(people.into_iter().map(|(id, name)| json!({"id": id, "name": name})).collect::<Vec<_>>());
    v["canExecute"] = json!(may(&state, r.project_id, role, "executeTests").await);
    v["canManage"] = json!(may(&state, r.project_id, role, "manageTests").await);
    v["cases"] = json!(
        cases
            .iter()
            .map(|c| {
                json!({
                    "id": c.id, "case": {"id": c.case_id, "key": case_key(&r.project_key, c.number), "title": c.title, "priority": c.priority},
                    "version": c.case_version, "outdated": c.current != c.case_version, "preconditions": c.preconditions, "kind": c.kind, "scenario": c.scenario,
                    "assignee": c.assignee_id.map(|id| json!({"id": id, "name": c.assignee})), "result": c.result, "comment": c.comment, "steps": step_results(c),
                    "executedBy": c.executed_by, "executedAt": c.executed_at,
                    "rendered": rendered(c.preconditions.as_deref(), &steps_of(&c.kind, &c.case_steps, c.scenario.as_deref())),
                    "files": files.iter().filter(|f| f.1 == c.id).map(|(id, _, step, name, mime, size)| json!({"id": id, "step": step, "name": name, "mime": mime, "size": size, "url": format!("/api/test-files/{id}")})).collect::<Vec<_>>(),
                    "bugs": bugs.iter().filter(|b| b.0 == c.id).map(|(_, id, key, title, status, category)| json!({"id": id, "key": key, "title": title, "status": {"name": status, "category": category}})).collect::<Vec<_>>(),
                })
            })
            .collect::<Vec<_>>()
    );
    Ok(Json(v))
}

/// `PATCH /test-runs/{id}` `{name?, environment?, testerIds?, versionId?, finished?}`. Finishing
/// a run closes it for results (reopen with `finished: false`) and runs the project's
/// "test run finished" rules.
pub async fn update_run(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<Uuid>,
    Json(req): Json<Map<String, Value>>,
) -> ApiResult<StatusCode> {
    let (r, role) = run_for(&state, &user, id).await?;
    execute(&state, r.project_id, role).await?;
    let name = match req.get("name") {
        Some(v) => text(v, "the name", 160)?.ok_or_else(|| ApiError::bad("a run needs a name"))?,
        None => r.name.clone(),
    };
    let environment = match req.get("environment") {
        Some(v) => text(v, "the environment", 200)?,
        None => r.environment.clone(),
    };
    let who = match req.get("testerIds") {
        Some(v) => testers(&state, user.workspace_id, v).await?,
        None => r.tester_ids.clone(),
    };
    let version = match req.get("versionId") {
        Some(v) => version_in(&state, r.project_id, v).await?,
        None => r.version_id,
    };
    let finished = match req.get("finished").and_then(Value::as_bool) {
        Some(true) => Some(r.finished_at.unwrap_or_else(Utc::now)),
        Some(false) => None,
        None => r.finished_at,
    };
    sqlx::query("UPDATE test_runs SET name = $2, environment = $3, tester_ids = $4, version_id = $5, finished_at = $6 WHERE id = $1")
        .bind(id)
        .bind(&name)
        .bind(&environment)
        .bind(&who)
        .bind(version)
        .bind(finished)
        .execute(&state.db)
        .await?;
    if finished.is_some() && r.finished_at.is_none() {
        let issues: Vec<Uuid> =
            sqlx::query_scalar("SELECT DISTINCT ci.issue_id FROM test_run_cases rc JOIN test_case_issues ci ON ci.case_id = rc.case_id WHERE rc.run_id = $1")
                .bind(id)
                .fetch_all(&state.db)
                .await?;
        fire(&state, user.workspace_id, &issues, "testRunFinished").await;
    }
    changed(&state, &user, r.project_id).await;
    Ok(StatusCode::NO_CONTENT)
}

/// Runs the automation rules that wait for a test event, for the issues the tests cover.
async fn fire(state: &AppState, workspace: Uuid, issues: &[Uuid], event: &'static str) {
    let bot = Actor { user: None, workspace, depth: 0 };
    for id in issues.iter().take(200) {
        if let Ok(row) = crate::issues::load(state, *id).await {
            let s = crate::issues::summary(state, row);
            crate::automation::run_event(state, &bot, &s, event).await;
        }
    }
}

/// `DELETE /test-runs/{id}`: the run and its results (also from the reports).
pub async fn delete_run(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    let (r, role) = run_for(&state, &user, id).await?;
    manage(&state, r.project_id, role).await?;
    let keys: Vec<String> = sqlx::query_scalar("SELECT f.storage_key FROM test_files f JOIN test_run_cases rc ON rc.id = f.run_case_id WHERE rc.run_id = $1")
        .bind(id)
        .fetch_all(&state.db)
        .await?;
    sqlx::query("DELETE FROM test_runs WHERE id = $1").bind(id).execute(&state.db).await?;
    for k in keys {
        let _ = state.blobs.delete(&ObjPath::from(k)).await;
    }
    changed(&state, &user, r.project_id).await;
    Ok(StatusCode::NO_CONTENT)
}

/// What the steps add up to: failed beats blocked beats unfinished; all skipped is skipped.
fn overall(steps: &[&str]) -> &'static str {
    if steps.contains(&"failed") {
        "failed"
    } else if steps.contains(&"blocked") {
        "blocked"
    } else if steps.is_empty() || steps.contains(&"untested") {
        "untested"
    } else if steps.iter().all(|s| *s == "skipped") {
        "skipped"
    } else {
        "passed"
    }
}

/// A case of a run the caller may execute: (the row, the run).
async fn run_case_for(state: &AppState, user: &User, id: Uuid, write: bool) -> ApiResult<(RunCase, RunRow, Role)> {
    on(state, user).await?;
    let run: Option<Uuid> = sqlx::query_scalar("SELECT run_id FROM test_run_cases WHERE id = $1").bind(id).fetch_optional(&state.db).await?;
    let (r, role) = run_for(state, user, run.ok_or(ApiError::NotFound("test"))?).await?;
    if write {
        execute(state, r.project_id, role).await?;
        if r.finished_at.is_some() {
            return Err(ApiError::Conflict("this run is finished: reopen it to change results".into()));
        }
    }
    let rc: RunCase = sqlx::query_as(sqlx::AssertSqlSafe(format!("{RUN_CASE_SQL} WHERE rc.id = $1"))).bind(id).fetch_one(&state.db).await?;
    Ok((rc, r, role))
}

fn result_of(v: &Value) -> ApiResult<&'static str> {
    let s = v.as_str().unwrap_or("").to_lowercase();
    RESULTS.iter().find(|r| **r == s).copied().ok_or_else(|| ApiError::bad("a result is passed, failed, blocked, skipped or untested"))
}

/// `PUT /test-run-cases/{id}` `{result?, comment?, steps?: [{result, comment}], step?: {index, result, comment}, assigneeId?}`:
/// records how a case went. Without `result` the steps decide; a result alone marks the open
/// steps the same way when it is passed or skipped.
pub async fn record(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<Uuid>,
    Json(req): Json<Map<String, Value>>,
) -> ApiResult<Json<Value>> {
    let (rc, r, _) = run_case_for(&state, &user, id, true).await?;
    let mut steps: Vec<(String, Option<String>)> =
        step_results(&rc).iter().map(|s| (s["result"].as_str().unwrap_or("untested").to_string(), s["comment"].as_str().map(str::to_string))).collect();
    let mut touched = false;
    if let Some(list) = req.get("steps").filter(|v| !v.is_null()) {
        let list = list.as_array().ok_or_else(|| ApiError::bad("steps is a list of {result, comment}"))?;
        if list.len() > steps.len() {
            return Err(ApiError::bad(format!("this test case has {} steps", steps.len())));
        }
        for (i, s) in list.iter().enumerate() {
            steps[i] = (result_of(&s["result"])?.to_string(), text(&s["comment"], "a comment", 5000)?);
        }
        touched = true;
    }
    if let Some(s) = req.get("step").filter(|v| !v.is_null()) {
        let i =
            s["index"].as_u64().filter(|i| (1..=steps.len() as u64).contains(i)).ok_or_else(|| ApiError::bad(format!("step.index is 1 to {}", steps.len())))?
                as usize
                - 1;
        let comment = if s.get("comment").is_some() { text(&s["comment"], "a comment", 5000)? } else { steps[i].1.clone() };
        steps[i] = (if s["result"].is_null() { steps[i].0.clone() } else { result_of(&s["result"])?.to_string() }, comment);
        touched = true;
    }
    let result: String = match req.get("result").filter(|v| !v.is_null()) {
        Some(v) => {
            let res = result_of(v)?;
            if !touched && matches!(res, "passed" | "skipped") {
                for s in steps.iter_mut().filter(|s| s.0 == "untested") {
                    s.0 = res.to_string();
                }
            }
            if res == "untested" && !touched {
                steps.iter_mut().for_each(|s| s.0 = "untested".into());
            }
            res.to_string()
        }
        None if touched => overall(&steps.iter().map(|s| s.0.as_str()).collect::<Vec<_>>()).to_string(),
        None => rc.result.clone(),
    };
    let comment = match req.get("comment") {
        Some(v) => text(v, "the comment", 10_000)?,
        None => rc.comment.clone(),
    };
    let assignee = match req.get("assigneeId") {
        Some(Value::Null) => None,
        Some(v) => testers(&state, user.workspace_id, &json!([v])).await?.first().copied(),
        None => rc.assignee_id,
    };
    let stored: Vec<Value> = steps.iter().map(|(res, c)| json!({"result": res, "comment": c})).collect();
    let moved = result != rc.result;
    let mut tx = state.db.begin().await?;
    sqlx::query(
        "UPDATE test_run_cases SET result = $2, comment = $3, steps = $4, assignee_id = $5,
                executed_by = CASE WHEN $6 THEN $7 ELSE executed_by END, executed_at = CASE WHEN $6 THEN now() ELSE executed_at END WHERE id = $1",
    )
    .bind(id)
    .bind(&result)
    .bind(&comment)
    .bind(json!(stored))
    .bind(assignee)
    .bind(moved || touched)
    .bind(user.id)
    .execute(&mut *tx)
    .await?;
    if moved {
        sqlx::query("INSERT INTO test_result_events (id, project_id, run_id, case_id, result, actor_id) VALUES ($1, $2, $3, $4, $5, $6)")
            .bind(new_id())
            .bind(r.project_id)
            .bind(r.id)
            .bind(rc.case_id)
            .bind(&result)
            .bind(user.id)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    if moved && result == "failed" {
        let issues: Vec<Uuid> = sqlx::query_scalar("SELECT issue_id FROM test_case_issues WHERE case_id = $1").bind(rc.case_id).fetch_all(&state.db).await?;
        fire(&state, user.workspace_id, &issues, "testFailed").await;
    }
    changed(&state, &user, r.project_id).await;
    Ok(Json(json!({"result": result, "steps": stored})))
}

/// `POST /test-run-cases/{id}/files?step=n` (multipart): screenshots and other files on a result.
pub async fn upload(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<Uuid>,
    Query(q): Query<HashMap<String, String>>,
    mut mp: Multipart,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let (rc, r, _) = run_case_for(&state, &user, id, true).await?;
    let count = step_results(&rc).len() as i32;
    let step = match q.get("step").filter(|s| !s.is_empty()) {
        Some(s) => Some(s.parse::<i32>().ok().filter(|n| (1..=count).contains(n)).ok_or_else(|| ApiError::bad(format!("step is 1 to {count}")))?),
        None => None,
    };
    let max = state.config.max_upload_mb.saturating_mul(1024 * 1024);
    let mut files = Vec::new();
    while let Some(field) = mp.next_field().await.map_err(|e| ApiError::bad(format!("upload: {e}")))? {
        let Some(name) = field.file_name().map(|n| n.replace(['/', '\\', '\0'], "_")).filter(|n| !n.trim().is_empty()) else { continue };
        let name: String = name.chars().take(200).collect();
        let mime = field.content_type().map(str::to_string).unwrap_or_else(|| mime_guess::from_path(&name).first_or_octet_stream().to_string());
        let mut data = Vec::new();
        let mut field = field;
        while let Some(chunk) = field.chunk().await.map_err(|e| ApiError::bad(format!("upload: {e}")))? {
            data.extend_from_slice(&chunk);
            if data.len() as u64 > max {
                return Err(ApiError::TooLarge);
            }
        }
        files.push((name, mime, data));
    }
    if files.is_empty() {
        return Err(ApiError::bad("no file in the upload"));
    }
    for (name, _, data) in &files {
        crate::files::check(&state, user.workspace_id, name, data).await?;
    }
    let mut out = Vec::new();
    for (name, mime, data) in files {
        let file = new_id();
        let key = format!("tests/{id}/{file}");
        let size = data.len() as i64;
        state.blobs.put(&ObjPath::from(key.clone()), data.into()).await.map_err(|e| ApiError::Internal(anyhow::anyhow!("storage: {e}")))?;
        sqlx::query("INSERT INTO test_files (id, run_case_id, step, name, mime, size, storage_key, uploaded_by) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)")
            .bind(file)
            .bind(id)
            .bind(step)
            .bind(&name)
            .bind(&mime)
            .bind(size)
            .bind(&key)
            .bind(user.id)
            .execute(&state.db)
            .await?;
        out.push(json!({"id": file, "step": step, "name": name, "mime": mime, "size": size, "url": format!("/api/test-files/{file}")}));
    }
    changed(&state, &user, r.project_id).await;
    Ok((StatusCode::CREATED, Json(json!(out))))
}

async fn file_for(state: &AppState, id: Uuid) -> ApiResult<(Uuid, String, String, String, i64)> {
    sqlx::query_as("SELECT run_case_id, name, mime, storage_key, size FROM test_files WHERE id = $1")
        .bind(id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(ApiError::NotFound("file"))
}

/// `GET /test-files/{id}`.
pub async fn download(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(id): Path<Uuid>) -> ApiResult<Response> {
    let (rc, name, mime, key, size) = file_for(&state, id).await?;
    run_case_for(&state, &user, rc, false).await.map_err(|_| ApiError::NotFound("file"))?;
    crate::routes::attachments::serve(&state, &name, mime, key, size).await
}

/// `DELETE /test-files/{id}`.
pub async fn delete_file(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    let (rc, _, _, key, _) = file_for(&state, id).await?;
    let (_, r, _) = run_case_for(&state, &user, rc, true).await?;
    sqlx::query("DELETE FROM test_files WHERE id = $1").bind(id).execute(&state.db).await?;
    let _ = state.blobs.delete(&ObjPath::from(key)).await;
    changed(&state, &user, r.project_id).await;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /test-run-cases/{id}/bug` `{title?, step?}`: a bug for a failure, filled in with the
/// test case, the failing step, expected and actual, the environment and the files; linked to
/// the run and to the issues the case covers.
pub async fn bug(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<Uuid>,
    Json(req): Json<Value>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let (rc, r, role) = run_case_for(&state, &user, id, false).await?;
    execute(&state, r.project_id, role).await?;
    if role < Role::Member {
        return Err(ApiError::Denied("you can only view this project".into()));
    }
    let steps = step_results(&rc);
    let failing = match req["step"].as_u64() {
        Some(n) if (1..=steps.len() as u64).contains(&n) => Some(n as usize - 1),
        Some(_) => return Err(ApiError::bad(format!("step is 1 to {}", steps.len()))),
        None => steps.iter().position(|s| s["result"] == "failed").or_else(|| steps.iter().position(|s| s["result"] == "blocked")),
    };
    let key = case_key(&r.project_key, rc.number);
    let title = text(&req["title"], "the title", 300)?.unwrap_or_else(|| format!("{}: {}", key, rc.title));
    let mut md = format!("**Test case**: {key} {} (version {})\n\n**Test run**: {}-R{} {}\n\n", rc.title, rc.case_version, r.project_key, r.number, r.name);
    if let Some(env) = &r.environment {
        md.push_str(&format!("**Environment**: {env}\n\n"));
    }
    if let Some(p) = &rc.preconditions {
        md.push_str(&format!("**Preconditions**\n\n{p}\n\n"));
    }
    if !steps.is_empty() {
        md.push_str("**Steps**\n\n");
        for (i, s) in steps.iter().enumerate() {
            let mark = match s["result"].as_str().unwrap_or("") {
                "passed" => " — passed",
                "failed" => " — **failed**",
                "blocked" => " — blocked",
                "skipped" => " — skipped",
                _ => "",
            };
            md.push_str(&format!("{}. {}{mark}\n", i + 1, s["action"].as_str().unwrap_or("").replace('\n', " ")));
        }
        md.push('\n');
    }
    if let Some(i) = failing {
        let s = &steps[i];
        md.push_str(&format!("**Failing step**: {}. {}\n\n", i + 1, s["action"].as_str().unwrap_or("")));
        if let Some(e) = s["expected"].as_str().filter(|e| !e.is_empty()) {
            md.push_str(&format!("**Expected**: {e}\n\n"));
        }
        md.push_str(&format!("**Actual**: {}\n\n", s["comment"].as_str().or(rc.comment.as_deref()).unwrap_or("—")));
    } else if let Some(c) = &rc.comment {
        md.push_str(&format!("**Actual**: {c}\n\n"));
    }
    let mut fields = Map::new();
    let bug_type: Option<Uuid> = sqlx::query_scalar("SELECT id FROM issue_types WHERE project_id = $1 AND lower(name) = 'bug' LIMIT 1")
        .bind(r.project_id)
        .fetch_optional(&state.db)
        .await?;
    if let Some(t) = bug_type {
        fields.insert("typeId".into(), json!(t));
    }
    if let Some(v) = r.version_id {
        fields.insert("affectedVersionIds".into(), json!([v]));
    }
    let actor = Actor { user: Some(user.id), workspace: user.workspace_id, depth: 0 };
    let s = crate::issues::create(
        &state,
        &actor,
        crate::issues::NewIssue { project: r.project_id, title, description: Some(crate::doc::from_markdown(&md)), fields },
    )
    .await?;
    sqlx::query("INSERT INTO test_bugs (run_case_id, issue_id) VALUES ($1, $2)").bind(id).bind(s.id).execute(&state.db).await?;
    // the issues the case covers: the bug relates to them
    let covered: Vec<Uuid> = sqlx::query_scalar("SELECT issue_id FROM test_case_issues WHERE case_id = $1").bind(rc.case_id).fetch_all(&state.db).await?;
    for issue in covered {
        sqlx::query("INSERT INTO issue_links (id, source_id, target_id, kind, created_by) VALUES ($1, $2, $3, 'relates', $4) ON CONFLICT DO NOTHING")
            .bind(new_id())
            .bind(s.id)
            .bind(issue)
            .bind(user.id)
            .execute(&state.db)
            .await?;
    }
    // the files of the failing step (or all of the case's, without one)
    let files: Vec<(String, String, String, Option<i32>)> =
        sqlx::query_as("SELECT name, mime, storage_key, step FROM test_files WHERE run_case_id = $1 ORDER BY created_at").bind(id).fetch_all(&state.db).await?;
    for (name, mime, storage_key, step) in files {
        if failing.is_some() && step.is_some() && step != failing.map(|i| i as i32 + 1) {
            continue;
        }
        if let Ok(got) = state.blobs.get(&ObjPath::from(storage_key)).await
            && let Ok(bytes) = got.bytes().await
        {
            crate::routes::attachments::store(&state, s.id, r.project_id, Some(user.id), None, &name, &mime, bytes.to_vec()).await?;
        }
    }
    changed(&state, &user, r.project_id).await;
    Ok((StatusCode::CREATED, Json(json!({"id": s.id, "key": s.key, "title": s.title}))))
}

// ---- reports ----------------------------------------------------------------------------------------

#[derive(Deserialize, Default)]
pub struct ReportQuery {
    /// a release: its runs and the coverage of its issues
    pub version: Option<Uuid>,
    /// how far the history goes back (default 30, at most 365)
    pub days: Option<i64>,
}

/// Cases whose passed / failed results keep flipping: `(case, flips, last results oldest first)`.
fn flaky(events: &[(Uuid, String, DateTime<Utc>)]) -> Vec<(Uuid, usize, Vec<String>)> {
    let mut by_case: BTreeMap<Uuid, Vec<&str>> = BTreeMap::new();
    for (case, result, _) in events {
        if matches!(result.as_str(), "passed" | "failed") {
            by_case.entry(*case).or_default().push(result);
        }
    }
    let mut out: Vec<(Uuid, usize, Vec<String>)> = by_case
        .into_iter()
        .filter_map(|(case, all)| {
            let last: Vec<&str> = all.iter().rev().take(10).rev().copied().collect();
            let flips = last.windows(2).filter(|w| w[0] != w[1]).count();
            (flips >= 2).then(|| (case, flips, last.into_iter().map(str::to_string).collect()))
        })
        .collect();
    out.sort_by_key(|a| std::cmp::Reverse(a.1));
    out
}

/// Replays the results: for each day, how many cases stood at which result at its end.
fn history(events: &[(Uuid, String, DateTime<Utc>)], from: NaiveDate, to: NaiveDate) -> Vec<Value> {
    let mut state: HashMap<Uuid, &str> = HashMap::new();
    let mut out = Vec::new();
    let mut i = 0;
    let mut day = from;
    while day <= to {
        while i < events.len() && events[i].2.date_naive() <= day {
            state.insert(events[i].0, events[i].1.as_str());
            i += 1;
        }
        let count = |r: &str| state.values().filter(|v| **v == r).count();
        out.push(json!({"date": day, "passed": count("passed"), "failed": count("failed"), "blocked": count("blocked"), "skipped": count("skipped")}));
        day += chrono::Duration::days(1);
    }
    out
}

async fn report(state: &AppState, user: &User, p: Uuid, key: &str, version: Option<Uuid>, days: i64) -> ApiResult<Value> {
    let runs: Vec<RunRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "{RUN_SQL} WHERE r.project_id = $1 AND ($2::uuid IS NULL OR r.version_id = $2) ORDER BY r.number DESC LIMIT 30"
    )))
    .bind(p)
    .bind(version)
    .fetch_all(&state.db)
    .await?;
    // the history of results, oldest first (of the release's runs, with a release)
    let events: Vec<(Uuid, String, DateTime<Utc>)> = sqlx::query_as(
        "SELECT e.case_id, e.result, e.at FROM test_result_events e JOIN test_runs r ON r.id = e.run_id
          WHERE e.project_id = $1 AND ($2::uuid IS NULL OR r.version_id = $2) ORDER BY e.at, e.id",
    )
    .bind(p)
    .bind(version)
    .fetch_all(&state.db)
    .await?;
    let today = Utc::now().date_naive();
    let titles: HashMap<Uuid, (i32, String)> = sqlx::query_as::<_, (Uuid, i32, String)>("SELECT id, number, title FROM test_cases WHERE project_id = $1")
        .bind(p)
        .fetch_all(&state.db)
        .await?
        .into_iter()
        .map(|(id, n, t)| (id, (n, t)))
        .collect();
    let case_ref = |id: &Uuid| titles.get(id).map(|(n, t)| json!({"id": id, "key": case_key(key, *n), "title": t}));
    // what stands open: each case's latest result in these runs
    let open: Vec<(Uuid, Uuid, String, Uuid, i32, String)> = sqlx::query_as(
        "SELECT DISTINCT ON (rc.case_id) rc.id, rc.case_id, rc.result, r.id, r.number, r.name
           FROM test_run_cases rc JOIN test_runs r ON r.id = rc.run_id
          WHERE r.project_id = $1 AND ($2::uuid IS NULL OR r.version_id = $2) AND rc.result <> 'untested'
          ORDER BY rc.case_id, rc.executed_at DESC NULLS LAST",
    )
    .bind(p)
    .bind(version)
    .fetch_all(&state.db)
    .await?;
    let mut failures = Vec::new();
    for (rc, case, result, run, number, name) in open.iter().filter(|o| matches!(o.2.as_str(), "failed" | "blocked")) {
        let bugs = issue_lines(state, user, "i.id IN (SELECT issue_id FROM test_bugs WHERE run_case_id = $1)", *rc).await?;
        failures.push(json!({"case": case_ref(case), "result": result, "run": {"id": run, "key": format!("{key}-R{number}"), "name": name}, "bugs": bugs}));
    }
    // coverage: the release's issues (or, without one, the project's open ones) and their tests
    let issues: Vec<(Uuid, String, String, String, i64, i64, i64)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT i.id, p.key || '-' || i.number, i.title, s.category,
                (SELECT count(*) FROM test_case_issues ci JOIN test_cases c ON c.id = ci.case_id WHERE ci.issue_id = i.id AND c.archived_at IS NULL),
                (SELECT count(*) FROM test_case_issues ci JOIN test_cases c ON c.id = ci.case_id {LATEST} WHERE ci.issue_id = i.id AND c.archived_at IS NULL AND l.result IN ('failed', 'blocked')),
                (SELECT count(*) FROM test_case_issues ci JOIN test_cases c ON c.id = ci.case_id {LATEST} WHERE ci.issue_id = i.id AND c.archived_at IS NULL AND l.result = 'passed')
           FROM issues i JOIN projects p ON p.id = i.project_id JOIN statuses s ON s.id = i.status_id JOIN issue_types it ON it.id = i.type_id
          WHERE i.project_id = $1 AND i.deleted_at IS NULL AND it.level = 0 AND {}
            AND (CASE WHEN $2::uuid IS NULL THEN s.category <> 'done' ELSE i.id IN (SELECT issue_id FROM issue_versions WHERE version_id = $2 AND kind = 'fix') END)
          ORDER BY i.number LIMIT 2000",
        access::sees_issue("i", "$3")
    )))
    .bind(p)
    .bind(version)
    .bind(user.id)
    .fetch_all(&state.db)
    .await?;
    let line = |i: &(Uuid, String, String, String, i64, i64, i64)| json!({"id": i.0, "key": i.1, "title": i.2, "tests": i.4, "failing": i.5});
    let without: Vec<Value> = issues.iter().filter(|i| i.4 == 0).take(200).map(line).collect();
    let failing: Vec<Value> = issues.iter().filter(|i| i.5 > 0).take(200).map(line).collect();
    let total = issues.len();
    let covered = issues.iter().filter(|i| i.4 > 0).count();
    let passing = issues.iter().filter(|i| i.4 > 0 && i.5 == 0 && i.6 == i.4).count();
    let executed = |r: &RunRow| r.passed + r.failed + r.blocked;
    Ok(json!({
        "runs": runs.iter().map(|r| {
            let mut v = run_json(r);
            v["passRate"] = if executed(r) > 0 { json!((r.passed as f64 / executed(r) as f64 * 1000.0).round() / 10.0) } else { Value::Null };
            v
        }).collect::<Vec<_>>(),
        "history": history(&events, today - chrono::Duration::days(days - 1), today),
        "flaky": flaky(&events).into_iter().take(50).filter_map(|(case, flips, results)| Some(json!({"case": case_ref(&case)?, "flips": flips, "results": results}))).collect::<Vec<_>>(),
        "failures": failures,
        "coverage": {"issues": total, "covered": covered, "passing": passing, "withoutTests": without, "failing": failing,
                     "withoutTestsCount": issues.iter().filter(|i| i.4 == 0).count(), "failingCount": issues.iter().filter(|i| i.5 > 0).count()},
        "ready": failures.is_empty() && runs.iter().all(|r| r.total == r.passed + r.skipped) && !runs.is_empty(),
    }))
}

/// `GET /projects/{key}/test-report?version=&days=`: the runs with their pass rates, the history
/// of results by day, flaky cases, open failures and the coverage of issues.
pub async fn project_report(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Path(key): Path<String>,
    Query(q): Query<ReportQuery>,
) -> ApiResult<Json<Value>> {
    let (p, _) = project(&state, &user, &key).await?;
    Ok(Json(report(&state, &user, p.id, &p.key, q.version, q.days.unwrap_or(30).clamp(1, 365)).await?))
}

/// `GET /versions/{id}/tests`: whether a release is ready — its test runs, the failures that
/// stand open, and which of its issues have no tests or failing ones.
pub async fn release(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(id): Path<Uuid>) -> ApiResult<Json<Value>> {
    let row: Option<(Uuid, String)> =
        sqlx::query_as("SELECT p.id, p.key FROM versions v JOIN projects p ON p.id = v.project_id WHERE v.id = $1 AND p.workspace_id = $2")
            .bind(id)
            .bind(user.workspace_id)
            .fetch_optional(&state.db)
            .await?;
    let (p, key) = row.ok_or(ApiError::NotFound("release"))?;
    role_in(&state, &user, p).await.map_err(|e| if matches!(e, ApiError::NotFound(_)) { ApiError::NotFound("release") } else { e })?;
    Ok(Json(report(&state, &user, p, &key, Some(id), 30).await?))
}

// ---- CSV --------------------------------------------------------------------------------------------

fn csv_safe(t: &str) -> String {
    if t.starts_with(['=', '+', '-', '@', '\t', '\r']) { format!("'{t}") } else { t.to_string() }
}

fn folder_paths(folders: &[(Uuid, Option<Uuid>, String)]) -> HashMap<Uuid, String> {
    fn path(folders: &[(Uuid, Option<Uuid>, String)], id: Uuid, depth: u8) -> String {
        match folders.iter().find(|f| f.0 == id) {
            Some((_, Some(parent), name)) if depth < 20 => format!("{} / {name}", path(folders, *parent, depth + 1)),
            Some((_, _, name)) => name.clone(),
            None => String::new(),
        }
    }
    folders.iter().map(|f| (f.0, path(folders, f.0, 0))).collect()
}

/// `GET /projects/{key}/tests/export.csv`: Key, Title, Folder, Priority, Labels, Preconditions,
/// Steps (one per line, `action => expected`), Scenario, Covers.
pub async fn export(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(key): Path<String>) -> ApiResult<Response> {
    let (p, _) = project(&state, &user, &key).await?;
    let folders: Vec<(Uuid, Option<Uuid>, String)> =
        sqlx::query_as("SELECT id, parent_id, name FROM test_folders WHERE project_id = $1").bind(p.id).fetch_all(&state.db).await?;
    let paths = folder_paths(&folders);
    let cases: Vec<CaseRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!("{CASE_SQL} WHERE c.project_id = $1 AND c.archived_at IS NULL ORDER BY c.number")))
        .bind(p.id)
        .fetch_all(&state.db)
        .await?;
    let covers: Vec<(Uuid, String)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT ci.case_id, ip.key || '-' || i.number FROM test_case_issues ci JOIN test_cases c ON c.id = ci.case_id JOIN issues i ON i.id = ci.issue_id
           JOIN projects ip ON ip.id = i.project_id JOIN tc_project_roles($2) r ON r.project_id = i.project_id AND r.rank >= 1
          WHERE c.project_id = $1 AND i.deleted_at IS NULL AND {} ORDER BY ip.key, i.number",
        access::sees_issue("i", "$2")
    )))
    .bind(p.id)
    .bind(user.id)
    .fetch_all(&state.db)
    .await?;
    let mut w = csv::Writer::from_writer(Vec::new());
    let _ = w.write_record(["Key", "Title", "Folder", "Priority", "Labels", "Preconditions", "Steps", "Scenario", "Covers"]);
    for c in &cases {
        let steps: Vec<String> = c
            .steps
            .as_array()
            .into_iter()
            .flatten()
            .map(|s| match s["expected"].as_str().filter(|e| !e.is_empty()) {
                Some(e) => format!("{} => {e}", s["action"].as_str().unwrap_or("").replace('\n', " ")),
                None => s["action"].as_str().unwrap_or("").replace('\n', " "),
            })
            .collect();
        let _ = w.write_record([
            case_key(&p.key, c.number),
            csv_safe(&c.title),
            csv_safe(c.folder_id.and_then(|f| paths.get(&f)).map(String::as_str).unwrap_or("")),
            c.priority.clone(),
            csv_safe(&c.labels.join(", ")),
            csv_safe(c.preconditions.as_deref().unwrap_or("")),
            csv_safe(&steps.join("\n")),
            csv_safe(c.scenario.as_deref().unwrap_or("")),
            covers.iter().filter(|x| x.0 == c.id).map(|x| x.1.as_str()).collect::<Vec<_>>().join(", "),
        ]);
    }
    let body = w.into_inner().map_err(|e| ApiError::Internal(anyhow::anyhow!("csv: {e}")))?;
    Ok(([(header::CONTENT_TYPE, "text/csv; charset=utf-8"), (header::CONTENT_DISPOSITION, "attachment; filename=\"test-cases.csv\"")], body).into_response())
}

/// `POST /projects/{key}/tests/import` `{csv, dryRun?}`: test cases from a CSV file with the
/// columns of the export (only Title is needed). A row whose Key names a case of the project
/// updates it; folders are created as needed.
pub async fn import(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(key): Path<String>, Json(req): Json<Value>) -> ApiResult<Json<Value>> {
    let (p, role) = project(&state, &user, &key).await?;
    manage(&state, p.id, role).await?;
    let text_in = req["csv"].as_str().ok_or_else(|| ApiError::bad("csv is the file's text"))?;
    if text_in.len() > 5 * 1024 * 1024 {
        return Err(ApiError::TooLarge);
    }
    let dry = req["dryRun"].as_bool().unwrap_or(false);
    let mut reader = csv::ReaderBuilder::new().flexible(true).from_reader(text_in.trim_start_matches('\u{feff}').as_bytes());
    let headers: Vec<String> =
        reader.headers().map_err(|e| ApiError::bad(format!("this isn't a CSV file: {e}")))?.iter().map(|h| h.trim().to_lowercase()).collect();
    let col = |name: &str| headers.iter().position(|h| h == name);
    let Some(title_col) = col("title") else { return Err(ApiError::bad("the file needs a Title column")) };
    let (key_col, folder_col, priority_col, labels_col, pre_col, steps_col, scenario_col, covers_col) =
        (col("key"), col("folder"), col("priority"), col("labels"), col("preconditions"), col("steps"), col("scenario"), col("covers"));
    let mut folders: Vec<(Uuid, Option<Uuid>, String)> =
        sqlx::query_as("SELECT id, parent_id, name FROM test_folders WHERE project_id = $1").bind(p.id).fetch_all(&state.db).await?;
    let (mut created, mut updated, mut errors) = (0, 0, Vec::new());
    for (n, rec) in reader.records().enumerate() {
        let row = n + 2;
        if n >= 5000 {
            errors.push(json!({"row": row, "message": "only the first 5000 rows are imported"}));
            break;
        }
        let rec = match rec {
            Ok(r) => r,
            Err(e) => {
                errors.push(json!({"row": row, "message": e.to_string()}));
                continue;
            }
        };
        let cell = |c: Option<usize>| c.and_then(|c| rec.get(c)).map(|s| s.trim().trim_start_matches('\'').to_string()).filter(|s| !s.is_empty());
        if rec.iter().all(|c| c.trim().is_empty()) {
            continue;
        }
        let mut body = Map::new();
        if let Some(t) = cell(Some(title_col)) {
            body.insert("title".into(), json!(t));
        }
        if let Some(v) = cell(priority_col) {
            body.insert("priority".into(), json!(v));
        }
        if let Some(v) = cell(labels_col) {
            body.insert("labels".into(), json!(v.split(',').map(str::trim).filter(|s| !s.is_empty()).collect::<Vec<_>>()));
        }
        if let Some(v) = cell(pre_col) {
            body.insert("preconditions".into(), json!(v));
        }
        if let Some(v) = cell(steps_col) {
            let steps: Vec<Value> = v
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(|l| match l.split_once("=>") {
                    Some((a, e)) => json!({"action": a.trim(), "expected": e.trim()}),
                    None => json!({"action": l, "expected": ""}),
                })
                .collect();
            body.insert("steps".into(), json!(steps));
            body.insert("kind".into(), json!("steps"));
        }
        if let Some(v) = cell(scenario_col) {
            body.insert("scenario".into(), json!(v));
            if !body.contains_key("steps") {
                body.insert("kind".into(), json!("scenario"));
            }
        }
        let existing = match cell(key_col) {
            Some(k) => match case_for(&state, &user, &k).await {
                Ok((c, _)) if c.project_id == p.id => Some(c),
                _ => {
                    errors.push(json!({"row": row, "message": format!("there is no test case {k} in this project")}));
                    continue;
                }
            },
            None => None,
        };
        // checks, as far as they go without writing
        let checked = content(&body, existing.as_ref())
            .and_then(|_| body.get("priority").map(clean_priority).transpose())
            .and_then(|_| body.get("labels").map(clean_labels).transpose());
        if let Err(e) = checked {
            errors.push(json!({"row": row, "message": e.to_string()}));
            continue;
        }
        let covers: Option<Vec<String>> = cell(covers_col).map(|v| v.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect());
        if let Some(keys) = &covers {
            let mut missing = None;
            for k in keys {
                if crate::issues::resolve(&state, &user, k, false).await.is_err() {
                    missing = Some(k.clone());
                    break;
                }
            }
            if let Some(k) = missing {
                errors.push(json!({"row": row, "message": format!("there is no issue {k}")}));
                continue;
            }
        }
        if dry {
            if existing.is_some() {
                updated += 1
            } else {
                created += 1
            }
            continue;
        }
        // the folder, by its path ("Checkout / Payment"), made when it isn't there
        if let Some(path) = cell(folder_col) {
            let mut parent: Option<Uuid> = None;
            for name in path.split('/').map(str::trim).filter(|s| !s.is_empty()) {
                let name: String = name.chars().take(80).collect();
                parent = Some(match folders.iter().find(|f| f.1 == parent && f.2.eq_ignore_ascii_case(&name)) {
                    Some(f) => f.0,
                    None => {
                        let id = new_id();
                        sqlx::query("INSERT INTO test_folders (id, project_id, parent_id, name, position) VALUES ($1, $2, $3, $4, (SELECT COALESCE(max(position), 0) + 1 FROM test_folders WHERE project_id = $2))")
                            .bind(id)
                            .bind(p.id)
                            .bind(parent)
                            .bind(&name)
                            .execute(&state.db)
                            .await?;
                        folders.push((id, parent, name));
                        id
                    }
                });
            }
            body.insert("folderId".into(), json!(parent));
        }
        let done = match &existing {
            Some(c) => update(State(state.clone()), CurrentUser(user.clone()), Path(c.id.to_string()), Json(body)).await.map(|_| c.id),
            None => new_case(&state, Some(user.id), p.id, &body).await.map(|(id, _)| id),
        };
        match done {
            Ok(id) => {
                if let Some(keys) = covers {
                    set_coverage(&state, &user, id, &json!(keys)).await?;
                }
                if existing.is_some() { updated += 1 } else { created += 1 }
            }
            Err(e) => errors.push(json!({"row": row, "message": e.to_string()})),
        }
    }
    if !dry {
        changed(&state, &user, p.id).await;
    }
    Ok(Json(json!({"dryRun": dry, "created": created, "updated": updated, "errors": errors})))
}

// ---- project exports --------------------------------------------------------------------------------

/// A project's test management for its export (None when it has none): folders, test cases with
/// their versions and the issues they cover, plans, and runs with their results. Files of
/// results stay behind.
pub async fn export_json(db: &sqlx::PgPool, project: Uuid) -> anyhow::Result<Option<Value>> {
    let one = |sql: &'static str| async move { sqlx::query_scalar::<_, Value>(sql).bind(project).fetch_one(db).await };
    let cases = one(
        "SELECT COALESCE(jsonb_agg(jsonb_build_object('id', c.id, 'number', c.number, 'folderId', c.folder_id, 'title', c.title, 'preconditions', c.preconditions,
                'kind', c.kind, 'steps', c.steps, 'scenario', c.scenario, 'priority', c.priority, 'labels', c.labels, 'version', c.version, 'archivedAt', c.archived_at,
                'createdAt', c.created_at, 'updatedAt', c.updated_at,
                'issues', (SELECT COALESCE(jsonb_agg(ci.issue_id), '[]') FROM test_case_issues ci WHERE ci.case_id = c.id),
                'versions', (SELECT COALESCE(jsonb_agg(jsonb_build_object('version', v.version, 'title', v.title, 'preconditions', v.preconditions, 'kind', v.kind,
                        'steps', v.steps, 'scenario', v.scenario, 'createdAt', v.created_at) ORDER BY v.version), '[]') FROM test_case_versions v WHERE v.case_id = c.id)) ORDER BY c.number), '[]')
           FROM test_cases c WHERE c.project_id = $1",
    )
    .await?;
    if cases.as_array().is_none_or(Vec::is_empty) {
        return Ok(None);
    }
    let folders = one("SELECT COALESCE(jsonb_agg(jsonb_build_object('id', id, 'parentId', parent_id, 'name', name, 'position', position)), '[]') FROM test_folders WHERE project_id = $1").await?;
    let plans = one(
        "SELECT COALESCE(jsonb_agg(jsonb_build_object('id', tp.id, 'name', tp.name, 'description', tp.description, 'versionId', tp.version_id, 'filter', tp.filter, 'createdAt', tp.created_at,
                'caseIds', (SELECT COALESCE(jsonb_agg(pc.case_id ORDER BY pc.position), '[]') FROM test_plan_cases pc WHERE pc.plan_id = tp.id))), '[]')
           FROM test_plans tp WHERE tp.project_id = $1",
    )
    .await?;
    let runs = one(
        "SELECT COALESCE(jsonb_agg(jsonb_build_object('id', r.id, 'number', r.number, 'name', r.name, 'planId', r.plan_id, 'versionId', r.version_id, 'environment', r.environment,
                'createdAt', r.created_at, 'finishedAt', r.finished_at,
                'cases', (SELECT COALESCE(jsonb_agg(jsonb_build_object('caseId', rc.case_id, 'caseVersion', rc.case_version, 'position', rc.position, 'result', rc.result,
                        'comment', rc.comment, 'steps', rc.steps, 'executedAt', rc.executed_at) ORDER BY rc.position), '[]') FROM test_run_cases rc WHERE rc.run_id = r.id)) ORDER BY r.number), '[]')
           FROM test_runs r WHERE r.project_id = $1",
    )
    .await?;
    Ok(Some(json!({"folders": folders, "cases": cases, "plans": plans, "runs": runs})))
}

/// Restores an export's test management into a project that has none yet. `issues` and
/// `versions` map the export's ids to the ones the import made.
pub async fn import_json(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    project: Uuid,
    v: &Value,
    issues: &HashMap<String, Uuid>,
    versions: &HashMap<String, Uuid>,
) -> anyhow::Result<()> {
    let has: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM test_cases WHERE project_id = $1)").bind(project).fetch_one(&mut **tx).await?;
    if has {
        return Ok(());
    }
    let list = |k: &str| v[k].as_array().cloned().unwrap_or_default();
    let s = |x: &Value| x.as_str().map(str::to_string);
    let at = |x: &Value| x.as_str().and_then(|t| t.parse::<DateTime<Utc>>().ok());
    let version = |x: &Value| x.as_str().and_then(|id| versions.get(id)).copied();
    // folders: parents before their children
    let mut folders: HashMap<String, Uuid> = HashMap::new();
    let mut left = list("folders");
    while !left.is_empty() {
        let before = left.len();
        let mut later = Vec::new();
        for f in left {
            let parent = match f["parentId"].as_str() {
                Some(p) => match folders.get(p) {
                    Some(id) => Some(*id),
                    None => {
                        later.push(f);
                        continue;
                    }
                },
                None => None,
            };
            let id = new_id();
            sqlx::query("INSERT INTO test_folders (id, project_id, parent_id, name, position) VALUES ($1, $2, $3, $4, $5)")
                .bind(id)
                .bind(project)
                .bind(parent)
                .bind(f["name"].as_str().unwrap_or("Folder"))
                .bind(f["position"].as_f64().unwrap_or(0.0))
                .execute(&mut **tx)
                .await?;
            folders.insert(f["id"].as_str().unwrap_or_default().to_string(), id);
        }
        if later.len() == before {
            break;
        }
        left = later;
    }
    let mut cases: HashMap<String, Uuid> = HashMap::new();
    let mut top = 0;
    for c in list("cases") {
        let id = new_id();
        let number = c["number"].as_i64().unwrap_or(0) as i32;
        top = top.max(number);
        let labels: Vec<String> = c["labels"].as_array().into_iter().flatten().filter_map(|l| l.as_str().map(str::to_string)).collect();
        sqlx::query(
            "INSERT INTO test_cases (id, project_id, number, folder_id, title, preconditions, kind, steps, scenario, priority, labels, version, archived_at, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, COALESCE($14, now()), COALESCE($15, now()))",
        )
        .bind(id)
        .bind(project)
        .bind(number)
        .bind(c["folderId"].as_str().and_then(|f| folders.get(f)).copied())
        .bind(c["title"].as_str().unwrap_or("Test case"))
        .bind(s(&c["preconditions"]))
        .bind(c["kind"].as_str().unwrap_or("steps"))
        .bind(&c["steps"])
        .bind(s(&c["scenario"]))
        .bind(c["priority"].as_str().unwrap_or("medium"))
        .bind(&labels)
        .bind(c["version"].as_i64().unwrap_or(1) as i32)
        .bind(at(&c["archivedAt"]))
        .bind(at(&c["createdAt"]))
        .bind(at(&c["updatedAt"]))
        .execute(&mut **tx)
        .await?;
        for ver in c["versions"].as_array().into_iter().flatten() {
            sqlx::query("INSERT INTO test_case_versions (case_id, version, title, preconditions, kind, steps, scenario, created_at) VALUES ($1, $2, $3, $4, $5, $6, $7, COALESCE($8, now()))")
                .bind(id)
                .bind(ver["version"].as_i64().unwrap_or(1) as i32)
                .bind(ver["title"].as_str().unwrap_or(""))
                .bind(s(&ver["preconditions"]))
                .bind(ver["kind"].as_str().unwrap_or("steps"))
                .bind(&ver["steps"])
                .bind(s(&ver["scenario"]))
                .bind(at(&ver["createdAt"]))
                .execute(&mut **tx)
                .await?;
        }
        let covered: Vec<Uuid> = c["issues"].as_array().into_iter().flatten().filter_map(|i| i.as_str().and_then(|i| issues.get(i)).copied()).collect();
        sqlx::query("INSERT INTO test_case_issues (case_id, issue_id) SELECT $1, unnest($2::uuid[]) ON CONFLICT DO NOTHING")
            .bind(id)
            .bind(&covered)
            .execute(&mut **tx)
            .await?;
        cases.insert(c["id"].as_str().unwrap_or_default().to_string(), id);
    }
    let case_list = |x: &Value| -> Vec<Uuid> { x.as_array().into_iter().flatten().filter_map(|c| c.as_str().and_then(|c| cases.get(c)).copied()).collect() };
    let mut plans: HashMap<String, Uuid> = HashMap::new();
    for p in list("plans") {
        let id = new_id();
        let mut filter = p["filter"].clone();
        if let Some(f) = filter["folderId"].as_str().map(str::to_string) {
            filter["folderId"] = json!(folders.get(&f));
        }
        sqlx::query("INSERT INTO test_plans (id, project_id, name, description, version_id, filter, created_at) VALUES ($1, $2, $3, $4, $5, $6, COALESCE($7, now())) ON CONFLICT DO NOTHING")
            .bind(id)
            .bind(project)
            .bind(p["name"].as_str().unwrap_or("Plan"))
            .bind(s(&p["description"]))
            .bind(version(&p["versionId"]))
            .bind(if filter.is_null() { None } else { Some(filter) })
            .bind(at(&p["createdAt"]))
            .execute(&mut **tx)
            .await?;
        let ids = case_list(&p["caseIds"]);
        let positions: Vec<i32> = (0..ids.len() as i32).collect();
        sqlx::query("INSERT INTO test_plan_cases (plan_id, case_id, position) SELECT $1, * FROM unnest($2::uuid[], $3::int[]) ON CONFLICT DO NOTHING")
            .bind(id)
            .bind(&ids)
            .bind(&positions)
            .execute(&mut **tx)
            .await?;
        plans.insert(p["id"].as_str().unwrap_or_default().to_string(), id);
    }
    let mut top_run = 0;
    for r in list("runs") {
        let id = new_id();
        let number = r["number"].as_i64().unwrap_or(0) as i32;
        top_run = top_run.max(number);
        sqlx::query("INSERT INTO test_runs (id, project_id, number, name, plan_id, version_id, environment, created_at, finished_at) VALUES ($1, $2, $3, $4, $5, $6, $7, COALESCE($8, now()), $9)")
            .bind(id)
            .bind(project)
            .bind(number)
            .bind(r["name"].as_str().unwrap_or("Test run"))
            .bind(r["planId"].as_str().and_then(|p| plans.get(p)).copied())
            .bind(version(&r["versionId"]))
            .bind(s(&r["environment"]))
            .bind(at(&r["createdAt"]))
            .bind(at(&r["finishedAt"]))
            .execute(&mut **tx)
            .await?;
        for rc in r["cases"].as_array().into_iter().flatten() {
            let Some(case) = rc["caseId"].as_str().and_then(|c| cases.get(c)).copied() else { continue };
            let result = rc["result"].as_str().filter(|x| RESULTS.contains(x)).unwrap_or("untested");
            sqlx::query("INSERT INTO test_run_cases (id, run_id, case_id, case_version, position, result, comment, steps, executed_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) ON CONFLICT DO NOTHING")
                .bind(new_id())
                .bind(id)
                .bind(case)
                .bind(rc["caseVersion"].as_i64().unwrap_or(1) as i32)
                .bind(rc["position"].as_i64().unwrap_or(0) as i32)
                .bind(result)
                .bind(s(&rc["comment"]))
                .bind(&rc["steps"])
                .bind(at(&rc["executedAt"]))
                .execute(&mut **tx)
                .await?;
            // the history the reports replay: where each case ended up in this run
            if result != "untested" {
                sqlx::query("INSERT INTO test_result_events (id, project_id, run_id, case_id, result, at) VALUES ($1, $2, $3, $4, $5, COALESCE($6, now()))")
                    .bind(new_id())
                    .bind(project)
                    .bind(id)
                    .bind(case)
                    .bind(result)
                    .bind(at(&rc["executedAt"]))
                    .execute(&mut **tx)
                    .await?;
            }
        }
    }
    sqlx::query(
        "UPDATE projects SET next_test_number = GREATEST(next_test_number, $2), next_test_run_number = GREATEST(next_test_run_number, $3) WHERE id = $1",
    )
    .bind(project)
    .bind(top + 1)
    .bind(top_run + 1)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_and_results() {
        let scenario = "Feature: Checkout\n  Given a cart with one item\n  When I pay\n  And the card is declined\n  Then I see an error\n# note";
        assert_eq!(steps_of("scenario", &json!([]), Some(scenario)).len(), 4);
        assert_eq!(steps_of("steps", &json!([{"action": "a", "expected": "b"}]), Some(scenario)).len(), 1);
        assert_eq!(overall(&["passed", "failed", "blocked"]), "failed");
        assert_eq!(overall(&["passed", "blocked"]), "blocked");
        assert_eq!(overall(&["passed", "untested"]), "untested");
        assert_eq!(overall(&["skipped", "skipped"]), "skipped");
        assert_eq!(overall(&["passed", "skipped"]), "passed");
        assert_eq!(overall(&[]), "untested");
        assert!(clean_steps(&json!([{"expected": "x"}])).is_err());
        assert_eq!(clean_steps(&json!([{"action": " Open the page "}])).unwrap(), json!([{"action": "Open the page", "expected": ""}]));
        assert_eq!(clean_labels(&json!(["smoke", "Smoke", "ui"])).unwrap(), ["smoke", "ui"]);
    }

    #[test]
    fn replays_history() {
        let (a, b) = (Uuid::from_u128(1), Uuid::from_u128(2));
        let at = |d: u32| chrono::NaiveDate::from_ymd_opt(2026, 3, d).unwrap().and_hms_opt(12, 0, 0).unwrap().and_utc();
        let events = vec![
            (a, "passed".to_string(), at(1)),
            (b, "failed".to_string(), at(1)),
            (a, "failed".to_string(), at(2)),
            (b, "passed".to_string(), at(3)),
            (a, "passed".to_string(), at(3)),
            (a, "failed".to_string(), at(4)),
        ];
        let h = history(&events, at(1).date_naive(), at(4).date_naive());
        assert_eq!((h[0]["passed"].as_u64(), h[0]["failed"].as_u64()), (Some(1), Some(1)));
        assert_eq!((h[1]["passed"].as_u64(), h[1]["failed"].as_u64()), (Some(0), Some(2)));
        assert_eq!((h[2]["passed"].as_u64(), h[3]["failed"].as_u64()), (Some(2), Some(1)));
        let f = flaky(&events);
        assert_eq!(f.len(), 1, "b only flipped once");
        assert_eq!((f[0].0, f[0].1), (a, 3));
    }
}
