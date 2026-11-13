//! Profile photos: a square image each person sets in their profile (the web app crops and
//! shrinks it first), shown wherever their avatar is. Admins can remove someone's photo.

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use chrono::{DateTime, Utc};
use object_store::ObjectStoreExt;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::app::{AppState, audit};
use crate::auth::{CurrentUser, SessionUser, require_admin};
use crate::error::{ApiError, ApiResult};

const MAX: usize = 1024 * 1024;

fn key(user: Uuid) -> object_store::path::Path {
    object_store::path::Path::from(format!("photos/{user}"))
}

/// The address of a person's photo (null without one); it changes with each new photo.
pub fn url(user: Uuid, at: Option<DateTime<Utc>>) -> Value {
    match at {
        Some(at) => json!(format!("/api/users/{user}/photo?v={}", at.timestamp_millis())),
        None => Value::Null,
    }
}

/// `PUT /me/photo`: the image itself as the body (PNG, JPEG, GIF or WebP, up to 1 MB).
pub async fn put(State(state): State<AppState>, SessionUser(user): SessionUser, body: Bytes) -> ApiResult<Json> {
    if body.len() > MAX {
        return Err(ApiError::bad("the photo is 1 MB at most"));
    }
    let mime = crate::features::image_type(&body).ok_or_else(|| ApiError::bad("the photo is a PNG, JPEG, GIF or WebP image"))?;
    state.blobs.put(&key(user.id), body.into()).await.map_err(|e| ApiError::Internal(anyhow::anyhow!("storage: {e}")))?;
    let at: DateTime<Utc> = sqlx::query_scalar(
        "UPDATE users SET photo_at = now(), preferences = jsonb_set(preferences, '{photoType}', to_jsonb($2::text)) WHERE id = $1 RETURNING photo_at",
    )
    .bind(user.id)
    .bind(mime)
    .fetch_one(&state.db)
    .await?;
    Ok(axum::Json(json!({"photo": url(user.id, Some(at))})))
}

type Json = axum::Json<Value>;

async fn clear(state: &AppState, id: Uuid) -> ApiResult<()> {
    let _ = state.blobs.delete(&key(id)).await;
    sqlx::query("UPDATE users SET photo_at = NULL, preferences = preferences - 'photoType' WHERE id = $1").bind(id).execute(&state.db).await?;
    Ok(())
}

pub async fn delete_mine(State(state): State<AppState>, SessionUser(user): SessionUser) -> ApiResult<StatusCode> {
    clear(&state, user.id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /users/{id}/photo`: admins take down someone's photo.
pub async fn delete(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    require_admin(&user)?;
    let found: Option<Uuid> =
        sqlx::query_scalar("SELECT id FROM users WHERE id = $1 AND workspace_id = $2").bind(id).bind(user.workspace_id).fetch_optional(&state.db).await?;
    found.ok_or(ApiError::NotFound("person"))?;
    clear(&state, id).await?;
    audit(&state, user.workspace_id, Some(user.id), "user.photo_removed", Some(id.to_string()), json!({})).await;
    Ok(StatusCode::NO_CONTENT)
}

/// Removes a person's photo when their account is removed for good.
pub async fn forget(state: &AppState, id: Uuid) {
    let _ = clear(state, id).await;
}

/// `GET /users/{id}/photo`: for people of the same workspace.
pub async fn get(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(id): Path<Uuid>) -> ApiResult<Response> {
    let row: Option<(Option<DateTime<Utc>>, Option<String>)> =
        sqlx::query_as("SELECT photo_at, preferences->>'photoType' FROM users WHERE id = $1 AND workspace_id = $2")
            .bind(id)
            .bind(user.workspace_id)
            .fetch_optional(&state.db)
            .await?;
    let Some((Some(_), mime)) = row else { return Err(ApiError::NotFound("photo")) };
    let got = state.blobs.get(&key(id)).await.map_err(|_| ApiError::NotFound("photo"))?;
    let bytes = got.bytes().await.map_err(|e| ApiError::Internal(e.into()))?;
    Ok((
        [
            (header::CONTENT_TYPE, mime.unwrap_or_else(|| "image/png".into())),
            // the address changes with each new photo (?v=…)
            (header::CACHE_CONTROL, "private, max-age=604800".to_string()),
        ],
        Body::from(bytes),
    )
        .into_response())
}
