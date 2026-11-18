//! The runner of TaskCraft's AI coding agents.
//!
//! It asks the server for a run, downloads the repository through the server (it holds no Git
//! token), lets the agent work in a checkout — the built-in agent, which talks to the model
//! through the server (it holds no model key either), or a command of your own — and hands the
//! changed files back. The server makes the branch, the commit and the draft pull request.
//!
//! The built-in agent is deliberately small: the model replies with one JSON object per turn
//! (`list`, `read`, `search`, `write`, `delete`, optionally `run`, and `finish`), the runner
//! does it inside the checkout and reports the result. Paths never leave the checkout; commands
//! run only when the runner was started with `--allow-commands`, which belongs in a container
//! that holds nothing else.

use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use serde_json::{Value, json};

const MAX_FILE: u64 = 1024 * 1024;
const MAX_RESULT_CHARS: usize = 20_000;
const MAX_CONTEXT_CHARS: usize = 400_000;
const COMMAND_SECONDS: u64 = 300;
const EXTERNAL_SECONDS: u64 = 1800;

pub struct Runner {
    pub server: String,
    pub token: String,
    pub allow_commands: bool,
    /// a command that does the work instead of the built-in agent
    pub command: Option<String>,
    pub max_steps: usize,
    /// prints a line of what the runner does
    pub out: Arc<dyn Fn(String) + Send + Sync>,
    http: reqwest::Client,
}

fn http() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent(concat!("taskcraft-runner/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(600))
        .build()
        .expect("an HTTP client")
}

fn said(status: u16, v: &Value) -> String {
    v["message"].as_str().map(str::to_string).unwrap_or_else(|| format!("the server answered HTTP {status}"))
}

/// Trades a registration token for the runner's own: (the runner's name, its token).
pub async fn register(server: &str, registration: &str, name: Option<&str>) -> Result<(String, String), String> {
    let res = http()
        .post(format!("{}/api/v1/agent-runner/register", server.trim_end_matches('/')))
        .json(&json!({"token": registration, "name": name}))
        .send()
        .await
        .map_err(|e| format!("can't reach {server}: {}", e.without_url()))?;
    let status = res.status().as_u16();
    let v: Value = res.json().await.unwrap_or(Value::Null);
    match (status, v["token"].as_str()) {
        (200, Some(t)) => Ok((v["name"].as_str().unwrap_or("runner").to_string(), t.to_string())),
        (401, _) => Err("the registration token isn't accepted (it works once: add the runner again in Administration → Agents)".into()),
        _ => Err(said(status, &v)),
    }
}

/// A run was cancelled (or timed out) on the server: nothing more to do for it.
struct Ended;

/// One run in progress.
struct Job<'a> {
    runner: &'a Runner,
    id: String,
    token: String,
}

impl Job<'_> {
    fn url(&self, what: &str) -> String {
        format!("{}/api/v1/agent-runs/{}/{what}", self.runner.server, self.id)
    }

    async fn post(&self, what: &str, body: Value) -> Result<(u16, Value), String> {
        let res = self
            .runner
            .http
            .post(self.url(what))
            .bearer_auth(&self.token)
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("can't reach the server: {}", e.without_url()))?;
        let status = res.status().as_u16();
        Ok((status, res.json().await.unwrap_or(Value::Null)))
    }

    /// A line in the run's log, on the issue's page.
    async fn log(&self, line: impl Into<String>) -> Result<(), Ended> {
        let line = line.into();
        (self.runner.out)(format!("[{}] {line}", &self.id[..8.min(self.id.len())]));
        match self.post("log", json!({"lines": [line]})).await {
            Ok((409, _)) => Err(Ended),
            _ => Ok(()),
        }
    }

    async fn ask(&self, system: &str, messages: &[(String, String)]) -> Result<Result<String, String>, Ended> {
        let body = json!({"system": system, "messages": messages.iter().map(|(role, content)| json!({"role": role, "content": content})).collect::<Vec<_>>()});
        match self.post("model", body).await {
            Ok((200, v)) => Ok(Ok(v["text"].as_str().unwrap_or("").to_string())),
            Ok((409, _)) => Err(Ended),
            Ok((status, v)) => Ok(Err(said(status, &v))),
            Err(e) => Ok(Err(e)),
        }
    }
}

// ---- the checkout ---------------------------------------------------------------------------------------

