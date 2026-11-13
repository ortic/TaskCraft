//! A project export: one project with everything in it — its workflow, issue types, releases,
//! components, sprints, fields, issues with their history, comments, time logs, watchers,
//! links between its issues, and the files — in a `.zip` another TaskCraft imports
//! (*Administration → Import*), next to the backup of a whole workspace.
//!
//! The zip holds `taskcraft-project.json` (the import [`Model`], texts as TaskCraft documents)
//! and the files as `files/<attachment id>`. People are matched by e-mail address on import.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::io::{Read, Write};
use std::path::{Path as FsPath, PathBuf};

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::header;
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, NaiveDate, Utc};
use object_store::ObjectStoreExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use super::model::{self, Attachment, BoxFuture, Change, Field, File as Download, Files, Format, Item, Model, Text, TooLarge};
use crate::app::{AppState, audit};
use crate::auth::{SessionUser, require_admin};
use crate::error::{ApiError, ApiResult};

/// The entry that makes a zip a project export.
pub const ENTRY: &str = "taskcraft-project.json";
/// What the import jobs call this source.
pub const SOURCE: &str = "taskcraft";
const FORMAT: &str = "taskcraft-project";
const VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
struct Envelope {
    format: String,
    version: u32,
    exported_at: DateTime<Utc>,
    /// the project's key where it came from
    project: String,
    model: Model,
}

const PRIORITIES: [&str; 5] = ["None", "Low", "Medium", "High", "Urgent"];

fn doc_text(v: &Value) -> Option<Text> {
    let empty = v.is_null() || crate::doc::text(v).trim().is_empty() && !v.to_string().contains("\"image\"");
    (!empty).then(|| Text::new(v.to_string(), Format::Doc))
}

fn plain_text(s: Option<String>) -> Option<Text> {
    Text::some(s.as_deref(), Format::Plain)
}

