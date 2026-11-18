//! The Git servers' APIs (GitHub, GitLab, Gitea / Forgejo): what "Create branch" on an issue
//! needs — the repository's default branch, a new branch, and optionally a draft pull / merge
//! request for it. A connected repository carries the token (`git_integrations.api_token`).
//!
//! Requests go to the address derived from the repository's (`api.github.com`, `<host>/api/v3`
//! for GitHub Enterprise, `<host>/api/v4` for GitLab, `<host>/api/v1` for Gitea) unless the
//! connection names another, are pinned to a checked address like webhook deliveries, and never
//! follow redirects.

use std::time::Duration;

use reqwest::Method;
use reqwest::Url;
use serde_json::{Value, json};

use crate::app::AppState;

pub const KINDS: &[&str] = &["github", "gitlab", "gitea"];

/// A repository on its server.
#[derive(Debug, Clone)]
pub struct Repo {
    pub kind: String,
    /// the repository's web address, without a trailing slash
    pub web: String,
    api: String,
    /// `owner/repo` (GitLab: the whole path, groups included)
    path: String,
    token: String,
}

/// What went wrong, in words for the person who clicked.
#[derive(Debug)]
pub enum GitError {
    /// the branch (or pull request) exists already
    Exists(String),
    Other(String),
}

impl std::fmt::Display for GitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GitError::Exists(m) | GitError::Other(m) => f.write_str(m),
        }
    }
}

type GitResult<T> = Result<T, GitError>;

fn enc(s: &str) -> String {
    crate::sso::urlencode(s)
}

/// A branch name in a URL path: slashes stay.
fn enc_ref(s: &str) -> String {
    s.split('/').map(enc).collect::<Vec<_>>().join("/")
}

impl Repo {
    /// `repo_url`: the repository's web address; `api_url`: the API's, when it isn't the usual one.
    pub fn new(kind: &str, repo_url: &str, api_url: Option<&str>, token: &str) -> Result<Repo, String> {
        if !KINDS.contains(&kind) {
            return Err("the server is github, gitlab or gitea".into());
        }
        let u = Url::parse(repo_url).map_err(|_| "the repository's address isn't a URL".to_string())?;
        let host = u.host_str().ok_or("the repository's address needs a host")?;
        let origin = match u.port() {
            Some(p) => format!("{}://{host}:{p}", u.scheme()),
            None => format!("{}://{host}", u.scheme()),
        };
        let path = u.path().trim_matches('/').trim_end_matches(".git").to_string();
        if path.split('/').filter(|s| !s.is_empty()).count() < 2 {
            return Err("the repository's address needs an owner and a name (https://host/owner/repository)".into());
        }
        let api = match api_url.map(str::trim).filter(|a| !a.is_empty()) {
            Some(a) => {
                let a = Url::parse(a).map_err(|_| "the API address isn't a URL".to_string())?;
                if !matches!(a.scheme(), "http" | "https") || !a.username().is_empty() || a.password().is_some() {
                    return Err("the API address starts with https:// and carries no credentials".into());
                }
                a.as_str().trim_end_matches('/').to_string()
            }
            None => match kind {
                "github" if host.eq_ignore_ascii_case("github.com") => "https://api.github.com".into(),
                "github" => format!("{origin}/api/v3"),
                "gitlab" => format!("{origin}/api/v4"),
                _ => format!("{origin}/api/v1"),
            },
        };
        Ok(Repo { kind: kind.into(), web: repo_url.trim_end_matches('/').to_string(), api, path, token: token.trim().to_string() })
    }

    fn repo_api(&self) -> String {
        match self.kind.as_str() {
            "gitlab" => format!("{}/projects/{}", self.api, enc(&self.path)),
            _ => format!("{}/repos/{}", self.api, self.path),
        }
    }

    /// Where a branch shows on the web (the same address its pushes are linked under).
    pub fn branch_url(&self, name: &str) -> String {
        match self.kind.as_str() {
            "gitlab" => format!("{}/-/tree/{name}", self.web),
            _ => format!("{}/tree/{name}", self.web),
        }
    }

