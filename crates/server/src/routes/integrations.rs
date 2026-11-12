//! WikiCraft from TaskCraft's side: page search and lookups, a project's wiki space, linked
//! pages with live titles, and pages written from issues, sprints and releases. Also the
//! lookup WikiCraft uses to show which issues link a page.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use super::issues::issue_for;
use super::projects::by_key;
use crate::access::{self, Role};
use crate::app::{AppState, new_id};
use crate::auth::{CurrentUser, User};
use crate::error::{ApiError, ApiResult};
use crate::events::{self, Event};
use crate::issues::{self, Change, Summary};
use crate::wikicraft;

pub async fn status(State(state): State<AppState>, CurrentUser(user): CurrentUser) -> ApiResult<Json<Value>> {
    let c = wikicraft::conn(&state, &user).await;
    Ok(Json(json!({"url": wikicraft::base_url(&state), "connected": c.is_some(), "personal": c.as_ref().is_some_and(|c| c.personal)})))
}

#[derive(Deserialize)]
pub struct WikiSearch {
    #[serde(default)]
    q: String,
    space: Option<String>,
    url: Option<String>,
}

pub async fn search(State(state): State<AppState>, CurrentUser(user): CurrentUser, Query(q): Query<WikiSearch>) -> ApiResult<Json<Value>> {
    let c = wikicraft::require(&state, &user).await?;
    // a pasted page address is looked up directly
    if let Some(url) = q.url.as_deref().or(Some(q.q.as_str()).filter(|s| s.starts_with("http"))) {
        return Ok(Json(json!(wikicraft::lookup(&state, &c, url).await?.into_iter().collect::<Vec<_>>())));
    }
    if q.q.trim().is_empty() {
        return match q.space.as_deref().filter(|s| !s.is_empty()) {
            Some(s) => Ok(Json(json!(wikicraft::recent(&state, &c, s).await?))),
            None => Ok(Json(json!([]))),
        };
    }
    Ok(Json(json!(wikicraft::search(&state, &c, &q.q, q.space.as_deref()).await?)))
}

pub async fn spaces(State(state): State<AppState>, CurrentUser(user): CurrentUser) -> ApiResult<Json<Value>> {
    let c = wikicraft::require(&state, &user).await?;
    Ok(Json(json!(wikicraft::spaces(&state, &c).await?)))
}

/// The project's wiki space and its recently updated pages.
pub async fn project_wiki(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(key): Path<String>) -> ApiResult<Json<Value>> {
    let (p, _) = by_key(&state, &user, &key, Role::Viewer).await?;
    let space = p.settings["wikiSpace"].as_str().filter(|s| !s.is_empty()).map(str::to_string);
    let Some(c) = wikicraft::conn(&state, &user).await else {
        return Ok(Json(json!({"connected": false, "space": space, "pages": []})));
    };
    let pages = match &space {
        Some(s) => wikicraft::recent(&state, &c, s).await.unwrap_or_default(),
        None => vec![],
    };
    Ok(Json(json!({"connected": true, "space": space, "spaceUrl": space.as_ref().map(|s| format!("{}/spaces/{s}", c.base)), "pages": pages})))
}

/// The issue's wiki pages with their current titles (pages the person can't see show as such).
pub async fn issue_pages(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(key): Path<String>) -> ApiResult<Json<Value>> {
    let (id, _, _) = issue_for(&state, &user, &key, Role::Viewer).await?;
    let links: Vec<(Uuid, String, String)> =
        sqlx::query_as("SELECT id, url, title FROM web_links WHERE issue_id = $1 AND kind = 'wiki' ORDER BY created_at").bind(id).fetch_all(&state.db).await?;
    let c = wikicraft::conn(&state, &user).await;
    let mut out = Vec::new();
    for (lid, url, title) in links {
        let live = match &c {
            Some(c) => wikicraft::lookup(&state, c, &url).await.ok().flatten(),
            None => None,
        };
        out.push(json!({"id": lid, "url": url, "title": live.as_ref().map(|p| p.title.clone()).unwrap_or(title), "page": live, "available": live.is_some()}));
    }
    Ok(Json(json!(out)))
}