/// The project as an import model, and its files (attachment id → storage key).
pub async fn model(state: &AppState, project: Uuid) -> anyhow::Result<(Model, Vec<(String, String)>)> {
    let db = &state.db;
    let pid = project.to_string();
    type P = (String, String, Option<String>, Option<Uuid>, String, Value);
    let (key, name, description, lead, kind, settings): P =
        sqlx::query_as("SELECT key, name, description, lead_id, kind, settings FROM projects WHERE id = $1").bind(project).fetch_one(db).await?;
    let former: Vec<String> = sqlx::query_scalar("SELECT key FROM project_key_aliases WHERE project_id = $1").bind(project).fetch_all(db).await?;
    let mut m = Model { origin: format!("taskcraft:{project}"), source: SOURCE.into(), markup: Format::Doc, global_refs: false, ..Default::default() };
    let mut people: BTreeSet<Uuid> = BTreeSet::new();
    let mut person = |u: Option<Uuid>| -> Option<String> {
        let u = u?;
        people.insert(u);
        Some(u.to_string())
    };
    m.projects.push(model::Project {
        id: pid.clone(),
        key: key.clone(),
        suggested_key: key.clone(),
        name,
        description: plain_text(description),
        lead: person(lead),
        former_keys: former,
        template: Some(kind),
        parent: None,
        path: Some(key.clone()),
    });
    m.resolutions = settings["resolutions"].as_array().into_iter().flatten().filter_map(|r| r.as_str().map(str::to_string)).collect();

    let statuses: Vec<(Uuid, String, String)> =
        sqlx::query_as("SELECT id, name, category FROM statuses WHERE project_id = $1 ORDER BY position").bind(project).fetch_all(db).await?;
    for (id, name, category) in &statuses {
        m.statuses.insert(id.to_string(), model::Status { name: name.clone(), category: Some(category.clone()), by_category: false, exact: true });
    }
    let types: Vec<(Uuid, String, i16)> = sqlx::query_as("SELECT id, name, level FROM issue_types WHERE project_id = $1").bind(project).fetch_all(db).await?;
    for (id, name, level) in types {
        m.types.insert(id.to_string(), model::IssueType { name, level });
    }
    for (p, name) in PRIORITIES.iter().enumerate() {
        m.priorities.insert(p.to_string(), model::Priority { name: name.to_string(), sequence: (4 - p) as f64 });
    }
    type C = (Uuid, String, Option<String>, Option<Uuid>);
    let components: Vec<C> = sqlx::query_as("SELECT id, name, description, lead_id FROM components WHERE project_id = $1").bind(project).fetch_all(db).await?;
    for (id, name, description, lead) in components {
        m.components.push(model::Component { id: id.to_string(), project: pid.clone(), name, description: plain_text(description), lead: person(lead) });
    }
    type V = (Uuid, String, Option<String>, String, Option<NaiveDate>, Option<NaiveDate>, f64);
    let versions: Vec<V> = sqlx::query_as(
        "SELECT id, name, description, status, start_date, release_date, position FROM versions WHERE project_id = $1 ORDER BY position, created_at",
    )
    .bind(project)
    .fetch_all(db)
    .await?;
    for (id, name, description, status, start, release, position) in versions {
        m.versions.push(model::Version {
            id: id.to_string(),
            project: pid.clone(),
            name,
            description: plain_text(description),
            sequence: position,
            released: status != "unreleased",
            archived: status == "archived",
            release_date: release,
            start_date: start,
        });
    }
    type S = (Uuid, String, Option<String>, String, Option<DateTime<Utc>>, Option<DateTime<Utc>>, Option<DateTime<Utc>>, f64);
    let sprints: Vec<S> = sqlx::query_as("SELECT id, name, goal, state, start_at, end_at, completed_at, position FROM sprints WHERE project_id = $1")
        .bind(project)
        .fetch_all(db)
        .await?;
    for (id, name, goal, state, start, end, completed, position) in sprints {
        m.sprints.insert(
            id.to_string(),
            model::Sprint { name, goal, closed: state == "closed", started: state != "future", start, end, completed, sequence: position },
        );
    }
    let fields: Vec<(Uuid, String, String, Value)> =
        sqlx::query_as("SELECT id, name, kind, options FROM custom_fields WHERE project_id = $1 ORDER BY position").bind(project).fetch_all(db).await?;
    // select options by id → label
    let mut option_labels: HashMap<String, String> = HashMap::new();
    let mut field_kinds: HashMap<String, String> = HashMap::new();
    for (id, name, kind, options) in &fields {
        let mut labels = Vec::new();
        for o in options.as_array().into_iter().flatten() {
            if let (Some(oid), Some(label)) = (o["id"].as_str(), o["label"].as_str()) {
                option_labels.insert(oid.to_string(), label.to_string());
                labels.push(label.to_string());
            }
        }
        field_kinds.insert(id.to_string(), kind.clone());
        m.fields.insert(id.to_string(), model::CustomField { name: name.clone(), kind: kind.clone(), options: labels });
    }

    // the issues, in rank order (the rank travels as its place)
    #[derive(sqlx::FromRow)]
    struct Row {
        id: Uuid,
        number: i32,
        type_id: Uuid,
        status_id: Uuid,
        priority: i16,
        resolution: Option<String>,
        resolved_at: Option<DateTime<Utc>>,
        title: String,
        description: Option<Value>,
        reporter_id: Option<Uuid>,
        assignee_id: Option<Uuid>,
        created_by: Option<Uuid>,
        created_at: DateTime<Utc>,
        updated_at: DateTime<Utc>,
        start_date: Option<NaiveDate>,
        due_date: Option<NaiveDate>,
        estimate_minutes: Option<i32>,
        remaining_minutes: Option<i32>,
        story_points: Option<f64>,
        sprint_id: Option<Uuid>,
        parent_id: Option<Uuid>,
        custom: Value,
        security: Option<String>,
    }
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT i.id, i.number, i.type_id, i.status_id, i.priority, i.resolution, i.resolved_at, i.title, i.description, i.reporter_id, i.assignee_id,
                i.created_by, i.created_at, i.updated_at, i.start_date, i.due_date, i.estimate_minutes, i.remaining_minutes, i.story_points, i.sprint_id,
                i.parent_id, i.custom, sl.name AS security
           FROM issues i LEFT JOIN security_levels sl ON sl.id = i.security_level_id
          WHERE i.project_id = $1 AND i.deleted_at IS NULL ORDER BY i.rank, i.number",
    )
    .bind(project)
    .fetch_all(db)
    .await?;
    let ids: Vec<Uuid> = rows.iter().map(|r| r.id).collect();
    let in_project: HashSet<Uuid> = ids.iter().copied().collect();

    let mut labels: HashMap<Uuid, Vec<String>> = HashMap::new();
    for (i, l) in
        sqlx::query_as::<_, (Uuid, String)>("SELECT issue_id, label FROM issue_labels WHERE issue_id = ANY($1) ORDER BY label").bind(&ids).fetch_all(db).await?
    {
        labels.entry(i).or_default().push(l);
    }
    let mut watchers: HashMap<Uuid, Vec<Uuid>> = HashMap::new();
    for (i, u) in sqlx::query_as::<_, (Uuid, Uuid)>("SELECT issue_id, user_id FROM issue_watchers WHERE issue_id = ANY($1)").bind(&ids).fetch_all(db).await? {
        watchers.entry(i).or_default().push(u);
    }
    let mut comps: HashMap<Uuid, Vec<String>> = HashMap::new();
    for (i, c) in
        sqlx::query_as::<_, (Uuid, Uuid)>("SELECT issue_id, component_id FROM issue_components WHERE issue_id = ANY($1)").bind(&ids).fetch_all(db).await?
    {
        comps.entry(i).or_default().push(c.to_string());
    }
    let mut vers: HashMap<(Uuid, String), Vec<String>> = HashMap::new();
    for (i, v, k) in sqlx::query_as::<_, (Uuid, Uuid, String)>("SELECT issue_id, version_id, kind FROM issue_versions WHERE issue_id = ANY($1)")
        .bind(&ids)
        .fetch_all(db)
        .await?
    {
        vers.entry((i, k)).or_default().push(v.to_string());
    }
    type Cm = (Uuid, Uuid, Option<Uuid>, Value, DateTime<Utc>, DateTime<Utc>, Option<String>);
    let mut comments: HashMap<Uuid, Vec<model::Comment>> = HashMap::new();
    for (id, issue, author, body, created, updated, visibility) in sqlx::query_as::<_, Cm>(
        "SELECT id, issue_id, author_id, body, created_at, updated_at, visibility FROM comments WHERE issue_id = ANY($1) ORDER BY created_at, id",
    )
    .bind(&ids)
    .fetch_all(db)
    .await?
    {
        comments.entry(issue).or_default().push(model::Comment {
            id: id.to_string(),
            author: person(author),
            body: doc_text(&body).unwrap_or_default(),
            created: Some(created),
            updated: Some(updated),
            restricted: visibility.is_some(),
        });
    }
    type W = (Uuid, Uuid, Option<Uuid>, i32, NaiveDate, Option<String>, DateTime<Utc>, Option<String>);
    let mut worklogs: HashMap<Uuid, Vec<model::Worklog>> = HashMap::new();
    for (id, issue, user, minutes, day, note, created, category) in sqlx::query_as::<_, W>(
        "SELECT w.id, w.issue_id, w.user_id, w.minutes, w.work_date, w.note, w.created_at, tc.name FROM worklogs w
           LEFT JOIN time_categories tc ON tc.id = w.category_id WHERE w.issue_id = ANY($1) ORDER BY w.created_at",
    )
    .bind(&ids)
    .fetch_all(db)
    .await?
    {
        worklogs.entry(issue).or_default().push(model::Worklog {
            id: id.to_string(),
            author: person(user),
            note: plain_text(note),
            created: Some(created),
            started: None,
            date: Some(day),
            seconds: minutes as f64 * 60.0,
            category,
        });
    }
    type A = (Uuid, Uuid, String, String, i64, Option<Uuid>, DateTime<Utc>, String);
    let mut attachments: HashMap<Uuid, Vec<Attachment>> = HashMap::new();
    let mut files = Vec::new();
    for (id, issue, name, mime, size, by, created, key) in sqlx::query_as::<_, A>(
        "SELECT id, issue_id, name, mime, size, uploaded_by, created_at, storage_key FROM attachments WHERE issue_id = ANY($1) ORDER BY created_at",
    )
    .bind(&ids)
    .fetch_all(db)
    .await?
    {
        files.push((id.to_string(), key));
        attachments.entry(issue).or_default().push(Attachment {
            id: id.to_string(),
            name,
            mime: Some(mime),
            size: Some(size as u64),
            author: person(by),
            created: Some(created),
            url: None,
            fallback_url: None,
            refs: vec![format!("/api/attachments/{id}")],
        });
    }
    type E = (Uuid, Option<Uuid>, String, Option<String>, Option<Value>, Option<Value>, DateTime<Utc>);
    let mut history: HashMap<Uuid, Vec<Change>> = HashMap::new();
    for (issue, actor, kind, field, old, new, at) in sqlx::query_as::<_, E>(
        "SELECT issue_id, actor_id, kind, field, old_value, new_value, created_at FROM issue_events WHERE issue_id = ANY($1) ORDER BY created_at, id",
    )
    .bind(&ids)
    .fetch_all(db)
    .await?
    {
        let author = person(actor);
        let items = if kind == "created" { created_items(new.as_ref()) } else { history_items(field.as_deref(), old.as_ref(), new.as_ref()) };
        if items.is_empty() {
            continue;
        }
        if let Some(Value::Object(a)) = new.as_ref().map(|n| &n["assignee"])
            && let Some(u) = a.get("id").and_then(Value::as_str).and_then(|s| s.parse().ok())
        {
            person(Some(u));
        }
        for it in &items {
            for v in [&it.old, &it.new] {
                if matches!(it.field, Field::Assignee | Field::Reporter)
                    && let Some(u) = v.as_deref().and_then(|s| s.parse().ok())
                {
                    person(Some(u));
                }
            }
        }
        history.entry(issue).or_default().push(Change { at: Some(at), author, initial: kind == "created", items });
    }

    for (place, r) in rows.into_iter().enumerate() {
        let mut custom = Vec::new();
        if let Value::Object(values) = &r.custom {
            for (fid, v) in values {
                let Some(kind) = field_kinds.get(fid) else { continue };
                let vals: Vec<String> = match (kind.as_str(), v) {
                    ("select", Value::String(o)) => option_labels.get(o).cloned().into_iter().collect(),
                    ("multiselect", Value::Array(os)) => os.iter().filter_map(|o| o.as_str().and_then(|o| option_labels.get(o).cloned())).collect(),
                    ("user", Value::String(u)) => {
                        let u: Option<Uuid> = u.parse().ok();
                        person(u).into_iter().collect()
                    }
                    (_, Value::String(s)) => vec![s.clone()],
                    (_, Value::Null) => vec![],
                    (_, other) => vec![other.to_string()],
                };
                if !vals.is_empty() {
                    custom.push((fid.clone(), vals));
                }
            }
        }
        let watchers_of: Vec<String> = watchers.remove(&r.id).unwrap_or_default().into_iter().filter_map(|u| person(Some(u))).collect();
        m.issues.push(model::Issue {
            id: r.id.to_string(),
            project: pid.clone(),
            number: r.number,
            keys: vec![format!("{key}-{}", r.number)],
            type_id: r.type_id.to_string(),
            status: r.status_id.to_string(),
            priority: Some(r.priority.to_string()),
            resolution: r.resolution.or_else(|| r.resolved_at.map(|_| String::new())),
            resolved: r.resolved_at,
            title: r.title,
            description: r.description.as_ref().and_then(doc_text),
            reporter: person(r.reporter_id),
            assignee: person(r.assignee_id),
            creator: person(r.created_by),
            created: Some(r.created_at),
            updated: Some(r.updated_at),
            start: r.start_date,
            due: r.due_date,
            original_estimate: r.estimate_minutes.map(|m| m as f64 * 60.0),
            remaining_estimate: r.remaining_minutes.map(|m| m as f64 * 60.0),
            points: r.story_points,
            sprints: r.sprint_id.map(|s| s.to_string()).into_iter().collect(),
            components: comps.remove(&r.id).unwrap_or_default(),
            versions: vers.remove(&(r.id, "fix".into())).unwrap_or_default(),
            affects: vers.remove(&(r.id, "affects".into())).unwrap_or_default(),
            labels: labels.remove(&r.id).unwrap_or_default(),
            watchers: watchers_of,
            custom,
            rank: Some(format!("{place:010}")),
            confidential: r.security.is_some(),
            security: r.security,
            comments: comments.remove(&r.id).unwrap_or_default(),
            worklogs: worklogs.remove(&r.id).unwrap_or_default(),
            attachments: attachments.remove(&r.id).unwrap_or_default(),
            history: history.remove(&r.id).unwrap_or_default(),
        });
        if let Some(parent) = r.parent_id.filter(|p| in_project.contains(p)) {
            m.parents.push((r.id.to_string(), parent.to_string()));
        }
    }
    // links between the project's own issues
    type L = (String, Uuid, Uuid, DateTime<Utc>);
    for (kind, source, target, created) in sqlx::query_as::<_, L>(
        "SELECT kind, source_id, target_id, created_at FROM issue_links WHERE source_id = ANY($1) AND target_id = ANY($1) ORDER BY created_at",
    )
    .bind(&ids)
    .fetch_all(db)
    .await?
    {
        m.links.push(model::Link {
            type_name: kind.clone(),
            kind,
            known: true,
            source: source.to_string(),
            target: target.to_string(),
            created: Some(created),
        });
    }
    let people: Vec<Uuid> = people.into_iter().collect();
    for (id, name, email, status) in sqlx::query_as::<_, (Uuid, String, Option<String>, String)>("SELECT id, name, email, status FROM users WHERE id = ANY($1)")
        .bind(&people)
        .fetch_all(db)
        .await?
    {
        m.people.insert(id.to_string(), model::Person { name, email, active: status == "active" });
    }
    Ok((m, files))
}