/// A path inside the checkout, or nothing when it would leave it.
pub fn inside(root: &Path, path: &str) -> Option<PathBuf> {
    let p = Path::new(path.trim());
    if p.is_absolute() {
        return None;
    }
    let mut out = root.to_path_buf();
    for c in p.components() {
        match c {
            Component::Normal(s) if s != ".git" => out.push(s),
            Component::CurDir => {}
            _ => return None,
        }
    }
    Some(out)
}

/// Unpacks a repository archive (.tar.gz with one folder at the top) into `dir`: plain files and
/// folders only, nothing outside `dir`.
pub fn unpack(archive: &[u8], dir: &Path) -> Result<usize, String> {
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(archive));
    let mut files = 0;
    for entry in tar.entries().map_err(|e| format!("the repository's archive can't be read: {e}"))? {
        let mut entry = entry.map_err(|e| format!("the repository's archive can't be read: {e}"))?;
        let path = entry.path().map_err(|e| e.to_string())?.into_owned();
        // without the archive's top folder
        let rel: PathBuf = path.components().skip(1).collect();
        let Some(rel) = rel.to_str().filter(|r| !r.is_empty()) else { continue };
        let Some(target) = inside(dir, rel) else { continue };
        match entry.header().entry_type() {
            tar::EntryType::Directory => std::fs::create_dir_all(&target).map_err(|e| e.to_string())?,
            tar::EntryType::Regular => {
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                }
                let mut out = std::fs::File::create(&target).map_err(|e| e.to_string())?;
                std::io::copy(&mut entry, &mut out).map_err(|e| e.to_string())?;
                files += 1;
            }
            // links and devices stay out
            _ => {}
        }
    }
    Ok(files)
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(read) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<_> = read.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let Ok(kind) = e.file_type() else { continue };
        let path = e.path();
        if kind.is_dir() {
            if e.file_name() != ".git" {
                walk(root, &path, out);
            }
        } else if kind.is_file()
            && let Ok(rel) = path.strip_prefix(root)
        {
            out.push(rel.to_path_buf());
        }
    }
}

/// Every file of the checkout with a fingerprint of its content.
pub fn snapshot(root: &Path) -> BTreeMap<String, u64> {
    let mut files = Vec::new();
    walk(root, root, &mut files);
    let mut out = BTreeMap::new();
    for rel in files {
        let Some(name) = rel.to_str().map(|s| s.replace('\\', "/")) else { continue };
        let Ok(bytes) = std::fs::read(root.join(&rel)) else { continue };
        let mut h = std::collections::hash_map::DefaultHasher::new();
        bytes.hash(&mut h);
        out.insert(name, h.finish());
    }
    out
}

/// What changed in the checkout since `before`: `[{path, status, content}]` for the server.
pub fn changes(root: &Path, before: &BTreeMap<String, u64>) -> Result<Vec<Value>, String> {
    let after = snapshot(root);
    let mut out = Vec::new();
    for (path, hash) in &after {
        let status = match before.get(path) {
            None => "added",
            Some(old) if old != hash => "modified",
            _ => continue,
        };
        let bytes = std::fs::read(root.join(path)).map_err(|e| e.to_string())?;
        if bytes.len() as u64 > MAX_FILE {
            return Err(format!("{path} is larger than 1 MB: the agent can't hand that in"));
        }
        out.push(json!({"path": path, "status": status, "content": base64::engine::general_purpose::STANDARD.encode(&bytes)}));
    }
    for path in before.keys().filter(|p| !after.contains_key(*p)) {
        out.push(json!({"path": path, "status": "deleted"}));
    }
    Ok(out)
}

fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    format!("{}\n… (cut: {} characters in all)", s.chars().take(max).collect::<String>(), s.chars().count())
}

fn list(root: &Path, path: &str) -> String {
    let Some(dir) = inside(root, path) else { return "That path is outside the repository.".into() };
    let Ok(read) = std::fs::read_dir(&dir) else { return format!("There is no folder {path}.") };
    let mut names: Vec<String> = read
        .flatten()
        .filter(|e| e.file_name() != ".git")
        .map(|e| format!("{}{}", e.file_name().to_string_lossy(), if e.file_type().is_ok_and(|t| t.is_dir()) { "/" } else { "" }))
        .collect();
    names.sort();
    if names.is_empty() { "(empty)".into() } else { names.join("\n") }
}

