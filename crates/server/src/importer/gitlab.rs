//! GitLab (gitlab.com and self-managed) through its REST API (v4): a project's issues with their
//! numbers (`iid`), Markdown texts, state, labels (scoped ones too), assignees, milestones as
//! releases, weight as story points, time estimates and the time logged (from the notes that
//! record it), due dates, confidential issues, comments (internal ones restricted), issue links,
//! epics as parents where the server has them, uploads referenced in texts, and history from
//! state, label and milestone events and the notes about assignees and titles.

use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use anyhow::{Context, bail};
use chrono::{DateTime, Utc};
use regex::Regex;
use reqwest::Url;
use serde_json::Value;

use super::http::{Http, base_url, day, id, status_of, text, time};
use super::model::{self, Attachment, Change, Field, Format, Item, Model, Text, Worklog};
use super::{Available, ProgressHandle, Warning, key_from, set_progress};

/// Where a GitLab is: its web address and its API.
#[derive(Debug, Clone)]
pub struct Site {
    pub web: Url,
    pub api: Url,
}

/// gitlab.com for an empty address, else a self-managed server (`https://gitlab.example.com`,
/// with or without `/api/v4`).
pub fn site(base: &str) -> anyhow::Result<Site> {
    let b = base.trim();
    let web = if b.is_empty() { Url::parse("https://gitlab.com/")? } else { base_url(b)? };
    let mut web = web;
    if web.path().trim_end_matches('/').ends_with("/api/v4") {
        let path = web.path().trim_end_matches('/').trim_end_matches("/api/v4").to_string() + "/";
        web.set_path(&path);
    }
    let api = web.join("api/v4/")?;
    Ok(Site { web, api })
}

/// A client for a GitLab, with a personal, group or project access token (`read_api`).
pub fn client(site: &Site, token: Option<&str>, allow_private: bool, progress: ProgressHandle) -> anyhow::Result<Http> {
    Http::new(site.api.clone(), token.map(|t| ("private-token", t.trim().to_string())), allow_private, progress)
}

fn enc(path: &str) -> String {
    path.trim().trim_matches('/').replace('/', "%2F")
}

/// Projects to choose from: a group's or user's, else the token's own.
pub async fn browse(http: &Http, owner: Option<&str>) -> anyhow::Result<Vec<Available>> {
    let list = match owner.map(str::trim).filter(|o| !o.is_empty()) {
        Some(o) => match http
            .all_pages(http.url(&format!("groups/{}/projects?per_page=100&include_subgroups=true&order_by=last_activity_at&simple=true", enc(o)))?, 300)
            .await
        {
            Ok(l) => l,
            Err(e) if status_of(&e) == Some(404) => {
                http.all_pages(http.url(&format!("users/{}/projects?per_page=100&order_by=last_activity_at&simple=true", enc(o)))?, 300).await?
            }
            Err(e) => return Err(e),
        },
        None if http.has_auth() => {
            http.all_pages(http.url("projects?membership=true&per_page=100&order_by=last_activity_at&simple=true&archived=false")?, 300).await?
        }
        None => bail!("name a group or user to list projects without a token"),
    };
    Ok(list
        .iter()
        .filter(|p| p["issues_enabled"].as_bool() != Some(false))
        .filter_map(|p| {
            Some(Available {
                key: text(&p["path_with_namespace"])?,
                name: text(&p["name"]).unwrap_or_default(),
                description: text(&p["description"]),
                issues: p["open_issues_count"].as_u64(),
                private: p["visibility"].as_str().is_some_and(|v| v != "public"),
                parent: None,
            })
        })
        .collect())
}

/// A duration as GitLab writes it (`1w 2d 3h 30m`; a day is 8 hours, a week 5 days, a month 4
/// weeks), in seconds.
pub fn duration(s: &str) -> Option<f64> {
    static PART: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)(\d+(?:\.\d+)?)\s*(mo|w|d|h|m|s)\b").expect("pattern"));
    let mut total = 0.0;
    let mut any = false;
    for c in PART.captures_iter(s) {
        let n: f64 = c[1].parse().ok()?;
        total += n * match c[2].to_lowercase().as_str() {
            "mo" => 4.0 * 5.0 * 8.0 * 3600.0,
            "w" => 5.0 * 8.0 * 3600.0,
            "d" => 8.0 * 3600.0,
            "h" => 3600.0,
            "m" => 60.0,
            _ => 1.0,
        };
        any = true;
    }
    any.then_some(total)
}

