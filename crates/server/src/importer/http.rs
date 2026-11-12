//! HTTP for imports from other trackers' APIs. Requests go to the source's own server only (and
//! to the hosts it keeps files on); names that resolve to private or loopback addresses are
//! refused unless `TASKCRAFT_IMPORT_ALLOW_PRIVATE` allows them; redirects are followed by hand so
//! credentials never leave the source's host; rate limits are waited out (the progress says
//! until when); credentials are never logged or put into error messages.

use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, bail};
use chrono::{DateTime, TimeZone, Utc};
use futures::StreamExt;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use reqwest::{StatusCode, Url};
use serde_json::Value;

use super::model::{File, TooLarge};
use super::{Progress, ProgressHandle};
use crate::webhooks::is_private;

/// JSON answers larger than this are refused.
const MAX_JSON: u64 = 64 * 1024 * 1024;
/// Waiting for one rate limit takes at most this long.
const MAX_WAIT: Duration = Duration::from_secs(65 * 60);

/// An answer the source gave instead of what was asked.
#[derive(Debug)]
pub struct Status {
    pub code: u16,
    /// what was asked (the address without its query)
    pub what: String,
    /// the source's own words, when it gave some
    pub message: Option<String>,
}

impl std::fmt::Display for Status {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let why = match self.code {
            401 => "the token was refused (check it and its expiry)",
            403 => "not allowed with this token (missing permission or scope)",
            404 => "not found, or the token can't see it",
            410 => "gone",
            _ => "the server answered with an error",
        };
        write!(f, "{} → HTTP {}: {why}", self.what, self.code)?;
        if let Some(m) = self.message.as_deref().filter(|m| !m.is_empty()) {
            write!(f, " ({})", m.chars().take(200).collect::<String>())?;
        }
        Ok(())
    }
}

impl std::error::Error for Status {}

/// The HTTP status of an error, when the source answered with one.
pub fn status_of(e: &anyhow::Error) -> Option<u16> {
    e.downcast_ref::<Status>().map(|s| s.code)
}

/// Resolves names, refusing private addresses unless allowed.
struct Guard {
    allow_private: bool,
}

