//! Test harness: a fresh database per test and the full app on an ephemeral port.

#![allow(dead_code)]

use serde_json::{Value, json};
use taskcraft::app::AppState;
use taskcraft::config::Config;

pub const ADMIN_URL: &str = "postgres://taskcraft:taskcraft@127.0.0.1:55433/postgres";

pub struct TestApp {
    pub base: String,
    pub state: AppState,
    pub _data: tempfile::TempDir,
}

/// Drops test databases older than two hours (younger ones may belong to running tests).
async fn drop_stale(admin: &sqlx::PgPool, now: u64) {
    static DONE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if DONE.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return;
    }
    let names: Vec<String> =
        sqlx::query_scalar("SELECT datname FROM pg_database WHERE datname LIKE 'taskcraft\\_test\\_%'").fetch_all(admin).await.unwrap_or_default();
    for name in names {
        let stale = name["taskcraft_test_".len()..].split_once('_').and_then(|(s, _)| s.parse::<u64>().ok()).is_none_or(|t| t + 2 * 3600 < now);
        if stale && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
            let _ = sqlx::query(sqlx::AssertSqlSafe(format!("DROP DATABASE IF EXISTS {name} WITH (FORCE)"))).execute(admin).await;
        }
    }
}

pub async fn spawn() -> TestApp {
    spawn_with(|_| {}).await
}

pub async fn spawn_with(tweak: impl FnOnce(&mut Config)) -> TestApp {
    let _ = tracing_subscriber::fmt().with_env_filter("taskcraft=debug,warn").with_test_writer().try_init();
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    let db_name = format!("taskcraft_test_{now}_{}", uuid::Uuid::new_v4().simple());
    let admin = sqlx::PgPool::connect(&std::env::var("TASKCRAFT_TEST_ADMIN_URL").unwrap_or(ADMIN_URL.into()))
        .await
        .expect("postgres reachable (docker container taskcraft-pg)");
    drop_stale(&admin, now).await;
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {db_name}"))).execute(&admin).await.unwrap();
    let data = tempfile::tempdir().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let admin_url = std::env::var("TASKCRAFT_TEST_ADMIN_URL").unwrap_or(ADMIN_URL.into());
    let mut config = Config {
        database_url: format!("{}/{db_name}", admin_url.rsplit_once('/').unwrap().0),
        bind: addr,
        base_url: format!("http://{addr}"),
        data_dir: data.path().to_path_buf(),
        static_dir: data.path().join("static"),
        s3_bucket: None,
        s3_endpoint: None,
        s3_region: None,
        s3_access_key_id: None,
        s3_secret_access_key: None,
        max_upload_mb: 16,
        secure_cookies: false,
        session_days: 30,
        log_format: "text".into(),
        metrics_token: None,
        webhook_allow_private: true,
        wikicraft_url: None,
        wikicraft_token: None,
        sso: Default::default(),
        smtp_url: None,
        mail_from: "TaskCraft <taskcraft@localhost>".into(),
        mail_in: Default::default(),
    };
    tweak(&mut config);
    // a busy shared Postgres (many test databases migrating at once) can time out a pool: retry
    let mut attempt = 0;
    let state = loop {
        attempt += 1;
        match AppState::new(config.clone()).await {
            Ok(s) => break s,
            Err(e) if attempt < 4 && e.to_string().contains("timed out") => {
                tokio::time::sleep(std::time::Duration::from_secs(2 * attempt)).await;
            }
            Err(e) => panic!("app state: {e}"),
        }
    };
    taskcraft::events::spawn_listener(&state);
    taskcraft::webhooks::spawn_worker(&state);
    let app = taskcraft::routes::router(state.clone());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    TestApp { base: format!("http://{addr}"), state, _data: data }
}

/// A browser-like client: cookie session plus the CSRF header.
pub fn client() -> reqwest::Client {
    let mut h = reqwest::header::HeaderMap::new();
    h.insert("x-taskcraft", reqwest::header::HeaderValue::from_static("1"));
    reqwest::Client::builder().cookie_store(true).default_headers(h).build().unwrap()
}

impl TestApp {
    pub fn url(&self, p: &str) -> String {
        format!("{}/api{p}", self.base)
    }

    /// Sets up the workspace and returns the owner's client.
    pub async fn setup(&self, demo: bool) -> reqwest::Client {
        let c = client();
        let r = c
            .post(self.url("/setup"))
            .json(&json!({"workspaceName": "Acme", "name": "Olivia Owner", "email": "owner@example.com", "password": "password123", "demo": demo}))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 200, "{}", r.text().await.unwrap());
        c
    }

    /// Creates a user with a password and returns their logged-in client.
    pub async fn user(&self, owner: &reqwest::Client, name: &str, email: &str, role: &str) -> (reqwest::Client, String) {
        let r: Value = owner
            .post(self.url("/users"))
            .json(&json!({"name": name, "email": email, "role": role, "password": "password123"}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let id = r["id"].as_str().unwrap().to_string();
        let c = client();
        let r = c.post(self.url("/auth/login")).json(&json!({"email": email, "password": "password123"})).send().await.unwrap();
        assert_eq!(r.status(), 200);
        (c, id)
    }
}

pub async fn get(c: &reqwest::Client, url: String) -> Value {
    let r = c.get(&url).send().await.unwrap();
    let status = r.status();
    let v: Value = r.json().await.unwrap_or(Value::Null);
    assert!(status.is_success(), "GET {url}: {status} {v}");
    v
}

pub async fn send(c: &reqwest::Client, method: reqwest::Method, url: String, body: Value) -> (u16, Value) {
    let r = c.request(method, &url).json(&body).send().await.unwrap();
    let status = r.status().as_u16();
    (status, r.json().await.unwrap_or(Value::Null))
}

pub async fn post(c: &reqwest::Client, url: String, body: Value) -> Value {
    let (s, v) = send(c, reqwest::Method::POST, url.clone(), body).await;
    assert!((200..300).contains(&s), "POST {url}: {s} {v}");
    v
}

pub async fn patch(c: &reqwest::Client, url: String, body: Value) -> Value {
    let (s, v) = send(c, reqwest::Method::PATCH, url.clone(), body).await;
    assert!((200..300).contains(&s), "PATCH {url}: {s} {v}");
    v
}