fn id_of(v: Option<&Value>) -> Option<String> {
    v.and_then(|v| v["id"].as_str()).map(str::to_string)
}

fn name_of(v: Option<&Value>) -> Option<String> {
    v.and_then(|v| v["name"].as_str()).map(str::to_string)
}

fn item(field: Field, old: Option<String>, old_name: Option<String>, new: Option<String>, new_name: Option<String>) -> Item {
    Item { field, old, old_name, new, new_name }
}

/// What an issue was created with (its first entry in the history).
fn created_items(new: Option<&Value>) -> Vec<Item> {
    let Some(n) = new else { return vec![] };
    let mut out = Vec::new();
    if let Some(s) = id_of(Some(&n["status"])) {
        out.push(item(Field::Status, None, None, Some(s), name_of(Some(&n["status"]))));
    }
    if let Some(s) = id_of(Some(&n["sprint"])) {
        out.push(item(Field::Sprint, None, None, Some(s), name_of(Some(&n["sprint"]))));
    }
    if let Some(p) = n["points"].as_f64() {
        out.push(Item::new(Field::Points, None, Some(p.to_string())));
    }
    match &n["assignee"] {
        Value::String(u) => out.push(item(Field::Assignee, None, None, Some(u.clone()), None)),
        a @ Value::Object(_) => {
            if let Some(u) = id_of(Some(a)) {
                out.push(item(Field::Assignee, None, None, Some(u), name_of(Some(a))));
            }
        }
        _ => {}
    }
    out
}

