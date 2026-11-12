//! Imports from the browser: an administrator uploads the backup (kept in attachment storage
//! for a day at most), the server checks it in the background and shows what it holds, the
//! administrator decides per project and about people, and the import runs in the background
//! with its progress and report stored, so the page can be left and reloaded.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use futures::StreamExt;
use object_store::path::Path as ObjPath;
use object_store::{ObjectStoreExt, WriteMultipart};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::AsyncWriteExt;
use uuid::Uuid;

use super::{Choice, Options, People, Progress, ProgressHandle, Source};
use crate::app::{AppState, audit, new_id};
use crate::auth::{SessionUser, User, require_admin};
use crate::error::{ApiError, ApiResult};

/// Uploaded backups are kept this long at most.
const KEEP_HOURS: i64 = 24;

fn file_key(id: Uuid) -> ObjPath {
    ObjPath::from(format!("imports/{id}.zip"))
}

const COLUMNS: &str = "id, file_name, size, status, summary, options, progress, report, error, file_deleted, created_at, updated_at, finished_at";

#[derive(sqlx::FromRow)]
struct Job {
    id: Uuid,
    file_name: String,
    size: i64,
    status: String,
    summary: Option<Value>,
    options: Option<Value>,
    progress: Option<Value>,
    report: Option<Value>,
    error: Option<String>,
    file_deleted: bool,
    created_at: chrono::DateTime<chrono::Utc>,
    updated_at: chrono::DateTime<chrono::Utc>,
    finished_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl Job {
    fn json(&self) -> Value {
        json!({
            "id": self.id, "fileName": self.file_name, "size": self.size, "status": self.status, "summary": self.summary,
            "options": self.options, "progress": self.progress, "report": self.report, "error": self.error,
            "fileKept": !self.file_deleted, "createdAt": self.created_at, "updatedAt": self.updated_at, "finishedAt": self.finished_at,
            "expiresAt": self.created_at + chrono::Duration::hours(KEEP_HOURS),
        })
    }
}

async fn job(state: &AppState, user: &User, id: Uuid) -> ApiResult<Job> {
    sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT {COLUMNS} FROM backup_imports WHERE id = $1 AND workspace_id = $2")))
        .bind(id)
        .bind(user.workspace_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(ApiError::NotFound("import"))
}

/// Imports that stopped reporting (the server restarted while checking or importing) fail;
/// files older than a day are removed.
pub async fn sweep(state: &AppState) {
    let _ = sqlx::query(
        "UPDATE backup_imports SET status = 'failed', error = 'interrupted: the server stopped while this was running. Start it again.', updated_at = now()
          WHERE status IN ('checking', 'running') AND updated_at < now() - interval '2 minutes'",
    )
    .execute(&state.db)
    .await;
    let old: Vec<Uuid> = sqlx::query_scalar(
        "UPDATE backup_imports SET file_deleted = true WHERE NOT file_deleted AND status NOT IN ('checking', 'running')
            AND created_at < now() - make_interval(hours => $1) RETURNING id",
    )
    .bind(KEEP_HOURS as i32)
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();
    for id in old {
        let _ = state.blobs.delete(&file_key(id)).await;
    }
}

/// Clears out old uploads every hour.
pub fn spawn_cleanup(state: &AppState) {
    let state = state.clone();
    tokio::spawn(async move {
        loop {
            sweep(&state).await;
            tokio::time::sleep(Duration::from_secs(3600)).await;
        }
    });
}

fn admin(user: &User) -> ApiResult<()> {
    require_admin(user)
}

pub async fn list(State(state): State<AppState>, SessionUser(user): SessionUser) -> ApiResult<Json<Value>> {
    admin(&user)?;
    sweep(&state).await;
    let rows: Vec<Job> =
        sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT {COLUMNS} FROM backup_imports WHERE workspace_id = $1 ORDER BY created_at DESC LIMIT 20")))
            .bind(user.workspace_id)
            .fetch_all(&state.db)
            .await?;
    Ok(Json(json!({
        "imports": rows.iter().map(Job::json).collect::<Vec<_>>(),
        "maxUploadMb": state.config.max_upload_mb,
    })))
}

pub async fn get(State(state): State<AppState>, SessionUser(user): SessionUser, Path(id): Path<Uuid>) -> ApiResult<Json<Value>> {
    admin(&user)?;
    sweep(&state).await;
    Ok(Json(job(&state, &user, id).await?.json()))
}

#[derive(Deserialize)]
pub struct UploadQuery {
    name: Option<String>,
}

/// The backup itself as the request body (`application/zip`); answers at once while the
/// server checks it in the background.
pub async fn upload(
    State(state): State<AppState>,
    SessionUser(user): SessionUser,
    Query(q): Query<UploadQuery>,
    headers: HeaderMap,
    body: Body,
) -> ApiResult<(StatusCode, Json<Value>)> {
    admin(&user)?;
    sweep(&state).await;
    let max = state.config.max_upload_mb.saturating_mul(1024 * 1024);
    let announced = headers.get(header::CONTENT_LENGTH).and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<u64>().ok());
    if announced.is_some_and(|n| n > max) {
        return Err(ApiError::TooLarge);
    }
    let busy: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM backup_imports WHERE workspace_id = $1 AND status IN ('checking', 'running'))")
        .bind(user.workspace_id)
        .fetch_one(&state.db)
        .await?;
    if busy {
        return Err(ApiError::Conflict("another backup is being checked or imported: wait until it's done".into()));
    }
    let name: String = q.name.as_deref().unwrap_or("backup.zip").replace(['/', '\\', '\0'], "_").chars().take(200).collect();
    let id = new_id();
    let upload = state.blobs.put_multipart(&file_key(id)).await.map_err(|e| ApiError::Internal(anyhow::anyhow!("storage: {e}")))?;
    let mut w = WriteMultipart::new(upload);
    let mut size: u64 = 0;
    let mut stream = body.into_data_stream();
    let mut head: Vec<u8> = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = match chunk {
            Ok(c) => c,
            Err(e) => {
                let _ = w.abort().await;
                return Err(ApiError::bad(format!("upload: {e}")));
            }
        };
        size += chunk.len() as u64;
        if size > max {
            let _ = w.abort().await;
            return Err(ApiError::TooLarge);
        }
        if head.len() < 4 {
            head.extend(chunk.iter().take(4 - head.len()));
        }
        if let Err(e) = w.wait_for_capacity(4).await {
            return Err(ApiError::Internal(anyhow::anyhow!("storage: {e}")));
        }
        w.write(&chunk);
    }
    if head != b"PK\x03\x04" {
        let _ = w.abort().await;
        return Err(ApiError::bad("that's not a .zip file: upload the XML site backup as it was downloaded (a .zip)"));
    }
    w.finish().await.map_err(|e| ApiError::Internal(anyhow::anyhow!("storage: {e}")))?;
    sqlx::query("INSERT INTO backup_imports (id, workspace_id, created_by, file_name, size, status) VALUES ($1, $2, $3, $4, $5, 'checking')")
        .bind(id)
        .bind(user.workspace_id)
        .bind(user.id)
        .bind(&name)
        .bind(size as i64)
        .execute(&state.db)
        .await?;
    audit(&state, user.workspace_id, Some(user.id), "backup.uploaded", Some(name.clone()), json!({"size": size})).await;
    let st = state.clone();
    let workspace = user.workspace_id;
    tokio::spawn(async move {
        let result = check(&st, id, workspace).await;
        let _ = match result {
            Ok(summary) => {
                sqlx::query("UPDATE backup_imports SET status = 'checked', summary = $2, progress = NULL, updated_at = now() WHERE id = $1")
                    .bind(id)
                    .bind(summary)
                    .execute(&st.db)
                    .await
            }
            Err(e) => {
                tracing::warn!(error = ?e, "backup check failed");
                sqlx::query("UPDATE backup_imports SET status = 'failed', error = $2, progress = NULL, updated_at = now() WHERE id = $1")
                    .bind(id)
                    .bind(format!("{e:#}"))
                    .execute(&st.db)
                    .await
            }
        };
    });
    Ok((StatusCode::ACCEPTED, Json(job(&state, &user, id).await?.json())))
}