    async fn call(&self, state: &AppState, method: Method, url: &str, body: Option<Value>) -> GitResult<(u16, Value)> {
        let other = |m: String| GitError::Other(m);
        let u = Url::parse(url).map_err(|e| other(e.to_string()))?;
        let host = u.host_str().unwrap_or_default().trim_start_matches('[').trim_end_matches(']').to_string();
        let port = u.port_or_known_default().unwrap_or(443);
        let addrs: Vec<std::net::SocketAddr> =
            tokio::net::lookup_host((host.as_str(), port)).await.map_err(|e| other(format!("can't resolve {host}: {e}")))?.collect();
        let first = *addrs.first().ok_or_else(|| other(format!("{host} has no address")))?;
        if !state.config.webhook_allow_private
            && let Some(bad) = addrs.iter().find(|a| crate::webhooks::is_private(a.ip()))
        {
            return Err(other(format!(
                "{host} resolves to a private address ({}); set TASKCRAFT_WEBHOOK_ALLOW_PRIVATE=true for a Git server on your network",
                bad.ip()
            )));
        }
        let client = reqwest::Client::builder()
            .resolve(&host, first)
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(15))
            .user_agent(concat!("TaskCraft/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| other(e.to_string()))?;
        let mut req = client.request(method, u).header("accept", if self.kind == "github" { "application/vnd.github+json" } else { "application/json" });
        req = match self.kind.as_str() {
            "gitlab" => req.header("private-token", &self.token),
            "gitea" => req.header("authorization", format!("token {}", self.token)),
            _ => req.header("authorization", format!("Bearer {}", self.token)).header("x-github-api-version", "2022-11-28"),
        };
        if let Some(b) = body {
            req = req.json(&b);
        }
        let res = req.send().await.map_err(|e| other(format!("the Git server didn't answer: {}", e.without_url())))?;
        let status = res.status().as_u16();
        let v: Value = res.json().await.unwrap_or(Value::Null);
        Ok((status, v))
    }

    /// The server's own words about a refusal.
    fn said(v: &Value) -> String {
        let m = &v["message"];
        let text = match m {
            Value::String(s) => s.clone(),
            Value::Array(a) => a.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join("; "),
            Value::Object(_) => m.to_string(),
            _ => v["error"].as_str().unwrap_or("").to_string(),
        };
        let more: Vec<&str> = v["errors"].as_array().into_iter().flatten().filter_map(|e| e["message"].as_str().or_else(|| e.as_str())).collect();
        let text = if more.is_empty() { text } else { format!("{text} ({})", more.join("; ")) };
        text.chars().take(300).collect()
    }

    fn refused(&self, what: &str, status: u16, v: &Value) -> GitError {
        let said = Self::said(v);
        let hint = match status {
            401 => "the token isn't accepted".to_string(),
            403 => "the token may not do this (it needs write access to the repository's code and pull requests)".to_string(),
            404 => "the repository wasn't found with this token".to_string(),
            _ => format!("HTTP {status}"),
        };
        GitError::Other(if said.is_empty() { format!("{what}: {hint}") } else { format!("{what}: {hint} — {said}") })
    }

    /// The repository's default branch (also: the token works).
    pub async fn default_branch(&self, state: &AppState) -> GitResult<String> {
        let (status, v) = self.call(state, Method::GET, &self.repo_api(), None).await?;
        if status != 200 {
            return Err(self.refused("Reading the repository", status, &v));
        }
        Ok(v["default_branch"].as_str().filter(|b| !b.is_empty()).unwrap_or("main").to_string())
    }

    /// The commit a branch points to (GitHub).
    async fn head(&self, state: &AppState, branch: &str) -> GitResult<String> {
        let (status, v) = self.call(state, Method::GET, &format!("{}/git/ref/heads/{}", self.repo_api(), enc_ref(branch)), None).await?;
        match (status, v["object"]["sha"].as_str()) {
            (200, Some(sha)) => Ok(sha.to_string()),
            (404, _) => Err(GitError::Other(format!("there is no branch {branch} to start from"))),
            _ => Err(self.refused("Reading the base branch", status, &v)),
        }
    }

    /// Creates `name` from `base`.
    pub async fn create_branch(&self, state: &AppState, name: &str, base: &str) -> GitResult<()> {
        let exists = || GitError::Exists(format!("the branch {name} exists already"));
        let (status, v) = match self.kind.as_str() {
            "github" => {
                let sha = self.head(state, base).await?;
                self.call(state, Method::POST, &format!("{}/git/refs", self.repo_api()), Some(json!({"ref": format!("refs/heads/{name}"), "sha": sha}))).await?
            }
            "gitlab" => {
                self.call(state, Method::POST, &format!("{}/repository/branches?branch={}&ref={}", self.repo_api(), enc(name), enc(base)), None).await?
            }
            _ => {
                self.call(state, Method::POST, &format!("{}/branches", self.repo_api()), Some(json!({"new_branch_name": name, "old_branch_name": base})))
                    .await?
            }
        };
        let said = Self::said(&v).to_lowercase();
        match status {
            200 | 201 => Ok(()),
            409 => Err(exists()),
            400 | 422 if said.contains("exist") => Err(exists()),
            404 if self.kind != "github" => {
                Err(GitError::Other(format!("there is no branch {base} to start from (or the repository wasn't found with this token)")))
            }
            _ => Err(self.refused("Creating the branch", status, &v)),
        }
    }

    /// Opens a draft pull / merge request from `branch` into `base`; (number, web address).
    ///
    /// GitHub only opens a pull request for a branch with a commit of its own, so an empty one
    /// (`message`) is added first.
    pub async fn draft_pull_request(
        &self,
        state: &AppState,
        branch: &str,
        base: &str,
        title: &str,
        body: &str,
        message: &str,
    ) -> GitResult<(Option<i64>, String)> {
        if self.kind == "github" {
            let sha = self.head(state, branch).await?;
            let (s, c) = self.call(state, Method::GET, &format!("{}/git/commits/{sha}", self.repo_api()), None).await?;
            let tree = c["tree"]["sha"].as_str().ok_or_else(|| self.refused("Reading the branch", s, &c))?.to_string();
            let (s, c) = self
                .call(state, Method::POST, &format!("{}/git/commits", self.repo_api()), Some(json!({"message": message, "tree": tree, "parents": [sha]})))
                .await?;
            let commit = c["sha"].as_str().ok_or_else(|| self.refused("Starting the branch", s, &c))?.to_string();
            let (s, r) =
                self.call(state, Method::PATCH, &format!("{}/git/refs/heads/{}", self.repo_api(), enc_ref(branch)), Some(json!({"sha": commit}))).await?;
            if s != 200 {
                return Err(self.refused("Starting the branch", s, &r));
            }
        }
        self.open_pull_request(state, branch, base, title, body).await
    }

    /// Opens a draft pull / merge request for a branch that has commits of its own.
    pub async fn open_pull_request(&self, state: &AppState, branch: &str, base: &str, title: &str, body: &str) -> GitResult<(Option<i64>, String)> {
        let (status, v) = match self.kind.as_str() {
            "github" => {
                self.call(
                    state,
                    Method::POST,
                    &format!("{}/pulls", self.repo_api()),
                    Some(json!({"title": title, "head": branch, "base": base, "body": body, "draft": true})),
                )
                .await?
            }
            "gitlab" => {
                self.call(
                    state,
                    Method::POST,
                    &format!("{}/merge_requests", self.repo_api()),
                    Some(json!({"source_branch": branch, "target_branch": base, "title": format!("Draft: {title}"), "description": body})),
                )
                .await?
            }
            _ => {
                self.call(
                    state,
                    Method::POST,
                    &format!("{}/pulls", self.repo_api()),
                    Some(json!({"title": format!("WIP: {title}"), "head": branch, "base": base, "body": body})),
                )
                .await?
            }
        };
        if !matches!(status, 200 | 201) {
            if status == 409 || Self::said(&v).to_lowercase().contains("already exists") {
                return Err(GitError::Exists("a pull request for this branch exists already".into()));
            }
            return Err(self.refused("Opening the pull request", status, &v));
        }
        let number = if self.kind == "gitlab" { v["iid"].as_i64() } else { v["number"].as_i64() };
        let url =
            v["html_url"].as_str().or_else(|| v["web_url"].as_str()).map(str::to_string).or_else(|| {
                number.map(|n| if self.kind == "gitlab" { format!("{}/-/merge_requests/{n}", self.web) } else { format!("{}/pull/{n}", self.web) })
            });
        Ok((number, url.ok_or_else(|| GitError::Other("the Git server opened the pull request but didn't say where".into()))?))
    }
}

/// One file of a commit made through the API.
#[derive(Debug, Clone)]
pub struct FileChange {
    pub path: String,
    /// added | modified | deleted
    pub status: String,
    pub content: Vec<u8>,
}

impl Repo {
    /// Commits `files` onto `branch` (which exists) in one commit.
    pub async fn commit_files(&self, state: &AppState, branch: &str, message: &str, files: &[FileChange]) -> GitResult<()> {
        use base64::Engine;
        let b64 = |c: &[u8]| base64::engine::general_purpose::STANDARD.encode(c);
        match self.kind.as_str() {
            "github" => {
                let head = self.head(state, branch).await?;
                let (s, c) = self.call(state, Method::GET, &format!("{}/git/commits/{head}", self.repo_api()), None).await?;
                let base_tree = c["tree"]["sha"].as_str().ok_or_else(|| self.refused("Reading the branch", s, &c))?.to_string();
                let mut tree = Vec::new();
                for f in files {
                    if f.status == "deleted" {
                        tree.push(json!({"path": f.path, "mode": "100644", "type": "blob", "sha": null}));
                        continue;
                    }
                    let (s, b) = self
                        .call(state, Method::POST, &format!("{}/git/blobs", self.repo_api()), Some(json!({"content": b64(&f.content), "encoding": "base64"})))
                        .await?;
                    let sha = b["sha"].as_str().ok_or_else(|| self.refused("Uploading a file", s, &b))?;
                    tree.push(json!({"path": f.path, "mode": "100644", "type": "blob", "sha": sha}));
                }
                let (s, t) =
                    self.call(state, Method::POST, &format!("{}/git/trees", self.repo_api()), Some(json!({"base_tree": base_tree, "tree": tree}))).await?;
                let tree = t["sha"].as_str().ok_or_else(|| self.refused("Writing the files", s, &t))?.to_string();
                let (s, c) = self
                    .call(state, Method::POST, &format!("{}/git/commits", self.repo_api()), Some(json!({"message": message, "tree": tree, "parents": [head]})))
                    .await?;
                let commit = c["sha"].as_str().ok_or_else(|| self.refused("Committing", s, &c))?.to_string();
                let (s, r) =
                    self.call(state, Method::PATCH, &format!("{}/git/refs/heads/{}", self.repo_api(), enc_ref(branch)), Some(json!({"sha": commit}))).await?;
                if s != 200 {
                    return Err(self.refused("Committing", s, &r));
                }
                Ok(())
            }
            "gitlab" => {
                let actions: Vec<Value> = files
                    .iter()
                    .map(|f| match f.status.as_str() {
                        "deleted" => json!({"action": "delete", "file_path": f.path}),
                        st => json!({"action": if st == "added" { "create" } else { "update" }, "file_path": f.path, "content": b64(&f.content), "encoding": "base64"}),
                    })
                    .collect();
                let (s, v) = self
                    .call(
                        state,
                        Method::POST,
                        &format!("{}/repository/commits", self.repo_api()),
                        Some(json!({"branch": branch, "commit_message": message, "actions": actions})),
                    )
                    .await?;
                if !matches!(s, 200 | 201) {
                    return Err(self.refused("Committing", s, &v));
                }
                Ok(())
            }
            _ => {
                let mut changes = Vec::new();
                for f in files {
                    let operation = match f.status.as_str() {
                        "added" => "create",
                        "deleted" => "delete",
                        _ => "update",
                    };
                    let mut c = json!({"operation": operation, "path": f.path});
                    if f.status != "deleted" {
                        c["content"] = json!(b64(&f.content));
                    }
                    if f.status != "added" {
                        // Gitea changes a file it's told the current version of
                        let (s, cur) =
                            self.call(state, Method::GET, &format!("{}/contents/{}?ref={}", self.repo_api(), enc_ref(&f.path), enc(branch)), None).await?;
                        c["sha"] = json!(cur["sha"].as_str().ok_or_else(|| self.refused("Reading a file", s, &cur))?);
                    }
                    changes.push(c);
                }
                let (s, v) = self
                    .call(state, Method::POST, &format!("{}/contents", self.repo_api()), Some(json!({"branch": branch, "message": message, "files": changes})))
                    .await?;
                if !matches!(s, 200 | 201) {
                    return Err(self.refused("Committing", s, &v));
                }
                Ok(())
            }
        }
    }