impl reqwest::dns::Resolve for Guard {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let allow = self.allow_private;
        let host = name.as_str().to_string();
        Box::pin(async move {
            let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), 0)).await?.collect();
            if !allow && let Some(bad) = addrs.iter().find(|a| is_private(a.ip())) {
                return Err(format!("{host} resolves to a private address ({}): set TASKCRAFT_IMPORT_ALLOW_PRIVATE=true to import from it", bad.ip()).into());
            }
            Ok(Box::new(addrs.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

/// A client for one source.
#[derive(Clone)]
pub struct Http {
    client: reqwest::Client,
    base: Url,
    /// other hosts files may come from (a leading dot: that domain and its subdomains)
    file_hosts: Vec<String>,
    /// sent to the source's own host only
    auth: Option<(HeaderName, HeaderValue)>,
    extra: HeaderMap,
    allow_private: bool,
    progress: ProgressHandle,
    /// requests aren't sent before this (a rate limit ran out)
    pause: Arc<Mutex<Option<DateTime<Utc>>>>,
    /// requests made so far
    pub requests: Arc<std::sync::atomic::AtomicU64>,
}

impl std::fmt::Debug for Http {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Http").field("base", &self.base.as_str()).field("auth", &self.auth.is_some()).finish()
    }
}

/// The address of a source as entered: `https` assumed, no credentials or query in it.
pub fn base_url(s: &str) -> anyhow::Result<Url> {
    let s = s.trim();
    let s = if s.contains("://") { s.to_string() } else { format!("https://{s}") };
    let mut u = Url::parse(&s).context("the address isn't a URL")?;
    anyhow::ensure!(matches!(u.scheme(), "http" | "https"), "the address must start with https:// (or http://)");
    anyhow::ensure!(u.host_str().is_some_and(|h| !h.is_empty()), "the address needs a host");
    anyhow::ensure!(u.username().is_empty() && u.password().is_none(), "put credentials in the token field, not in the address");
    u.set_query(None);
    u.set_fragment(None);
    if !u.path().ends_with('/') {
        let p = format!("{}/", u.path());
        u.set_path(&p);
    }
    Ok(u)
}

fn same_origin(a: &Url, b: &Url) -> bool {
    a.scheme() == b.scheme() && a.host_str() == b.host_str() && a.port_or_known_default() == b.port_or_known_default()
}

/// An address without its query (signed links carry tokens there) for messages.
pub fn shown(u: &Url) -> String {
    let mut u = u.clone();
    u.set_query(None);
    u.set_fragment(None);
    let _ = u.set_password(None);
    let _ = u.set_username("");
    u.to_string()
}

impl Http {
    /// A client for the source at `base`; `auth` is a header with the credentials.
    pub fn new(base: Url, auth: Option<(&str, String)>, allow_private: bool, progress: ProgressHandle) -> anyhow::Result<Http> {
        let client = reqwest::Client::builder()
            .dns_resolver(Arc::new(Guard { allow_private }))
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(120))
            .user_agent(concat!("TaskCraft-Import/", env!("CARGO_PKG_VERSION")))
            .build()?;
        let auth = match auth {
            Some((name, value)) if !value.trim().is_empty() => {
                let mut v = HeaderValue::from_str(value.trim()).map_err(|_| anyhow::anyhow!("the token has characters a token can't have"))?;
                v.set_sensitive(true);
                Some((HeaderName::from_bytes(name.as_bytes())?, v))
            }
            _ => None,
        };
        Ok(Http { client, base, file_hosts: vec![], auth, extra: HeaderMap::new(), allow_private, progress, pause: Arc::default(), requests: Arc::default() })
    }

    /// Hosts files may also come from (`.example.com`: the domain and its subdomains).
    pub fn with_file_hosts(mut self, hosts: Vec<String>) -> Http {
        self.file_hosts = hosts;
        self
    }

    /// A header sent with every request.
    pub fn with_header(mut self, name: &'static str, value: &'static str) -> Http {
        self.extra.insert(name, HeaderValue::from_static(value));
        self
    }

    pub fn base(&self) -> &Url {
        &self.base
    }

    pub fn has_auth(&self) -> bool {
        self.auth.is_some()
    }

    /// An address of the API: `path` (with its query) relative to the base.
    pub fn url(&self, path: &str) -> anyhow::Result<Url> {
        self.base.join(path.trim_start_matches('/')).with_context(|| format!("bad address {path}"))
    }

    fn allowed(&self, u: &Url) -> bool {
        if !matches!(u.scheme(), "http" | "https") {
            return false;
        }
        if same_origin(u, &self.base) {
            return true;
        }
        let Some(host) = u.host_str() else { return false };
        let base_host = self.base.host_str().unwrap_or("");
        host == base_host
            || self.file_hosts.iter().any(|h| match h.strip_prefix('.') {
                Some(domain) => host == domain || host.ends_with(&format!(".{domain}")),
                None => host == h,
            })
    }

    /// Refuses addresses whose host is a private IP address (names are checked when resolved).
    fn check_ip(&self, u: &Url) -> anyhow::Result<()> {
        let Some(ip) = host_ip(u) else { return Ok(()) };
        if !self.allow_private && is_private(ip) {
            bail!("{ip} is a private address: set TASKCRAFT_IMPORT_ALLOW_PRIVATE=true to import from it");
        }
        Ok(())
    }

    /// Waits while a rate limit is out, saying so in the progress.
    async fn wait_pause(&self) {
        let until = self.pause.lock().ok().and_then(|p| *p);
        let Some(until) = until else { return };
        let wait = (until - Utc::now()).to_std().unwrap_or_default().min(MAX_WAIT);
        if !wait.is_zero() {
            let before: Progress = self.progress.lock().map(|p| p.clone()).unwrap_or_default();
            if let Ok(mut p) = self.progress.lock() {
                *p = Progress { phase: "waiting".into(), until: Some(until), ..before.clone() };
            }
            tracing::info!(until = %until, "import: waiting for the source's rate limit");
            tokio::time::sleep(wait).await;
            if let Ok(mut p) = self.progress.lock() {
                *p = before;
            }
        }
        if let Ok(mut p) = self.pause.lock() {
            *p = None;
        }
    }

    /// When a rate limit lets requests through again, by the answer's headers.
    fn limit_reset(status: StatusCode, h: &HeaderMap) -> Option<DateTime<Utc>> {
        let num = |k: &str| h.get(k).and_then(|v| v.to_str().ok()).and_then(|v| v.trim().parse::<i64>().ok());
        let limited = status == StatusCode::TOO_MANY_REQUESTS
            || (status == StatusCode::FORBIDDEN && (num("x-ratelimit-remaining") == Some(0) || h.contains_key("retry-after")));
        if !limited {
            return None;
        }
        if let Some(s) = num("retry-after") {
            return Some(Utc::now() + chrono::Duration::seconds(s.clamp(1, 3600)));
        }
        for k in ["x-ratelimit-reset", "ratelimit-reset"] {
            if let Some(t) = num(k) {
                // epoch seconds (GitHub, GitLab) or seconds from now (the draft standard)
                let at = if t > 1_000_000_000 { Utc.timestamp_opt(t, 0).single()? } else { Utc::now() + chrono::Duration::seconds(t) };
                return Some(at.max(Utc::now()) + chrono::Duration::seconds(1));
            }
        }
        Some(Utc::now() + chrono::Duration::seconds(60))
    }

    /// One GET with retries, rate limits and redirects; the answer when it's a success.
    async fn send(&self, url: Url, accept: &str) -> anyhow::Result<reqwest::Response> {
        let mut url = url;
        let mut tries = 0u32;
        let mut waits = 0u32;
        let mut hops = 0u32;
        loop {
            anyhow::ensure!(self.allowed(&url), "{} isn't on the source's server: not fetched", shown(&url));
            self.check_ip(&url)?;
            self.wait_pause().await;
            let mut req = self.client.get(url.clone()).header("accept", accept).headers(self.extra.clone());
            if same_origin(&url, &self.base)
                && let Some((k, v)) = &self.auth
            {
                req = req.header(k.clone(), v.clone());
            }
            self.requests.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let res = match req.send().await {
                Ok(r) => r,
                Err(e) if tries < 3 && (e.is_timeout() || e.is_connect() || e.is_request()) && !format!("{e:#}").contains("private address") => {
                    tries += 1;
                    tokio::time::sleep(Duration::from_secs(2u64.pow(tries))).await;
                    continue;
                }
                Err(e) => {
                    // without the address reqwest puts in its message (signed links carry tokens)
                    let e = e.without_url();
                    let mut msg = format!("{} → {e}", shown(&url));
                    let mut src = std::error::Error::source(&e);
                    while let Some(s) = src {
                        msg.push_str(&format!(": {s}"));
                        src = s.source();
                    }
                    bail!(msg);
                }
            };
            let status = res.status();
            if status.is_redirection() {
                let to = res.headers().get("location").and_then(|v| v.to_str().ok()).and_then(|l| url.join(l).ok());
                hops += 1;
                match to {
                    Some(to) if hops <= 5 => {
                        url = to;
                        continue;
                    }
                    _ => bail!("{} → too many or broken redirects", shown(&url)),
                }
            }
            if let Some(until) = Self::limit_reset(status, res.headers())
                && waits < 30
            {
                waits += 1;
                if let Ok(mut p) = self.pause.lock() {
                    *p = Some(until);
                }
                continue;
            }
            if status.is_server_error() && tries < 3 {
                tries += 1;
                tokio::time::sleep(Duration::from_secs(2u64.pow(tries))).await;
                continue;
            }
            // the last request of the hour: wait before the next one
            let remaining = res.headers().get("x-ratelimit-remaining").and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<i64>().ok());
            if remaining == Some(0)
                && let Some(t) = res.headers().get("x-ratelimit-reset").and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<i64>().ok())
                && let Some(at) = Utc.timestamp_opt(t, 0).single()
                && let Ok(mut p) = self.pause.lock()
            {
                *p = Some(at + chrono::Duration::seconds(1));
            }
            if !status.is_success() {
                let what = shown(&url);
                let message = res.bytes().await.ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()).and_then(|v| {
                    v["message"].as_str().or(v["error"].as_str()).map(str::to_string).or_else(|| v["errors"].as_array().map(|e| format!("{e:?}")))
                });
                return Err(Status { code: status.as_u16(), what, message }.into());
            }
            return Ok(res);
        }
    }

    async fn body(res: reqwest::Response, max: u64) -> anyhow::Result<Vec<u8>> {
        if res.content_length().is_some_and(|n| n > max) {
            return Err(TooLarge.into());
        }
        let mut out = Vec::new();
        let mut stream = res.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            if (out.len() + chunk.len()) as u64 > max {
                return Err(TooLarge.into());
            }
            out.extend_from_slice(&chunk);
        }
        Ok(out)
    }

    /// A JSON answer with its headers.
    pub async fn get(&self, url: Url) -> anyhow::Result<(Value, HeaderMap)> {
        let what = shown(&url);
        let res = self.send(url, "application/json").await?;
        let headers = res.headers().clone();
        let body = Self::body(res, MAX_JSON).await.with_context(|| what.clone())?;
        let v = serde_json::from_slice(&body).with_context(|| format!("{what}: the answer isn't JSON (is this the API's address?)"))?;
        Ok((v, headers))
    }

    /// A JSON answer, or None when it's not there (404).
    pub async fn get_opt(&self, url: Url) -> anyhow::Result<Option<Value>> {
        match self.get(url).await {
            Ok((v, _)) => Ok(Some(v)),
            Err(e) if status_of(&e) == Some(404) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Every item of a list the API pages with `Link: <…>; rel="next"` headers.
    pub async fn all_pages(&self, first: Url, limit: usize) -> anyhow::Result<Vec<Value>> {
        let mut out = Vec::new();
        let mut next = Some(first);
        while let Some(url) = next.take() {
            let (v, h) = self.get(url).await?;
            match v {
                Value::Array(items) => out.extend(items),
                other => bail!("expected a list, got {}", other.to_string().chars().take(100).collect::<String>()),
            }
            if out.len() >= limit {
                out.truncate(limit);
                break;
            }
            next = next_link(&h).filter(|u| self.allowed(u));
        }
        Ok(out)
    }

    /// A file: `Ok(None)` when it's gone, [`TooLarge`] beyond `max` bytes.
    pub async fn download(&self, url: &str, max: u64) -> anyhow::Result<Option<File>> {
        let u = Url::parse(url).or_else(|_| self.url(url)).with_context(|| format!("bad address {url}"))?;
        let res = match self.send(u, "*/*").await {
            Ok(r) => r,
            Err(e) if matches!(status_of(&e), Some(404 | 410)) => return Ok(None),
            Err(e) => return Err(e),
        };
        let mime = res.headers().get("content-type").and_then(|v| v.to_str().ok()).map(|m| m.split(';').next().unwrap_or(m).trim().to_string());
        // a sign-in page instead of the file (files of private projects behind a browser session)
        let page = |p: &str| p.ends_with(".html") || p.ends_with(".htm");
        if mime.as_deref() == Some("text/html") && !page(url) && !page(res.url().path()) {
            return Ok(None);
        }
        let name = res
            .headers()
            .get("content-disposition")
            .and_then(|v| v.to_str().ok())
            .and_then(|d| d.split(';').find_map(|p| p.trim().strip_prefix("filename=").map(|n| n.trim_matches('"').to_string())))
            .or_else(|| res.url().path_segments().and_then(|mut s| s.next_back()).map(|s| s.to_string()).filter(|s| s.contains('.')));
        let data = Self::body(res, max).await?;
        Ok(Some(File { data, mime, name }))
    }

    /// Whether the source's host may be reached (names checked against private addresses).
    pub async fn check_host(&self) -> anyhow::Result<()> {
        self.check_ip(&self.base)?;
        if let Some(host) = self.base.host_str().filter(|_| host_ip(&self.base).is_none()) {
            let port = self.base.port_or_known_default().unwrap_or(443);
            let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host, port)).await.with_context(|| format!("can't find the server {host}"))?.collect();
            anyhow::ensure!(!addrs.is_empty(), "can't find the server {host}");
            if !self.allow_private
                && let Some(bad) = addrs.iter().find(|a| is_private(a.ip()))
            {
                bail!("{host} resolves to a private address ({}): set TASKCRAFT_IMPORT_ALLOW_PRIVATE=true to import from it", bad.ip());
            }
        }
        Ok(())
    }
}

