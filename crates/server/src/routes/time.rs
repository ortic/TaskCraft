//! Time tracking beyond single logs: the workspace's time categories (what time is spent on,
//! billable or not) and the time report (logged time grouped by person, project, issue,
//! category or period, as JSON or CSV).

use std::collections::HashMap;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use chrono::{Datelike, NaiveDate, Utc};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::app::{AppState, audit, new_id};
use crate::auth::{CurrentUser, User};
use crate::error::{ApiError, ApiResult};

// ---- categories ----

fn admin(user: &User) -> ApiResult<()> {
    if user.is_admin() { Ok(()) } else { Err(ApiError::Denied("only workspace admins manage time categories".into())) }
}

/// The workspace's time categories (archived ones too, marked).
pub async fn categories(State(state): State<AppState>, CurrentUser(user): CurrentUser) -> ApiResult<Json<Value>> {
    let rows: Vec<(Uuid, String, bool, bool, i64)> = sqlx::query_as(
        "SELECT c.id, c.name, c.billable, c.archived, (SELECT count(*) FROM worklogs w WHERE w.category_id = c.id)
           FROM time_categories c WHERE c.workspace_id = $1 ORDER BY c.position, lower(c.name)",
    )
    .bind(user.workspace_id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(json!(
        rows.into_iter()
            .map(|(id, name, billable, archived, logs)| json!({"id": id, "name": name, "billable": billable, "archived": archived, "logCount": logs}))
            .collect::<Vec<_>>()
    )))
}

fn category_name(v: &Value) -> ApiResult<String> {
    let n = v.as_str().map(str::trim).filter(|n| !n.is_empty()).ok_or_else(|| ApiError::bad("a time category needs a name"))?;
    if n.chars().count() > 60 {
        return Err(ApiError::bad("time category names are at most 60 characters"));
    }
    Ok(n.to_string())
}

fn duplicate(e: sqlx::Error) -> ApiError {
    match &e {
        sqlx::Error::Database(d) if d.is_unique_violation() => ApiError::bad("there is a time category of that name"),
        _ => e.into(),
    }
}

pub async fn create_category(State(state): State<AppState>, CurrentUser(user): CurrentUser, Json(req): Json<Value>) -> ApiResult<(StatusCode, Json<Value>)> {
    admin(&user)?;
    let name = category_name(&req["name"])?;
    let id = new_id();
    sqlx::query(
        "INSERT INTO time_categories (id, workspace_id, name, billable, position)
         VALUES ($1, $2, $3, $4, (SELECT COALESCE(max(position), 0) + 1 FROM time_categories WHERE workspace_id = $2))",
    )
    .bind(id)
    .bind(user.workspace_id)
    .bind(&name)
    .bind(req["billable"].as_bool().unwrap_or(false))
    .execute(&state.db)
    .await
    .map_err(duplicate)?;
    audit(&state, user.workspace_id, Some(user.id), "time.category", Some(name), json!({"created": true})).await;
    Ok((StatusCode::CREATED, Json(json!({"id": id}))))
}

pub async fn update_category(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<Uuid>,
    Json(req): Json<Value>,
) -> ApiResult<StatusCode> {
    admin(&user)?;
    let found: Option<String> = sqlx::query_scalar("SELECT name FROM time_categories WHERE id = $1 AND workspace_id = $2")
        .bind(id)
        .bind(user.workspace_id)
        .fetch_optional(&state.db)
        .await?;
    found.ok_or(ApiError::NotFound("time category"))?;
    if req.get("name").is_some() {
        sqlx::query("UPDATE time_categories SET name = $2 WHERE id = $1")
            .bind(id)
            .bind(category_name(&req["name"])?)
            .execute(&state.db)
            .await
            .map_err(duplicate)?;
    }
    for (k, col) in [("billable", "billable"), ("archived", "archived")] {
        if let Some(v) = req.get(k) {
            let b = v.as_bool().ok_or_else(|| ApiError::bad(format!("{k} must be true or false")))?;
            sqlx::query(sqlx::AssertSqlSafe(format!("UPDATE time_categories SET {col} = $2 WHERE id = $1"))).bind(id).bind(b).execute(&state.db).await?;
        }
    }
    Ok(StatusCode::NO_CONTENT)
}

