//! Git integration: repositories report pushes and pull / merge requests to a webhook URL per
//! repository (GitHub, GitLab, or a small generic JSON format for other servers). Issue keys in
//! commit messages, branch names and pull request titles / descriptions link the commit, branch
//! or pull request to the issue ("development links", one per URL and issue), write a history
//! entry and, when the project asks for it, move the issue on: "a branch is pushed / a pull
//! request is opened → In progress", "a pull request is merged → Done". Those moves are made by
//! Automation (no person), only forward in the workflow and only along allowed transitions.
//!
//! Only the integration's own project is linked (its key and former keys): a repository
//! connected to WEB can't attach anything to issues of other projects.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use chrono::{DateTime, Utc};
use hmac::{Hmac, KeyInit, Mac};
use serde::Serialize;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::app::{AppState, new_id};
use crate::error::ApiResult;
use crate::events::{self, Event};
use crate::issues::{self, Actor, Change};

pub const PROVIDERS: &[&str] = &["github", "gitlab", "generic"];

/// Commits handled per delivery (GitHub sends at most 20, GitLab 20 of `total_commits_count`).
const MAX_COMMITS: usize = 200;

// ---- issue keys ----

/// Issue keys (`WEB-12`, any case) in a text, as (upper-case key, number), each once, in order.
/// A key starts and ends at a word boundary: letters or digits right before or after it make it
/// part of another word (`XWEB-12`, `WEB-12a`); `-`, `_`, `/`, `.`, spaces and the like separate.
pub fn extract_keys(text: &str) -> Vec<(String, i32)> {
    let b = text.as_bytes();
    let mut out: Vec<(String, i32)> = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let starts_word = i == 0 || !b[i - 1].is_ascii_alphanumeric();
        if !(b[i].is_ascii_alphabetic() && starts_word) {
            i += 1;
            continue;
        }
        let mut j = i;
        while j < b.len() && b[j].is_ascii_alphanumeric() {
            j += 1;
        }
        // KEY-123: a hyphen, digits, then no letter or digit
        if j < b.len() && b[j] == b'-' {
            let mut k = j + 1;
            while k < b.len() && b[k].is_ascii_digit() {
                k += 1;
            }
            let ends_word = k == b.len() || !b[k].is_ascii_alphanumeric();
            if k > j + 1 && ends_word && (2..=10).contains(&(j - i)) && k - j - 1 <= 9 {
                let key = text[i..j].to_ascii_uppercase();
                if let Ok(n) = text[j + 1..k].parse::<i32>()
                    && n > 0
                    && !out.iter().any(|(ok, on)| *ok == key && *on == n)
                {
                    out.push((key, n));
                }
            }
            i = k.max(j + 1);
        } else {
            i = j;
        }
    }
    out
}