    /// The repository at `reference` as a .tar.gz (up to `max` bytes); one redirect is followed
    /// (GitHub hands archives out from another host), without the token.
    pub async fn archive(&self, state: &AppState, reference: &str, max: usize) -> GitResult<Vec<u8>> {
        let url = match self.kind.as_str() {
            "github" => format!("{}/tarball/{}", self.repo_api(), enc_ref(reference)),
            "gitlab" => format!("{}/repository/archive.tar.gz?sha={}", self.repo_api(), enc(reference)),
            _ => format!("{}/archive/{}.tar.gz", self.repo_api(), enc_ref(reference)),
        };
        let mut res = self.raw(state, &url, true).await?;
        if res.status().is_redirection() {
            let next = res
                .headers()
                .get("location")
                .and_then(|l| l.to_str().ok())
                .map(str::to_string)
                .ok_or_else(|| GitError::Other("the Git server redirected nowhere".into()))?;
            res = self.raw(state, &next, false).await?;
        }
        if !res.status().is_success() {
            return Err(GitError::Other(format!("Downloading the repository: HTTP {}", res.status().as_u16())));
        }
        let mut out = Vec::new();
        while let Some(chunk) = res.chunk().await.map_err(|e| GitError::Other(e.without_url().to_string()))? {
            out.extend_from_slice(&chunk);
            if out.len() > max {
                return Err(GitError::Other(format!("the repository is larger than {} MB", max / (1024 * 1024))));
            }
        }
        Ok(out)
    }

