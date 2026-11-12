//! Issue endpoints: create, read, change, move, delete and restore; search with TQL; links,
//! watchers, history, children, "my work" and CSV export.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use sqlx::QueryBuilder;
use uuid::Uuid;

use crate::access::Role;
use crate::app::{AppState, new_id};
use crate::auth::{CurrentUser, User};
use crate::error::{ApiError, ApiResult};
use crate::events::{self, Event};
use crate::issues::{self, Actor, NewIssue, Summary};

/// Resolves an issue and requires a role in its project.
pub async fn issue_for(state: &AppState, user: &User, key: &str, min: Role) -> ApiResult<(Uuid, Uuid, Role)> {
    let (id, project, role) = issues::resolve(state, user, key, false).await?;
    if role < min {
        return Err(ApiError::Denied("you can only view this project".into()));
    }
    Ok((id, project, role))
}

pub async fn create(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Json(req): Json<Map<String, Value>>,
) -> ApiResult<(StatusCode, Json<Summary>)> {
    let mut fields = req;
    let project_ref = fields.remove("projectKey").or_else(|| fields.remove("projectId")).and_then(|v| v.as_str().map(str::to_string));
    let project_ref = project_ref.ok_or_else(|| ApiError::bad("projectKey is required"))?;
    let (p, _) = super::projects::by_key(&state, &user, &project_ref, Role::Member).await?;
    let title = fields.remove("title").and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default();
    let description = match (fields.remove("description"), fields.remove("descriptionMarkdown")) {
        (_, Some(Value::String(md))) => Some(crate::doc::from_markdown(&md)),
        (Some(Value::String(s)), _) => Some(crate::doc::from_plain(&s)),
        (Some(d), _) if !d.is_null() => Some(d),
        _ => None,
    };
    // placement in a backlog or board column
    let after = fields.remove("after").and_then(|v| v.as_str().and_then(|s| s.parse::<Uuid>().ok()));
    let template = fields.remove("templateId").and_then(|v| v.as_str().and_then(|s| s.parse::<Uuid>().ok()));
    let s = match template {
        Some(tid) => {
            let ok: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM issue_templates WHERE id = $1 AND project_id = $2)")
                .bind(tid)
                .bind(p.id)
                .fetch_one(&state.db)
                .await?;
            if !ok {
                return Err(ApiError::bad("that template belongs to another project"));
            }
            let mut overrides = fields;
            if !title.trim().is_empty() {
                overrides.insert("title".into(), json!(title));
            }
            if let Some(d) = description {
                overrides.insert("description".into(), d);
            }
            crate::schedule::instantiate(&state, &Actor::of(&user), tid, overrides).await?
        }
        None => issues::create(&state, &Actor::of(&user), NewIssue { project: p.id, title, description, fields }).await?,
    };
    if after.is_some() {
        let mut tx = state.db.begin().await?;
        let r = issues::rank_between(&mut tx, p.id, after, None, s.id).await?;
        sqlx::query("UPDATE issues SET rank = $2 WHERE id = $1").bind(s.id).bind(r).execute(&mut *tx).await?;
        tx.commit().await?;
    }
    Ok((StatusCode::CREATED, Json(s)))
}

#[derive(Deserialize)]
pub struct ViewQuery {
    /// `1` / `true`: count this as the caller viewing the issue (recently viewed)
    view: Option<String>,
}

