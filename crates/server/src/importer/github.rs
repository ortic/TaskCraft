//! GitHub (github.com and GitHub Enterprise Server) through its REST API: a repository's issues
//! (not pull requests) with their numbers, Markdown texts, state and the reason it was closed,
//! labels, assignees, milestones as releases, comments, sub-issues as parents, issue types, the
//! timeline as history (closing and reopening, assignees, labels, milestones, titles), and the
//! files people attached in texts.

use std::collections::HashMap;
use std::sync::LazyLock;

use anyhow::{Context, bail};
use regex::Regex;
use reqwest::Url;
use serde_json::Value;

use super::http::{Http, base_url, day, id, status_of, text, time};
use super::model::{self, Attachment, Change, Field, Format, Item, Model, Text};
use super::{Available, ProgressHandle, Warning, key_from, set_progress};

/// Where a GitHub is: its API and its web address.
#[derive(Debug, Clone)]
pub struct Site {
    pub api: Url,
    pub web: Url,
}

/// github.com for an empty address, else GitHub Enterprise Server (`https://github.example.com`,
/// with or without `/api/v3`).
pub fn site(base: &str) -> anyhow::Result<Site> {
    let b = base.trim();
    if b.is_empty() || matches!(b.trim_end_matches('/').trim_start_matches("https://").trim_start_matches("http://"), "github.com" | "api.github.com") {
        return Ok(Site { api: Url::parse("https://api.github.com/")?, web: Url::parse("https://github.com/")? });
    }
    let u = base_url(b)?;
    if u.path().trim_end_matches('/').ends_with("/api/v3") {
        let mut web = u.clone();
        let path = u.path().trim_end_matches('/').trim_end_matches("/api/v3").to_string() + "/";
        web.set_path(&path);
        Ok(Site { api: u, web })
    } else {
        Ok(Site { api: u.join("api/v3/")?, web: u })
    }
}

/// A client for a GitHub, with a token when there is one (fine-grained or classic, read-only is
/// enough).
pub fn client(site: &Site, token: Option<&str>, allow_private: bool, progress: ProgressHandle) -> anyhow::Result<Http> {
    let host = site.web.host_str().unwrap_or_default().to_string();
    let files =
        if host == "github.com" { vec!["github.com".to_string(), ".githubusercontent.com".to_string()] } else { vec![host.clone(), format!(".{host}")] };
    Ok(Http::new(site.api.clone(), token.map(|t| ("authorization", format!("Bearer {}", t.trim()))), allow_private, progress)?
        .with_file_hosts(files)
        .with_header("x-github-api-version", "2022-11-28"))
}

/// Repositories to choose from: an owner's (user or organization), else the token's own.
pub async fn browse(http: &Http, owner: Option<&str>) -> anyhow::Result<Vec<Available>> {
    let list = match owner.map(str::trim).filter(|o| !o.is_empty()) {
        Some(o) => {
            let o = o.trim_matches('/');
            match http.all_pages(http.url(&format!("orgs/{o}/repos?per_page=100&sort=updated"))?, 300).await {
                Ok(l) => l,
                Err(e) if status_of(&e) == Some(404) => http.all_pages(http.url(&format!("users/{o}/repos?per_page=100&sort=updated"))?, 300).await?,
                Err(e) => return Err(e),
            }
        }
        None if http.has_auth() => {
            http.all_pages(http.url("user/repos?per_page=100&sort=updated&affiliation=owner,collaborator,organization_member")?, 300).await?
        }
        None => bail!("name an owner (a user or organization) to list repositories without a token"),
    };
    Ok(list
        .iter()
        .filter(|r| r["has_issues"].as_bool() != Some(false))
        .filter_map(|r| {
            Some(Available {
                key: text(&r["full_name"])?,
                name: text(&r["name"]).unwrap_or_default(),
                description: text(&r["description"]),
                issues: r["open_issues_count"].as_u64(),
                private: r["private"].as_bool().unwrap_or(false),
                parent: None,
            })
        })
        .collect())
}

/// The people the source names, by GitHub user id.
#[derive(Default)]
struct People {
    /// id → (login, bot)
    seen: HashMap<String, (String, bool)>,
}

impl People {
    /// The person key of a user object (`{"login", "id", "type"}`).
    fn key(&mut self, u: &Value) -> Option<String> {
        let login = text(&u["login"])?;
        let key = id(&u["id"]).unwrap_or_else(|| login.to_lowercase());
        let bot = u["type"].as_str() == Some("Bot") || login.ends_with("[bot]");
        self.seen.entry(key.clone()).or_insert((login, bot));
        Some(key)
    }
}

