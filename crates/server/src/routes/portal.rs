//! The service desk portal's API under `/api/portal` (public: requesters sign in with e-mailed
//! links, request forms take submissions from anyone) and a project's portal settings. See
//! `crate::portal`.

use axum::Json;
use axum::extract::{Multipart, Path, State};
use axum::http::{HeaderMap, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use uuid::Uuid;

use super::comments::{CommentOpts, add_with};
use super::projects::{ProjectRow, by_key};
use crate::access::Role;
use crate::app::{AppState, audit};
use crate::auth::CurrentUser;
use crate::error::{ApiError, ApiResult};
use crate::events::{self, Event};
use crate::issues::{self, Actor, NewIssue};
use crate::mail_in::{NewRequester, parse::text_to_doc};
use crate::portal::{self, ClientIp, MaybeRequester, Requester, Settings, Show, Stamp, csrf, form_path, plain_state};

type Time = chrono::DateTime<chrono::Utc>;

/// `GET /api/portal` — the workspace's name and colour, who is signed in, whether signing in
/// works (it needs e-mail) and the request forms on the start page.
pub async fn info(State(state): State<AppState>, MaybeRequester(email): MaybeRequester) -> ApiResult<Json<Value>> {
    let ws: Option<(String, Value)> = sqlx::query_as("SELECT name, settings FROM workspaces ORDER BY created_at LIMIT 1").fetch_optional(&state.db).await?;
    let (name, settings) = ws.unwrap_or_default();
    let projects: Vec<(String, String, Value)> = sqlx::query_as(
        "SELECT key, name, settings FROM projects WHERE archived_at IS NULL AND settings->'portal'->'form'->'enabled' = 'true'::jsonb ORDER BY lower(name)",
    )
    .fetch_all(&state.db)
    .await?;
    let forms: Vec<Value> = projects
        .iter()
        .filter_map(|(key, project, s)| {
            let form = Settings::of(s).form;
            form.listed.then(|| json!({"path": form_path(&form, key), "title": form.title, "project": project}))
        })
        .collect();
    Ok(Json(json!({
        "workspace": {"name": name, "accentColor": settings.get("accentColor")},
        "email": email,
        "signIn": state.mailer.enabled(),
        "forms": forms,
    })))
}

#[derive(Deserialize)]
pub struct SignInReq {
    email: String,
    /// the request (key) the link should lead to
    #[serde(default)]
    request: Option<String>,
}

/// `POST /api/portal/sign-in` — e-mails a sign-in link. Answers 204 whether or not the address
/// has requests (the e-mail goes out in the background, only to addresses with requests).
pub async fn sign_in(
    State(state): State<AppState>,
    method: Method,
    headers: HeaderMap,
    ClientIp(ip): ClientIp,
    Json(req): Json<SignInReq>,
) -> ApiResult<StatusCode> {
    csrf(&method, &headers)?;
    if !state.mailer.enabled() {
        return Err(ApiError::bad("signing in needs e-mail, which isn't set up here"));
    }
    let email = super::auth::validate_email(&req.email)?;
    if !portal::attempt(&state, "sign-in-ip", &ip, portal::SIGN_INS_PER_IP, 15 * 60).await? {
        return Err(ApiError::TooMany("too many sign-in links were asked for from here: try again in a quarter of an hour".into()));
    }
    let st = state.clone();
    tokio::spawn(async move {
        if let Err(e) = portal::send_link(&st, &email, req.request.as_deref()).await {
            tracing::warn!("portal sign-in link: {e:#}");
        }
    });
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct SessionReq {
    token: String,
}

/// `POST /api/portal/session` — signs in with a link's token (once).
pub async fn start_session(State(state): State<AppState>, method: Method, headers: HeaderMap, Json(req): Json<SessionReq>) -> ApiResult<Response> {
    csrf(&method, &headers)?;
    let email = portal::redeem(&state, req.token.trim()).await?.ok_or_else(|| ApiError::bad("this sign-in link has expired or was already used"))?;
    let cookie = portal::start_session(&state, &email).await?;
    Ok(([(header::SET_COOKIE, cookie)], Json(json!({"email": email}))).into_response())
}

/// `DELETE /api/portal/session` — signs out.
pub async fn end_session(State(state): State<AppState>, method: Method, headers: HeaderMap) -> ApiResult<Response> {
    csrf(&method, &headers)?;
    portal::end_session(&state, &headers).await?;
    Ok(([(header::SET_COOKIE, portal::cookie("", 0, state.config.secure_cookies))], StatusCode::NO_CONTENT).into_response())
}

/// When a request last changed in ways its requester sees.
const SEEN_UPDATE: &str = "GREATEST(i.created_at, i.status_changed_at, (SELECT max(c.created_at) FROM comments c WHERE c.issue_id = i.id AND c.for_requester))";

#[derive(sqlx::FromRow)]
struct Listed {
    key: String,
    title: String,
    project: String,
    category: String,
    created_at: Time,
    updated_at: Time,
    resolved_at: Option<Time>,
}

/// `GET /api/portal/requests` — the signed-in requester's requests, latest first.
pub async fn requests(State(state): State<AppState>, Requester(email): Requester) -> ApiResult<Json<Value>> {
    let rows: Vec<Listed> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT p.key || '-' || i.number AS key, i.title, p.name AS project, s.category, i.created_at, {SEEN_UPDATE} AS updated_at, i.resolved_at
           FROM issue_requesters r JOIN issues i ON i.id = r.issue_id JOIN projects p ON p.id = i.project_id JOIN statuses s ON s.id = i.status_id
          WHERE r.email = $1 AND i.deleted_at IS NULL
          ORDER BY updated_at DESC LIMIT 500"
    )))
    .bind(&email)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(json!({
        "email": email,
        "requests": rows.iter().map(|r| json!({
            "key": r.key, "title": r.title, "project": r.project, "state": plain_state(&r.category),
            "createdAt": r.created_at, "updatedAt": r.updated_at, "resolvedAt": r.resolved_at,
        })).collect::<Vec<_>>(),
    })))
}

