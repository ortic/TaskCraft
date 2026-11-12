//! Built-in MCP (Model Context Protocol) server at `/api/mcp` (and `/api/v1/mcp`), streamable HTTP.
//!
//! AI assistants authenticate with a personal API token (`Authorization: Bearer tct_…`) — or a
//! browser session with the CSRF header — and act with exactly that person's permissions: the
//! tools go through the same handlers and checks as the web app (project roles, restricted
//! workflows, archived projects). Read-only tokens may call the reading tools only.
//!
//! The server is stateless (no MCP sessions kept in memory), so it works behind several replicas.

use std::collections::BTreeMap;

use axum::extract::{FromRequestParts, Json as AxJson, Path, Query, State};
use axum::http::request::Parts;
use rmcp::handler::server::tool::Extension;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerConfig};
use rmcp::transport::streamable_http_server::session::never::NeverSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use rmcp::{ErrorData, ServerHandler, tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use uuid::Uuid;

use crate::access::{self, Role};
use crate::app::AppState;
use crate::auth::{CurrentUser, MaybeUser, ScopeCheckedByCaller, TokenScope, User};
use crate::error::{ApiError, ApiResult};
use crate::issues::{self, Actor, Summary};
use crate::routes;

#[derive(Clone)]
pub struct TaskMcp {
    state: AppState,
}

type ToolResult = Result<CallToolResult, ErrorData>;

fn done(r: ApiResult<String>) -> ToolResult {
    Ok(match r {
        Ok(text) => CallToolResult::success(vec![ContentBlock::text(text)]),
        Err(e) => CallToolResult::error(vec![ContentBlock::text(e.to_string())]),
    })
}

fn compact(v: &Value) -> String {
    serde_json::to_string(v).unwrap_or_default()
}

fn bad<T>(msg: impl Into<String>) -> ApiResult<T> {
    Err(ApiError::BadRequest(msg.into()))
}

/// `1h 30m`
fn duration(minutes: i64) -> String {
    match (minutes / 60, minutes % 60) {
        (0, m) => format!("{m}m"),
        (h, 0) => format!("{h}h"),
        (h, m) => format!("{h}h {m}m"),
    }
}

/// An issue in a list: the fields people scan for, empty ones left out.
fn brief(s: &Summary) -> Value {
    let mut m = Map::new();
    m.insert("key".into(), json!(s.key));
    m.insert("title".into(), json!(s.title));
    m.insert("type".into(), json!(s.kind.name));
    m.insert("status".into(), json!(s.status.name));
    m.insert("category".into(), json!(s.status.category));
    m.insert("priority".into(), json!(s.priority));
    if let Some(a) = &s.assignee {
        m.insert("assignee".into(), json!(a.name));
    }
    if let Some(p) = &s.parent {
        m.insert("parent".into(), json!(p.key));
    }
    if let Some(sp) = &s.sprint {
        m.insert("sprint".into(), json!(sp.name));
    }
    if let Some(p) = s.story_points {
        m.insert("points".into(), json!(p));
    }
    if !s.labels.is_empty() {
        m.insert("labels".into(), json!(s.labels));
    }
    if let Some(d) = s.due_date {
        m.insert("due".into(), json!(d));
    }
    if s.blocked {
        m.insert("blocked".into(), json!(true));
    }
    m.insert("updated".into(), json!(s.updated_at.format("%Y-%m-%d %H:%M").to_string()));
    m.insert("url".into(), json!(s.url));
    Value::Object(m)
}

/// `WEB-12` from a key or an issue URL.
fn clean_key(k: &str) -> String {
    k.trim().trim_end_matches('/').rsplit('/').next().unwrap_or("").to_string()
}

fn yes(b: bool) -> &'static str {
    if b { "yes" } else { "no" }
}

/// Drops presentation-only keys (colours, icons) from report figures.
fn strip(v: &mut Value) {
    match v {
        Value::Object(o) => {
            o.remove("color");
            o.remove("icon");
            o.values_mut().for_each(strip);
        }
        Value::Array(a) => a.iter_mut().for_each(strip),
        _ => {}
    }
}

// ---- arguments ----

const TQL_HELP: &str = "Search issues with TQL, TaskCraft's query language. Returns the total and the matching issues (key, title, type, status, category, priority, assignee, sprint, points, labels, due date, URL).