/// Files attached in a text (`user-attachments`, older image hosts): their address and a name.
pub fn attachment_urls(text: &str, web: &Url) -> Vec<(String, String)> {
    static URL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"https?://[^\s)"'<>\]]+"#).expect("pattern"));
    static ALT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"!\[([^\]]*)\]\(\s*<?(https?://[^\s)>]+)"#).expect("pattern"));
    let alts: HashMap<String, String> = ALT.captures_iter(text).map(|c| (c[2].to_string(), c[1].trim().to_string())).collect();
    let web_host = web.host_str().unwrap_or_default();
    let mut out: Vec<(String, String)> = Vec::new();
    for m in URL.find_iter(text) {
        let s = m.as_str().trim_end_matches(['.', ',', ';', ':', '!', '?']);
        let Ok(u) = Url::parse(s) else { continue };
        let host = u.host_str().unwrap_or_default();
        let path = u.path();
        let on_web = host == web_host || host.ends_with(&format!(".{web_host}"));
        let is_file = (on_web
            && (path.contains("/user-attachments/") || path.contains("/assets/") || path.contains("/files/") || path.starts_with("/storage/user/")))
            || matches!(host, "user-images.githubusercontent.com" | "private-user-images.githubusercontent.com");
        if !is_file || out.iter().any(|(x, _)| x == s) {
            continue;
        }
        let last = u.path_segments().and_then(|mut p| p.next_back()).unwrap_or("file").replace("%20", " ");
        let name = match alts.get(s).filter(|a| a.contains('.') && !last.contains('.')) {
            Some(a) => a.clone(),
            None => last,
        };
        out.push((s.to_string(), name));
    }
    out
}

/// Labels as one text (history keeps label sets like that).
fn label_set(labels: &[String]) -> String {
    let mut v: Vec<String> = labels.iter().map(|l| crate::query::normalize_label(l)).filter(|l| !l.is_empty()).collect();
    v.sort();
    v.dedup();
    v.join(" ")
}

