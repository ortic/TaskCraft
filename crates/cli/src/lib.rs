//! `tc`: TaskCraft from the command line — list, read, create and move issues, comment, link,
//! log time, run the timer and start branches, for people in a terminal and for scripts
//! (`--json`). It talks to the REST API described in `crates/server/openapi.json`; every address
//! it uses is in [`ENDPOINTS`], and a test keeps that list in step with the description.
//!
//! Sign in with `tc login` (the browser approves, the token is stored in a profile) or set
//! `TASKCRAFT_URL` and `TASKCRAFT_TOKEN`. Where a command takes an issue key and none is given,
//! it's read from the current Git branch (`web-12-short-title` → `WEB-12`).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use clap::{Args, Parser, Subcommand};
use reqwest::Method;
use serde_json::{Map, Value, json};

/// Every operation of the REST API the client uses: (method, path as in the OpenAPI description).
pub const ENDPOINTS: &[(&str, &str)] = &[
    ("POST", "/cli/logins"),
    ("POST", "/cli/token"),
    ("GET", "/me"),
    ("GET", "/people"),
    ("GET", "/search"),
    ("GET", "/projects/{key}"),
    ("POST", "/issues"),
    ("GET", "/issues/{key}"),
    ("PATCH", "/issues/{key}"),
    ("GET", "/issues/{key}/comments"),
    ("POST", "/issues/{key}/comments"),
    ("POST", "/issues/{key}/links"),
    ("POST", "/issues/{key}/worklogs"),
    ("GET", "/issues/{key}/development"),
    ("POST", "/issues/{key}/branch"),
    ("POST", "/issues/{key}/timer"),
    ("GET", "/me/timer"),
    ("POST", "/me/timer/pause"),
    ("POST", "/me/timer/resume"),
    ("POST", "/me/timer/stop"),
];

/// Where the client runs: its settings, the folder it's in, and how it speaks.
#[derive(Clone)]
pub struct Env {
    /// holds `config.json` (profiles with their server and token)
    pub config_dir: PathBuf,
    /// the working directory (for the Git branch)
    pub cwd: PathBuf,
    /// `TASKCRAFT_URL`
    pub url: Option<String>,
    /// `TASKCRAFT_TOKEN`
    pub token: Option<String>,
    /// open pages in the browser (off: only print their address)
    pub browser: bool,
    /// prints a line
    pub out: Arc<dyn Fn(String) + Send + Sync>,
    /// reads standard input to its end (`-` as a text)
    pub stdin: Arc<dyn Fn() -> String + Send + Sync>,
}

/// Why a command stopped.
#[derive(Debug)]
pub struct Failure {
    pub message: String,
    pub code: i32,
    /// already worded for the terminal (help, usage)
    pub quiet: bool,
}

type Res<T> = Result<T, Failure>;

fn fail<T>(message: impl Into<String>) -> Res<T> {
    Err(Failure { message: message.into(), code: 1, quiet: false })
}

#[derive(Parser)]
#[command(
    name = "tc",
    version,
    about = "TaskCraft from the command line",
    after_help = "Sign in with `tc login`, or set TASKCRAFT_URL and TASKCRAFT_TOKEN. An issue key left out is taken from the current Git branch."
)]
struct Cli {
    /// The profile to use (default: the current one)
    #[arg(long, global = true)]
    profile: Option<String>,
    /// Print JSON instead of text
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Sign in through the browser and keep the token in a profile
    Login {
        /// The server's address, e.g. https://tasks.example.com
        server: Option<String>,
        /// Use this API token instead of asking the browser
        #[arg(long)]
        token: Option<String>,
    },
    /// Forget a profile's token
    Logout,
    /// Who you are on the server
    Whoami,
    /// The profiles (servers) you signed in to
    #[command(subcommand)]
    Profile(ProfileCmd),
    /// Issues: list, view, create, edit, move, assign
    #[command(subcommand)]
    Issue(IssueCmd),
    /// Comment on an issue: `tc comment [KEY] TEXT` (`-` reads the text from standard input)
    Comment {
        #[arg(num_args = 1..=2, value_name = "[KEY] TEXT")]
        args: Vec<String>,
    },
    /// Link two issues: `tc link WEB-12 blocks WEB-15`
    Link {
        key: String,
        /// blocks, "is blocked by", "relates to", duplicates, … or one of the workspace's own
        kind: String,
        other: String,
    },
    /// Log time: `tc log 1h30m [KEY]`
    Log {
        /// e.g. 45m, 1h30m, 2d
        time: String,
        key: Option<String>,
        /// What you did
        #[arg(short = 'm', long)]
        note: Option<String>,
        /// The day (YYYY-MM-DD; default: today)
        #[arg(long)]
        date: Option<String>,
    },
    /// Your timer: start, stop, pause, resume, status
    #[command(subcommand)]
    Timer(TimerCmd),
    /// Start the issue's branch: `git checkout -b` with the suggested name, or `--remote` to create it on the Git server
    Branch {
        key: Option<String>,
        /// Create the branch on the Git server (then fetch and check it out)
        #[arg(long)]
        remote: bool,
        /// With --remote: also open a draft pull request
        #[arg(long)]
        pr: bool,
        /// With --remote: the branch to start from (default: the repository's default branch)
        #[arg(long)]
        base: Option<String>,
        /// Only print the name
        #[arg(long)]
        print: bool,
    },
    /// Open an issue in the browser
    Open { key: Option<String> },
}