/// A branch name for an issue: `web-12-short-title-slug` (lower case, ASCII, a few words).
pub fn branch_name(key: &str, title: &str) -> String {
    let mut slug = String::new();
    for c in title.chars() {
        let folded: &str = match c {
            'ä' | 'Ä' => "ae",
            'ö' | 'Ö' => "oe",
            'ü' | 'Ü' => "ue",
            'ß' => "ss",
            'à' | 'á' | 'â' | 'ã' | 'å' | 'À' | 'Á' | 'Â' | 'Ã' | 'Å' => "a",
            'ç' | 'Ç' => "c",
            'è' | 'é' | 'ê' | 'ë' | 'È' | 'É' | 'Ê' | 'Ë' => "e",
            'ì' | 'í' | 'î' | 'ï' | 'Ì' | 'Í' | 'Î' | 'Ï' => "i",
            'ñ' | 'Ñ' => "n",
            'ò' | 'ó' | 'ô' | 'õ' | 'ø' | 'Ò' | 'Ó' | 'Ô' | 'Õ' | 'Ø' => "o",
            'ù' | 'ú' | 'û' | 'Ù' | 'Ú' | 'Û' => "u",
            'ý' | 'ÿ' | 'Ý' => "y",
            _ => "",
        };
        if !folded.is_empty() {
            slug.push_str(folded);
        } else if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let mut words: Vec<&str> = Vec::new();
    let mut len = 0;
    for w in slug.split('-').filter(|w| !w.is_empty()) {
        if words.len() >= 6 || len + w.len() > 40 {
            break;
        }
        len += w.len() + 1;
        words.push(w);
    }
    let key = key.to_ascii_lowercase();
    if words.is_empty() { key } else { format!("{key}-{}", words.join("-")) }
}

// ---- authenticity ----

/// Checks GitHub's `X-Hub-Signature-256: sha256=<hex HMAC-SHA256 of the body>` (constant time).
pub fn signature_valid(secret: &str, body: &[u8], header: &str) -> bool {
    let Some(hex_sig) = header.trim().strip_prefix("sha256=") else { return false };
    let Ok(sig) = hex::decode(hex_sig) else { return false };
    let Ok(mut mac) = <Hmac<Sha256> as KeyInit>::new_from_slice(secret.as_bytes()) else { return false };
    mac.update(body);
    mac.verify_slice(&sig).is_ok()
}

/// Compares a shared token without revealing where it differs.
pub fn token_valid(secret: &str, given: &str) -> bool {
    let (a, b) = (Sha256::digest(secret.as_bytes()), Sha256::digest(given.trim().as_bytes()));
    a.iter().zip(b.iter()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0 && !secret.is_empty()
}

// ---- what a delivery says, independent of the provider ----

#[derive(Debug, Clone, Default)]
pub struct Commit {
    pub sha: String,
    pub message: String,
    pub url: String,
    pub author: Option<String>,
    pub at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Default)]
pub struct Branch {
    pub name: String,
    pub url: String,
    pub author: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct PullRequest {
    pub number: Option<i64>,
    pub title: String,
    pub body: String,
    pub url: String,
    /// open | merged | closed
    pub state: String,
    pub author: Option<String>,
    pub branch: Option<String>,
    pub at: Option<DateTime<Utc>>,
}

/// One delivery: pushed commits and branch, or a pull / merge request.
#[derive(Debug, Clone, Default)]
pub struct Activity {
    pub commits: Vec<Commit>,
    pub branch: Option<Branch>,
    pub pull_request: Option<PullRequest>,
}

fn s(v: &Value) -> String {
    v.as_str().unwrap_or("").to_string()
}

fn opt(v: &Value) -> Option<String> {
    v.as_str().map(str::trim).filter(|x| !x.is_empty()).map(str::to_string)
}

/// RFC 3339, or GitLab's `2026-11-12 10:00:00 UTC` / `… +0100`.
pub fn timestamp(v: &Value) -> Option<DateTime<Utc>> {
    let t = v.as_str()?.trim();
    DateTime::parse_from_rfc3339(t)
        .ok()
        .or_else(|| DateTime::parse_from_str(t, "%Y-%m-%d %H:%M:%S %z").ok())
        .map(|d| d.with_timezone(&Utc))
        .or_else(|| chrono::NaiveDateTime::parse_from_str(t.trim_end_matches(" UTC"), "%Y-%m-%d %H:%M:%S").ok().map(|n| n.and_utc()))
}

/// Only web addresses are kept (a `javascript:` URL must never become a link).
fn web_url(u: &str) -> Option<String> {
    let u = u.trim();
    ((u.starts_with("https://") || u.starts_with("http://")) && u.len() <= 2000 && !u.contains(char::is_whitespace)).then(|| u.to_string())
}

fn clip(t: &str, n: usize) -> String {
    t.chars().take(n).collect()
}

/// The repository's web address without a trailing slash or `.git`.
pub fn normalize_repo_url(u: &str) -> Option<String> {
    let u = web_url(u)?;
    let u = u.trim_end_matches('/');
    Some(u.strip_suffix(".git").unwrap_or(u).to_string())
}

const ZERO_SHA: &str = "0000000000000000000000000000000000000000";

/// GitHub (and Gitea / Forgejo, which send the same payloads): `push` and `pull_request`.
/// `None` for events that carry nothing to link (`ping`, tags, deleted branches, others).
pub fn parse_github(event: &str, v: &Value, repo_url: &str) -> Result<Option<Activity>, String> {
    let repo = v["repository"]["html_url"].as_str().and_then(normalize_repo_url).unwrap_or_else(|| repo_url.to_string());
    match event {
        "push" => {
            let Some(branch) = v["ref"].as_str().and_then(|r| r.strip_prefix("refs/heads/")) else { return Ok(None) };
            if v["deleted"].as_bool() == Some(true) || v["after"].as_str() == Some(ZERO_SHA) {
                return Ok(None);
            }
            let pusher = opt(&v["pusher"]["name"]).or_else(|| opt(&v["sender"]["login"]));
            let commits = v["commits"]
                .as_array()
                .into_iter()
                .flatten()
                .take(MAX_COMMITS)
                .filter_map(|c| {
                    let sha = opt(&c["id"])?;
                    Some(Commit {
                        url: c["url"].as_str().and_then(web_url).unwrap_or_else(|| format!("{repo}/commit/{sha}")),
                        sha,
                        message: s(&c["message"]),
                        author: opt(&c["author"]["name"]).or_else(|| opt(&c["author"]["username"])),
                        at: timestamp(&c["timestamp"]),
                    })
                })
                .collect();
            Ok(Some(Activity {
                commits,
                branch: Some(Branch { name: branch.to_string(), url: format!("{repo}/tree/{branch}"), author: pusher }),
                pull_request: None,
            }))
        }
        "pull_request" => {
            let pr = &v["pull_request"];
            if !pr.is_object() {
                return Err("pull_request is missing".into());
            }
            let state = if pr["merged"].as_bool() == Some(true) || pr["merged_at"].is_string() {
                "merged"
            } else if pr["state"].as_str() == Some("closed") {
                "closed"
            } else {
                "open"
            };
            let number = pr["number"].as_i64().or_else(|| v["number"].as_i64());
            Ok(Some(Activity {
                pull_request: Some(PullRequest {
                    url: pr["html_url"]
                        .as_str()
                        .and_then(web_url)
                        .or_else(|| number.map(|n| format!("{repo}/pull/{n}")))
                        .ok_or("the pull request has no URL")?,
                    number,
                    title: s(&pr["title"]),
                    body: s(&pr["body"]),
                    state: state.into(),
                    author: opt(&pr["user"]["login"]),
                    branch: opt(&pr["head"]["ref"]),
                    at: timestamp(&pr["updated_at"]),
                }),
                ..Default::default()
            }))
        }
        _ => Ok(None),
    }
}

/// GitLab: `Push Hook` and `Merge Request Hook` (or the body's `object_kind`).
pub fn parse_gitlab(event: &str, v: &Value, repo_url: &str) -> Result<Option<Activity>, String> {
    let repo = v["project"]["web_url"].as_str().and_then(normalize_repo_url).unwrap_or_else(|| repo_url.to_string());
    let kind = match (event, v["object_kind"].as_str()) {
        ("Push Hook", _) | (_, Some("push")) => "push",
        ("Merge Request Hook", _) | (_, Some("merge_request")) => "merge_request",
        _ => return Ok(None),
    };
    if kind == "push" {
        let Some(branch) = v["ref"].as_str().and_then(|r| r.strip_prefix("refs/heads/")) else { return Ok(None) };
        if v["after"].as_str() == Some(ZERO_SHA) {
            return Ok(None);
        }
        let commits = v["commits"]
            .as_array()
            .into_iter()
            .flatten()
            .take(MAX_COMMITS)
            .filter_map(|c| {
                let sha = opt(&c["id"])?;
                Some(Commit {
                    url: c["url"].as_str().and_then(web_url).unwrap_or_else(|| format!("{repo}/-/commit/{sha}")),
                    sha,
                    message: s(&c["message"]),
                    author: opt(&c["author"]["name"]),
                    at: timestamp(&c["timestamp"]),
                })
            })
            .collect();
        let author = opt(&v["user_name"]).or_else(|| opt(&v["user_username"]));
        return Ok(Some(Activity {
            commits,
            branch: Some(Branch { name: branch.to_string(), url: format!("{repo}/-/tree/{branch}"), author }),
            pull_request: None,
        }));
    }
    let mr = &v["object_attributes"];
    if !mr.is_object() {
        return Err("object_attributes is missing".into());
    }
    let state = match mr["state"].as_str().unwrap_or("opened") {
        "merged" => "merged",
        "closed" => "closed",
        _ => "open",
    };
    let number = mr["iid"].as_i64();
    Ok(Some(Activity {
        pull_request: Some(PullRequest {
            url: mr["url"]
                .as_str()
                .and_then(web_url)
                .or_else(|| number.map(|n| format!("{repo}/-/merge_requests/{n}")))
                .ok_or("the merge request has no URL")?,
            number,
            title: s(&mr["title"]),
            body: s(&mr["description"]),
            state: state.into(),
            author: opt(&v["user"]["name"]).or_else(|| opt(&v["user"]["username"])),
            branch: opt(&mr["source_branch"]),
            at: timestamp(&mr["updated_at"]),
        }),
        ..Default::default()
    }))
}

/// The generic format (see the README): `commits`, `branch` and `pullRequest`, all optional.
pub fn parse_generic(v: &Value, repo_url: &str) -> Result<Activity, String> {
    if !v.is_object() {
        return Err("expected a JSON object with commits, branch or pullRequest".into());
    }
    let mut a = Activity::default();
    for c in v["commits"].as_array().into_iter().flatten().take(MAX_COMMITS) {
        let Some(sha) = opt(&c["sha"]).or_else(|| opt(&c["id"])) else { continue };
        let url = match c["url"].as_str() {
            Some(u) => web_url(u).ok_or("commit URLs must be web addresses (https://…)")?,
            None => format!("{repo_url}/commit/{sha}"),
        };
        a.commits.push(Commit { sha, message: s(&c["message"]), url, author: opt(&c["author"]), at: timestamp(&c["timestamp"]) });
    }
    let b = &v["branch"];
    if let Some(name) = opt(&b["name"]).or_else(|| opt(b)) {
        let url = match b["url"].as_str() {
            Some(u) => web_url(u).ok_or("branch URLs must be web addresses (https://…)")?,
            None => format!("{repo_url}/tree/{name}"),
        };
        a.branch = Some(Branch { name, url, author: opt(&b["author"]) });
    }
    let pr = if v["pullRequest"].is_object() { &v["pullRequest"] } else { &v["pull_request"] };
    if pr.is_object() {
        let url = pr["url"].as_str().and_then(web_url).ok_or("pullRequest.url must be a web address (https://…)")?;
        let state = match pr["state"].as_str().unwrap_or("open").to_ascii_lowercase().as_str() {
            "merged" => "merged",
            "closed" | "declined" | "rejected" => "closed",
            _ => "open",
        };
        a.pull_request = Some(PullRequest {
            number: pr["number"].as_i64(),
            title: s(&pr["title"]),
            body: s(&pr["body"]),
            url,
            state: state.into(),
            author: opt(&pr["author"]),
            branch: opt(&pr["branch"]),
            at: timestamp(&pr["updatedAt"]).or_else(|| timestamp(&pr["timestamp"])),
        });
    }
    Ok(a)
}

// ---- the state of a pull request: draft, checks, reviews ----

/// What a delivery says about a pull request beyond open / merged / closed.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PrStatus {
    pub number: Option<i64>,
    /// its branch, when the delivery doesn't name the pull request (commit statuses)
    pub branch: Option<String>,
    pub draft: Option<bool>,
    /// pending | success | failure
    pub checks: Option<String>,
    /// the people asked to review, as they are now
    pub requested: Option<Vec<String>>,
    /// someone's review: (name, approved | changes_requested | dismissed)
    pub review: Option<(String, String)>,
}

fn checks_word(s: &str) -> Option<&'static str> {
    match s.to_ascii_lowercase().as_str() {
        "success" | "passed" | "neutral" | "skipped" => Some("success"),
        "failure" | "failed" | "error" | "timed_out" | "cancelled" | "canceled" | "action_required" | "startup_failure" => Some("failure"),
        "pending" | "running" | "queued" | "in_progress" | "requested" | "created" | "waiting_for_resource" | "preparing" | "scheduled" => Some("pending"),
        _ => None,
    }
}

