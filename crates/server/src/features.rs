//! Workspace settings beyond the name and colour: parts of TaskCraft a workspace can turn off
//! (`settings.features`), the default time zone and date format, an announcement for everyone,
//! and the logo.

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use chrono::{DateTime, Utc};
use object_store::ObjectStoreExt;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::app::{AppState, audit};
use crate::auth::{CurrentUser, SessionUser, require_admin};
use crate::error::{ApiError, ApiResult};

/// What can be turned off, and what it's called in messages.
pub const FEATURES: [(&str, &str); 6] = [
    ("timeTracking", "Time tracking"),
    ("votes", "Voting"),
    ("portal", "The service desk portal"),
    ("automation", "Automation"),
    ("sharing", "Sharing with links"),
    ("ai", "AI assistance"),
];

/// Whether a part of TaskCraft is on in a workspace (everything is, unless turned off).
pub async fn on(state: &AppState, workspace: Uuid, feature: &str) -> bool {
    let off: Option<bool> = sqlx::query_scalar("SELECT (settings->'features'->>$2) = 'false' FROM workspaces WHERE id = $1")
        .bind(workspace)
        .bind(feature)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten();
    off != Some(true)
}

/// Refuses when a part is off.
pub async fn require(state: &AppState, workspace: Uuid, feature: &str) -> ApiResult<()> {
    if on(state, workspace, feature).await {
        return Ok(());
    }
    let name = FEATURES.iter().find(|(k, _)| *k == feature).map(|(_, n)| *n).unwrap_or(feature);
    Err(ApiError::bad(format!("{name} is turned off in this workspace")))
}

/// The portal and request forms answer 404 while the portal is off (they're public: one
/// workspace per server).
pub async fn portal_switch(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let off: bool = sqlx::query_scalar("SELECT COALESCE((settings->'features'->>'portal') = 'false', false) FROM workspaces ORDER BY created_at LIMIT 1")
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten()
        .unwrap_or(false);
    if off {
        return ApiError::NotFound("portal").into_response();
    }
    next.run(req).await
}

/// Checks and tidies the settings this module keeps; the others are left alone.
pub fn check(req: &Value, settings: &mut Value) -> ApiResult<()> {
    if let Some(v) = req.get("features") {
        let o = v.as_object().ok_or_else(|| ApiError::bad("features must be an object"))?;
        let mut out = serde_json::Map::new();
        for (k, on) in o {
            if !FEATURES.iter().any(|(f, _)| f == k) {
                return Err(ApiError::bad(format!("unknown feature {k}")));
            }
            out.insert(k.clone(), json!(on.as_bool().ok_or_else(|| ApiError::bad(format!("{k} is true or false")))?));
        }
        settings["features"] = Value::Object(out);
    }
    if let Some(v) = req.get("timeZone") {
        match v.as_str().filter(|s| !s.is_empty()) {
            Some(tz) => {
                tz.parse::<chrono_tz::Tz>().map_err(|_| ApiError::bad(format!("unknown time zone {tz}")))?;
                settings["timeZone"] = json!(tz);
            }
            None => settings["timeZone"] = Value::Null,
        }
    }
    if let Some(v) = req.get("dateFormat") {
        match v.as_str().filter(|s| !s.is_empty() && *s != "auto") {
            Some(f) if ["dmy", "mdy", "ymd"].contains(&f) => settings["dateFormat"] = json!(f),
            Some(f) => return Err(ApiError::bad(format!("dateFormat is auto, dmy, mdy or ymd, not {f}"))),
            None => settings["dateFormat"] = Value::Null,
        }
    }
    if let Some(v) = req.get("announcement") {
        if v.is_null() || v["text"].as_str().is_some_and(|t| t.trim().is_empty()) {
            settings["announcement"] = Value::Null;
        } else {
            let text = v["text"].as_str().ok_or_else(|| ApiError::bad("the announcement needs a text"))?.trim();
            if text.chars().count() > 500 {
                return Err(ApiError::bad("the announcement is 500 characters at most"));
            }
            let tone = v["tone"].as_str().unwrap_or("info");
            if !["info", "warning"].contains(&tone) {
                return Err(ApiError::bad("the announcement's tone is info or warning"));
            }
            let until = match v["until"].as_str().filter(|s| !s.is_empty()) {
                Some(u) => Some(DateTime::parse_from_rfc3339(u).map_err(|_| ApiError::bad("until is a date and time (RFC 3339)"))?.with_timezone(&Utc)),
                None => None,
            };
            // a new id each time: people who closed the old one see the new one
            settings["announcement"] = json!({"id": crate::app::new_id(), "text": text, "tone": tone, "until": until, "at": Utc::now()});
        }
    }
    Ok(())
}

/// The announcement while it's current.
pub fn announcement(settings: &Value) -> Value {
    let a = &settings["announcement"];
    let current = a.is_object() && a["until"].as_str().and_then(|u| DateTime::parse_from_rfc3339(u).ok()).is_none_or(|u| u.with_timezone(&Utc) > Utc::now());
    if current { a.clone() } else { Value::Null }
}

