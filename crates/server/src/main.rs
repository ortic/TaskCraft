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
    /// Import another tracker's XML site backup (the .zip, or the folder it was unpacked into):
    /// people, projects, issues with their numbers, comments, time logs, attachments, links and history.
    ImportBackup {
        /// the backup's .zip, its unpacked folder or its entities.xml
        path: std::path::PathBuf,
        /// check and report only; nothing is changed
        #[arg(long)]
        dry_run: bool,
        /// import a project under another key: `OLD=NEW` (NEW may be an existing project); repeatable
        #[arg(long = "project", value_name = "OLD=NEW")]
        projects: Vec<String>,
        /// only these projects of the backup (by key); repeatable
        #[arg(long, value_name = "KEY")]
        only: Vec<String>,
        /// leave a project of the backup out (by key); repeatable
        #[arg(long, value_name = "KEY")]
        skip: Vec<String>,
        /// people without an account here: invite (invited or, when inactive in the source,
        /// deactivated accounts), deactivated (deactivated accounts) or skip (no accounts).
        /// Nobody is e-mailed.
        #[arg(long, default_value = "invite")]
        people: taskcraft::site_backup::People,
        /// a folder with the attachments, when they aren't in the backup
        #[arg(long)]
        attachments: Option<std::path::PathBuf>,
        /// the time zone the backup's times are written in (its server's zone)
        #[arg(long, default_value = "UTC")]
        time_zone: chrono_tz::Tz,
        /// template of projects the import creates: scrum, kanban, tracker or business
        /// (default: from the source project)
        #[arg(long)]
        template: Option<String>,
        /// the administrator the import runs as (e-mail address; default: the owner)
        #[arg(long = "as", value_name = "EMAIL")]
        as_user: Option<String>,
    },
    /// Import issues from GitHub (github.com or GitHub Enterprise): repositories as projects,
    /// issues with their numbers, comments, labels, milestones, sub-issues, history and files.
    ImportGithub(ApiImport),
    /// Import issues from GitLab (gitlab.com or self-managed): projects with their issues, notes,
    /// labels, milestones, time logs, links, epics, history and uploads.
    ImportGitlab(ApiImport),
    /// Import from Redmine (its REST API, with an API key): projects and subprojects, issues with
    /// their ids, journals, time entries, custom fields, relations, watchers and attachments.
    ImportRedmine(ApiImport),
}

/// What an import from another tracker's API takes on the command line.
#[derive(clap::Args)]
struct ApiImport {
    /// the repositories or projects to import (`owner/repo`, `group/project`, an identifier)
    #[arg(required = true, value_name = "PROJECT")]
    projects: Vec<String>,
    /// the server's address (empty: github.com or gitlab.com; else the Enterprise or self-managed
    /// server; Redmine: required)
    #[arg(long)]
    url: Option<String>,
    /// a personal access token or API key with read access (best given in the environment)
    #[arg(long, env = "TASKCRAFT_IMPORT_TOKEN", hide_env_values = true)]
    token: Option<String>,
    /// check and report only; nothing is changed
    #[arg(long)]
    dry_run: bool,
    /// import a project under another key: `OLD=NEW` (OLD as named above, NEW may be an existing project); repeatable
    #[arg(long = "project", value_name = "OLD=NEW")]
    rename: Vec<String>,
    /// leave a project out (subprojects that come along); repeatable
    #[arg(long, value_name = "PROJECT")]
    skip: Vec<String>,
    /// people without an account here: invite, deactivated or skip (nobody is e-mailed)
    #[arg(long, default_value = "invite")]
    people: taskcraft::importer::People,
    /// template of projects the import creates: scrum, kanban, tracker or business
    #[arg(long)]
    template: Option<String>,
    /// how texts are written when the server doesn't say: textile or markdown
    #[arg(long)]
    markup: Option<taskcraft::importer::Format>,
    /// the administrator the import runs as (e-mail address; default: the owner)
    #[arg(long = "as", value_name = "EMAIL")]
    as_user: Option<String>,
}

/// The administrator an import from the command line runs as.
async fn import_user(state: &AppState, as_user: Option<&str>) -> anyhow::Result<taskcraft::auth::User> {
    Ok(match as_user {
        Some(email) => sqlx::query_as(
            "SELECT id, workspace_id, email, name, role, status FROM users WHERE lower(email) = lower($1) AND role IN ('owner', 'admin') AND status = 'active'",
        )
        .bind(email)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| anyhow::anyhow!("no active administrator with the e-mail address {email}"))?,
        None => sqlx::query_as("SELECT id, workspace_id, email, name, role, status FROM users WHERE role = 'owner' ORDER BY created_at LIMIT 1")
            .fetch_optional(&state.db)
            .await?
            .ok_or_else(|| anyhow::anyhow!("set up the workspace first (open the web app)"))?,
    })
}