/// A change in the history as the import model keeps it (fields the reports and the history
/// show; others are left out).
fn history_items(field: Option<&str>, old: Option<&Value>, new: Option<&Value>) -> Vec<Item> {
    let named = |f: Field| item(f, id_of(old), name_of(old), id_of(new), name_of(new));
    let text = |v: Option<&Value>| match v {
        Some(Value::Null) | None => None,
        Some(Value::String(s)) => Some(s.clone()),
        Some(other) => Some(other.to_string()),
    };
    let minutes = |v: Option<&Value>| v.and_then(Value::as_f64).map(|m| (m * 60.0).to_string());
    let one = match field {
        Some("status") => named(Field::Status),
        Some("assignee") => named(Field::Assignee),
        Some("reporter") => named(Field::Reporter),
        Some("type") => named(Field::Type),
        Some("sprint") => named(Field::Sprint),
        Some("parent") => item(
            Field::Parent,
            id_of(old),
            old.and_then(|o| o["key"].as_str()).map(str::to_string),
            id_of(new),
            new.and_then(|n| n["key"].as_str()).map(str::to_string),
        ),
        Some("priority") => {
            let p = |v: Option<&Value>| v.and_then(Value::as_str).and_then(crate::query::parse_priority).map(|p| p.to_string());
            item(Field::Priority, p(old), text(old), p(new), text(new))
        }
        Some("title") => Item::new(Field::Title, text(old), text(new)),
        Some("description") => Item::new(Field::Description, None, None),
        Some("points") => Item::new(Field::Points, text(old), text(new)),
        Some("estimate") => Item::new(Field::Estimate, minutes(old), minutes(new)),
        Some("remaining") => Item::new(Field::Remaining, minutes(old), minutes(new)),
        Some("due") => Item::new(Field::Due, text(old), text(new)),
        Some("labels") => {
            let list = |v: Option<&Value>| {
                v.and_then(Value::as_array).map(|l| l.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" ")).filter(|s| !s.is_empty())
            };
            Item::new(Field::Labels, list(old), list(new))
        }
        _ => return vec![],
    };
    vec![one]
}