/// The uploaded file on local disk (zip archives are read with random access).
struct Local(std::path::PathBuf);

impl Drop for Local {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

async fn fetch(state: &AppState, id: Uuid) -> anyhow::Result<Local> {
    let dir = state.config.data_dir.join("tmp");
    tokio::fs::create_dir_all(&dir).await?;
    let local = Local(dir.join(format!("import-{id}-{}.zip", new_id().simple())));
    let got = state.blobs.get(&file_key(id)).await.map_err(|e| anyhow::anyhow!("the uploaded file is gone ({e}): upload it again"))?;
    let mut f = tokio::fs::File::create(&local.0).await?;
    let mut stream = got.into_stream();
    while let Some(chunk) = stream.next().await {
        f.write_all(&chunk?).await?;
    }
    f.flush().await?;
    Ok(local)
}

/// Keeps the job's progress and liveness up to date while `work` runs.
async fn with_heartbeat<T>(state: &AppState, id: Uuid, progress: ProgressHandle, work: impl std::future::Future<Output = T>) -> T {
    let st = state.clone();
    let p = progress.clone();
    let beat = tokio::spawn(async move {
        loop {
            let now: Progress = p.lock().map(|g| g.clone()).unwrap_or_default();
            let _ = sqlx::query("UPDATE backup_imports SET progress = $2, updated_at = now() WHERE id = $1").bind(id).bind(json!(now)).execute(&st.db).await;
            tokio::time::sleep(Duration::from_millis(700)).await;
        }
    });
    let out = work.await;
    beat.abort();
    out
}

async fn open(state: &AppState, id: Uuid, progress: &ProgressHandle, tz: chrono_tz::Tz) -> anyhow::Result<(Local, Source, super::Backup)> {
    super::set_progress(progress, "reading", None, 0, 0);
    let local = fetch(state, id).await?;
    let path = local.0.clone();
    let (source, backup) = tokio::task::spawn_blocking(move || -> anyhow::Result<(Source, super::Backup)> {
        let mut s = Source::open(&path, None)?;
        let b = super::load(&mut s, tz)?;
        Ok((s, b))
    })
    .await??;
    Ok((local, source, backup))
}

async fn check(state: &AppState, id: Uuid, workspace: Uuid) -> anyhow::Result<Value> {
    let progress: ProgressHandle = Arc::default();
    with_heartbeat(state, id, progress.clone(), async {
        let (_local, source, backup) = open(state, id, &progress, chrono_tz::UTC).await?;
        let summary = super::summarize(state, workspace, &source, &backup).await?;
        Ok(json!(summary))
    })
    .await
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunReq {
    /// per source project key
    #[serde(default)]
    projects: BTreeMap<String, Choice>,
    #[serde(default)]
    people: People,
    #[serde(default)]
    dry_run: bool,
    /// the time zone of the backup's times (default UTC)
    time_zone: Option<String>,
}

/// Starts the import with the administrator's choices.
pub async fn start(
    State(state): State<AppState>,
    SessionUser(user): SessionUser,
    Path(id): Path<Uuid>,
    Json(req): Json<RunReq>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    admin(&user)?;
    sweep(&state).await;
    let j = job(&state, &user, id).await?;
    if j.file_deleted {
        return Err(ApiError::bad("the uploaded backup is gone (imports keep it for a day): upload it again"));
    }
    if !matches!(j.status.as_str(), "checked" | "failed" | "done") {
        return Err(ApiError::Conflict(format!("the import is {}", j.status)));
    }
    if j.summary.is_none() {
        return Err(ApiError::bad("the backup couldn't be read: upload it again"));
    }
    let tz: chrono_tz::Tz = match req.time_zone.as_deref().filter(|z| !z.is_empty()) {
        Some(z) => z.parse().map_err(|_| ApiError::bad(format!("unknown time zone {z}")))?,
        None => chrono_tz::UTC,
    };
    for (k, c) in &req.projects {
        match c {
            Choice::Auto { key } | Choice::New { key } | Choice::Into { key } if crate::routes::projects::valid_key(key).is_err() => {
                return Err(ApiError::bad(format!("{key} can't be a project key (for {k}): 2–10 letters or digits, starting with a letter")));
            }
            _ => {}
        }
    }
    if req.projects.values().all(|c| *c == Choice::Skip) && !req.projects.is_empty() {
        return Err(ApiError::bad("choose at least one project to import"));
    }
    let options = json!({"projects": req.projects, "people": req.people, "dryRun": req.dry_run, "timeZone": tz.name()});
    let started = sqlx::query(
        "UPDATE backup_imports SET status = 'running', options = $2, report = NULL, error = NULL, progress = NULL, updated_at = now(), finished_at = NULL
          WHERE id = $1 AND status IN ('checked', 'failed', 'done')",
    )
    .bind(id)
    .bind(&options)
    .execute(&state.db)
    .await?
    .rows_affected();
    if started == 0 {
        return Err(ApiError::Conflict("the import has started already".into()));
    }
    let opts = Options {
        dry_run: req.dry_run,
        people: req.people,
        choices: req.projects.into_iter().map(|(k, c)| (k.to_uppercase(), c)).collect(),
        only: vec![],
        template: None,
        time_zone: tz,
    };
    let st = state.clone();
    let u = user.clone();
    tokio::spawn(async move {
        let progress: ProgressHandle = Arc::default();
        let result = with_heartbeat(&st, id, progress.clone(), async {
            let (_local, mut source, backup) = open(&st, id, &progress, opts.time_zone).await?;
            super::run(&st, &u, &mut source, &backup, &opts, progress.clone()).await
        })
        .await;
        match result {
            Ok(report) => {
                let _ = sqlx::query(
                    "UPDATE backup_imports SET status = 'done', report = $2, progress = NULL, updated_at = now(), finished_at = now() WHERE id = $1",
                )
                .bind(id)
                .bind(json!(report))
                .execute(&st.db)
                .await;
                // the backup isn't needed any more once it's in
                if !opts.dry_run {
                    let _ = st.blobs.delete(&file_key(id)).await;
                    let _ = sqlx::query("UPDATE backup_imports SET file_deleted = true WHERE id = $1").bind(id).execute(&st.db).await;
                }
            }
            Err(e) => {
                tracing::warn!(error = ?e, "backup import failed");
                let _ = sqlx::query(
                    "UPDATE backup_imports SET status = 'failed', error = $2, progress = NULL, updated_at = now(), finished_at = now() WHERE id = $1",
                )
                .bind(id)
                .bind(format!("{e:#}"))
                .execute(&st.db)
                .await;
            }
        }
    });
    Ok((StatusCode::ACCEPTED, Json(job(&state, &user, id).await?.json())))
}

/// Discards an upload (and its file).
pub async fn discard(State(state): State<AppState>, SessionUser(user): SessionUser, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    admin(&user)?;
    let j = job(&state, &user, id).await?;
    if matches!(j.status.as_str(), "checking" | "running") {
        return Err(ApiError::Conflict("wait until it's done".into()));
    }
    let _ = state.blobs.delete(&file_key(id)).await;
    sqlx::query("DELETE FROM backup_imports WHERE id = $1").bind(id).execute(&state.db).await?;
    Ok(StatusCode::NO_CONTENT)
}
