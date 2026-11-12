//! Projects and their configuration: workflow (statuses and allowed moves), issue types,
//! versions, components, custom fields, members, stars and automation rules.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::access::{self, Role};
use crate::app::{AppState, audit, new_id};
use crate::auth::{CurrentUser, User};
use crate::error::{ApiError, ApiResult};
use crate::events::{self, Event};
use object_store::ObjectStoreExt;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ProjectRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub key: String,
    pub name: String,
    pub description: Option<String>,
    pub icon: Option<String>,
    pub color: Option<String>,
    pub kind: String,
    pub lead_id: Option<Uuid>,
    pub default_role: String,
    pub settings: Value,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub archived_at: Option<chrono::DateTime<chrono::Utc>>,
}

const PROJECT_COLS: &str = "id, workspace_id, key, name, description, icon, color, kind, lead_id, default_role, settings, created_at, archived_at";

/// A project by key (or former key) or id, with the caller's role (at least `min`).
pub async fn by_key(state: &AppState, user: &User, key: &str, min: Role) -> ApiResult<(ProjectRow, Role)> {
    let row: Option<ProjectRow> = if let Ok(id) = key.parse::<Uuid>() {
        sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT {PROJECT_COLS} FROM projects WHERE id = $1 AND workspace_id = $2")))
            .bind(id)
            .bind(user.workspace_id)
            .fetch_optional(&state.db)
            .await?
    } else {
        sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT {PROJECT_COLS} FROM projects WHERE workspace_id = $1 AND (lower(key) = lower($2)
                OR id IN (SELECT project_id FROM project_key_aliases WHERE workspace_id = $1 AND lower(key) = lower($2)))
              ORDER BY lower(key) = lower($2) DESC LIMIT 1"
        )))
        .bind(user.workspace_id)
        .bind(key)
        .fetch_optional(&state.db)
        .await?
    };
    let p = row.ok_or(ApiError::NotFound("project"))?;
    let role = access::require(state, user, p.id, min).await?;
    Ok((p, role))
}

fn project_json(p: &ProjectRow, role: Role, extra: Value) -> Value {
    let mut v = json!({
        "id": p.id, "key": p.key, "name": p.name, "description": p.description, "icon": p.icon, "color": p.color, "kind": p.kind,
        "leadId": p.lead_id, "defaultRole": p.default_role, "settings": p.settings, "createdAt": p.created_at, "archivedAt": p.archived_at,
        "role": role.as_str(),
    });
    if let (Some(o), Some(e)) = (v.as_object_mut(), extra.as_object()) {
        for (k, x) in e {
            o.insert(k.clone(), x.clone());
        }
    }
    v
}

pub async fn list(State(state): State<AppState>, CurrentUser(user): CurrentUser) -> ApiResult<Json<Value>> {
    #[derive(sqlx::FromRow)]
    struct ListRow {
        #[sqlx(flatten)]
        p: ProjectRow,
        rank: i32,
        starred: bool,
        open_count: i64,
        done_count: i64,
        lead_name: Option<String>,
        active_sprint: Option<String>,
    }
    let rows: Vec<ListRow> = sqlx::query_as(
        "SELECT p.id, p.workspace_id, p.key, p.name, p.description, p.icon, p.color, p.kind, p.lead_id, p.default_role, p.settings, p.created_at, p.archived_at,
                r.rank, EXISTS (SELECT 1 FROM project_stars s WHERE s.project_id = p.id AND s.user_id = $1) AS starred,
                (SELECT count(*) FROM issues i JOIN statuses s ON s.id = i.status_id WHERE i.project_id = p.id AND i.deleted_at IS NULL AND s.category <> 'done') AS open_count,
                (SELECT count(*) FROM issues i JOIN statuses s ON s.id = i.status_id WHERE i.project_id = p.id AND i.deleted_at IS NULL AND s.category = 'done') AS done_count,
                (SELECT name FROM users WHERE id = p.lead_id) AS lead_name,
                (SELECT name FROM sprints WHERE project_id = p.id AND state = 'active' ORDER BY start_at LIMIT 1) AS active_sprint
           FROM projects p JOIN tc_project_roles($1) r ON r.project_id = p.id
          WHERE r.rank >= 1 ORDER BY p.archived_at IS NOT NULL, lower(p.name)",
    )
    .bind(user.id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(json!(
        rows.into_iter()
            .map(|r| project_json(
                &r.p,
                Role::from_rank(r.rank),
                json!({"starred": r.starred, "openCount": r.open_count, "doneCount": r.done_count, "leadName": r.lead_name, "activeSprint": r.active_sprint})
            ))
            .collect::<Vec<_>>()
    )))
}

/// Everything the project's screens need: statuses, types, versions, components, fields,
/// open sprints, boards and the allowed moves.
pub async fn get(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(key): Path<String>) -> ApiResult<Json<Value>> {
    let (p, role) = by_key(&state, &user, &key, Role::Viewer).await?;
    let statuses: Vec<(Uuid, String, String, Option<String>, f64)> =
        sqlx::query_as("SELECT id, name, category, color, position FROM statuses WHERE project_id = $1 ORDER BY position").bind(p.id).fetch_all(&state.db).await?;
    let types: Vec<(Uuid, String, String, String, i16, Option<Value>)> =
        sqlx::query_as("SELECT id, name, icon, color, level, template FROM issue_types WHERE project_id = $1 ORDER BY position").bind(p.id).fetch_all(&state.db).await?;
    let versions: Vec<(Uuid, String, Option<String>, String, Option<chrono::NaiveDate>, Option<chrono::NaiveDate>, Option<chrono::DateTime<chrono::Utc>>)> =
        sqlx::query_as("SELECT id, name, description, status, start_date, release_date, released_at FROM versions WHERE project_id = $1 ORDER BY position, created_at")
            .bind(p.id)
            .fetch_all(&state.db)
            .await?;
    let components: Vec<(Uuid, String, Option<String>, Option<Uuid>)> =
        sqlx::query_as("SELECT id, name, description, lead_id FROM components WHERE project_id = $1 ORDER BY lower(name)").bind(p.id).fetch_all(&state.db).await?;
    let fields: Vec<(Uuid, String, String, Value, Option<String>)> =
        sqlx::query_as("SELECT id, name, kind, options, description FROM custom_fields WHERE project_id = $1 ORDER BY position").bind(p.id).fetch_all(&state.db).await?;
    let sprints: Vec<(Uuid, String, Option<String>, String, Option<chrono::DateTime<chrono::Utc>>, Option<chrono::DateTime<chrono::Utc>>)> = sqlx::query_as(
        "SELECT id, name, goal, state, start_at, end_at FROM sprints WHERE project_id = $1 AND state <> 'closed' ORDER BY state = 'active' DESC, position, created_at",
    )
    .bind(p.id)
    .fetch_all(&state.db)
    .await?;
    let boards: Vec<(Uuid, String, String)> = sqlx::query_as("SELECT id, name, kind FROM boards WHERE project_id = $1 ORDER BY position, created_at").bind(p.id).fetch_all(&state.db).await?;
    let transitions: Vec<(Uuid, Uuid)> =
        sqlx::query_as("SELECT from_status_id, to_status_id FROM transitions WHERE project_id = $1").bind(p.id).fetch_all(&state.db).await?;
    let starred: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM project_stars WHERE project_id = $1 AND user_id = $2)").bind(p.id).bind(user.id).fetch_one(&state.db).await?;
    let lead_name: Option<String> = match p.lead_id {
        Some(l) => sqlx::query_scalar("SELECT name FROM users WHERE id = $1").bind(l).fetch_optional(&state.db).await?,
        None => None,
    };
    Ok(Json(project_json(
        &p,
        role,
        json!({
            "starred": starred,
            "leadName": lead_name,
            "statuses": statuses.iter().map(|(id, n, c, col, pos)| json!({"id": id, "name": n, "category": c, "color": col, "position": pos})).collect::<Vec<_>>(),
            "types": types.iter().map(|(id, n, i, c, l, t)| json!({"id": id, "name": n, "icon": i, "color": c, "level": l, "template": t})).collect::<Vec<_>>(),
            "versions": versions.iter().map(|(id, n, d, s, sd, rd, ra)| json!({"id": id, "name": n, "description": d, "status": s, "startDate": sd, "releaseDate": rd, "releasedAt": ra})).collect::<Vec<_>>(),
            "components": components.iter().map(|(id, n, d, l)| json!({"id": id, "name": n, "description": d, "leadId": l})).collect::<Vec<_>>(),
            "fields": fields.iter().map(|(id, n, k, o, d)| json!({"id": id, "name": n, "kind": k, "options": o, "description": d})).collect::<Vec<_>>(),
            "sprints": sprints.iter().map(|(id, n, g, s, sa, ea)| json!({"id": id, "name": n, "goal": g, "state": s, "startAt": sa, "endAt": ea})).collect::<Vec<_>>(),
            "boards": boards.iter().map(|(id, n, k)| json!({"id": id, "name": n, "kind": k})).collect::<Vec<_>>(),
            "transitions": transitions.iter().map(|(f, t)| json!({"from": f, "to": t})).collect::<Vec<_>>(),
        }),
    )))
}