/// A request of the signed-in requester.
#[derive(sqlx::FromRow)]
struct Found {
    id: Uuid,
    project_id: Uuid,
    workspace_id: Uuid,
    key: String,
    title: String,
    project_name: String,
    category: String,
    archived: bool,
    settings: Value,
    name: Option<String>,
    description: Option<Value>,
    created_at: Time,
    updated_at: Time,
    resolved_at: Option<Time>,
}

/// The request `key` (also a former key) if `email` is its requester; else not found, as for
/// requests that don't exist.
async fn find(state: &AppState, email: &str, key: &str) -> ApiResult<Found> {
    let (k, n) = crate::query::split_key(key).ok_or(ApiError::NotFound("request"))?;
    sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT i.id, i.project_id, p.workspace_id, p.key || '-' || i.number AS key, i.title, p.name AS project_name, s.category,
                p.archived_at IS NOT NULL AS archived, p.settings, r.name, i.description, i.created_at, {SEEN_UPDATE} AS updated_at, i.resolved_at
           FROM issues i JOIN projects p ON p.id = i.project_id JOIN statuses s ON s.id = i.status_id JOIN issue_requesters r ON r.issue_id = i.id
          WHERE r.email = $3 AND i.deleted_at IS NULL
            AND ((i.number = $2 AND (upper(p.key) = $1 OR p.id IN (SELECT project_id FROM project_key_aliases WHERE upper(key) = $1)))
                 OR i.id IN (SELECT issue_id FROM issue_key_aliases WHERE upper(project_key) = $1 AND number = $2))
          ORDER BY upper(p.key) = $1 DESC LIMIT 1"
    )))
    .bind(&k)
    .bind(n)
    .bind(email)
    .fetch_optional(&state.db)
    .await?
    .ok_or(ApiError::NotFound("request"))
}

/// Size of the files of one answer, together.
fn files_limit(state: &AppState) -> u64 {
    portal::MAX_FILES_MB.min(state.config.max_upload_mb).max(1)
}

