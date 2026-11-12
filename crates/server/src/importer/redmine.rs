//! Redmine (any server with its REST API enabled) through its JSON API and an API key: projects
//! (subprojects each as their own project), trackers as issue types, statuses (closed ones are
//! done), priorities, issues with their ids as numbers, Textile or Markdown texts, categories as
//! components, versions as releases, parents, dates, estimates, custom fields, journals (notes
//! as comments, private ones restricted; details as history), time entries as time logs,
//! attachments, relations and watchers.

use std::collections::{HashMap, HashSet};

use anyhow::{Context, bail};
use serde_json::Value;

use super::http::{Http, base_url, day, id, status_of, text, time};
use super::model::{self, Attachment, Change, CustomField, Field, Format, Item, Model, Text, Worklog};
use super::{Available, ProgressHandle, Warning, key_from, set_progress};

/// A client for a Redmine, with an API key (*My account → API access key*).
pub fn client(base: &str, key: Option<&str>, allow_private: bool, progress: ProgressHandle) -> anyhow::Result<Http> {
    anyhow::ensure!(!base.trim().is_empty(), "enter the Redmine server's address");
    Http::new(base_url(base)?, key.map(|k| ("x-redmine-api-key", k.trim().to_string())), allow_private, progress)
}

/// Every item of a list Redmine pages with `offset` and `limit` (`total_count` says how many).
async fn all(http: &Http, path: &str, field: &str) -> anyhow::Result<Vec<Value>> {
    let mut out = Vec::new();
    let sep = if path.contains('?') { '&' } else { '?' };
    loop {
        let (v, _) = http.get(http.url(&format!("{path}{sep}limit=100&offset={}", out.len()))?).await?;
        let items = v[field].as_array().cloned().unwrap_or_default();
        let total = v["total_count"].as_u64().unwrap_or(0) as usize;
        let n = items.len();
        out.extend(items);
        if n == 0 || out.len() >= total || v.get("total_count").is_none() {
            break;
        }
    }
    Ok(out)
}

/// The visible projects.
async fn projects(http: &Http) -> anyhow::Result<Vec<Value>> {
    match all(http, "projects.json", "projects").await {
        Ok(p) => Ok(p),
        Err(e) if matches!(status_of(&e), Some(401)) => bail!("the API key was refused (check it under My account → API access key)"),
        Err(e) if matches!(status_of(&e), Some(403)) => {
            bail!("the server refused the API: is the REST web service enabled (Administration → Settings → API)?")
        }
        Err(e) => Err(e),
    }
}

/// Projects to choose from (`owner`: a project whose subprojects are listed).
pub async fn browse(http: &Http, owner: Option<&str>) -> anyhow::Result<Vec<Available>> {
    let all = projects(http).await?;
    let by_id: HashMap<String, &Value> = all.iter().filter_map(|p| id(&p["id"]).map(|i| (i, p))).collect();
    let parent_of = |p: &Value| p["parent"]["id"].as_i64().and_then(|i| by_id.get(&i.to_string())).and_then(|p| text(&p["identifier"]));
    let owner = owner.map(str::trim).filter(|o| !o.is_empty());
    Ok(all
        .iter()
        .filter(|p| owner.is_none_or(|o| text(&p["identifier"]).as_deref() == Some(o) || parent_of(p).as_deref() == Some(o)))
        .filter_map(|p| {
            Some(Available {
                key: text(&p["identifier"])?,
                name: text(&p["name"]).unwrap_or_default(),
                description: text(&p["description"]).map(|d| d.chars().take(200).collect()),
                issues: None,
                private: p["is_public"].as_bool() == Some(false),
                parent: parent_of(p),
            })
        })
        .collect())
}

/// Whether texts look like Textile rather than Markdown (counting what only one of them has).
pub fn detect_markup<'a>(texts: impl Iterator<Item = &'a str>) -> Format {
    use regex::Regex;
    use std::sync::LazyLock;
    static TEXTILE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r#"(?m)(^h[1-6]\. |^bq\. |^p[<>=]*\. |"[^"\n]+":(?:https?://|/)|(?:^|\s)@[^@\s][^@\n]*@(?:\s|$)|<pre>|^\*+ |^#+ [^#]|!\S+\.(?:png|jpe?g|gif)!)"#,
        )
        .expect("pattern")
    });
    static MARKDOWN: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?m)(^#{1,6} |^```|\[[^\]\n]+\]\((?:https?://|/)[^)\s]*\)|\*\*[^*\n]+\*\*|!\[[^\]\n]*\]\(|^- \[[ x]\] |^[-*] )").expect("pattern")
    });
    let (mut t, mut md) = (0usize, 0usize);
    for s in texts {
        t += TEXTILE.find_iter(s).count();
        md += MARKDOWN.find_iter(s).count();
    }
    if md > t { Format::Markdown } else { Format::Textile }
}