pub fn valid_key(k: &str) -> ApiResult<String> {
    let k = k.trim().to_ascii_uppercase();
    if k.len() < 2 || k.len() > 10 || !k.starts_with(|c: char| c.is_ascii_uppercase()) || !k.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit()) {
        return Err(ApiError::bad("project keys are 2–10 letters or digits, starting with a letter (like WEB or APP2)"));
    }
    Ok(k)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewProject {
    key: String,
    name: String,
    /// scrum | kanban | tracker | business
    template: Option<String>,
    description: Option<String>,
    icon: Option<String>,
    color: Option<String>,
    lead_id: Option<Uuid>,
}

/// Statuses, issue types, fields and a board for a new project.
pub async fn apply_template(tx: &mut Transaction<'_, Postgres>, project: Uuid, key: &str, template: &str) -> ApiResult<()> {
    let statuses: &[(&str, &str, &str)] = match template {
        "kanban" => &[("Backlog", "todo", "#8b8d98"), ("Selected", "todo", "#5b8def"), ("In progress", "in_progress", "#f59e0b"), ("Review", "in_progress", "#a855f7"), ("Done", "done", "#22a06b")],
        "tracker" => &[("New", "todo", "#5b8def"), ("Open", "todo", "#8b8d98"), ("In progress", "in_progress", "#f59e0b"), ("Waiting", "in_progress", "#e5484d"), ("Resolved", "done", "#22a06b"), ("Closed", "done", "#5c6370")],
        "business" => &[("Not started", "todo", "#8b8d98"), ("Working on it", "in_progress", "#f59e0b"), ("Stuck", "in_progress", "#e5484d"), ("Done", "done", "#22a06b")],
        _ => &[("To do", "todo", "#8b8d98"), ("In progress", "in_progress", "#3b82f6"), ("In review", "in_progress", "#a855f7"), ("Done", "done", "#22a06b")],
    };
    let types: &[(&str, &str, &str, i16)] = match template {
        "tracker" => &[("Bug", "bug", "#e5484d", 0), ("Feature request", "lightbulb", "#22a06b", 0), ("Question", "help", "#5b8def", 0), ("Task", "task", "#3b82f6", 0), ("Sub-task", "subtask", "#64748b", -1)],
        "business" => &[("Task", "task", "#3b82f6", 0), ("Milestone", "flag", "#f59e0b", 1), ("Sub-task", "subtask", "#64748b", -1)],
        _ => &[("Epic", "epic", "#8b5cf6", 1), ("Story", "story", "#22a06b", 0), ("Task", "task", "#3b82f6", 0), ("Bug", "bug", "#e5484d", 0), ("Sub-task", "subtask", "#64748b", -1)],
    };
    let mut status_ids = Vec::new();
    for (i, (name, cat, color)) in statuses.iter().enumerate() {
        let id = new_id();
        sqlx::query("INSERT INTO statuses (id, project_id, name, category, color, position) VALUES ($1, $2, $3, $4, $5, $6)")
            .bind(id)
            .bind(project)
            .bind(name)
            .bind(cat)
            .bind(color)
            .bind(i as f64)
            .execute(&mut **tx)
            .await?;
        status_ids.push((id, *name, *cat));
    }
    for (i, (name, icon, color, level)) in types.iter().enumerate() {
        let template = match *name {
            "Bug" => Some(crate::doc::from_markdown("**Steps to reproduce**\n\n1. \n\n**Expected**\n\n\n**Actual**\n\n\n**Environment**\n")),
            "Story" => Some(crate::doc::from_markdown("As a … I want … so that …\n\n**Acceptance criteria**\n\n- [ ] ")),
            _ => None,
        };
        sqlx::query("INSERT INTO issue_types (id, project_id, name, icon, color, level, position, template) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)")
            .bind(new_id())
            .bind(project)
            .bind(name)
            .bind(icon)
            .bind(color)
            .bind(level)
            .bind(i as f64)
            .bind(template)
            .execute(&mut **tx)
            .await?;
    }
    if template == "tracker" {
        let options = json!([
            {"id": "s1", "label": "Critical", "color": "#e5484d"},
            {"id": "s2", "label": "Major", "color": "#f59e0b"},
            {"id": "s3", "label": "Minor", "color": "#5b8def"},
            {"id": "s4", "label": "Trivial", "color": "#8b8d98"}
        ]);
        sqlx::query("INSERT INTO custom_fields (id, project_id, name, kind, options, position) VALUES ($1, $2, 'Severity', 'select', $3, 0), ($4, $2, 'Environment', 'text', '[]', 1)")
            .bind(new_id())
            .bind(project)
            .bind(options)
            .bind(new_id())
            .execute(&mut **tx)
            .await?;
    }
    if template == "business" {
        sqlx::query("INSERT INTO custom_fields (id, project_id, name, kind, options, position) VALUES ($1, $2, 'Budget', 'number', '[]', 0)")
            .bind(new_id())
            .bind(project)
            .execute(&mut **tx)
            .await?;
    }
    // a board: one column per status (kanban: the backlog stays in the list views)
    let scrum = matches!(template, "scrum" | "");
    let columns: Vec<Value> = status_ids
        .iter()
        .filter(|(_, name, _)| !(template == "kanban" && *name == "Backlog"))
        .map(|(id, name, cat)| json!({"id": new_id(), "name": name, "statusIds": [id], "wipLimit": if *cat == "in_progress" && !scrum { Some(5) } else { None }}))
        .collect();
    let config = json!({
        "columns": columns,
        "swimlanes": "none",
        "quickFilters": [
            {"name": "Only my issues", "query": "assignee = me()"},
            {"name": "Recently updated", "query": "updated > -1d"},
            {"name": "Blocked", "query": "blocked = true"}
        ],
        "cardFields": ["priority", "assignee", "points", "due", "labels"],
        "hideDoneAfterDays": 14,
    });
    sqlx::query("INSERT INTO boards (id, project_id, name, kind, config) VALUES ($1, $2, $3, $4, $5)")
        .bind(new_id())
        .bind(project)
        .bind(if scrum { format!("{key} sprint board") } else { format!("{key} board") })
        .bind(if scrum { "scrum" } else { "kanban" })
        .bind(config)
        .execute(&mut **tx)
        .await?;
    if scrum {
        sqlx::query("INSERT INTO sprints (id, project_id, name, state, position) VALUES ($1, $2, $3, 'future', 1)")
            .bind(new_id())
            .bind(project)
            .bind(format!("{key} Sprint 1"))
            .execute(&mut **tx)
            .await?;
    }
    Ok(())
}