#[derive(Subcommand)]
enum ProfileCmd {
    /// List the profiles
    List,
    /// Make a profile the current one
    Use { name: String },
}

#[derive(Subcommand)]
enum IssueCmd {
    /// List issues by a TQL query: `tc issue list 'assignee = me() and category != done'`
    List {
        /// TQL (default: your open issues)
        query: Option<String>,
        #[arg(long, default_value_t = 50)]
        limit: u32,
    },
    /// Show an issue
    View {
        key: Option<String>,
        /// With its comments
        #[arg(long)]
        comments: bool,
    },
    /// Create an issue
    Create(CreateArgs),
    /// Change an issue's fields
    Edit {
        key: Option<String>,
        /// A new title
        #[arg(long)]
        title: Option<String>,
        #[command(flatten)]
        fields: Fields,
    },
    /// Move an issue to a status: `tc issue move WEB-12 "In progress"`
    Move {
        #[arg(num_args = 1..=2, value_name = "[KEY] STATUS")]
        args: Vec<String>,
        /// A comment, where the status asks for one
        #[arg(short = 'm', long)]
        comment: Option<String>,
    },
    /// Assign an issue: to a person (name or e-mail), `me` (the default) or `none`
    Assign {
        #[arg(num_args = 0..=2, value_name = "[KEY] [WHO]")]
        args: Vec<String>,
    },
}

#[derive(Args)]
struct CreateArgs {
    /// The project's key (default: the project of the current branch's issue)
    #[arg(short, long)]
    project: Option<String>,
    /// The title
    #[arg(value_name = "TITLE")]
    title: String,
    /// The issue type by name (default: the project's first)
    #[arg(long = "type")]
    kind: Option<String>,
    /// The parent issue or epic
    #[arg(long)]
    parent: Option<String>,
    #[command(flatten)]
    fields: Fields,
}

#[derive(Args, Default)]
struct Fields {
    /// The description in Markdown (`-`: from standard input)
    #[arg(short, long)]
    description: Option<String>,
    /// none, low, medium, high or urgent
    #[arg(long)]
    priority: Option<String>,
    /// A person (name or e-mail), `me` or `none`
    #[arg(long)]
    assignee: Option<String>,
    /// Labels (repeat; replaces the issue's labels)
    #[arg(long = "label")]
    labels: Vec<String>,
    /// Due date (YYYY-MM-DD, or `none`)
    #[arg(long)]
    due: Option<String>,
    /// Story points
    #[arg(long)]
    points: Option<f64>,
}

#[derive(Subcommand)]
enum TimerCmd {
    /// Start the timer on an issue
    Start {
        key: Option<String>,
        #[arg(short = 'm', long)]
        note: Option<String>,
    },
    /// Stop the timer and log its time (`--draft`: keep it as a draft instead)
    Stop {
        #[arg(long)]
        draft: bool,
        #[arg(short = 'm', long)]
        note: Option<String>,
    },
    Pause,
    Resume,
    /// The running timer
    Status,
}

// ---- settings ---------------------------------------------------------------------------------------

fn config_path(env: &Env) -> PathBuf {
    env.config_dir.join("config.json")
}

fn load_config(env: &Env) -> Value {
    std::fs::read_to_string(config_path(env))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({"profiles": {}}))
}

fn save_config(env: &Env, c: &Value) -> Res<()> {
    let path = config_path(env);
    let write = || -> std::io::Result<()> {
        std::fs::create_dir_all(&env.config_dir)?;
        std::fs::write(&path, serde_json::to_string_pretty(c).unwrap_or_default())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        }
        Ok(())
    };
    write().or_else(|e| fail(format!("can't write {}: {e}", path.display())))
}

fn clean_url(u: &str) -> Res<String> {
    let u = u.trim().trim_end_matches('/');
    let u = u.strip_suffix("/api").unwrap_or(u);
    let u = if u.contains("://") { u.to_string() } else { format!("https://{u}") };
    if !(u.starts_with("https://") || u.starts_with("http://")) || u.len() < 10 {
        return fail("the server's address looks like https://tasks.example.com");
    }
    Ok(u)
}

/// The server and token to use: the environment's, else the profile's.
struct Api {
    base: String,
    token: Option<String>,
    http: reqwest::Client,
}

fn profile_name(cli_profile: &Option<String>, config: &Value) -> String {
    cli_profile.clone().or_else(|| config["current"].as_str().map(str::to_string)).unwrap_or_else(|| "default".into())
}

