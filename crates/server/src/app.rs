use std::sync::Arc;

use object_store::ObjectStore;
use sqlx::PgPool;

use crate::config::Config;
use crate::events::Hub;

/// Shared application state.
#[derive(Clone)]
pub struct AppState {
    pub inner: Arc<Inner>,
}

pub struct Inner {
    pub db: PgPool,
    pub config: Config,
    /// attachments: S3-compatible storage or the local data directory
    pub blobs: Arc<dyn ObjectStore>,
    /// live updates for connected browsers (fed through Postgres LISTEN/NOTIFY, so every
    /// replica sees every change)
    pub hub: Hub,
    /// outgoing HTTP (WikiCraft, webhooks)
    pub http: reqwest::Client,
    /// outgoing e-mail (disabled without `TASKCRAFT_SMTP_URL`)
    pub mailer: crate::mail::Mailer,
    pub shutting_down: std::sync::atomic::AtomicBool,
    /// configured external sign-in providers (single sign-on)
    pub providers: Vec<crate::sso::Provider>,
    /// request counters for `/api/metrics`
    pub metrics: crate::metrics::Metrics,
}

impl std::ops::Deref for AppState {
    type Target = Inner;
    fn deref(&self) -> &Inner {
        &self.inner
    }
}

impl AppState {
    pub async fn new(config: Config) -> anyhow::Result<Self> {
        // invalid sign-in configuration stops the start (fail fast)
        let providers = crate::sso::load(&config.sso)?;
        // configuration mistakes surface before anything else happens
        let mailer = crate::mail::Mailer::new(&config)?;
        std::fs::create_dir_all(&config.data_dir)?;
        std::fs::create_dir_all(config.upload_dir())?;
        let db = connect(&config).await?;
        // several replicas may start at once: the migrator serializes itself with an advisory lock
        migrator().run(&db).await?;
        let blobs = blob_store(&config)?;
        let http = reqwest::Client::builder()
            .user_agent(concat!("TaskCraft/", env!("CARGO_PKG_VERSION")))
            .timeout(std::time::Duration::from_secs(15))
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        Ok(AppState {
            inner: Arc::new(Inner {
                db,
                config,
                blobs,
                hub: Hub::new(),
                http,
                mailer,
                shutting_down: Default::default(),
                providers,
                metrics: Default::default(),
            }),
        })
    }
}

/// The database pool (without running migrations).
///
/// JIT compilation is off: TaskCraft's queries are short, and for the larger ones Postgres
/// would spend more time compiling than it saves (over half a second for the portfolio with
/// 100 000 issues).
pub async fn connect(config: &Config) -> anyhow::Result<PgPool> {
    let options = <sqlx::postgres::PgConnectOptions as std::str::FromStr>::from_str(&config.database_url)?.options([("jit", "off")]);
    Ok(sqlx::postgres::PgPoolOptions::new().max_connections(20).acquire_timeout(std::time::Duration::from_secs(10)).connect_with(options).await?)
}

/// The schema migrations built into this binary.
pub fn migrator() -> sqlx::migrate::Migrator {
    sqlx::migrate!("./migrations")
}

/// Attachment storage: S3-compatible when a bucket is configured, else the data directory.
pub fn blob_store(config: &Config) -> anyhow::Result<Arc<dyn ObjectStore>> {
    Ok(match &config.s3_bucket {
        Some(bucket) => {
            let mut b = object_store::aws::AmazonS3Builder::from_env().with_bucket_name(bucket);
            if let Some(e) = &config.s3_endpoint {
                b = b.with_endpoint(e).with_allow_http(e.starts_with("http://")).with_virtual_hosted_style_request(false);
            }
            if let Some(r) = &config.s3_region {
                b = b.with_region(r);
            }
            if let Some(k) = &config.s3_access_key_id {
                b = b.with_access_key_id(k);
            }
            if let Some(s) = &config.s3_secret_access_key {
                b = b.with_secret_access_key(s);
            }
            Arc::new(b.build()?)
        }
        None => {
            std::fs::create_dir_all(config.blob_dir())?;
            Arc::new(object_store::local::LocalFileSystem::new_with_prefix(config.blob_dir())?)
        }
    })
}

pub fn new_id() -> uuid::Uuid {
    uuid::Uuid::now_v7()
}

/// Writes an audit log entry (best effort).
pub async fn audit(state: &AppState, workspace: uuid::Uuid, actor: Option<uuid::Uuid>, action: &str, target: Option<String>, data: serde_json::Value) {
    let _ = sqlx::query("INSERT INTO audit_log (id, workspace_id, actor_id, action, target, data) VALUES ($1, $2, $3, $4, $5, $6)")
        .bind(new_id())
        .bind(workspace)
        .bind(actor)
        .bind(action)
        .bind(target)
        .bind(data)
        .execute(&state.db)
        .await;
}