/// Imports from another tracker's API, as the command line asked.
async fn import_api(config: taskcraft::config::Config, kind: taskcraft::importer::api::Kind, a: ApiImport) -> anyhow::Result<()> {
    use taskcraft::importer::{Choice, Options, api::Connection};
    let state = AppState::new(config).await?;
    let user = import_user(&state, a.as_user.as_deref()).await?;
    let mut choices = std::collections::BTreeMap::new();
    for p in &a.rename {
        let (old, new) = p.split_once('=').ok_or_else(|| anyhow::anyhow!("--project takes OLD=NEW, like --project octo/web=WEB"))?;
        choices.insert(old.trim().to_uppercase(), Choice::Auto { key: new.trim().to_uppercase() });
    }
    for k in &a.skip {
        choices.insert(k.trim().to_uppercase(), Choice::Skip);
    }
    let opts = Options { dry_run: a.dry_run, people: a.people, choices, template: a.template, markup: a.markup, ..Options::default() };
    let conn = Connection { source: kind, base_url: a.url.unwrap_or_default(), token: a.token, projects: a.projects, markup: a.markup };
    eprintln!("reading {} from {} …", conn.projects.join(", "), conn.shown_url());
    let report = taskcraft::importer::jobs::import_api(&state, &user, &conn, &opts).await?;
    println!("{report}");
    Ok(())
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
        Command::ImportBackup { path, dry_run, projects, only, skip, people, attachments, time_zone, template, as_user } => {
            use taskcraft::site_backup::{self, Choice, Options};
            let state = AppState::new(cli.config).await?;
            let user = import_user(&state, as_user.as_deref()).await?;
            let mut choices = std::collections::BTreeMap::new();
            for p in &projects {
                let (old, new) = p.split_once('=').ok_or_else(|| anyhow::anyhow!("--project takes OLD=NEW, like --project ABC=WEB"))?;
                choices.insert(old.trim().to_uppercase(), Choice::Auto { key: new.trim().to_uppercase() });
            }
            for k in &skip {
                choices.insert(k.trim().to_uppercase(), Choice::Skip);
            }
            let opts = Options { dry_run, people, choices, only: only.iter().map(|k| k.trim().to_uppercase()).collect(), template, time_zone, markup: None };
            eprintln!("reading {} …", path.display());
            let (mut source, backup) = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
                let mut s = site_backup::Source::open(&path, attachments.as_deref())?;
                let b = site_backup::load(&mut s, time_zone)?;
                Ok((s, b))
            })
            .await??;
            eprintln!(
                "{} projects, {} issues, {} comments, {} work logs, {} attachments ({} files), {} links, {} history entries",
                backup.projects.len(),
                backup.issues.len(),
                backup.comments.len(),
                backup.worklogs.len(),
                backup.attachments.len(),
                source.file_count(),
                backup.links.len(),
                backup.groups.len()
            );
            let progress = site_backup::ProgressHandle::default();
            let report = site_backup::run(&state, &user, &mut source, &backup, &opts, progress).await?;
            println!("{report}");
        }
        Command::ImportGithub(a) => import_api(cli.config, taskcraft::importer::api::Kind::Github, a).await?,
        Command::ImportGitlab(a) => import_api(cli.config, taskcraft::importer::api::Kind::Gitlab, a).await?,
        Command::ImportRedmine(a) => import_api(cli.config, taskcraft::importer::api::Kind::Redmine, a).await?,
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
            taskcraft::mail_in::spawn(&state);
            taskcraft::importer::jobs::spawn_cleanup(&state);
            taskcraft::portal::spawn(&state);
            let app = taskcraft::routes::router(state.clone());
            let listener = tokio::net::TcpListener::bind(bind).await?;
            tracing::info!("TaskCraft listening on http://{bind}; web app: {}", taskcraft::web_assets::source(&state.config.static_dir));
            // the peer's address is known (rate limits of the public portal pages)
            axum::serve(listener, app.into_make_service_with_connect_info::<std::net::SocketAddr>()).with_graceful_shutdown(shutdown(state)).await?;
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
