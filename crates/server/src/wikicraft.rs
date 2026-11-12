//! WikiCraft integration: documentation next to the work.
//!
//! - issues link wiki pages; their titles and spaces are looked up live (with the person's own
//!   WikiCraft token when they connected one, else the installation's)
//! - a project can name its wiki space: the project shows its recent pages and searches them
//! - pages can be created from an issue (a spec), a sprint (a review) or a release (notes)
//!
//! The other direction (issue chips and live issue tables in wiki pages) is WikiCraft's side:
//! it reads TaskCraft's API with a TaskCraft token.

use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::app::AppState;
use crate::auth::User;
use crate::error::{ApiError, ApiResult};

#[derive(Debug, Clone)]
pub struct Conn {
    pub base: String,
    pub token: String,
    /// the person's own token (else the installation's)
    pub personal: bool,
}

/// The WikiCraft connection for a person, None when WikiCraft isn't set up.
pub async fn conn(state: &AppState, user: &User) -> Option<Conn> {
    let base = state.config.wikicraft_url.as_deref()?.trim_end_matches('/').to_string();
    let personal: Option<String> = sqlx::query_scalar("SELECT preferences->>'wikicraftToken' FROM users WHERE id = $1")
        .bind(user.id)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten()
        .flatten()
        .filter(|t: &String| !t.is_empty());
    match (personal, state.config.wikicraft_token.clone()) {
        (Some(t), _) => Some(Conn { base, token: t, personal: true }),
        (None, Some(t)) => Some(Conn { base, token: t, personal: false }),
        (None, None) => None,
    }
}

pub fn base_url(state: &AppState) -> Option<String> {
    state.config.wikicraft_url.as_deref().map(|b| b.trim_end_matches('/').to_string())
}

/// Where a WikiCraft address points: a page id, or a space key and readable name.
#[derive(Debug, Clone, PartialEq)]
pub enum PageRef {
    Id(Uuid),
    Slug { space: String, slug: String },
}

/// Recognizes `<wiki>/pages/<id>` and `<wiki>/wiki/<KEY>/<slug>` on the configured WikiCraft.
pub fn parse_page_url(base: &str, url: &str) -> Option<PageRef> {
    let base = base.trim_end_matches('/');
    let rest = url.trim().strip_prefix(base)?;
    let rest = rest.split(['?', '#']).next().unwrap_or("");
    let parts: Vec<&str> = rest.split('/').filter(|p| !p.is_empty()).collect();
    match parts.as_slice() {
        ["pages", id, ..] => id.parse().ok().map(PageRef::Id),
        ["wiki", space, slug] => Some(PageRef::Slug { space: decode(space), slug: decode(slug) }),
        _ => None,
    }
}

fn decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(h) = s.get(i + 1..i + 3).and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(h);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn enc(s: &str) -> String {
    s.bytes()
        .map(|b| if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") })
        .collect()
}

async fn get(state: &AppState, c: &Conn, path: &str) -> ApiResult<Value> {
    let res = state
        .http
        .get(format!("{}/api/v1{path}", c.base))
        .bearer_auth(&c.token)
        .send()
        .await
        .map_err(|e| ApiError::BadRequest(format!("WikiCraft can't be reached: {e}")))?;
    let status = res.status();
    if status == reqwest::StatusCode::NOT_FOUND || status == reqwest::StatusCode::FORBIDDEN {
        return Err(ApiError::NotFound("wiki page"));
    }
    if !status.is_success() {
        return Err(ApiError::BadRequest(format!("WikiCraft answered {status}")));
    }
    res.json().await.map_err(|e| ApiError::BadRequest(format!("WikiCraft sent something unexpected: {e}")))
}

async fn post(state: &AppState, c: &Conn, path: &str, body: &Value) -> ApiResult<Value> {
    let res = state
        .http
        .post(format!("{}/api/v1{path}", c.base))
        .bearer_auth(&c.token)
        .json(body)
        .send()
        .await
        .map_err(|e| ApiError::BadRequest(format!("WikiCraft can't be reached: {e}")))?;
    let status = res.status();
    let v: Value = res.json().await.unwrap_or(Value::Null);
    if !status.is_success() {
        let msg = v["message"].as_str().unwrap_or("").to_string();
        return Err(ApiError::BadRequest(format!("WikiCraft answered {status}{}{msg}", if msg.is_empty() { "" } else { ": " })));
    }
    Ok(v)
}

/// A wiki page as TaskCraft shows it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PageInfo {
    pub id: Uuid,
    pub title: String,
    pub space_key: String,
    pub space_name: String,
    pub url: String,
    pub updated_at: Option<String>,
    pub excerpt: Option<String>,
}

fn page_info(c: &Conn, v: &Value) -> Option<PageInfo> {
    let id: Uuid = v["id"].as_str().or(v["pageId"].as_str())?.parse().ok()?;
    Some(PageInfo {
        id,
        title: v["title"].as_str().unwrap_or("").to_string(),
        space_key: v["spaceKey"].as_str().unwrap_or("").to_string(),
        space_name: v["spaceName"].as_str().unwrap_or("").to_string(),
        url: format!("{}/pages/{id}", c.base),
        updated_at: v["updatedAt"].as_str().map(str::to_string),
        excerpt: v["snippet"].as_str().map(|s| s.replace("<mark>", "").replace("</mark>", "")),
    })
}