pub async fn create(State(state): State<AppState>, CurrentUser(user): CurrentUser, Json(req): Json<NewProject>) -> ApiResult<(StatusCode, Json<Value>)> {
    if user.is_guest() {
        return Err(ApiError::Denied("guests can't create projects".into()));
    }
    let p = create_project(&state, &user, req).await?;
    Ok((StatusCode::CREATED, Json(json!({"id": p.0, "key": p.1}))))
}

/// A project to create (sample data, imports).
pub struct NewProjectSpec {
    pub key: String,
    pub name: String,
    pub template: String,
    pub description: Option<String>,
    pub icon: String,
    pub color: String,
}

pub async fn create_project_spec(state: &AppState, user: &User, s: NewProjectSpec) -> ApiResult<(Uuid, String)> {
    create_project(
        state,
        user,
        NewProject { key: s.key, name: s.name, template: Some(s.template), description: s.description, icon: Some(s.icon), color: Some(s.color), lead_id: None },
    )
    .await
}

pub async fn create_project(state: &AppState, user: &User, req: NewProject) -> ApiResult<(Uuid, String)> {
    let key = valid_key(&req.key)?;
    let name = req.name.trim();
    if name.is_empty() {
        return Err(ApiError::bad("a project needs a name"));
    }
    let template = req.template.as_deref().unwrap_or("scrum");
    if !matches!(template, "scrum" | "kanban" | "tracker" | "business") {
        return Err(ApiError::bad("template must be scrum, kanban, tracker or business"));
    }
    let taken: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM project_key_aliases WHERE workspace_id = $1 AND lower(key) = lower($2))")
        .bind(user.workspace_id)
        .bind(&key)
        .fetch_one(&state.db)
        .await?;
    if taken {
        return Err(ApiError::Conflict(format!("{key} was used by a project before (its old issue links still point there)")));
    }
    let id = new_id();
    let mut tx = state.db.begin().await?;
    let kind = match template {
        "scrum" => "scrum",
        "tracker" => "tracker",
        _ => "kanban",
    };
    let estimation = if template == "tracker" || template == "business" { "hours" } else { "points" };
    sqlx::query(
        "INSERT INTO projects (id, workspace_id, key, name, description, icon, color, kind, lead_id, created_by, settings) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
    )
    .bind(id)
    .bind(user.workspace_id)
    .bind(&key)
    .bind(name)
    .bind(req.description.as_deref().map(str::trim).filter(|d| !d.is_empty()))
    .bind(req.icon.as_deref().unwrap_or("rocket"))
    .bind(req.color.as_deref().unwrap_or("#4f6bed"))
    .bind(kind)
    .bind(req.lead_id.unwrap_or(user.id))
    .bind(user.id)
    .bind(json!({"estimation": estimation, "workflow": "free", "template": template}))
    .execute(&mut *tx)
    .await
    .map_err(|e| match ApiError::from(e) {
        ApiError::Conflict(_) => ApiError::Conflict(format!("the key {key} is taken")),
        other => other,
    })?;
    sqlx::query("INSERT INTO project_members (project_id, subject_type, subject_id, role) VALUES ($1, 'user', $2, 'admin')").bind(id).bind(user.id).execute(&mut *tx).await?;
    sqlx::query("INSERT INTO project_stars (user_id, project_id) VALUES ($1, $2)").bind(user.id).bind(id).execute(&mut *tx).await?;
    apply_template(&mut tx, id, &key, template).await?;
    tx.commit().await?;
    audit(state, user.workspace_id, Some(user.id), "project.created", Some(key.clone()), json!({"template": template})).await;
    events::publish(state, Event::new(user.workspace_id, "project").project(id).actor(user.id)).await;
    Ok((id, key))
}