/// Writes the export of a project to a file in `dir` (removed by the caller).
pub async fn write(state: &AppState, project: Uuid, dir: &FsPath) -> anyhow::Result<PathBuf> {
    let (m, files) = model(state, project).await?;
    let key = m.projects[0].key.clone();
    tokio::fs::create_dir_all(dir).await?;
    let path = dir.join(format!("export-{project}-{}.zip", crate::app::new_id().simple()));
    let envelope = Envelope { format: FORMAT.into(), version: VERSION, exported_at: Utc::now(), project: key, model: m };
    let json = serde_json::to_vec(&envelope)?;
    let file = std::fs::File::create(&path)?;
    let mut zip = zip::ZipWriter::new(file);
    let deflate = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated).large_file(true);
    let stored = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored).large_file(true);
    zip.start_file(ENTRY, deflate)?;
    zip.write_all(&json)?;
    for (id, key) in files {
        let bytes = match state.blobs.get(&object_store::path::Path::from(key)).await {
            Ok(got) => got.bytes().await?,
            Err(e) => {
                tracing::warn!(attachment = %id, error = %e, "project export: file not in storage, left out");
                continue;
            }
        };
        zip.start_file(format!("files/{id}"), stored)?;
        zip.write_all(&bytes)?;
    }
    zip.finish()?;
    Ok(path)
}

