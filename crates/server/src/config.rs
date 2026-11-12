use std::net::SocketAddr;
use std::path::PathBuf;

use clap::Args;

/// Server configuration (CLI flags or `TASKCRAFT_*` environment variables).
#[derive(Debug, Clone, Args)]
pub struct Config {
    /// PostgreSQL connection URL.
    #[arg(long, env = "TASKCRAFT_DATABASE_URL", default_value = "postgres://taskcraft:taskcraft@127.0.0.1:55433/taskcraft")]
    pub database_url: String,

    /// Address to listen on.
    #[arg(long, env = "TASKCRAFT_BIND", default_value = "127.0.0.1:8788")]
    pub bind: SocketAddr,

    /// Public base URL (used in links to issues, e-mails and integrations).
    #[arg(long, env = "TASKCRAFT_BASE_URL", default_value = "http://127.0.0.1:8788")]
    pub base_url: String,

    /// Directory for local data (attachments when no S3 bucket is set, uploads).
    #[arg(long, env = "TASKCRAFT_DATA_DIR", default_value = "./data")]
    pub data_dir: PathBuf,

    /// Directory with the built frontend (`web/dist`). Served at `/`; when it holds no build,
    /// the copy embedded in the binary is served.
    #[arg(long, env = "TASKCRAFT_STATIC_DIR", default_value = "./web/dist")]
    pub static_dir: PathBuf,

    /// S3 bucket for attachments (local filesystem storage when unset).
    #[arg(long, env = "TASKCRAFT_S3_BUCKET")]
    pub s3_bucket: Option<String>,

    /// Custom S3 endpoint (MinIO, Ceph, R2, …).
    #[arg(long, env = "TASKCRAFT_S3_ENDPOINT")]
    pub s3_endpoint: Option<String>,

    /// S3 region (defaults to AWS_REGION / us-east-1).
    #[arg(long, env = "TASKCRAFT_S3_REGION")]
    pub s3_region: Option<String>,

    /// S3 access key id (defaults to AWS_ACCESS_KEY_ID).
    #[arg(long, env = "TASKCRAFT_S3_ACCESS_KEY_ID")]
    pub s3_access_key_id: Option<String>,

    /// S3 secret access key (defaults to AWS_SECRET_ACCESS_KEY).
    #[arg(long, env = "TASKCRAFT_S3_SECRET_ACCESS_KEY", hide_env_values = true)]
    pub s3_secret_access_key: Option<String>,

    /// Maximum attachment / import size in megabytes.
    #[arg(long, env = "TASKCRAFT_MAX_UPLOAD_MB", default_value_t = 256)]
    pub max_upload_mb: u64,

    /// Mark session cookies `Secure` (enable behind HTTPS).
    #[arg(long, env = "TASKCRAFT_SECURE_COOKIES", default_value_t = false)]
    pub secure_cookies: bool,

    /// Session lifetime in days.
    #[arg(long, env = "TASKCRAFT_SESSION_DAYS", default_value_t = 30)]
    pub session_days: i64,

    /// Log format: `text` or `json`.
    #[arg(long, env = "TASKCRAFT_LOG_FORMAT", default_value = "text")]
    pub log_format: String,

    /// Bearer token for Prometheus at `/api/metrics` (without one the endpoint is off).
    #[arg(long, env = "TASKCRAFT_METRICS_TOKEN", hide_env_values = true)]
    pub metrics_token: Option<String>,

    /// Allow webhooks to private / loopback addresses (off by default: SSRF protection).
    #[arg(long, env = "TASKCRAFT_WEBHOOK_ALLOW_PRIVATE", default_value_t = false)]
    pub webhook_allow_private: bool,

    /// Allow imports from other trackers' APIs on private / loopback addresses (a server in the
    /// same network; off by default: SSRF protection).
    #[arg(long, env = "TASKCRAFT_IMPORT_ALLOW_PRIVATE", default_value_t = false)]
    pub import_allow_private: bool,

    /// WikiCraft for documentation next to the work: linked pages with live titles, a project's
    /// wiki space, pages created from issues, sprints and releases. E.g. `https://wiki.example.com`.
    #[arg(long, env = "TASKCRAFT_WIKICRAFT_URL")]
    pub wikicraft_url: Option<String>,

    /// WikiCraft API token (`wct_…`) used when a person hasn't connected their own. Everyone
    /// then sees page titles this account can see: use an account with read access only to what
    /// all TaskCraft members may read.
    #[arg(long, env = "TASKCRAFT_WIKICRAFT_TOKEN", hide_env_values = true)]
    pub wikicraft_token: Option<String>,

    /// Single sign-on: OIDC / GitHub / LDAP / SAML providers, `TASKCRAFT_DISABLE_PASSWORD_LOGIN`.
    #[command(flatten)]
    pub sso: crate::sso::SsoConfig,

    /// Outgoing mail server: `smtps://user:pass@mail.example.com` (TLS, port 465),
    /// `smtp://user:pass@mail.example.com:587?tls=required` (STARTTLS) or `smtp://localhost:25`
    /// (unencrypted, local relays only). URL-encode special characters in the password.
    /// Enables e-mailed invitations, password resets and notification e-mails.
    #[arg(long, env = "TASKCRAFT_SMTP_URL", hide_env_values = true)]
    pub smtp_url: Option<String>,

    /// Sender of e-mails.
    #[arg(long, env = "TASKCRAFT_MAIL_FROM", default_value = "TaskCraft <taskcraft@localhost>")]
    pub mail_from: String,

    /// E-mail in: `TASKCRAFT_IMAP_URL` (a mailbox to read), `TASKCRAFT_MAIL_IN_TOKEN` (the HTTP
    /// endpoint for mail servers).
    #[command(flatten)]
    pub mail_in: crate::mail_in::MailInConfig,
}

impl Config {
    pub fn blob_dir(&self) -> PathBuf {
        self.data_dir.join("blobs")
    }

    pub fn upload_dir(&self) -> PathBuf {
        self.data_dir.join("uploads")
    }

    /// The base URL without a trailing slash.
    pub fn base(&self) -> &str {
        self.base_url.trim_end_matches('/')
    }

    pub fn issue_url(&self, key: &str) -> String {
        format!("{}/issues/{key}", self.base())
    }
}
