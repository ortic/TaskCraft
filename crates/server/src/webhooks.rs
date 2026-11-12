//! Outgoing webhooks: JSON `POST`s for issue, comment and sprint events, signed with
//! HMAC-SHA256 (`X-TaskCraft-Signature: sha256=<hex>`), retried with backoff from a Postgres
//! outbox (any replica delivers). Private and loopback targets are refused unless allowed.

use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use hmac::{Hmac, KeyInit, Mac};
use reqwest::Url;
use serde_json::{Value, json};
use sha2::Sha256;
use uuid::Uuid;

use crate::app::{AppState, new_id};

pub const EVENTS: &[&str] = &["issue.created", "issue.updated", "issue.deleted", "comment.created", "sprint.started", "sprint.completed", "version.released"];

/// Queues `event` for the workspace's matching webhooks (best effort).
pub async fn enqueue(state: &AppState, workspace: Uuid, project: Option<Uuid>, event: &str, data: Value) {
    let hooks: Vec<Uuid> = sqlx::query_scalar(
        "SELECT id FROM webhooks WHERE workspace_id = $1 AND active AND ($2 = ANY(events) OR '*' = ANY(events) OR cardinality(events) = 0)
           AND (project_id IS NULL OR project_id = $3)",
    )
    .bind(workspace)
    .bind(event)
    .bind(project)
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();
    for h in hooks {
        let payload = json!({"event": event, "timestamp": chrono::Utc::now(), "webhookId": h, "deliveryId": new_id(), "data": data});
        let _ = sqlx::query("INSERT INTO webhook_outbox (id, webhook_id, payload) VALUES ($1, $2, $3)").bind(new_id()).bind(h).bind(payload).execute(&state.db).await;
    }
}

pub fn sign(secret: &str, body: &[u8]) -> String {
    let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(secret.as_bytes()).expect("any key length");
    mac.update(body);
    format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
}

pub fn is_private(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_multicast()
                || (o[0] == 100 && (64..128).contains(&o[1]))
                || o[0] == 0
        }
        IpAddr::V6(v6) => {
            let s = v6.segments();
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (s[0] & 0xfe00) == 0xfc00
                || (s[0] & 0xffc0) == 0xfe80
                || v6.to_ipv4_mapped().is_some_and(|v4| is_private(IpAddr::V4(v4)))
        }
    }
}

pub fn validate_url(url: &str) -> Result<Url, String> {
    let u = Url::parse(url.trim()).map_err(|e| format!("invalid URL: {e}"))?;
    if !matches!(u.scheme(), "http" | "https") {
        return Err("webhook URLs must use http or https".into());
    }
    if u.host_str().is_none_or(|h| h.is_empty()) {
        return Err("the URL needs a host".into());
    }
    if !u.username().is_empty() || u.password().is_some() {
        return Err("put credentials in the secret, not the URL".into());
    }
    Ok(u)
}

async fn resolve(u: &Url, allow_private: bool) -> Result<SocketAddr, String> {
    let host = u.host_str().unwrap_or_default().trim_start_matches('[').trim_end_matches(']').to_string();
    let port = u.port_or_known_default().unwrap_or(443);
    let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), port)).await.map_err(|e| format!("can't resolve {host}: {e}"))?.collect();
    let first = *addrs.first().ok_or_else(|| format!("{host} has no address"))?;
    if !allow_private && let Some(bad) = addrs.iter().find(|a| is_private(a.ip())) {
        return Err(format!("{host} resolves to a private address ({}); set TASKCRAFT_WEBHOOK_ALLOW_PRIVATE=true for internal targets", bad.ip()));
    }
    Ok(first)
}

/// Sends one delivery; Ok(status) or Err(reason).
pub async fn deliver(state: &AppState, url: &str, secret: &str, payload: &Value) -> Result<u16, String> {
    let u = validate_url(url)?;
    let addr = resolve(&u, state.config.webhook_allow_private).await?;
    let host = u.host_str().unwrap_or_default().to_string();
    // pinned to the checked address: a second DNS answer can't redirect the request
    let client = reqwest::Client::builder()
        .resolve(&host, addr)
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(10))
        .user_agent(concat!("TaskCraft-Webhooks/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| e.to_string())?;
    let body = serde_json::to_vec(payload).map_err(|e| e.to_string())?;
    let res = client
        .post(u)
        .header("content-type", "application/json")
        .header("x-taskcraft-event", payload["event"].as_str().unwrap_or(""))
        .header("x-taskcraft-signature", sign(secret, &body))
        .body(body)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let status = res.status().as_u16();
    if res.status().is_success() { Ok(status) } else { Err(format!("HTTP {status}")) }
}

/// Delivers queued events (every replica runs this; rows are claimed with SKIP LOCKED).
pub fn spawn_worker(state: &AppState) {
    let state = state.clone();
    tokio::spawn(async move {
        loop {
            if let Err(e) = tick(&state).await {
                tracing::warn!(error = %e, "webhook worker");
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    });
}

async fn tick(state: &AppState) -> anyhow::Result<()> {
    for _ in 0..20 {
        let mut tx = state.db.begin().await?;
        let due: Option<(Uuid, Uuid, Value, i32, String, String)> = sqlx::query_as(
            "SELECT o.id, o.webhook_id, o.payload, o.attempts, w.url, w.secret FROM webhook_outbox o JOIN webhooks w ON w.id = o.webhook_id
              WHERE o.next_at <= now() ORDER BY o.next_at LIMIT 1 FOR UPDATE OF o SKIP LOCKED",
        )
        .fetch_optional(&mut *tx)
        .await?;
        let Some((id, hook, payload, attempts, url, secret)) = due else { return Ok(()) };
        let result = deliver(state, &url, &secret, &payload).await;
        match &result {
            Ok(_) => {
                sqlx::query("DELETE FROM webhook_outbox WHERE id = $1").bind(id).execute(&mut *tx).await?;
            }
            Err(_) if attempts >= 7 => {
                sqlx::query("DELETE FROM webhook_outbox WHERE id = $1").bind(id).execute(&mut *tx).await?;
            }
            Err(_) => {
                // 30 s, 2 min, 8 min, … about 8 hours in all
                let wait = 30i64 * 4i64.pow(attempts.clamp(0, 6) as u32).min(4 * 3600);
                sqlx::query("UPDATE webhook_outbox SET attempts = attempts + 1, next_at = now() + make_interval(secs => $2) WHERE id = $1")
                    .bind(id)
                    .bind(wait as f64)
                    .execute(&mut *tx)
                    .await?;
            }
        }
        sqlx::query("UPDATE webhooks SET last_status = $2, last_error = $3, last_sent_at = now() WHERE id = $1")
            .bind(hook)
            .bind(result.as_ref().ok().map(|s| *s as i32))
            .bind(result.as_ref().err())
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_and_addresses() {
        // RFC 4231 test case 2
        assert_eq!(sign("Jefe", b"what do ya want for nothing?"), "sha256=5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843");
        assert!(is_private("127.0.0.1".parse().unwrap()));
        assert!(is_private("10.1.2.3".parse().unwrap()));
        assert!(is_private("::ffff:192.168.1.1".parse().unwrap()));
        assert!(!is_private("1.1.1.1".parse().unwrap()));
        assert!(validate_url("ftp://x").is_err());
        assert!(validate_url("https://u:p@x.com").is_err());
        assert!(validate_url("https://hooks.example.com/x").is_ok());
    }
}