/// `GET /projects/{key}/export`: the project as a `.zip` (workspace admins: it holds private
/// issues, restricted comments and people's e-mail addresses).
pub async fn download(State(state): State<AppState>, SessionUser(user): SessionUser, Path(key): Path<String>) -> ApiResult<Response> {
    require_admin(&user)?;
    let found: Option<(Uuid, String)> = sqlx::query_as(
        "SELECT p.id, p.key FROM projects p WHERE p.workspace_id = $1 AND (upper(p.key) = upper($2)
            OR p.id = (SELECT project_id FROM project_key_aliases WHERE workspace_id = $1 AND upper(key) = upper($2)))",
    )
    .bind(user.workspace_id)
    .bind(&key)
    .fetch_optional(&state.db)
    .await?;
    let (id, key) = found.ok_or(ApiError::NotFound("project"))?;
    let path = write(&state, id, &state.config.data_dir.join("tmp")).await.map_err(ApiError::Internal)?;
    let file = tokio::fs::File::open(&path).await.map_err(|e| ApiError::Internal(e.into()))?;
    let size = file.metadata().await.map(|m| m.len()).unwrap_or(0);
    // the open file stays readable after it's removed
    let _ = tokio::fs::remove_file(&path).await;
    audit(&state, user.workspace_id, Some(user.id), "project.exported", Some(key.clone()), json!({"size": size})).await;
    let stream = futures::stream::unfold(file, |mut f| async move {
        use tokio::io::AsyncReadExt;
        let mut buf = vec![0u8; 256 * 1024];
        match f.read(&mut buf).await {
            Ok(0) => None,
            Ok(n) => {
                buf.truncate(n);
                Some((Ok::<_, std::io::Error>(bytes::Bytes::from(buf)), f))
            }
            Err(e) => Some((Err(e), f)),
        }
    });
    let name = format!("{key}-{}.taskcraft.zip", Utc::now().format("%Y-%m-%d"));
    Ok((
        [
            (header::CONTENT_TYPE, "application/zip".to_string()),
            (header::CONTENT_LENGTH, size.to_string()),
            (header::CONTENT_DISPOSITION, format!("attachment; filename=\"{name}\"")),
        ],
        // fused: the body may be polled once more after its end
        Body::from_stream(futures::StreamExt::fuse(stream)),
    )
        .into_response())
}