/// A custom field's kind here, from its format there.
fn field_kind(format: &str, multiple: bool) -> &'static str {
    match format {
        "int" | "float" => "number",
        "date" => "date",
        "bool" => "checkbox",
        "list" | "enumeration" | "key_value" if multiple => "multiselect",
        "list" | "enumeration" | "key_value" => "select",
        "user" => "user",
        "link" => "url",
        _ => "text",
    }
}

/// The values of a custom field on an issue (`value` is a string or a list).
fn values(v: &Value) -> Vec<String> {
    match v {
        Value::Array(a) => a.iter().filter_map(|x| x.as_str().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string)).collect(),
        Value::String(s) if !s.trim().is_empty() => vec![s.trim().to_string()],
        _ => vec![],
    }
}

/// The people the source names: id → name seen next to it.
#[derive(Default)]
struct People(HashMap<String, String>);

impl People {
    fn key(&mut self, u: &Value) -> Option<String> {
        let k = id(&u["id"])?;
        if let Some(n) = text(&u["name"]) {
            self.0.entry(k.clone()).or_insert(n);
        } else {
            self.0.entry(k.clone()).or_default();
        }
        Some(k)
    }
}

/// Reads the projects (identifiers, with their subprojects) into the model.
pub async fn fetch(http: &Http, asked: &[String], markup: Option<Format>, progress: &ProgressHandle) -> anyhow::Result<Model> {
    anyhow::ensure!(!asked.is_empty(), "name at least one project (its identifier)");
    let base = http.base().clone();
    let host = format!("{}{}", base.host_str().unwrap_or("redmine"), base.path().trim_end_matches('/'));
    let mut m = Model { origin: format!("redmine:{host}"), source: "redmine".into(), global_refs: true, ..Default::default() };
    set_progress(progress, "fetching", None, 0, 0);
    let me = match http.get_opt(http.url("users/current.json")?).await {
        Ok(v) => v.unwrap_or(Value::Null),
        Err(e) if matches!(status_of(&e), Some(401 | 403)) => Value::Null,
        Err(e) => return Err(e),
    };
    let admin = me["user"]["admin"].as_bool() == Some(true);
    if !http.has_auth() {
        m.notes.push(Warning::new("no-key", "without an API key only public projects can be read, without private issues and notes"));
    }
    if !admin {
        m.notes.push(Warning::new(
            "not-admin",
            "the API key isn't an administrator's: watchers, e-mail addresses, locked accounts and custom field definitions may be missing",
        ));
    }
    // projects asked for and their subprojects
    let visible = projects(http).await?;
    let by_id: HashMap<String, &Value> = visible.iter().filter_map(|p| id(&p["id"]).map(|i| (i, p))).collect();
    let mut chosen: Vec<&Value> = Vec::new();
    for a in asked.iter().map(|a| a.trim()).filter(|a| !a.is_empty()) {
        let p = visible
            .iter()
            .find(|p| text(&p["identifier"]).as_deref() == Some(a) || id(&p["id"]).as_deref() == Some(a))
            .with_context(|| format!("{a}: no such project, or the API key can't see it"))?;
        let mut stack: Vec<&Value> = vec![p];
        while let Some(p) = stack.pop() {
            if chosen.iter().any(|c| c["id"] == p["id"]) {
                continue;
            }
            chosen.push(p);
            stack.extend(visible.iter().filter(|c| c["parent"]["id"] == p["id"]));
        }
    }
    // what issues refer to
    let get_list = |path: &'static str, field: &'static str| async move {
        match http.get(http.url(path)?).await {
            Ok((v, _)) => Ok(v[field].as_array().cloned().unwrap_or_default()),
            Err(e) if matches!(status_of(&e), Some(401 | 403 | 404)) => Ok(vec![]),
            Err(e) => Err(e),
        }
    };
    for t in get_list("trackers.json", "trackers").await? {
        if let (Some(i), Some(n)) = (id(&t["id"]), text(&t["name"])) {
            m.types.insert(i, model::IssueType { name: n, level: 0 });
        }
    }
    let mut closed_status: HashSet<String> = HashSet::new();
    for s in get_list("issue_statuses.json", "issue_statuses").await? {
        if let (Some(i), Some(n)) = (id(&s["id"]), text(&s["name"])) {
            let closed = s["is_closed"].as_bool() == Some(true);
            if closed {
                closed_status.insert(i.clone());
            }
            let guess = crate::routes::import::guess_category(&n);
            let category = if closed {
                "done"
            } else if guess == "done" {
                "in_progress"
            } else {
                guess
            };
            m.statuses.insert(i, model::Status { name: n, category: Some(category.into()), by_category: false, exact: true });
        }
    }
    for (k, p) in get_list("enumerations/issue_priorities.json", "issue_priorities").await?.iter().enumerate() {
        if let (Some(i), Some(n)) = (id(&p["id"]), text(&p["name"])) {
            m.priorities.insert(i, model::Priority { name: n, sequence: -(k as f64) });
        }
    }
    // custom field definitions (administrators only), else what the values look like
    let mut definitions: HashMap<String, Value> = HashMap::new();
    for f in get_list("custom_fields.json", "custom_fields").await? {
        if f["customized_type"].as_str() == Some("issue")
            && let Some(i) = id(&f["id"])
        {
            definitions.insert(i, f);
        }
    }
    let mut people = People::default();
    let mut seen_relations: HashSet<String> = HashSet::new();
    let mut done_ratios = 0u64;
    let mut versions_seen: HashMap<(String, String), String> = HashMap::new();
    for p in &chosen {
        let pid = id(&p["id"]).context("a project without an id")?;
        let identifier = text(&p["identifier"]).unwrap_or_else(|| pid.clone());
        let parent = p["parent"]["id"].as_i64().and_then(|i| by_id.get(&i.to_string())).and_then(|x| text(&x["identifier"]));
        m.projects.push(model::Project {
            id: pid.clone(),
            key: identifier.clone(),
            suggested_key: key_from(&identifier),
            name: text(&p["name"]).unwrap_or_else(|| identifier.clone()),
            description: Text::some(p["description"].as_str(), Format::Source),
            lead: None,
            former_keys: vec![],
            template: Some("tracker".into()),
            parent,
            path: Some(identifier.clone()),
        });
        set_progress(progress, "fetching", Some(&identifier), 0, 0);
        // versions (shared ones too) and categories
        let versions = match http.get(http.url(&format!("projects/{pid}/versions.json"))?).await {
            Ok((v, _)) => v["versions"].as_array().cloned().unwrap_or_default(),
            Err(e) if matches!(status_of(&e), Some(403 | 404)) => vec![],
            Err(e) => return Err(e),
        };
        for v in &versions {
            let Some(vid) = id(&v["id"]) else { continue };
            let key = format!("{pid}:{vid}");
            versions_seen.insert((pid.clone(), vid.clone()), key.clone());
            m.versions.push(model::Version {
                id: key,
                project: pid.clone(),
                name: text(&v["name"]).unwrap_or_else(|| format!("Version {vid}")),
                description: Text::some(v["description"].as_str(), Format::Plain),
                sequence: v["id"].as_f64().unwrap_or(0.0),
                released: v["status"].as_str() == Some("closed"),
                archived: false,
                release_date: day(&v["due_date"]).or_else(|| day(&v["effective_date"])),
                start_date: None,
            });
        }
        let categories = match http.get(http.url(&format!("projects/{pid}/issue_categories.json"))?).await {
            Ok((v, _)) => v["issue_categories"].as_array().cloned().unwrap_or_default(),
            Err(e) if matches!(status_of(&e), Some(403 | 404)) => vec![],
            Err(e) => return Err(e),
        };
        for c in &categories {
            let Some(cid) = id(&c["id"]) else { continue };
            m.components.push(model::Component {
                id: format!("{pid}:{cid}"),
                project: pid.clone(),
                name: text(&c["name"]).unwrap_or_else(|| format!("Category {cid}")),
                description: None,
                lead: people.key(&c["assigned_to"]),
            });
        }
        // the issues of this project (not its subprojects'), then each with its journals
        let list = all_issues(http, &pid).await?;
        let total = list.len() as u64;
        for (k, brief) in list.iter().enumerate() {
            set_progress(progress, "fetching", Some(&identifier), k as u64, total);
            let Some(iid) = id(&brief["id"]) else { continue };
            let detail = http
                .get_opt(http.url(&format!("issues/{iid}.json?include=journals,attachments,relations,children,watchers"))?)
                .await?
                .map(|v| v["issue"].clone())
                .unwrap_or_else(|| brief.clone());
            let issue = read_issue(&base, &mut m, &mut people, &mut seen_relations, &mut versions_seen, &definitions, &closed_status, &pid, &detail);
            if detail["done_ratio"].as_i64().is_some_and(|d| d > 0 && d < 100) {
                done_ratios += 1;
            }
            m.issues.push(issue);
        }
    }
    // time entries, booked to their issues
    let mut seen_entries: HashSet<String> = HashSet::new();
    let (mut without_issue, mut elsewhere) = (0u64, 0u64);
    let mut no_time = false;
    for p in &chosen {
        let Some(pid) = id(&p["id"]) else { continue };
        let entries = match all(http, &format!("time_entries.json?project_id={pid}"), "time_entries").await {
            Ok(e) => e,
            Err(e) if matches!(status_of(&e), Some(403 | 404)) => {
                no_time = true;
                continue;
            }
            Err(e) => return Err(e),
        };
        for e in entries {
            let Some(eid) = id(&e["id"]) else { continue };
            if !seen_entries.insert(eid.clone()) {
                continue;
            }
            let Some(issue) = id(&e["issue"]["id"]) else {
                without_issue += 1;
                continue;
            };
            let Some(target) = m.issues.iter_mut().find(|i| i.id == issue) else {
                elsewhere += 1;
                continue;
            };
            target.worklogs.push(Worklog {
                id: eid,
                author: people.key(&e["user"]),
                note: text(&e["comments"]).map(|n| Text::new(n, Format::Plain)),
                created: time(&e["created_on"]),
                started: None,
                date: day(&e["spent_on"]),
                seconds: e["hours"].as_f64().unwrap_or(0.0) * 3600.0,
                category: text(&e["activity"]["name"]),
            });
        }
    }
    if no_time {
        m.notes.push(Warning::new("no-time-entries", "time entries couldn't be read (time tracking is off, or the API key may not see them)"));
    }
    if without_issue > 0 {
        m.warnings.push(Warning::new("time-without-issue", "time logged on a project rather than an issue is left out").times(without_issue));
    }
    if elsewhere > 0 {
        m.warnings.push(Warning::new("time-elsewhere", "time logged on issues of projects that aren't read is left out").times(elsewhere));
    }
    if done_ratios > 0 {
        m.warnings.push(Warning::new("done-ratio", "the percentage done of issues isn't kept").times(done_ratios));
    }
    // custom fields: their definitions, else guessed from the values
    let mut used: HashMap<String, (String, Vec<Vec<String>>)> = HashMap::new();
    for i in &m.issues {
        for (f, v) in &i.custom {
            let e = used.entry(f.clone()).or_default();
            e.1.push(v.clone());
        }
    }
    let mut guessed = false;
    for (fid, (_, vals)) in used {
        let def = definitions.get(&fid);
        let (name, kind, options) = match def {
            Some(d) => {
                let kind = field_kind(d["field_format"].as_str().unwrap_or("string"), d["multiple"].as_bool() == Some(true));
                let options: Vec<String> =
                    d["possible_values"].as_array().into_iter().flatten().filter_map(|o| text(&o["label"]).or_else(|| text(&o["value"]))).collect();
                (text(&d["name"]).unwrap_or_else(|| format!("Field {fid}")), kind, options)
            }
            None => {
                guessed = true;
                let flat: Vec<&String> = vals.iter().flatten().collect();
                let kind = if vals.iter().any(|v| v.len() > 1) {
                    "multiselect"
                } else if !flat.is_empty() && flat.iter().all(|v| v.parse::<f64>().is_ok()) {
                    "number"
                } else if !flat.is_empty() && flat.iter().all(|v| chrono::NaiveDate::parse_from_str(v, "%Y-%m-%d").is_ok()) {
                    "date"
                } else {
                    "text"
                };
                (m.fields.get(&fid).map(|f| f.name.clone()).unwrap_or_else(|| format!("Field {fid}")), kind, vec![])
            }
        };
        m.fields.insert(fid, CustomField { name, kind: kind.into(), options });
    }
    if guessed {
        m.notes.push(Warning::new(
            "fields-guessed",
            "custom fields read without their definitions (an administrator's key has them): their kinds are guessed from the values",
        ));
    }
    // enumeration and key/value lists store keys: their labels instead; user fields keep people
    for i in m.issues.iter_mut() {
        for (fid, vals) in i.custom.iter_mut() {
            if let Some(d) = definitions.get(fid) {
                let format = d["field_format"].as_str().unwrap_or("");
                if matches!(format, "enumeration" | "key_value") {
                    for v in vals.iter_mut() {
                        if let Some(l) =
                            d["possible_values"].as_array().into_iter().flatten().find(|o| o["value"].as_str() == Some(v)).and_then(|o| text(&o["label"]))
                        {
                            *v = l;
                        }
                    }
                }
            }
        }
    }
    let user_fields: HashSet<String> = m.fields.iter().filter(|(_, f)| f.kind == "user").map(|(k, _)| k.clone()).collect();
    for i in &m.issues {
        for (f, vals) in &i.custom {
            if user_fields.contains(f) {
                for v in vals {
                    people.0.entry(v.clone()).or_default();
                }
            }
        }
    }
    // people: names, logins, e-mail addresses (administrators see them), locked accounts
    let total = people.0.len() as u64;
    for (n, (key, seen_name)) in people.0.iter().enumerate() {
        set_progress(progress, "people", None, n as u64, total);
        let u = http.get_opt(http.url(&format!("users/{key}.json"))?).await.ok().flatten().map(|v| v["user"].clone()).unwrap_or(Value::Null);
        let full = [text(&u["firstname"]), text(&u["lastname"])].into_iter().flatten().collect::<Vec<_>>().join(" ");
        let name = if !full.trim().is_empty() {
            full
        } else if !seen_name.is_empty() {
            seen_name.clone()
        } else {
            text(&u["login"]).unwrap_or_else(|| format!("User {key}"))
        };
        m.people.insert(
            key.clone(),
            model::Person { name, email: text(&u["mail"]).filter(|e| e.contains('@')), active: u["status"].as_i64().is_none_or(|s| s == 1) },
        );
        if let Some(login) = text(&u["login"]) {
            m.aliases.insert(login.to_lowercase(), key.clone());
            m.aliases.insert(login, key.clone());
        }
    }
    // how the texts are written: as asked, else as they look
    m.markup = markup.filter(|f| matches!(f, Format::Textile | Format::Markdown)).unwrap_or_else(|| {
        detect_markup(
            m.issues.iter().flat_map(|i| i.description.iter().map(|d| d.body.as_str()).chain(i.comments.iter().map(|c| c.body.body.as_str()))).take(5000),
        )
    });
    super::distinct_keys(&mut m.projects);
    // issue numbers keep the ids; the key the project would have had keeps working for renumbered ones
    let keys: HashMap<String, String> = m.projects.iter().map(|p| (p.id.clone(), p.suggested_key.clone())).collect();
    for i in m.issues.iter_mut() {
        if let Some(k) = keys.get(&i.project) {
            i.keys.push(format!("{k}-{}", i.id));
        }
    }
    Ok(m)
}

