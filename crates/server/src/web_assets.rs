//! The web app (`web/dist`): served from `TASKCRAFT_STATIC_DIR` when that directory holds a
//! build, otherwise from the copy compiled into the binary (release builds embed `web/dist` as it
//! was at build time; debug builds read it from disk), so a single binary is enough to run
//! TaskCraft.
//!
//! Vite's hashed files under `assets/` are cached for a year (`immutable`); everything else, above
//! all `index.html`, is revalidated on every load so a new version shows up right away.

use std::path::Path;

use axum::body::Body;
use axum::extract::Request;
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::MethodRouter;
use tower_http::services::{ServeDir, ServeFile};

#[derive(rust_embed::RustEmbed)]
#[folder = "../../web/dist"]
#[allow_missing = true]
struct Embedded;

const IMMUTABLE: &str = "public, max-age=31536000, immutable";
const REVALIDATE: &str = "no-cache";

fn cache_control(path: &str) -> &'static str {
    if path.trim_start_matches('/').starts_with("assets/") { IMMUTABLE } else { REVALIDATE }
}

/// Where the web app comes from (for the startup log).
pub fn source(static_dir: &Path) -> String {
    if static_dir.join("index.html").is_file() {
        format!("directory {}", static_dir.display())
    } else if Embedded::get("index.html").is_some() {
        "embedded in the binary".into()
    } else {
        "missing (build web/ or set TASKCRAFT_STATIC_DIR)".into()
    }
}

/// The fallback service for everything that isn't an API or legacy route.
pub fn service(static_dir: &Path) -> MethodRouter {
    if static_dir.join("index.html").is_file() {
        let spa = ServeDir::new(static_dir).fallback(ServeFile::new(static_dir.join("index.html")));
        axum::routing::get_service(spa).layer(axum::middleware::from_fn(disk_cache_headers))
    } else {
        axum::routing::get(embedded)
    }
}

async fn disk_cache_headers(req: Request, next: Next) -> Response {
    let cc = cache_control(req.uri().path());
    let mut res = next.run(req).await;
    if res.status().is_success() {
        res.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static(cc));
    }
    res
}

async fn embedded(req: Request) -> Response {
    let path = req.uri().path().trim_start_matches('/');
    let (file, path) = match Embedded::get(path).filter(|_| !path.is_empty()) {
        Some(f) => (f, path),
        // a missing hashed asset is a 404, not the app (a stale tab after an upgrade)
        None if path.starts_with("assets/") => return StatusCode::NOT_FOUND.into_response(),
        // client-side routes (/projects/…, /issues/…) get the app
        None => match Embedded::get("index.html") {
            Some(f) => (f, "index.html"),
            None => return (StatusCode::NOT_FOUND, "The web app isn't built: run `npm run build` in web/ or set TASKCRAFT_STATIC_DIR.").into_response(),
        },
    };
    let etag = format!("\"{}\"", hex::encode(&file.metadata.sha256_hash()[..16]));
    if req.headers().get(header::IF_NONE_MATCH).and_then(|v| v.to_str().ok()).is_some_and(|v| v.split(',').any(|t| t.trim() == etag)) {
        return Response::builder().status(StatusCode::NOT_MODIFIED).header(header::ETAG, etag).body(Body::empty()).unwrap_or_default();
    }
    Response::builder()
        .header(header::CONTENT_TYPE, file.metadata.mimetype())
        .header(header::CACHE_CONTROL, cache_control(path))
        .header(header::ETAG, etag)
        .body(Body::from(file.data.into_owned()))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashed_assets_are_immutable_the_rest_revalidates() {
        assert_eq!(cache_control("/assets/index-DzalhLSO.js"), IMMUTABLE);
        assert_eq!(cache_control("/index.html"), REVALIDATE);
        assert_eq!(cache_control("/pages/123"), REVALIDATE);
        assert_eq!(cache_control("/projects/WEB/board"), REVALIDATE);
    }
}
