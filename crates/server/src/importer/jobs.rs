//! Imports from the browser. From an XML site backup: an administrator uploads it (kept in
//! attachment storage for a day at most). From another tracker's API: an administrator connects
//! (address, token, projects) and the server reads the projects into a snapshot kept the same
//! way. Either way the server checks what the source holds in the background and shows it, the
//! administrator decides per project and about people, and the import runs in the background with
//! its progress and report stored, so the page can be left and reloaded. Tokens are used for the
//! request they come with (reading, or downloading attachments while importing) and never stored.

use std::collections::BTreeMap;
use std::io::{Read, Write};
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

use super::api::{ApiFiles, Connection, Kind};
use super::model::{Format, Model};
use super::{Choice, Options, People, Progress, ProgressHandle};
use crate::app::{AppState, audit, new_id};
use crate::auth::{SessionUser, User, require_admin};
use crate::error::{ApiError, ApiResult};
use crate::site_backup::{self, Backup, Source};

/// Uploaded backups and read snapshots are kept this long at most.
const KEEP_HOURS: i64 = 24;

const BACKUP: &str = "xml-site-backup";

/// Where the job's file is: the uploaded backup, or the snapshot read from an API.
fn file_key(id: Uuid, source: &str) -> ObjPath {
    if source == BACKUP { ObjPath::from(format!("imports/{id}.zip")) } else { ObjPath::from(format!("imports/{id}.json.gz")) }
}

const COLUMNS: &str =
    "id, source, connection, file_name, size, status, summary, options, progress, report, error, file_deleted, created_at, updated_at, finished_at";