fn api_for(env: &Env, profile: &Option<String>) -> Res<Api> {
    let http = reqwest::Client::builder()
        .user_agent(concat!("tc/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(60))
        .build()
        .map_err(|e| Failure { message: e.to_string(), code: 1, quiet: false })?;
    // an explicit --profile wins over the environment
    if profile.is_none()
        && let (Some(url), Some(token)) = (&env.url, &env.token)
    {
        return Ok(Api { base: clean_url(url)?, token: Some(token.clone()), http });
    }
    let config = load_config(env);
    let name = profile_name(profile, &config);
    let p = &config["profiles"][&name];
    match (p["url"].as_str(), p["token"].as_str()) {
        (Some(url), Some(token)) => Ok(Api { base: url.to_string(), token: Some(token.to_string()), http }),
        _ if profile.is_some() => fail(format!("there is no profile {name}: sign in with `tc --profile {name} login <server>`")),
        _ => fail("not signed in: run `tc login <server>`, or set TASKCRAFT_URL and TASKCRAFT_TOKEN"),
    }
}

impl Api {
    /// One request. `path` is an entry of [`ENDPOINTS`]; `{key}` is filled from `key`.
    async fn send(&self, method: Method, path: &'static str, key: &str, query: &[(&str, String)], body: Option<Value>) -> Res<(u16, Value)> {
        assert!(ENDPOINTS.contains(&(method.as_str(), path)), "{method} {path} isn't in ENDPOINTS");
        let mut url = format!("{}/api/v1{}", self.base, path.replace("{key}", &encode(key)));
        for (i, (k, v)) in query.iter().enumerate() {
            url.push(if i == 0 { '?' } else { '&' });
            url.push_str(&format!("{k}={}", encode(v)));
        }
        let mut req = self.http.request(method, &url);
        if let Some(t) = &self.token {
            req = req.bearer_auth(t);
        }
        if let Some(b) = body {
            req = req.json(&b);
        }
        let res = req.send().await.or_else(|e| fail(format!("can't reach {}: {}", self.base, e.without_url())))?;
        let status = res.status().as_u16();
        let text = res.text().await.unwrap_or_default();
        Ok((status, serde_json::from_str(&text).unwrap_or(Value::Null)))
    }

    /// A request that must succeed; the server's own words otherwise.
    async fn call(&self, method: Method, path: &'static str, key: &str, query: &[(&str, String)], body: Option<Value>) -> Res<Value> {
        let (status, v) = self.send(method, path, key, query, body).await?;
        match status {
            200..=299 => Ok(v),
            401 => fail("the server doesn't accept the token: sign in again with `tc login`"),
            _ => fail(v["message"].as_str().map(str::to_string).unwrap_or_else(|| format!("the server answered HTTP {status}"))),
        }
    }

    async fn get(&self, path: &'static str, key: &str) -> Res<Value> {
        self.call(Method::GET, path, key, &[], None).await
    }
}

fn encode(s: &str) -> String {
    s.bytes().map(|b| if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') { (b as char).to_string() } else { format!("%{b:02X}") }).collect()
}

// ---- issue keys and Git -------------------------------------------------------------------------------

/// `WEB-12` (any case), all of the text.
pub fn is_key(s: &str) -> bool {
    key_in(s).is_some_and(|k| k.len() == s.len())
}

/// The first issue key in a text (`feature/web-12-login` → `WEB-12`).
pub fn key_in(text: &str) -> Option<String> {
    let b = text.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let starts = i == 0 || !b[i - 1].is_ascii_alphanumeric();
        if b[i].is_ascii_alphabetic() && starts {
            let mut j = i;
            while j < b.len() && b[j].is_ascii_alphanumeric() {
                j += 1;
            }
            if j < b.len() && b[j] == b'-' && (2..=10).contains(&(j - i)) {
                let mut k = j + 1;
                while k < b.len() && b[k].is_ascii_digit() {
                    k += 1;
                }
                if k > j + 1 && k - j - 1 <= 9 && (k == b.len() || !b[k].is_ascii_alphanumeric()) {
                    return Some(text[i..k].to_ascii_uppercase());
                }
            }
            i = j.max(i + 1);
        } else {
            i += 1;
        }
    }
    None
}

fn git(cwd: &Path, args: &[&str]) -> Result<String, String> {
    let out = std::process::Command::new("git").args(args).current_dir(cwd).output().map_err(|e| format!("can't run git: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// The key given, else the one in the current branch's name.
fn key_or_branch(env: &Env, key: Option<String>) -> Res<String> {
    if let Some(k) = key {
        return if is_key(&k) { Ok(k.to_ascii_uppercase()) } else { fail(format!("{k} isn't an issue key (like WEB-12)")) };
    }
    let branch = git(&env.cwd, &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap_or_default();
    key_in(&branch).ok_or(()).or_else(|_| {
        fail(if branch.is_empty() {
            "say which issue (like WEB-12): this isn't a Git repository".to_string()
        } else {
            format!("say which issue (like WEB-12): the branch {branch} doesn't name one")
        })
    })
}

/// `[KEY] REST…` → (key, rest): the first argument is the key when it looks like one.
fn split_key(env: &Env, mut args: Vec<String>, rest: usize) -> Res<(String, Vec<String>)> {
    let key = if args.len() > rest || args.first().is_some_and(|a| is_key(a) && args.len() == rest + 1) { Some(args.remove(0)) } else { None };
    Ok((key_or_branch(env, key)?, args))
}

// ---- output -------------------------------------------------------------------------------------------

fn say(env: &Env, line: impl Into<String>) {
    (env.out)(line.into());
}

fn show(env: &Env, json: bool, v: &Value, text: impl FnOnce() -> String) {
    if json {
        say(env, serde_json::to_string_pretty(v).unwrap_or_default());
    } else {
        say(env, text());
    }
}

fn s(v: &Value) -> &str {
    v.as_str().unwrap_or("")
}

/// Columns padded to their widest cell (the last one isn't padded).
pub fn table(rows: &[Vec<String>]) -> String {
    let cols = rows.iter().map(Vec::len).max().unwrap_or(0);
    let widths: Vec<usize> = (0..cols).map(|c| rows.iter().filter_map(|r| r.get(c)).map(|x| x.chars().count()).max().unwrap_or(0)).collect();
    rows.iter()
        .map(|r| {
            r.iter()
                .enumerate()
                .map(|(i, cell)| if i + 1 == r.len() { cell.clone() } else { format!("{cell}{}", " ".repeat(widths[i] - cell.chars().count())) })
                .collect::<Vec<_>>()
                .join("  ")
                .trim_end()
                .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn issue_rows(issues: &[Value]) -> String {
    let mut rows = vec![vec!["KEY".to_string(), "STATUS".into(), "PRIORITY".into(), "ASSIGNEE".into(), "TITLE".into()]];
    for i in issues {
        rows.push(vec![
            s(&i["key"]).into(),
            s(&i["status"]["name"]).into(),
            s(&i["priority"]).into(),
            i["assignee"]["name"].as_str().unwrap_or("–").into(),
            s(&i["title"]).into(),
        ]);
    }
    table(&rows)
}

/// The text of a rich-text document (paragraphs on their own lines, list items with a dash).
pub fn doc_text(doc: &Value) -> String {
    fn walk(n: &Value, out: &mut String) {
        match n["type"].as_str().unwrap_or("") {
            "text" => out.push_str(s(&n["text"])),
            "hardBreak" => out.push('\n'),
            "mention" => {
                out.push('@');
                out.push_str(s(&n["attrs"]["label"]));
            }
            "listItem" | "taskItem" => out.push_str("- "),
            _ => {}
        }
        for c in n["content"].as_array().into_iter().flatten() {
            walk(c, out);
        }
        if matches!(n["type"].as_str(), Some("paragraph" | "heading" | "codeBlock")) && !out.ends_with('\n') {
            out.push('\n');
        }
    }
    let mut out = String::new();
    walk(doc, &mut out);
    out.trim_end().to_string()
}

fn minutes(m: i64) -> String {
    match (m / 60, m % 60) {
        (0, m) => format!("{m}m"),
        (h, 0) => format!("{h}h"),
        (h, m) => format!("{h}h {m}m"),
    }
}

fn open_page(env: &Env, url: &str) {
    if !env.browser {
        return;
    }
    let program = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(windows) {
        "explorer"
    } else {
        "xdg-open"
    };
    let _ = std::process::Command::new(program)
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

// ---- commands -----------------------------------------------------------------------------------------

/// Runs one command line (`args[0]` is the program's name).
pub async fn run(args: &[String], env: &Env) -> Res<()> {
    let cli = Cli::try_parse_from(args).map_err(|e| Failure { message: e.render().to_string(), code: if e.use_stderr() { 2 } else { 0 }, quiet: true })?;
    let json = cli.json;
    match cli.command {
        Command::Login { server, token } => login(env, &cli.profile, server, token).await,
        Command::Logout => {
            let mut c = load_config(env);
            let name = profile_name(&cli.profile, &c);
            if let Some(p) = c["profiles"].as_object_mut() {
                p.remove(&name);
            }
            save_config(env, &c)?;
            say(env, format!("Signed out of {name}. The token is still valid on the server until you delete it under Profile & settings → API tokens."));
            Ok(())
        }
        Command::Whoami => {
            let api = api_for(env, &cli.profile)?;
            let me = api.get("/me", "").await?;
            if me["user"].is_null() {
                return fail("the server doesn't accept the token: sign in again with `tc login`");
            }
            show(env, json, &json!({"user": me["user"], "workspace": me["workspace"]["name"], "server": api.base}), || {
                format!("{} <{}> · {} · {}", s(&me["user"]["name"]), s(&me["user"]["email"]), s(&me["workspace"]["name"]), api.base)
            });
            Ok(())
        }
        Command::Profile(ProfileCmd::List) => {
            let c = load_config(env);
            let current = profile_name(&None, &c);
            let list: Vec<Value> = c["profiles"]
                .as_object()
                .into_iter()
                .flatten()
                .map(|(n, p)| json!({"name": n, "url": p["url"], "user": p["user"], "current": *n == current}))
                .collect();
            show(env, json, &json!(list), || {
                if list.is_empty() {
                    return "No profiles yet: sign in with `tc login <server>`.".into();
                }
                table(
                    &list
                        .iter()
                        .map(|p| {
                            vec![
                                if p["current"] == true { "*".into() } else { " ".into() },
                                s(&p["name"]).to_string(),
                                s(&p["url"]).to_string(),
                                s(&p["user"]).to_string(),
                            ]
                        })
                        .collect::<Vec<_>>(),
                )
            });
            Ok(())
        }
        Command::Profile(ProfileCmd::Use { name }) => {
            let mut c = load_config(env);
            if c["profiles"][&name].is_null() {
                return fail(format!("there is no profile {name}"));
            }
            c["current"] = json!(name);
            save_config(env, &c)?;
            say(env, format!("Using {name}."));
            Ok(())
        }
        Command::Issue(cmd) => issue(env, &api_for(env, &cli.profile)?, json, cmd).await,
        Command::Comment { args } => {
            let api = api_for(env, &cli.profile)?;
            let (key, mut rest) = split_key(env, args, 1)?;
            let text = text_or_stdin(env, rest.pop().unwrap_or_default());
            if text.trim().is_empty() {
                return fail("the comment is empty");
            }
            let v = api.call(Method::POST, "/issues/{key}/comments", &key, &[], Some(json!({"markdown": text}))).await?;
            show(env, json, &v, || format!("Commented on {key}."));
            Ok(())
        }
        Command::Link { key, kind, other } => {
            let api = api_for(env, &cli.profile)?;
            let (key, other) = (key_or_branch(env, Some(key))?, key_or_branch(env, Some(other))?);
            let v = api.call(Method::POST, "/issues/{key}/links", &key, &[], Some(json!({"kind": kind, "target": other}))).await?;
            show(env, json, &v, || format!("{key} {kind} {other}."));
            Ok(())
        }
        Command::Log { time, key, note, date } => {
            let api = api_for(env, &cli.profile)?;
            // `tc log WEB-12 1h` works too
            let (time, key) = if is_key(&time) && key.as_deref().is_some_and(|k| !is_key(k)) { (key.unwrap_or_default(), Some(time)) } else { (time, key) };
            let key = key_or_branch(env, key)?;
            let v = api.call(Method::POST, "/issues/{key}/worklogs", &key, &[], Some(json!({"time": spaced(&time), "note": note, "date": date}))).await?;
            show(env, json, &v, || format!("Logged {} on {key}.", minutes(v["minutes"].as_i64().unwrap_or(0))));
            Ok(())
        }
        Command::Timer(cmd) => timer(env, &api_for(env, &cli.profile)?, json, cmd).await,
        Command::Branch { key, remote, pr, base, print } => branch(env, &api_for(env, &cli.profile)?, json, key, remote, pr, base, print).await,
        Command::Open { key } => {
            let api = api_for(env, &cli.profile)?;
            let key = key_or_branch(env, key)?;
            let url = format!("{}/issues/{key}", api.base);
            open_page(env, &url);
            show(env, json, &json!({"url": url}), || url.clone());
            Ok(())
        }
    }
}

fn text_or_stdin(env: &Env, text: String) -> String {
    if text == "-" { (env.stdin)() } else { text }
}

/// `1h30m` → `1h 30m` (how the server reads times).
fn spaced(time: &str) -> String {
    let mut out = String::new();
    let mut prev_alpha = false;
    for c in time.trim().chars() {
        if c.is_ascii_digit() && prev_alpha {
            out.push(' ');
        }
        prev_alpha = c.is_alphabetic();
        out.push(c);
    }
    out
}

async fn login(env: &Env, profile: &Option<String>, server: Option<String>, token: Option<String>) -> Res<()> {
    let mut config = load_config(env);
    let name = profile_name(profile, &config);
    let url = match server.or_else(|| env.url.clone()).or_else(|| config["profiles"][&name]["url"].as_str().map(str::to_string)) {
        Some(u) => clean_url(&u)?,
        None => return fail("say which server: `tc login https://tasks.example.com`"),
    };
    let http = reqwest::Client::builder()
        .user_agent(concat!("tc/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(30))
        .build()
        .or_else(|e| fail(e.to_string()))?;
    let mut api = Api { base: url.clone(), token: None, http };
    let token = match token {
        Some(t) => t.trim().to_string(),
        None => {
            let host = std::env::var("HOSTNAME")
                .ok()
                .filter(|h| !h.is_empty())
                .or_else(|| std::fs::read_to_string("/etc/hostname").ok().map(|h| h.trim().to_string()))
                .filter(|h| !h.is_empty())
                .unwrap_or_else(|| "this computer".into());
            let start = api.call(Method::POST, "/cli/logins", "", &[], Some(json!({"name": host}))).await?;
            let (page, code, verifier) = (s(&start["url"]).to_string(), s(&start["code"]).to_string(), s(&start["verifier"]).to_string());
            say(env, format!("Open this page and approve the sign-in (code {code}):\n  {page}"));
            open_page(env, &page);
            let wait = Duration::from_millis((start["interval"].as_u64().unwrap_or(2) * 1000).clamp(200, 10_000));
            let until = std::time::Instant::now() + Duration::from_secs(start["expiresIn"].as_u64().unwrap_or(600));
            loop {
                let (status, v) = api.send(Method::POST, "/cli/token", "", &[], Some(json!({"verifier": verifier}))).await?;
                match status {
                    200 => break s(&v["token"]).to_string(),
                    202 if std::time::Instant::now() < until => tokio::time::sleep(wait).await,
                    _ => return fail("the sign-in wasn't approved in time (or was refused): run `tc login` again"),
                }
            }
        }
    };
    api.token = Some(token.clone());
    let me = api.get("/me", "").await?;
    if me["user"].is_null() {
        return fail("the server doesn't accept this token");
    }
    config["profiles"][&name] = json!({"url": url, "token": token, "user": me["user"]["name"]});
    config["current"] = json!(name);
    save_config(env, &config)?;
    say(env, format!("Signed in to {url} as {} (profile {name}).", s(&me["user"]["name"])));
    Ok(())
}

/// A person by name or e-mail, `me`, or `none` → their id (null for none).
async fn person(api: &Api, who: &str) -> Res<Value> {
    match who.trim().to_lowercase().as_str() {
        "none" | "nobody" | "-" => Ok(Value::Null),
        "me" | "" => Ok(api.get("/me", "").await?["user"]["id"].clone()),
        w => {
            let people = api.get("/people", "").await?;
            let list = people.as_array().cloned().unwrap_or_default();
            let exact: Vec<&Value> = list.iter().filter(|p| s(&p["name"]).to_lowercase() == w || s(&p["email"]).to_lowercase() == w).collect();
            let found: Vec<&Value> = if exact.is_empty() { list.iter().filter(|p| s(&p["name"]).to_lowercase().contains(w)).collect() } else { exact };
            match found.as_slice() {
                [one] => Ok(one["id"].clone()),
                [] => fail(format!("nobody is called {who}")),
                many => fail(format!("{who} could be {}: say it more exactly", many.iter().map(|p| s(&p["name"])).collect::<Vec<_>>().join(", "))),
            }
        }
    }
}

/// What `--title`, `--priority`, … say, as a change for the API.
async fn patch_of(env: &Env, api: &Api, f: Fields) -> Res<Map<String, Value>> {
    let mut m = Map::new();
    if let Some(d) = f.description {
        m.insert("descriptionMarkdown".into(), json!(text_or_stdin(env, d)));
    }
    if let Some(p) = f.priority {
        m.insert("priority".into(), json!(p.to_lowercase()));
    }
    if let Some(a) = f.assignee {
        m.insert("assigneeId".into(), person(api, &a).await?);
    }
    if !f.labels.is_empty() {
        m.insert("labels".into(), json!(f.labels.iter().filter(|l| !l.is_empty()).collect::<Vec<_>>()));
    }
    if let Some(d) = f.due {
        m.insert("dueDate".into(), if d.eq_ignore_ascii_case("none") { Value::Null } else { json!(d) });
    }
    if let Some(p) = f.points {
        m.insert("storyPoints".into(), json!(p));
    }
    Ok(m)
}

fn issue_line(i: &Value) -> String {
    format!("{} {} [{}]", s(&i["key"]), s(&i["title"]), s(&i["status"]["name"]))
}

async fn issue(env: &Env, api: &Api, json: bool, cmd: IssueCmd) -> Res<()> {
    match cmd {
        IssueCmd::List { query, limit } => {
            let q = query.unwrap_or_else(|| "assignee = me() and category != done order by priority desc, updated desc".into());
            let v = api.call(Method::GET, "/search", "", &[("q", q), ("limit", limit.clamp(1, 500).to_string())], None).await?;
            let list = v["issues"].as_array().cloned().unwrap_or_default();
            show(env, json, &v, || {
                if list.is_empty() {
                    return "No issues.".into();
                }
                let total = v["total"].as_u64().unwrap_or(list.len() as u64);
                let more = if total > list.len() as u64 { format!("\n… {} of {total} (raise --limit to see more)", list.len()) } else { String::new() };
                format!("{}{more}", issue_rows(&list))
            });
        }
        IssueCmd::View { key, comments } => {
            let key = key_or_branch(env, key)?;
            let mut v = api.get("/issues/{key}", &key).await?;
            if comments {
                v["comments"] = api.get("/issues/{key}/comments", &key).await?;
            }
            show(env, json, &v, || {
                let mut out = format!("{} {}\n", s(&v["key"]), s(&v["title"]));
                let mut facts = vec![
                    vec!["Status".to_string(), s(&v["status"]["name"]).to_string()],
                    vec!["Type".into(), s(&v["type"]["name"]).into()],
                    vec!["Priority".into(), s(&v["priority"]).into()],
                ];
                facts.push(vec!["Assignee".into(), v["assignee"]["name"].as_str().unwrap_or("–").into()]);
                facts.push(vec!["Reporter".into(), v["reporter"]["name"].as_str().unwrap_or("–").into()]);
                for (label, field) in [("Sprint", &v["sprint"]["name"]), ("Due", &v["dueDate"]), ("Parent", &v["parent"]["key"])] {
                    if let Some(x) = field.as_str() {
                        facts.push(vec![label.into(), x.into()]);
                    }
                }
                if let Some(l) = v["labels"].as_array().filter(|l| !l.is_empty()) {
                    facts.push(vec!["Labels".into(), l.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", ")]);
                }
                facts.push(vec!["URL".into(), format!("{}/issues/{}", api.base, s(&v["key"]))]);
                out.push_str(&table(&facts));
                let text = doc_text(&v["description"]);
                if !text.is_empty() {
                    out.push_str(&format!("\n\n{text}"));
                }
                for c in v["comments"].as_array().into_iter().flatten() {
                    out.push_str(&format!(
                        "\n\n— {} ({}):\n{}",
                        c["author"]["name"].as_str().or_else(|| c["author"].as_str()).unwrap_or("Automation"),
                        s(&c["createdAt"]),
                        doc_text(&c["body"])
                    ));
                }
                out
            });
        }
        IssueCmd::Create(a) => {
            let project = match a.project {
                Some(p) => p.to_uppercase(),
                None => key_or_branch(env, None)
                    .ok()
                    .and_then(|k| k.split('-').next().map(str::to_string))
                    .ok_or(())
                    .or_else(|_| fail("say which project: --project KEY"))?,
            };
            let mut body = patch_of(env, api, a.fields).await?;
            body.insert("projectKey".into(), json!(project));
            body.insert("title".into(), json!(a.title));
            if let Some(kind) = a.kind {
                let p = api.get("/projects/{key}", &project).await?;
                let types = p["types"].as_array().cloned().unwrap_or_default();
                let t = types.iter().find(|t| s(&t["name"]).eq_ignore_ascii_case(&kind)).ok_or(()).or_else(|_| {
                    fail(format!("{project} has no type {kind}; there are: {}", types.iter().map(|t| s(&t["name"])).collect::<Vec<_>>().join(", ")))
                })?;
                body.insert("typeId".into(), t["id"].clone());
            }
            if let Some(parent) = a.parent {
                let parent = api.get("/issues/{key}", &key_or_branch(env, Some(parent))?).await?;
                body.insert("parentId".into(), parent["id"].clone());
            }
            let v = api.call(Method::POST, "/issues", "", &[], Some(Value::Object(body))).await?;
            show(env, json, &v, || format!("Created {}\n{}/issues/{}", issue_line(&v), api.base, s(&v["key"])));
        }
        IssueCmd::Edit { key, title, fields } => {
            let key = key_or_branch(env, key)?;
            let mut body = patch_of(env, api, fields).await?;
            if let Some(t) = title {
                body.insert("title".into(), json!(t));
            }
            if body.is_empty() {
                return fail("say what to change: --title, --description, --priority, --assignee, --label, --due or --points");
            }
            let v = api.call(Method::PATCH, "/issues/{key}", &key, &[], Some(Value::Object(body))).await?;
            show(env, json, &v, || format!("Changed {}", issue_line(&v)));
        }
        IssueCmd::Move { args, comment } => {
            let (key, rest) = split_key(env, args, 1)?;
            let wanted = rest.first().cloned().unwrap_or_default();
            let cur = api.get("/issues/{key}", &key).await?;
            let p = api.get("/projects/{key}", s(&cur["projectKey"])).await?;
            let statuses = p["statuses"].as_array().cloned().unwrap_or_default();
            let w = wanted.to_lowercase();
            let exact: Vec<&Value> = statuses.iter().filter(|x| s(&x["name"]).to_lowercase() == w).collect();
            let found: Vec<&Value> = if exact.is_empty() { statuses.iter().filter(|x| s(&x["name"]).to_lowercase().starts_with(&w)).collect() } else { exact };
            let [status] = found.as_slice() else {
                return fail(format!(
                    "{} has no status {wanted:?}; there are: {}",
                    s(&p["key"]),
                    statuses.iter().map(|x| s(&x["name"])).collect::<Vec<_>>().join(", ")
                ));
            };
            let mut body = json!({"statusId": status["id"]});
            if let Some(c) = comment {
                body["comment"] = json!(c);
            }
            let v = api.call(Method::PATCH, "/issues/{key}", &key, &[], Some(body)).await?;
            show(env, json, &v, || format!("Moved {}", issue_line(&v)));
        }
        IssueCmd::Assign { mut args } => {
            let key = if args.first().is_some_and(|a| is_key(a)) { Some(args.remove(0)) } else { None };
            let key = key_or_branch(env, key)?;
            let who = args.first().cloned().unwrap_or_else(|| "me".into());
            let v = api.call(Method::PATCH, "/issues/{key}", &key, &[], Some(json!({"assigneeId": person(api, &who).await?}))).await?;
            show(env, json, &v, || match v["assignee"]["name"].as_str() {
                Some(n) => format!("{key} is assigned to {n}."),
                None => format!("{key} is unassigned."),
            });
        }
    }
    Ok(())
}

fn timer_text(v: &Value) -> String {
    let t = &v["timer"];
    if !t.is_object() {
        return "No timer is running.".into();
    }
    let state = if t["pausedAt"].is_null() { "running" } else { "paused" };
    format!("Timer {state} on {} {}: {}", s(&t["issue"]["key"]), s(&t["issue"]["title"]), minutes(t["elapsedSeconds"].as_i64().unwrap_or(0) / 60))
}

async fn timer(env: &Env, api: &Api, json: bool, cmd: TimerCmd) -> Res<()> {
    match cmd {
        TimerCmd::Start { key, note } => {
            let key = key_or_branch(env, key)?;
            let v = api.call(Method::POST, "/issues/{key}/timer", &key, &[], Some(json!({"note": note, "replace": true}))).await?;
            show(env, json, &v, || format!("Timer started on {key}."));
        }
        TimerCmd::Stop { draft, note } => {
            let v = api.call(Method::POST, "/me/timer/stop", "", &[], Some(json!({"book": !draft, "note": note}))).await?;
            show(env, json, &v, || {
                let m = v["worklog"]["minutes"].as_i64().or_else(|| v["draft"]["minutes"].as_i64()).or_else(|| v["minutes"].as_i64()).unwrap_or(0);
                if v["worklog"].is_object() {
                    format!("Timer stopped: logged {}.", minutes(m))
                } else {
                    format!("Timer stopped: {} wait as a draft.", minutes(m))
                }
            });
        }
        TimerCmd::Pause => {
            let v = api.call(Method::POST, "/me/timer/pause", "", &[], Some(json!({}))).await?;
            show(env, json, &v, || "Timer paused.".into());
        }
        TimerCmd::Resume => {
            let v = api.call(Method::POST, "/me/timer/resume", "", &[], Some(json!({}))).await?;
            show(env, json, &v, || "Timer running again.".into());
        }
        TimerCmd::Status => {
            let v = api.get("/me/timer", "").await?;
            show(env, json, &v, || timer_text(&v));
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn branch(env: &Env, api: &Api, json: bool, key: Option<String>, remote: bool, pr: bool, base: Option<String>, print: bool) -> Res<()> {
    let key = key_or_branch(env, key)?;
    if !remote {
        let d = api.get("/issues/{key}/development", &key).await?;
        let name = s(&d["branchName"]).to_string();
        if print {
            show(env, json, &json!({"branch": name}), || name.clone());
            return Ok(());
        }
        // an existing branch is checked out, a new one created
        let done = git(&env.cwd, &["checkout", &name]).or_else(|_| git(&env.cwd, &["checkout", "-b", &name]));
        return match done {
            Ok(_) => {
                show(env, json, &json!({"branch": name}), || format!("On branch {name}."));
                Ok(())
            }
            Err(e) => fail(format!("git checkout -b {name}: {e}")),
        };
    }
    let v = api.call(Method::POST, "/issues/{key}/branch", &key, &[], Some(json!({"base": base, "pullRequest": pr}))).await?;
    let name = s(&v["branch"]["name"]).to_string();
    let checked_out = !print && git(&env.cwd, &["fetch", "origin", &name]).and_then(|_| git(&env.cwd, &["checkout", &name])).is_ok();
    show(env, json, &v, || {
        let mut out = format!("Created {name} on the Git server: {}", s(&v["branch"]["url"]));
        if let Some(u) = v["pullRequest"]["url"].as_str() {
            out.push_str(&format!("\nDraft pull request: {u}"));
        }
        if let Some(w) = v["warning"].as_str() {
            out.push_str(&format!("\n{w}"));
        }
        out.push_str(&if checked_out {
            format!("\nOn branch {name}.")
        } else {
            format!("\nCheck it out with: git fetch origin {name} && git checkout {name}")
        });
        out
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys() {
        assert_eq!(key_in("feature/web-12-login-timeout").as_deref(), Some("WEB-12"));
        assert_eq!(key_in("WEB-7").as_deref(), Some("WEB-7"));
        assert_eq!(key_in("main"), None);
        assert_eq!(key_in("fix-ops-3b"), None);
        assert!(is_key("ops-3") && !is_key("ops-3-x") && !is_key("In progress"));
    }

    #[test]
    fn texts() {
        assert_eq!(spaced("1h30m"), "1h 30m");
        assert_eq!(spaced("45m"), "45m");
        assert_eq!(minutes(90), "1h 30m");
        assert_eq!(table(&[vec!["a".into(), "bb".into()], vec!["ccc".into(), "d".into()]]), "a    bb\nccc  d");
        let doc = json!({"type": "doc", "content": [{"type": "paragraph", "content": [{"type": "text", "text": "Hello "}, {"type": "mention", "attrs": {"label": "Mia"}}]},
            {"type": "bulletList", "content": [{"type": "listItem", "content": [{"type": "paragraph", "content": [{"type": "text", "text": "one"}]}]}]}]});
        assert_eq!(doc_text(&doc), "Hello @Mia\n- one");
        assert_eq!(clean_url("tasks.example.com/api/").unwrap(), "https://tasks.example.com");
    }

    /// The client and the API in step: every operation the client uses is in the server's
    /// OpenAPI description.
    #[test]
    fn every_endpoint_is_in_the_api_description() {
        let spec: Value = serde_json::from_str(include_str!("../../server/openapi.json")).unwrap();
        for (method, path) in ENDPOINTS {
            assert!(spec["paths"][path][method.to_lowercase()].is_object(), "{method} {path} isn't in crates/server/openapi.json");
        }
    }
}