TQL: clauses `field operator value` joined with `and`, `or`, `not` and parentheses, optionally followed by `order by field [asc|desc], …`. Bare words search titles and descriptions (`login timeout`).
Fields: project, key, title, text (title, description and more), description, comment, type, status, category (todo | in_progress | done), priority (none < low < medium < high < urgent), assignee, reporter, creator, watcher, labels, sprint, version, component, parent / epic (an issue key), points, estimate and spent (hours), created, updated, resolved, due, start, blocked (true | false), and custom fields by name (quote names with spaces: \"Customer tier\" = Gold).
Operators: = != ~ (contains) !~ > >= < <=, in (a, b), not in (a, b), is empty, is not empty.
Values: quote values with spaces (status = \"In progress\"); people by name, e-mail or me(); dates as 2026-11-12, relative (-7d, +2w, -1m) or now(), today(), startOfWeek(), endOfWeek(), startOfMonth(), endOfMonth(); sprints by name or openSprints() (the running ones), futureSprints(), closedSprints(); versions by name or releasedVersions(), unreleasedVersions().
Examples:
  project = WEB and sprint in openSprints() and assignee = me() order by rank
  type = Bug and priority >= high and created > -14d
  category != done and due < endOfWeek()
  labels is empty or assignee is empty
  project = WEB and text ~ \"payment\" order by updated desc
Status and type names differ per project: list_projects shows them.";

#[derive(Deserialize, JsonSchema)]
pub struct SearchArgs {
    /// A TQL query, e.g. `project = WEB and category != done order by priority desc`
    pub query: String,
    /// Only issues of this project (its key, e.g. WEB)
    pub project_key: Option<String>,
    /// Max results (default 25, at most 100)
    pub limit: Option<i64>,
    /// Skip this many results (paging)
    pub offset: Option<i64>,
}

#[derive(Deserialize, JsonSchema)]
pub struct IssueArgs {
    /// Issue key, e.g. WEB-12
    pub key: String,
    /// How many of the latest comments to include (default 20)
    pub max_comments: Option<usize>,
}

#[derive(Deserialize, JsonSchema)]
pub struct CreateIssueArgs {
    /// Project key, e.g. WEB
    pub project_key: String,
    pub title: String,
    /// Description in Markdown (headings, lists, `- [ ]` task lists, code, links)
    pub description: Option<String>,
    /// Issue type by name (e.g. Story, Task, Bug, Epic, Sub-task); default: the project's first standard type
    #[serde(rename = "type")]
    pub kind: Option<String>,
    /// none, low, medium, high or urgent
    pub priority: Option<String>,
    /// Assignee by name or e-mail, or "me"
    pub assignee: Option<String>,
    pub labels: Option<Vec<String>>,
    /// Parent issue key: the epic of a standard issue, or the issue a sub-task belongs to
    pub parent: Option<String>,
    /// Sprint by name, or "active" for the running sprint; leave out for the backlog
    pub sprint: Option<String>,
    pub story_points: Option<f64>,
    /// Original estimate like "2h 30m", "45m" or "2d" (8-hour days)
    pub estimate: Option<String>,
    /// Due date, YYYY-MM-DD
    pub due_date: Option<String>,
    /// Start date, YYYY-MM-DD
    pub start_date: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct UpdateIssueArgs {
    /// Issue key, e.g. WEB-12
    pub key: String,
    pub title: Option<String>,
    /// New description in Markdown (replaces the current one)
    pub description: Option<String>,
    /// Status by name (e.g. "In progress"); restricted workflows only allow their transitions
    pub status: Option<String>,
    /// Issue type by name
    #[serde(rename = "type")]
    pub kind: Option<String>,
    /// none, low, medium, high or urgent
    pub priority: Option<String>,
    /// Assignee by name or e-mail, "me", or "none" to unassign
    pub assignee: Option<String>,
    /// Replaces all labels
    pub labels: Option<Vec<String>>,
    pub add_labels: Option<Vec<String>>,
    pub remove_labels: Option<Vec<String>>,
    /// Parent issue key (epic or parent issue), or "none"
    pub parent: Option<String>,
    /// Sprint by name, "active" for the running sprint, or "backlog"
    pub sprint: Option<String>,
    pub story_points: Option<f64>,
    /// Remaining estimate like "2h 30m", or "none"
    pub estimate: Option<String>,
    /// YYYY-MM-DD, or "none"
    pub due_date: Option<String>,
    /// YYYY-MM-DD, or "none"
    pub start_date: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct CommentArgs {
    /// Issue key, e.g. WEB-12
    pub key: String,
    /// The comment in Markdown
    pub text: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct WorkArgs {
    /// Issue key, e.g. WEB-12
    pub key: String,
    /// Time spent: "1h 30m", "45m", "2d" (8-hour days) or minutes
    pub time: String,
    /// The day the work was done, YYYY-MM-DD (default today)
    pub date: Option<String>,
    /// What was done
    pub note: Option<String>,
    /// Also lower the remaining estimate by this time
    pub reduce_estimate: Option<bool>,
}

#[derive(Deserialize, JsonSchema)]
pub struct ProjectsArgs {
    /// One project's full setup (workflow transitions, versions, components, custom fields, people)
    pub project_key: Option<String>,
    /// Include archived projects
    pub include_archived: Option<bool>,
}

#[derive(Deserialize, JsonSchema)]
pub struct SprintArgs {
    /// Project key; without `sprint` its running sprint
    pub project_key: Option<String>,
    /// Sprint name or id
    pub sprint: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct BoardArgs {
    /// Project key (its first board unless `board` names another)
    pub project_key: Option<String>,
    /// Board name or id
    pub board: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct ReportArgs {
    /// Project key
    pub project_key: String,
    /// overview, burndown, velocity, cumulative-flow, cycle-time, created-resolved or workload
    pub report: String,
    /// burndown: sprint name or id (default: the running sprint)
    pub sprint: Option<String>,
    /// cumulative-flow, cycle-time, created-resolved: days back (default 30; cycle time 90)
    pub days: Option<i64>,
    /// velocity: how many closed sprints (default 8)
    pub count: Option<i64>,
}

// ---- lookups by name ----

async fn find_type(state: &AppState, project: Uuid, name: &str) -> ApiResult<Uuid> {
    let types: Vec<(Uuid, String)> =
        sqlx::query_as("SELECT id, name FROM issue_types WHERE project_id = $1 ORDER BY position").bind(project).fetch_all(&state.db).await?;
    types.iter().find(|(_, n)| n.eq_ignore_ascii_case(name.trim())).map(|(id, _)| *id).ok_or_else(|| {
        ApiError::BadRequest(format!("no issue type {name:?} in this project; types: {}", types.iter().map(|(_, n)| n.as_str()).collect::<Vec<_>>().join(", ")))
    })
}

async fn find_status(state: &AppState, project: Uuid, name: &str) -> ApiResult<Uuid> {
    let statuses: Vec<(Uuid, String)> =
        sqlx::query_as("SELECT id, name FROM statuses WHERE project_id = $1 ORDER BY position").bind(project).fetch_all(&state.db).await?;
    statuses.iter().find(|(_, n)| n.eq_ignore_ascii_case(name.trim())).map(|(id, _)| *id).ok_or_else(|| {
        ApiError::BadRequest(format!(
            "no status {name:?} in this project; statuses: {}",
            statuses.iter().map(|(_, n)| n.as_str()).collect::<Vec<_>>().join(", ")
        ))
    })
}

fn clears(v: &str) -> bool {
    matches!(v.trim().to_ascii_lowercase().as_str(), "" | "none" | "nobody" | "null" | "unassigned" | "-")
}

/// A person who can see the project, by e-mail, name or "me" (None: "none").
async fn find_person(state: &AppState, user: &User, project: Uuid, who: &str) -> ApiResult<Option<Uuid>> {
    let w = who.trim().to_lowercase();
    if clears(&w) {
        return Ok(None);
    }
    if matches!(w.as_str(), "me" | "me()" | "myself") {
        return Ok(Some(user.id));
    }
    let people: Vec<(Uuid, String, Option<String>)> = sqlx::query_as(
        "SELECT u.id, u.name, u.email FROM users u CROSS JOIN LATERAL (SELECT rank FROM tc_project_roles(u.id) WHERE project_id = $1) r
          WHERE u.workspace_id = $2 AND u.status = 'active' AND r.rank >= 1 ORDER BY lower(u.name)",
    )
    .bind(project)
    .bind(user.workspace_id)
    .fetch_all(&state.db)
    .await?;
    let exact: Vec<&(Uuid, String, Option<String>)> =
        people.iter().filter(|(_, n, e)| n.to_lowercase() == w || e.as_deref().is_some_and(|e| e.to_lowercase() == w)).collect();
    let found = if exact.is_empty() { people.iter().filter(|(_, n, _)| n.to_lowercase().contains(&w)).collect() } else { exact };
    match found.as_slice() {
        [one] => Ok(Some(one.0)),
        [] => bad(format!("nobody called {who:?} has access to this project")),
        many => bad(format!("{who:?} matches several people: {}", many.iter().map(|p| p.1.as_str()).collect::<Vec<_>>().join(", "))),
    }
}

/// A sprint of the project by name or id, "active" / "next"; None for "backlog" / "none".
async fn find_sprint(state: &AppState, project: Uuid, name: &str) -> ApiResult<Option<Uuid>> {
    let n = name.trim().to_lowercase();
    if clears(&n) || n == "backlog" {
        return Ok(None);
    }
    let sprints: Vec<(Uuid, String, String)> = sqlx::query_as(
        "SELECT id, name, state FROM sprints WHERE project_id = $1 ORDER BY CASE state WHEN 'active' THEN 0 WHEN 'future' THEN 1 ELSE 2 END, start_at, position, created_at",
    )
    .bind(project)
    .fetch_all(&state.db)
    .await?;
    let hit = match n.as_str() {
        "active" | "current" | "running" | "opensprints()" => sprints.iter().find(|s| s.2 == "active"),
        "next" | "future" => sprints.iter().find(|s| s.2 == "future"),
        _ => sprints.iter().find(|s| s.1.to_lowercase() == n || s.0.to_string() == n),
    };
    hit.map(|s| Some(s.0)).ok_or_else(|| {
        let open: Vec<String> = sprints.iter().filter(|s| s.2 != "closed").map(|s| format!("{} ({})", s.1, s.2)).collect();
        ApiError::BadRequest(format!("no sprint {name:?}; open sprints: {}", if open.is_empty() { "none".into() } else { open.join(", ") }))
    })
}

fn date_value(v: &str) -> Value {
    if clears(v) { Value::Null } else { json!(v.trim()) }
}

fn minutes(v: &str) -> ApiResult<Value> {
    if clears(v) {
        return Ok(Value::Null);
    }
    routes::comments::parse_duration(v).map(|m| json!(m)).ok_or_else(|| ApiError::bad("estimates look like 2h 30m, 45m or 2d"))
}

async fn allowed_from(state: &AppState, status: Uuid) -> ApiResult<Vec<String>> {
    Ok(sqlx::query_scalar("SELECT s.name FROM transitions t JOIN statuses s ON s.id = t.to_status_id WHERE t.from_status_id = $1 ORDER BY s.position")
        .bind(status)
        .fetch_all(&state.db)
        .await?)
}

impl TaskMcp {
    /// The caller and what their token may do. MCP is one POST endpoint for reading and writing
    /// tools alike, so the token scope is checked per tool ([`ScopeCheckedByCaller`]).
    async fn authenticate(&self, parts: &Parts) -> ApiResult<(User, TokenScope)> {
        let mut parts = parts.clone();
        parts.extensions.clear();
        parts.extensions.insert(ScopeCheckedByCaller);
        let user = MaybeUser::from_request_parts(&mut parts, &self.state).await?.0.ok_or(ApiError::Unauthorized)?;
        let scope = parts.extensions.get::<TokenScope>().copied().unwrap_or(TokenScope::Write);
        Ok((user, scope))
    }

    async fn reader(&self, parts: &Parts) -> ApiResult<User> {
        Ok(self.authenticate(parts).await?.0)
    }

    async fn writer(&self, parts: &Parts) -> ApiResult<User> {
        match self.authenticate(parts).await? {
            (_, TokenScope::Read) => Err(ApiError::Denied("this API token is read-only".into())),
            (u, _) => Ok(u),
        }
    }

    // ---- the tools' work ----

    async fn search(&self, user: User, a: SearchArgs) -> ApiResult<String> {
        let limit = a.limit.unwrap_or(25).clamp(1, 100);
        let offset = a.offset.unwrap_or(0).max(0);
        let r = routes::issues::run_search(&self.state, &user, &a.query, a.project_key.as_deref(), limit, offset, "i.updated_at DESC, i.id").await?;
        let issues: Vec<Value> = r.issues.iter().map(brief).collect();
        let mut v = json!({"total": r.total, "offset": offset, "issues": issues});
        if r.total > offset + r.issues.len() as i64 {
            v["more"] = json!(format!("{} more: call again with offset {}", r.total - offset - r.issues.len() as i64, offset + r.issues.len() as i64));
        }
        Ok(compact(&v))
    }

    async fn issue_markdown(&self, user: User, a: IssueArgs) -> ApiResult<String> {
        let st = &self.state;
        let key = clean_key(&a.key);
        let view = serde_json::from_value(json!({})).map_err(ApiError::from)?;
        let AxJson(v) = routes::issues::get(State(st.clone()), CurrentUser(user.clone()), Path(key.clone()), Query(view)).await?;
        let AxJson(project) =
            routes::projects::get(State(st.clone()), CurrentUser(user.clone()), Path(v["projectKey"].as_str().unwrap_or("").to_string())).await?;
        let txt = |x: &Value| x.as_str().unwrap_or("").to_string();
        let name = |x: &Value| x["name"].as_str().map(str::to_string);
        let mut md = format!("# {}: {}\n\n", txt(&v["key"]), txt(&v["title"]));
        let mut line = |label: &str, value: String| md.push_str(&format!("- {label}: {value}\n"));
        line("Project", format!("{} ({})", txt(&v["projectName"]), txt(&v["projectKey"])));
        line("Type", txt(&v["type"]["name"]));
        line("Status", format!("{} ({})", txt(&v["status"]["name"]), txt(&v["status"]["category"])));
        line("Priority", txt(&v["priority"]));
        line("Assignee", name(&v["assignee"]).unwrap_or_else(|| "unassigned".into()));
        if let Some(r) = name(&v["reporter"]) {
            line("Reporter", r);
        }
        if v["parent"].is_object() {
            line("Parent", format!("{} {}", txt(&v["parent"]["key"]), txt(&v["parent"]["title"])));
        }
        if v["sprint"].is_object() {
            line("Sprint", format!("{} ({})", txt(&v["sprint"]["name"]), txt(&v["sprint"]["state"])));
        }
        if let Some(n) = name(&v["version"]) {
            line("Release", n);
        }
        if let Some(n) = name(&v["component"]) {
            line("Component", n);
        }
        let labels: Vec<&str> = v["labels"].as_array().into_iter().flatten().filter_map(|l| l.as_str()).collect();
        if !labels.is_empty() {
            line("Labels", labels.join(", "));
        }
        if let Some(p) = v["storyPoints"].as_f64() {
            line("Story points", p.to_string());
        }
        let spent = v["spentMinutes"].as_i64().unwrap_or(0);
        match v["estimateMinutes"].as_i64() {
            Some(e) => line("Time", format!("{} logged, {} remaining estimate", duration(spent), duration(e))),
            None if spent > 0 => line("Time", format!("{} logged", duration(spent))),
            None => {}
        }
        for (label, field) in [("Start", "startDate"), ("Due", "dueDate")] {
            if let Some(d) = v[field].as_str() {
                line(label, d.to_string());
            }
        }
        // custom fields by name, select options by label
        if let Some(values) = v["custom"].as_object() {
            for f in project["fields"].as_array().into_iter().flatten() {
                let Some(val) = values.get(f["id"].as_str().unwrap_or("")) else { continue };
                let label_of = |id: &Value| {
                    f["options"].as_array().into_iter().flatten().find(|o| o["id"] == *id).and_then(|o| o["label"].as_str()).unwrap_or("?").to_string()
                };
                let shown = match f["kind"].as_str().unwrap_or("") {
                    "select" => label_of(val),
                    "multiselect" => val.as_array().into_iter().flatten().map(label_of).collect::<Vec<_>>().join(", "),
                    "user" => {
                        let id = val.as_str().and_then(|s| s.parse::<Uuid>().ok());
                        let n: Option<String> = match id {
                            Some(id) => sqlx::query_scalar("SELECT name FROM users WHERE id = $1").bind(id).fetch_optional(&st.db).await?,
                            None => None,
                        };
                        n.unwrap_or_else(|| "?".into())
                    }
                    _ => val.as_str().map(str::to_string).unwrap_or_else(|| val.to_string()),
                };
                line(f["name"].as_str().unwrap_or("?"), shown);
            }
        }
        if v["blocked"] == true {
            line("Blocked", "yes (an open issue blocks it)".into());
        }
        let watchers: Vec<&str> = v["watchers"].as_array().into_iter().flatten().filter_map(|w| w["name"].as_str()).collect();
        if !watchers.is_empty() {
            line("Watchers", watchers.join(", "));
        }
        let mut when = format!("created {}, updated {}", txt(&v["createdAt"]).get(..16).unwrap_or(""), txt(&v["updatedAt"]).get(..16).unwrap_or(""));
        if let Some(r) = v["resolvedAt"].as_str() {
            when.push_str(&format!(", resolved {}", r.get(..16).unwrap_or(r)));
        }
        line("Dates", when.replace('T', " "));
        line("Branch name", crate::git::branch_name(&txt(&v["key"]), &txt(&v["title"])));
        line("You can edit", yes(v["permissions"]["edit"] == true).into());
        line("URL", txt(&v["url"]));

        md.push_str("\n## Description\n\n");
        let description = if v["description"].is_object() { crate::doc::to_markdown(&v["description"]) } else { String::new() };
        md.push_str(if description.trim().is_empty() { "_No description._" } else { description.trim() });
        md.push('\n');

        let row = |i: &Value| {
            let mut s = format!("{} [{}] {}", txt(&i["key"]), txt(&i["status"]["name"]), txt(&i["title"]));
            if let Some(a) = i["assignee"]["name"].as_str() {
                s.push_str(&format!(" ({a})"));
            }
            s
        };
        let children = v["children"].as_array().cloned().unwrap_or_default();
        if !children.is_empty() {
            let done = children.iter().filter(|c| c["status"]["category"] == "done").count();
            let heading = if v["type"]["level"].as_i64().unwrap_or(0) > 0 { "Issues in this epic" } else { "Sub-tasks" };
            md.push_str(&format!("\n## {heading} ({done} of {} done)\n\n", children.len()));
            for c in &children {
                md.push_str(&format!("- {}\n", row(c)));
            }
        }
        let links = v["links"].as_array().cloned().unwrap_or_default();
        if !links.is_empty() {
            md.push_str("\n## Linked issues\n\n");
            for l in &links {
                md.push_str(&format!("- {} {}\n", txt(&l["label"]), row(&l["issue"])));
            }
        }
        let issue_id: Uuid = serde_json::from_value(v["id"].clone()).map_err(ApiError::from)?;
        let dev = crate::git::links(st, issue_id).await?;
        if !dev.is_empty() {
            md.push_str("\n## Development\n\n");
            for d in dev.iter().take(30) {
                let what = match d.kind.as_str() {
                    "pull_request" => {
                        format!("Pull request{} ({})", d.number.map(|n| format!(" #{n}")).unwrap_or_default(), d.state.as_deref().unwrap_or("open"))
                    }
                    "branch" => "Branch".into(),
                    _ => format!("Commit {}", d.sha.as_deref().unwrap_or("").get(..7).unwrap_or("")),
                };
                let by = d.author.as_deref().map(|a| format!(", {a}")).unwrap_or_default();
                md.push_str(&format!("- {what}: {} ({}{by}) {}\n", d.title, d.at.format("%Y-%m-%d"), d.url));
            }
        }
        let web = v["webLinks"].as_array().cloned().unwrap_or_default();
        if !web.is_empty() {
            md.push_str("\n## Web links and wiki pages\n\n");
            for w in &web {
                md.push_str(&format!("- [{}]({})\n", txt(&w["title"]), txt(&w["url"])));
            }
        }
        let files = v["attachments"].as_array().cloned().unwrap_or_default();
        if !files.is_empty() {
            md.push_str(&format!(
                "\n## Attachments\n\n{}\n",
                files.iter().map(|f| format!("- {} ({} KB)", txt(&f["name"]), f["size"].as_i64().unwrap_or(0) / 1024)).collect::<Vec<_>>().join("\n")
            ));
        }
        let AxJson(comments) = routes::comments::list(State(st.clone()), CurrentUser(user), Path(key)).await?;
        let comments = comments.as_array().cloned().unwrap_or_default();
        if !comments.is_empty() {
            let max = a.max_comments.unwrap_or(20);
            md.push_str(&format!("\n## Comments ({})\n", comments.len()));
            if comments.len() > max {
                md.push_str(&format!("\n_{} older comments not shown._\n", comments.len() - max));
            }
            for c in comments.iter().skip(comments.len().saturating_sub(max)) {
                let who = c["author"]["name"].as_str().unwrap_or("Automation");
                let at = txt(&c["createdAt"]).get(..16).unwrap_or("").replace('T', " ");
                md.push_str(&format!("\n**{who}** · {at}\n\n{}\n", crate::doc::to_markdown(&c["body"]).trim()));
            }
        }
        Ok(md)
    }

    async fn create(&self, user: User, a: CreateIssueArgs) -> ApiResult<String> {
        let st = &self.state;
        let (p, _) = routes::projects::by_key(st, &user, &a.project_key, Role::Member).await?;
        let mut f = Map::new();
        f.insert("projectKey".into(), json!(p.key));
        f.insert("title".into(), json!(a.title));
        if let Some(d) = a.description.filter(|d| !d.trim().is_empty()) {
            f.insert("descriptionMarkdown".into(), json!(d));
        }
        if let Some(t) = &a.kind {
            f.insert("typeId".into(), json!(find_type(st, p.id, t).await?));
        }
        if let Some(pr) = a.priority {
            f.insert("priority".into(), json!(pr));
        }
        if let Some(who) = &a.assignee {
            f.insert("assigneeId".into(), json!(find_person(st, &user, p.id, who).await?));
        }
        if let Some(l) = a.labels {
            f.insert("labels".into(), json!(l));
        }
        if let Some(parent) = a.parent.filter(|x| !clears(x)) {
            f.insert("parentKey".into(), json!(clean_key(&parent)));
        }
        if let Some(s) = &a.sprint {
            f.insert("sprintId".into(), json!(find_sprint(st, p.id, s).await?));
        }
        if let Some(pts) = a.story_points {
            f.insert("storyPoints".into(), json!(pts));
        }
        if let Some(e) = &a.estimate {
            f.insert("estimateMinutes".into(), minutes(e)?);
        }
        if let Some(d) = &a.due_date {
            f.insert("dueDate".into(), date_value(d));
        }
        if let Some(d) = &a.start_date {
            f.insert("startDate".into(), date_value(d));
        }
        let (_, AxJson(s)) = routes::issues::create(State(st.clone()), CurrentUser(user), AxJson(f)).await?;
        Ok(compact(&json!({"created": s.key, "issue": brief(&s)})))
    }

    async fn update(&self, user: User, a: UpdateIssueArgs) -> ApiResult<String> {
        let st = &self.state;
        let (id, project, _) = routes::issues::issue_for(st, &user, &clean_key(&a.key), Role::Member).await?;
        let cur = issues::summary(st, issues::load(st, id).await?);
        let mut patch = Map::new();
        if let Some(t) = a.title {
            patch.insert("title".into(), json!(t));
        }
        if let Some(d) = a.description {
            patch.insert("descriptionMarkdown".into(), if d.trim().is_empty() { Value::Null } else { json!(d) });
        }
        let mut target_status = None;
        if let Some(s) = &a.status {
            let sid = find_status(st, project, s).await?;
            target_status = Some(sid);
            patch.insert("statusId".into(), json!(sid));
        }
        if let Some(t) = &a.kind {
            patch.insert("typeId".into(), json!(find_type(st, project, t).await?));
        }
        if let Some(p) = a.priority {
            patch.insert("priority".into(), json!(p));
        }
        if let Some(who) = &a.assignee {
            patch.insert("assigneeId".into(), json!(find_person(st, &user, project, who).await?));
        }
        let mut labels: Option<Vec<String>> = a.labels;
        if a.add_labels.is_some() || a.remove_labels.is_some() {
            let mut l = labels.take().unwrap_or_else(|| cur.labels.clone());
            l.extend(a.add_labels.unwrap_or_default());
            let remove: Vec<String> = a.remove_labels.unwrap_or_default().iter().map(|x| crate::query::normalize_label(x)).collect();
            l.retain(|x| !remove.contains(&crate::query::normalize_label(x)));
            labels = Some(l);
        }
        if let Some(l) = labels {
            patch.insert("labels".into(), json!(l));
        }
        if let Some(parent) = &a.parent {
            patch.insert("parentKey".into(), json!(if clears(parent) { String::new() } else { clean_key(parent) }));
        }
        if let Some(s) = &a.sprint {
            patch.insert("sprintId".into(), json!(find_sprint(st, project, s).await?));
        }
        if let Some(p) = a.story_points {
            patch.insert("storyPoints".into(), json!(p));
        }
        if let Some(e) = &a.estimate {
            patch.insert("estimateMinutes".into(), minutes(e)?);
        }
        if let Some(d) = &a.due_date {
            patch.insert("dueDate".into(), date_value(d));
        }
        if let Some(d) = &a.start_date {
            patch.insert("startDate".into(), date_value(d));
        }
        if patch.is_empty() {
            return bad("nothing to change: give at least one field");
        }
        let (s, changes) = match issues::update(st, &Actor::of(&user), id, &patch).await {
            Ok(r) => r,
            // a restricted workflow refused the move: say where it can go
            Err(ApiError::BadRequest(msg)) if target_status.is_some() && msg.contains("workflow") => {
                let allowed = allowed_from(st, cur.status.id).await?;
                return bad(format!(
                    "{msg}. From {} it can move to: {}",
                    cur.status.name,
                    if allowed.is_empty() { "nowhere".into() } else { allowed.join(", ") }
                ));
            }
            Err(e) => return Err(e),
        };
        let changed: Vec<&str> = changes.iter().map(|c| c.field).collect();
        Ok(compact(&json!({"updated": s.key, "changed": changed, "issue": brief(&s)})))
    }

    async fn comment(&self, user: User, a: CommentArgs) -> ApiResult<String> {
        let key = clean_key(&a.key);
        let req = serde_json::from_value(json!({"markdown": a.text})).map_err(ApiError::from)?;
        let (_, AxJson(v)) = routes::comments::create(State(self.state.clone()), CurrentUser(user), Path(key.clone()), AxJson(req)).await?;
        Ok(compact(&json!({"commented": key, "commentId": v["id"], "url": self.state.config.issue_url(&key)})))
    }

    async fn work(&self, user: User, a: WorkArgs) -> ApiResult<String> {
        let key = clean_key(&a.key);
        let body = json!({"time": a.time, "date": a.date, "note": a.note, "reduceEstimate": a.reduce_estimate.unwrap_or(false)});
        let (_, AxJson(v)) = routes::comments::log_work(State(self.state.clone()), CurrentUser(user.clone()), Path(key.clone()), AxJson(body)).await?;
        let (id, _, _) = routes::issues::issue_for(&self.state, &user, &key, Role::Viewer).await?;
        let s = issues::summary(&self.state, issues::load(&self.state, id).await?);
        let logged = v["minutes"].as_i64().unwrap_or(0);
        Ok(compact(&json!({
            "logged": duration(logged), "issue": s.key, "totalSpent": duration(s.spent_minutes as i64),
            "remainingEstimate": s.estimate_minutes.map(|m| duration(m as i64)),
        })))
    }

    async fn projects(&self, user: User, a: ProjectsArgs) -> ApiResult<String> {
        let st = &self.state;
        if let Some(key) = &a.project_key {
            return Ok(compact(&self.project_detail(&user, key, true).await?));
        }
        let AxJson(list) = routes::projects::list(State(st.clone()), CurrentUser(user.clone())).await?;
        let mut out = Vec::new();
        for p in list.as_array().into_iter().flatten() {
            if !p["archivedAt"].is_null() && !a.include_archived.unwrap_or(false) {
                continue;
            }
            let key = p["key"].as_str().unwrap_or("").to_string();
            let mut v = if out.len() < 40 { self.project_detail(&user, &key, false).await? } else { json!({"key": key, "name": p["name"]}) };
            v["openIssues"] = p["openCount"].clone();
            v["doneIssues"] = p["doneCount"].clone();
            if let Some(l) = p["leadName"].as_str() {
                v["lead"] = json!(l);
            }
            out.push(v);
        }
        Ok(compact(&json!(out)))
    }

    /// A project's setup in the words TQL and the tools use.
    async fn project_detail(&self, user: &User, key: &str, full: bool) -> ApiResult<Value> {
        let st = &self.state;
        let AxJson(p) = routes::projects::get(State(st.clone()), CurrentUser(user.clone()), Path(key.to_string())).await?;
        let arr = |k: &str| p[k].as_array().cloned().unwrap_or_default();
        let statuses = arr("statuses");
        let status_name = |id: &Value| statuses.iter().find(|s| s["id"] == *id).and_then(|s| s["name"].as_str()).unwrap_or("?").to_string();
        let level = |l: i64| match l {
            1.. => "epic",
            0 => "standard",
            _ => "sub-task",
        };
        let sprints = arr("sprints");
        let active = sprints
            .iter()
            .find(|s| s["state"] == "active")
            .map(|s| json!({"id": s["id"], "name": s["name"], "goal": s["goal"], "startAt": s["startAt"], "endAt": s["endAt"]}));
        let mut v = json!({
            "key": p["key"], "name": p["name"], "kind": p["kind"], "yourRole": p["role"],
            "workflow": p["settings"]["workflow"].as_str().unwrap_or("free"),
            "estimation": p["settings"]["estimation"].as_str().unwrap_or("points"),
            "statuses": statuses.iter().map(|s| json!({"name": s["name"], "category": s["category"]})).collect::<Vec<_>>(),
            "types": arr("types").iter().map(|t| json!({"name": t["name"], "level": level(t["level"].as_i64().unwrap_or(0))})).collect::<Vec<_>>(),
            "activeSprint": active,
        });
        if let Some(d) = p["description"].as_str() {
            v["description"] = json!(d);
        }
        if !p["archivedAt"].is_null() {
            v["archived"] = json!(true);
        }
        let future: Vec<&Value> = sprints.iter().filter(|s| s["state"] == "future").map(|s| &s["name"]).collect();
        if !future.is_empty() {
            v["futureSprints"] = json!(future);
        }
        if !full {
            return Ok(v);
        }
        if v["workflow"] == "restricted" {
            let mut moves: BTreeMap<String, Vec<String>> = BTreeMap::new();
            for t in arr("transitions") {
                moves.entry(status_name(&t["from"])).or_default().push(status_name(&t["to"]));
            }
            v["transitions"] = json!(moves);
        }
        v["versions"] =
            json!(arr("versions").iter().map(|x| json!({"name": x["name"], "status": x["status"], "releaseDate": x["releaseDate"]})).collect::<Vec<_>>());
        v["components"] = json!(arr("components").iter().map(|x| x["name"].clone()).collect::<Vec<_>>());
        v["customFields"] = json!(
            arr("fields")
                .iter()
                .map(|f| {
                    let options: Vec<Value> = f["options"].as_array().into_iter().flatten().map(|o| o["label"].clone()).collect();
                    if options.is_empty() {
                        json!({"name": f["name"], "kind": f["kind"]})
                    } else {
                        json!({"name": f["name"], "kind": f["kind"], "options": options})
                    }
                })
                .collect::<Vec<_>>()
        );
        v["boards"] = json!(arr("boards").iter().map(|b| json!({"name": b["name"], "kind": b["kind"]})).collect::<Vec<_>>());
        let AxJson(m) = routes::projects::members(State(st.clone()), CurrentUser(user.clone()), Path(key.to_string())).await?;
        v["people"] = json!(m["people"].as_array().into_iter().flatten().map(|x| x["name"].clone()).collect::<Vec<_>>());
        Ok(v)
    }

    /// A sprint: by id, by name in a project, or the project's running one.
    async fn sprint_row(&self, user: &User, project_key: Option<&str>, sprint: Option<&str>) -> ApiResult<(Uuid, Uuid)> {
        let st = &self.state;
        if let Some(id) = sprint.and_then(|s| s.trim().parse::<Uuid>().ok()) {
            let project: Option<Uuid> =
                sqlx::query_scalar("SELECT s.project_id FROM sprints s JOIN projects p ON p.id = s.project_id WHERE s.id = $1 AND p.workspace_id = $2")
                    .bind(id)
                    .bind(user.workspace_id)
                    .fetch_optional(&st.db)
                    .await?;
            let project = project.ok_or(ApiError::NotFound("sprint"))?;
            access::require(st, user, project, Role::Viewer).await?;
            return Ok((id, project));
        }
        let key = project_key.ok_or_else(|| ApiError::bad("give project_key (and optionally the sprint's name)"))?;
        let (p, _) = routes::projects::by_key(st, user, key, Role::Viewer).await?;
        match find_sprint(st, p.id, sprint.unwrap_or("active")).await {
            Ok(Some(id)) => Ok((id, p.id)),
            Ok(None) => bad("the backlog isn't a sprint"),
            Err(e) if sprint.is_none() => bad(format!("{} has no running sprint ({e})", p.key)),
            Err(e) => Err(e),
        }
    }

    async fn sprint(&self, user: User, a: SprintArgs) -> ApiResult<String> {
        let st = &self.state;
        let (sid, _) = self.sprint_row(&user, a.project_key.as_deref(), a.sprint.as_deref()).await?;
        #[derive(sqlx::FromRow)]
        struct S {
            name: String,
            goal: Option<String>,
            state: String,
            start_at: Option<chrono::DateTime<chrono::Utc>>,
            end_at: Option<chrono::DateTime<chrono::Utc>>,
            completed_at: Option<chrono::DateTime<chrono::Utc>>,
            stats: Value,
        }
        let s: S =
            sqlx::query_as("SELECT name, goal, state, start_at, end_at, completed_at, stats FROM sprints WHERE id = $1").bind(sid).fetch_one(&st.db).await?;
        let ids: Vec<Uuid> =
            sqlx::query_scalar("SELECT id FROM issues WHERE sprint_id = $1 AND deleted_at IS NULL ORDER BY rank").bind(sid).fetch_all(&st.db).await?;
        let list = issues::load_many(st, &ids).await?;
        let mut by_status: BTreeMap<String, usize> = BTreeMap::new();
        let mut by_person: BTreeMap<String, (usize, f64)> = BTreeMap::new();
        let (mut points, mut points_done, mut done) = (0.0, 0.0, 0);
        for i in &list {
            *by_status.entry(i.status.name.clone()).or_default() += 1;
            let e = by_person.entry(i.assignee.as_ref().map(|a| a.name.clone()).unwrap_or_else(|| "unassigned".into())).or_default();
            e.0 += 1;
            e.1 += i.story_points.unwrap_or(0.0);
            points += i.story_points.unwrap_or(0.0);
            if i.status.category == "done" {
                done += 1;
                points_done += i.story_points.unwrap_or(0.0);
            }
        }
        let days_left = s.end_at.filter(|_| s.state == "active").map(|e| (e - chrono::Utc::now()).num_days());
        let mut v = json!({
            "sprint": {"id": sid, "name": s.name, "goal": s.goal, "state": s.state, "startAt": s.start_at, "endAt": s.end_at, "completedAt": s.completed_at, "daysLeft": days_left},
            "figures": {
                "issues": list.len(), "done": done, "points": points, "pointsDone": points_done,
                "byStatus": by_status,
                "byAssignee": by_person.iter().map(|(n, (c, p))| (n.clone(), json!({"issues": c, "points": p}))).collect::<Map<String, Value>>(),
            },
            "issues": list.iter().map(brief).collect::<Vec<_>>(),
        });
        if s.state == "closed" && s.stats.as_object().is_some_and(|o| !o.is_empty()) {
            v["closingFigures"] = s.stats;
        }
        Ok(compact(&v))
    }

    async fn board(&self, user: User, a: BoardArgs) -> ApiResult<String> {
        let st = &self.state;
        let board_id = match a.board.as_deref().and_then(|b| b.trim().parse::<Uuid>().ok()) {
            Some(id) => id,
            None => {
                let key = a.project_key.as_deref().ok_or_else(|| ApiError::bad("give project_key (and optionally the board's name)"))?;
                let (p, _) = routes::projects::by_key(st, &user, key, Role::Viewer).await?;
                let boards: Vec<(Uuid, String)> =
                    sqlx::query_as("SELECT id, name FROM boards WHERE project_id = $1 ORDER BY position, created_at").bind(p.id).fetch_all(&st.db).await?;
                let hit = match a.board.as_deref() {
                    Some(name) => boards.iter().find(|b| b.1.eq_ignore_ascii_case(name.trim())),
                    None => boards.first(),
                };
                hit.map(|b| b.0).ok_or_else(|| {
                    ApiError::BadRequest(format!("no such board in {}; boards: {}", p.key, boards.iter().map(|b| b.1.as_str()).collect::<Vec<_>>().join(", ")))
                })?
            }
        };
        let q = serde_json::from_value(json!({})).map_err(ApiError::from)?;
        let AxJson(b) = routes::boards::get(State(st.clone()), CurrentUser(user), Path(board_id), Query(q)).await?;
        let all = b["issues"].as_array().cloned().unwrap_or_default();
        let columns: Vec<Value> = b["board"]["config"]["columns"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|c| {
                let ids: Vec<&Value> = c["statusIds"].as_array().into_iter().flatten().collect();
                let cards: Vec<Value> = all
                    .iter()
                    .filter(|i| ids.contains(&&i["status"]["id"]))
                    .map(|i| {
                        let mut m = Map::new();
                        m.insert("key".into(), i["key"].clone());
                        m.insert("title".into(), i["title"].clone());
                        m.insert("status".into(), i["status"]["name"].clone());
                        m.insert("priority".into(), i["priority"].clone());
                        if let Some(a) = i["assignee"]["name"].as_str() {
                            m.insert("assignee".into(), json!(a));
                        }
                        if !i["storyPoints"].is_null() {
                            m.insert("points".into(), i["storyPoints"].clone());
                        }
                        if i["blocked"] == true {
                            m.insert("blocked".into(), json!(true));
                        }
                        Value::Object(m)
                    })
                    .collect();
                let limit = c["wipLimit"].as_u64();
                let mut col = json!({"name": c["name"], "count": cards.len(), "issues": cards});
                if let Some(l) = limit {
                    col["wipLimit"] = json!(l);
                    if cards_len(&col) > l as usize {
                        col["overLimit"] = json!(true);
                    }
                }
                col
            })
            .collect();
        Ok(compact(&json!({
            "board": {"id": board_id, "name": b["board"]["name"], "kind": b["board"]["kind"]},
            "sprint": b["sprint"].as_object().map(|s| json!({"name": s.get("name"), "goal": s.get("goal"), "endAt": s.get("endAt")})),
            "columns": columns,
        })))
    }

    async fn report(&self, user: User, a: ReportArgs) -> ApiResult<String> {
        let st = &self.state;
        let (p, _) = routes::projects::by_key(st, &user, &a.project_key, Role::Viewer).await?;
        let days = a.days.map(|d| d.clamp(1, 730));
        let mut v = match a.report.trim().to_lowercase().replace(['_', ' '], "-").as_str() {
            "overview" => crate::reports::overview(st, p.id).await?,
            "velocity" => crate::reports::velocity(st, p.id, a.count.unwrap_or(8)).await?,
            "cumulative-flow" | "cfd" | "flow" => crate::reports::cumulative_flow(st, p.id, days.unwrap_or(30)).await?,
            "cycle-time" | "lead-time" => crate::reports::cycle_time(st, p.id, days.unwrap_or(90)).await?,
            "created-resolved" | "created-vs-resolved" => crate::reports::created_resolved(st, p.id, days.unwrap_or(30)).await?,
            "workload" => crate::reports::workload(st, p.id).await?,
            "burndown" | "sprint" => {
                let (sid, project) = self.sprint_row(&user, Some(&p.key), a.sprint.as_deref()).await?;
                if project != p.id {
                    return bad("that sprint belongs to another project");
                }
                crate::reports::burndown(st, sid).await?
            }
            other => return bad(format!("unknown report {other:?}: overview, burndown, velocity, cumulative-flow, cycle-time, created-resolved or workload")),
        };
        strip(&mut v);
        Ok(compact(&json!({"project": p.key, "report": a.report, "data": v})))
    }
}

fn cards_len(col: &Value) -> usize {
    col["issues"].as_array().map(Vec::len).unwrap_or(0)
}

#[tool_router]
impl TaskMcp {
    pub fn new(state: AppState) -> Self {
        Self { state }
    }

    #[tool(description = TQL_HELP)]
    async fn search_issues(&self, Extension(parts): Extension<Parts>, Parameters(a): Parameters<SearchArgs>) -> ToolResult {
        match self.reader(&parts).await {
            Ok(u) => done(self.search(u, a).await),
            Err(e) => done(Err(e)),
        }
    }

    #[tool(
        description = "Read one issue in full, as Markdown: all fields (type, status, priority, assignee, sprint, parent, labels, estimates, custom fields), the description, sub-tasks or the epic's issues, linked issues, development links (commits, branches, pull requests), web and wiki links, attachments and the latest comments."
    )]
    async fn get_issue(&self, Extension(parts): Extension<Parts>, Parameters(a): Parameters<IssueArgs>) -> ToolResult {
        match self.reader(&parts).await {
            Ok(u) => done(self.issue_markdown(u, a).await),
            Err(e) => done(Err(e)),
        }
    }

    #[tool(
        description = "Create an issue. Types, sprints and people are given by name (see list_projects); the description is Markdown. Returns the new key and the issue."
    )]
    async fn create_issue(&self, Extension(parts): Extension<Parts>, Parameters(a): Parameters<CreateIssueArgs>) -> ToolResult {
        match self.writer(&parts).await {
            Ok(u) => done(self.create(u, a).await),
            Err(e) => done(Err(e)),
        }
    }

    #[tool(
        description = "Change an issue: only the fields given change. Move it with `status` (a status name of its project; restricted workflows only allow their transitions), assign with a name, e-mail, \"me\" or \"none\", replace or add / remove labels, set the sprint (\"active\", a name or \"backlog\"), parent, type, priority, points, estimate, dates, title or description (Markdown). Returns what changed."
    )]
    async fn update_issue(&self, Extension(parts): Extension<Parts>, Parameters(a): Parameters<UpdateIssueArgs>) -> ToolResult {
        match self.writer(&parts).await {
            Ok(u) => done(self.update(u, a).await),
            Err(e) => done(Err(e)),
        }
    }

    #[tool(description = "Comment on an issue (Markdown). Watchers are notified as usual.")]
    async fn add_comment(&self, Extension(parts): Extension<Parts>, Parameters(a): Parameters<CommentArgs>) -> ToolResult {
        match self.writer(&parts).await {
            Ok(u) => done(self.comment(u, a).await),
            Err(e) => done(Err(e)),
        }
    }

    #[tool(description = "Log time spent on an issue (\"1h 30m\", \"45m\", \"2d\"), optionally for another day and lowering the remaining estimate.")]
    async fn log_work(&self, Extension(parts): Extension<Parts>, Parameters(a): Parameters<WorkArgs>) -> ToolResult {
        match self.writer(&parts).await {
            Ok(u) => done(self.work(u, a).await),
            Err(e) => done(Err(e)),
        }
    }

    #[tool(
        description = "The projects you can see: key, name, kind, your role, open / done issue counts, workflow, statuses (with their category), issue types and the running sprint. With project_key: that project's full setup, including allowed transitions, versions, components, custom fields and people."
    )]
    async fn list_projects(&self, Extension(parts): Extension<Parts>, Parameters(a): Parameters<ProjectsArgs>) -> ToolResult {
        match self.reader(&parts).await {
            Ok(u) => done(self.projects(u, a).await),
            Err(e) => done(Err(e)),
        }
    }

    #[tool(
        description = "A sprint at a glance: name, goal, dates and days left, issues and points done, counts by status and by assignee, and its issues. The project's running sprint unless a sprint name or id is given."
    )]
    async fn get_sprint(&self, Extension(parts): Extension<Parts>, Parameters(a): Parameters<SprintArgs>) -> ToolResult {
        match self.reader(&parts).await {
            Ok(u) => done(self.sprint(u, a).await),
            Err(e) => done(Err(e)),
        }
    }

    #[tool(
        description = "A board as its columns show it (scrum boards: the running sprint): each column with its WIP limit and its issues (key, title, status, assignee, points, priority, blocked)."
    )]
    async fn get_board(&self, Extension(parts): Extension<Parts>, Parameters(a): Parameters<BoardArgs>) -> ToolResult {
        match self.reader(&parts).await {
            Ok(u) => done(self.board(u, a).await),
            Err(e) => done(Err(e)),
        }
    }

    #[tool(
        description = "A project report's figures as JSON: overview (issues by status, type and priority; overdue, unassigned, stale), burndown (a sprint's committed, completed, remaining, scope changes and steps), velocity (committed vs completed per closed sprint and the average), cumulative-flow, cycle-time (median and 85th percentile), created-resolved, workload (open work per person). Reports replay the issues' history."
    )]
    async fn run_report(&self, Extension(parts): Extension<Parts>, Parameters(a): Parameters<ReportArgs>) -> ToolResult {
        match self.reader(&parts).await {
            Ok(u) => done(self.report(u, a).await),
            Err(e) => done(Err(e)),
        }
    }
}