/// GitHub, Gitea and Forgejo: `pull_request` (draft, requested reviewers), reviews, check suites
/// and runs, commit statuses.
pub fn status_github(event: &str, v: &Value) -> Option<PrStatus> {
    let pr = &v["pull_request"];
    let number = pr["number"].as_i64().or_else(|| v["number"].as_i64());
    let names = |list: &Value| list.as_array().map(|a| a.iter().filter_map(|u| opt(&u["login"]).or_else(|| opt(&u["username"]))).collect::<Vec<_>>());
    match event {
        "pull_request" => Some(PrStatus {
            number,
            // Gitea marks drafts by a title prefix
            draft: pr["draft"].as_bool().or_else(|| pr["title"].as_str().map(|t| t.starts_with("WIP:") || t.starts_with("[WIP]") || t.starts_with("Draft:"))),
            requested: names(&pr["requested_reviewers"]),
            ..Default::default()
        }),
        "pull_request_review" | "pull_request_approved" | "pull_request_rejected" | "pull_request_review_approved" | "pull_request_review_rejected" => {
            let who = opt(&v["review"]["user"]["login"]).or_else(|| opt(&v["sender"]["login"]))?;
            let state = v["review"]["state"].as_str().or_else(|| v["review"]["type"].as_str()).unwrap_or(event).to_ascii_lowercase();
            let state = match state.as_str() {
                s if s.contains("approved") => "approved",
                s if s.contains("changes_requested") || s.contains("rejected") => "changes_requested",
                "dismissed" => "dismissed",
                _ => return None,
            };
            Some(PrStatus { number, review: Some((who, state.into())), ..Default::default() })
        }
        "check_suite" | "check_run" => {
            let c = &v[event];
            let word = c["conclusion"].as_str().and_then(checks_word).or_else(|| c["status"].as_str().and_then(checks_word))?;
            let prs = if event == "check_suite" { &c["pull_requests"] } else { &c["check_suite"]["pull_requests"] };
            let branch = opt(&c["head_branch"]).or_else(|| opt(&c["check_suite"]["head_branch"]));
            Some(PrStatus { number: prs[0]["number"].as_i64(), branch, checks: Some(word.into()), ..Default::default() })
        }
        "status" => Some(PrStatus { branch: opt(&v["branches"][0]["name"]), checks: Some(checks_word(v["state"].as_str()?)?.into()), ..Default::default() })
            .filter(|s| s.branch.is_some()),
        _ => None,
    }
}