async fn link_page(state: &AppState, user: &User, issue: Uuid, project: Uuid, url: &str, title: &str) -> ApiResult<()> {
    let mut tx = state.db.begin().await?;
    sqlx::query("INSERT INTO web_links (id, issue_id, url, title, kind, created_by) VALUES ($1, $2, $3, $4, 'wiki', $5)")
        .bind(new_id())
        .bind(issue)
        .bind(url)
        .bind(title)
        .bind(user.id)
        .execute(&mut *tx)
        .await?;
    issues::record(
        &mut tx,
        issue,
        project,
        Some(user.id),
        "link",
        Some(&Change { field: "webLink", old: Value::Null, new: json!({"url": url, "title": title}) }),
    )
    .await?;
    tx.commit().await?;
    events::publish(state, Event::new(user.workspace_id, "issue").project(project).issue(issue, "").actor(user.id)).await;
    Ok(())
}

fn md_issue_line(state: &AppState, s: &Summary) -> String {
    let who = s.assignee.as_ref().map(|a| format!(" · {}", a.name)).unwrap_or_default();
    let pts = s.story_points.map(|p| format!(" · {p} pts")).unwrap_or_default();
    format!("- [{}]({}) {} — *{}*{who}{pts}", s.key, state.config.issue_url(&s.key), md(&s.title), s.status.name)
}