/// The uploads a text refers to (`/uploads/<secret>/<name>`): the reference and the file name.
pub fn uploads(text: &str) -> Vec<(String, String, String)> {
    static UP: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"(?:https?://[^\s)"'<>]*?)?(/uploads/([0-9a-f]{16,64})/([^\s)"'<>\]]+))"#).expect("pattern"));
    let mut out: Vec<(String, String, String)> = Vec::new();
    for c in UP.captures_iter(text) {
        let whole = c[0].to_string();
        let name = c[3].trim_end_matches(['.', ',', ';']).to_string();
        if !out.iter().any(|(w, _, _)| *w == whole) {
            out.push((whole, c[2].to_string(), name));
        }
    }
    out
}

/// The people the source names, by GitLab user id.
#[derive(Default)]
struct People {
    /// id → username
    seen: HashMap<String, String>,
    /// username (lower case) → id
    by_name: HashMap<String, String>,
}

impl People {
    fn key(&mut self, u: &Value) -> Option<String> {
        let username = text(&u["username"])?;
        let key = id(&u["id"]).unwrap_or_else(|| username.to_lowercase());
        self.by_name.insert(username.to_lowercase(), key.clone());
        self.seen.entry(key.clone()).or_insert(username);
        Some(key)
    }

    fn named(&self, username: &str) -> String {
        let u = username.trim().trim_start_matches('@').to_lowercase();
        self.by_name.get(&u).cloned().unwrap_or(u)
    }
}

/// Labels as one text (history keeps label sets like that).
fn label_set(labels: &[String]) -> String {
    let mut v: Vec<String> = labels.iter().map(|l| crate::query::normalize_label(l)).filter(|l| !l.is_empty()).collect();
    v.sort();
    v.dedup();
    v.join(" ")
}