pub async fn update(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(key): Path<String>, Json(req): Json<Map<String, Value>>) -> ApiResult<Json<Value>> {
    let (p, _) = by_key(&state, &user, &key, Role::Admin).await?;
    let mut tx = state.db.begin().await?;
    for (k, v) in &req {
        match k.as_str() {
            "name" => {
                let n = v.as_str().map(str::trim).filter(|n| !n.is_empty()).ok_or_else(|| ApiError::bad("a project needs a name"))?;
                sqlx::query("UPDATE projects SET name = $2 WHERE id = $1").bind(p.id).bind(n).execute(&mut *tx).await?;
            }
            "key" => {
                let nk = valid_key(v.as_str().unwrap_or(""))?;
                if nk != p.key {
                    // the old key keeps working for links and issue keys
                    sqlx::query("DELETE FROM project_key_aliases WHERE workspace_id = $1 AND upper(key) = $2 AND project_id = $3")
                        .bind(p.workspace_id)
                        .bind(&nk)
                        .bind(p.id)
                        .execute(&mut *tx)
                        .await?;
                    sqlx::query("INSERT INTO project_key_aliases (workspace_id, key, project_id) VALUES ($1, $2, $3) ON CONFLICT DO NOTHING")
                        .bind(p.workspace_id)
                        .bind(&p.key)
                        .bind(p.id)
                        .execute(&mut *tx)
                        .await?;
                    let other_alias: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM project_key_aliases WHERE workspace_id = $1 AND upper(key) = $2)")
                        .bind(p.workspace_id)
                        .bind(&nk)
                        .fetch_one(&mut *tx)
                        .await?;
                    if other_alias {
                        return Err(ApiError::Conflict(format!("{nk} was used by another project before")));
                    }
                    sqlx::query("UPDATE projects SET key = $2 WHERE id = $1").bind(p.id).bind(&nk).execute(&mut *tx).await.map_err(|e| match ApiError::from(e) {
                        ApiError::Conflict(_) => ApiError::Conflict(format!("the key {nk} is taken")),
                        other => other,
                    })?;
                }
            }
            "description" | "icon" | "color" => {
                let s = v.as_str().map(str::trim).filter(|s| !s.is_empty());
                sqlx::query(sqlx::AssertSqlSafe(format!("UPDATE projects SET {k} = $2 WHERE id = $1"))).bind(p.id).bind(s).execute(&mut *tx).await?;
            }
            "leadId" => {
                let l = v.as_str().and_then(|s| s.parse::<Uuid>().ok());
                sqlx::query("UPDATE projects SET lead_id = $2 WHERE id = $1").bind(p.id).bind(l).execute(&mut *tx).await?;
            }
            "defaultRole" => {
                let r = v.as_str().and_then(Role::parse).ok_or_else(|| ApiError::bad("defaultRole must be none, viewer, member or admin"))?;
                sqlx::query("UPDATE projects SET default_role = $2 WHERE id = $1").bind(p.id).bind(r.as_str()).execute(&mut *tx).await?;
            }
            "settings" => {
                let obj = v.as_object().ok_or_else(|| ApiError::bad("settings must be an object"))?;
                let mut cur = p.settings.as_object().cloned().unwrap_or_default();
                for (sk, sv) in obj {
                    match sk.as_str() {
                        "estimation" if !matches!(sv.as_str(), Some("points" | "hours")) => return Err(ApiError::bad("estimation must be points or hours")),
                        "workflow" if !matches!(sv.as_str(), Some("free" | "restricted")) => return Err(ApiError::bad("workflow must be free or restricted")),
                        "estimation" | "workflow" | "wikiSpace" | "defaultAssignee" | "wikiParentPage" | "doneAfterDays" => {}
                        other => return Err(ApiError::bad(format!("unknown setting {other}"))),
                    }
                    if sv.is_null() {
                        cur.remove(sk);
                    } else {
                        cur.insert(sk.clone(), sv.clone());
                    }
                }
                sqlx::query("UPDATE projects SET settings = $2 WHERE id = $1").bind(p.id).bind(Value::Object(cur)).execute(&mut *tx).await?;
            }
            other => return Err(ApiError::bad(format!("unknown field {other}"))),
        }
    }
    sqlx::query("UPDATE projects SET updated_at = now() WHERE id = $1").bind(p.id).execute(&mut *tx).await?;
    tx.commit().await?;
    events::publish(&state, Event::new(user.workspace_id, "project").project(p.id).actor(user.id)).await;
    let key: String = sqlx::query_scalar("SELECT key FROM projects WHERE id = $1").bind(p.id).fetch_one(&state.db).await?;
    Ok(Json(json!({"key": key})))
}