/// `GET /api/portal/requests/{key}` — a request as its requester sees it: the description, the
/// conversation (their messages and the comments marked for them) and their files.
pub async fn request(State(state): State<AppState>, Requester(email): Requester, Path(key): Path<String>) -> ApiResult<Json<Value>> {
    let f = find(&state, &email, &key).await?;
    #[derive(sqlx::FromRow)]
    struct Message {
        id: Uuid,
        author_id: Option<Uuid>,
        author: Option<String>,
        author_email: Option<String>,
        body: Value,
        created_at: Time,
        edited: bool,
    }
    let messages: Vec<Message> = sqlx::query_as(
        "SELECT c.id, c.author_id, u.name AS author, c.author_email, c.body, c.created_at, c.edited
           FROM comments c LEFT JOIN users u ON u.id = c.author_id WHERE c.issue_id = $1 AND c.for_requester ORDER BY c.created_at",
    )
    .bind(f.id)
    .fetch_all(&state.db)
    .await?;
    let files: Vec<(Uuid, String, String, i64, Time)> =
        sqlx::query_as("SELECT id, name, mime, size, created_at FROM attachments WHERE issue_id = $1 AND by_requester ORDER BY created_at")
            .bind(f.id)
            .fetch_all(&state.db)
            .await?;
    let state_word = plain_state(&f.category);
    let settings = Settings::of(&f.settings);
    Ok(Json(json!({
        "key": f.key, "title": f.title, "project": f.project_name, "state": state_word,
        "createdAt": f.created_at, "updatedAt": f.updated_at, "resolvedAt": f.resolved_at,
        "description": f.description.as_ref().map(portal::public_doc),
        "messages": messages.iter().map(|m| {
            let mine = m.author_id.is_none() && m.author_email.as_deref() == Some(email.as_str());
            json!({
                "id": m.id, "mine": mine, "author": if mine { None } else { m.author.clone() },
                "body": portal::public_doc(&m.body), "createdAt": m.created_at, "edited": m.edited,
            })
        }).collect::<Vec<_>>(),
        "files": files.iter().map(|(id, name, mime, size, at)| json!({
            "id": id, "name": name, "mime": mime, "size": size, "createdAt": at, "url": format!("/api/portal/attachments/{id}"),
        })).collect::<Vec<_>>(),
        "canReply": !f.archived,
        "canResolve": settings.resolve && !f.archived && state_word != "resolved",
        "limits": {"files": portal::MAX_FILES, "mb": files_limit(&state), "text": portal::MAX_TEXT},
    })))
}

fn bad_upload(e: axum::extract::multipart::MultipartError) -> ApiError {
    if e.status() == StatusCode::PAYLOAD_TOO_LARGE { ApiError::TooLarge } else { ApiError::bad(format!("upload: {}", e.body_text())) }
}

/// `POST /api/portal/requests/{key}/comments` — the requester answers (multipart: `text` and
/// up to a few `files`): a comment by them, like an answer by e-mail.
pub async fn reply(
    State(state): State<AppState>,
    Requester(email): Requester,
    Path(key): Path<String>,
    mut mp: Multipart,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let f = find(&state, &email, &key).await?;
    if f.archived {
        return Err(ApiError::bad("this request is closed: it can't be answered any more"));
    }
    let recent: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM comments WHERE author_email = $1 AND author_id IS NULL AND via_portal AND created_at > now() - interval '1 hour'",
    )
    .bind(&email)
    .fetch_one(&state.db)
    .await?;
    if recent >= portal::REPLIES_PER_HOUR {
        return Err(ApiError::TooMany("too many messages in the last hour: try again later".into()));
    }
    let max = files_limit(&state) * 1024 * 1024;
    let mut text = String::new();
    let mut files: Vec<(String, String, Vec<u8>)> = Vec::new();
    let mut total: u64 = 0;
    while let Some(mut field) = mp.next_field().await.map_err(bad_upload)? {
        match field.name() {
            Some("text") => {
                text = field.text().await.map_err(bad_upload)?;
                if text.chars().count() > portal::MAX_TEXT {
                    return Err(ApiError::bad("the message is too long"));
                }
            }
            Some("files") => {
                let Some(name) = field.file_name().map(|n| n.replace(['/', '\\', '\0'], "_")).filter(|n| !n.trim().is_empty()) else { continue };
                if files.len() >= portal::MAX_FILES {
                    return Err(ApiError::bad(format!("at most {} files at a time", portal::MAX_FILES)));
                }
                let name: String = name.chars().take(200).collect();
                let mime = field.content_type().map(str::to_string).unwrap_or_else(|| mime_guess::from_path(&name).first_or_octet_stream().to_string());
                let mut data = Vec::new();
                while let Some(chunk) = field.chunk().await.map_err(bad_upload)? {
                    total += chunk.len() as u64;
                    if total > max {
                        return Err(ApiError::TooLarge);
                    }
                    data.extend_from_slice(&chunk);
                }
                files.push((name, mime, data));
            }
            _ => {}
        }
    }
    let text = text.replace("\r\n", "\n").trim().to_string();
    if text.is_empty() && files.is_empty() {
        return Err(ApiError::bad("write a message or add a file"));
    }
    let by = f.name.clone().unwrap_or_else(|| email.clone());
    let names: Vec<String> = files.iter().map(|(n, _, _)| n.clone()).collect();
    for (name, mime, data) in files {
        super::attachments::store(&state, f.id, f.project_id, None, Some(&by), &name, &mime, data).await?;
    }
    let mut body = text;
    if !names.is_empty() {
        let line = format!("📎 {}", names.join(" · "));
        body = if body.is_empty() { line } else { format!("{body}\n\n{line}") };
    }
    let actor = Actor { user: None, workspace: f.workspace_id, depth: 0 };
    let opts = CommentOpts { via_portal: true, for_requester: true, external: Some((email.clone(), f.name.clone())), ..Default::default() };
    let id = add_with(&state, &actor, f.id, text_to_doc(&body), &opts).await?;
    if !names.is_empty() {
        events::publish(&state, Event::new(f.workspace_id, "issue").project(f.project_id).issue(f.id, &f.key)).await;
    }
    Ok((StatusCode::CREATED, Json(json!({"id": id}))))
}