/// Reads the repositories (`owner/repo`) into the model.
pub async fn fetch(http: &Http, site: &Site, repos: &[String], progress: &ProgressHandle) -> anyhow::Result<Model> {
    anyhow::ensure!(!repos.is_empty(), "name at least one repository (owner/repo)");
    let web_host = site.web.host_str().unwrap_or("github.com").to_string();
    let mut m = Model { origin: format!("github:{web_host}"), source: "github".into(), markup: Format::Markdown, ..Default::default() };
    m.statuses.insert("open".into(), model::Status { name: "Open".into(), category: Some("todo".into()), by_category: true });
    m.statuses.insert("closed".into(), model::Status { name: "Closed".into(), category: Some("done".into()), by_category: true });
    m.types.insert("task".into(), model::IssueType { name: "Task".into(), level: 0 });
    m.types.insert("bug".into(), model::IssueType { name: "Bug".into(), level: 0 });
    if !http.has_auth() {
        m.notes.push(Warning::new(
            "no-token",
            "without a token GitHub allows 60 requests an hour: reading anything but a small repository takes long (the import waits for the limit)",
        ));
    }
    let mut people = People::default();
    let mut pull_requests = 0u64;
    let mut wanted: Vec<String> = repos.iter().map(|r| r.trim().trim_matches('/').to_string()).filter(|r| !r.is_empty()).collect();
    wanted.dedup();
    for repo in &wanted {
        anyhow::ensure!(repo.split('/').count() == 2, "{repo}: name repositories as owner/repo");
        set_progress(progress, "fetching", Some(repo), 0, 0);
        let r = match http.get(http.url(&format!("repos/{repo}"))?).await {
            Ok((v, _)) => v,
            Err(e) if matches!(status_of(&e), Some(404 | 403)) => {
                bail!("{repo}: no such repository, or the token can't read it{}", if http.has_auth() { "" } else { " (private repositories need a token)" })
            }
            Err(e) => return Err(e),
        };
        let full = text(&r["full_name"]).unwrap_or_else(|| repo.clone());
        let pid = id(&r["id"]).unwrap_or_else(|| full.clone());
        if r["has_issues"].as_bool() == Some(false) {
            m.notes.push(Warning::new("issues-off", "{project} has its issues turned off").with("project", full.clone()));
        }
        m.projects.push(model::Project {
            id: pid.clone(),
            key: full.clone(),
            suggested_key: key_from(text(&r["name"]).as_deref().unwrap_or(&full)),
            name: text(&r["name"]).unwrap_or_else(|| full.clone()),
            description: Text::some(r["description"].as_str(), Format::Plain),
            lead: None,
            former_keys: vec![],
            template: None,
            parent: None,
            path: Some(full.clone()),
        });
        // milestones are releases
        let milestones = http.all_pages(http.url(&format!("repos/{full}/milestones?state=all&per_page=100"))?, 10_000).await?;
        let mut by_title: HashMap<String, String> = HashMap::new();
        for ms in &milestones {
            let Some(n) = id(&ms["number"]) else { continue };
            let vid = format!("{pid}:{n}");
            let title = text(&ms["title"]).unwrap_or_else(|| format!("Milestone {n}"));
            by_title.insert(title.clone(), vid.clone());
            m.versions.push(model::Version {
                id: vid,
                project: pid.clone(),
                name: title,
                description: Text::some(ms["description"].as_str(), Format::Markdown),
                sequence: ms["number"].as_f64().unwrap_or(0.0),
                released: ms["state"].as_str() == Some("closed"),
                archived: false,
                release_date: day(&ms["due_on"]).or_else(|| day(&ms["closed_at"]).filter(|_| ms["state"] == "closed")),
                start_date: None,
            });
        }
        let all = http.all_pages(http.url(&format!("repos/{full}/issues?state=all&per_page=100&sort=created&direction=asc"))?, 1_000_000).await?;
        let issues: Vec<&Value> = all.iter().filter(|i| i.get("pull_request").is_none()).collect();
        pull_requests += (all.len() - issues.len()) as u64;
        let total = issues.len() as u64;
        for (k, it) in issues.into_iter().enumerate() {
            set_progress(progress, "fetching", Some(&full), k as u64, total);
            let issue = read_issue(http, site, &mut m, &mut people, &pid, &full, &by_title, it).await?;
            m.issues.push(issue);
        }
    }
    if pull_requests > 0 {
        m.notes.push(Warning::new("pull-requests", "pull requests aren't imported, only issues").times(pull_requests));
    }
    // names and public e-mail addresses of the people
    set_progress(progress, "people", None, 0, people.seen.len() as u64);
    for (n, (key, (login, bot))) in people.seen.iter().enumerate() {
        set_progress(progress, "people", None, n as u64, people.seen.len() as u64);
        if login.eq_ignore_ascii_case("ghost") {
            m.people.insert(key.clone(), model::Person { name: "Deleted user".into(), email: None, active: false });
            continue;
        }
        let profile = if *bot { None } else { http.get_opt(http.url(&format!("users/{login}"))?).await.ok().flatten() };
        let p = profile.unwrap_or(Value::Null);
        m.people.insert(
            key.clone(),
            model::Person { name: text(&p["name"]).unwrap_or_else(|| login.clone()), email: text(&p["email"]).filter(|e| e.contains('@')), active: !bot },
        );
        m.aliases.insert(login.clone(), key.clone());
        m.aliases.insert(login.to_lowercase(), key.clone());
    }
    super::distinct_keys(&mut m.projects);
    Ok(m)
}