fn md(t: &str) -> String {
    t.replace('\\', "\\\\").replace('[', "\\[").replace(']', "\\]").replace('*', "\\*").replace('_', "\\_").replace('<', "\\<")
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewPage {
    space_key: Option<String>,
    title: Option<String>,
    parent_id: Option<Uuid>,
}

fn space_of(p: &super::projects::ProjectRow, req: &NewPage) -> ApiResult<String> {
    req.space_key
        .clone()
        .filter(|s| !s.is_empty())
        .or_else(|| p.settings["wikiSpace"].as_str().map(str::to_string))
        .filter(|s| !s.is_empty())
        .ok_or_else(|| ApiError::bad("choose a wiki space (or set the project's wiki space in its settings)"))
}

fn parent_of(p: &super::projects::ProjectRow, req: &NewPage) -> Option<Uuid> {
    req.parent_id.or_else(|| p.settings["wikiParentPage"].as_str().and_then(|s| s.parse().ok()))
}

/// Writes a wiki page for an issue (a specification to fill in) and links it.
pub async fn issue_page(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Path(key): Path<String>,
    Json(req): Json<NewPage>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let (id, project, _) = issue_for(&state, &user, &key, Role::Member).await?;
    let (p, _) = by_key(&state, &user, &project.to_string(), Role::Viewer).await?;
    let c = wikicraft::require(&state, &user).await?;
    let s = issues::summary(&state, issues::load(&state, id).await?);
    let description: Option<Value> = sqlx::query_scalar("SELECT description FROM issues WHERE id = $1").bind(id).fetch_one(&state.db).await?;
    let title = req.title.clone().filter(|t| !t.trim().is_empty()).unwrap_or_else(|| format!("{}: {}", s.key, s.title));
    let mut body = format!(
        "> Work item [{}]({}) · {} · {}\n\n## Background\n\n{}\n\n## Goals\n\n- \n\n## Approach\n\n\n## Open questions\n\n- [ ] \n",
        s.key,
        s.url,
        s.kind.name,
        s.status.name,
        description.as_ref().map(crate::doc::to_markdown).filter(|d| !d.trim().is_empty()).unwrap_or_else(|| "*(from the issue)*".into()),
    );
    if !s.labels.is_empty() {
        body.push_str(&format!("\n---\nLabels: {}\n", s.labels.join(", ")));
    }
    let page = wikicraft::create_page(&state, &c, &space_of(&p, &req)?, &title, &body, parent_of(&p, &req)).await?;
    link_page(&state, &user, id, project, &page.url, &title).await?;
    Ok((StatusCode::CREATED, Json(json!(page))))
}

/// A sprint review page: goal, figures, what was finished and what wasn't.
pub async fn sprint_page(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<Uuid>,
    Json(req): Json<NewPage>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let project = crate::reports::sprint_project(&state, id).await?;
    access::require(&state, &user, project, Role::Member).await?;
    let (p, _) = by_key(&state, &user, &project.to_string(), Role::Viewer).await?;
    let c = wikicraft::require(&state, &user).await?;
    let b = crate::reports::burndown(&state, id, Some(user.id)).await?;
    let ids: Vec<Uuid> = sqlx::query_scalar(
        "SELECT DISTINCT e.issue_id FROM issue_events e WHERE e.project_id = $2 AND ((e.field = 'sprint' AND (e.new_value->>'id' = $1::text OR e.old_value->>'id' = $1::text))
            OR (e.kind = 'created' AND e.new_value->'sprint'->>'id' = $1::text))
         UNION SELECT id FROM issues WHERE sprint_id = $1",
    )
    .bind(id)
    .bind(project)
    .fetch_all(&state.db)
    .await?;
    // wiki pages are read by others: private issues stay out of them
    let mut all = issues::load_many(&state, &ids).await?;
    all.retain(|s| s.security.is_none());
    let in_sprint: Vec<&Summary> = all.iter().filter(|s| s.sprint.as_ref().is_some_and(|x| x.id == id)).collect();
    let done: Vec<&&Summary> = in_sprint.iter().filter(|s| s.status.category == "done").collect();
    let open: Vec<&&Summary> = in_sprint.iter().filter(|s| s.status.category != "done").collect();
    let moved: Vec<&Summary> = all.iter().filter(|s| s.sprint.as_ref().is_none_or(|x| x.id != id)).collect();
    let unit = b["unit"].as_str().unwrap_or("points");
    let name = b["sprint"]["name"].as_str().unwrap_or("Sprint").to_string();
    let title = req.title.clone().filter(|t| !t.trim().is_empty()).unwrap_or_else(|| format!("{name} review"));
    let mut body = String::new();
    if let Some(g) = b["sprint"]["goal"].as_str().filter(|g| !g.is_empty()) {
        body.push_str(&format!("> **Sprint goal:** {}\n\n", md(g)));
    }
    let date = |v: &Value| v.as_str().and_then(|s| s.get(..10)).unwrap_or("").to_string();
    body.push_str(&format!(
        "| | |\n|---|---|\n| Dates | {} – {} |\n| Committed | {} {unit} ({} issues) |\n| Completed | {} {unit} ({} issues) |\n| Added during the sprint | {} |\n| Removed | {} |\n\n",
        date(&b["start"]),
        date(&b["end"]),
        b["committed"],
        b["committedIssues"],
        b["completed"],
        b["completedIssues"],
        b["added"],
        b["removed"]
    ));
    body.push_str("## Completed\n\n");
    for s in &done {
        body.push_str(&md_issue_line(&state, s));
        body.push('\n');
    }
    if done.is_empty() {
        body.push_str("*(none)*\n");
    }
    body.push_str("\n## Not completed\n\n");
    for s in open.iter().map(|s| **s).chain(moved.iter().copied()) {
        body.push_str(&md_issue_line(&state, s));
        body.push('\n');
    }
    if open.is_empty() && moved.is_empty() {
        body.push_str("*(none)*\n");
    }
    body.push_str("\n## What went well\n\n- \n\n## What to improve\n\n- \n\n## Actions\n\n- [ ] \n");
    let page = wikicraft::create_page(&state, &c, &space_of(&p, &req)?, &title, &body, parent_of(&p, &req)).await?;
    Ok((StatusCode::CREATED, Json(json!(page))))
}