#[derive(Deserialize, Default)]
pub struct ResolveReq {
    /// a last word (a comment by them)
    #[serde(default)]
    text: Option<String>,
}

/// `POST /api/portal/requests/{key}/resolve` — the requester marks their request resolved, where
/// the project lets them: it moves to a done status, in their name.
pub async fn resolve(
    State(state): State<AppState>,
    Requester(email): Requester,
    Path(key): Path<String>,
    Json(req): Json<ResolveReq>,
) -> ApiResult<Json<Value>> {
    let f = find(&state, &email, &key).await?;
    let settings = Settings::of(&f.settings);
    if !settings.resolve || f.archived {
        return Err(ApiError::Denied("requests here are closed by the team".into()));
    }
    if f.category == "done" {
        return Ok(Json(json!({"state": "resolved"})));
    }
    let status: Uuid =
        sqlx::query_scalar("SELECT id FROM statuses WHERE project_id = $1 AND category = 'done' ORDER BY id = $2 DESC NULLS LAST, position LIMIT 1")
            .bind(f.project_id)
            .bind(settings.resolve_status)
            .fetch_optional(&state.db)
            .await?
            .ok_or_else(|| ApiError::bad("this project has no status for resolved requests"))?;
    let by = f.name.clone().unwrap_or_else(|| email.clone());
    let actor = Actor { user: None, workspace: f.workspace_id, depth: 0 };
    let text = req.text.as_deref().map(|t| t.replace("\r\n", "\n").trim().to_string()).unwrap_or_default();
    if text.chars().count() > portal::MAX_TEXT {
        return Err(ApiError::bad("the message is too long"));
    }
    if !text.is_empty() {
        let opts = CommentOpts { via_portal: true, for_requester: true, external: Some((email.clone(), f.name.clone())), ..Default::default() };
        add_with(&state, &actor, f.id, text_to_doc(&text), &opts).await?;
    }
    let mut patch = Map::new();
    patch.insert("statusId".into(), json!(status));
    let mut tx = state.db.begin().await?;
    let mut changes = issues::apply(&state, &mut tx, &actor, f.id, f.project_id, &patch, false).await?;
    // the history names the requester (there's no person to point to)
    sqlx::query(
        "UPDATE issue_events SET new_value = new_value || jsonb_build_object('by', $2::text)
          WHERE id = (SELECT id FROM issue_events WHERE issue_id = $1 AND field = 'status' ORDER BY id DESC LIMIT 1)",
    )
    .bind(f.id)
    .bind(&by)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    for c in &mut changes {
        if let Some(o) = c.new.as_object_mut() {
            o.insert("by".into(), json!(by));
        }
    }
    let s = issues::summary(&state, issues::load(&state, f.id).await?);
    issues::after_change(&state, &actor, &s, &changes, false).await;
    Ok(Json(json!({"state": plain_state(&s.status.category)})))
}