#[derive(sqlx::FromRow)]
struct Job {
    id: Uuid,
    source: String,
    connection: Option<Value>,
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
            "id": self.id, "source": self.source, "connection": self.connection, "fileName": self.file_name, "size": self.size, "status": self.status,
            "summary": self.summary, "options": self.options, "progress": self.progress, "report": self.report, "error": self.error,
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
    let old: Vec<(Uuid, String)> = sqlx::query_as(
        "UPDATE backup_imports SET file_deleted = true WHERE NOT file_deleted AND status NOT IN ('checking', 'running')
            AND created_at < now() - make_interval(hours => $1) RETURNING id, source",
    )
    .bind(KEEP_HOURS as i32)
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();
    for (id, source) in old {
        let _ = state.blobs.delete(&file_key(id, &source)).await;
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

async fn ensure_idle(state: &AppState, user: &User) -> ApiResult<()> {
    let busy: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM backup_imports WHERE workspace_id = $1 AND status IN ('checking', 'running'))")
        .bind(user.workspace_id)
        .fetch_one(&state.db)
        .await?;
    if busy {
        return Err(ApiError::Conflict("another import is being checked or run: wait until it's done".into()));
    }
    Ok(())
}

/// Records the outcome of a check.
async fn checked(state: &AppState, id: Uuid, result: anyhow::Result<(Value, i64)>) {
    let _ = match result {
        Ok((summary, size)) => {
            sqlx::query("UPDATE backup_imports SET status = 'checked', summary = $2, size = $3, progress = NULL, updated_at = now() WHERE id = $1")
                .bind(id)
                .bind(summary)
                .bind(size)
                .execute(&state.db)
                .await
        }
        Err(e) => {
            tracing::warn!(error = %format!("{e:#}"), "import check failed");
            sqlx::query("UPDATE backup_imports SET status = 'failed', error = $2, progress = NULL, updated_at = now() WHERE id = $1")
                .bind(id)
                .bind(format!("{e:#}"))
                .execute(&state.db)
                .await
        }
    };
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
    ensure_idle(&state, &user).await?;
    let name: String = q.name.as_deref().unwrap_or("backup.zip").replace(['/', '\\', '\0'], "_").chars().take(200).collect();
    let id = new_id();
    let upload = state.blobs.put_multipart(&file_key(id, BACKUP)).await.map_err(|e| ApiError::Internal(anyhow::anyhow!("storage: {e}")))?;
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
        let result = check(&st, id, workspace).await.map(|s| (s, size as i64));
        checked(&st, id, result).await;
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
    let got = state.blobs.get(&file_key(id, BACKUP)).await.map_err(|e| anyhow::anyhow!("the uploaded file is gone ({e}): upload it again"))?;
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

async fn open(state: &AppState, id: Uuid, progress: &ProgressHandle, tz: chrono_tz::Tz) -> anyhow::Result<(Local, Source, Backup)> {
    super::set_progress(progress, "reading", None, 0, 0);
    let local = fetch(state, id).await?;
    let path = local.0.clone();
    let (source, backup) = tokio::task::spawn_blocking(move || -> anyhow::Result<(Source, Backup)> {
        let mut s = Source::open(&path, None)?;
        let b = site_backup::load(&mut s, tz)?;
        Ok((s, b))
    })
    .await??;
    Ok((local, source, backup))
}

async fn check(state: &AppState, id: Uuid, workspace: Uuid) -> anyhow::Result<Value> {
    let progress: ProgressHandle = Arc::default();
    with_heartbeat(state, id, progress.clone(), async {
        let (_local, source, backup) = open(state, id, &progress, chrono_tz::UTC).await?;
        let summary = site_backup::summarize(state, workspace, &source, &backup).await?;
        Ok(json!(summary))
    })
    .await
}

// ---- other trackers' APIs ----

/// The model as a gzipped JSON snapshot.
fn pack(m: &Model) -> anyhow::Result<Vec<u8>> {
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    serde_json::to_writer(&mut gz, m)?;
    Ok(gz.finish()?)
}

fn unpack(bytes: &[u8]) -> anyhow::Result<Model> {
    let mut json = Vec::new();
    flate2::read::GzDecoder::new(bytes).read_to_end(&mut json)?;
    Ok(serde_json::from_slice(&json)?)
}

/// Reads a source into a snapshot and checks it.
async fn check_api(state: &AppState, id: Uuid, workspace: Uuid, conn: &Connection) -> anyhow::Result<(Value, i64)> {
    let progress: ProgressHandle = Arc::default();
    with_heartbeat(state, id, progress.clone(), async {
        super::set_progress(&progress, "fetching", None, 0, 0);
        let http = conn.client(state.config.import_allow_private, progress.clone())?;
        let m = conn.fetch(&http, &progress).await?;
        let packed = pack(&m)?;
        let size = packed.len() as i64;
        state.blobs.put(&file_key(id, conn.source.name()), packed.into()).await.map_err(|e| anyhow::anyhow!("storage: {e}"))?;
        let summary = super::summarize(state, workspace, &m, &ApiFiles::new(&m, None)).await?;
        Ok((json!(summary), size))
    })
    .await
}

async fn load_snapshot(state: &AppState, id: Uuid, source: &str) -> anyhow::Result<Model> {
    let got = state.blobs.get(&file_key(id, source)).await.map_err(|e| anyhow::anyhow!("what was read is gone ({e}): connect again"))?;
    let bytes = got.bytes().await?;
    unpack(&bytes)
}

/// Projects to choose from: the token's own, or an owner's (user, organization, group).
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowseReq {
    #[serde(flatten)]
    conn: Connection,
    owner: Option<String>,
}

pub async fn browse(State(state): State<AppState>, SessionUser(user): SessionUser, Json(req): Json<BrowseReq>) -> ApiResult<Json<Value>> {
    admin(&user)?;
    let progress: ProgressHandle = Arc::default();
    let http = req.conn.client(state.config.import_allow_private, progress).map_err(|e| ApiError::bad(format!("{e:#}")))?;
    let list = tokio::time::timeout(Duration::from_secs(60), req.conn.browse(&http, req.owner.as_deref()))
        .await
        .map_err(|_| ApiError::bad("the source didn't answer within a minute"))?
        .map_err(|e| ApiError::bad(format!("{e:#}")))?;
    Ok(Json(json!({"projects": list})))
}

/// Connects to a source and reads the projects asked for in the background (the check).
pub async fn connect(State(state): State<AppState>, SessionUser(user): SessionUser, Json(conn): Json<Connection>) -> ApiResult<(StatusCode, Json<Value>)> {
    admin(&user)?;
    sweep(&state).await;
    let mut conn = conn;
    conn.projects = conn.projects.iter().map(|p| p.trim().trim_matches('/').to_string()).filter(|p| !p.is_empty()).collect();
    conn.projects.dedup();
    if conn.projects.is_empty() {
        return Err(ApiError::bad("choose at least one project to read"));
    }
    if conn.projects.len() > 50 {
        return Err(ApiError::bad("read at most 50 projects at once"));
    }
    // the address is checked before anything starts (private addresses, typos)
    conn.client(state.config.import_allow_private, Arc::default()).map_err(|e| ApiError::bad(format!("{e:#}")))?;
    ensure_idle(&state, &user).await?;
    let id = new_id();
    let url = conn.shown_url();
    let name: String = format!("{} · {}", conn.source.label(), conn.projects.join(", ")).chars().take(200).collect();
    let connection =
        json!({"baseUrl": url, "projects": conn.projects, "markup": conn.markup, "token": conn.token.as_deref().is_some_and(|t| !t.trim().is_empty())});
    sqlx::query(
        "INSERT INTO backup_imports (id, workspace_id, created_by, source, connection, file_name, size, status) VALUES ($1, $2, $3, $4, $5, $6, 0, 'checking')",
    )
    .bind(id)
    .bind(user.workspace_id)
    .bind(user.id)
    .bind(conn.source.name())
    .bind(&connection)
    .bind(&name)
    .execute(&state.db)
    .await?;
    audit(&state, user.workspace_id, Some(user.id), "import.connected", Some(name), json!({"source": conn.source.name(), "url": url})).await;
    let st = state.clone();
    let workspace = user.workspace_id;
    tokio::spawn(async move {
        let result = check_api(&st, id, workspace, &conn).await;
        drop(conn);
        checked(&st, id, result).await;
    });
    Ok((StatusCode::ACCEPTED, Json(job(&state, &user, id).await?.json())))
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
    /// API sources: the token again (attachments are downloaded while importing)
    token: Option<String>,
    /// API sources whose markup is a server setting: how texts are written
    markup: Option<Format>,
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
        return Err(ApiError::bad(if j.source == BACKUP {
            "the uploaded backup is gone (imports keep it for a day): upload it again"
        } else {
            "what was read is gone (imports keep it for a day): connect again"
        }));
    }
    if !matches!(j.status.as_str(), "checked" | "failed" | "done") {
        return Err(ApiError::Conflict(format!("the import is {}", j.status)));
    }
    if j.summary.is_none() {
        return Err(ApiError::bad(if j.source == BACKUP {
            "the backup couldn't be read: upload it again"
        } else {
            "the source couldn't be read: connect again"
        }));
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
    let api: Option<Connection> = if j.source == BACKUP {
        None
    } else {
        let kind: Kind = j.source.parse().map_err(ApiError::bad)?;
        let c = j.connection.clone().unwrap_or(Value::Null);
        Some(Connection {
            source: kind,
            base_url: c["baseUrl"].as_str().unwrap_or_default().to_string(),
            token: req.token.clone().filter(|t| !t.trim().is_empty()),
            projects: vec![],
            markup: None,
        })
    };
    if let Some(conn) = &api
        && j.connection.as_ref().is_some_and(|c| c["token"] == true)
        && conn.token.is_none()
        && !req.dry_run
    {
        return Err(ApiError::bad("enter the token again: attachments are downloaded while importing"));
    }
    let options = json!({"projects": req.projects, "people": req.people, "dryRun": req.dry_run, "timeZone": tz.name(), "markup": req.markup});
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
        markup: req.markup,
    };
    let st = state.clone();
    let u = user.clone();
    let source = j.source.clone();
    tokio::spawn(async move {
        let progress: ProgressHandle = Arc::default();
        let result = with_heartbeat(&st, id, progress.clone(), async {
            match &api {
                None => {
                    let (_local, mut source, backup) = open(&st, id, &progress, opts.time_zone).await?;
                    site_backup::run(&st, &u, &mut source, &backup, &opts, progress.clone()).await
                }
                Some(conn) => {
                    super::set_progress(&progress, "reading", None, 0, 0);
                    let m = load_snapshot(&st, id, &source).await?;
                    let http = if opts.dry_run { None } else { Some(conn.client(st.config.import_allow_private, progress.clone())?) };
                    let mut files = ApiFiles::new(&m, http);
                    super::run(&st, &u, &mut files, &m, &opts, progress.clone()).await
                }
            }
        })
        .await;
        drop(api);
        match result {
            Ok(report) => {
                // the backup or snapshot isn't needed any more once it's in
                let _ = sqlx::query(
                    "UPDATE backup_imports SET status = 'done', report = $2, progress = NULL, updated_at = now(), finished_at = now(), file_deleted = file_deleted OR $3
                      WHERE id = $1",
                )
                .bind(id)
                .bind(json!(report))
                .bind(!opts.dry_run)
                .execute(&st.db)
                .await;
                if !opts.dry_run {
                    let _ = st.blobs.delete(&file_key(id, &source)).await;
                }
            }
            Err(e) => {
                tracing::warn!(error = %format!("{e:#}"), "import failed");
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

/// Discards an upload or a snapshot (and its file).
pub async fn discard(State(state): State<AppState>, SessionUser(user): SessionUser, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    admin(&user)?;
    let j = job(&state, &user, id).await?;
    if matches!(j.status.as_str(), "checking" | "running") {
        return Err(ApiError::Conflict("wait until it's done".into()));
    }
    let _ = state.blobs.delete(&file_key(id, &j.source)).await;
    sqlx::query("DELETE FROM backup_imports WHERE id = $1").bind(id).execute(&state.db).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// For the command line: reads a source and imports it in one go (progress on standard error).
pub async fn import_api(state: &AppState, user: &User, conn: &Connection, opts: &Options) -> anyhow::Result<super::Report> {
    let progress: ProgressHandle = Arc::default();
    let p = progress.clone();
    let printer = tokio::spawn(async move {
        let mut last = String::new();
        loop {
            tokio::time::sleep(Duration::from_secs(3)).await;
            let now: Progress = p.lock().map(|g| g.clone()).unwrap_or_default();
            let line = match now.phase.as_str() {
                "waiting" => format!("waiting for the rate limit until {}", now.until.map(|u| u.format("%H:%M:%S UTC").to_string()).unwrap_or_default()),
                "" => continue,
                phase => format!("{phase} {} {}/{}", now.project.unwrap_or_default(), now.done, now.total),
            };
            if line != last {
                let mut err = std::io::stderr();
                let _ = writeln!(err, "{line}");
                last = line;
            }
        }
    });
    let result = async {
        let http = conn.client(state.config.import_allow_private, progress.clone())?;
        let m = conn.fetch(&http, &progress).await?;
        let mut files = ApiFiles::new(&m, Some(http));
        for w in &m.notes {
            eprintln!("note: {}", w.message);
        }
        super::run(state, user, &mut files, &m, opts, progress.clone()).await
    }
    .await;
    printer.abort();
    result
}