pub async fn archive(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(key): Path<String>, Json(req): Json<Value>) -> ApiResult<StatusCode> {
    let (p, _) = by_key(&state, &user, &key, Role::Admin).await?;
    let archived = req["archived"].as_bool().unwrap_or(true);
    sqlx::query("UPDATE projects SET archived_at = CASE WHEN $2 THEN now() ELSE NULL END WHERE id = $1").bind(p.id).bind(archived).execute(&state.db).await?;
    audit(&state, user.workspace_id, Some(user.id), if archived { "project.archived" } else { "project.restored" }, Some(p.key), json!({})).await;
    events::publish(&state, Event::new(user.workspace_id, "project").project(p.id).actor(user.id)).await;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct ConfirmDelete {
    confirm: String,
}

/// Deletes a project and all its issues (workspace admins; the key must be typed to confirm).
pub async fn delete(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(key): Path<String>, Json(req): Json<ConfirmDelete>) -> ApiResult<StatusCode> {
    crate::auth::require_admin(&user)?;
    let (p, _) = by_key(&state, &user, &key, Role::Admin).await?;
    if !req.confirm.eq_ignore_ascii_case(&p.key) {
        return Err(ApiError::bad("type the project key to confirm"));
    }
    let keys: Vec<String> = sqlx::query_scalar("SELECT storage_key FROM attachments a JOIN issues i ON i.id = a.issue_id WHERE i.project_id = $1").bind(p.id).fetch_all(&state.db).await?;
    sqlx::query("DELETE FROM projects WHERE id = $1").bind(p.id).execute(&state.db).await?;
    for k in keys {
        let _ = state.blobs.delete(&object_store::path::Path::from(k)).await;
    }
    audit(&state, user.workspace_id, Some(user.id), "project.deleted", Some(p.key), json!({"name": p.name})).await;
    events::publish(&state, Event::new(user.workspace_id, "project").actor(user.id)).await;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn star(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(key): Path<String>, Json(req): Json<Value>) -> ApiResult<StatusCode> {
    let (p, _) = by_key(&state, &user, &key, Role::Viewer).await?;
    if req["starred"].as_bool().unwrap_or(true) {
        sqlx::query("INSERT INTO project_stars (user_id, project_id) VALUES ($1, $2) ON CONFLICT DO NOTHING").bind(user.id).bind(p.id).execute(&state.db).await?;
    } else {
        sqlx::query("DELETE FROM project_stars WHERE user_id = $1 AND project_id = $2").bind(user.id).bind(p.id).execute(&state.db).await?;
    }
    Ok(StatusCode::NO_CONTENT)
}

pub async fn members(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(key): Path<String>) -> ApiResult<Json<Value>> {
    let (p, _) = by_key(&state, &user, &key, Role::Viewer).await?;
    let rows: Vec<(String, Uuid, String, Option<String>)> = sqlx::query_as(
        "SELECT m.subject_type, m.subject_id, m.role, COALESCE(u.name, t.name) FROM project_members m
           LEFT JOIN users u ON m.subject_type = 'user' AND u.id = m.subject_id
           LEFT JOIN teams t ON m.subject_type = 'team' AND t.id = m.subject_id
          WHERE m.project_id = $1 ORDER BY m.subject_type DESC, lower(COALESCE(u.name, t.name))",
    )
    .bind(p.id)
    .fetch_all(&state.db)
    .await?;
    // everyone who can see the project (for assignee pickers)
    let people: Vec<(Uuid, String, i32)> = sqlx::query_as(
        "SELECT u.id, u.name, r.rank FROM users u CROSS JOIN LATERAL (SELECT rank FROM tc_project_roles(u.id) WHERE project_id = $1) r
          WHERE u.workspace_id = $2 AND u.status = 'active' AND r.rank >= 1 ORDER BY lower(u.name)",
    )
    .bind(p.id)
    .bind(p.workspace_id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(json!({
        "defaultRole": p.default_role,
        "grants": rows.iter().map(|(t, id, r, n)| json!({"subjectType": t, "subjectId": id, "role": r, "name": n})).collect::<Vec<_>>(),
        "people": people.iter().map(|(id, n, r)| json!({"id": id, "name": n, "role": Role::from_rank(*r).as_str()})).collect::<Vec<_>>(),
    })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Grant {
    subject_type: String,
    subject_id: Uuid,
    role: String,
}

pub async fn put_members(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(key): Path<String>, Json(req): Json<Vec<Grant>>) -> ApiResult<StatusCode> {
    let (p, _) = by_key(&state, &user, &key, Role::Admin).await?;
    let mut tx = state.db.begin().await?;
    sqlx::query("DELETE FROM project_members WHERE project_id = $1").bind(p.id).execute(&mut *tx).await?;
    for g in &req {
        if !matches!(g.subject_type.as_str(), "user" | "team") || Role::parse(&g.role).is_none_or(|r| r == Role::None) {
            return Err(ApiError::bad("grants are {subjectType: user|team, subjectId, role: viewer|member|admin}"));
        }
        sqlx::query("INSERT INTO project_members (project_id, subject_type, subject_id, role) VALUES ($1, $2, $3, $4) ON CONFLICT DO NOTHING")
            .bind(p.id)
            .bind(&g.subject_type)
            .bind(g.subject_id)
            .bind(&g.role)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    // nobody may lock themselves out of administering the project
    if access::role(&state, &user, p.id).await? < Role::Admin {
        return Err(ApiError::bad("you would lose admin rights on this project; keep yourself (or a team of yours) as admin"));
    }
    audit(&state, user.workspace_id, Some(user.id), "project.members", Some(p.key), json!({"grants": req.len()})).await;
    events::publish(&state, Event::new(user.workspace_id, "project").project(p.id).actor(user.id)).await;
    Ok(StatusCode::NO_CONTENT)
}

// ---- statuses and workflow ----

fn category(v: &Value) -> ApiResult<String> {
    crate::query::parse_category(v.as_str().unwrap_or("")).map(str::to_string).ok_or_else(|| ApiError::bad("category must be todo, in_progress or done"))
}

async fn changed(state: &AppState, user: &User, project: Uuid) {
    events::publish(state, Event::new(user.workspace_id, "project").project(project).actor(user.id)).await;
}

pub async fn create_status(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(key): Path<String>, Json(req): Json<Value>) -> ApiResult<(StatusCode, Json<Value>)> {
    let (p, _) = by_key(&state, &user, &key, Role::Admin).await?;
    let name = req["name"].as_str().map(str::trim).filter(|n| !n.is_empty()).ok_or_else(|| ApiError::bad("a status needs a name"))?;
    let cat = category(&req["category"])?;
    let id = new_id();
    sqlx::query("INSERT INTO statuses (id, project_id, name, category, color, position) VALUES ($1, $2, $3, $4, $5, (SELECT COALESCE(max(position), 0) + 1 FROM statuses WHERE project_id = $2))")
        .bind(id)
        .bind(p.id)
        .bind(name)
        .bind(&cat)
        .bind(req["color"].as_str())
        .execute(&state.db)
        .await?;
    // free workflows: the new status is reachable from everywhere and back
    sqlx::query(
        "INSERT INTO transitions (project_id, from_status_id, to_status_id) SELECT $1, s.id, $2 FROM statuses s WHERE s.project_id = $1 AND s.id <> $2
         UNION ALL SELECT $1, $2, s.id FROM statuses s WHERE s.project_id = $1 AND s.id <> $2 ON CONFLICT DO NOTHING",
    )
    .bind(p.id)
    .bind(id)
    .execute(&state.db)
    .await?;
    changed(&state, &user, p.id).await;
    Ok((StatusCode::CREATED, Json(json!({"id": id}))))
}

async fn status_project(state: &AppState, user: &User, id: Uuid, table: &str) -> ApiResult<Uuid> {
    let p: Option<Uuid> =
        sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT x.project_id FROM {table} x JOIN projects p ON p.id = x.project_id WHERE x.id = $1 AND p.workspace_id = $2")))
            .bind(id)
            .bind(user.workspace_id)
            .fetch_optional(&state.db)
            .await?;
    let p = p.ok_or(ApiError::NotFound("item"))?;
    access::require(state, user, p, Role::Admin).await?;
    Ok(p)
}

pub async fn update_status(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(id): Path<Uuid>, Json(req): Json<Value>) -> ApiResult<StatusCode> {
    let project = status_project(&state, &user, id, "statuses").await?;
    if let Some(n) = req["name"].as_str().map(str::trim).filter(|n| !n.is_empty()) {
        sqlx::query("UPDATE statuses SET name = $2 WHERE id = $1").bind(id).bind(n).execute(&state.db).await?;
    }
    if !req["category"].is_null() {
        sqlx::query("UPDATE statuses SET category = $2 WHERE id = $1").bind(id).bind(category(&req["category"])?).execute(&state.db).await?;
    }
    if req.get("color").is_some() {
        sqlx::query("UPDATE statuses SET color = $2 WHERE id = $1").bind(id).bind(req["color"].as_str()).execute(&state.db).await?;
    }
    changed(&state, &user, project).await;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MoveTo {
    move_to: Option<Uuid>,
}

/// Deletes a status; its issues move to `moveTo`.
pub async fn delete_status(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(id): Path<Uuid>, Query(q): Query<MoveTo>) -> ApiResult<StatusCode> {
    let project = status_project(&state, &user, id, "statuses").await?;
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM statuses WHERE project_id = $1").bind(project).fetch_one(&state.db).await?;
    if count <= 1 {
        return Err(ApiError::bad("a project needs at least one status"));
    }
    let mut tx = state.db.begin().await?;
    let used: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM issues WHERE status_id = $1)").bind(id).fetch_one(&mut *tx).await?;
    if used {
        let to = q.move_to.ok_or_else(|| ApiError::bad("issues have this status: say where they go (moveTo)"))?;
        let ok: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM statuses WHERE id = $1 AND project_id = $2)").bind(to).bind(project).fetch_one(&mut *tx).await?;
        if !ok || to == id {
            return Err(ApiError::bad("moveTo must be another status of this project"));
        }
        sqlx::query("UPDATE issues SET status_id = $2, updated_at = now() WHERE status_id = $1").bind(id).bind(to).execute(&mut *tx).await?;
    }
    sqlx::query("DELETE FROM statuses WHERE id = $1").bind(id).execute(&mut *tx).await?;
    // boards forget the status
    let boards: Vec<(Uuid, Value)> = sqlx::query_as("SELECT id, config FROM boards WHERE project_id = $1").bind(project).fetch_all(&mut *tx).await?;
    for (bid, mut cfg) in boards {
        if let Some(cols) = cfg["columns"].as_array_mut() {
            for c in cols.iter_mut() {
                if let Some(ids) = c["statusIds"].as_array_mut() {
                    ids.retain(|s| s.as_str() != Some(&id.to_string()));
                }
            }
            cols.retain(|c| c["statusIds"].as_array().is_some_and(|a| !a.is_empty()));
        }
        sqlx::query("UPDATE boards SET config = $2 WHERE id = $1").bind(bid).bind(cfg).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    changed(&state, &user, project).await;
    Ok(StatusCode::NO_CONTENT)
}

/// Orders statuses (or issue types / custom fields): `{"ids": [...]}`.
pub async fn reorder(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path((key, what)): Path<(String, String)>, Json(req): Json<Value>) -> ApiResult<StatusCode> {
    let (p, _) = by_key(&state, &user, &key, Role::Admin).await?;
    let table = match what.as_str() {
        "statuses" => "statuses",
        "types" => "issue_types",
        "fields" => "custom_fields",
        "versions" => "versions",
        _ => return Err(ApiError::NotFound("list")),
    };
    let ids: Vec<Uuid> = serde_json::from_value(req["ids"].clone())?;
    for (i, id) in ids.iter().enumerate() {
        sqlx::query(sqlx::AssertSqlSafe(format!("UPDATE {table} SET position = $3 WHERE id = $1 AND project_id = $2"))).bind(id).bind(p.id).bind(i as f64).execute(&state.db).await?;
    }
    changed(&state, &user, p.id).await;
    Ok(StatusCode::NO_CONTENT)
}

/// Allowed moves: `{"workflow": "free" | "restricted", "transitions": [{"from", "to"}]}`.
pub async fn put_transitions(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(key): Path<String>, Json(req): Json<Value>) -> ApiResult<StatusCode> {
    let (p, _) = by_key(&state, &user, &key, Role::Admin).await?;
    let mode = req["workflow"].as_str().unwrap_or("free");
    if !matches!(mode, "free" | "restricted") {
        return Err(ApiError::bad("workflow must be free or restricted"));
    }
    let mut tx = state.db.begin().await?;
    sqlx::query("DELETE FROM transitions WHERE project_id = $1").bind(p.id).execute(&mut *tx).await?;
    for t in req["transitions"].as_array().into_iter().flatten() {
        let (Some(f), Some(to)) = (t["from"].as_str().and_then(|s| s.parse::<Uuid>().ok()), t["to"].as_str().and_then(|s| s.parse::<Uuid>().ok())) else {
            return Err(ApiError::bad("transitions are {from, to} status ids"));
        };
        sqlx::query(
            "INSERT INTO transitions (project_id, from_status_id, to_status_id) SELECT $1, $2, $3
              WHERE EXISTS (SELECT 1 FROM statuses WHERE id = $2 AND project_id = $1) AND EXISTS (SELECT 1 FROM statuses WHERE id = $3 AND project_id = $1) AND $2 <> $3
             ON CONFLICT DO NOTHING",
        )
        .bind(p.id)
        .bind(f)
        .bind(to)
        .execute(&mut *tx)
        .await?;
    }
    sqlx::query("UPDATE projects SET settings = jsonb_set(settings, '{workflow}', to_jsonb($2::text)) WHERE id = $1").bind(p.id).bind(mode).execute(&mut *tx).await?;
    tx.commit().await?;
    changed(&state, &user, p.id).await;
    Ok(StatusCode::NO_CONTENT)
}

// ---- issue types ----

pub async fn create_type(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(key): Path<String>, Json(req): Json<Value>) -> ApiResult<(StatusCode, Json<Value>)> {
    let (p, _) = by_key(&state, &user, &key, Role::Admin).await?;
    let name = req["name"].as_str().map(str::trim).filter(|n| !n.is_empty()).ok_or_else(|| ApiError::bad("a type needs a name"))?;
    let level = req["level"].as_i64().unwrap_or(0).clamp(-1, 1) as i16;
    let id = new_id();
    sqlx::query(
        "INSERT INTO issue_types (id, project_id, name, icon, color, level, position) VALUES ($1, $2, $3, $4, $5, $6, (SELECT COALESCE(max(position), 0) + 1 FROM issue_types WHERE project_id = $2))",
    )
    .bind(id)
    .bind(p.id)
    .bind(name)
    .bind(req["icon"].as_str().unwrap_or("task"))
    .bind(req["color"].as_str().unwrap_or("#3b82f6"))
    .bind(level)
    .execute(&state.db)
    .await?;
    changed(&state, &user, p.id).await;
    Ok((StatusCode::CREATED, Json(json!({"id": id}))))
}

pub async fn update_type(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(id): Path<Uuid>, Json(req): Json<Value>) -> ApiResult<StatusCode> {
    let project = status_project(&state, &user, id, "issue_types").await?;
    if let Some(n) = req["name"].as_str().map(str::trim).filter(|n| !n.is_empty()) {
        sqlx::query("UPDATE issue_types SET name = $2 WHERE id = $1").bind(id).bind(n).execute(&state.db).await?;
    }
    for col in ["icon", "color"] {
        if let Some(v) = req[col].as_str() {
            sqlx::query(sqlx::AssertSqlSafe(format!("UPDATE issue_types SET {col} = $2 WHERE id = $1"))).bind(id).bind(v).execute(&state.db).await?;
        }
    }
    if let Some(l) = req["level"].as_i64() {
        sqlx::query("UPDATE issue_types SET level = $2 WHERE id = $1").bind(id).bind(l.clamp(-1, 1) as i16).execute(&state.db).await?;
    }
    if let Some(t) = req.get("template") {
        let t = match t {
            Value::String(md) if !md.trim().is_empty() => Some(crate::doc::from_markdown(md)),
            Value::Object(_) => {
                crate::doc::validate(t).map_err(ApiError::BadRequest)?;
                Some(t.clone())
            }
            _ => None,
        };
        sqlx::query("UPDATE issue_types SET template = $2 WHERE id = $1").bind(id).bind(t).execute(&state.db).await?;
    }
    changed(&state, &user, project).await;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn delete_type(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(id): Path<Uuid>, Query(q): Query<MoveTo>) -> ApiResult<StatusCode> {
    let project = status_project(&state, &user, id, "issue_types").await?;
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM issue_types WHERE project_id = $1").bind(project).fetch_one(&state.db).await?;
    if count <= 1 {
        return Err(ApiError::bad("a project needs at least one issue type"));
    }
    let mut tx = state.db.begin().await?;
    let used: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM issues WHERE type_id = $1)").bind(id).fetch_one(&mut *tx).await?;
    if used {
        let to = q.move_to.ok_or_else(|| ApiError::bad("issues have this type: say which type they get (moveTo)"))?;
        let ok: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM issue_types WHERE id = $1 AND project_id = $2)").bind(to).bind(project).fetch_one(&mut *tx).await?;
        if !ok || to == id {
            return Err(ApiError::bad("moveTo must be another type of this project"));
        }
        sqlx::query("UPDATE issues SET type_id = $2 WHERE type_id = $1").bind(id).bind(to).execute(&mut *tx).await?;
    }
    sqlx::query("DELETE FROM issue_types WHERE id = $1").bind(id).execute(&mut *tx).await?;
    tx.commit().await?;
    changed(&state, &user, project).await;
    Ok(StatusCode::NO_CONTENT)
}

// ---- versions (releases) and components ----

fn date(v: &Value) -> Option<chrono::NaiveDate> {
    v.as_str().and_then(|s| chrono::NaiveDate::parse_from_str(s.get(..10).unwrap_or(s), "%Y-%m-%d").ok())
}

pub async fn create_version(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(key): Path<String>, Json(req): Json<Value>) -> ApiResult<(StatusCode, Json<Value>)> {
    let (p, _) = by_key(&state, &user, &key, Role::Member).await?;
    let name = req["name"].as_str().map(str::trim).filter(|n| !n.is_empty()).ok_or_else(|| ApiError::bad("a version needs a name"))?;
    let id = new_id();
    sqlx::query(
        "INSERT INTO versions (id, project_id, name, description, start_date, release_date, position) VALUES ($1, $2, $3, $4, $5, $6, (SELECT COALESCE(max(position), 0) + 1 FROM versions WHERE project_id = $2))",
    )
    .bind(id)
    .bind(p.id)
    .bind(name)
    .bind(req["description"].as_str())
    .bind(date(&req["startDate"]))
    .bind(date(&req["releaseDate"]))
    .execute(&state.db)
    .await?;
    changed(&state, &user, p.id).await;
    Ok((StatusCode::CREATED, Json(json!({"id": id}))))
}

async fn member_of(state: &AppState, user: &User, id: Uuid, table: &str) -> ApiResult<Uuid> {
    let p: Option<Uuid> =
        sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT x.project_id FROM {table} x JOIN projects p ON p.id = x.project_id WHERE x.id = $1 AND p.workspace_id = $2")))
            .bind(id)
            .bind(user.workspace_id)
            .fetch_optional(&state.db)
            .await?;
    let p = p.ok_or(ApiError::NotFound("item"))?;
    access::require(state, user, p, Role::Member).await?;
    Ok(p)
}

pub async fn update_version(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(id): Path<Uuid>, Json(req): Json<Value>) -> ApiResult<StatusCode> {
    let project = member_of(&state, &user, id, "versions").await?;
    if let Some(n) = req["name"].as_str().map(str::trim).filter(|n| !n.is_empty()) {
        sqlx::query("UPDATE versions SET name = $2 WHERE id = $1").bind(id).bind(n).execute(&state.db).await?;
    }
    if req.get("description").is_some() {
        sqlx::query("UPDATE versions SET description = $2 WHERE id = $1").bind(id).bind(req["description"].as_str()).execute(&state.db).await?;
    }
    if req.get("startDate").is_some() {
        sqlx::query("UPDATE versions SET start_date = $2 WHERE id = $1").bind(id).bind(date(&req["startDate"])).execute(&state.db).await?;
    }
    if req.get("releaseDate").is_some() {
        sqlx::query("UPDATE versions SET release_date = $2 WHERE id = $1").bind(id).bind(date(&req["releaseDate"])).execute(&state.db).await?;
    }
    if let Some(s) = req["status"].as_str() {
        if !matches!(s, "unreleased" | "released" | "archived") {
            return Err(ApiError::bad("status must be unreleased, released or archived"));
        }
        sqlx::query("UPDATE versions SET status = $2, released_at = CASE WHEN $2 = 'released' THEN COALESCE(released_at, now()) WHEN $2 = 'unreleased' THEN NULL ELSE released_at END WHERE id = $1")
            .bind(id)
            .bind(s)
            .execute(&state.db)
            .await?;
    }
    changed(&state, &user, project).await;
    Ok(StatusCode::NO_CONTENT)
}

/// Releases a version; unfinished issues move to `moveTo` (another version) or lose the version.
pub async fn release_version(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(id): Path<Uuid>, Json(req): Json<Value>) -> ApiResult<Json<Value>> {
    let project = member_of(&state, &user, id, "versions").await?;
    let move_to = req["moveTo"].as_str().and_then(|s| s.parse::<Uuid>().ok());
    let unfinished: Vec<Uuid> = sqlx::query_scalar(
        "SELECT i.id FROM issues i JOIN statuses s ON s.id = i.status_id WHERE i.version_id = $1 AND i.deleted_at IS NULL AND s.category <> 'done'",
    )
    .bind(id)
    .fetch_all(&state.db)
    .await?;
    let actor = crate::issues::Actor::of(&user);
    for issue in &unfinished {
        let mut patch = Map::new();
        patch.insert("versionId".into(), json!(move_to));
        crate::issues::update(&state, &actor, *issue, &patch).await?;
    }
    sqlx::query("UPDATE versions SET status = 'released', released_at = now(), release_date = COALESCE(release_date, current_date) WHERE id = $1").bind(id).execute(&state.db).await?;
    let name: String = sqlx::query_scalar("SELECT name FROM versions WHERE id = $1").bind(id).fetch_one(&state.db).await?;
    crate::webhooks::enqueue(&state, user.workspace_id, Some(project), "version.released", json!({"version": {"id": id, "name": name}, "moved": unfinished.len()})).await;
    changed(&state, &user, project).await;
    Ok(Json(json!({"moved": unfinished.len()})))
}

pub async fn delete_version(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    let project = member_of(&state, &user, id, "versions").await?;
    sqlx::query("DELETE FROM versions WHERE id = $1").bind(id).execute(&state.db).await?;
    changed(&state, &user, project).await;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn create_component(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(key): Path<String>, Json(req): Json<Value>) -> ApiResult<(StatusCode, Json<Value>)> {
    let (p, _) = by_key(&state, &user, &key, Role::Admin).await?;
    let name = req["name"].as_str().map(str::trim).filter(|n| !n.is_empty()).ok_or_else(|| ApiError::bad("a component needs a name"))?;
    let id = new_id();
    sqlx::query("INSERT INTO components (id, project_id, name, description, lead_id) VALUES ($1, $2, $3, $4, $5)")
        .bind(id)
        .bind(p.id)
        .bind(name)
        .bind(req["description"].as_str())
        .bind(req["leadId"].as_str().and_then(|s| s.parse::<Uuid>().ok()))
        .execute(&state.db)
        .await?;
    changed(&state, &user, p.id).await;
    Ok((StatusCode::CREATED, Json(json!({"id": id}))))
}

pub async fn update_component(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(id): Path<Uuid>, Json(req): Json<Value>) -> ApiResult<StatusCode> {
    let project = status_project(&state, &user, id, "components").await?;
    if let Some(n) = req["name"].as_str().map(str::trim).filter(|n| !n.is_empty()) {
        sqlx::query("UPDATE components SET name = $2 WHERE id = $1").bind(id).bind(n).execute(&state.db).await?;
    }
    if req.get("description").is_some() {
        sqlx::query("UPDATE components SET description = $2 WHERE id = $1").bind(id).bind(req["description"].as_str()).execute(&state.db).await?;
    }
    if req.get("leadId").is_some() {
        sqlx::query("UPDATE components SET lead_id = $2 WHERE id = $1").bind(id).bind(req["leadId"].as_str().and_then(|s| s.parse::<Uuid>().ok())).execute(&state.db).await?;
    }
    changed(&state, &user, project).await;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn delete_component(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    let project = status_project(&state, &user, id, "components").await?;
    sqlx::query("DELETE FROM components WHERE id = $1").bind(id).execute(&state.db).await?;
    changed(&state, &user, project).await;
    Ok(StatusCode::NO_CONTENT)
}

// ---- custom fields ----

const FIELD_KINDS: &[&str] = &["text", "number", "select", "multiselect", "date", "user", "checkbox", "url"];

fn clean_options(v: &Value) -> ApiResult<Value> {
    let mut out = Vec::new();
    for o in v.as_array().into_iter().flatten() {
        let label = o["label"].as_str().map(str::trim).filter(|l| !l.is_empty()).ok_or_else(|| ApiError::bad("every option needs a label"))?;
        let id = o["id"].as_str().filter(|s| !s.is_empty()).map(str::to_string).unwrap_or_else(|| new_id().simple().to_string()[..12].to_string());
        out.push(json!({"id": id, "label": label, "color": o["color"].as_str()}));
    }
    Ok(Value::Array(out))
}

pub async fn create_field(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(key): Path<String>, Json(req): Json<Value>) -> ApiResult<(StatusCode, Json<Value>)> {
    let (p, _) = by_key(&state, &user, &key, Role::Admin).await?;
    let name = req["name"].as_str().map(str::trim).filter(|n| !n.is_empty()).ok_or_else(|| ApiError::bad("a field needs a name"))?;
    if crate::query::builtin_fields().contains(&name.to_lowercase().as_str()) {
        return Err(ApiError::bad(format!("{name} is a built-in field name; pick another")));
    }
    let kind = req["kind"].as_str().filter(|k| FIELD_KINDS.contains(k)).ok_or_else(|| ApiError::bad(format!("kind must be one of {}", FIELD_KINDS.join(", "))))?;
    let id = new_id();
    sqlx::query(
        "INSERT INTO custom_fields (id, project_id, name, kind, options, description, position) VALUES ($1, $2, $3, $4, $5, $6, (SELECT COALESCE(max(position), 0) + 1 FROM custom_fields WHERE project_id = $2))",
    )
    .bind(id)
    .bind(p.id)
    .bind(name)
    .bind(kind)
    .bind(clean_options(&req["options"])?)
    .bind(req["description"].as_str())
    .execute(&state.db)
    .await?;
    changed(&state, &user, p.id).await;
    Ok((StatusCode::CREATED, Json(json!({"id": id}))))
}

pub async fn update_field(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(id): Path<Uuid>, Json(req): Json<Value>) -> ApiResult<StatusCode> {
    let project = status_project(&state, &user, id, "custom_fields").await?;
    if let Some(n) = req["name"].as_str().map(str::trim).filter(|n| !n.is_empty()) {
        sqlx::query("UPDATE custom_fields SET name = $2 WHERE id = $1").bind(id).bind(n).execute(&state.db).await?;
    }
    if req.get("description").is_some() {
        sqlx::query("UPDATE custom_fields SET description = $2 WHERE id = $1").bind(id).bind(req["description"].as_str()).execute(&state.db).await?;
    }
    if req.get("options").is_some() {
        sqlx::query("UPDATE custom_fields SET options = $2 WHERE id = $1").bind(id).bind(clean_options(&req["options"])?).execute(&state.db).await?;
    }
    changed(&state, &user, project).await;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn delete_field(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    let project = status_project(&state, &user, id, "custom_fields").await?;
    sqlx::query("DELETE FROM custom_fields WHERE id = $1").bind(id).execute(&state.db).await?;
    sqlx::query("UPDATE issues SET custom = custom - $2 WHERE project_id = $1 AND custom ? $2").bind(project).bind(id.to_string()).execute(&state.db).await?;
    changed(&state, &user, project).await;
    Ok(StatusCode::NO_CONTENT)
}

// ---- automation rules ----

pub async fn rules(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(key): Path<String>) -> ApiResult<Json<Value>> {
    let (p, _) = by_key(&state, &user, &key, Role::Viewer).await?;
    let rows: Vec<(Uuid, String, bool, Value, String, Value, i32, Option<chrono::DateTime<chrono::Utc>>)> = sqlx::query_as(
        "SELECT id, name, enabled, trigger, condition, actions, runs, last_run_at FROM automation_rules WHERE project_id = $1 ORDER BY created_at",
    )
    .bind(p.id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(json!(
        rows.into_iter()
            .map(|(id, n, e, t, c, a, r, l)| json!({"id": id, "name": n, "enabled": e, "trigger": t, "condition": c, "actions": a, "runs": r, "lastRunAt": l}))
            .collect::<Vec<_>>()
    )))
}

fn check_rule(req: &Value) -> ApiResult<()> {
    if !matches!(req["trigger"]["event"].as_str(), Some("created" | "status" | "assigned" | "field" | "commented")) {
        return Err(ApiError::bad("trigger.event must be created, status, assigned, field or commented"));
    }
    if let Some(c) = req["condition"].as_str().filter(|c| !c.trim().is_empty()) {
        crate::query::parse(c)?;
    }
    let actions = req["actions"].as_array().ok_or_else(|| ApiError::bad("actions must be a list"))?;
    if actions.is_empty() || actions.len() > 10 {
        return Err(ApiError::bad("a rule needs 1–10 actions"));
    }
    for a in actions {
        if !matches!(a["action"].as_str(), Some("setStatus" | "assign" | "setPriority" | "addLabel" | "removeLabel" | "setSprint" | "comment")) {
            return Err(ApiError::bad("unknown action"));
        }
    }
    Ok(())
}

pub async fn create_rule(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(key): Path<String>, Json(req): Json<Value>) -> ApiResult<(StatusCode, Json<Value>)> {
    let (p, _) = by_key(&state, &user, &key, Role::Admin).await?;
    check_rule(&req)?;
    let id = new_id();
    sqlx::query("INSERT INTO automation_rules (id, project_id, name, enabled, trigger, condition, actions, created_by) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)")
        .bind(id)
        .bind(p.id)
        .bind(req["name"].as_str().map(str::trim).filter(|n| !n.is_empty()).unwrap_or("Rule"))
        .bind(req["enabled"].as_bool().unwrap_or(true))
        .bind(&req["trigger"])
        .bind(req["condition"].as_str().unwrap_or(""))
        .bind(&req["actions"])
        .bind(user.id)
        .execute(&state.db)
        .await?;
    Ok((StatusCode::CREATED, Json(json!({"id": id}))))
}

pub async fn update_rule(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(id): Path<Uuid>, Json(req): Json<Value>) -> ApiResult<StatusCode> {
    status_project(&state, &user, id, "automation_rules").await?;
    if req.get("trigger").is_some() || req.get("actions").is_some() {
        check_rule(&req)?;
        sqlx::query("UPDATE automation_rules SET trigger = $2, actions = $3, condition = $4 WHERE id = $1")
            .bind(id)
            .bind(&req["trigger"])
            .bind(&req["actions"])
            .bind(req["condition"].as_str().unwrap_or(""))
            .execute(&state.db)
            .await?;
    }
    if let Some(n) = req["name"].as_str().map(str::trim).filter(|n| !n.is_empty()) {
        sqlx::query("UPDATE automation_rules SET name = $2 WHERE id = $1").bind(id).bind(n).execute(&state.db).await?;
    }
    if let Some(e) = req["enabled"].as_bool() {
        sqlx::query("UPDATE automation_rules SET enabled = $2 WHERE id = $1").bind(id).bind(e).execute(&state.db).await?;
    }
    Ok(StatusCode::NO_CONTENT)
}

pub async fn delete_rule(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    status_project(&state, &user, id, "automation_rules").await?;
    sqlx::query("DELETE FROM automation_rules WHERE id = $1").bind(id).execute(&state.db).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Labels in use (for pickers), optionally starting with `prefix`.
pub async fn labels(State(state): State<AppState>, CurrentUser(user): CurrentUser, Query(q): Query<std::collections::HashMap<String, String>>) -> ApiResult<Json<Value>> {
    let visible = access::visible_projects(&state, &user).await?;
    let prefix = q.get("prefix").map(|p| crate::query::normalize_label(p)).unwrap_or_default();
    let rows: Vec<(String, i64)> = sqlx::query_as(
        "SELECT l.label, count(*) FROM issue_labels l JOIN issues i ON i.id = l.issue_id WHERE i.project_id = ANY($1) AND i.deleted_at IS NULL AND l.label LIKE $2 || '%'
          GROUP BY l.label ORDER BY count(*) DESC, l.label LIMIT 50",
    )
    .bind(&visible)
    .bind(prefix)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(json!(rows.into_iter().map(|(l, n)| json!({"label": l, "count": n})).collect::<Vec<_>>())))
}
