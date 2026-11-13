//! What may be uploaded: the workspace's blocked file types and size limit
//! (`settings.uploads`), and a virus scan with ClamAV when `TASKCRAFT_CLAMAV_ADDRESS` is set
//! (clamd's INSTREAM over TCP). Without a scanner answer files are refused, unless
//! `TASKCRAFT_CLAMAV_FAIL_OPEN` says to store them anyway.
//!
//! Applies to files on issues from the browser, the portal and e-mail; imports of other
//! trackers' data are the admin's own and aren't scanned.

use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use uuid::Uuid;

use crate::app::{AppState, audit};
use crate::error::{ApiError, ApiResult};

/// Checks and tidies `settings.uploads`: `{"blockedTypes": ["exe", …], "maxMb": n}`.
pub fn check_settings(v: &Value, server_max_mb: u64) -> ApiResult<Value> {
    let o = v.as_object().ok_or_else(|| ApiError::bad("uploads must be an object"))?;
    let mut out = serde_json::Map::new();
    for (k, val) in o {
        match k.as_str() {
            "blockedTypes" => {
                let mut types: Vec<String> = Vec::new();
                for t in val.as_array().ok_or_else(|| ApiError::bad("blockedTypes must be a list of file endings"))? {
                    let t = t.as_str().unwrap_or("").trim().trim_start_matches('.').to_lowercase();
                    if t.is_empty() || t.len() > 20 || !t.chars().all(|c| c.is_ascii_alphanumeric()) {
                        return Err(ApiError::bad("file endings look like exe or docm"));
                    }
                    if !types.contains(&t) {
                        types.push(t);
                    }
                }
                out.insert(k.clone(), json!(types));
            }
            "maxMb" => {
                if !val.is_null() {
                    let mb = val
                        .as_u64()
                        .filter(|m| (1..=server_max_mb).contains(m))
                        .ok_or_else(|| ApiError::bad(format!("maxMb is 1 to {server_max_mb} (TASKCRAFT_MAX_UPLOAD_MB)")))?;
                    out.insert(k.clone(), json!(mb));
                }
            }
            other => return Err(ApiError::bad(format!("unknown upload setting {other}"))),
        }
    }
    Ok(Value::Object(out))
}

/// Whether a file may be stored in the workspace: its type, its size and (with a scanner) its
/// content. Infected files are recorded in the audit log.
pub async fn check(state: &AppState, workspace: Uuid, name: &str, data: &[u8]) -> ApiResult<()> {
    let settings: Value =
        sqlx::query_scalar("SELECT COALESCE(settings->'uploads', '{}') FROM workspaces WHERE id = $1").bind(workspace).fetch_one(&state.db).await?;
    let ending = name.rsplit_once('.').map(|(_, e)| e.to_lowercase()).unwrap_or_default();
    if !ending.is_empty() && settings["blockedTypes"].as_array().into_iter().flatten().any(|t| t.as_str() == Some(ending.as_str())) {
        return Err(ApiError::bad(format!("{name}: .{ending} files can't be uploaded here")));
    }
    if let Some(mb) = settings["maxMb"].as_u64()
        && data.len() as u64 > mb * 1024 * 1024
    {
        return Err(ApiError::bad(format!("{name} is larger than {mb} MB, the most files may be here")));
    }
    let Some(address) = state.config.clamav_address.as_deref().filter(|a| !a.is_empty()) else { return Ok(()) };
    match scan(address, data).await {
        Ok(None) => Ok(()),
        Ok(Some(signature)) => {
            audit(state, workspace, None, "file.infected", Some(name.to_string()), json!({"signature": signature})).await;
            Err(ApiError::bad(format!("{name} looks infected ({signature}) and wasn't stored")))
        }
        Err(e) if state.config.clamav_fail_open => {
            tracing::warn!(error = %e, "virus scanner unavailable: storing the file anyway (TASKCRAFT_CLAMAV_FAIL_OPEN)");
            Ok(())
        }
        Err(e) => {
            tracing::warn!(error = %e, "virus scanner unavailable: file refused");
            Err(ApiError::bad("the virus scanner can't be reached, so files can't be stored right now; try again later"))
        }
    }
}

/// Asks clamd about some bytes: None when clean, else the signature it found.
pub async fn scan(address: &str, data: &[u8]) -> anyhow::Result<Option<String>> {
    let work = async {
        let mut s = tokio::time::timeout(Duration::from_secs(10), tokio::net::TcpStream::connect(address))
            .await
            .map_err(|_| anyhow::anyhow!("no connection to clamd at {address} within 10 seconds"))??;
        s.write_all(b"zINSTREAM\0").await?;
        for chunk in data.chunks(64 * 1024) {
            s.write_all(&(chunk.len() as u32).to_be_bytes()).await?;
            s.write_all(chunk).await?;
        }
        s.write_all(&0u32.to_be_bytes()).await?;
        let mut answer = Vec::new();
        s.read_to_end(&mut answer).await?;
        let text = String::from_utf8_lossy(&answer).trim_end_matches(['\0', '\n']).to_string();
        // "stream: OK", "stream: Eicar-Signature FOUND", "INSTREAM size limit exceeded. ERROR"
        if text.ends_with("OK") {
            Ok(None)
        } else if let Some(sig) = text.strip_suffix(" FOUND") {
            Ok(Some(sig.trim_start_matches("stream:").trim().to_string()))
        } else {
            anyhow::bail!("unexpected answer from clamd: {text}")
        }
    };
    tokio::time::timeout(Duration::from_secs(60), work).await.map_err(|_| anyhow::anyhow!("clamd didn't answer within a minute"))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_are_checked() {
        assert_eq!(
            check_settings(&json!({"blockedTypes": [".EXE", "bat", "exe"], "maxMb": 10}), 25).unwrap(),
            json!({"blockedTypes": ["exe", "bat"], "maxMb": 10})
        );
        assert!(check_settings(&json!({"maxMb": 50}), 25).is_err(), "not above the server's limit");
        assert!(check_settings(&json!({"blockedTypes": ["../x"]}), 25).is_err());
        assert!(check_settings(&json!({"colour": 1}), 25).is_err());
    }
}