#[tool_handler]
impl ServerHandler for TaskMcp {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("taskcraft", env!("CARGO_PKG_VERSION")))
            .with_instructions(
                "TaskCraft issue and project management. Issue keys look like WEB-12. Start with `list_projects` to learn project keys and the exact names of statuses, types and sprints; find issues with `search_issues` (TQL queries), read one with `get_issue`, and change things with `create_issue`, `update_issue`, `add_comment` and `log_work`. `get_sprint`, `get_board` and `run_report` summarize the work. You act with the permissions of the person whose token you use; with a read-only token the changing tools refuse.",
            )
    }
}

/// The tower service mounted at `/api/mcp` and `/api/v1/mcp`: stateless (any replica answers any
/// request), JSON responses, and only this server's host names (DNS rebinding protection).
pub fn service(state: AppState) -> StreamableHttpService<TaskMcp, NeverSessionManager> {
    let mut hosts = vec!["localhost".to_string(), "127.0.0.1".to_string(), "::1".to_string()];
    if let Some(authority) = state.config.base_url.split("://").nth(1).map(|r| r.split('/').next().unwrap_or("").to_string()) {
        let host =
            authority.rsplit_once(':').filter(|(_, port)| port.chars().all(|c| c.is_ascii_digit())).map(|(h, _)| h.to_string()).unwrap_or(authority.clone());
        hosts.push(authority);
        hosts.push(host);
    }
    let config = StreamableHttpServerConfig::default().with_allowed_hosts(hosts).with_legacy_session_mode(false).with_json_response(true);
    StreamableHttpService::new(move || Ok(TaskMcp::new(state.clone())), NeverSessionManager::default().into(), config)
}