/// The issues of one project, open and closed, without its subprojects'.
async fn all_issues(http: &Http, pid: &str) -> anyhow::Result<Vec<Value>> {
    all(http, &format!("issues.json?project_id={pid}&subproject_id=!*&status_id=*&sort=id"), "issues").await
}

/// One issue with its journals, attachments, relations and watchers.
fn read_issue(
    base: &reqwest::Url,
    m: &mut Model,
    people: &mut People,
    seen_relations: &mut HashSet<String>,
    versions: &mut HashMap<(String, String), String>,
    definitions: &HashMap<String, Value>,
    closed_status: &HashSet<String>,
    pid: &str,
    it: &Value,
) -> model::Issue {
    let iid = id(&it["id"]).unwrap_or_default();
    let number = it["id"].as_i64().unwrap_or(0) as i32;
    let created = time(&it["created_on"]);
    let updated = time(&it["updated_on"]);
    let status = id(&it["status"]["id"]).unwrap_or_default();
    if !m.statuses.contains_key(&status)
        && let Some(n) = text(&it["status"]["name"])
    {
        let closed = it["status"]["is_closed"].as_bool() == Some(true);
        m.statuses.insert(status.clone(), model::Status { name: n, category: closed.then(|| "done".into()), by_category: false, exact: true });
    }
    let type_id = id(&it["tracker"]["id"]).unwrap_or_else(|| "task".into());
    if !m.types.contains_key(&type_id) {
        m.types.insert(type_id.clone(), model::IssueType { name: text(&it["tracker"]["name"]).unwrap_or_else(|| "Task".into()), level: 0 });
    }
    let author = people.key(&it["author"]);
    let assignee = people.key(&it["assigned_to"]);
    let closed = closed_status.contains(&status) || it["status"]["is_closed"].as_bool() == Some(true);
    let status_name = text(&it["status"]["name"]).unwrap_or_default().to_lowercase();
    let resolution = closed.then(|| {
        if status_name.contains("reject") || status_name.contains("won") {
            "Won't do"
        } else if status_name.contains("duplicat") {
            "Duplicate"
        } else {
            "Done"
        }
        .to_string()
    });
    let mut version_of = |v: &str, name: Option<String>| -> String {
        versions
            .entry((pid.to_string(), v.to_string()))
            .or_insert_with(|| {
                // a version shared from another project: one of this project too
                let key = format!("{pid}:{v}");
                m.versions.push(model::Version {
                    id: key.clone(),
                    project: pid.to_string(),
                    name: name.unwrap_or_else(|| format!("Version {v}")),
                    sequence: v.parse().unwrap_or(0.0),
                    ..Default::default()
                });
                key
            })
            .clone()
    };
    let fixed = id(&it["fixed_version"]["id"]).map(|v| version_of(&v, text(&it["fixed_version"]["name"])));
    // attachments: downloaded from this server, texts name them by file name
    let attachments: Vec<Attachment> = it["attachments"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|a| {
            let aid = id(&a["id"])?;
            let name = text(&a["filename"]).unwrap_or_else(|| format!("attachment-{aid}"));
            let url = base.join(&format!("attachments/download/{aid}/{}", name.replace(' ', "%20"))).ok()?.to_string();
            Some(Attachment {
                id: aid,
                name: name.clone(),
                mime: text(&a["content_type"]),
                size: a["filesize"].as_u64(),
                author: people.key(&a["author"]),
                created: time(&a["created_on"]),
                url: Some(url),
                fallback_url: None,
                refs: vec![name],
            })
        })
        .collect();
    // journals: notes are comments, details the history
    let mut comments = Vec::new();
    let mut history = Vec::new();
    for j in it["journals"].as_array().into_iter().flatten() {
        let who = people.key(&j["user"]);
        let at = time(&j["created_on"]);
        if let Some(notes) = text(&j["notes"]) {
            comments.push(model::Comment {
                id: id(&j["id"]).unwrap_or_default(),
                author: who.clone(),
                body: Text::new(notes, Format::Source),
                created: at,
                updated: time(&j["updated_on"]),
                restricted: j["private_notes"].as_bool() == Some(true),
            });
        }
        let mut items = Vec::new();
        for d in j["details"].as_array().into_iter().flatten().filter(|d| d["property"] == "attr") {
            let (old, new) = (text(&d["old_value"]), text(&d["new_value"]));
            let field = match d["name"].as_str().unwrap_or("") {
                "status_id" => Field::Status,
                "assigned_to_id" => Field::Assignee,
                "priority_id" => Field::Priority,
                "tracker_id" => Field::Type,
                "subject" => Field::Title,
                "description" => Field::Description,
                "due_date" => Field::Due,
                "parent_id" => Field::Parent,
                "estimated_hours" => {
                    let secs = |v: Option<String>| v.and_then(|v| v.parse::<f64>().ok()).map(|h| (h * 3600.0).to_string());
                    items.push(Item { field: Field::Estimate, old: secs(old), old_name: None, new: secs(new), new_name: None });
                    continue;
                }
                "fixed_version_id" => {
                    let mut v = |x: Option<String>| x.map(|x| version_of(&x, None));
                    items.push(Item { field: Field::Version, old: v(old), old_name: None, new: v(new), new_name: None });
                    continue;
                }
                "category_id" => {
                    let c = |x: Option<String>| x.map(|x| format!("{pid}:{x}"));
                    items.push(Item { field: Field::Component, old: c(old), old_name: None, new: c(new), new_name: None });
                    continue;
                }
                _ => continue,
            };
            let name = |v: &Option<String>| match field {
                Field::Status => v.as_ref().and_then(|s| m.statuses.get(s).map(|s| s.name.clone())),
                Field::Title => v.clone(),
                _ => None,
            };
            items.push(Item { field, old_name: name(&old), new_name: name(&new), old, new });
        }
        if !items.is_empty() {
            history.push(Change { at, author: who, initial: false, items });
        }
    }
    // relations, once each
    for r in it["relations"].as_array().into_iter().flatten() {
        let Some(rid) = id(&r["id"]) else { continue };
        if !seen_relations.insert(rid) {
            continue;
        }
        let (Some(a), Some(b)) = (id(&r["issue_id"]), id(&r["issue_to_id"])) else { continue };
        let kind = r["relation_type"].as_str().unwrap_or("relates");
        let (kind, source, target) = match kind {
            "blocks" | "precedes" => ("blocks", a, b),
            "blocked" | "follows" => ("blocks", b, a),
            "duplicates" => ("duplicates", a, b),
            "duplicated" => ("duplicates", b, a),
            "copied_to" => ("clones", b, a),
            "copied_from" => ("clones", a, b),
            _ => ("relates", a, b),
        };
        m.links.push(model::Link {
            kind: kind.into(),
            known: true,
            type_name: r["relation_type"].as_str().unwrap_or("").into(),
            source,
            target,
            created: None,
        });
    }
    if let Some(parent) = id(&it["parent"]["id"]) {
        m.parents.push((iid.clone(), parent));
    }
    // custom field values (their names when the definitions aren't readable)
    let mut custom = Vec::new();
    for f in it["custom_fields"].as_array().into_iter().flatten() {
        let Some(fid) = id(&f["id"]) else { continue };
        let vals = values(&f["value"]);
        if vals.is_empty() {
            continue;
        }
        if !definitions.contains_key(&fid)
            && let Some(n) = text(&f["name"])
        {
            m.fields.entry(fid.clone()).or_insert(CustomField { name: n, kind: "text".into(), options: vec![] });
        }
        custom.push((fid, vals));
    }
    let watchers: Vec<String> = it["watchers"].as_array().into_iter().flatten().filter_map(|w| people.key(w)).collect();
    model::Issue {
        id: iid,
        project: pid.to_string(),
        number,
        keys: vec![],
        type_id,
        status,
        priority: id(&it["priority"]["id"]),
        resolution,
        resolved: if closed { time(&it["closed_on"]).or(updated) } else { None },
        title: text(&it["subject"]).unwrap_or_default(),
        description: Text::some(it["description"].as_str(), Format::Source),
        reporter: author.clone(),
        assignee,
        creator: author,
        created,
        updated,
        start: day(&it["start_date"]),
        due: day(&it["due_date"]),
        original_estimate: it["estimated_hours"].as_f64().filter(|h| *h > 0.0).map(|h| h * 3600.0),
        remaining_estimate: None,
        points: None,
        sprints: vec![],
        components: id(&it["category"]["id"]).map(|c| format!("{pid}:{c}")).into_iter().collect(),
        versions: fixed.into_iter().collect(),
        affects: vec![],
        labels: vec![],
        watchers,
        custom,
        rank: None,
        confidential: it["is_private"].as_bool() == Some(true),
        security: (it["is_private"].as_bool() == Some(true)).then(|| "Private".to_string()),
        comments,
        worklogs: vec![],
        attachments,
        history,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markup_is_found_from_the_texts() {
        let textile = ["h2. Steps\n\n* one\n* two\n\nSee \"the docs\":https://example.com and @code@.", "bq. quoted"];
        assert_eq!(detect_markup(textile.iter().copied()), Format::Textile);
        let md = ["## Steps\n\n- one\n- two\n\nSee [the docs](https://example.com) and **this**.", "```\ncode\n```"];
        assert_eq!(detect_markup(md.iter().copied()), Format::Markdown);
        assert_eq!(detect_markup(["plain words only"].iter().copied()), Format::Textile);
    }

    #[test]
    fn custom_field_kinds() {
        assert_eq!(field_kind("int", false), "number");
        assert_eq!(field_kind("list", true), "multiselect");
        assert_eq!(field_kind("enumeration", false), "select");
        assert_eq!(field_kind("user", false), "user");
        assert_eq!(field_kind("link", false), "url");
        assert_eq!(field_kind("version", false), "text");
        assert_eq!(values(&serde_json::json!(["a", " ", "b"])), vec!["a".to_string(), "b".to_string()]);
        assert_eq!(values(&serde_json::json!("")), Vec::<String>::new());
    }
}