/// Whether a zip is a project export.
pub fn is_export(path: &FsPath) -> bool {
    std::fs::File::open(path).ok().and_then(|f| zip::ZipArchive::new(f).ok()).is_some_and(|z| z.index_for_name(ENTRY).is_some())
}

/// An uploaded export: its model and its files.
pub struct Export {
    archive: zip::ZipArchive<std::fs::File>,
    files: HashMap<String, usize>,
}

impl Export {
    pub fn open(path: &FsPath) -> anyhow::Result<(Export, Model)> {
        let mut archive = zip::ZipArchive::new(std::fs::File::open(path)?)?;
        let envelope: Envelope = {
            let entry = archive.by_name(ENTRY).map_err(|_| anyhow::anyhow!("that's not a TaskCraft project export ({ENTRY} is missing)"))?;
            serde_json::from_reader(std::io::BufReader::new(entry)).map_err(|e| anyhow::anyhow!("{ENTRY} can't be read: {e}"))?
        };
        anyhow::ensure!(envelope.format == FORMAT, "that's not a TaskCraft project export");
        anyhow::ensure!(envelope.version <= VERSION, "this export comes from a newer TaskCraft (format {}): update this one first", envelope.version);
        let mut files = HashMap::new();
        for i in 0..archive.len() {
            if let Some(Ok(name)) = archive.name_for_index(i)
                && let Some(id) = name.strip_prefix("files/")
            {
                files.insert(id.to_string(), i);
            }
        }
        Ok((Export { archive, files }, envelope.model))
    }
}

impl Files for Export {
    fn has(&self, a: &Attachment) -> bool {
        self.files.contains_key(&a.id)
    }

    fn count(&self) -> u64 {
        self.files.len() as u64
    }

    fn size(&mut self, a: &Attachment) -> Option<u64> {
        let i = *self.files.get(&a.id)?;
        self.archive.by_index_raw(i).ok().map(|f| f.size())
    }

    fn read<'a>(&'a mut self, a: &'a Attachment, max: u64) -> BoxFuture<'a, anyhow::Result<Option<Download>>> {
        let got = (|| -> anyhow::Result<Option<Vec<u8>>> {
            let Some(i) = self.files.get(&a.id).copied() else { return Ok(None) };
            let entry = self.archive.by_index(i)?;
            if entry.size() > max {
                return Err(TooLarge.into());
            }
            let mut data = Vec::with_capacity(entry.size() as usize);
            entry.take(max + 1).read_to_end(&mut data)?;
            Ok(Some(data))
        })();
        Box::pin(async move { Ok(got?.map(|data| Download { data, mime: None, name: None })) })
    }
}