/// GitLab: merge requests (draft, reviewers, approvals) and pipelines.
pub fn status_gitlab(event: &str, v: &Value) -> Option<PrStatus> {
    let a = &v["object_attributes"];
    match (event, v["object_kind"].as_str()) {
        ("Merge Request Hook", _) | (_, Some("merge_request")) => {
            let who = opt(&v["user"]["name"]).or_else(|| opt(&v["user"]["username"]));
            let review = match (a["action"].as_str(), who) {
                (Some("approved" | "approval"), Some(w)) => Some((w, "approved".to_string())),
                (Some("unapproved" | "unapproval"), Some(w)) => Some((w, "dismissed".to_string())),
                _ => None,
            };
            Some(PrStatus {
                number: a["iid"].as_i64(),
                draft: a["draft"].as_bool().or_else(|| a["work_in_progress"].as_bool()),
                requested: v["reviewers"].as_array().map(|r| r.iter().filter_map(|u| opt(&u["name"]).or_else(|| opt(&u["username"]))).collect()),
                review,
                ..Default::default()
            })
        }
        ("Pipeline Hook", _) | (_, Some("pipeline")) => Some(PrStatus {
            number: v["merge_request"]["iid"].as_i64(),
            branch: opt(&a["ref"]),
            checks: Some(checks_word(a["status"].as_str()?)?.into()),
            ..Default::default()
        }),
        _ => None,
    }
}

/// Brings a delivery's news to the pull requests it's about (of this repository); the issues
/// showing them refresh. Returns how many links changed.
pub async fn apply_status(state: &AppState, hook: &Hook, s: &PrStatus) -> ApiResult<usize> {
    let rows: Vec<(Uuid, Uuid, Option<Value>)> = sqlx::query_as(
        "SELECT id, issue_id, reviewers FROM dev_links WHERE integration_id = $1 AND kind = 'pull_request'
            AND (number = $2 OR ($2 IS NULL AND branch = $3 AND state = 'open'))",
    )
    .bind(hook.id)
    .bind(s.number.and_then(|n| i32::try_from(n).ok()))
    .bind(&s.branch)
    .fetch_all(&state.db)
    .await?;
    for (id, _, reviewers) in &rows {
        let mut list: Vec<(String, String)> = reviewers
            .as_ref()
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|r| Some((r["name"].as_str()?.to_string(), r["state"].as_str()?.to_string())))
            .collect();
        if let Some(asked) = &s.requested {
            // whoever decided stays; the others are those asked now
            list.retain(|(n, st)| st != "requested" || asked.contains(n));
            for n in asked {
                match list.iter_mut().find(|(x, _)| x == n) {
                    // asked again after a review: it's open again
                    Some(r) => r.1 = "requested".into(),
                    None => list.push((clip(n, 100), "requested".into())),
                }
            }
        }
        if let Some((who, st)) = &s.review {
            list.retain(|(n, _)| n != who);
            if st != "dismissed" {
                list.push((clip(who, 100), st.clone()));
            }
        }
        list.truncate(30);
        let review = if list.iter().any(|r| r.1 == "changes_requested") {
            Some("changes_requested")
        } else if list.iter().any(|r| r.1 == "approved") {
            Some("approved")
        } else {
            None
        };
        sqlx::query(
            "UPDATE dev_links SET draft = COALESCE($2, draft), checks = COALESCE($3, checks), review = $4, reviewers = $5, updated_at = now() WHERE id = $1",
        )
        .bind(id)
        .bind(s.draft)
        .bind(&s.checks)
        .bind(review)
        .bind(json!(list.iter().map(|(n, st)| json!({"name": n, "state": st})).collect::<Vec<_>>()))
        .execute(&state.db)
        .await?;
    }
    let issues: BTreeSet<Uuid> = rows.iter().map(|r| r.1).collect();
    for issue in issues {
        let key: Option<String> = sqlx::query_scalar("SELECT p.key || '-' || i.number FROM issues i JOIN projects p ON p.id = i.project_id WHERE i.id = $1")
            .bind(issue)
            .fetch_optional(&state.db)
            .await?;
        if let Some(key) = key {
            events::publish(state, Event::new(hook.workspace_id, "issue").project(hook.project_id).issue(issue, &key)).await;
        }
    }
    Ok(rows.len())
}