    /// A GET whose answer is read as it comes (archives), pinned like every other request.
    async fn raw(&self, state: &AppState, url: &str, with_token: bool) -> GitResult<reqwest::Response> {
        let u = crate::webhooks::validate_url(url).map_err(GitError::Other)?;
        let client = crate::webhooks::pinned_client(state, &u, 120).await.map_err(GitError::Other)?;
        let mut req = client.get(u);
        if with_token {
            req = match self.kind.as_str() {
                "gitlab" => req.header("private-token", &self.token),
                "gitea" => req.header("authorization", format!("token {}", self.token)),
                _ => req.header("authorization", format!("Bearer {}", self.token)).header("accept", "application/vnd.github+json"),
            };
        }
        req.send().await.map_err(|e| GitError::Other(format!("the Git server didn't answer: {}", e.without_url())))
    }
}

/// A branch name Git accepts (the rules of `git check-ref-format`, a little stricter).
pub fn valid_branch(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 200
        && !name.starts_with(['-', '/', '.'])
        && !name.ends_with(['/', '.'])
        && !name.ends_with(".lock")
        && !name.contains("..")
        && !name.contains("//")
        && !name.contains("@{")
        && name.chars().all(|c| c.is_ascii_graphic() && !matches!(c, '~' | '^' | ':' | '?' | '*' | '[' | '\\'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses() {
        let r = Repo::new("github", "https://github.com/acme/web", None, " t ").unwrap();
        assert_eq!((r.repo_api().as_str(), r.token.as_str()), ("https://api.github.com/repos/acme/web", "t"));
        assert_eq!(Repo::new("github", "https://git.acme.test/acme/web", None, "t").unwrap().repo_api(), "https://git.acme.test/api/v3/repos/acme/web");
        let g = Repo::new("gitlab", "https://gitlab.com/acme/platform/web", None, "t").unwrap();
        assert_eq!(g.repo_api(), "https://gitlab.com/api/v4/projects/acme%2Fplatform%2Fweb");
        assert_eq!(g.branch_url("web-1-x"), "https://gitlab.com/acme/platform/web/-/tree/web-1-x");
        assert_eq!(Repo::new("gitea", "http://127.0.0.1:3000/acme/web", None, "t").unwrap().repo_api(), "http://127.0.0.1:3000/api/v1/repos/acme/web");
        assert_eq!(
            Repo::new("gitlab", "https://acme.test/gitlab/acme/web", Some("https://acme.test/gitlab/api/v4/"), "t").unwrap().api,
            "https://acme.test/gitlab/api/v4"
        );
        assert!(Repo::new("github", "https://github.com/acme", None, "t").is_err());
        assert!(Repo::new("svn", "https://github.com/acme/web", None, "t").is_err());
    }

    #[test]
    fn branch_names() {
        for ok in ["web-12-login", "feature/web-12", "v1.2-fix"] {
            assert!(valid_branch(ok), "{ok}");
        }
        for bad in ["", "-x", "a b", "a..b", "x.lock", "a~1", "a:b", "feature/", "/x", "a//b", "ä"] {
            assert!(!valid_branch(bad), "{bad}");
        }
    }
}