/// Deletes a category; time logged with it keeps its billable flag and loses the category.
pub async fn delete_category(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    admin(&user)?;
    let name: Option<String> = sqlx::query_scalar("DELETE FROM time_categories WHERE id = $1 AND workspace_id = $2 RETURNING name")
        .bind(id)
        .bind(user.workspace_id)
        .fetch_optional(&state.db)
        .await?;
    let name = name.ok_or(ApiError::NotFound("time category"))?;
    audit(&state, user.workspace_id, Some(user.id), "time.category", Some(name), json!({"deleted": true})).await;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn order_categories(State(state): State<AppState>, CurrentUser(user): CurrentUser, Json(req): Json<Value>) -> ApiResult<StatusCode> {
    admin(&user)?;
    let ids: Vec<Uuid> = serde_json::from_value(req["ids"].clone()).map_err(|_| ApiError::bad("ids must be a list of ids"))?;
    for (i, id) in ids.iter().enumerate() {
        sqlx::query("UPDATE time_categories SET position = $3 WHERE id = $1 AND workspace_id = $2")
            .bind(id)
            .bind(user.workspace_id)
            .bind(i as f64)
            .execute(&state.db)
            .await?;
    }
    Ok(StatusCode::NO_CONTENT)
}

/// A time log's category (`categoryId`): one of the workspace's, not archived. Returns it with
/// its billable default; `None` for no category.
pub async fn category_of(state: &AppState, workspace: Uuid, v: &Value) -> ApiResult<Option<(Uuid, bool)>> {
    let Some(s) = v.as_str().filter(|s| !s.is_empty()) else {
        return if v.is_null() { Ok(None) } else { Err(ApiError::bad("categoryId must be an id or null")) };
    };
    let id: Uuid = s.parse().map_err(|_| ApiError::bad("categoryId must be an id or null"))?;
    let row: Option<(bool, bool)> = sqlx::query_as("SELECT billable, archived FROM time_categories WHERE id = $1 AND workspace_id = $2")
        .bind(id)
        .bind(workspace)
        .fetch_optional(&state.db)
        .await?;
    match row {
        None => Err(ApiError::bad("unknown time category")),
        Some((_, true)) => Err(ApiError::bad("that time category is archived")),
        Some((billable, false)) => Ok(Some((id, billable))),
    }
}

// ---- report ----

#[derive(Deserialize)]
pub struct ReportQuery {
    from: Option<NaiveDate>,
    to: Option<NaiveDate>,
    /// person | project | issue | category | day | week | month
    group: Option<String>,
    /// a second grouping inside the first
    then: Option<String>,
    /// project keys, comma separated
    projects: Option<String>,
    /// with `projects`: their subprojects too
    #[serde(default)]
    subprojects: bool,
    /// a person's id
    user: Option<Uuid>,
    /// a category's id, or `none`
    category: Option<String>,
    billable: Option<bool>,
    /// `csv`: every log as a row
    format: Option<String>,
}

const GROUPS: [&str; 7] = ["person", "project", "issue", "category", "day", "week", "month"];

#[derive(sqlx::FromRow)]
struct Log {
    work_date: NaiveDate,
    user_id: Option<Uuid>,
    user_name: Option<String>,
    project_id: Uuid,
    project_key: String,
    project_name: String,
    issue_id: Uuid,
    issue_key: String,
    title: String,
    category_id: Option<Uuid>,
    category_name: Option<String>,
    billable: bool,
    minutes: i32,
    note: Option<String>,
}

/// Where a log falls in a grouping: its id, its label and how groups sort (chronological for
/// periods, else by name).
fn bucket(l: &Log, group: &str) -> (String, String, String) {
    match group {
        "person" => {
            // someone removed: no id and no name (the page says so in its language)
            let name = l.user_name.clone().unwrap_or_default();
            (l.user_id.map(|u| u.to_string()).unwrap_or_default(), name.clone(), if name.is_empty() { "\u{10ffff}".into() } else { name.to_lowercase() })
        }
        "project" => (l.project_id.to_string(), format!("{} · {}", l.project_key, l.project_name), l.project_key.clone()),
        "issue" => (
            l.issue_id.to_string(),
            format!("{} {}", l.issue_key, l.title),
            format!("{}-{:09}", l.project_key, l.issue_key.rsplit('-').next().and_then(|n| n.parse::<u32>().ok()).unwrap_or(0)),
        ),
        "category" => {
            let name = l.category_name.clone().unwrap_or_default();
            (
                l.category_id.map(|c| c.to_string()).unwrap_or_else(|| "none".into()),
                name.clone(),
                if name.is_empty() { "\u{10ffff}".into() } else { name.to_lowercase() },
            )
        }
        "week" => {
            let monday = l.work_date - chrono::Duration::days(l.work_date.weekday().num_days_from_monday() as i64);
            (monday.to_string(), monday.to_string(), monday.to_string())
        }
        "month" => {
            let m = format!("{:04}-{:02}", l.work_date.year(), l.work_date.month());
            (m.clone(), m.clone(), m)
        }
        _ => (l.work_date.to_string(), l.work_date.to_string(), l.work_date.to_string()),
    }
}

#[derive(Default)]
struct Group {
    id: String,
    label: String,
    sort: String,
    minutes: i64,
    billable: i64,
    children: HashMap<String, Group>,
}

fn add(g: &mut Group, l: &Log) {
    g.minutes += l.minutes as i64;
    if l.billable {
        g.billable += l.minutes as i64;
    }
}

fn rows(groups: HashMap<String, Group>, by_time: bool) -> Vec<Value> {
    let mut list: Vec<Group> = groups.into_values().collect();
    if by_time {
        list.sort_by(|a, b| a.sort.cmp(&b.sort));
    } else {
        list.sort_by(|a, b| b.minutes.cmp(&a.minutes).then_with(|| a.sort.cmp(&b.sort)));
    }
    list.into_iter()
        .map(|g| {
            let mut v = json!({"id": g.id, "label": g.label, "minutes": g.minutes, "billableMinutes": g.billable});
            if !g.children.is_empty() {
                v["children"] = json!(rows(g.children, by_time));
            }
            v
        })
        .collect()
}

/// Logged time in a period, grouped (once or twice), over the issues the caller sees.
pub async fn report(State(state): State<AppState>, CurrentUser(user): CurrentUser, Query(q): Query<ReportQuery>) -> ApiResult<Response> {
    let today = Utc::now().date_naive();
    let from = q.from.unwrap_or_else(|| today.with_day(1).unwrap_or(today));
    let to = q.to.unwrap_or(today);
    if to < from || (to - from).num_days() > 731 {
        return Err(ApiError::bad("the period runs from `from` to `to`, at most two years"));
    }
    let group = q.group.as_deref().unwrap_or("person");
    let then = q.then.as_deref().filter(|t| !t.is_empty() && *t != "none");
    for g in std::iter::once(group).chain(then) {
        if !GROUPS.contains(&g) {
            return Err(ApiError::bad(format!("group by {}", GROUPS.join(", "))));
        }
    }
    let mut visible = crate::access::visible_projects(&state, &user).await?;
    if let Some(keys) = q.projects.as_deref().filter(|k| !k.trim().is_empty()) {
        let keys: Vec<String> = keys.split(',').map(|k| k.trim().to_uppercase()).filter(|k| !k.is_empty()).collect();
        let wanted: Vec<Uuid> =
            sqlx::query_scalar("SELECT CASE WHEN $3 THEN tc_project_tree(id) ELSE id END FROM projects WHERE workspace_id = $1 AND upper(key) = ANY($2)")
                .bind(user.workspace_id)
                .bind(&keys)
                .bind(q.subprojects)
                .fetch_all(&state.db)
                .await?;
        visible.retain(|p| wanted.contains(p));
    }
    let sql = format!(
        "SELECT w.work_date, w.user_id, u.name AS user_name, i.project_id, p.key AS project_key, p.name AS project_name, i.id AS issue_id,
                p.key || '-' || i.number AS issue_key, i.title, w.category_id, c.name AS category_name, w.billable, w.minutes, w.note
           FROM worklogs w JOIN issues i ON i.id = w.issue_id JOIN projects p ON p.id = i.project_id
           LEFT JOIN users u ON u.id = w.user_id LEFT JOIN time_categories c ON c.id = w.category_id
          WHERE w.work_date BETWEEN $1 AND $2 AND i.project_id = ANY($3) AND i.deleted_at IS NULL AND {}
            AND ($5::uuid IS NULL OR w.user_id = $5)
            AND ($6::text IS NULL OR ($6 = 'none' AND w.category_id IS NULL) OR w.category_id::text = $6)
            AND ($7::boolean IS NULL OR w.billable = $7)
          ORDER BY w.work_date, p.key, i.number, w.created_at LIMIT 100001",
        crate::access::sees_issue("i", "$4")
    );
    let mut logs: Vec<Log> = sqlx::query_as(sqlx::AssertSqlSafe(sql))
        .bind(from)
        .bind(to)
        .bind(&visible)
        .bind(user.id)
        .bind(q.user)
        .bind(q.category.as_deref().filter(|c| !c.is_empty()))
        .bind(q.billable)
        .fetch_all(&state.db)
        .await?;
    let truncated = logs.len() > 100_000;
    logs.truncate(100_000);

    if q.format.as_deref() == Some("csv") {
        let mut w = csv::Writer::from_writer(Vec::new());
        let _ = w.write_record(["Date", "Person", "Project", "Issue", "Title", "Category", "Billable", "Hours", "Minutes", "Note"]);
        let safe = |t: &str| if t.starts_with(['=', '+', '-', '@']) { format!("'{t}") } else { t.to_string() };
        for l in &logs {
            let _ = w.write_record([
                l.work_date.to_string(),
                l.user_name.clone().unwrap_or_default(),
                l.project_key.clone(),
                l.issue_key.clone(),
                safe(&l.title),
                l.category_name.clone().unwrap_or_default(),
                if l.billable { "yes" } else { "no" }.to_string(),
                format!("{:.2}", l.minutes as f64 / 60.0),
                l.minutes.to_string(),
                safe(l.note.as_deref().unwrap_or("")),
            ]);
        }
        let body = w.into_inner().map_err(|e| ApiError::Internal(anyhow::anyhow!("csv: {e}")))?;
        let name = format!("attachment; filename=\"time-{from}-{to}.csv\"");
        return Ok(([(header::CONTENT_TYPE, "text/csv; charset=utf-8".to_string()), (header::CONTENT_DISPOSITION, name)], body).into_response());
    }

    let mut groups: HashMap<String, Group> = HashMap::new();
    let (mut total, mut billable) = (0i64, 0i64);
    for l in &logs {
        total += l.minutes as i64;
        if l.billable {
            billable += l.minutes as i64;
        }
        let (id, label, sort) = bucket(l, group);
        let g = groups.entry(id.clone()).or_insert_with(|| Group { id, label, sort, ..Default::default() });
        add(g, l);
        if let Some(t) = then {
            let (id, label, sort) = bucket(l, t);
            let c = g.children.entry(id.clone()).or_insert_with(|| Group { id, label, sort, ..Default::default() });
            add(c, l);
        }
    }
    let by_time = |g: &str| matches!(g, "day" | "week" | "month");
    let mut out = rows(groups, by_time(group));
    // the inner grouping sorts its own way
    if let Some(t) = then
        && by_time(t) != by_time(group)
    {
        for r in out.iter_mut() {
            if let Some(children) = r.get_mut("children").and_then(Value::as_array_mut) {
                if by_time(t) {
                    children.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
                } else {
                    children.sort_by(|a, b| b["minutes"].as_i64().cmp(&a["minutes"].as_i64()));
                }
            }
        }
    }
    Ok(Json(json!({
        "from": from, "to": to, "group": group, "then": then,
        "totalMinutes": total, "billableMinutes": billable, "logCount": logs.len(), "truncated": truncated,
        "rows": out,
    }))
    .into_response())
}