// ---- integrations and links ----

/// A connected repository (the webhook's target).
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Hook {
    pub id: Uuid,
    pub project_id: Uuid,
    pub workspace_id: Uuid,
    pub provider: String,
    pub repo_url: String,
    pub secret: String,
}

/// A commit, branch or pull request linked to an issue.
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct DevLink {
    pub id: Uuid,
    /// commit | branch | pull_request
    pub kind: String,
    pub title: String,
    pub url: String,
    pub author: Option<String>,
    /// pull requests: open | merged | closed
    pub state: Option<String>,
    pub sha: Option<String>,
    pub branch: Option<String>,
    pub number: Option<i32>,
    pub repo: Option<String>,
    pub at: DateTime<Utc>,
    /// pull requests: not ready for review yet
    pub draft: Option<bool>,
    /// pull requests: pending | success | failure (checks / CI)
    pub checks: Option<String>,
    /// pull requests: approved | changes_requested
    pub review: Option<String>,
    /// pull requests: `[{name, state: requested | approved | changes_requested}]`
    pub reviewers: Option<Value>,
}

/// An issue's development links: pull requests first, then branches, then commits (newest first).
pub async fn links(state: &AppState, issue: Uuid) -> ApiResult<Vec<DevLink>> {
    Ok(sqlx::query_as(
        "SELECT id, kind, title, url, author, state, sha, branch, number, repo, at, draft, checks, review, reviewers FROM dev_links WHERE issue_id = $1
          ORDER BY CASE kind WHEN 'pull_request' THEN 0 WHEN 'branch' THEN 1 ELSE 2 END, at DESC LIMIT 300",
    )
    .bind(issue)
    .fetch_all(&state.db)
    .await?)
}

/// `org/repo` from a repository URL (for display).
pub fn repo_name(url: &str) -> String {
    let path = url.split("://").nth(1).and_then(|r| r.split_once('/')).map(|(_, p)| p).unwrap_or(url);
    path.trim_matches('/').to_string()
}

struct Item {
    kind: &'static str,
    title: String,
    url: String,
    author: Option<String>,
    state: Option<String>,
    sha: Option<String>,
    branch: Option<String>,
    number: Option<i32>,
    at: DateTime<Utc>,
    numbers: Vec<i32>,
}

/// The project's keys: the current one and the ones it had before (upper case).
async fn project_keys(state: &AppState, project: Uuid) -> ApiResult<HashSet<String>> {
    let keys: Vec<String> =
        sqlx::query_scalar("SELECT upper(key) FROM projects WHERE id = $1 UNION SELECT upper(key) FROM project_key_aliases WHERE project_id = $1")
            .bind(project)
            .fetch_all(&state.db)
            .await?;
    Ok(keys.into_iter().collect())
}

fn numbers_in(text: &str, keys: &HashSet<String>) -> Vec<i32> {
    extract_keys(text).into_iter().filter(|(k, _)| keys.contains(k)).map(|(_, n)| n).collect()
}

/// What a delivery did.
#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Outcome {
    /// issues that got a new link or a changed pull request
    pub issues: Vec<String>,
    pub new_links: usize,
    /// issues the project's rules moved
    pub moved: Vec<String>,
}