/// Reads the projects (`group/project`) into the model.
pub async fn fetch(http: &Http, site: &Site, paths: &[String], progress: &ProgressHandle) -> anyhow::Result<Model> {
    anyhow::ensure!(!paths.is_empty(), "name at least one project (group/project)");
    let host = format!("{}{}", site.web.host_str().unwrap_or("gitlab"), site.web.path().trim_end_matches('/'));
    let mut m = Model { origin: format!("gitlab:{host}"), source: "gitlab".into(), markup: Format::Markdown, ..Default::default() };
    m.statuses.insert("opened".into(), model::Status { name: "Open".into(), category: Some("todo".into()), by_category: true, exact: false });
    m.statuses.insert("closed".into(), model::Status { name: "Closed".into(), category: Some("done".into()), by_category: true, exact: false });
    for (k, name, level) in [("task", "Task", 0), ("bug", "Bug", 0), ("incident", "Incident", 0), ("test_case", "Test case", 0), ("epic", "Epic", 1)] {
        m.types.insert(k.into(), model::IssueType { name: name.into(), level });
    }
    if !http.has_auth() {
        m.notes.push(Warning::new("no-token-gitlab", "without a token only public projects can be read, and confidential issues and internal notes stay out"));
    }
    let mut people = People::default();
    let mut epics: HashMap<String, String> = HashMap::new();
    let mut seen_links: HashSet<String> = HashSet::new();
    for path in paths.iter().map(|p| p.trim().trim_matches('/')).filter(|p| !p.is_empty()) {
        set_progress(progress, "fetching", Some(path), 0, 0);
        let p = match http.get(http.url(&format!("projects/{}", enc(path)))?).await {
            Ok((v, _)) => v,
            Err(e) if matches!(status_of(&e), Some(404 | 403)) => {
                bail!("{path}: no such project, or the token can't read it{}", if http.has_auth() { "" } else { " (private projects need a token)" })
            }
            Err(e) => return Err(e),
        };
        let full = text(&p["path_with_namespace"]).unwrap_or_else(|| path.to_string());
        let pid = id(&p["id"]).with_context(|| format!("{full} has no id"))?;
        let web = text(&p["web_url"]).unwrap_or_else(|| format!("{}{full}", site.web));
        if p["issues_enabled"].as_bool() == Some(false) || p["issues_access_level"].as_str() == Some("disabled") {
            m.notes.push(Warning::new("issues-off", "{project} has its issues turned off").with("project", full.clone()));
        }
        m.projects.push(model::Project {
            id: pid.clone(),
            key: full.clone(),
            suggested_key: key_from(text(&p["path"]).as_deref().unwrap_or(&full)),
            name: text(&p["name"]).unwrap_or_else(|| full.clone()),
            description: Text::some(p["description"].as_str(), Format::Markdown),
            lead: None,
            former_keys: vec![],
            template: None,
            parent: None,
            path: Some(full.clone()),
        });
        let issues = http.all_pages(http.url(&format!("projects/{pid}/issues?scope=all&per_page=100&order_by=created_at&sort=asc"))?, 1_000_000).await?;
        let total = issues.len() as u64;
        for (k, it) in issues.iter().enumerate() {
            set_progress(progress, "fetching", Some(&full), k as u64, total);
            let issue = read_issue(http, &mut m, &mut people, &mut epics, &mut seen_links, &pid, &web, it).await?;
            m.issues.push(issue);
        }
    }
    // names, public e-mail addresses and whether they're still active
    let total = people.seen.len() as u64;
    for (n, (key, username)) in people.seen.iter().enumerate() {
        set_progress(progress, "people", None, n as u64, total);
        let u = http.get_opt(http.url(&format!("users/{key}"))?).await.ok().flatten().unwrap_or(Value::Null);
        let email = text(&u["public_email"]).or_else(|| text(&u["email"])).filter(|e| e.contains('@'));
        let active = u["state"].as_str().is_none_or(|s| s == "active") && u["bot"].as_bool() != Some(true);
        m.people.insert(key.clone(), model::Person { name: text(&u["name"]).unwrap_or_else(|| username.clone()), email, active });
        m.aliases.insert(username.clone(), key.clone());
        m.aliases.insert(username.to_lowercase(), key.clone());
    }
    super::distinct_keys(&mut m.projects);
    Ok(m)
}

/// The time logged in notes like `added 1h 30m of time spent at 2026-01-02`.
fn time_note(body: &str) -> Option<(f64, Option<chrono::NaiveDate>)> {
    static ADDED: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(added|subtracted) (.+?) of time spent(?: at (\d{4}-\d{2}-\d{2}))?\s*$").expect("pattern"));
    let c = ADDED.captures(body.trim())?;
    let secs = duration(&c[2])?;
    let date = c.get(3).and_then(|d| chrono::NaiveDate::parse_from_str(d.as_str(), "%Y-%m-%d").ok());
    Some((if &c[1] == "subtracted" { -secs } else { secs }, date))
}

/// Assignees a note says were assigned and unassigned (`assigned to @a and unassigned @b`).
fn assignee_note(body: &str) -> Option<(Vec<String>, Vec<String>)> {
    static NAME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"@([A-Za-z0-9_.-]+)").expect("pattern"));
    let b = body.trim();
    if !(b.starts_with("assigned to ") || b.starts_with("unassigned ")) {
        return None;
    }
    let (assigned, unassigned) = match b.split_once("unassigned ") {
        Some((a, u)) => (a, u),
        None => (b, ""),
    };
    let names = |s: &str| NAME.captures_iter(s).map(|c| c[1].to_string()).collect::<Vec<_>>();
    Some((names(assigned), names(unassigned)))
}