fn search(root: &Path, query: &str) -> String {
    if query.trim().is_empty() {
        return "Say what to search for.".into();
    }
    let mut files = Vec::new();
    walk(root, root, &mut files);
    let mut hits = Vec::new();
    for rel in files {
        let Ok(text) = std::fs::read_to_string(root.join(&rel)) else { continue };
        for (n, line) in text.lines().enumerate() {
            if line.contains(query) {
                hits.push(format!("{}:{}: {}", rel.display(), n + 1, clip(line.trim(), 200)));
                if hits.len() >= 100 {
                    hits.push("… (more matches: search for something narrower)".into());
                    return hits.join("\n");
                }
            }
        }
    }
    if hits.is_empty() { format!("Nothing contains {query:?}.") } else { hits.join("\n") }
}

async fn shell(root: &Path, command: &str, seconds: u64, env: &[(&str, String)]) -> String {
    let mut cmd = tokio::process::Command::new("sh");
    cmd.arg("-c").arg(command).current_dir(root).stdin(std::process::Stdio::null()).kill_on_drop(true);
    for (k, v) in env {
        cmd.env(k, v);
    }
    match tokio::time::timeout(Duration::from_secs(seconds), cmd.output()).await {
        Err(_) => format!("The command was stopped after {seconds} seconds."),
        Ok(Err(e)) => format!("The command couldn't start: {e}"),
        Ok(Ok(out)) => {
            let mut text = String::from_utf8_lossy(&out.stdout).to_string();
            let err = String::from_utf8_lossy(&out.stderr);
            if !err.trim().is_empty() {
                text.push_str(&format!("\n[stderr]\n{err}"));
            }
            format!("exit code {}\n{}", out.status.code().unwrap_or(-1), text.trim())
        }
    }
}

/// The JSON object in a model's reply (models sometimes wrap it in a code fence or a sentence).
pub fn action_of(reply: &str) -> Option<Value> {
    let (start, end) = (reply.find('{')?, reply.rfind('}')?);
    if end < start {
        return None;
    }
    serde_json::from_str::<Value>(&reply[start..=end]).ok().filter(|v| v["tool"].is_string())
}

/// How a run ended on the runner's side.
#[derive(Debug, Default)]
struct Outcome {
    summary: Option<String>,
    answer: Option<String>,
}

fn task_text(job: &Value) -> String {
    let issue = &job["issue"];
    let mut t = format!("# {} {}\n\n", issue["key"].as_str().unwrap_or(""), issue["title"].as_str().unwrap_or(""));
    t.push_str(&format!(
        "Type: {} · Status: {} · Priority: {}\n",
        issue["type"].as_str().unwrap_or(""),
        issue["status"].as_str().unwrap_or(""),
        issue["priority"].as_str().unwrap_or("")
    ));
    let description = issue["description"].as_str().unwrap_or("").trim();
    t.push_str(&format!("\n## Description\n\n{}\n", if description.is_empty() { "(none)" } else { description }));
    let comments = issue["comments"].as_array().cloned().unwrap_or_default();
    if !comments.is_empty() {
        t.push_str("\n## Comments (oldest first)\n\n");
        for c in &comments {
            t.push_str(&format!("**{}**: {}\n\n", c["author"].as_str().unwrap_or("Someone"), c["text"].as_str().unwrap_or("").trim()));
        }
    }
    if let Some(p) = job["prompt"].as_str().filter(|p| !p.trim().is_empty()) {
        t.push_str(&format!("\n## What you were asked in the comment that mentioned you\n\n{}\n", p.trim()));
    }
    t
}

impl Runner {
    pub fn new(server: &str, token: &str) -> Runner {
        Runner {
            server: server.trim_end_matches('/').to_string(),
            token: token.to_string(),
            allow_commands: false,
            command: None,
            max_steps: 40,
            out: Arc::new(|_| {}),
            http: http(),
        }
    }