/// The address of a URL whose host is an IP address.
fn host_ip(u: &Url) -> Option<IpAddr> {
    u.host_str()?.trim_start_matches('[').trim_end_matches(']').parse().ok()
}

/// The `rel="next"` address of a `Link` header.
pub fn next_link(h: &HeaderMap) -> Option<Url> {
    let link = h.get("link")?.to_str().ok()?;
    link.split(',').find_map(|part| {
        let (url, rest) = part.trim().split_once(';')?;
        rest.split(';').any(|p| p.trim().trim_matches('"') == "rel=\"next\"" || p.trim() == "rel=\"next\"" || p.trim() == "rel=next").then_some(())?;
        Url::parse(url.trim().trim_start_matches('<').trim_end_matches('>')).ok()
    })
}

/// A moment as APIs write it (RFC 3339 / ISO 8601).
pub fn time(v: &Value) -> Option<DateTime<Utc>> {
    v.as_str().and_then(|s| DateTime::parse_from_rfc3339(s.trim()).ok()).map(|t| t.with_timezone(&Utc))
}

/// A day as APIs write it (`2026-11-12`, also at the start of a moment).
pub fn day(v: &Value) -> Option<chrono::NaiveDate> {
    v.as_str().and_then(super::engine::parse_day)
}

/// A string field with something in it.
pub fn text(v: &Value) -> Option<String> {
    v.as_str().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string)
}