/// `GET /api/portal/attachments/{id}` — one of the requester's own files.
pub async fn download(State(state): State<AppState>, Requester(email): Requester, Path(id): Path<Uuid>) -> ApiResult<Response> {
    let row: Option<(String, String, String, i64)> = sqlx::query_as(
        "SELECT a.name, a.mime, a.storage_key, a.size FROM attachments a JOIN issues i ON i.id = a.issue_id JOIN issue_requesters r ON r.issue_id = a.issue_id
          WHERE a.id = $1 AND a.by_requester AND r.email = $2 AND i.deleted_at IS NULL",
    )
    .bind(id)
    .bind(&email)
    .fetch_optional(&state.db)
    .await?;
    let (name, mime, key, size) = row.ok_or(ApiError::NotFound("file"))?;
    super::attachments::serve(&state, &name, mime, key, size).await
}

// ---- request forms ---------------------------------------------------------------------------

#[derive(sqlx::FromRow)]
struct FormProject {
    id: Uuid,
    workspace_id: Uuid,
    key: String,
    name: String,
    settings: Value,
}

/// The project whose form lives at `path` (its slug, key or a former key), if the form is on.
async fn form_project(state: &AppState, path: &str) -> ApiResult<(FormProject, portal::Form)> {
    let path = path.trim().to_lowercase();
    if path.is_empty() || path.len() > 60 {
        return Err(ApiError::NotFound("form"));
    }
    let p: FormProject = sqlx::query_as(
        "SELECT p.id, p.workspace_id, p.key, p.name, p.settings FROM projects p
          WHERE p.archived_at IS NULL AND p.settings->'portal'->'form'->'enabled' = 'true'::jsonb
            AND (lower(p.settings->'portal'->'form'->>'slug') = $1 OR lower(p.key) = $1
                 OR p.id IN (SELECT project_id FROM project_key_aliases WHERE lower(key) = $1))
          ORDER BY COALESCE(lower(p.settings->'portal'->'form'->>'slug') = $1, false) DESC, lower(p.key) = $1 DESC LIMIT 1",
    )
    .bind(&path)
    .fetch_optional(&state.db)
    .await?
    .ok_or(ApiError::NotFound("form"))?;
    let form = Settings::of(&p.settings).form;
    if !form.enabled {
        return Err(ApiError::NotFound("form"));
    }
    Ok((p, form))
}

/// The custom fields a form shows (this project's, not people), in the form's order.
async fn form_fields(state: &AppState, project: Uuid, form: &portal::Form) -> ApiResult<Vec<(Uuid, String, String, Value, Option<String>, bool)>> {
    let ids: Vec<Uuid> = form.custom.iter().map(|f| f.id).collect();
    let rows: Vec<(Uuid, String, String, Value, Option<String>)> =
        sqlx::query_as("SELECT id, name, kind, options, description FROM custom_fields WHERE project_id = $1 AND id = ANY($2) AND kind <> 'user'")
            .bind(project)
            .bind(&ids)
            .fetch_all(&state.db)
            .await?;
    Ok(form
        .custom
        .iter()
        .filter_map(|c| {
            rows.iter()
                .find(|r| r.0 == c.id)
                .map(|(id, name, kind, options, description)| (*id, name.clone(), kind.clone(), options.clone(), description.clone(), c.required))
        })
        .collect())
}