/// One issue with its timeline, sub-issues and files.
async fn read_issue(
    http: &Http,
    site: &Site,
    m: &mut Model,
    people: &mut People,
    pid: &str,
    full: &str,
    milestones: &HashMap<String, String>,
    it: &Value,
) -> anyhow::Result<model::Issue> {
    let number = it["number"].as_i64().unwrap_or(0) as i32;
    let iid = id(&it["id"]).with_context(|| format!("{full}#{number} has no id"))?;
    let created = time(&it["created_at"]);
    let updated = time(&it["updated_at"]);
    let closed = it["state"].as_str() == Some("closed");
    let author = people.key(&it["user"]);
    let assignees: Vec<String> = it["assignees"].as_array().into_iter().flatten().filter_map(|u| people.key(u)).collect();
    let labels: Vec<String> = it["labels"].as_array().into_iter().flatten().filter_map(|l| text(&l["name"]).or_else(|| text(l))).collect();
    // the type: the organization's issue type, else a bug label, else a task
    let type_id = match text(&it["type"]["name"]) {
        Some(name) => {
            let tid = format!("type:{}", name.to_lowercase());
            let level = if name.eq_ignore_ascii_case("epic") || name.eq_ignore_ascii_case("initiative") { 1 } else { 0 };
            m.types.entry(tid.clone()).or_insert(model::IssueType { name, level });
            tid
        }
        None if labels.iter().any(|l| {
            let l = l.to_lowercase();
            l.contains("bug") || l.contains("defect")
        }) =>
        {
            "bug".into()
        }
        None => "task".into(),
    };
    let body = text(&it["body"]);
    let mut attachments: Vec<Attachment> = Vec::new();
    let add_files = |t: &str, who: &Option<String>, at: Option<chrono::DateTime<chrono::Utc>>, atts: &mut Vec<Attachment>| {
        for (url, name) in attachment_urls(t, &site.web) {
            if !atts.iter().any(|a| a.id == url) {
                atts.push(Attachment {
                    id: url.clone(),
                    name,
                    mime: None,
                    size: None,
                    author: who.clone(),
                    created: at,
                    url: Some(url.clone()),
                    refs: vec![url],
                });
            }
        }
    };
    if let Some(b) = &body {
        add_files(b, &author, created, &mut attachments);
    }
    // the timeline: comments and what changed (not needed for an issue nobody touched)
    let touched = it["comments"].as_u64().unwrap_or(0) > 0 || closed || matches!((created, updated), (Some(c), Some(u)) if (u - c).num_seconds() > 10);
    let events = if touched { http.all_pages(http.url(&format!("repos/{full}/issues/{number}/timeline?per_page=100"))?, 100_000).await? } else { vec![] };
    let mut comments = Vec::new();
    for e in events.iter().filter(|e| e["event"] == "commented") {
        let who = people.key(if e["user"].is_object() { &e["user"] } else { &e["actor"] });
        let at = time(&e["created_at"]);
        let b = e["body"].as_str().unwrap_or("").to_string();
        add_files(&b, &who, at, &mut attachments);
        comments.push(model::Comment {
            id: id(&e["id"]).unwrap_or_default(),
            author: who,
            body: Text::new(b, Format::Markdown),
            created: at,
            updated: time(&e["updated_at"]),
            restricted: false,
        });
    }
    // history: walk back from where the issue is to where it started, then forward again
    let changes: Vec<&Value> = events
        .iter()
        .filter(|e| {
            matches!(
                e["event"].as_str(),
                Some("closed" | "reopened" | "assigned" | "unassigned" | "labeled" | "unlabeled" | "milestoned" | "demilestoned" | "renamed")
            )
        })
        .collect();
    let mut cur_assignees = assignees.clone();
    let mut cur_labels = labels.clone();
    let mut cur_milestone = text(&it["milestone"]["title"]);
    for e in changes.iter().rev() {
        match e["event"].as_str() {
            Some("assigned") => {
                if let Some(k) = people.key(&e["assignee"]) {
                    cur_assignees.retain(|a| *a != k);
                }
            }
            Some("unassigned") => {
                if let Some(k) = people.key(&e["assignee"])
                    && !cur_assignees.contains(&k)
                {
                    cur_assignees.push(k);
                }
            }
            Some("labeled") => {
                if let Some(l) = text(&e["label"]["name"]) {
                    cur_labels.retain(|x| *x != l);
                }
            }
            Some("unlabeled") => {
                if let Some(l) = text(&e["label"]["name"])
                    && !cur_labels.contains(&l)
                {
                    cur_labels.push(l);
                }
            }
            Some("milestoned") => cur_milestone = None,
            Some("demilestoned") => cur_milestone = text(&e["milestone"]["title"]),
            _ => {}
        }
    }
    let mut history = Vec::new();
    let mut status = "open";
    for e in changes {
        let mut items = Vec::new();
        match e["event"].as_str() {
            Some("closed") if status == "open" => {
                items.push(Item::new(Field::Status, Some("open".into()), Some("closed".into())));
                status = "closed";
            }
            Some("reopened") if status == "closed" => {
                items.push(Item::new(Field::Status, Some("closed".into()), Some("open".into())));
                status = "open";
            }
            Some(ev @ ("assigned" | "unassigned")) => {
                if let Some(k) = people.key(&e["assignee"]) {
                    let before = cur_assignees.first().cloned();
                    if ev == "assigned" {
                        if !cur_assignees.contains(&k) {
                            cur_assignees.push(k);
                        }
                    } else {
                        cur_assignees.retain(|a| *a != k);
                    }
                    let after = cur_assignees.first().cloned();
                    if before != after {
                        items.push(Item { field: Field::Assignee, old: before, old_name: None, new: after, new_name: None });
                    }
                }
            }
            Some(ev @ ("labeled" | "unlabeled")) => {
                if let Some(l) = text(&e["label"]["name"]) {
                    let before = label_set(&cur_labels);
                    if ev == "labeled" {
                        if !cur_labels.contains(&l) {
                            cur_labels.push(l);
                        }
                    } else {
                        cur_labels.retain(|x| *x != l);
                    }
                    let after = label_set(&cur_labels);
                    if before != after {
                        items.push(Item { field: Field::Labels, old: None, old_name: Some(before), new: None, new_name: Some(after) });
                    }
                }
            }
            Some(ev @ ("milestoned" | "demilestoned")) => {
                let before = cur_milestone.clone();
                cur_milestone = if ev == "milestoned" { text(&e["milestone"]["title"]) } else { None };
                if before != cur_milestone {
                    let vid = |t: &Option<String>| t.as_ref().and_then(|t| milestones.get(t).cloned());
                    items.push(Item {
                        field: Field::Version,
                        old: vid(&before),
                        old_name: before.clone(),
                        new: vid(&cur_milestone),
                        new_name: cur_milestone.clone(),
                    });
                }
            }
            Some("renamed") => {
                items.push(Item::new(Field::Title, text(&e["rename"]["from"]), text(&e["rename"]["to"])));
            }
            _ => {}
        }
        if !items.is_empty() {
            history.push(Change { at: time(&e["created_at"]), author: people.key(&e["actor"]), initial: false, items });
        }
    }
    // sub-issues: this issue is their parent
    if it["sub_issues_summary"]["total"].as_u64().unwrap_or(0) > 0 {
        match http.all_pages(http.url(&format!("repos/{full}/issues/{number}/sub_issues?per_page=100"))?, 10_000).await {
            Ok(subs) => {
                for s in subs {
                    if let Some(child) = id(&s["id"]) {
                        m.parents.push((child, iid.clone()));
                    }
                }
            }
            Err(e) if matches!(status_of(&e), Some(404 | 403)) => {}
            Err(e) => return Err(e),
        }
    }
    let resolution = closed.then(|| {
        match it["state_reason"].as_str() {
            Some("not_planned") => "Won't do",
            Some("duplicate") => "Duplicate",
            _ => "Done",
        }
        .to_string()
    });
    Ok(model::Issue {
        id: iid,
        project: pid.to_string(),
        number,
        keys: vec![],
        type_id,
        status: if closed { "closed" } else { "open" }.into(),
        priority: None,
        resolution,
        resolved: if closed { time(&it["closed_at"]) } else { None },
        title: text(&it["title"]).unwrap_or_default(),
        description: body.map(|b| Text::new(b, Format::Markdown)),
        reporter: author.clone(),
        assignee: assignees.first().cloned(),
        creator: author,
        created,
        updated,
        start: None,
        due: None,
        original_estimate: None,
        remaining_estimate: None,
        points: None,
        sprints: vec![],
        components: vec![],
        versions: text(&it["milestone"]["title"]).and_then(|t| milestones.get(&t).cloned()).into_iter().collect(),
        labels,
        watchers: assignees.iter().skip(1).cloned().collect(),
        custom: vec![],
        rank: None,
        confidential: false,
        comments,
        worklogs: vec![],
        attachments,
        history,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sites() {
        let s = site("").unwrap();
        assert_eq!((s.api.as_str(), s.web.as_str()), ("https://api.github.com/", "https://github.com/"));
        let s = site("https://github.com/").unwrap();
        assert_eq!(s.api.as_str(), "https://api.github.com/");
        let s = site("https://ghe.example.com").unwrap();
        assert_eq!((s.api.as_str(), s.web.as_str()), ("https://ghe.example.com/api/v3/", "https://ghe.example.com/"));
        let s = site("https://ghe.example.com/api/v3/").unwrap();
        assert_eq!((s.api.as_str(), s.web.as_str()), ("https://ghe.example.com/api/v3/", "https://ghe.example.com/"));
    }

    #[test]
    fn attachments_in_texts() {
        let web = Url::parse("https://github.com/").unwrap();
        let t = "See ![Screen shot.png](https://github.com/user-attachments/assets/0f1e-2d3c) and <img src=\"https://github.com/user-attachments/assets/0f1e-2d3c\">, \
                 the [log](https://github.com/user-attachments/files/123/build%20log.txt), https://user-images.githubusercontent.com/1/abc.png. \
                 Not https://github.com/octo/repo/issues/3 or https://example.com/x.png";
        let found = attachment_urls(t, &web);
        assert_eq!(
            found,
            vec![
                ("https://github.com/user-attachments/assets/0f1e-2d3c".to_string(), "Screen shot.png".to_string()),
                ("https://github.com/user-attachments/files/123/build%20log.txt".to_string(), "build log.txt".to_string()),
                ("https://user-images.githubusercontent.com/1/abc.png".to_string(), "abc.png".to_string()),
            ]
        );
    }
}