    /// Claims one run and works on it. `Ok(false)`: nothing was waiting.
    pub async fn work_once(&self) -> Result<bool, String> {
        let res = self
            .http
            .post(format!("{}/api/v1/agent-runner/claim", self.server))
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|e| format!("can't reach {}: {}", self.server, e.without_url()))?;
        match res.status().as_u16() {
            204 => return Ok(false),
            200 => {}
            401 => return Err("the server doesn't know this runner (any more): register it again".into()),
            s => return Err(format!("the server answered HTTP {s}")),
        }
        let job: Value = res.json().await.map_err(|e| e.to_string())?;
        let run = Job { runner: self, id: job["run"]["id"].as_str().unwrap_or("").to_string(), token: job["run"]["token"].as_str().unwrap_or("").to_string() };
        let dir = tempfile::tempdir().map_err(|e| format!("no working folder: {e}"))?;
        match self.work(&run, &job, dir.path()).await {
            // cancelled on the server: it knows
            Err(Ended) => (self.out)(format!("[{}] ended on the server", &run.id[..8.min(run.id.len())])),
            Ok(Err(error)) => {
                let _ = run.post("finish", json!({"error": error})).await;
            }
            Ok(Ok(body)) => {
                let (status, v) = run.post("finish", body).await?;
                if status != 200 && status != 409 {
                    (self.out)(format!("the result wasn't accepted: {}", said(status, &v)));
                }
            }
        }
        Ok(true)
    }

    /// The work of one run; the body of its `finish`, or why it failed.
    async fn work(&self, run: &Job<'_>, job: &Value, dir: &Path) -> Result<Result<Value, String>, Ended> {
        let repo = job["repository"]["name"].as_str();
        let mut before = BTreeMap::new();
        if let Some(name) = repo {
            run.log(format!("Fetching {name} ({}).", job["repository"]["baseBranch"].as_str().unwrap_or("main"))).await?;
            let res = match self.http.get(run.url("source")).bearer_auth(&run.token).send().await {
                Ok(r) => r,
                Err(e) => return Ok(Err(format!("the repository couldn't be fetched: {}", e.without_url()))),
            };
            if res.status().as_u16() == 409 {
                return Err(Ended);
            }
            if !res.status().is_success() {
                let status = res.status().as_u16();
                return Ok(Err(format!("the repository couldn't be fetched: {}", said(status, &res.json().await.unwrap_or(Value::Null)))));
            }
            let bytes = match res.bytes().await {
                Ok(b) => b,
                Err(e) => return Ok(Err(format!("the repository couldn't be fetched: {}", e.without_url()))),
            };
            let (archive, target) = (bytes.to_vec(), dir.to_path_buf());
            match tokio::task::spawn_blocking(move || unpack(&archive, &target)).await {
                Ok(Ok(files)) => run.log(format!("{files} files.")).await?,
                Ok(Err(e)) => return Ok(Err(e)),
                Err(_) => return Ok(Err("unpacking the repository crashed".into())),
            }
            before = snapshot(dir);
        }
        let outcome = match &self.command {
            Some(command) => self.external(run, job, dir, command).await?,
            None => self.agent(run, job, dir, repo.is_some()).await?,
        };
        let outcome = match outcome {
            Ok(o) => o,
            Err(e) => return Ok(Err(e)),
        };
        let files = match changes(dir, &before) {
            Ok(f) => f,
            Err(e) => return Ok(Err(e)),
        };
        if !files.is_empty() {
            run.log(format!("{} files changed.", files.len())).await?;
        }
        Ok(Ok(json!({"summary": outcome.summary, "answer": outcome.answer, "files": files})))
    }

    /// Your own agent: a command that works in the checkout.
    async fn external(&self, run: &Job<'_>, job: &Value, dir: &Path, command: &str) -> Result<Result<Outcome, String>, Ended> {
        let scratch = match tempfile::tempdir() {
            Ok(d) => d,
            Err(e) => return Ok(Err(e.to_string())),
        };
        let (task, result) = (scratch.path().join("TASK.md"), scratch.path().join("RESULT.md"));
        let instructions = job["agent"]["instructions"].as_str().unwrap_or("");
        if let Err(e) = std::fs::write(&task, format!("{}\n\n## Instructions\n\n{instructions}\n", task_text(job))) {
            return Ok(Err(e.to_string()));
        }
        run.log("Handing the work to the runner's command.").await?;
        let env = [
            ("TASKCRAFT_TASK_FILE", task.display().to_string()),
            ("TASKCRAFT_RESULT_FILE", result.display().to_string()),
            ("TASKCRAFT_ISSUE", job["issue"]["key"].as_str().unwrap_or("").to_string()),
        ];
        let output = shell(dir, command, EXTERNAL_SECONDS, &env).await;
        for line in output.lines().rev().take(40).collect::<Vec<_>>().into_iter().rev() {
            run.log(clip(line, 500)).await?;
        }
        if !output.starts_with("exit code 0") {
            return Ok(Err(format!("the runner's command failed: {}", clip(output.lines().next().unwrap_or(""), 200))));
        }
        let said = std::fs::read_to_string(&result).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
        Ok(Ok(Outcome { summary: said.clone(), answer: said }))
    }

    /// The built-in agent: the model acts through one JSON object per turn.
    async fn agent(&self, run: &Job<'_>, job: &Value, dir: &Path, has_repo: bool) -> Result<Result<Outcome, String>, Ended> {
        let name = job["agent"]["name"].as_str().unwrap_or("the agent");
        let mut system = format!(
            "You are {name}, a software engineer working on an issue from the team's tracker{}.\n\n\
             You act by replying with exactly ONE JSON object and nothing else. The tools:\n",
            if has_repo { ", in a checkout of the team's repository" } else { "" }
        );
        if has_repo {
            system.push_str(
                "{\"tool\": \"list\", \"path\": \".\"} — the files and folders in a folder\n\
                 {\"tool\": \"read\", \"path\": \"src/main.rs\"} — a file's content\n\
                 {\"tool\": \"search\", \"query\": \"exact text\"} — the lines containing a text, across the repository\n\
                 {\"tool\": \"write\", \"path\": \"src/main.rs\", \"content\": \"…\"} — create or replace a file (always the whole file)\n\
                 {\"tool\": \"delete\", \"path\": \"old.txt\"} — remove a file\n",
            );
            if self.allow_commands {
                system.push_str("{\"tool\": \"run\", \"command\": \"cargo test\"} — run a shell command in the checkout (to build or test)\n");
            }
            system.push_str(
                "{\"tool\": \"finish\", \"summary\": \"…\"} — you're done: what you changed and why, in Markdown, for the pull request and the issue\n\
                 {\"tool\": \"finish\", \"answer\": \"…\"} — the issue is a question, or needs no change: your answer, in Markdown\n\n\
                 Look before you change: list and read what's relevant first. Keep the change as small as the issue needs and in the style of the code around it. \
                 Don't touch what the issue doesn't ask for. When you're done, finish — your changes become a draft pull request for the team to review.\n",
            );
        } else {
            system.push_str(
                "{\"tool\": \"finish\", \"answer\": \"…\"} — your answer to the issue, in Markdown (no repository is connected, so nothing can be changed)\n",
            );
        }
        if let Some(i) = job["agent"]["instructions"].as_str().filter(|i| !i.trim().is_empty()) {
            system.push_str(&format!("\nThe team's instructions for you:\n{}\n", i.trim()));
        }
        let mut first = task_text(job);
        if has_repo {
            first.push_str(&format!("\n## The repository's top folder\n\n{}\n", list(dir, ".")));
        }
        let mut messages: Vec<(String, String)> = vec![("user".into(), first)];
        for step in 1..=self.max_steps {
            // an old conversation makes room: the oldest results go first
            let mut total: usize = messages.iter().map(|m| m.1.len()).sum();
            for m in messages.iter_mut().skip(1) {
                if total <= MAX_CONTEXT_CHARS {
                    break;
                }
                if m.0 == "user" && m.1.len() > 200 {
                    total -= m.1.len();
                    m.1 = "(an earlier result, left out to save room)".into();
                }
            }
            let reply = match run.ask(&system, &messages).await? {
                Ok(r) => r,
                Err(e) => return Ok(Err(format!("the model couldn't be asked: {e}"))),
            };
            let Some(action) = action_of(&reply) else {
                messages.push(("assistant".into(), reply));
                messages.push(("user".into(), "Reply with exactly one JSON object with a \"tool\", as described.".into()));
                continue;
            };
            let path = action["path"].as_str().unwrap_or("");
            let result = match action["tool"].as_str().unwrap_or("") {
                "finish" => {
                    let text = |k: &str| action[k].as_str().map(str::trim).filter(|t| !t.is_empty()).map(str::to_string);
                    return Ok(Ok(Outcome { summary: text("summary"), answer: text("answer") }));
                }
                _ if !has_repo => "No repository is connected: finish with your answer.".to_string(),
                "list" => {
                    run.log(format!("Looking at {}", if path.is_empty() { "." } else { path })).await?;
                    list(dir, if path.is_empty() { "." } else { path })
                }
                "read" => {
                    run.log(format!("Reading {path}")).await?;
                    match inside(dir, path) {
                        None => "That path is outside the repository.".into(),
                        Some(p) => match std::fs::read(&p) {
                            Err(_) => format!("There is no file {path}."),
                            Ok(b) if b.len() as u64 > MAX_FILE => "The file is larger than 1 MB.".into(),
                            Ok(b) => match String::from_utf8(b) {
                                Ok(t) => clip(&t, MAX_RESULT_CHARS),
                                Err(_) => "The file isn't text.".into(),
                            },
                        },
                    }
                }
                "search" => {
                    let q = action["query"].as_str().unwrap_or("");
                    run.log(format!("Searching for {q:?}")).await?;
                    clip(&search(dir, q), MAX_RESULT_CHARS)
                }
                "write" => {
                    run.log(format!("Writing {path}")).await?;
                    match (inside(dir, path), action["content"].as_str()) {
                        (None, _) => "That path is outside the repository.".into(),
                        (_, None) => "write needs \"content\": the whole file.".into(),
                        (Some(p), Some(content)) => {
                            let made = p.parent().map(std::fs::create_dir_all).transpose().and_then(|_| std::fs::write(&p, content));
                            match made {
                                Ok(()) => format!("Wrote {path} ({} bytes).", content.len()),
                                Err(e) => format!("Couldn't write {path}: {e}"),
                            }
                        }
                    }
                }
                "delete" => {
                    run.log(format!("Deleting {path}")).await?;
                    match inside(dir, path).map(std::fs::remove_file) {
                        Some(Ok(())) => format!("Deleted {path}."),
                        _ => format!("There is no file {path}."),
                    }
                }
                "run" if self.allow_commands => {
                    let command = action["command"].as_str().unwrap_or("");
                    run.log(format!("Running {}", clip(command, 200))).await?;
                    clip(&shell(dir, command, COMMAND_SECONDS, &[]).await, MAX_RESULT_CHARS)
                }
                "run" => "This runner doesn't run commands. Work with list, read, search, write and delete.".into(),
                other => format!("There is no tool {other:?}."),
            };
            messages.push(("assistant".into(), action.to_string()));
            let left = self.max_steps - step;
            messages.push(("user".into(), if left <= 3 { format!("{result}\n\n({left} steps left: finish soon.)") } else { result }));
        }
        Ok(Err(format!("the agent didn't finish within {} steps", self.max_steps)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_stay_inside() {
        let root = Path::new("/work/checkout");
        assert_eq!(inside(root, "src/main.rs"), Some(root.join("src/main.rs")));
        assert_eq!(inside(root, "./a/./b"), Some(root.join("a/b")));
        for bad in ["/etc/passwd", "../secret", "a/../../b", ".git/config", "x/.git/hooks"] {
            assert_eq!(inside(root, bad), None, "{bad}");
        }
    }

    #[test]
    fn actions() {
        assert_eq!(action_of("{\"tool\": \"list\", \"path\": \".\"}").unwrap()["tool"], "list");
        assert_eq!(action_of("Sure!\n```json\n{\"tool\": \"read\", \"path\": \"a {b}.txt\"}\n```").unwrap()["path"], "a {b}.txt");
        assert!(action_of("I think we should read the file.").is_none());
        assert!(action_of("{\"path\": \"x\"}").is_none(), "no tool");
    }

    #[test]
    fn archives_and_changes() {
        // an archive like the Git servers hand out: one folder at the top, and an entry that tries to get out
        let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default()));
        for (path, content) in [("repo-main/README.md", "# Hello\n"), ("repo-main/src/lib.rs", "pub fn one() -> u8 { 1 }\n"), ("repo-main/old.txt", "old\n")] {
            let mut header = tar::Header::new_gnu();
            header.set_size(content.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append_data(&mut header, path, content.as_bytes()).unwrap();
        }
        let archive = builder.into_inner().unwrap().finish().unwrap();
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(unpack(&archive, dir.path()).unwrap(), 3);
        assert_eq!(std::fs::read_to_string(dir.path().join("src/lib.rs")).unwrap(), "pub fn one() -> u8 { 1 }\n");
        let before = snapshot(dir.path());
        assert!(changes(dir.path(), &before).unwrap().is_empty());
        std::fs::write(dir.path().join("src/lib.rs"), "pub fn one() -> u8 { 2 }\n").unwrap();
        std::fs::write(dir.path().join("NEW.md"), "new\n").unwrap();
        std::fs::remove_file(dir.path().join("old.txt")).unwrap();
        let mut what: Vec<(String, String)> = changes(dir.path(), &before)
            .unwrap()
            .iter()
            .map(|c| (c["path"].as_str().unwrap().to_string(), c["status"].as_str().unwrap().to_string()))
            .collect();
        what.sort();
        assert_eq!(what, [("NEW.md".into(), "added".into()), ("old.txt".into(), "deleted".into()), ("src/lib.rs".into(), "modified".into())]);
        assert!(search(dir.path(), "one()").contains("src/lib.rs:1"));
        assert_eq!(list(dir.path(), "."), "NEW.md\nREADME.md\nsrc/");
    }
}