/// Links what a delivery mentions to the project's issues, records history, applies the rules.
pub async fn process(state: &AppState, hook: &Hook, a: &Activity) -> ApiResult<Outcome> {
    let keys = project_keys(state, hook.project_id).await?;
    let repo = repo_name(&hook.repo_url);
    let now = Utc::now();
    let mut items: Vec<Item> = Vec::new();
    for c in &a.commits {
        let numbers = numbers_in(&c.message, &keys);
        if numbers.is_empty() {
            continue;
        }
        let first_line = c.message.lines().next().unwrap_or("").trim();
        items.push(Item {
            kind: "commit",
            title: clip(if first_line.is_empty() { &c.sha } else { first_line }, 300),
            url: c.url.clone(),
            author: c.author.as_deref().map(|x| clip(x, 200)),
            state: None,
            sha: Some(clip(&c.sha, 64)),
            branch: a.branch.as_ref().map(|b| clip(&b.name, 250)),
            number: None,
            at: c.at.unwrap_or(now),
            numbers,
        });
    }
    if let Some(b) = &a.branch {
        let numbers = numbers_in(&b.name, &keys);
        if !numbers.is_empty() {
            items.push(Item {
                kind: "branch",
                title: clip(&b.name, 250),
                url: b.url.clone(),
                author: b.author.as_deref().map(|x| clip(x, 200)),
                state: None,
                sha: None,
                branch: Some(clip(&b.name, 250)),
                number: None,
                at: a.commits.iter().filter_map(|c| c.at).max().unwrap_or(now),
                numbers,
            });
        }
    }
    if let Some(pr) = &a.pull_request {
        let text = format!("{}\n{}\n{}", pr.title, pr.body, pr.branch.as_deref().unwrap_or(""));
        let numbers = numbers_in(&text, &keys);
        if !numbers.is_empty() {
            items.push(Item {
                kind: "pull_request",
                title: clip(if pr.title.trim().is_empty() { "Pull request" } else { pr.title.trim() }, 300),
                url: pr.url.clone(),
                author: pr.author.as_deref().map(|x| clip(x, 200)),
                state: Some(pr.state.clone()),
                sha: None,
                branch: pr.branch.as_deref().map(|x| clip(x, 250)),
                number: pr.number.and_then(|n| i32::try_from(n).ok()),
                at: pr.at.unwrap_or(now),
                numbers,
            });
        }
    }
    let mut out = Outcome::default();
    if items.is_empty() {
        return Ok(out);
    }
    let wanted: Vec<i32> = items.iter().flat_map(|i| i.numbers.iter().copied()).collect::<BTreeSet<_>>().into_iter().collect();
    let found: Vec<(Uuid, i32, String)> = sqlx::query_as(
        "SELECT i.id, i.number, p.key || '-' || i.number FROM issues i JOIN projects p ON p.id = i.project_id
          WHERE i.project_id = $1 AND i.number = ANY($2) AND i.deleted_at IS NULL",
    )
    .bind(hook.project_id)
    .bind(&wanted)
    .fetch_all(&state.db)
    .await?;
    let by_number: HashMap<i32, (Uuid, String)> = found.into_iter().map(|(id, n, k)| (n, (id, k))).collect();

    // touched issues (by key, for a stable order) and what happened to each
    let mut touched: BTreeMap<String, Uuid> = BTreeMap::new();
    let mut opened: BTreeSet<Uuid> = BTreeSet::new();
    let mut merged: BTreeSet<Uuid> = BTreeSet::new();
    let mut tx = state.db.begin().await?;
    // new commits per issue become one history entry ("linked 3 commits")
    let mut new_commits: BTreeMap<Uuid, Vec<&Item>> = BTreeMap::new();
    for item in &items {
        for n in &item.numbers {
            let Some((issue, key)) = by_number.get(n) else { continue };
            let old: Option<(Option<String>,)> = sqlx::query_as("SELECT state FROM dev_links WHERE issue_id = $1 AND url = $2 FOR UPDATE")
                .bind(issue)
                .bind(&item.url)
                .fetch_optional(&mut *tx)
                .await?;
            match old {
                None => {
                    sqlx::query(
                        "INSERT INTO dev_links (id, issue_id, integration_id, kind, title, url, author, state, sha, branch, number, repo, at)
                         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13) ON CONFLICT (issue_id, url) DO NOTHING",
                    )
                    .bind(new_id())
                    .bind(issue)
                    .bind(hook.id)
                    .bind(item.kind)
                    .bind(&item.title)
                    .bind(&item.url)
                    .bind(&item.author)
                    .bind(&item.state)
                    .bind(&item.sha)
                    .bind(&item.branch)
                    .bind(item.number)
                    .bind(&repo)
                    .bind(item.at)
                    .execute(&mut *tx)
                    .await?;
                    out.new_links += 1;
                    touched.insert(key.clone(), *issue);
                    match item.kind {
                        "commit" => new_commits.entry(*issue).or_default().push(item),
                        kind => {
                            let field = if kind == "branch" { "branch" } else { "pull_request" };
                            let c = Change { field, old: Value::Null, new: link_json(item) };
                            issues::record(&mut tx, *issue, hook.project_id, None, "dev", Some(&c)).await?;
                            match item.state.as_deref() {
                                None | Some("open") => {
                                    opened.insert(*issue);
                                }
                                Some("merged") => {
                                    merged.insert(*issue);
                                }
                                _ => {}
                            }
                        }
                    }
                }
                Some((old_state,)) => {
                    sqlx::query("UPDATE dev_links SET title = $3, state = COALESCE($4, state), author = COALESCE(author, $5), updated_at = now() WHERE issue_id = $1 AND url = $2")
                        .bind(issue)
                        .bind(&item.url)
                        .bind(&item.title)
                        .bind(&item.state)
                        .bind(&item.author)
                        .execute(&mut *tx)
                        .await?;
                    if item.kind == "pull_request" && item.state.is_some() && item.state != old_state {
                        let c = Change { field: "pull_request", old: json!({"state": old_state}), new: link_json(item) };
                        issues::record(&mut tx, *issue, hook.project_id, None, "dev", Some(&c)).await?;
                        touched.insert(key.clone(), *issue);
                        match item.state.as_deref() {
                            Some("merged") => {
                                merged.insert(*issue);
                            }
                            Some("open") => {
                                opened.insert(*issue);
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
    }
    for (issue, commits) in &new_commits {
        let first = commits[0];
        let mut new = link_json(first);
        new["count"] = json!(commits.len());
        let c = Change { field: "commit", old: Value::Null, new };
        issues::record(&mut tx, *issue, hook.project_id, None, "dev", Some(&c)).await?;
    }
    tx.commit().await?;

    for (key, id) in &touched {
        events::publish(state, Event::new(hook.workspace_id, "issue").project(hook.project_id).issue(*id, key)).await;
    }
    out.issues = touched.keys().cloned().collect();

    // the project's rules
    let rules: Option<(Option<Uuid>, Option<Uuid>)> = sqlx::query_as("SELECT merged_status_id, opened_status_id FROM git_rules WHERE project_id = $1")
        .bind(hook.project_id)
        .fetch_optional(&state.db)
        .await?;
    if let Some((on_merge, on_open)) = rules {
        let bot = Actor { user: None, workspace: hook.workspace_id, depth: 0 };
        let mut moves: Vec<(Uuid, Uuid)> = Vec::new();
        if let Some(target) = on_merge {
            moves.extend(merged.iter().map(|i| (*i, target)));
        }
        if let Some(target) = on_open {
            moves.extend(opened.iter().filter(|i| !merged.contains(i)).map(|i| (*i, target)));
        }
        for (issue, target) in moves {
            match move_forward(state, &bot, hook.project_id, issue, target).await {
                Ok(Some(key)) => out.moved.push(key),
                Ok(None) => {}
                Err(e) => tracing::warn!(issue = %issue, error = %e, "git rule: can't move the issue"),
            }
        }
    }
    Ok(out)
}

/// Links a branch or pull request someone made from the issue itself (it's theirs in the
/// history); a draft when it's a pull request.
#[allow(clippy::too_many_arguments)]
pub async fn link_created(
    state: &AppState,
    hook: &Hook,
    issue: Uuid,
    user: Uuid,
    kind: &'static str,
    title: &str,
    url: &str,
    branch: &str,
    number: Option<i64>,
    author: Option<&str>,
) -> ApiResult<()> {
    let pr = kind == "pull_request";
    let item = Item {
        kind,
        title: clip(title, 300),
        url: url.to_string(),
        author: author.map(|a| clip(a, 200)),
        state: pr.then(|| "open".to_string()),
        sha: None,
        branch: Some(clip(branch, 250)),
        number: number.and_then(|n| i32::try_from(n).ok()),
        at: Utc::now(),
        numbers: vec![],
    };
    let mut tx = state.db.begin().await?;
    let added = sqlx::query(
        "INSERT INTO dev_links (id, issue_id, integration_id, kind, title, url, author, state, branch, number, repo, at, draft, created_by)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14) ON CONFLICT (issue_id, url) DO NOTHING",
    )
    .bind(new_id())
    .bind(issue)
    .bind(hook.id)
    .bind(item.kind)
    .bind(&item.title)
    .bind(&item.url)
    .bind(&item.author)
    .bind(&item.state)
    .bind(&item.branch)
    .bind(item.number)
    .bind(repo_name(&hook.repo_url))
    .bind(item.at)
    .bind(pr.then_some(true))
    .bind(user)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if added > 0 {
        let c = Change { field: if pr { "pull_request" } else { "branch" }, old: Value::Null, new: link_json(&item) };
        issues::record(&mut tx, issue, hook.project_id, Some(user), "dev", Some(&c)).await?;
    }
    tx.commit().await?;
    Ok(())
}

/// After a branch was made from an issue: the project's "work started" rule, and screens refresh.
pub async fn after_created(state: &AppState, hook: &Hook, issue: Uuid, key: &str) {
    let on_open: Option<Uuid> = sqlx::query_scalar("SELECT opened_status_id FROM git_rules WHERE project_id = $1")
        .bind(hook.project_id)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten()
        .flatten();
    if let Some(target) = on_open {
        let bot = Actor { user: None, workspace: hook.workspace_id, depth: 0 };
        if let Err(e) = move_forward(state, &bot, hook.project_id, issue, target).await {
            tracing::warn!(issue = %issue, error = %e, "git rule: can't move the issue");
        }
    }
    events::publish(state, Event::new(hook.workspace_id, "issue").project(hook.project_id).issue(issue, key)).await;
}

fn link_json(item: &Item) -> Value {
    let mut m = Map::new();
    m.insert("kind".into(), json!(item.kind));
    m.insert("title".into(), json!(item.title));
    m.insert("url".into(), json!(item.url));
    for (k, v) in [("author", &item.author), ("state", &item.state), ("sha", &item.sha), ("branch", &item.branch)] {
        if let Some(v) = v {
            m.insert(k.into(), json!(v));
        }
    }
    if let Some(n) = item.number {
        m.insert("number".into(), json!(n));
    }
    Value::Object(m)
}

fn category_rank(c: &str) -> i32 {
    match c {
        "todo" => 0,
        "in_progress" => 1,
        _ => 2,
    }
}

/// Moves an issue to `target` as Automation, but only forward in the workflow (a later category,
/// or a later status of the same category), and in restricted workflows only along an allowed
/// transition. Returns the key when it moved.
pub async fn move_forward(state: &AppState, bot: &Actor, project: Uuid, issue: Uuid, target: Uuid) -> ApiResult<Option<String>> {
    let cur: Option<(Uuid, String, f64, bool)> = sqlx::query_as(
        "SELECT i.status_id, s.category, s.position, COALESCE(p.settings->>'workflow' = 'restricted', false)
           FROM issues i JOIN statuses s ON s.id = i.status_id JOIN projects p ON p.id = i.project_id
          WHERE i.id = $1 AND i.project_id = $2 AND i.deleted_at IS NULL AND p.archived_at IS NULL",
    )
    .bind(issue)
    .bind(project)
    .fetch_optional(&state.db)
    .await?;
    let Some((from, from_cat, from_pos, restricted)) = cur else { return Ok(None) };
    let to: Option<(String, f64)> = sqlx::query_as("SELECT category, position FROM statuses WHERE id = $1 AND project_id = $2")
        .bind(target)
        .bind(project)
        .fetch_optional(&state.db)
        .await?;
    let Some((to_cat, to_pos)) = to else { return Ok(None) };
    if from == target || (category_rank(&from_cat), from_pos) > (category_rank(&to_cat), to_pos) {
        return Ok(None);
    }
    if restricted {
        let allowed: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM transitions WHERE project_id = $1 AND from_status_id = $2 AND to_status_id = $3)")
            .bind(project)
            .bind(from)
            .bind(target)
            .fetch_one(&state.db)
            .await?;
        if !allowed {
            return Ok(None);
        }
    }
    let mut patch = Map::new();
    patch.insert("statusId".into(), json!(target));
    let (s, changes) = issues::update(state, bot, issue, &patch).await?;
    Ok((!changes.is_empty()).then_some(s.key))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(t: &str) -> Vec<String> {
        extract_keys(t).into_iter().map(|(k, n)| format!("{k}-{n}")).collect()
    }

    #[test]
    fn finds_issue_keys() {
        assert_eq!(keys("WEB-12: fix the login"), ["WEB-12"]);
        assert_eq!(keys("fix login (web-12, Web-13) and WEB-12 again"), ["WEB-12", "WEB-13"]);
        assert_eq!(keys("feature/web-12-login-timeout"), ["WEB-12"]);
        assert_eq!(keys("feature_WEB-7_x"), ["WEB-7"]);
        assert_eq!(keys("Merge branch 'app2-3-x' into main"), ["APP2-3"]);
        assert_eq!(keys("see https://tasks.example.com/issues/OPS-99."), ["OPS-99"]);
        // not at a word boundary, or not a key at all
        assert!(keys("XWEB12-3a, WEB-12a, 3WEB-1").is_empty(), "{:?}", keys("XWEB12-3a, WEB-12a, 3WEB-1"));
        assert!(keys("WEB-0 W-1 ABCDEFGHIJK-2 WEB- -12").is_empty());
        assert_eq!(keys("A1-1 utf-8"), ["A1-1", "UTF-8"], "candidates; the project's keys filter them");
        assert_eq!(keys("WEB-12-WEB-13"), ["WEB-12", "WEB-13"]);
        let only: HashSet<String> = ["WEB".to_string(), "OLD".to_string()].into();
        assert_eq!(numbers_in("utf-8 and old-4 and web-5 and api-6", &only), [4, 5]);
    }

    #[test]
    fn branch_names() {
        assert_eq!(branch_name("WEB-12", "Login times out after 5 minutes!"), "web-12-login-times-out-after-5-minutes");
        assert_eq!(branch_name("WEB-3", "Größe der Übersicht ändern"), "web-3-groesse-der-uebersicht-aendern");
        assert_eq!(branch_name("WEB-4", "   "), "web-4");
        assert_eq!(branch_name("WEB-5", "a very long title with many many words that goes on and on"), "web-5-a-very-long-title-with-many");
    }

    #[test]
    fn signatures_and_tokens() {
        let body = br#"{"zen":"Keep it logically awesome."}"#;
        let sig = crate::webhooks::sign("s3cret", body);
        assert!(signature_valid("s3cret", body, &sig));
        assert!(!signature_valid("other", body, &sig));
        assert!(!signature_valid("s3cret", b"tampered", &sig));
        assert!(!signature_valid("s3cret", body, "sha1=abc"));
        assert!(!signature_valid("s3cret", body, "sha256=zz"));
        // GitHub's documented example
        assert!(signature_valid("It's a Secret to Everybody", b"Hello, World!", "sha256=757107ea0eb2509fc211221cce984b8a37570b6d7586c22c46f4379c8b043e17"));
        assert!(token_valid("abc", "abc"));
        assert!(token_valid("abc", " abc "));
        assert!(!token_valid("abc", "abd"));
        assert!(!token_valid("", ""));
    }

    #[test]
    fn parses_payloads() {
        let push = json!({
            "ref": "refs/heads/web-12-login", "after": "abc", "repository": {"html_url": "https://github.com/acme/shop"},
            "pusher": {"name": "max"},
            "commits": [{"id": "abc123", "message": "WEB-12 fix\n\nmore", "url": "https://github.com/acme/shop/commit/abc123", "author": {"name": "Max"}, "timestamp": "2026-11-12T10:00:00+01:00"}]
        });
        let a = parse_github("push", &push, "https://github.com/acme/shop").unwrap().unwrap();
        assert_eq!(a.branch.as_ref().unwrap().url, "https://github.com/acme/shop/tree/web-12-login");
        assert_eq!(a.commits[0].at.unwrap().to_rfc3339(), "2026-11-12T09:00:00+00:00");
        assert!(parse_github("ping", &json!({}), "").unwrap().is_none());
        assert!(parse_github("push", &json!({"ref": "refs/tags/v1"}), "").unwrap().is_none());
        let pr = json!({"action": "closed", "pull_request": {"html_url": "https://github.com/acme/shop/pull/5", "number": 5, "title": "Login", "body": null, "state": "closed", "merged": true, "user": {"login": "max"}, "head": {"ref": "web-12-login"}}});
        let a = parse_github("pull_request", &pr, "").unwrap().unwrap();
        assert_eq!(a.pull_request.as_ref().unwrap().state, "merged");
        let mr = json!({"object_kind": "merge_request", "user": {"name": "Max"}, "object_attributes": {"iid": 7, "title": "WEB-1", "state": "opened", "url": "https://gitlab.com/a/b/-/merge_requests/7", "updated_at": "2026-11-12 10:00:00 UTC"}});
        let a = parse_gitlab("Merge Request Hook", &mr, "").unwrap().unwrap();
        let p = a.pull_request.unwrap();
        assert_eq!((p.state.as_str(), p.number), ("open", Some(7)));
        assert!(p.at.is_some());
        let g =
            parse_generic(&json!({"commits": [{"sha": "1", "message": "WEB-1"}], "pullRequest": {"url": "javascript:alert(1)"}}), "https://git.example.com/x");
        assert!(g.is_err(), "only web addresses become links");
        let g = parse_generic(&json!({"commits": [{"sha": "1", "message": "WEB-1"}], "branch": "web-1-x"}), "https://git.example.com/x").unwrap();
        assert_eq!(g.commits[0].url, "https://git.example.com/x/commit/1");
        assert_eq!(g.branch.unwrap().url, "https://git.example.com/x/tree/web-1-x");
        assert_eq!(normalize_repo_url("https://github.com/acme/shop.git/").as_deref(), Some("https://github.com/acme/shop"));
        assert_eq!(repo_name("https://github.com/acme/shop"), "acme/shop");
    }
}