pub async fn get(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(key): Path<String>, Query(q): Query<ViewQuery>) -> ApiResult<Json<Value>> {
    let (id, _, role) = issue_for(&state, &user, &key, Role::Viewer).await?;
    let s = issues::summary(&state, issues::load(&state, id).await?);
    let description: Option<Value> = sqlx::query_scalar("SELECT description FROM issues WHERE id = $1").bind(id).fetch_one(&state.db).await?;
    let watchers: Vec<(Uuid, String)> =
        sqlx::query_as("SELECT u.id, u.name FROM issue_watchers w JOIN users u ON u.id = w.user_id WHERE w.issue_id = $1 ORDER BY lower(u.name)")
            .bind(id)
            .fetch_all(&state.db)
            .await?;
    let links: Vec<(Uuid, String, Uuid, Uuid)> =
        sqlx::query_as("SELECT id, kind, source_id, target_id FROM issue_links WHERE source_id = $1 OR target_id = $1 ORDER BY created_at")
            .bind(id)
            .fetch_all(&state.db)
            .await?;
    let other_ids: Vec<Uuid> = links.iter().map(|(_, _, s, t)| if *s == id { *t } else { *s }).collect();
    let visible = crate::access::visible_projects(&state, &user).await?;
    let others: std::collections::HashMap<Uuid, Summary> =
        issues::load_many(&state, &other_ids).await?.into_iter().filter(|o| visible.contains(&o.project_id)).map(|o| (o.id, o)).collect();
    let links: Vec<Value> = links
        .iter()
        .filter_map(|(lid, kind, src, tgt)| {
            let outward = *src == id;
            let other = others.get(if outward { tgt } else { src })?;
            Some(json!({"id": lid, "kind": kind, "label": link_label(kind, outward), "outward": outward, "issue": other}))
        })
        .collect();
    let web: Vec<(Uuid, String, String, String, chrono::DateTime<chrono::Utc>)> =
        sqlx::query_as("SELECT id, url, title, kind, created_at FROM web_links WHERE issue_id = $1 ORDER BY created_at").bind(id).fetch_all(&state.db).await?;
    let attachments: Vec<(Uuid, String, String, i64, Option<String>, chrono::DateTime<chrono::Utc>)> = sqlx::query_as(
        "SELECT a.id, a.name, a.mime, a.size, u.name, a.created_at FROM attachments a LEFT JOIN users u ON u.id = a.uploaded_by WHERE a.issue_id = $1 ORDER BY a.created_at",
    )
    .bind(id)
    .fetch_all(&state.db)
    .await?;
    let child_ids: Vec<Uuid> =
        sqlx::query_scalar("SELECT id FROM issues WHERE parent_id = $1 AND deleted_at IS NULL ORDER BY rank").bind(id).fetch_all(&state.db).await?;
    let children = issues::load_many(&state, &child_ids).await?;
    if q.view.as_deref().is_some_and(|v| v == "1" || v == "true") {
        let _ = sqlx::query("INSERT INTO recent_issues (user_id, issue_id) VALUES ($1, $2) ON CONFLICT (user_id, issue_id) DO UPDATE SET viewed_at = now()")
            .bind(user.id)
            .bind(id)
            .execute(&state.db)
            .await;
    }
    let mut v = serde_json::to_value(&s)?;
    v["description"] = description.unwrap_or(Value::Null);
    v["watchers"] = json!(watchers.iter().map(|(id, n)| json!({"id": id, "name": n})).collect::<Vec<_>>());
    v["watching"] = json!(watchers.iter().any(|(w, _)| *w == user.id));
    v["links"] = json!(links);
    v["webLinks"] = json!(web.iter().map(|(id, u, t, k, at)| json!({"id": id, "url": u, "title": t, "kind": k, "createdAt": at})).collect::<Vec<_>>());
    v["attachments"] = json!(
        attachments
            .iter()
            .map(|(id, n, m, s, by, at)| json!({"id": id, "name": n, "mime": m, "size": s, "uploadedBy": by, "createdAt": at, "url": format!("/api/attachments/{id}")}))
            .collect::<Vec<_>>()
    );
    v["children"] = json!(children);
    v["sla"] = json!(crate::sla::for_issues(&state, &[id]).await?.remove(&id).unwrap_or_default());
    // who it was opened for (no account): by e-mail, on the request form or in the portal;
    // `mail`: comments for them are e-mailed and they can sign in to the portal
    v["requester"] = crate::mail_in::requester(&state, id)
        .await?
        .map(|r| json!({"email": r.email, "name": r.name, "mail": state.mailer.enabled(), "source": r.source}))
        .unwrap_or(Value::Null);
    v["permissions"] = json!({"edit": role >= Role::Member, "comment": role >= Role::Viewer, "admin": role >= Role::Admin});
    Ok(Json(v))
}

pub fn link_label(kind: &str, outward: bool) -> &'static str {
    match (kind, outward) {
        ("blocks", true) => "blocks",
        ("blocks", false) => "is blocked by",
        ("duplicates", true) => "duplicates",
        ("duplicates", false) => "is duplicated by",
        ("clones", true) => "clones",
        ("clones", false) => "is cloned by",
        _ => "relates to",
    }
}

