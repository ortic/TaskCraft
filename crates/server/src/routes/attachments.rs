//! Files attached to issues (local storage or S3).

use axum::Json;
use axum::body::Body;
use axum::extract::{Multipart, Path, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use futures::StreamExt;
use object_store::ObjectStoreExt;
use object_store::path::Path as ObjPath;
use serde_json::{Value, json};
use uuid::Uuid;

use super::issues::issue_for;
use crate::access::Role;
use crate::app::{AppState, new_id};
use crate::auth::CurrentUser;
use crate::error::{ApiError, ApiResult};
use crate::events::{self, Event};
use crate::issues::{self, Change};

pub async fn upload(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Path(key): Path<String>,
    mut mp: Multipart,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let (issue, project, _) = issue_for(&state, &user, &key, Role::Member).await?;
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
    // all or none: blocked types, the workspace's size limit, a virus scan
    for (name, _, data) in &files {
        crate::files::check(&state, user.workspace_id, name, data).await?;
    }
    let mut out = Vec::new();
    for (name, mime, data) in files {
        out.push(store(&state, issue, project, Some(user.id), None, &name, &mime, data).await?);
    }
    events::publish(&state, Event::new(user.workspace_id, "issue").project(project).actor(user.id)).await;
    Ok((StatusCode::CREATED, Json(json!(out))))
}

/// Stores a file on an issue (the caller checked the permission, the size and `files::check`) with a history
/// entry; `by` names an uploader without an account (the issue's requester, writing by e-mail,
/// on the request form or in the portal): their files are the ones they see in the portal.
pub async fn store(
    state: &AppState,
    issue: Uuid,
    project: Uuid,
    uploader: Option<Uuid>,
    by: Option<&str>,
    name: &str,
    mime: &str,
    data: Vec<u8>,
) -> ApiResult<Value> {
    let id = new_id();
    let storage_key = format!("attachments/{issue}/{id}");
    let size = data.len() as i64;
    state.blobs.put(&ObjPath::from(storage_key.clone()), data.into()).await.map_err(|e| ApiError::Internal(anyhow::anyhow!("storage: {e}")))?;
    let mut tx = state.db.begin().await?;
    sqlx::query("INSERT INTO attachments (id, issue_id, name, mime, size, storage_key, uploaded_by, by_requester) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)")
        .bind(id)
        .bind(issue)
        .bind(name)
        .bind(mime)
        .bind(size)
        .bind(&storage_key)
        .bind(uploader)
        .bind(uploader.is_none() && by.is_some())
        .execute(&mut *tx)
        .await?;
    let mut new = json!({"name": name});
    if let Some(by) = by {
        new["by"] = json!(by);
    }
    issues::record(&mut tx, issue, project, uploader, "attachment", Some(&Change { field: "attachment", old: Value::Null, new })).await?;
    sqlx::query("UPDATE issues SET updated_at = now() WHERE id = $1").bind(issue).execute(&mut *tx).await?;
    tx.commit().await?;
    // its text, for search
    crate::extract::index_soon(state, id);
    Ok(json!({"id": id, "name": name, "mime": mime, "size": size, "url": format!("/api/attachments/{id}")}))
}

async fn attachment(state: &AppState, id: Uuid) -> ApiResult<(Uuid, String, String, String, i64)> {
    sqlx::query_as("SELECT issue_id, name, mime, storage_key, size FROM attachments WHERE id = $1")
        .bind(id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(ApiError::NotFound("attachment"))
}

/// Images, PDFs, video and plain text show in the browser; everything else downloads. Files
/// never run as part of TaskCraft (sandboxed, no sniffing).
pub async fn download(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(id): Path<Uuid>) -> ApiResult<Response> {
    let (issue, name, mime, key, size) = attachment(&state, id).await?;
    issue_for(&state, &user, &issue.to_string(), Role::Viewer).await?;
    serve(&state, &name, mime, key, size).await
}

/// Streams a stored file (the caller checked who may see it).
pub async fn serve(state: &AppState, name: &str, mime: String, key: String, size: i64) -> ApiResult<Response> {
    let got = state.blobs.get(&ObjPath::from(key)).await.map_err(|_| ApiError::NotFound("file"))?;
    let stream = got.into_stream().map(|r| r.map_err(std::io::Error::other));
    let inline = (mime.starts_with("image/") && mime != "image/svg+xml") || mime == "application/pdf" || mime.starts_with("video/") || mime == "text/plain";
    let ascii: String = name.chars().map(|c| if c.is_ascii_alphanumeric() || ".-_ ".contains(c) { c } else { '_' }).collect();
    let disposition = format!(
        "{}; filename=\"{ascii}\"; filename*=UTF-8''{}",
        if inline { "inline" } else { "attachment" },
        name.bytes().map(|b| if b.is_ascii_alphanumeric() || b".-_".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect::<String>()
    );
    Ok((
        [
            (header::CONTENT_TYPE, if inline { mime } else { "application/octet-stream".into() }),
            (header::CONTENT_DISPOSITION, disposition),
            (header::CONTENT_LENGTH, size.to_string()),
            (header::CONTENT_SECURITY_POLICY, "sandbox; default-src 'none'; img-src 'self'; media-src 'self'; style-src 'unsafe-inline'".into()),
            (header::CACHE_CONTROL, "private, max-age=3600".into()),
        ],
        Body::from_stream(stream),
    )
        .into_response())
}

pub async fn delete(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    let (issue, name, _, key, _) = attachment(&state, id).await?;
    let (_, project, _) = issue_for(&state, &user, &issue.to_string(), Role::Member).await?;
    let mut tx = state.db.begin().await?;
    sqlx::query("DELETE FROM attachments WHERE id = $1").bind(id).execute(&mut *tx).await?;
    issues::record(&mut tx, issue, project, Some(user.id), "attachment", Some(&Change { field: "attachment", old: json!({"name": name}), new: Value::Null }))
        .await?;
    tx.commit().await?;
    let _ = state.blobs.delete(&ObjPath::from(key)).await;
    events::publish(&state, Event::new(user.workspace_id, "issue").project(project).actor(user.id)).await;
    Ok(StatusCode::NO_CONTENT)
}