/// `GET /api/portal/forms/{path}` — a public request form, with a stamp of when it was opened.
pub async fn form(State(state): State<AppState>, Path(path): Path<String>) -> ApiResult<Json<Value>> {
    let (p, form) = form_project(&state, &path).await?;
    let types: Vec<(Uuid, String)> = if form.types.len() > 1 {
        sqlx::query_as("SELECT id, name FROM issue_types WHERE project_id = $1 AND id = ANY($2) AND level = 0 ORDER BY position")
            .bind(p.id)
            .bind(&form.types)
            .fetch_all(&state.db)
            .await?
    } else {
        Vec::new()
    };
    let fields = form_fields(&state, p.id, &form).await?;
    let stamp = portal::form_stamp(&state, p.id, chrono::Utc::now().timestamp()).await?;
    Ok(Json(json!({
        "path": form_path(&form, &p.key),
        "project": p.name,
        "title": form.title, "intro": form.intro, "success": form.success,
        "description": form.description, "name": form.name,
        "types": types.iter().map(|(id, name)| json!({"id": id, "name": name})).collect::<Vec<_>>(),
        "priority": form.priority,
        "fields": fields.iter().map(|(id, name, kind, options, description, required)| json!({
            "id": id, "name": name, "kind": kind, "description": description, "required": required,
            "options": options.as_array().into_iter().flatten().map(|o| json!({"id": o["id"], "label": o["label"]})).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
        "stamp": stamp,
        "minSeconds": portal::MIN_FILL_SECS,
        "suggest": form.suggest_space.is_some() && state.config.wikicraft_url.is_some() && state.config.wikicraft_token.is_some(),
    })))
}

#[derive(Deserialize)]
pub struct SuggestQuery {
    #[serde(default)]
    q: String,
}

/// Pages of the form's WikiCraft space matching what a requester types (at most five), read with
/// the installation's WikiCraft token. Nothing when the form suggests nothing, the text is short
/// or someone asks too often.
pub async fn suggest(
    State(state): State<AppState>,
    ClientIp(ip): ClientIp,
    Path(path): Path<String>,
    axum::extract::Query(q): axum::extract::Query<SuggestQuery>,
) -> ApiResult<Json<Value>> {
    let (_, form) = form_project(&state, &path).await?;
    let text: String = q.q.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(200).collect();
    let (Some(space), Some(base), Some(token)) = (form.suggest_space, crate::wikicraft::base_url(&state), state.config.wikicraft_token.clone()) else {
        return Ok(Json(json!([])));
    };
    if text.chars().count() < 3 || !portal::attempt(&state, "suggest", &ip, 60, 60).await? {
        return Ok(Json(json!([])));
    }
    let c = crate::wikicraft::Conn { base, token, personal: false };
    let pages = match crate::wikicraft::search(&state, &c, &text, Some(&space)).await {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(error = %e, "portal suggestions from WikiCraft failed");
            Vec::new()
        }
    };
    Ok(Json(json!(
        pages
            .into_iter()
            // that space only, whatever the search returns
            .filter(|p| p.space_key.eq_ignore_ascii_case(&space))
            .take(5)
            .map(|p| json!({"title": p.title, "url": p.url, "excerpt": p.excerpt}))
            .collect::<Vec<_>>()
    )))
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct Submission {
    /// from the form: when it was opened, signed
    stamp: String,
    email: String,
    name: String,
    summary: String,
    description: String,
    type_id: Option<String>,
    priority: Option<String>,
    custom: Map<String, Value>,
    /// the hidden field: people leave it empty
    website: String,
}

fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn is_empty_value(v: &Value) -> bool {
    match v {
        Value::Null | Value::Bool(false) => true,
        Value::String(s) => s.trim().is_empty(),
        Value::Array(a) => a.is_empty(),
        _ => false,
    }
}

/// `POST /api/portal/forms/{path}` — a submission: an issue with the submitter as its requester
/// (signed-in requesters: their portal address), who gets an acknowledgement.
pub async fn submit(
    State(state): State<AppState>,
    method: Method,
    headers: HeaderMap,
    ClientIp(ip): ClientIp,
    MaybeRequester(signed_in): MaybeRequester,
    Path(path): Path<String>,
    Json(req): Json<Submission>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    csrf(&method, &headers)?;
    let (p, form) = form_project(&state, &path).await?;
    if !req.website.trim().is_empty() {
        // a bot filled the hidden field: as far as it can tell, that worked
        tracing::info!(project = %p.key, ip = %ip, "request form: hidden field filled, ignored");
        return Ok((StatusCode::CREATED, Json(json!({"ok": true, "request": null}))));
    }
    match portal::check_stamp(&state, p.id, &req.stamp).await? {
        Stamp::Ok => {}
        Stamp::TooFast => return Err(ApiError::bad("that was quick: please check the form and send it again")),
        Stamp::Invalid => return Err(ApiError::bad("this form has expired: reload the page and send it again")),
    }
    let (email, source) = match &signed_in {
        Some(e) => (e.clone(), "portal"),
        None => (super::auth::validate_email(&req.email)?, "form"),
    };
    let summary = one_line(&req.summary);
    if summary.is_empty() {
        return Err(ApiError::bad("a summary is required"));
    }
    if summary.chars().count() > portal::MAX_SUMMARY {
        return Err(ApiError::bad(format!("the summary is longer than {} characters", portal::MAX_SUMMARY)));
    }
    let description = if form.description == Show::Off { String::new() } else { req.description.replace("\r\n", "\n").trim().to_string() };
    if form.description == Show::Required && description.is_empty() {
        return Err(ApiError::bad("a description is required"));
    }
    if description.chars().count() > portal::MAX_TEXT {
        return Err(ApiError::bad("the description is too long"));
    }
    let name = if form.name == Show::Off { String::new() } else { one_line(&req.name) };
    if form.name == Show::Required && name.is_empty() {
        return Err(ApiError::bad("your name is required"));
    }
    if name.chars().count() > portal::MAX_NAME {
        return Err(ApiError::bad("the name is too long"));
    }
    let mut fields = Map::new();
    let type_id = match form.types.as_slice() {
        [] => form.type_id,
        [only] => Some(*only),
        offered => Some(
            req.type_id
                .as_deref()
                .and_then(|t| t.parse::<Uuid>().ok())
                .filter(|t| offered.contains(t))
                .ok_or_else(|| ApiError::bad("choose one of the request types"))?,
        ),
    };
    if let Some(t) = type_id {
        fields.insert("typeId".into(), json!(t));
    }
    if form.priority
        && let Some(pr) = req.priority.as_deref().filter(|p| !p.is_empty())
    {
        if !matches!(pr, "low" | "medium" | "high" | "urgent") {
            return Err(ApiError::bad("priority must be low, medium, high or urgent"));
        }
        fields.insert("priority".into(), json!(pr));
    }
    let shown = form_fields(&state, p.id, &form).await?;
    let mut custom = Map::new();
    for (id, v) in &req.custom {
        let Some(f) = shown.iter().find(|f| f.0.to_string() == *id) else {
            return Err(ApiError::bad("the form has no such field"));
        };
        if is_empty_value(v) {
            continue;
        }
        if v.to_string().len() > 4000 {
            return Err(ApiError::bad(format!("{} is too long", f.1)));
        }
        custom.insert(id.clone(), v.clone());
    }
    if let Some(f) = shown.iter().find(|f| f.5 && !custom.contains_key(&f.0.to_string())) {
        return Err(ApiError::bad(format!("{} is required", f.1)));
    }
    if !custom.is_empty() {
        fields.insert("custom".into(), Value::Object(custom));
    }
    // requests that were opened count (not ones refused for a mistake)
    if !portal::allowed(&state, "form-ip", &ip, portal::FORMS_PER_IP, 3600).await? {
        return Err(ApiError::TooMany("too many requests were sent from here: try again later".into()));
    }
    if !portal::allowed(&state, "form-email", &email, portal::FORMS_PER_EMAIL, 3600).await? {
        return Err(ApiError::TooMany("too many requests were sent for this e-mail address: try again later".into()));
    }
    let actor = Actor { user: None, workspace: p.workspace_id, depth: 0 };
    let description = if description.is_empty() { crate::doc::empty() } else { text_to_doc(&description) };
    let s = issues::create(&state, &actor, NewIssue { project: p.id, title: summary, description: Some(description), fields }).await?;
    portal::note(&state, "form-ip", &ip).await?;
    portal::note(&state, "form-email", &email).await?;
    // the issue exists: from here on, trouble is logged rather than reported (sending the form
    // again would make a second issue)
    let name = (!name.is_empty()).then_some(name.as_str());
    let reply_to = crate::mail_in::reply_address(&state, p.id).await;
    let r = NewRequester { email: &email, name, reply_to: reply_to.as_deref(), message_id: None, source };
    if let Err(e) = crate::mail_in::add_requester(&state, &s, &r).await {
        tracing::warn!(issue = %s.key, "request form: noting the requester failed: {e:#}");
    }
    if let Err(e) = crate::mail_in::acknowledge(&state, &s, &p.name, &email, name, reply_to.as_deref(), None).await {
        tracing::warn!(issue = %s.key, "request form: acknowledgement: {e:#}");
    }
    tracing::info!(issue = %s.key, source, "request form");
    // only a signed-in requester (their address is theirs) learns the key to open it
    Ok((StatusCode::CREATED, Json(json!({"ok": true, "request": signed_in.is_some().then_some(s.key)}))))
}

// ---- a project's portal settings ---------------------------------------------------------------

fn overview(state: &AppState, p: &ProjectRow) -> Value {
    let s = Settings::of(&p.settings);
    json!({
        "formUrl": format!("{}/portal/forms/{}", state.config.base(), form_path(&s.form, &p.key)),
        "portalUrl": format!("{}/portal", state.config.base()),
        "mail": state.mailer.enabled(),
        "settings": s,
    })
}

/// `GET /api/projects/{key}/portal` (project admins).
pub async fn settings(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(key): Path<String>) -> ApiResult<Json<Value>> {
    let (p, _) = by_key(&state, &user, &key, Role::Admin).await?;
    Ok(Json(overview(&state, &p)))
}

fn clip(s: &str, max: usize, what: &str) -> ApiResult<String> {
    let s = s.trim();
    if s.chars().count() > max {
        return Err(ApiError::bad(format!("the {what} is longer than {max} characters")));
    }
    Ok(s.to_string())
}

/// `PUT /api/projects/{key}/portal` — whether requesters may resolve their requests, and the
/// request form (`crate::portal::Settings`).
pub async fn put_settings(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Path(key): Path<String>,
    Json(req): Json<Settings>,
) -> ApiResult<Json<Value>> {
    let (p, _) = by_key(&state, &user, &key, Role::Admin).await?;
    let mut s = req;
    s.form.title = clip(&s.form.title, 200, "title")?;
    s.form.intro = clip(&s.form.intro, 5000, "introduction")?;
    s.form.success = clip(&s.form.success, 2000, "message after sending")?;
    s.form.suggest_space = s.form.suggest_space.as_deref().map(str::trim).filter(|x| !x.is_empty()).map(|x| x.chars().take(60).collect());
    s.form.slug = match s.form.slug.as_deref().map(|x| x.trim().to_lowercase()).filter(|x| !x.is_empty()) {
        None => None,
        Some(slug) => {
            let ok = (2..=40).contains(&slug.len())
                && slug.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
                && !slug.starts_with('-')
                && !slug.ends_with('-');
            if !ok {
                return Err(ApiError::bad("the form's address takes 2 to 40 letters (a–z), digits and dashes"));
            }
            let taken: Option<String> =
                sqlx::query_scalar("SELECT key FROM projects WHERE id <> $1 AND (lower(key) = $2 OR lower(settings->'portal'->'form'->>'slug') = $2) LIMIT 1")
                    .bind(p.id)
                    .bind(&slug)
                    .fetch_optional(&state.db)
                    .await?;
            if let Some(k) = taken {
                return Err(ApiError::Conflict(format!("{slug} is already the address of {k}'s form")));
            }
            Some(slug)
        }
    };
    let standard: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM issue_types WHERE project_id = $1 AND level = 0").bind(p.id).fetch_all(&state.db).await?;
    if s.form.types.iter().chain(s.form.type_id.iter()).any(|t| !standard.contains(t)) {
        return Err(ApiError::bad("forms offer this project's standard issue types (not epics or sub-tasks)"));
    }
    let mut seen_types = Vec::new();
    s.form.types.retain(|t| {
        let new = !seen_types.contains(t);
        seen_types.push(*t);
        new
    });
    let fields: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM custom_fields WHERE project_id = $1 AND kind <> 'user'").bind(p.id).fetch_all(&state.db).await?;
    if s.form.custom.iter().any(|f| !fields.contains(&f.id)) {
        return Err(ApiError::bad("forms show this project's own fields, except people fields"));
    }
    let mut seen = Vec::new();
    s.form.custom.retain(|f| {
        let new = !seen.contains(&f.id);
        seen.push(f.id);
        new
    });
    if let Some(st) = s.resolve_status {
        let done: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM statuses WHERE id = $1 AND project_id = $2 AND category = 'done')")
            .bind(st)
            .bind(p.id)
            .fetch_one(&state.db)
            .await?;
        if !done {
            return Err(ApiError::bad("resolved requests go to a done status of this project"));
        }
    }
    sqlx::query("UPDATE projects SET settings = jsonb_set(settings, '{portal}', $2), updated_at = now() WHERE id = $1")
        .bind(p.id)
        .bind(serde_json::to_value(&s)?)
        .execute(&state.db)
        .await?;
    audit(
        &state,
        user.workspace_id,
        Some(user.id),
        "project.portal",
        Some(p.key.clone()),
        json!({"form": s.form.enabled, "slug": s.form.slug, "resolve": s.resolve}),
    )
    .await;
    events::publish(&state, Event::new(user.workspace_id, "project").project(p.id).actor(user.id)).await;
    let (p, _) = by_key(&state, &user, &p.id.to_string(), Role::Admin).await?;
    Ok(Json(overview(&state, &p)))
}