/// The features, all of them, on or off.
pub fn all(settings: &Value) -> Value {
    Value::Object(FEATURES.iter().map(|(k, _)| (k.to_string(), json!(settings["features"][k] != json!(false)))).collect())
}

// ---- the logo ----

const LOGO_MAX: usize = 512 * 1024;

fn logo_key(workspace: Uuid) -> object_store::path::Path {
    object_store::path::Path::from(format!("workspace/{workspace}/logo"))
}

/// A PNG, JPEG, GIF or WebP by its first bytes (not SVG: it can carry scripts).
pub fn image_type(data: &[u8]) -> Option<&'static str> {
    if data.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if data.starts_with(b"\xff\xd8\xff") {
        Some("image/jpeg")
    } else if data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if data.len() > 12 && &data[..4] == b"RIFF" && &data[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

/// `PUT /admin/workspace/logo`: the image itself as the body.
pub async fn put_logo(State(state): State<AppState>, SessionUser(user): SessionUser, body: Bytes) -> ApiResult<StatusCode> {
    require_admin(&user)?;
    if body.len() > LOGO_MAX {
        return Err(ApiError::bad("the logo is 512 KB at most"));
    }
    let mime = image_type(&body).ok_or_else(|| ApiError::bad("the logo is a PNG, JPEG, GIF or WebP image"))?;
    state.blobs.put(&logo_key(user.workspace_id), body.into()).await.map_err(|e| ApiError::Internal(anyhow::anyhow!("storage: {e}")))?;
    sqlx::query("UPDATE workspaces SET settings = jsonb_set(settings, '{logo}', $2) WHERE id = $1")
        .bind(user.workspace_id)
        .bind(json!({"type": mime, "at": Utc::now().timestamp_millis()}))
        .execute(&state.db)
        .await?;
    audit(&state, user.workspace_id, Some(user.id), "workspace.logo", None, json!({"type": mime})).await;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn delete_logo(State(state): State<AppState>, SessionUser(user): SessionUser) -> ApiResult<StatusCode> {
    require_admin(&user)?;
    let _ = state.blobs.delete(&logo_key(user.workspace_id)).await;
    sqlx::query("UPDATE workspaces SET settings = settings - 'logo' WHERE id = $1").bind(user.workspace_id).execute(&state.db).await?;
    audit(&state, user.workspace_id, Some(user.id), "workspace.logo", None, json!({"removed": true})).await;
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /workspace/logo`.
pub async fn logo(State(state): State<AppState>, CurrentUser(user): CurrentUser) -> ApiResult<Response> {
    let settings: Value = sqlx::query_scalar("SELECT settings FROM workspaces WHERE id = $1").bind(user.workspace_id).fetch_one(&state.db).await?;
    let mime = settings["logo"]["type"].as_str().ok_or(ApiError::NotFound("logo"))?.to_string();
    let got = state.blobs.get(&logo_key(user.workspace_id)).await.map_err(|_| ApiError::NotFound("logo"))?;
    let bytes = got.bytes().await.map_err(|e| ApiError::Internal(e.into()))?;
    Ok((
        [
            (header::CONTENT_TYPE, mime),
            // the address changes with each new logo (?v=…)
            (header::CACHE_CONTROL, "private, max-age=86400".to_string()),
        ],
        Body::from(bytes),
    )
        .into_response())
}

/// The logo's address for the web app (None: TaskCraft's own mark).
pub fn logo_url(settings: &Value) -> Value {
    match settings["logo"]["at"].as_i64() {
        Some(v) => json!(format!("/api/workspace/logo?v={v}")),
        None => Value::Null,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_are_checked() {
        let mut s = json!({"name": "x"});
        check(&json!({"features": {"votes": false}, "timeZone": "Europe/Zurich", "dateFormat": "ymd"}), &mut s).unwrap();
        assert_eq!(s["features"], json!({"votes": false}));
        assert_eq!(all(&s), json!({"timeTracking": true, "votes": false, "portal": true, "automation": true, "sharing": true, "ai": true}));
        assert_eq!(s["timeZone"], "Europe/Zurich");
        assert!(check(&json!({"features": {"teleport": false}}), &mut s).is_err());
        assert!(check(&json!({"timeZone": "Mars/Base"}), &mut s).is_err());
        assert!(check(&json!({"dateFormat": "yy"}), &mut s).is_err());
        check(&json!({"dateFormat": "auto", "timeZone": ""}), &mut s).unwrap();
        assert_eq!((s["dateFormat"].clone(), s["timeZone"].clone()), (Value::Null, Value::Null));
        check(&json!({"announcement": {"text": "Maintenance tonight", "tone": "warning"}}), &mut s).unwrap();
        assert_eq!(announcement(&s)["text"], "Maintenance tonight");
        check(&json!({"announcement": {"text": "Old", "until": "2020-01-01T00:00:00Z"}}), &mut s).unwrap();
        assert_eq!(announcement(&s), Value::Null, "over");
        assert!(check(&json!({"announcement": {"text": "x", "tone": "party"}}), &mut s).is_err());
        assert_eq!(image_type(b"\x89PNG\r\n\x1a\nrest"), Some("image/png"));
        assert_eq!(image_type(b"<svg onload=alert(1)>"), None);
    }
}
