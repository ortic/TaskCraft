use clap::{Parser, Subcommand};
use taskcraft::app::AppState;
use taskcraft::config::Config;
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(name = "taskcraft", version, about = "TaskCraft — self-hosted issue and project management")]
struct Cli {
    #[command(flatten)]
    config: Config,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Run the web server (default).
    Serve,
    /// Apply database migrations and exit.
    Migrate,
    /// Add sample projects, people and history to the workspace (as its owner).
    Demo,
    /// Write a backup of the database and the attachments to FILE (`-`: standard output).
    Backup {
        /// e.g. `taskcraft-2026-11-12.tar.gz`, or `-`
        file: std::path::PathBuf,
        /// leave the attachments out (when the S3 bucket is backed up on its own)
        #[arg(long)]
        no_files: bool,
    },
    /// Restore a backup from FILE (`-`: standard input) into an empty database.
    Restore {
        file: std::path::PathBuf,
        /// replace everything in a database that already holds a workspace
        #[arg(long)]
        force: bool,
    },
    /// Probe the running server's `/api/health` (for container HEALTHCHECKs); exits non-zero when unhealthy.
    Healthcheck,
}

/// Plain-TCP GET of `/api/health` on the configured bind address (no curl needed in the image).
fn healthcheck(bind: std::net::SocketAddr) -> anyhow::Result<()> {
    use std::io::{Read, Write};
    use std::time::Duration;
    let mut addr = bind;
    if addr.ip().is_unspecified() {
        addr.set_ip(if addr.is_ipv4() { std::net::Ipv4Addr::LOCALHOST.into() } else { std::net::Ipv6Addr::LOCALHOST.into() });
    }
    let mut s = std::net::TcpStream::connect_timeout(&addr, Duration::from_secs(3))?;
    s.set_read_timeout(Some(Duration::from_secs(3)))?;
    s.write_all(format!("GET /api/health HTTP/1.0\r\nHost: {addr}\r\n\r\n").as_bytes())?;
    let mut body = String::new();
    s.take(64 * 1024).read_to_string(&mut body)?;
    anyhow::ensure!(body.starts_with("HTTP/1.") && body.contains(" 200 ") && body.contains("\"ok\""), "unhealthy: {}", body.lines().next().unwrap_or(""));
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    if let Some(Command::Healthcheck) = cli.command {
        return healthcheck(cli.config.bind);
    }
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,tower_http=info,sqlx=warn"));
    // backups may stream to standard output: log to standard error
    if cli.config.log_format.eq_ignore_ascii_case("json") {
        tracing_subscriber::fmt()
            .json()
            .flatten_event(true)
            .with_current_span(true)
            .with_span_list(false)
            .with_env_filter(filter)
            .with_writer(std::io::stderr)
            .init();
    } else {
        tracing_subscriber::fmt().with_env_filter(filter).with_writer(std::io::stderr).init();
    }
    match cli.command.unwrap_or(Command::Serve) {
        Command::Healthcheck => unreachable!(),
        Command::Migrate => {
            AppState::new(cli.config).await?;
            println!("migrations applied");
        }
        Command::Backup { file, no_files } => {
            let db = taskcraft::app::connect(&cli.config).await?;
            let blobs = taskcraft::app::blob_store(&cli.config)?;
            let summary = taskcraft::backup::backup(&db, blobs.as_ref(), &file, !no_files).await?;
            eprintln!("backup written: {summary}");
        }
        Command::Restore { file, force } => {
            let summary = taskcraft::backup::restore(&cli.config, &file, force).await?;
            eprintln!("backup restored: {summary}");
        }
        Command::Demo => {
            let state = AppState::new(cli.config).await?;
            let owner: uuid::Uuid = sqlx::query_scalar("SELECT id FROM users WHERE role = 'owner' ORDER BY created_at LIMIT 1")
                .fetch_optional(&state.db)
                .await?
                .ok_or_else(|| anyhow::anyhow!("set up the workspace first (open the web app)"))?;
            taskcraft::demo::seed(&state, owner).await?;
            println!("sample data added");
        }
        Command::Serve => {
            let bind = cli.config.bind;
            let state = AppState::new(cli.config).await?;
            taskcraft::events::spawn_listener(&state);
            taskcraft::webhooks::spawn_worker(&state);
            taskcraft::schedule::spawn(&state);
            taskcraft::mail::spawn_worker(&state);
            let app = taskcraft::routes::router(state.clone());
            let listener = tokio::net::TcpListener::bind(bind).await?;
            tracing::info!("TaskCraft listening on http://{bind}; web app: {}", taskcraft::web_assets::source(&state.config.static_dir));
            axum::serve(listener, app).with_graceful_shutdown(shutdown(state)).await?;
        }
    }
    Ok(())
}

/// Waits for Ctrl-C or SIGTERM, then reports not-ready while connections drain.
async fn shutdown(state: AppState) {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let term = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {},
        _ = term => {},
    }
    state.shutting_down.store(true, std::sync::atomic::Ordering::Relaxed);
    tracing::info!("shutting down");
}