/// One issue with its notes, events, links and files.
async fn read_issue(
    http: &Http,
    m: &mut Model,
    people: &mut People,
    epics: &mut HashMap<String, String>,
    seen_links: &mut HashSet<String>,
    pid: &str,
    web: &str,
    it: &Value,
) -> anyhow::Result<model::Issue> {
    let iid = it["iid"].as_i64().unwrap_or(0);
    let gid = id(&it["id"]).with_context(|| format!("issue {iid} has no id"))?;
    let created = time(&it["created_at"]);
    let updated = time(&it["updated_at"]);
    let closed = it["state"].as_str() == Some("closed");
    let author = people.key(&it["author"]);
    let assignees: Vec<String> = it["assignees"].as_array().into_iter().flatten().filter_map(|u| people.key(u)).collect();
    let labels: Vec<String> = it["labels"].as_array().into_iter().flatten().filter_map(|l| text(l).or_else(|| text(&l["name"]))).collect();
    let type_id = match it["issue_type"].as_str().or(it["type"].as_str()).map(str::to_lowercase).as_deref() {
        Some("incident") => "incident",
        Some("test_case") => "test_case",
        Some("task") => "task",
        _ if labels.iter().any(|l| {
            let l = l.to_lowercase();
            l.contains("bug") || l.contains("defect")
        }) =>
        {
            "bug"
        }
        _ => "task",
    }
    .to_string();
    let mut attachments: Vec<Attachment> = Vec::new();
    let api_upload = |secret: &str, name: &str| http.url(&format!("projects/{pid}/uploads/{secret}/{name}")).ok().map(|u| u.to_string());
    let add_files = |t: &str, who: &Option<String>, at: Option<DateTime<Utc>>, atts: &mut Vec<Attachment>| {
        for (whole, secret, name) in uploads(t) {
            let rel = format!("/uploads/{secret}/{name}");
            if let Some(a) = atts.iter_mut().find(|a| a.id == rel) {
                if !a.refs.contains(&whole) {
                    a.refs.push(whole);
                }
                continue;
            }
            let web_url = format!("{}{rel}", web.trim_end_matches('/'));
            // with a token the API serves uploads of private projects too; else the web address
            let (url, fallback) = match (http.has_auth(), api_upload(&secret, &name)) {
                (true, Some(api)) => (api, Some(web_url)),
                _ => (web_url, None),
            };
            let mut refs = vec![rel.clone()];
            if whole != rel {
                refs.push(whole);
            }
            atts.push(Attachment {
                id: rel,
                name: name.replace("%20", " "),
                mime: None,
                size: None,
                author: who.clone(),
                created: at,
                url: Some(url),
                fallback_url: fallback,
                refs,
            });
        }
    };
    let description = text(&it["description"]);
    if let Some(d) = &description {
        add_files(d, &author, created, &mut attachments);
    }
    let touched = it["user_notes_count"].as_u64().unwrap_or(0) > 0 || closed || matches!((created, updated), (Some(c), Some(u)) if (u - c).num_seconds() > 10);
    let base = format!("projects/{pid}/issues/{iid}");
    let list = |what: &str| http.url(&format!("{base}/{what}"));
    let notes = if touched {
        match http.all_pages(list("notes?per_page=100&sort=asc&order_by=created_at")?, 100_000).await {
            Ok(n) => n,
            // some servers show notes to signed-in people only
            Err(e) if matches!(status_of(&e), Some(401 | 403)) => {
                if !m.notes.iter().any(|w| w.code == "notes-need-token") {
                    m.notes.push(Warning::new("notes-need-token", "the server shows comments, time logs and their history only with a token: they stay out"));
                }
                vec![]
            }
            Err(e) => return Err(e),
        }
    } else {
        vec![]
    };
    let mut comments = Vec::new();
    let mut worklogs = Vec::new();
    let mut history = Vec::new();
    let mut assignee_notes: Vec<(Option<DateTime<Utc>>, Option<String>, Vec<String>, Vec<String>)> = Vec::new();
    for n in &notes {
        let who = people.key(&n["author"]);
        let at = time(&n["created_at"]);
        let body = n["body"].as_str().unwrap_or("").to_string();
        if n["system"].as_bool() == Some(true) {
            if let Some((secs, date)) = time_note(&body) {
                worklogs.push(Worklog {
                    id: id(&n["id"]).unwrap_or_default(),
                    author: who,
                    note: None,
                    created: at,
                    started: None,
                    date: date.or_else(|| at.map(|t| t.date_naive())),
                    seconds: secs,
                    category: None,
                });
            } else if let Some((a, u)) = assignee_note(&body) {
                assignee_notes.push((at, who, a, u));
            } else if let Some(rest) = body.strip_prefix("changed title from ") {
                static TITLE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)^\*\*(.*)\*\* to \*\*(.*)\*\*$").expect("pattern"));
                if let Some(c) = TITLE.captures(rest.trim()) {
                    let clean = |s: &str| s.replace("{-", "").replace("-}", "").replace("{+", "").replace("+}", "");
                    history.push(Change { at, author: who, initial: false, items: vec![Item::new(Field::Title, Some(clean(&c[1])), Some(clean(&c[2])))] });
                }
            }
            continue;
        }
        add_files(&body, &who, at, &mut attachments);
        comments.push(model::Comment {
            id: id(&n["id"]).unwrap_or_default(),
            author: who,
            body: Text::new(body, Format::Markdown),
            created: at,
            updated: time(&n["updated_at"]),
            restricted: n["internal"].as_bool() == Some(true) || n["confidential"].as_bool() == Some(true),
        });
    }
    // time logged in notes; what they don't explain goes to the assignee on the closing day
    let logged: f64 = worklogs.iter().map(|w| w.seconds).sum();
    let spent = it["time_stats"]["total_time_spent"].as_f64().unwrap_or(0.0);
    let mut kept: Vec<Worklog> = worklogs.into_iter().filter(|w| w.seconds > 0.0).collect();
    if spent > 0.0 && (spent - logged).abs() >= 60.0 {
        if spent > logged {
            kept.push(Worklog {
                id: format!("{gid}:rest"),
                author: assignees.first().cloned().or(author.clone()),
                note: Some(Text::new("time spent (without its details in the source)", Format::Plain)),
                created: time(&it["closed_at"]).or(updated),
                started: None,
                date: time(&it["closed_at"]).or(updated).map(|t| t.date_naive()),
                seconds: spent - logged,
                category: None,
            });
        } else {
            m.warnings.push(Warning::new("time-subtracted", "time subtracted in the source is left out: some issues show more time spent there"));
        }
    }
    // status, labels and milestones from their events
    let events = |what: &'static str| list(&format!("{what}?per_page=100"));
    let (states, label_events, milestone_events) = if touched {
        let get = |u: Url| async move {
            match http.all_pages(u, 100_000).await {
                Ok(v) => Ok(v),
                Err(e) if matches!(status_of(&e), Some(401 | 403 | 404)) => Ok(vec![]),
                Err(e) => Err(e),
            }
        };
        (get(events("resource_state_events")?).await?, get(events("resource_label_events")?).await?, get(events("resource_milestone_events")?).await?)
    } else {
        (vec![], vec![], vec![])
    };
    let mut status = "opened";
    for e in &states {
        let new = match e["state"].as_str() {
            Some("closed") if status == "opened" => "closed",
            Some("reopened" | "opened") if status == "closed" => "opened",
            _ => continue,
        };
        history.push(Change {
            at: time(&e["created_at"]),
            author: people.key(&e["user"]),
            initial: false,
            items: vec![Item::new(Field::Status, Some(status.into()), Some(new.into()))],
        });
        status = new;
    }
    // labels: back from where they are to where they started, then forward
    let mut cur: Vec<String> = labels.clone();
    for e in label_events.iter().rev() {
        let Some(l) = text(&e["label"]["name"]) else { continue };
        match e["action"].as_str() {
            Some("add") => cur.retain(|x| *x != l),
            Some("remove") if !cur.contains(&l) => cur.push(l),
            _ => {}
        }
    }
    for e in &label_events {
        let Some(l) = text(&e["label"]["name"]) else { continue };
        let before = label_set(&cur);
        match e["action"].as_str() {
            Some("add") if !cur.contains(&l) => cur.push(l),
            Some("remove") => cur.retain(|x| *x != l),
            _ => {}
        }
        let after = label_set(&cur);
        if before != after {
            history.push(Change {
                at: time(&e["created_at"]),
                author: people.key(&e["user"]),
                initial: false,
                items: vec![Item { field: Field::Labels, old: None, old_name: Some(before), new: None, new_name: Some(after) }],
            });
        }
    }
    let vid = |ms: &Value| id(&ms["id"]).map(|i| format!("{pid}:{i}"));
    for e in &milestone_events {
        let (old, new) = match e["action"].as_str() {
            Some("add") => (None, Some(&e["milestone"])),
            Some("remove") => (Some(&e["milestone"]), None),
            _ => continue,
        };
        history.push(Change {
            at: time(&e["created_at"]),
            author: people.key(&e["user"]),
            initial: false,
            items: vec![Item {
                field: Field::Version,
                old: old.and_then(vid),
                old_name: old.and_then(|v| text(&v["title"])),
                new: new.and_then(vid),
                new_name: new.and_then(|v| text(&v["title"])),
            }],
        });
    }
    // assignees from the notes about them: back to the start, then forward
    let mut cur: Vec<String> = assignees.clone();
    for (_, _, a, u) in assignee_notes.iter().rev() {
        for x in a {
            let k = people.named(x);
            cur.retain(|c| *c != k);
        }
        for x in u {
            let k = people.named(x);
            if !cur.contains(&k) {
                cur.push(k);
            }
        }
    }
    for (at, who, a, u) in &assignee_notes {
        let before = cur.first().cloned();
        for x in u {
            let k = people.named(x);
            cur.retain(|c| *c != k);
        }
        for x in a {
            let k = people.named(x);
            if !cur.contains(&k) {
                cur.push(k);
            }
        }
        let after = cur.first().cloned();
        if before != after {
            history.push(Change {
                at: *at,
                author: who.clone(),
                initial: false,
                items: vec![Item { field: Field::Assignee, old: before, old_name: None, new: after, new_name: None }],
            });
        }
    }
    history.sort_by_key(|c| c.at);
    // links to other issues
    if touched {
        match http.all_pages(list("links?per_page=100")?, 10_000).await {
            Ok(links) => {
                for l in links {
                    let (Some(other), Some(link)) = (id(&l["id"]), id(&l["issue_link_id"])) else { continue };
                    if !seen_links.insert(link) {
                        continue;
                    }
                    let (kind, source, target) = match l["link_type"].as_str() {
                        Some("blocks") => ("blocks", gid.clone(), other),
                        Some("is_blocked_by") => ("blocks", other, gid.clone()),
                        _ => ("relates", gid.clone(), other),
                    };
                    let type_name = l["link_type"].as_str().unwrap_or("relates_to").to_string();
                    m.links.push(model::Link { kind: kind.into(), known: true, type_name, source, target, created: time(&l["link_created_at"]) });
                }
            }
            Err(e) if matches!(status_of(&e), Some(401 | 403 | 404)) => {}
            Err(e) => return Err(e),
        }
    }
    // the epic (where the server has them) becomes the parent
    if let Some(eid) = id(&it["epic"]["id"]) {
        let key = format!("epic:{eid}");
        if !epics.contains_key(&key) {
            epics.insert(key.clone(), pid.to_string());
            let group = id(&it["epic"]["group_id"]);
            let eiid = id(&it["epic"]["iid"]);
            let full = match (group, eiid) {
                (Some(g), Some(i)) => http.get_opt(http.url(&format!("groups/{g}/epics/{i}"))?).await.ok().flatten(),
                _ => None,
            };
            let e = full.as_ref().unwrap_or(&it["epic"]);
            let state = if e["state"].as_str() == Some("closed") { "closed" } else { "opened" };
            let epic_author = people.key(&e["author"]);
            m.issues.push(model::Issue {
                id: key.clone(),
                project: pid.to_string(),
                number: 0,
                type_id: "epic".into(),
                status: state.into(),
                resolution: (state == "closed").then(|| "Done".to_string()),
                resolved: time(&e["closed_at"]),
                title: text(&e["title"]).unwrap_or_else(|| format!("Epic {eid}")),
                description: Text::some(e["description"].as_str(), Format::Markdown),
                reporter: epic_author.clone(),
                creator: epic_author,
                created: time(&e["created_at"]),
                updated: time(&e["updated_at"]),
                labels: e["labels"].as_array().into_iter().flatten().filter_map(text).collect(),
                ..Default::default()
            });
        }
        m.parents.push((gid.clone(), key));
    }
    // the milestone is a release of the project
    let versions: Vec<String> = match vid(&it["milestone"]) {
        Some(v) => {
            if !m.versions.iter().any(|x| x.id == v) {
                let ms = &it["milestone"];
                m.versions.push(model::Version {
                    id: v.clone(),
                    project: pid.to_string(),
                    name: text(&ms["title"]).unwrap_or_else(|| "Milestone".into()),
                    description: Text::some(ms["description"].as_str(), Format::Markdown),
                    sequence: ms["iid"].as_f64().or(ms["id"].as_f64()).unwrap_or(0.0),
                    released: ms["state"].as_str() == Some("closed"),
                    archived: false,
                    release_date: day(&ms["due_date"]),
                    start_date: day(&ms["start_date"]),
                });
            }
            vec![v]
        }
        None => vec![],
    };
    let estimate = it["time_stats"]["time_estimate"].as_f64().filter(|s| *s > 0.0);
    Ok(model::Issue {
        id: gid,
        project: pid.to_string(),
        number: iid as i32,
        keys: vec![],
        type_id,
        status: if closed { "closed" } else { "opened" }.into(),
        priority: None,
        resolution: closed.then(|| "Done".to_string()),
        resolved: if closed { time(&it["closed_at"]).or(updated) } else { None },
        title: text(&it["title"]).unwrap_or_default(),
        description: description.map(|d| Text::new(d, Format::Markdown)),
        reporter: author.clone(),
        assignee: assignees.first().cloned(),
        creator: author,
        created,
        updated,
        start: None,
        due: day(&it["due_date"]),
        original_estimate: estimate,
        remaining_estimate: None,
        points: it["weight"].as_f64(),
        sprints: vec![],
        components: vec![],
        versions,
        affects: vec![],
        labels,
        watchers: assignees.iter().skip(1).cloned().collect(),
        custom: vec![],
        rank: None,
        confidential: it["confidential"].as_bool() == Some(true),
        security: None,
        comments,
        worklogs: kept,
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
        assert_eq!((s.web.as_str(), s.api.as_str()), ("https://gitlab.com/", "https://gitlab.com/api/v4/"));
        let s = site("https://git.example.com/gitlab/api/v4").unwrap();
        assert_eq!((s.web.as_str(), s.api.as_str()), ("https://git.example.com/gitlab/", "https://git.example.com/gitlab/api/v4/"));
    }

    #[test]
    fn durations_and_notes() {
        assert_eq!(duration("1h 30m"), Some(5400.0));
        assert_eq!(duration("1w 2d"), Some((40.0 + 16.0) * 3600.0));
        assert_eq!(duration("1mo"), Some(160.0 * 3600.0));
        assert_eq!(duration("soon"), None);
        let d = chrono::NaiveDate::from_ymd_opt(2026, 1, 2);
        assert_eq!(time_note("added 1h 30m of time spent at 2026-01-02"), Some((5400.0, d)));
        assert_eq!(time_note("subtracted 30m of time spent"), Some((-1800.0, None)));
        assert_eq!(time_note("changed the description"), None);
        assert_eq!(assignee_note("assigned to @alice and unassigned @bob"), Some((vec!["alice".into()], vec!["bob".into()])));
        assert_eq!(assignee_note("unassigned @bob"), Some((vec![], vec!["bob".into()])));
        assert_eq!(assignee_note("assigned to @a.b and @c-d"), Some((vec!["a.b".into(), "c-d".into()], vec![])));
    }

    #[test]
    fn uploads_in_texts() {
        let t = "Shot: ![screen](/uploads/0123456789abcdef0123456789abcdef/screen%20shot.png) and \
                 https://gitlab.example.com/g/p/uploads/fedcba9876543210fedcba9876543210/log.txt.";
        let u = uploads(t);
        assert_eq!(u.len(), 2);
        assert_eq!(
            u[0],
            ("/uploads/0123456789abcdef0123456789abcdef/screen%20shot.png".into(), "0123456789abcdef0123456789abcdef".into(), "screen%20shot.png".into())
        );
        assert_eq!(u[1].2, "log.txt");
        assert!(u[1].0.starts_with("https://gitlab.example.com/g/p/uploads/"));
    }
}
