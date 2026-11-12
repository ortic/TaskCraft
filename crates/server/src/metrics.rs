//! Prometheus metrics at `/api/metrics`, read with `TASKCRAFT_METRICS_TOKEN` as a bearer token
//! (off without one): requests by route, method and status class with their durations, the
//! database pool, the e-mail and webhook outboxes, e-mail in, live connections and a few counts.
//! Each replica reports its own requests; the gauges read from the database are the same on all.

use std::collections::HashMap;
use std::fmt::Write;
use std::sync::Mutex;
use std::time::Instant;

use axum::extract::{MatchedPath, Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use sha2::{Digest, Sha256};

use crate::app::AppState;

/// Upper bounds (seconds) of the request duration histogram.
const BUCKETS: [f64; 11] = [0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0];

#[derive(Default)]
struct Series {
    count: u64,
    timed: u64,
    sum: f64,
    buckets: [u64; BUCKETS.len()],
}

/// Request counters of this replica.
pub struct Metrics {
    started: Instant,
    requests: Mutex<HashMap<(String, String, &'static str), Series>>,
}

impl Default for Metrics {
    fn default() -> Self {
        Metrics { started: Instant::now(), requests: Mutex::default() }
    }
}

fn class(status: u16) -> &'static str {
    match status / 100 {
        1 => "1xx",
        2 => "2xx",
        3 => "3xx",
        4 => "4xx",
        _ => "5xx",
    }
}

impl Metrics {
    /// One request; `secs` is left out for streams that stay open (live events).
    pub fn record(&self, method: &str, route: &str, status: u16, secs: Option<f64>) {
        let mut map = self.requests.lock().unwrap_or_else(|e| e.into_inner());
        let s = map.entry((method.to_string(), route.to_string(), class(status))).or_default();
        s.count += 1;
        if let Some(secs) = secs {
            s.timed += 1;
            s.sum += secs;
            for (i, b) in BUCKETS.iter().enumerate() {
                if secs <= *b {
                    s.buckets[i] += 1;
                }
            }
        }
    }
}

/// Middleware on the API routes: counts each request under its route pattern
/// (`/api/issues/{key}`, not every key).
pub async fn track(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let route = req.extensions().get::<MatchedPath>().map(|p| p.as_str().to_string());
    let method = req.method().to_string();
    let start = Instant::now();
    let res = next.run(req).await;
    if let Some(route) = route {
        let secs = (!route.ends_with("/events")).then(|| start.elapsed().as_secs_f64());
        state.metrics.record(&method, &route, res.status().as_u16(), secs);
    }
    res
}

fn label(v: &str) -> String {
    v.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n")
}

fn gauge(out: &mut String, name: &str, help: &str, samples: &[(String, f64)]) {
    let _ = writeln!(out, "# HELP {name} {help}\n# TYPE {name} gauge");
    for (labels, v) in samples {
        let _ = writeln!(out, "{name}{labels} {v}");
    }
}

/// `GET /api/metrics` in the Prometheus text format.
pub async fn scrape(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let Some(token) = state.config.metrics_token.as_deref().filter(|t| !t.is_empty()) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let given = headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()).and_then(|v| v.strip_prefix("Bearer ")).unwrap_or("");
    // compared as hashes, so the time taken says nothing about the token
    if Sha256::digest(given.as_bytes()) != Sha256::digest(token.as_bytes()) {
        return (StatusCode::UNAUTHORIZED, [(header::WWW_AUTHENTICATE, "Bearer")]).into_response();
    }
    let mut out = String::new();
    gauge(&mut out, "taskcraft_build_info", "Version of this replica.", &[(format!("{{version=\"{}\"}}", env!("CARGO_PKG_VERSION")), 1.0)]);
    gauge(&mut out, "taskcraft_uptime_seconds", "Seconds since this replica started.", &[(String::new(), state.metrics.started.elapsed().as_secs_f64())]);
    let shutting = state.shutting_down.load(std::sync::atomic::Ordering::Relaxed);
    gauge(&mut out, "taskcraft_shutting_down", "1 while this replica drains connections before stopping.", &[(String::new(), f64::from(u8::from(shutting)))]);

    {
        let map = state.metrics.requests.lock().unwrap_or_else(|e| e.into_inner());
        let mut keys: Vec<_> = map.keys().collect();
        keys.sort();
        let _ =
            writeln!(out, "# HELP taskcraft_http_requests_total API requests by method, route and status class.\n# TYPE taskcraft_http_requests_total counter");
        for k in &keys {
            let _ = writeln!(out, "taskcraft_http_requests_total{{method=\"{}\",route=\"{}\",status=\"{}\"}} {}", label(&k.0), label(&k.1), k.2, map[*k].count);
        }
        let _ = writeln!(
            out,
            "# HELP taskcraft_http_request_duration_seconds API request durations (live event streams left out).\n# TYPE taskcraft_http_request_duration_seconds histogram"
        );
        for k in &keys {
            let s = &map[*k];
            if s.timed == 0 {
                continue;
            }
            let l = format!("method=\"{}\",route=\"{}\",status=\"{}\"", label(&k.0), label(&k.1), k.2);
            for (i, b) in BUCKETS.iter().enumerate() {
                let _ = writeln!(out, "taskcraft_http_request_duration_seconds_bucket{{{l},le=\"{b}\"}} {}", s.buckets[i]);
            }
            let _ = writeln!(out, "taskcraft_http_request_duration_seconds_bucket{{{l},le=\"+Inf\"}} {}", s.timed);
            let _ = writeln!(out, "taskcraft_http_request_duration_seconds_sum{{{l}}} {}", s.sum);
            let _ = writeln!(out, "taskcraft_http_request_duration_seconds_count{{{l}}} {}", s.timed);
        }
    }

    let size = f64::from(state.db.size());
    let idle = state.db.num_idle() as f64;
    gauge(
        &mut out,
        "taskcraft_db_pool_connections",
        "Database connections of this replica.",
        &[("{state=\"idle\"}".into(), idle), ("{state=\"busy\"}".into(), (size - idle).max(0.0))],
    );
    gauge(&mut out, "taskcraft_live_connections", "Browsers following live updates on this replica.", &[(String::new(), state.hub.subscribers() as f64)]);

    // from the database: the same on every replica
    let mail: Option<(i64, i64, Option<f64>)> = sqlx::query_as(
        "SELECT count(*) FILTER (WHERE attempts < $1), count(*) FILTER (WHERE attempts >= $1),
                EXTRACT(EPOCH FROM now() - min(created_at) FILTER (WHERE attempts < $1))::float8
           FROM mail_outbox WHERE sent_at IS NULL",
    )
    .bind(crate::mail::MAX_ATTEMPTS)
    .fetch_one(&state.db)
    .await
    .ok();
    if let Some((pending, failed, oldest)) = mail {
        gauge(
            &mut out,
            "taskcraft_mail_outbox",
            "E-mails waiting to be sent, and ones given up on.",
            &[("{state=\"pending\"}".into(), pending as f64), ("{state=\"failed\"}".into(), failed as f64)],
        );
        gauge(&mut out, "taskcraft_mail_outbox_oldest_seconds", "Age of the oldest e-mail waiting to be sent.", &[(String::new(), oldest.unwrap_or(0.0))]);
    }
    let hooks: Option<(i64, Option<f64>)> =
        sqlx::query_as("SELECT count(*), EXTRACT(EPOCH FROM now() - min(created_at))::float8 FROM webhook_outbox").fetch_one(&state.db).await.ok();
    if let Some((pending, oldest)) = hooks {
        gauge(&mut out, "taskcraft_webhook_outbox", "Webhook deliveries waiting (being retried).", &[(String::new(), pending as f64)]);
        gauge(&mut out, "taskcraft_webhook_outbox_oldest_seconds", "Age of the oldest waiting webhook delivery.", &[(String::new(), oldest.unwrap_or(0.0))]);
    }
    let received: Vec<(String, i64)> =
        sqlx::query_as("SELECT status, count(*) FROM mail_in WHERE received_at > now() - interval '1 day' GROUP BY status ORDER BY status")
            .fetch_all(&state.db)
            .await
            .unwrap_or_default();
    gauge(
        &mut out,
        "taskcraft_mail_in_last_day",
        "E-mails received in the last 24 hours by outcome.",
        &received.iter().map(|(s, n)| (format!("{{status=\"{}\"}}", label(s)), *n as f64)).collect::<Vec<_>>(),
    );
    let counts: Option<(i64, i64, i64)> = sqlx::query_as(
        "SELECT (SELECT count(*) FROM issues WHERE deleted_at IS NULL), (SELECT count(*) FROM projects WHERE archived_at IS NULL),
                (SELECT count(*) FROM users WHERE status = 'active')",
    )
    .fetch_one(&state.db)
    .await
    .ok();
    if let Some((issues, projects, people)) = counts {
        gauge(&mut out, "taskcraft_issues", "Issues (not in the trash).", &[(String::new(), issues as f64)]);
        gauge(&mut out, "taskcraft_projects", "Projects (not archived).", &[(String::new(), projects as f64)]);
        gauge(&mut out, "taskcraft_active_people", "Active accounts.", &[(String::new(), people as f64)]);
    }
    ([(header::CONTENT_TYPE, "text/plain; version=0.0.4; charset=utf-8")], out).into_response()
}