pub async fn update(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Path(key): Path<String>,
    Json(req): Json<Map<String, Value>>,
) -> ApiResult<Json<Summary>> {
    let (id, _, _) = issue_for(&state, &user, &key, Role::Member).await?;
    let (s, _) = issues::update(&state, &Actor::of(&user), id, &req).await?;
    Ok(Json(s))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MoveReq {
    status_id: Option<Uuid>,
    /// present: move to this sprint (null = the backlog)
    #[serde(default, deserialize_with = "double_option")]
    sprint_id: Option<Option<Uuid>>,
    after: Option<Uuid>,
    before: Option<Uuid>,
    /// fields to set along with the move (e.g. what the target status requires) and a comment
    #[serde(default)]
    fields: Map<String, Value>,
    comment: Option<String>,
}

fn double_option<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Option<Uuid>>, D::Error> {
    Ok(Some(Option::<Uuid>::deserialize(d)?))
}

pub async fn move_issue(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Path(key): Path<String>,
    Json(req): Json<MoveReq>,
) -> ApiResult<Json<Summary>> {
    let (id, _, _) = issue_for(&state, &user, &key, Role::Member).await?;
    let mut extra = req.fields;
    if let Some(c) = req.comment {
        extra.insert("comment".into(), json!(c));
    }
    Ok(Json(issues::move_issue(&state, &Actor::of(&user), id, req.status_id, req.sprint_id, req.after, req.before, &extra).await?))
}

pub async fn delete(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(key): Path<String>) -> ApiResult<StatusCode> {
    let (id, project, _) = issue_for(&state, &user, &key, Role::Member).await?;
    let s = issues::summary(&state, issues::load(&state, id).await?);
    let mut tx = state.db.begin().await?;
    sqlx::query("UPDATE issues SET deleted_at = now(), deleted_by = $2 WHERE id = $1").bind(id).bind(user.id).execute(&mut *tx).await?;
    issues::record(&mut tx, id, project, Some(user.id), "deleted", None).await?;
    tx.commit().await?;
    events::publish(&state, Event::new(user.workspace_id, "issue").project(project).issue(id, &s.key).actor(user.id)).await;
    crate::webhooks::enqueue(&state, user.workspace_id, Some(project), "issue.deleted", json!({"issue": s})).await;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn restore(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(key): Path<String>) -> ApiResult<Json<Summary>> {
    let (id, project, role) = issues::resolve(&state, &user, &key, true).await?;
    if role < Role::Member {
        return Err(ApiError::Forbidden);
    }
    let mut tx = state.db.begin().await?;
    sqlx::query("UPDATE issues SET deleted_at = NULL, deleted_by = NULL, updated_at = now() WHERE id = $1").bind(id).execute(&mut *tx).await?;
    issues::record(&mut tx, id, project, Some(user.id), "restored", None).await?;
    tx.commit().await?;
    let s = issues::summary(&state, issues::load(&state, id).await?);
    events::publish(&state, Event::new(user.workspace_id, "issue").project(project).issue(id, &s.key).actor(user.id)).await;
    Ok(Json(s))
}

/// Deleted issues of a project (the trash).
pub async fn trash(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(key): Path<String>) -> ApiResult<Json<Value>> {
    let (p, _) = super::projects::by_key(&state, &user, &key, Role::Member).await?;
    let rows: Vec<(Uuid, i32, String, Option<chrono::DateTime<chrono::Utc>>, Option<String>)> = sqlx::query_as(
        "SELECT i.id, i.number, i.title, i.deleted_at, u.name FROM issues i LEFT JOIN users u ON u.id = i.deleted_by
          WHERE i.project_id = $1 AND i.deleted_at IS NOT NULL ORDER BY i.deleted_at DESC LIMIT 200",
    )
    .bind(p.id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(json!(
        rows.iter()
            .map(|(id, n, t, at, by)| json!({"id": id, "key": format!("{}-{n}", p.key), "title": t, "deletedAt": at, "deletedBy": by}))
            .collect::<Vec<_>>()
    )))
}

#[derive(Deserialize)]
pub struct BulkReq {
    ids: Vec<Uuid>,
    patch: Option<Map<String, Value>>,
    /// delete
    action: Option<String>,
}

/// Changes or deletes many issues; each one is checked and recorded like a single change.
pub async fn bulk(State(state): State<AppState>, CurrentUser(user): CurrentUser, Json(req): Json<BulkReq>) -> ApiResult<Json<Value>> {
    if req.ids.len() > 500 {
        return Err(ApiError::bad("at most 500 issues at once"));
    }
    let actor = Actor::of(&user);
    let mut done = 0;
    let mut errors = Vec::new();
    for id in &req.ids {
        let r: ApiResult<()> = async {
            let (id, project, role) = issues::resolve(&state, &user, &id.to_string(), false).await?;
            if role < Role::Member {
                return Err(ApiError::Forbidden);
            }
            if req.action.as_deref() == Some("delete") {
                let mut tx = state.db.begin().await?;
                sqlx::query("UPDATE issues SET deleted_at = now(), deleted_by = $2 WHERE id = $1").bind(id).bind(user.id).execute(&mut *tx).await?;
                issues::record(&mut tx, id, project, Some(user.id), "deleted", None).await?;
                tx.commit().await?;
                events::publish(&state, Event::new(user.workspace_id, "issue").project(project).actor(user.id)).await;
            } else if let Some(p) = &req.patch {
                issues::update(&state, &actor, id, p).await?;
            }
            Ok(())
        }
        .await;
        match r {
            Ok(()) => done += 1,
            Err(e) => errors.push(json!({"id": id, "error": e.to_string()})),
        }
    }
    Ok(Json(json!({"updated": done, "errors": errors})))
}

#[derive(Deserialize)]
pub struct SearchQuery {
    #[serde(default)]
    q: String,
    limit: Option<i64>,
    offset: Option<i64>,
    /// a project key: the query is limited to it
    project: Option<String>,
    /// default order when the query has none
    order: Option<String>,
}

pub struct SearchResult {
    pub total: i64,
    pub issues: Vec<Summary>,
}

/// Runs a TQL query for `user` (optionally within one project).
pub async fn run_search(
    state: &AppState,
    user: &User,
    q: &str,
    project: Option<&str>,
    limit: i64,
    offset: i64,
    default_order: &str,
) -> ApiResult<SearchResult> {
    let parsed = crate::query::parse(q)?;
    let mut ctx = crate::query::context(state, user, &parsed).await?;
    if let Some(key) = project.filter(|k| !k.is_empty()) {
        let (p, _) = super::projects::by_key(state, user, key, Role::Viewer).await?;
        ctx.visible = vec![p.id];
    }
    let from = " FROM issues i JOIN projects p ON p.id = i.project_id JOIN statuses s ON s.id = i.status_id JOIN issue_types it ON it.id = i.type_id WHERE";
    let mut count: QueryBuilder<sqlx::Postgres> = QueryBuilder::new(format!("SELECT count(*){from}"));
    crate::query::push_where(&mut count, &ctx, &parsed)?;
    let total: i64 = count.build_query_scalar().fetch_one(&state.db).await?;
    let mut ids: QueryBuilder<sqlx::Postgres> = QueryBuilder::new(format!("SELECT i.id{from}"));
    crate::query::push_where(&mut ids, &ctx, &parsed)?;
    crate::query::push_order(&mut ids, &ctx, &parsed.order, default_order)?;
    ids.push(" LIMIT ");
    ids.push_bind(limit.clamp(1, 1000));
    ids.push(" OFFSET ");
    ids.push_bind(offset.max(0));
    let ids: Vec<Uuid> = ids.build_query_scalar().fetch_all(&state.db).await?;
    let mut found = issues::load_many(state, &ids).await?;
    // keep the query's order
    let pos: std::collections::HashMap<Uuid, usize> = ids.iter().enumerate().map(|(i, id)| (*id, i)).collect();
    found.sort_by_key(|s| pos.get(&s.id).copied().unwrap_or(usize::MAX));
    Ok(SearchResult { total, issues: found })
}

fn default_order(o: Option<&str>) -> &'static str {
    match o {
        Some("rank") => "i.rank, i.id",
        Some("created") => "i.created_at DESC, i.id",
        Some("priority") => "i.priority DESC, i.rank, i.id",
        _ => "i.updated_at DESC, i.id",
    }
}

pub async fn search(State(state): State<AppState>, CurrentUser(user): CurrentUser, Query(q): Query<SearchQuery>) -> ApiResult<Json<Value>> {
    let r = run_search(&state, &user, &q.q, q.project.as_deref(), q.limit.unwrap_or(100), q.offset.unwrap_or(0), default_order(q.order.as_deref())).await?;
    Ok(Json(json!({"total": r.total, "issues": r.issues})))
}

/// The same search as CSV (spreadsheet export).
pub async fn search_csv(State(state): State<AppState>, CurrentUser(user): CurrentUser, Query(q): Query<SearchQuery>) -> ApiResult<Response> {
    let r = run_search(&state, &user, &q.q, q.project.as_deref(), 1000, 0, default_order(q.order.as_deref())).await?;
    let mut w = csv::Writer::from_writer(Vec::new());
    let _ = w.write_record([
        "Key",
        "Title",
        "Type",
        "Status",
        "Priority",
        "Assignee",
        "Reporter",
        "Sprint",
        "Version",
        "Parent",
        "Points",
        "Estimate (h)",
        "Spent (h)",
        "Labels",
        "Start",
        "Due",
        "Created",
        "Updated",
        "Resolved",
        "Resolution",
    ]);
    let h = |m: Option<i32>| m.map(|m| format!("{:.1}", m as f64 / 60.0)).unwrap_or_default();
    for s in r.issues {
        // a leading = + - @ would make spreadsheets evaluate the cell
        let safe = |t: &str| if t.starts_with(['=', '+', '-', '@']) { format!("'{t}") } else { t.to_string() };
        let _ = w.write_record([
            s.key.clone(),
            safe(&s.title),
            s.kind.name.clone(),
            s.status.name.clone(),
            s.priority.to_string(),
            s.assignee.as_ref().map(|a| a.name.clone()).unwrap_or_default(),
            s.reporter.as_ref().map(|a| a.name.clone()).unwrap_or_default(),
            s.sprint.as_ref().map(|a| a.name.clone()).unwrap_or_default(),
            s.version.as_ref().map(|a| a.name.clone()).unwrap_or_default(),
            s.parent.as_ref().map(|a| a.key.clone()).unwrap_or_default(),
            s.story_points.map(|p| p.to_string()).unwrap_or_default(),
            h(s.estimate_minutes),
            h(Some(s.spent_minutes)),
            s.labels.join(" "),
            s.start_date.map(|d| d.to_string()).unwrap_or_default(),
            s.due_date.map(|d| d.to_string()).unwrap_or_default(),
            s.created_at.format("%Y-%m-%d %H:%M").to_string(),
            s.updated_at.format("%Y-%m-%d %H:%M").to_string(),
            s.resolved_at.map(|d| d.format("%Y-%m-%d %H:%M").to_string()).unwrap_or_default(),
            s.resolution.clone().unwrap_or_default(),
        ]);
    }
    let body = w.into_inner().map_err(|e| ApiError::Internal(anyhow::anyhow!("csv: {e}")))?;
    Ok(([(header::CONTENT_TYPE, "text/csv; charset=utf-8"), (header::CONTENT_DISPOSITION, "attachment; filename=\"issues.csv\"")], body).into_response())
}

/// Checks a query: `{"ok": true}` or the error message.
pub async fn validate(State(state): State<AppState>, CurrentUser(user): CurrentUser, Json(req): Json<Value>) -> ApiResult<Json<Value>> {
    let q = req["q"].as_str().unwrap_or("");
    let r: ApiResult<()> = async {
        let parsed = crate::query::parse(q)?;
        let ctx = crate::query::context(&state, &user, &parsed).await?;
        let mut qb: QueryBuilder<sqlx::Postgres> = QueryBuilder::new("SELECT 1 FROM issues i WHERE");
        crate::query::push_where(&mut qb, &ctx, &parsed)?;
        crate::query::push_order(&mut qb, &ctx, &parsed.order, "i.id")?;
        Ok(())
    }
    .await;
    Ok(Json(match r {
        Ok(()) => json!({"ok": true}),
        Err(e) => json!({"ok": false, "message": e.to_string()}),
    }))
}

/// Field names and values for query autocompletion.
pub async fn suggest(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> ApiResult<Json<Value>> {
    let visible = crate::access::visible_projects(&state, &user).await?;
    let field = q.get("field").map(|f| f.to_lowercase()).unwrap_or_default();
    let values: Vec<String> = match field.as_str() {
        "" => {
            let custom: Vec<String> =
                sqlx::query_scalar("SELECT DISTINCT name FROM custom_fields WHERE project_id = ANY($1)").bind(&visible).fetch_all(&state.db).await?;
            let mut all: Vec<String> = [
                "project",
                "key",
                "title",
                "text",
                "type",
                "status",
                "category",
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
                "created",
                "updated",
                "resolved",
                "resolution",
                "due",
                "start",
                "watcher",
                "blocked",
                "comment",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect();
            all.extend(custom.into_iter().map(|c| taskcraft_tql::quote(&c)));
            all
        }
        "project" => sqlx::query_scalar("SELECT key FROM projects WHERE id = ANY($1) ORDER BY key").bind(&visible).fetch_all(&state.db).await?,
        "status" => {
            sqlx::query_scalar("SELECT DISTINCT name FROM statuses WHERE project_id = ANY($1) ORDER BY name").bind(&visible).fetch_all(&state.db).await?
        }
        "type" | "issuetype" => {
            sqlx::query_scalar("SELECT DISTINCT name FROM issue_types WHERE project_id = ANY($1) ORDER BY name").bind(&visible).fetch_all(&state.db).await?
        }
        "category" | "statuscategory" => vec!["todo".into(), "in_progress".into(), "done".into()],
        "resolution" => {
            // each project's list (or the defaults), then what issues actually carry
            let settings: Vec<Value> = sqlx::query_scalar("SELECT settings FROM projects WHERE id = ANY($1)").bind(&visible).fetch_all(&state.db).await?;
            let mut names: Vec<String> = Vec::new();
            for s in &settings {
                for r in crate::workflow::resolutions(s) {
                    if !names.contains(&r) {
                        names.push(r);
                    }
                }
            }
            names
        }
        "priority" => crate::query::PRIORITIES.iter().rev().map(|s| s.to_string()).collect(),
        "assignee" | "reporter" | "watcher" | "creator" => {
            let mut v = vec!["me()".to_string()];
            v.extend(
                sqlx::query_scalar::<_, String>("SELECT name FROM users WHERE workspace_id = $1 AND status = 'active' ORDER BY lower(name)")
                    .bind(user.workspace_id)
                    .fetch_all(&state.db)
                    .await?,
            );
            v
        }
        "labels" | "label" => {
            sqlx::query_scalar(
                "SELECT DISTINCT l.label FROM issue_labels l JOIN issues i ON i.id = l.issue_id WHERE i.project_id = ANY($1) ORDER BY l.label LIMIT 200",
            )
            .bind(&visible)
            .fetch_all(&state.db)
            .await?
        }
        "sprint" => {
            let mut v = vec!["openSprints()".to_string(), "futureSprints()".into(), "closedSprints()".into()];
            v.extend(
                sqlx::query_scalar::<_, String>("SELECT DISTINCT name FROM sprints WHERE project_id = ANY($1) AND state <> 'closed' ORDER BY name")
                    .bind(&visible)
                    .fetch_all(&state.db)
                    .await?,
            );
            v
        }
        "version" | "fixversion" | "release" => {
            let mut v = vec!["unreleasedVersions()".to_string(), "releasedVersions()".into()];
            v.extend(
                sqlx::query_scalar::<_, String>("SELECT DISTINCT name FROM versions WHERE project_id = ANY($1) ORDER BY name")
                    .bind(&visible)
                    .fetch_all(&state.db)
                    .await?,
            );
            v
        }
        "component" => {
            sqlx::query_scalar("SELECT DISTINCT name FROM components WHERE project_id = ANY($1) ORDER BY name").bind(&visible).fetch_all(&state.db).await?
        }
        "created" | "updated" | "resolved" | "due" | "start" => {
            ["-1d", "-7d", "-30d", "today()", "startOfWeek()", "endOfWeek()", "startOfMonth()", "endOfMonth()", "now()"].iter().map(|s| s.to_string()).collect()
        }
        "blocked" => vec!["true".into(), "false".into()],
        other => {
            let opts: Vec<Value> = sqlx::query_scalar("SELECT options FROM custom_fields WHERE project_id = ANY($1) AND lower(name) = $2")
                .bind(&visible)
                .bind(other)
                .fetch_all(&state.db)
                .await?;
            opts.iter().flat_map(|o| o.as_array().cloned().unwrap_or_default()).filter_map(|o| o["label"].as_str().map(str::to_string)).collect()
        }
    };
    Ok(Json(json!(values.iter().map(|v| if v.ends_with(')') { v.clone() } else { taskcraft_tql::quote(v) }).collect::<Vec<_>>())))
}

/// Everything that happened to an issue (newest last).
pub async fn history(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(key): Path<String>) -> ApiResult<Json<Value>> {
    let (id, _, _) = issue_for(&state, &user, &key, Role::Viewer).await?;
    let rows: Vec<(Uuid, Option<Uuid>, Option<String>, String, Option<String>, Option<Value>, Option<Value>, chrono::DateTime<chrono::Utc>)> = sqlx::query_as(
        "SELECT e.id, e.actor_id, u.name, e.kind, e.field, e.old_value, e.new_value, e.created_at FROM issue_events e LEFT JOIN users u ON u.id = e.actor_id
          WHERE e.issue_id = $1 ORDER BY e.created_at, e.id LIMIT 1000",
    )
    .bind(id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(json!(
        rows.into_iter()
            .map(|(id, aid, an, kind, field, old, new, at)| json!({
                "id": id, "actor": aid.map(|a| json!({"id": a, "name": an})), "kind": kind, "field": field, "old": old, "new": new, "at": at
            }))
            .collect::<Vec<_>>()
    )))
}

pub async fn watch(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(key): Path<String>, Json(req): Json<Value>) -> ApiResult<StatusCode> {
    let (id, _, _) = issue_for(&state, &user, &key, Role::Viewer).await?;
    let who = req["userId"].as_str().and_then(|s| s.parse::<Uuid>().ok()).unwrap_or(user.id);
    if who != user.id {
        // adding someone else needs edit rights
        issue_for(&state, &user, &key, Role::Member).await?;
    }
    if req["watching"].as_bool().unwrap_or(true) {
        sqlx::query("INSERT INTO issue_watchers (issue_id, user_id) SELECT $1, id FROM users WHERE id = $2 AND workspace_id = $3 ON CONFLICT DO NOTHING")
            .bind(id)
            .bind(who)
            .bind(user.workspace_id)
            .execute(&state.db)
            .await?;
    } else {
        sqlx::query("DELETE FROM issue_watchers WHERE issue_id = $1 AND user_id = $2").bind(id).bind(who).execute(&state.db).await?;
    }
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkReq {
    /// blocks | is blocked by | relates | duplicates | is duplicated by | clones
    kind: String,
    target: String,
}

pub async fn add_link(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Path(key): Path<String>,
    Json(req): Json<LinkReq>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let (id, project, _) = issue_for(&state, &user, &key, Role::Member).await?;
    let (target, tproject, _) = issues::resolve(&state, &user, &req.target, false).await?;
    if target == id {
        return Err(ApiError::bad("an issue can't link to itself"));
    }
    let (kind, src, tgt) = match req.kind.as_str() {
        "blocks" => ("blocks", id, target),
        "is blocked by" | "blockedBy" => ("blocks", target, id),
        "duplicates" => ("duplicates", id, target),
        "is duplicated by" | "duplicatedBy" => ("duplicates", target, id),
        "clones" => ("clones", id, target),
        "relates" | "relates to" => ("relates", id, target),
        _ => return Err(ApiError::bad("kind must be blocks, is blocked by, relates, duplicates, is duplicated by or clones")),
    };
    let lid = new_id();
    let mut tx = state.db.begin().await?;
    sqlx::query("INSERT INTO issue_links (id, source_id, target_id, kind, created_by) VALUES ($1, $2, $3, $4, $5) ON CONFLICT DO NOTHING")
        .bind(lid)
        .bind(src)
        .bind(tgt)
        .bind(kind)
        .bind(user.id)
        .execute(&mut *tx)
        .await?;
    let tkey: String = sqlx::query_scalar("SELECT p.key || '-' || i.number FROM issues i JOIN projects p ON p.id = i.project_id WHERE i.id = $1")
        .bind(target)
        .fetch_one(&mut *tx)
        .await?;
    let skey: String = sqlx::query_scalar("SELECT p.key || '-' || i.number FROM issues i JOIN projects p ON p.id = i.project_id WHERE i.id = $1")
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
    let c = issues::Change { field: "link", old: Value::Null, new: json!({"kind": req.kind, "key": tkey}) };
    issues::record(&mut tx, id, project, Some(user.id), "link", Some(&c)).await?;
    let back = issues::Change { field: "link", old: Value::Null, new: json!({"kind": link_label(kind, src != id), "key": skey}) };
    issues::record(&mut tx, target, tproject, Some(user.id), "link", Some(&back)).await?;
    sqlx::query("UPDATE issues SET updated_at = now() WHERE id = ANY($1)").bind(vec![id, target]).execute(&mut *tx).await?;
    tx.commit().await?;
    for (pid, iid, k) in [(project, id, &skey), (tproject, target, &tkey)] {
        events::publish(&state, Event::new(user.workspace_id, "issue").project(pid).issue(iid, k).actor(user.id)).await;
    }
    Ok((StatusCode::CREATED, Json(json!({"id": lid}))))
}

pub async fn delete_link(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    let row: Option<(Uuid, Uuid)> = sqlx::query_as("SELECT source_id, target_id FROM issue_links WHERE id = $1").bind(id).fetch_optional(&state.db).await?;
    let (src, tgt) = row.ok_or(ApiError::NotFound("link"))?;
    let (_, project, _) = issue_for(&state, &user, &src.to_string(), Role::Member).await?;
    sqlx::query("DELETE FROM issue_links WHERE id = $1").bind(id).execute(&state.db).await?;
    sqlx::query("UPDATE issues SET updated_at = now() WHERE id = ANY($1)").bind(vec![src, tgt]).execute(&state.db).await?;
    events::publish(&state, Event::new(user.workspace_id, "issue").project(project).actor(user.id)).await;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn add_web_link(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Path(key): Path<String>,
    Json(req): Json<Value>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let (id, project, _) = issue_for(&state, &user, &key, Role::Member).await?;
    let url = req["url"].as_str().map(str::trim).unwrap_or("");
    if !(url.starts_with("https://") || url.starts_with("http://")) || url.len() > 2000 {
        return Err(ApiError::bad("links need a web address (https://…)"));
    }
    let wiki_base = crate::wikicraft::base_url(&state);
    let is_wiki = wiki_base.as_deref().is_some_and(|b| crate::wikicraft::parse_page_url(b, url).is_some());
    let mut title = req["title"].as_str().map(str::trim).filter(|t| !t.is_empty()).map(str::to_string);
    if title.is_none()
        && is_wiki
        && let Some(c) = crate::wikicraft::conn(&state, &user).await
        && let Ok(Some(p)) = crate::wikicraft::lookup(&state, &c, url).await
    {
        title = Some(p.title);
    }
    let lid = new_id();
    let title = title.unwrap_or_else(|| url.to_string());
    let mut tx = state.db.begin().await?;
    sqlx::query("INSERT INTO web_links (id, issue_id, url, title, kind, created_by) VALUES ($1, $2, $3, $4, $5, $6)")
        .bind(lid)
        .bind(id)
        .bind(url)
        .bind(&title)
        .bind(if is_wiki { "wiki" } else { "web" })
        .bind(user.id)
        .execute(&mut *tx)
        .await?;
    let c = issues::Change { field: "webLink", old: Value::Null, new: json!({"url": url, "title": title}) };
    issues::record(&mut tx, id, project, Some(user.id), "link", Some(&c)).await?;
    tx.commit().await?;
    events::publish(&state, Event::new(user.workspace_id, "issue").project(project).actor(user.id)).await;
    Ok((StatusCode::CREATED, Json(json!({"id": lid, "title": title, "kind": if is_wiki { "wiki" } else { "web" }}))))
}

pub async fn delete_web_link(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    let issue: Uuid =
        sqlx::query_scalar("SELECT issue_id FROM web_links WHERE id = $1").bind(id).fetch_optional(&state.db).await?.ok_or(ApiError::NotFound("link"))?;
    let (_, project, _) = issue_for(&state, &user, &issue.to_string(), Role::Member).await?;
    sqlx::query("DELETE FROM web_links WHERE id = $1").bind(id).execute(&state.db).await?;
    events::publish(&state, Event::new(user.workspace_id, "issue").project(project).actor(user.id)).await;
    Ok(StatusCode::NO_CONTENT)
}

/// Copies an issue (title, description, fields, labels) and links the copy to the original.
pub async fn clone_issue(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(key): Path<String>) -> ApiResult<(StatusCode, Json<Summary>)> {
    let (id, project, _) = issue_for(&state, &user, &key, Role::Member).await?;
    let src = issues::summary(&state, issues::load(&state, id).await?);
    let description: Option<Value> = sqlx::query_scalar("SELECT description FROM issues WHERE id = $1").bind(id).fetch_one(&state.db).await?;
    let mut fields = Map::new();
    fields.insert("typeId".into(), json!(src.kind.id));
    fields.insert("priority".into(), json!(src.priority));
    fields.insert("labels".into(), json!(src.labels));
    fields.insert("storyPoints".into(), json!(src.story_points));
    fields.insert("estimateMinutes".into(), json!(src.estimate_minutes));
    if let Some(p) = &src.parent {
        fields.insert("parentId".into(), json!(p.id));
    }
    if let Some(v) = &src.version {
        fields.insert("versionId".into(), json!(v.id));
    }
    if src.custom.as_object().is_some_and(|o| !o.is_empty()) {
        fields.insert("custom".into(), src.custom.clone());
    }
    let copy = issues::create(&state, &Actor::of(&user), NewIssue { project, title: format!("Copy of {}", src.title), description, fields }).await?;
    sqlx::query("INSERT INTO issue_links (id, source_id, target_id, kind, created_by) VALUES ($1, $2, $3, 'clones', $4)")
        .bind(new_id())
        .bind(copy.id)
        .bind(id)
        .bind(user.id)
        .execute(&state.db)
        .await?;
    Ok((StatusCode::CREATED, Json(copy)))
}

/// The signed-in person's work: assigned, due soon, reported, recently viewed and watched.
pub async fn my_work(State(state): State<AppState>, CurrentUser(user): CurrentUser) -> ApiResult<Json<Value>> {
    let assigned =
        run_search(&state, &user, "assignee = me() and category != done", None, 200, 0, "i.due_date ASC NULLS LAST, i.priority DESC, i.rank").await?;
    let reported = run_search(&state, &user, "reporter = me() and category != done and assignee != me()", None, 50, 0, "i.updated_at DESC").await?;
    let done = run_search(&state, &user, "assignee = me() and resolved >= -7d", None, 50, 0, "i.resolved_at DESC").await?;
    let watching = run_search(&state, &user, "watcher = me() and updated >= -7d and assignee != me()", None, 30, 0, "i.updated_at DESC").await?;
    let recent_ids: Vec<Uuid> = sqlx::query_scalar(
        "SELECT r.issue_id FROM recent_issues r JOIN issues i ON i.id = r.issue_id WHERE r.user_id = $1 AND i.deleted_at IS NULL ORDER BY r.viewed_at DESC LIMIT 12",
    )
    .bind(user.id)
    .fetch_all(&state.db)
    .await?;
    let visible = crate::access::visible_projects(&state, &user).await?;
    let mut recent = issues::load_many(&state, &recent_ids).await?;
    recent.retain(|s| visible.contains(&s.project_id));
    let pos: std::collections::HashMap<Uuid, usize> = recent_ids.iter().enumerate().map(|(i, id)| (*id, i)).collect();
    recent.sort_by_key(|s| pos.get(&s.id).copied().unwrap_or(usize::MAX));
    let logged: Option<i64> =
        sqlx::query_scalar("SELECT sum(minutes) FROM worklogs WHERE user_id = $1 AND work_date >= date_trunc('week', current_date)::date")
            .bind(user.id)
            .fetch_one(&state.db)
            .await?;
    Ok(Json(json!({
        "assigned": assigned.issues, "reported": reported.issues, "doneThisWeek": done.issues, "watching": watching.issues, "recent": recent,
        "loggedThisWeek": logged.unwrap_or(0),
    })))
}

/// Quick search for pickers and the command palette: issues by key or words.
pub async fn quick(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> ApiResult<Json<Value>> {
    let text = q.get("q").map(|s| s.trim().to_string()).unwrap_or_default();
    if text.is_empty() {
        return my_recent(&state, &user).await;
    }
    let tql = format!("text ~ {}", taskcraft_tql::quote(&text));
    let r = run_search(&state, &user, &tql, q.get("project").map(String::as_str), 15, 0, "i.updated_at DESC").await?;
    Ok(Json(json!(r.issues)))
}

async fn my_recent(state: &AppState, user: &User) -> ApiResult<Json<Value>> {
    let ids: Vec<Uuid> =
        sqlx::query_scalar("SELECT issue_id FROM recent_issues WHERE user_id = $1 ORDER BY viewed_at DESC LIMIT 10").bind(user.id).fetch_all(&state.db).await?;
    let visible = crate::access::visible_projects(state, user).await?;
    let mut list = issues::load_many(state, &ids).await?;
    list.retain(|s| visible.contains(&s.project_id));
    let pos: std::collections::HashMap<Uuid, usize> = ids.iter().enumerate().map(|(i, id)| (*id, i)).collect();
    list.sort_by_key(|s| pos.get(&s.id).copied().unwrap_or(usize::MAX));
    Ok(Json(json!(list)))
}

/// Recent activity in the projects the caller can see (or one project).
pub async fn activity(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> ApiResult<Json<Value>> {
    let mut visible = crate::access::visible_projects(&state, &user).await?;
    if let Some(k) = q.get("project").filter(|k| !k.is_empty()) {
        let (p, _) = super::projects::by_key(&state, &user, k, Role::Viewer).await?;
        visible = vec![p.id];
    }
    let rows: Vec<(Uuid, Option<Uuid>, Option<String>, String, Option<String>, Option<Value>, Option<Value>, chrono::DateTime<chrono::Utc>, String, String)> =
        sqlx::query_as(
            "SELECT e.id, e.actor_id, u.name, e.kind, e.field, e.old_value, e.new_value, e.created_at, p.key || '-' || i.number, i.title
               FROM issue_events e JOIN issues i ON i.id = e.issue_id JOIN projects p ON p.id = e.project_id LEFT JOIN users u ON u.id = e.actor_id
              WHERE e.project_id = ANY($1) AND i.deleted_at IS NULL AND (e.field IS NULL OR e.field NOT IN ('description'))
              ORDER BY e.created_at DESC LIMIT 60",
        )
        .bind(&visible)
        .fetch_all(&state.db)
        .await?;
    Ok(Json(json!(
        rows.into_iter()
            .map(|(id, aid, an, kind, field, old, new, at, key, title)| json!({
                "id": id, "actor": aid.map(|a| json!({"id": a, "name": an})), "kind": kind, "field": field, "old": old, "new": new, "at": at, "key": key, "title": title
            }))
            .collect::<Vec<_>>()
    )))
}