/// Release notes for a version, grouped by issue type.
pub async fn version_page(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<Uuid>,
    Json(req): Json<NewPage>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let row: Option<(Uuid, String, Option<String>, Option<chrono::NaiveDate>)> =
        sqlx::query_as("SELECT v.project_id, v.name, v.description, v.release_date FROM versions v JOIN projects p ON p.id = v.project_id WHERE v.id = $1 AND p.workspace_id = $2")
            .bind(id)
            .bind(user.workspace_id)
            .fetch_optional(&state.db)
            .await?;
    let (project, name, description, release_date) = row.ok_or(ApiError::NotFound("version"))?;
    access::require(&state, &user, project, Role::Member).await?;
    let (p, _) = by_key(&state, &user, &project.to_string(), Role::Viewer).await?;
    let c = wikicraft::require(&state, &user).await?;
    let ids: Vec<Uuid> = sqlx::query_scalar(
        "SELECT i.id FROM issue_versions iv JOIN issues i ON i.id = iv.issue_id WHERE iv.version_id = $1 AND iv.kind = 'fix' AND i.deleted_at IS NULL",
    )
    .bind(id)
    .fetch_all(&state.db)
    .await?;
    let mut list = issues::load_many(&state, &ids).await?;
    list.retain(|s| s.security.is_none());
    list.sort_by(|a, b| a.kind.name.cmp(&b.kind.name).then(a.number.cmp(&b.number)));
    let title = req.title.clone().filter(|t| !t.trim().is_empty()).unwrap_or_else(|| format!("{} {name} release notes", p.name));
    let mut body = String::new();
    if let Some(d) = release_date {
        body.push_str(&format!("Released {d}.\n\n"));
    }
    if let Some(d) = description.filter(|d| !d.trim().is_empty()) {
        body.push_str(&format!("{}\n\n", md(&d)));
    }
    let mut current = String::new();
    for s in list.iter().filter(|s| s.status.category == "done") {
        if s.kind.name != current {
            current = s.kind.name.clone();
            body.push_str(&format!("\n## {}\n\n", md(&current)));
        }
        body.push_str(&format!("- {} ([{}]({}))\n", md(&s.title), s.key, s.url));
    }
    let open: Vec<&Summary> = list.iter().filter(|s| s.status.category != "done").collect();
    if !open.is_empty() {
        body.push_str("\n## Known issues / not included\n\n");
        for s in open {
            body.push_str(&md_issue_line(&state, s));
            body.push('\n');
        }
    }
    let page = wikicraft::create_page(&state, &c, &space_of(&p, &req)?, &title, &body, parent_of(&p, &req)).await?;
    Ok((StatusCode::CREATED, Json(json!(page))))
}

#[derive(Deserialize)]
pub struct ByUrl {
    url: String,
}

/// Issues that link a page (WikiCraft asks this to show "linked issues" on a page). Matches
/// `/pages/<id>` addresses by the page id, whatever host they were linked with.
pub async fn linked_issues(State(state): State<AppState>, CurrentUser(user): CurrentUser, Query(q): Query<ByUrl>) -> ApiResult<Json<Value>> {
    let url = q.url.trim();
    let id_part = url.split("/pages/").nth(1).and_then(|r| r.split(['/', '?', '#']).next()).and_then(|s| s.parse::<Uuid>().ok());
    let visible = access::visible_projects(&state, &user).await?;
    let ids: Vec<Uuid> = match id_part {
        Some(pid) => sqlx::query_scalar(
            "SELECT DISTINCT w.issue_id FROM web_links w JOIN issues i ON i.id = w.issue_id WHERE i.project_id = ANY($1) AND i.deleted_at IS NULL AND w.url LIKE '%/pages/' || $2 || '%'",
        )
        .bind(&visible)
        .bind(pid.to_string())
        .fetch_all(&state.db)
        .await?,
        None => sqlx::query_scalar("SELECT DISTINCT w.issue_id FROM web_links w JOIN issues i ON i.id = w.issue_id WHERE i.project_id = ANY($1) AND i.deleted_at IS NULL AND w.url = $2")
            .bind(&visible)
            .bind(url)
            .fetch_all(&state.db)
            .await?,
    };
    Ok(Json(json!(issues::load_many_for(&state, user.id, &ids).await?)))
}

/// Links an existing page to an issue.
pub async fn add_issue_page(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Path(key): Path<String>,
    Json(req): Json<ByUrl>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let (id, project, _) = issue_for(&state, &user, &key, Role::Member).await?;
    let c = wikicraft::require(&state, &user).await?;
    let page = wikicraft::lookup(&state, &c, &req.url).await?.ok_or_else(|| ApiError::bad("that isn't a WikiCraft page you can open"))?;
    let exists: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM web_links WHERE issue_id = $1 AND url = $2)").bind(id).bind(&page.url).fetch_one(&state.db).await?;
    if !exists {
        link_page(&state, &user, id, project, &page.url, &page.title).await?;
    }
    Ok((StatusCode::CREATED, Json(json!(page))))
}