/// An id (number or string) as text.
pub fn id(v: &Value) -> Option<String> {
    match v {
        Value::Number(n) => Some(n.to_string()),
        Value::String(s) if !s.trim().is_empty() => Some(s.trim().to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_and_addresses() {
        let mut h = HeaderMap::new();
        h.insert(
            "link",
            HeaderValue::from_static(r#"<https://api.example.com/x?page=1>; rel="prev", <https://api.example.com/x?page=3>; rel="next", <https://api.example.com/x?page=9>; rel="last""#),
        );
        assert_eq!(next_link(&h).unwrap().as_str(), "https://api.example.com/x?page=3");
        assert_eq!(base_url("gitlab.example.com").unwrap().as_str(), "https://gitlab.example.com/");
        assert_eq!(base_url("https://ghe.example.com/api/v3?x=1").unwrap().as_str(), "https://ghe.example.com/api/v3/");
        assert!(base_url("ftp://example.com").is_err());
        assert!(base_url("https://user:pw@example.com").is_err());
        assert_eq!(shown(&Url::parse("https://files.example.com/a.png?jwt=secret").unwrap()), "https://files.example.com/a.png");
    }

    #[tokio::test]
    async fn private_addresses_are_refused() {
        let http = Http::new(base_url("http://127.0.0.1:9/").unwrap(), Some(("authorization", "Bearer x".into())), false, ProgressHandle::default()).unwrap();
        let e = http.get(http.url("/x").unwrap()).await.unwrap_err();
        assert!(e.to_string().contains("private address"), "{e}");
        assert!(!format!("{e:#}").contains("Bearer"), "{e:#}");
        let other = http.download("https://elsewhere.example.com/file.png", 10).await.unwrap_err();
        assert!(other.to_string().contains("isn't on the source's server"), "{other}");
        let http = Http::new(base_url("http://localhost:9/").unwrap(), None, false, ProgressHandle::default()).unwrap();
        let e = http.check_host().await.unwrap_err();
        assert!(e.to_string().contains("private address"), "{e}");
    }

    #[test]
    fn rate_limits() {
        let mut h = HeaderMap::new();
        h.insert("x-ratelimit-remaining", HeaderValue::from_static("0"));
        h.insert("x-ratelimit-reset", HeaderValue::from_str(&(Utc::now().timestamp() + 30).to_string()).unwrap());
        let at = Http::limit_reset(StatusCode::FORBIDDEN, &h).unwrap();
        assert!((at - Utc::now()).num_seconds() >= 29);
        assert!(Http::limit_reset(StatusCode::FORBIDDEN, &HeaderMap::new()).is_none(), "a plain 403 isn't a rate limit");
        let mut h = HeaderMap::new();
        h.insert("retry-after", HeaderValue::from_static("5"));
        let at = Http::limit_reset(StatusCode::TOO_MANY_REQUESTS, &h).unwrap();
        assert!((at - Utc::now()).num_seconds() <= 5);
    }
}