/// Cache of page lookups (title and space change rarely; ten minutes).
static CACHE: std::sync::LazyLock<std::sync::Mutex<std::collections::HashMap<(String, String), (Instant, Option<PageInfo>)>>> =
    std::sync::LazyLock::new(Default::default);

/// Looks up a page by its address (None when it's gone or the person can't see it).
pub async fn lookup(state: &AppState, c: &Conn, url: &str) -> ApiResult<Option<PageInfo>> {
    let Some(r) = parse_page_url(&c.base, url) else { return Ok(None) };
    let cache_key = (c.token.clone(), url.to_string());
    if let Some((at, hit)) = CACHE.lock().ok().and_then(|m| m.get(&cache_key).cloned())
        && at.elapsed() < Duration::from_secs(600)
    {
        return Ok(hit);
    }
    let id = match r {
        PageRef::Id(id) => Some(id),
        PageRef::Slug { space, slug } => {
            let v = get(state, c, &format!("/resolve?space={}&slug={}", enc(&space), enc(&slug))).await?;
            v["pageId"].as_str().and_then(|s| s.parse().ok())
        }
    };
    let info = match id {
        Some(id) => match get(state, c, &format!("/pages/{id}")).await {
            Ok(v) => page_info(c, &v),
            Err(ApiError::NotFound(_)) => None,
            Err(e) => return Err(e),
        },
        None => None,
    };
    if let Ok(mut m) = CACHE.lock() {
        if m.len() > 5000 {
            m.clear();
        }
        m.insert(cache_key, (Instant::now(), info.clone()));
    }
    Ok(info)
}

pub async fn search(state: &AppState, c: &Conn, q: &str, space: Option<&str>) -> ApiResult<Vec<PageInfo>> {
    let mut path = format!("/search?q={}&limit=15", enc(q));
    if let Some(s) = space.filter(|s| !s.is_empty()) {
        path.push_str(&format!("&space={}", enc(s)));
    }
    let v = get(state, c, &path).await?;
    Ok(v.as_array().into_iter().flatten().filter_map(|p| page_info(c, p)).collect())
}

/// Recently updated pages of a space.
pub async fn recent(state: &AppState, c: &Conn, space: &str) -> ApiResult<Vec<PageInfo>> {
    let v = get(state, c, &format!("/reports/pages?spaces={}&sort=updated&max=12", enc(space))).await?;
    Ok(v.as_array().into_iter().flatten().filter_map(|p| page_info(c, p)).collect())
}

pub async fn spaces(state: &AppState, c: &Conn) -> ApiResult<Vec<Value>> {
    let v = get(state, c, "/spaces").await?;
    Ok(v.as_array()
        .into_iter()
        .flatten()
        .filter(|s| s["archivedAt"].is_null())
        .map(|s| json!({"key": s["key"], "name": s["name"], "url": format!("{}/spaces/{}", c.base, s["key"].as_str().unwrap_or(""))}))
        .collect())
}

/// Creates a page (Markdown content) and returns it.
pub async fn create_page(state: &AppState, c: &Conn, space: &str, title: &str, markdown: &str, parent: Option<Uuid>) -> ApiResult<PageInfo> {
    let mut body = json!({"spaceKey": space, "title": title, "markdown": markdown, "blank": true});
    if let Some(p) = parent {
        body["parentId"] = json!(p);
    }
    let v = post(state, c, "/pages", &body).await?;
    let id: Uuid = v["id"].as_str().and_then(|s| s.parse().ok()).ok_or_else(|| ApiError::BadRequest("WikiCraft didn't return the new page".into()))?;
    Ok(PageInfo {
        id,
        title: title.to_string(),
        space_key: space.to_string(),
        space_name: String::new(),
        url: format!("{}/pages/{id}", c.base),
        updated_at: None,
        excerpt: None,
    })
}

/// Requires a WikiCraft connection.
pub async fn require(state: &AppState, user: &User) -> ApiResult<Conn> {
    conn(state, user).await.ok_or_else(|| ApiError::BadRequest("WikiCraft isn't connected (set TASKCRAFT_WIKICRAFT_URL and a token)".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_addresses() {
        let id = Uuid::now_v7();
        assert_eq!(parse_page_url("https://wiki.acme.com/", &format!("https://wiki.acme.com/pages/{id}#x")), Some(PageRef::Id(id)));
        assert_eq!(
            parse_page_url("https://wiki.acme.com", "https://wiki.acme.com/wiki/ENG/Release%20notes"),
            Some(PageRef::Slug { space: "ENG".into(), slug: "Release notes".into() })
        );
        assert_eq!(parse_page_url("https://wiki.acme.com", "https://other.com/pages/x"), None);
        assert_eq!(parse_page_url("https://wiki.acme.com", "https://wiki.acme.com/spaces/ENG"), None);
        assert_eq!(enc("a b/ü"), "a%20b%2F%C3%BC");
    }
}
