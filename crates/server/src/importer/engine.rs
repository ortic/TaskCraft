//! The import itself: projects, people, issues with everything that hangs on them, then parents
//! and links once all issues exist, all in one transaction. It works on the source-neutral
//! [`Model`]; where the data came from only shows in the origin and the wording of warnings.

use std::collections::{BTreeMap, HashMap, HashSet};

use anyhow::{Context, bail};
use chrono::{DateTime, Duration, NaiveDate, Utc};
use object_store::ObjectStoreExt;
use object_store::path::Path as ObjPath;
use serde_json::{Value, json};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use super::model::{self, Change, Field, Files, Format, Issue, Item, Model, Text, TooLarge, id_order};
use super::text::{self, Names};
use super::{Choice, ExistingProject, Options, People, PeopleSummary, ProgressHandle, ProjectReport, ProjectSummary, Report, Summary, Warning, set_progress};
use crate::app::{AppState, audit, new_id};
use crate::auth::User;
use crate::events::{self, Event};
use crate::query::{normalize_label, parse_priority, priority_name, split_key};
use crate::routes::import::{Lookup, create_status_in, guess_category};

type Tx = Transaction<'static, Postgres>;

/// The kinds a custom field can have here.
const FIELD_KINDS: &[&str] = &["text", "number", "select", "multiselect", "date", "user", "checkbox", "url"];

/// Keys that look like account ids rather than names.
fn opaque(key: &str) -> bool {
    key.contains(':') || (key.len() >= 20 && key.chars().all(|c| c.is_ascii_hexdigit() || c == '-'))
}

/// The calendar day of a stored date (`2026-11-12`, also with a time after it).
pub fn parse_day(s: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(s.trim().get(..10)?, "%Y-%m-%d").ok()
}

/// Warnings, each once with how often it happened.
#[derive(Default)]
struct Warnings(Vec<Warning>, HashMap<String, usize>);

impl Warnings {
    fn add(&mut self, code: &str, template: &str, params: &[(&str, String)]) {
        self.add_n(code, template, params, 1);
    }

    /// A warning by its code (the web app words it in the reader's language) with `{name}`
    /// parameters, worded in English for the command line.
    fn add_n(&mut self, code: &str, template: &str, params: &[(&str, String)], n: u64) {
        if n == 0 {
            return;
        }
        let mut message = template.to_string();
        for (k, v) in params {
            message = message.replace(&format!("{{{k}}}"), v);
        }
        match self.1.get(&message) {
            Some(i) => self.0[*i].count += n,
            None => {
                self.1.insert(message.clone(), self.0.len());
                let params = params.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
                self.0.push(Warning { code: code.to_string(), message, params, count: n });
            }
        }
    }

    fn push(&mut self, w: &Warning) {
        let params: Vec<(&str, String)> = w.params.iter().map(|(k, v)| (k.as_str(), v.clone())).collect();
        self.add_n(&w.code, &w.message, &params, w.count);
    }
}

/// An issue as it ends up here.
#[derive(Debug, Clone)]
struct Target {
    id: Uuid,
    key: String,
    title: String,
    level: i16,
    /// made by this import (else from an earlier one)
    new: bool,
}

/// A history entry to write.
struct Ev {
    kind: &'static str,
    field: Option<&'static str>,
    old: Value,
    new: Value,
    actor: Option<Uuid>,
    at: DateTime<Utc>,
}

/// One project being imported.
struct Proj<'a> {
    src: &'a model::Project,
    id: Uuid,
    key: String,
    lookup: Lookup,
    /// source status id → (id, name, category)
    statuses: HashMap<String, (Uuid, String, String)>,
    /// source type id → (id, name, level)
    types: HashMap<String, (Uuid, String, i16)>,
    components: HashMap<String, (Uuid, String)>,
    versions: HashMap<String, (Uuid, String)>,
    /// source sprint id → (id, name, sequence)
    sprints: HashMap<String, (Uuid, String, f64)>,
    /// source field id → (id, name, kind, options)
    fields: HashMap<String, (Uuid, String, String, Value)>,
    /// lower-case security level name → id
    levels: HashMap<String, Uuid>,
    report: ProjectReport,
}

impl Proj<'_> {
    fn count(&mut self, what: &str, imported: u64, existing: u64) {
        let c = self.report.counts.entry(what.to_string()).or_default();
        c.imported += imported;
        c.existing += existing;
    }

    fn status_json(&self, id: &str) -> Value {
        self.statuses.get(id).map(|(id, n, c)| json!({"id": id, "name": n, "category": c})).unwrap_or(Value::Null)
    }

    /// The sprint of a list of sprint ids (`"12, 13"`): the latest one.
    fn sprint_of<'s>(&self, ids: impl Iterator<Item = &'s str>) -> Option<(Uuid, String)> {
        ids.filter_map(|s| self.sprints.get(s.trim())).max_by(|a, b| a.2.total_cmp(&b.2)).map(|(id, n, _)| (*id, n.clone()))
    }
}

struct Run<'a> {
    state: &'a AppState,
    user: &'a User,
    m: &'a Model,
    opts: &'a Options,
    progress: ProgressHandle,
    w: Warnings,
    /// how texts in the source's own markup are read
    markup: Format,
    /// source project id → its issues
    by_project: HashMap<&'a str, Vec<&'a Issue>>,
    /// (source project id, number in the source) → source issue id, for references in texts
    numbers: HashMap<(&'a str, i32), &'a str>,
    /// lower-case project path (`owner/repo`) → source project id
    paths: HashMap<String, &'a str>,
    /// person key → person here (None: left out)
    people: HashMap<String, Option<(Uuid, String)>>,
    people_counts: BTreeMap<String, u64>,
    /// source issue id → issue here
    targets: HashMap<String, Target>,
    /// source priority id → priority
    priorities: HashMap<String, i16>,
    /// files stored so far (removed again when the import fails)
    written: Vec<String>,
    max_file: u64,
    /// lower-case time category name → id
    categories: HashMap<String, Uuid>,
}

impl Names for Run<'_> {
    fn person(&self, key: &str) -> (Option<Uuid>, Option<String>) {
        let key = key.trim();
        // by the key the text uses, else by the person behind it (account id, user name)
        if let Some(Some((id, name))) = self.people.get(key).or_else(|| self.people.get(&self.m.canonical(key))) {
            return (Some(*id), Some(name.clone()));
        }
        (None, self.m.person(key).map(|p| p.name.clone()))
    }

    fn issue(&self, project: &str, path: Option<&str>, number: &str) -> Option<String> {
        let source: &str = if self.m.global_refs && path.is_none() {
            number
        } else {
            let pid = match path {
                Some(p) => *self.paths.get(&p.to_lowercase())?,
                None => project,
            };
            self.numbers.get(&(pid, number.parse().ok()?))?
        };
        self.targets.get(source).map(|t| t.key.clone())
    }
}

/// The TaskCraft priority of each source priority: by name, else by its place in the list.
fn priorities(m: &Model) -> HashMap<String, i16> {
    let mut list: Vec<(&String, &model::Priority)> = m.priorities.iter().collect();
    list.sort_by(|a, b| a.1.sequence.total_cmp(&b.1.sequence).then_with(|| id_order(a.0, b.0)));
    let n = list.len();
    list.iter()
        .enumerate()
        .map(|(i, (id, p))| {
            let by_place = if n <= 1 { 2 } else { 4 - ((i as f64 / (n - 1) as f64) * 3.0).round() as i16 };
            ((*id).clone(), parse_priority(&p.name).filter(|p| *p > 0).unwrap_or(by_place))
        })
        .collect()
}

fn type_look(name: &str, level: i16) -> (&'static str, &'static str) {
    let l = name.to_lowercase();
    if level < 0 {
        ("subtask", "#64748b")
    } else if level > 0 || l.contains("epic") {
        ("epic", "#8b5cf6")
    } else if l.contains("bug") || l.contains("defect") || l.contains("incident") {
        ("bug", "#e5484d")
    } else if l.contains("story") {
        ("story", "#22a06b")
    } else if l.contains("feature") || l.contains("improvement") || l.contains("idea") {
        ("lightbulb", "#22a06b")
    } else if l.contains("question") || l.contains("support") {
        ("help", "#5b8def")
    } else {
        ("task", "#3b82f6")
    }
}

fn group_by<'a, T>(rows: impl Iterator<Item = (&'a str, T)>) -> HashMap<&'a str, Vec<T>> {
    let mut m: HashMap<&str, Vec<T>> = HashMap::new();
    for (k, v) in rows {
        m.entry(k).or_default().push(v);
    }
    m
}

/// Every person key the rows of `projects` mention.
fn referenced_people(m: &Model, by_project: &HashMap<&str, Vec<&Issue>>, projects: &[&model::Project]) -> Vec<String> {
    let mut keys: Vec<String> = Vec::new();
    let mut seen = HashSet::new();
    let mut add = |k: Option<&str>| {
        if let Some(k) = k.map(str::trim).filter(|k| !k.is_empty())
            && seen.insert(k.to_string())
        {
            keys.push(k.to_string());
        }
    };
    let pids: HashSet<&str> = projects.iter().map(|p| p.id.as_str()).collect();
    for p in projects {
        add(p.lead.as_deref());
    }
    for c in m.components.iter().filter(|c| pids.contains(c.project.as_str())) {
        add(c.lead.as_deref());
    }
    let user_fields: HashSet<&str> = m.fields.iter().filter(|(_, f)| f.kind == "user").map(|(id, _)| id.as_str()).collect();
    for p in projects {
        for i in by_project.get(p.id.as_str()).into_iter().flatten() {
            add(i.reporter.as_deref());
            add(i.assignee.as_deref());
            add(i.creator.as_deref());
            for c in &i.comments {
                add(c.author.as_deref());
            }
            for w in &i.worklogs {
                add(w.author.as_deref());
            }
            for a in &i.attachments {
                add(a.author.as_deref());
            }
            for u in &i.watchers {
                add(Some(u));
            }
            for g in &i.history {
                add(g.author.as_deref());
                for it in g.items.iter().filter(|it| matches!(it.field, Field::Assignee | Field::Reporter)) {
                    add(it.old.as_deref());
                    add(it.new.as_deref());
                }
            }
            for (f, vals) in i.custom.iter().filter(|(f, _)| user_fields.contains(f.as_str())) {
                let _ = f;
                for v in vals {
                    add(Some(v));
                }
            }
        }
    }
    keys
}

impl<'a> Run<'a> {
    fn person(&self, key: Option<&str>) -> Option<Uuid> {
        key.and_then(|k| self.people.get(k.trim()).or_else(|| self.people.get(&self.m.canonical(k)))).and_then(|p| p.as_ref()).map(|p| p.0)
    }

    fn person_json(&self, key: Option<&str>, name: Option<&str>) -> Value {
        match key.map(str::trim).filter(|k| !k.is_empty()) {
            None => Value::Null,
            Some(k) => match self.people.get(k).or_else(|| self.people.get(&self.m.canonical(k))) {
                Some(Some((id, n))) => json!({"id": id, "name": n}),
                _ => {
                    json!({"id": null, "name": name.map(str::to_string).or_else(|| self.m.person(k).map(|p| p.name.clone())).unwrap_or_else(|| k.to_string())})
                }
            },
        }
    }

    /// How a text is read.
    fn fmt(&self, t: &Text) -> Format {
        if t.format == Format::Source { self.markup } else { t.format }
    }

    fn plain(&self, t: &Text) -> String {
        text::plain(t, self.fmt(t))
    }

    /// Finds or creates the people of the selected projects.
    async fn resolve_people(&mut self, tx: &mut Tx, keys: &[String]) -> anyhow::Result<()> {
        set_progress(&self.progress, "people", None, 0, keys.len() as u64);
        let ws = self.user.workspace_id;
        let existing: Vec<(Uuid, String, Option<String>)> =
            sqlx::query_as("SELECT id, name, email FROM users WHERE workspace_id = $1 ORDER BY status = 'deactivated', status = 'invited', created_at")
                .bind(ws)
                .fetch_all(&mut **tx)
                .await?;
        let known: HashMap<String, Uuid> = sqlx::query_as::<_, (String, Uuid)>(
            "SELECT s.source_id, s.target_id FROM import_sources s JOIN users u ON u.id = s.target_id WHERE s.workspace_id = $1 AND s.origin = $2 AND s.kind = 'user'",
        )
        .bind(ws)
        .bind(&self.m.origin)
        .fetch_all(&mut **tx)
        .await?
        .into_iter()
        .collect();
        let mut emails: HashSet<String> = existing.iter().filter_map(|u| u.2.as_ref().map(|e| e.to_lowercase())).collect();
        // one person may be named by several keys (user key, account id, user name)
        let mut by_person: HashMap<String, Option<(Uuid, String)>> = HashMap::new();
        for key in keys {
            let k = &self.m.canonical(key);
            if let Some(p) = by_person.get(k) {
                self.people.insert(key.clone(), p.clone());
                continue;
            }
            let src = self.m.people.get(k);
            let name = match src {
                Some(p) => p.name.clone(),
                None if opaque(k) => format!("Former user {}", k.chars().rev().take(6).collect::<Vec<_>>().into_iter().rev().collect::<String>()),
                None => k.clone(),
            };
            let by_email =
                src.and_then(|p| p.email.as_deref()).and_then(|e| existing.iter().find(|u| u.2.as_deref().is_some_and(|x| x.eq_ignore_ascii_case(e))));
            let by_name = || {
                let same: Vec<&(Uuid, String, Option<String>)> = existing.iter().filter(|u| u.1.trim().eq_ignore_ascii_case(name.trim())).collect();
                (same.len() == 1).then(|| same[0])
            };
            let found = known.get(k).map(|id| (*id, existing.iter().find(|u| u.0 == *id).map(|u| u.1.clone()).unwrap_or_else(|| name.clone())));
            let found = found.or_else(|| by_email.or_else(by_name).map(|u| (u.0, u.1.clone())));
            let person = match found {
                Some(p) => {
                    *self.people_counts.entry("matched".into()).or_default() += 1;
                    Some(p)
                }
                None if self.opts.people == People::Skip => {
                    *self.people_counts.entry("skipped".into()).or_default() += 1;
                    None
                }
                None => {
                    let active = src.is_some_and(|p| p.active) && self.opts.people == People::Invite;
                    let email = src.and_then(|p| p.email.clone()).filter(|e| !emails.contains(&e.to_lowercase()));
                    if let Some(e) = &email {
                        emails.insert(e.to_lowercase());
                    }
                    let id = new_id();
                    let display: String = name.chars().take(200).collect();
                    sqlx::query("INSERT INTO users (id, workspace_id, email, name, role, status) VALUES ($1, $2, $3, $4, 'member', $5)")
                        .bind(id)
                        .bind(ws)
                        .bind(&email)
                        .bind(&display)
                        .bind(if active { "invited" } else { "deactivated" })
                        .execute(&mut **tx)
                        .await?;
                    *self.people_counts.entry(if active { "invited" } else { "deactivated" }.into()).or_default() += 1;
                    if src.is_none() {
                        self.w.add("people-unlisted", "people the source names but doesn't list (removed from it) are imported as deactivated accounts", &[]);
                    }
                    Some((id, display))
                }
            };
            if let Some((id, _)) = &person {
                self.remember(tx, "user", k, *id).await?;
            }
            by_person.insert(k.clone(), person.clone());
            self.people.insert(k.clone(), person.clone());
            self.people.insert(key.clone(), person);
        }
        Ok(())
    }

    async fn remember(&self, tx: &mut Tx, kind: &str, source_id: &str, target: Uuid) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO import_sources (workspace_id, origin, kind, source_id, target_id) VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT (workspace_id, origin, kind, source_id) DO UPDATE SET target_id = EXCLUDED.target_id",
        )
        .bind(self.user.workspace_id)
        .bind(&self.m.origin)
        .bind(kind)
        .bind(source_id)
        .bind(target)
        .execute(&mut **tx)
        .await?;
        Ok(())
    }

    /// The project here for a source project: found by key, or created.
    async fn open_project(&mut self, tx: &mut Tx, src: &'a model::Project, choice: &Choice) -> anyhow::Result<Option<Proj<'a>>> {
        let (key, mode) = match choice {
            Choice::Skip => return Ok(None),
            Choice::Auto { key } => (key, 0),
            Choice::New { key } => (key, 1),
            Choice::Into { key } => (key, 2),
        };
        let key = crate::routes::projects::valid_key(key).map_err(|_| {
            anyhow::anyhow!("{} can't be a project key here (2–10 letters or digits, starting with a letter): choose another key for {}", key, src.key)
        })?;
        let ws = self.user.workspace_id;
        let found: Option<(Uuid, String, Option<chrono::DateTime<Utc>>)> = sqlx::query_as(
            "SELECT id, key, archived_at FROM projects WHERE workspace_id = $1 AND (lower(key) = lower($2)
                OR id IN (SELECT project_id FROM project_key_aliases WHERE workspace_id = $1 AND lower(key) = lower($2))) ORDER BY lower(key) = lower($2) DESC LIMIT 1",
        )
        .bind(ws)
        .bind(&key)
        .fetch_optional(&mut **tx)
        .await?;
        let (id, key, created) = match found {
            Some((_, k, _)) if mode == 1 => bail!("the key {k} is taken: choose another key for {}, or import into that project", src.key),
            Some((_, k, Some(_))) => bail!("{k} is archived: restore it first or import {} under another key", src.key),
            Some((id, k, None)) => (id, k, false),
            None if mode == 2 => bail!("there's no project {key} to import {} into", src.key),
            None => (self.create_project(tx, src, &key).await?, key, true),
        };
        let mut lookup = Lookup::default();
        lookup.statuses =
            sqlx::query_as("SELECT id, name, category FROM statuses WHERE project_id = $1 ORDER BY position").bind(id).fetch_all(&mut **tx).await?;
        lookup.types = sqlx::query_as("SELECT id, name, level FROM issue_types WHERE project_id = $1 ORDER BY position").bind(id).fetch_all(&mut **tx).await?;
        Ok(Some(Proj {
            src,
            id,
            key: key.clone(),
            lookup,
            statuses: HashMap::new(),
            types: HashMap::new(),
            components: HashMap::new(),
            versions: HashMap::new(),
            sprints: HashMap::new(),
            fields: HashMap::new(),
            levels: HashMap::new(),
            report: ProjectReport {
                source: src.key.clone(),
                key,
                name: src.name.clone(),
                id: Some(id),
                created,
                skipped: false,
                parent: src.parent.clone(),
                counts: BTreeMap::new(),
            },
        }))
    }

    async fn create_project(&mut self, tx: &mut Tx, src: &model::Project, key: &str) -> anyhow::Result<Uuid> {
        let has_sprints = self.by_project.get(src.id.as_str()).is_some_and(|is| is.iter().any(|i| !i.sprints.is_empty()));
        let template = match self.opts.template.as_deref().or(src.template.as_deref()) {
            Some(t) => t,
            None if has_sprints => "scrum",
            None => "kanban",
        };
        anyhow::ensure!(matches!(template, "scrum" | "kanban" | "tracker" | "business"), "template must be scrum, kanban, tracker or business");
        let taken: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM project_key_aliases WHERE workspace_id = $1 AND lower(key) = lower($2))")
            .bind(self.user.workspace_id)
            .bind(key)
            .fetch_one(&mut **tx)
            .await?;
        anyhow::ensure!(!taken, "{key} was used by a project before: choose another key for {}", src.key);
        let id = new_id();
        let estimation = if matches!(template, "tracker" | "business") { "hours" } else { "points" };
        let lead = self.person(src.lead.as_deref()).unwrap_or(self.user.id);
        let name: String = src.name.trim().chars().take(200).collect();
        sqlx::query(
            "INSERT INTO projects (id, workspace_id, key, name, description, icon, color, kind, lead_id, created_by, settings)
             VALUES ($1, $2, $3, $4, $5, 'rocket', '#4f6bed', $6, $7, $8, $9)",
        )
        .bind(id)
        .bind(self.user.workspace_id)
        .bind(key)
        .bind(if name.is_empty() { key.to_string() } else { name })
        .bind(src.description.as_ref().map(|d| self.plain(d)).filter(|d| !d.is_empty()))
        .bind(match template {
            "scrum" => "scrum",
            "tracker" => "tracker",
            _ => "kanban",
        })
        .bind(lead)
        .bind(self.user.id)
        .bind({
            let mut settings = json!({"estimation": estimation, "workflow": "free", "template": template, "importedFrom": self.m.source});
            // the source's own resolutions (Done, Won't Do, …) become the project's list
            if !self.m.resolutions.is_empty() {
                settings["resolutions"] = json!(self.m.resolutions);
            }
            settings
        })
        .execute(&mut **tx)
        .await?;
        sqlx::query("INSERT INTO project_members (project_id, subject_type, subject_id, role) VALUES ($1, 'user', $2, 'admin')")
            .bind(id)
            .bind(self.user.id)
            .execute(&mut **tx)
            .await?;
        sqlx::query("INSERT INTO project_stars (user_id, project_id) VALUES ($1, $2) ON CONFLICT DO NOTHING")
            .bind(self.user.id)
            .bind(id)
            .execute(&mut **tx)
            .await?;
        crate::routes::projects::apply_template(tx, id, key, template).await?;
        // the source brings its own sprints: no empty first sprint of the template
        if has_sprints {
            sqlx::query("DELETE FROM sprints WHERE project_id = $1").bind(id).execute(&mut **tx).await?;
        }
        Ok(id)
    }

    /// Statuses and types the project's issues use (created when missing), components,
    /// versions, sprints and custom fields.
    async fn prepare(&mut self, tx: &mut Tx, p: &mut Proj<'a>, issues: &[&'a Issue]) -> anyhow::Result<()> {
        // statuses: of the issues and of their history (deleted statuses by the name history gives)
        let mut wanted: Vec<(String, Option<String>)> = Vec::new();
        let mut seen = HashSet::new();
        for i in issues {
            if seen.insert(i.status.clone()) {
                wanted.push((i.status.clone(), None));
            }
            for g in &i.history {
                for it in g.items.iter().filter(|it| it.field == Field::Status) {
                    for (v, s) in [(&it.old, &it.old_name), (&it.new, &it.new_name)] {
                        if let Some(v) = v
                            && seen.insert(v.clone())
                        {
                            wanted.push((v.clone(), s.clone()));
                        }
                    }
                }
            }
        }
        for (sid, fallback) in wanted {
            let (name, cat, by_category, exact) = match self.m.statuses.get(&sid) {
                Some(s) => (s.name.clone(), s.category.clone(), s.by_category, s.exact),
                None => (fallback.unwrap_or_else(|| format!("Status {sid}")), None, false, false),
            };
            let category: String = cat.filter(|c| matches!(c.as_str(), "todo" | "in_progress" | "done")).unwrap_or_else(|| guess_category(&name).to_string());
            let loose = |p: &Proj| p.lookup.status(&name).and_then(|id| p.lookup.statuses.iter().find(|s| s.0 == id).cloned());
            let found = if by_category {
                p.lookup.statuses.iter().find(|s| s.2 == category).cloned()
            } else if exact {
                match p.lookup.statuses.iter().find(|s| s.1.trim().eq_ignore_ascii_case(name.trim())).cloned() {
                    // a project made by this import follows the source's workflow
                    Some(s) if s.2 != category && p.report.created => {
                        sqlx::query("UPDATE statuses SET category = $2 WHERE id = $1").bind(s.0).bind(&category).execute(&mut **tx).await?;
                        if let Some(x) = p.lookup.statuses.iter_mut().find(|x| x.0 == s.0) {
                            x.2 = category.clone();
                        }
                        Some((s.0, s.1, category.clone()))
                    }
                    Some(s) => Some(s),
                    None if p.report.created => None,
                    None => loose(p),
                }
            } else {
                loose(p)
            };
            let st = match found {
                Some(s) => s,
                None => {
                    let id = create_status_in(tx, p.id, &name, &category).await?;
                    let s = (id, name.trim().to_string(), category.clone());
                    p.lookup.statuses.push(s.clone());
                    p.count("statuses", 1, 0);
                    s
                }
            };
            p.statuses.insert(sid, st);
        }
        // issue types
        let mut type_ids: Vec<&str> = issues.iter().map(|i| i.type_id.as_str()).collect();
        type_ids.sort();
        type_ids.dedup();
        for tid in type_ids {
            let (name, level) = self.m.types.get(tid).map(|t| (t.name.clone(), t.level)).unwrap_or_else(|| (format!("Type {tid}"), 0));
            let found = p
                .lookup
                .types
                .iter()
                .find(|t| t.1.eq_ignore_ascii_case(name.trim()))
                .or_else(|| (level != 0).then(|| p.lookup.types.iter().find(|t| t.2 == level)).flatten())
                .cloned();
            let t = match found {
                Some(t) => t,
                None => {
                    let id = new_id();
                    let (icon, color) = type_look(&name, level);
                    sqlx::query(
                        "INSERT INTO issue_types (id, project_id, name, icon, color, level, position)
                         VALUES ($1, $2, $3, $4, $5, $6, (SELECT COALESCE(max(position), 0) + 1 FROM issue_types WHERE project_id = $2))",
                    )
                    .bind(id)
                    .bind(p.id)
                    .bind(name.trim())
                    .bind(icon)
                    .bind(color)
                    .bind(level)
                    .execute(&mut **tx)
                    .await?;
                    let t = (id, name.trim().to_string(), level);
                    p.lookup.types.push(t.clone());
                    p.count("types", 1, 0);
                    t
                }
            };
            p.types.insert(tid.to_string(), t);
        }
        // components and versions: by name
        let comps: Vec<&model::Component> = self.m.components.iter().filter(|c| c.project == p.src.id).collect();
        for c in comps {
            let found: Option<Uuid> = sqlx::query_scalar("SELECT id FROM components WHERE project_id = $1 AND lower(name) = lower($2)")
                .bind(p.id)
                .bind(c.name.trim())
                .fetch_optional(&mut **tx)
                .await?;
            let id = match found {
                Some(id) => {
                    p.count("components", 0, 1);
                    id
                }
                None => {
                    let id = new_id();
                    sqlx::query("INSERT INTO components (id, project_id, name, description, lead_id) VALUES ($1, $2, $3, $4, $5)")
                        .bind(id)
                        .bind(p.id)
                        .bind(c.name.trim())
                        .bind(c.description.as_ref().map(|d| self.plain(d)).filter(|d| !d.is_empty()))
                        .bind(self.person(c.lead.as_deref()))
                        .execute(&mut **tx)
                        .await?;
                    p.count("components", 1, 0);
                    id
                }
            };
            p.components.insert(c.id.clone(), (id, c.name.trim().to_string()));
        }
        let mut versions: Vec<&model::Version> = self.m.versions.iter().filter(|v| v.project == p.src.id).collect();
        versions.sort_by(|a, b| a.sequence.total_cmp(&b.sequence));
        for v in versions {
            let found: Option<Uuid> = sqlx::query_scalar("SELECT id FROM versions WHERE project_id = $1 AND lower(name) = lower($2)")
                .bind(p.id)
                .bind(v.name.trim())
                .fetch_optional(&mut **tx)
                .await?;
            let id = match found {
                Some(id) => {
                    p.count("versions", 0, 1);
                    id
                }
                None => {
                    let id = new_id();
                    let status = if v.archived {
                        "archived"
                    } else if v.released {
                        "released"
                    } else {
                        "unreleased"
                    };
                    let released_at = (v.released || v.archived).then(|| v.release_date.and_then(|d| d.and_hms_opt(12, 0, 0)).map(|t| t.and_utc())).flatten();
                    sqlx::query(
                        "INSERT INTO versions (id, project_id, name, description, status, start_date, release_date, released_at, position)
                         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
                    )
                    .bind(id)
                    .bind(p.id)
                    .bind(v.name.trim())
                    .bind(v.description.as_ref().map(|d| self.plain(d)).filter(|d| !d.is_empty()))
                    .bind(status)
                    .bind(v.start_date)
                    .bind(v.release_date)
                    .bind(released_at)
                    .bind(v.sequence)
                    .execute(&mut **tx)
                    .await?;
                    p.count("versions", 1, 0);
                    id
                }
            };
            p.versions.insert(v.id.clone(), (id, v.name.trim().to_string()));
        }
        // sprints the issues were in (now or in their history)
        let mut sprint_names: HashMap<String, String> = HashMap::new();
        let mut used: Vec<String> = Vec::new();
        let use_sprint = |id: &str, used: &mut Vec<String>| {
            let id = id.trim();
            if !id.is_empty() && !used.iter().any(|u| u == id) {
                used.push(id.to_string());
            }
        };
        for i in issues {
            for s in &i.sprints {
                use_sprint(s, &mut used);
            }
            for g in &i.history {
                for it in g.items.iter().filter(|it| it.field == Field::Sprint) {
                    for (v, s) in [(&it.old, &it.old_name), (&it.new, &it.new_name)] {
                        let ids: Vec<&str> = v.as_deref().unwrap_or("").split(',').map(str::trim).filter(|x| !x.is_empty()).collect();
                        let names: Vec<&str> = s.as_deref().unwrap_or("").split(',').map(str::trim).collect();
                        for (k, id) in ids.iter().enumerate() {
                            use_sprint(id, &mut used);
                            if ids.len() == names.len()
                                && let Some(n) = names.get(k).filter(|n| !n.is_empty())
                            {
                                sprint_names.entry(id.to_string()).or_insert_with(|| n.to_string());
                            }
                        }
                    }
                }
            }
        }
        let mut no_details = 0;
        for sid in used {
            let known: Option<Uuid> = sqlx::query_scalar(
                "SELECT s.target_id FROM import_sources s JOIN sprints sp ON sp.id = s.target_id WHERE s.workspace_id = $1 AND s.origin = $2 AND s.kind = 'sprint' AND s.source_id = $3",
            )
            .bind(self.user.workspace_id)
            .bind(&self.m.origin)
            .bind(format!("{sid}@{}", p.id))
            .fetch_optional(&mut **tx)
            .await?;
            let src = self.m.sprints.get(&sid);
            let name = src.map(|s| s.name.clone()).or_else(|| sprint_names.get(&sid).cloned()).unwrap_or_else(|| format!("Sprint {sid}"));
            let sequence = src.map(|s| s.sequence).unwrap_or_else(|| sid.parse().unwrap_or(0.0));
            let id = match known {
                Some(id) => {
                    p.count("sprints", 0, 1);
                    id
                }
                None => {
                    let id = new_id();
                    let state = match src {
                        Some(s) if s.closed => "closed",
                        Some(s) if s.started => "active",
                        _ => "future",
                    };
                    if src.is_none() {
                        no_details += 1;
                    }
                    let completed = src.filter(|s| s.closed).and_then(|s| s.completed.or(s.end));
                    sqlx::query(
                        "INSERT INTO sprints (id, project_id, name, goal, state, start_at, end_at, completed_at, position) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
                    )
                    .bind(id)
                    .bind(p.id)
                    .bind(name.chars().take(200).collect::<String>())
                    .bind(src.and_then(|s| s.goal.clone()))
                    .bind(state)
                    .bind(src.and_then(|s| s.start))
                    .bind(src.and_then(|s| s.end))
                    .bind(completed)
                    .bind(sequence)
                    .execute(&mut **tx)
                    .await?;
                    self.remember(tx, "sprint", &format!("{sid}@{}", p.id), id).await?;
                    p.count("sprints", 1, 0);
                    id
                }
            };
            p.sprints.insert(sid, (id, name, sequence));
        }
        if no_details > 0 {
            self.w.add_n(
                "sprints-no-dates",
                "sprints without their dates (the backup has no activeobjects.xml or doesn't list them) are imported as future sprints",
                &[],
                no_details,
            );
        }
        self.prepare_fields(tx, p, issues).await
    }

    /// Custom fields the issues have values for: found by name, else created; options of
    /// selects added as the values need them.
    async fn prepare_fields(&mut self, tx: &mut Tx, p: &mut Proj<'a>, issues: &[&'a Issue]) -> anyhow::Result<()> {
        let mut used: Vec<&str> = Vec::new();
        let mut labels: HashMap<&str, Vec<&str>> = HashMap::new();
        for i in issues {
            for (f, vals) in &i.custom {
                if !used.contains(&f.as_str()) {
                    used.push(f);
                }
                labels.entry(f).or_default().extend(vals.iter().map(|v| v.trim()).filter(|v| !v.is_empty()));
            }
        }
        for fid in used {
            let Some(f) = self.m.fields.get(fid) else { continue };
            let mut name: String = f.name.trim().chars().take(100).collect();
            if name.is_empty() {
                continue;
            }
            if crate::query::builtin_fields().contains(&name.to_lowercase().as_str()) {
                name = format!("{name} (imported)");
            }
            let found: Option<(Uuid, String, Value)> =
                sqlx::query_as("SELECT id, kind, options FROM custom_fields WHERE project_id = $1 AND lower(name) = lower($2)")
                    .bind(p.id)
                    .bind(&name)
                    .fetch_optional(&mut **tx)
                    .await?;
            let (id, kind, mut options) = match found {
                Some(x) => {
                    p.count("fields", 0, 1);
                    x
                }
                None => {
                    let id = new_id();
                    let kind = if FIELD_KINDS.contains(&f.kind.as_str()) { f.kind.clone() } else { "text".to_string() };
                    sqlx::query(
                        "INSERT INTO custom_fields (id, project_id, name, kind, options, position)
                         VALUES ($1, $2, $3, $4, '[]', (SELECT COALESCE(max(position), 0) + 1 FROM custom_fields WHERE project_id = $2))",
                    )
                    .bind(id)
                    .bind(p.id)
                    .bind(&name)
                    .bind(&kind)
                    .execute(&mut **tx)
                    .await?;
                    p.count("fields", 1, 0);
                    (id, kind, json!([]))
                }
            };
            if matches!(kind.as_str(), "select" | "multiselect") {
                let mut list: Vec<Value> = options.as_array().cloned().unwrap_or_default();
                let before = list.len();
                for l in f.options.iter().map(|o| o.trim()).chain(labels.get(fid).into_iter().flatten().copied()) {
                    if !l.is_empty() && !list.iter().any(|o| o["label"].as_str().is_some_and(|x| x.eq_ignore_ascii_case(l))) {
                        list.push(json!({"id": Uuid::new_v4().simple().to_string()[..12].to_string(), "label": l.chars().take(100).collect::<String>(), "color": null}));
                    }
                }
                if list.len() != before {
                    options = Value::Array(list);
                    sqlx::query("UPDATE custom_fields SET options = $2 WHERE id = $1").bind(id).bind(&options).execute(&mut **tx).await?;
                }
            }
            p.fields.insert(fid.to_string(), (id, name, kind, options));
        }
        Ok(())
    }

    /// Ids and numbers for the issues: earlier imports are found again, the others keep their
    /// number when it's free.
    async fn plan_issues(&mut self, tx: &mut Tx, p: &mut Proj<'a>) -> anyhow::Result<()> {
        let all: Vec<&Issue> = self.by_project.get(p.src.id.as_str()).cloned().unwrap_or_default();
        let ids: Vec<String> = all.iter().map(|i| i.id.clone()).collect();
        let known: Vec<(String, Uuid, String, i32, String, i16)> = sqlx::query_as(
            "SELECT s.source_id, i.id, pr.key, i.number, i.title, it.level FROM import_sources s JOIN issues i ON i.id = s.target_id
               JOIN projects pr ON pr.id = i.project_id JOIN issue_types it ON it.id = i.type_id
              WHERE s.workspace_id = $1 AND s.origin = $2 AND s.kind = 'issue' AND s.source_id = ANY($3)",
        )
        .bind(self.user.workspace_id)
        .bind(&self.m.origin)
        .bind(&ids)
        .fetch_all(&mut **tx)
        .await?;
        for (sid, id, key, n, title, level) in known {
            self.targets.insert(sid, Target { id, key: format!("{key}-{n}"), title, level, new: false });
        }
        let mut taken: HashSet<i32> =
            sqlx::query_scalar::<_, i32>("SELECT number FROM issues WHERE project_id = $1").bind(p.id).fetch_all(&mut **tx).await?.into_iter().collect();
        let mut fresh: Vec<&Issue> = all.into_iter().filter(|i| !self.targets.contains_key(&i.id)).collect();
        fresh.sort_by_key(|i| i.number);
        let mut later = Vec::new();
        let mut numbers: HashMap<&str, i32> = HashMap::new();
        for i in &fresh {
            if i.number > 0 && taken.insert(i.number) {
                numbers.insert(&i.id, i.number);
            } else {
                later.push(*i);
            }
        }
        let seq: i32 = sqlx::query_scalar("SELECT issue_seq FROM projects WHERE id = $1").bind(p.id).fetch_one(&mut **tx).await?;
        let mut next = taken.iter().copied().max().unwrap_or(0).max(seq);
        for i in later {
            next += 1;
            numbers.insert(&i.id, next);
            if i.number > 0 {
                self.w.add(
                    "number-taken",
                    "{key} is taken in {project}: the issue gets a new number (its old key keeps working)",
                    &[("key", format!("{}-{}", p.src.key, i.number)), ("project", p.key.clone())],
                );
            }
        }
        for i in fresh {
            let level = p.types.get(&i.type_id).map(|t| t.2).or_else(|| self.m.types.get(&i.type_id).map(|t| t.level)).unwrap_or(0);
            let n = numbers[i.id.as_str()];
            self.targets.insert(i.id.clone(), Target { id: new_id(), key: format!("{}-{n}", p.key), title: clean_title(&i.title, n), level, new: true });
        }
        Ok(())
    }

    fn points_value(raw: Option<&str>) -> Value {
        raw.and_then(|s| s.trim().parse::<f64>().ok()).filter(|n| n.is_finite() && *n >= 0.0).map(|n| json!(n)).unwrap_or(Value::Null)
    }

    /// The history of an issue as events, and when its status last changed and it got done.
    fn history(&mut self, p: &Proj, i: &Issue, cur: &Current, created: DateTime<Utc>) -> (Vec<Ev>, DateTime<Utc>, Option<DateTime<Utc>>) {
        let mut groups: Vec<&Change> = i.history.iter().collect();
        groups.sort_by_key(|g| g.at);
        let creator = self.person(i.creator.as_deref().or(i.reporter.as_deref()));
        // the values the issue was created with: from the creation entry, else the old value
        // of its first change, else the value it has now
        let mut initial: HashMap<Field, (Option<&str>, Option<&str>)> = HashMap::new();
        let mut changes: Vec<(&Change, &Item, bool)> = Vec::new();
        let any_estimate = groups.iter().any(|g| g.items.iter().any(|it| it.field == Field::Estimate));
        for g in &groups {
            // remaining estimates that went down because someone logged work aren't changes
            let worklog = g.items.iter().any(|it| it.field == Field::Worklog);
            for it in g.items.iter().filter(|it| it.field != Field::Worklog) {
                if g.initial {
                    initial.entry(it.field).or_insert((it.new.as_deref(), it.new_name.as_deref()));
                } else {
                    initial.entry(it.field).or_insert((it.old.as_deref(), it.old_name.as_deref()));
                    changes.push((g, it, worklog));
                }
            }
        }
        let status_v = |v: Option<&str>, s: Option<&str>| -> Value {
            match v {
                Some(v) if p.statuses.contains_key(v.trim()) => p.status_json(v.trim()),
                _ => s
                    .and_then(|s| p.lookup.status(s))
                    .and_then(|id| p.lookup.statuses.iter().find(|x| x.0 == id))
                    .map(|(id, n, c)| json!({"id": id, "name": n, "category": c}))
                    .unwrap_or(Value::Null),
            }
        };
        let sprint_v =
            |v: Option<&str>| -> Value { p.sprint_of(v.unwrap_or("").split(',')).map(|(id, n)| json!({"id": id, "name": n})).unwrap_or(Value::Null) };
        let initial_status = initial.get(&Field::Status).map(|(v, s)| status_v(*v, *s)).filter(|v| !v.is_null()).unwrap_or_else(|| p.status_json(&i.status));
        let initial_assignee = match initial.get(&Field::Assignee) {
            Some((v, _)) => self.person(*v),
            None => cur.assignee,
        };
        let initial_sprint = match initial.get(&Field::Sprint) {
            Some((v, _)) => sprint_v(*v),
            None => cur.sprint.as_ref().map(|(id, n)| json!({"id": id, "name": n})).unwrap_or(Value::Null),
        };
        let initial_points = match initial.get(&Field::Points) {
            Some((_, s)) => Self::points_value(*s),
            None => cur.points.map(|n| json!(n)).unwrap_or(Value::Null),
        };
        let mut evs = vec![Ev {
            kind: "created",
            field: Some("issue"),
            old: Value::Null,
            new: json!({"status": initial_status, "sprint": initial_sprint, "points": initial_points, "assignee": initial_assignee}),
            actor: creator,
            at: created,
        }];
        let mut last_status = initial_status.clone();
        let mut last_sprint = initial_sprint.clone();
        let mut last_points = initial_points.clone();
        let mut done_at: Option<DateTime<Utc>> = None;
        let mut status_at = created;
        let mut last_at = created;
        for (g, it, worklog) in changes {
            let f = it.field;
            let at = g.at.unwrap_or(created).max(created);
            last_at = last_at.max(at);
            let (ov, os, nv, ns) = (it.old.as_deref(), it.old_name.as_deref(), it.new.as_deref(), it.new_name.as_deref());
            let (field, old, new): (&'static str, Value, Value) = match f {
                Field::Status => ("status", status_v(ov, os), status_v(nv, ns)),
                Field::Assignee => ("assignee", self.person_json(ov, os), self.person_json(nv, ns)),
                Field::Reporter => ("reporter", self.person_json(ov, os), self.person_json(nv, ns)),
                Field::Priority => {
                    let pr = |v: Option<&str>, s: Option<&str>| -> Value {
                        v.and_then(|v| self.priorities.get(v.trim()).copied())
                            .or_else(|| s.and_then(parse_priority))
                            .map(|n| json!(priority_name(n)))
                            .unwrap_or(Value::Null)
                    };
                    ("priority", pr(ov, os), pr(nv, ns))
                }
                Field::Type => {
                    let ty = |v: Option<&str>, s: Option<&str>| -> Value {
                        v.and_then(|v| p.types.get(v.trim()))
                            .map(|t| json!({"id": t.0, "name": t.1}))
                            .or_else(|| s.map(|s| json!({"id": null, "name": s})))
                            .unwrap_or(Value::Null)
                    };
                    ("type", ty(ov, os), ty(nv, ns))
                }
                Field::Title => ("title", json!(os.unwrap_or("")), json!(ns.unwrap_or(""))),
                Field::Description => ("description", Value::Null, Value::Null),
                Field::Due => ("due", json!(ov.and_then(parse_day)), json!(nv.and_then(parse_day))),
                Field::Labels => {
                    let ls = |s: Option<&str>| -> Value {
                        let mut v: Vec<String> = s.unwrap_or("").split_whitespace().map(normalize_label).filter(|l| !l.is_empty()).collect();
                        v.sort();
                        v.dedup();
                        json!(v)
                    };
                    ("labels", ls(os), ls(ns))
                }
                Field::Estimate | Field::Remaining => {
                    if f == Field::Remaining && (any_estimate || worklog || cur.original) {
                        continue;
                    }
                    let m = |v: Option<&str>| -> Value {
                        v.and_then(|v| v.trim().parse::<f64>().ok()).map(|s| json!((s / 60.0).round() as i64)).unwrap_or(Value::Null)
                    };
                    ("estimate", m(ov), m(nv))
                }
                Field::Component | Field::Version => {
                    let map = if f == Field::Component { &p.components } else { &p.versions };
                    let named = |v: Option<&str>, s: Option<&str>| -> Value {
                        v.and_then(|v| map.get(v.trim()))
                            .map(|(id, n)| json!({"id": id, "name": n}))
                            .or_else(|| s.map(|s| json!({"id": null, "name": s})))
                            .unwrap_or(Value::Null)
                    };
                    (if f == Field::Component { "component" } else { "version" }, named(ov, os), named(nv, ns))
                }
                Field::Parent => {
                    let parent = |v: Option<&str>, s: Option<&str>| -> Value {
                        v.and_then(|v| self.targets.get(v.trim()))
                            .map(|t| json!({"id": t.id, "key": t.key, "name": t.title}))
                            .or_else(|| s.map(|s| json!({"id": null, "key": s, "name": s})))
                            .unwrap_or(Value::Null)
                    };
                    ("parent", parent(ov, os), parent(nv, ns))
                }
                Field::Sprint => ("sprint", sprint_v(ov), sprint_v(nv)),
                Field::Points => ("points", Self::points_value(os), Self::points_value(ns)),
                Field::Worklog => continue,
            };
            if old == new && f != Field::Description {
                continue;
            }
            match f {
                Field::Status => {
                    if new.is_null() {
                        continue;
                    }
                    let was_done = last_status["category"] == "done";
                    if new["category"] == "done" && !was_done {
                        done_at = Some(at);
                    }
                    status_at = at;
                    last_status = new.clone();
                }
                Field::Sprint => last_sprint = new.clone(),
                Field::Points => last_points = new.clone(),
                _ => {}
            }
            evs.push(Ev { kind: "field", field: Some(field), old, new, actor: self.person(g.author.as_deref()), at });
        }
        // the history ends where the issue is now (reports replay it)
        let end = cur.resolved.or(i.updated).unwrap_or(last_at).max(last_at);
        let final_status = json!({"id": cur.status.0, "name": cur.status.1, "category": cur.status.2});
        if last_status["id"] != final_status["id"] {
            if cur.status.2 == "done" && last_status["category"] != "done" {
                done_at = Some(end);
            }
            evs.push(Ev { kind: "field", field: Some("status"), old: last_status, new: final_status, actor: None, at: end });
            status_at = end;
        }
        let final_sprint = cur.sprint.as_ref().map(|(id, n)| json!({"id": id, "name": n})).unwrap_or(Value::Null);
        if last_sprint["id"] != final_sprint["id"] {
            evs.push(Ev { kind: "field", field: Some("sprint"), old: last_sprint, new: final_sprint, actor: None, at: end });
        }
        let final_points = cur.points.map(|n| json!(n)).unwrap_or(Value::Null);
        if last_points.as_f64() != final_points.as_f64() {
            evs.push(Ev { kind: "field", field: Some("points"), old: last_points, new: final_points, actor: None, at: end });
        }
        let resolved = (cur.status.2 == "done").then(|| cur.resolved.or(done_at).or(i.updated).unwrap_or(created));
        (evs, status_at, resolved)
    }

    async fn write_events(tx: &mut Tx, issue: Uuid, project: Uuid, mut evs: Vec<Ev>) -> anyhow::Result<u64> {
        evs.sort_by_key(|e| e.at);
        let n = evs.len();
        let ids: Vec<Uuid> = (0..n).map(|_| new_id()).collect();
        let actors: Vec<Option<Uuid>> = evs.iter().map(|e| e.actor).collect();
        let kinds: Vec<&str> = evs.iter().map(|e| e.kind).collect();
        let fields: Vec<Option<&str>> = evs.iter().map(|e| e.field).collect();
        let at: Vec<DateTime<Utc>> = evs.iter().map(|e| e.at).collect();
        let (olds, news): (Vec<Value>, Vec<Value>) = evs.into_iter().map(|e| (e.old, e.new)).unzip();
        sqlx::query(
            "INSERT INTO issue_events (id, issue_id, project_id, actor_id, kind, field, old_value, new_value, created_at)
             SELECT u.id, $2, $3, u.actor, u.kind, u.field, u.old, u.new, u.at
               FROM UNNEST($1::uuid[], $4::uuid[], $5::text[], $6::text[], $7::jsonb[], $8::jsonb[], $9::timestamptz[]) AS u(id, actor, kind, field, old, new, at)",
        )
        .bind(&ids)
        .bind(issue)
        .bind(project)
        .bind(&actors)
        .bind(&kinds)
        .bind(&fields)
        .bind(&olds)
        .bind(&news)
        .bind(&at)
        .execute(&mut **tx)
        .await?;
        Ok(n as u64)
    }

    /// The custom field values of an issue, as this project's fields keep them.
    fn custom_values(&mut self, p: &Proj, i: &Issue) -> serde_json::Map<String, Value> {
        let mut out = serde_json::Map::new();
        for (fid, vals) in &i.custom {
            let Some((id, name, kind, options)) = p.fields.get(fid) else { continue };
            let vals: Vec<&str> = vals.iter().map(|v| v.trim()).filter(|v| !v.is_empty()).collect();
            let Some(first) = vals.first().copied() else { continue };
            let opt = |label: &str| {
                options
                    .as_array()
                    .and_then(|o| o.iter().find(|x| x["label"].as_str().is_some_and(|l| l.eq_ignore_ascii_case(label.trim()))))
                    .and_then(|x| x["id"].as_str())
                    .map(str::to_string)
            };
            let v = match kind.as_str() {
                "number" => first.replace(',', ".").parse::<f64>().ok().filter(|n| n.is_finite()).map(|n| json!(n)),
                "date" => parse_day(first).map(|d| json!(d)),
                "checkbox" => Some(json!(matches!(first.to_lowercase().as_str(), "1" | "true" | "yes" | "y" | "x"))),
                "select" => opt(first).map(|o| json!(o)),
                "multiselect" => Some(json!(vals.iter().filter_map(|v| opt(v)).collect::<Vec<_>>())),
                "user" => self.person(Some(first)).map(|u| json!(u.to_string())),
                _ => Some(json!(vals.join(", ").chars().take(5000).collect::<String>())),
            };
            match v {
                Some(v) => {
                    out.insert(id.to_string(), v);
                }
                None => self.w.add("field-value", "values that don't fit the field {name} are left out", &[("name", name.clone())]),
            }
        }
        out
    }

    /// One issue with its comments, time logs, attachments, watchers, labels and history.
    /// The project's security level of that name (any case), created when it's missing: seen by
    /// the project's members, which project admins can change afterwards.
    async fn security_level(&mut self, tx: &mut Tx, p: &mut Proj<'a>, name: &str) -> anyhow::Result<Uuid> {
        let name: String = name.trim().chars().take(60).collect();
        let k = name.to_lowercase();
        if let Some(id) = p.levels.get(&k) {
            return Ok(*id);
        }
        let found: Option<Uuid> = sqlx::query_scalar("SELECT id FROM security_levels WHERE project_id = $1 AND lower(name) = $2")
            .bind(p.id)
            .bind(&k)
            .fetch_optional(&mut **tx)
            .await?;
        let id = match found {
            Some(id) => id,
            None => {
                let id = new_id();
                sqlx::query(
                    "INSERT INTO security_levels (id, project_id, name, min_rank, position)
                     VALUES ($1, $2, $3, 2, (SELECT COALESCE(max(position), 0) + 1 FROM security_levels WHERE project_id = $2))",
                )
                .bind(id)
                .bind(p.id)
                .bind(&name)
                .execute(&mut **tx)
                .await?;
                self.w.add(
                    "security-level",
                    "private issues in {project} get the security level “{level}”: the project's members see them (not viewers or guests); who exactly can be changed in the project's settings",
                    &[("project", p.key.clone()), ("level", name.clone())],
                );
                id
            }
        };
        p.levels.insert(k, id);
        Ok(id)
    }

    /// The workspace's time category of that name (any case), created when it's missing (not
    /// billable: admins decide).
    async fn time_category(&mut self, tx: &mut Tx, name: &str) -> anyhow::Result<Uuid> {
        let name: String = name.trim().chars().take(60).collect();
        let k = name.to_lowercase();
        if let Some(id) = self.categories.get(&k) {
            return Ok(*id);
        }
        let found: Option<Uuid> = sqlx::query_scalar("SELECT id FROM time_categories WHERE workspace_id = $1 AND lower(name) = $2")
            .bind(self.user.workspace_id)
            .bind(&k)
            .fetch_optional(&mut **tx)
            .await?;
        let id = match found {
            Some(id) => id,
            None => {
                let id = new_id();
                sqlx::query(
                    "INSERT INTO time_categories (id, workspace_id, name, position)
                     VALUES ($1, $2, $3, (SELECT COALESCE(max(position), 0) + 1 FROM time_categories WHERE workspace_id = $2))",
                )
                .bind(id)
                .bind(self.user.workspace_id)
                .bind(&name)
                .execute(&mut **tx)
                .await?;
                self.w.add("time-category", "the activity “{name}” becomes a time category (not billable until an admin says so)", &[("name", name.clone())]);
                id
            }
        };
        self.categories.insert(k, id);
        Ok(id)
    }

    async fn import_issue(&mut self, tx: &mut Tx, files: &mut dyn Files, p: &mut Proj<'a>, i: &'a Issue, rank: f64) -> anyhow::Result<()> {
        let t = self.targets[&i.id].clone();
        let id = t.id;
        let now = Utc::now();
        let created = i.created.or(i.updated).unwrap_or(now);
        // where it is now
        let mut status = p.statuses.get(&i.status).cloned().ok_or_else(|| anyhow::anyhow!("status {} of {} unknown", i.status, t.key))?;
        if i.resolution.is_some()
            && status.2 != "done"
            && let Some(done) = p.lookup.statuses.iter().find(|s| s.2 == "done").cloned()
        {
            self.w.add(
                "resolved-not-done",
                "issues in “{status}” with a resolution are imported as “{done}” (done)",
                &[("status", status.1.clone()), ("done", done.1.clone())],
            );
            status = done;
        }
        let sprint = p.sprint_of(i.sprints.iter().map(String::as_str));
        let points = i.points.filter(|n| n.is_finite());
        let estimate = i.original_estimate.filter(|s| *s > 0.0).or(i.remaining_estimate.filter(|s| *s > 0.0)).map(|s| (s / 60.0).round() as i32);
        // the source's remaining estimate, next to an original one (else it follows the time logged)
        let remaining = i.remaining_estimate.filter(|s| *s >= 0.0 && i.original_estimate.is_some_and(|o| o > 0.0)).map(|s| (s / 60.0).round() as i32);
        // private in the source: private here
        let level = match i.security.clone().or_else(|| i.confidential.then(|| "Confidential".to_string())) {
            Some(name) => {
                p.count("private", 1, 0);
                Some(self.security_level(tx, p, &name).await?)
            }
            None => None,
        };
        let cur = Current {
            status: status.clone(),
            assignee: self.person(i.assignee.as_deref()),
            sprint: sprint.clone(),
            points,
            resolved: i.resolved,
            original: i.original_estimate.is_some_and(|s| s > 0.0),
        };
        // all of them: releases it's fixed in and found in, components
        let ids = |list: &[String], of: &HashMap<String, (Uuid, String)>| {
            let mut out: Vec<Uuid> = Vec::new();
            for x in list.iter().filter_map(|x| of.get(x)).map(|x| x.0) {
                if !out.contains(&x) {
                    out.push(x);
                }
            }
            out
        };
        let fix_versions = ids(&i.versions, &p.versions);
        let affected = ids(&i.affects, &p.versions);
        let components = ids(&i.components, &p.components);
        // attachments first: descriptions and comments link to them
        let mut atts: Vec<&model::Attachment> = i.attachments.iter().collect();
        atts.sort_by_key(|a| a.created);
        let mut linked: HashMap<String, Uuid> = HashMap::new();
        let mut stored: Vec<(&model::Attachment, Uuid, String, i64, String, String)> = Vec::new();
        for a in atts {
            if !files.has(a) {
                self.missing_file();
                continue;
            }
            let size = files.size(a);
            if size.is_some_and(|s| s > self.max_file) {
                self.too_large();
                continue;
            }
            let aid = new_id();
            let storage_key = format!("attachments/{id}/{aid}");
            let mut name: String = a.name.replace(['/', '\\', '\0'], "_").chars().take(200).collect();
            let mut mime = a.mime.clone().filter(|m| m.contains('/'));
            let size = if self.opts.dry_run {
                size.unwrap_or(0) as i64
            } else {
                match files.read(a, self.max_file).await {
                    Ok(Some(f)) => {
                        if !name.contains('.')
                            && let Some(n) = f.name.filter(|n| n.contains('.'))
                        {
                            name = n.replace(['/', '\\', '\0'], "_").chars().take(200).collect();
                        }
                        mime = mime.or(f.mime.filter(|m| m.contains('/') && !m.starts_with("application/octet-stream")));
                        let n = f.data.len() as i64;
                        self.state.blobs.put(&ObjPath::from(storage_key.clone()), f.data.into()).await.map_err(|e| anyhow::anyhow!("storage: {e}"))?;
                        self.written.push(storage_key.clone());
                        n
                    }
                    Ok(None) => {
                        self.missing_file();
                        continue;
                    }
                    Err(e) if e.downcast_ref::<TooLarge>().is_some() => {
                        self.too_large();
                        continue;
                    }
                    Err(e) => {
                        self.w.add("unreadable-file", "an attachment couldn't be read: {error}", &[("error", format!("{e:#}"))]);
                        continue;
                    }
                }
            };
            let mime = mime.unwrap_or_else(|| mime_guess::from_path(&name).first_or_octet_stream().to_string());
            for r in a.refs.iter().map(|r| r.trim()).filter(|r| !r.is_empty()) {
                linked.insert(r.to_string(), aid);
            }
            stored.push((a, aid, storage_key, size, name, mime));
        }
        let description = i.description.as_ref().filter(|d| !d.body.trim().is_empty()).map(|d| text::rich(d, self.fmt(d), self, &linked, &i.project));
        let description_text = description.as_ref().map(crate::doc::text).unwrap_or_default();
        let (mut evs, status_at, resolved) = self.history(p, i, &cur, created);
        let updated = i.updated.unwrap_or(created).max(created);
        let custom = self.custom_values(p, i);
        sqlx::query(
            "INSERT INTO issues (id, workspace_id, project_id, number, title, description, description_text, type_id, status_id, priority, assignee_id, reporter_id,
                                 sprint_id, story_points, estimate_minutes, start_date, due_date, rank, resolved_at, status_changed_at,
                                 created_by, created_at, updated_at, resolution, custom, remaining_minutes, security_level_id)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, $20, $21, $22, $23, $24, $25, $26, $27)",
        )
        .bind(id)
        .bind(self.user.workspace_id)
        .bind(p.id)
        .bind(t.key.rsplit_once('-').and_then(|(_, n)| n.parse::<i32>().ok()).unwrap_or(0))
        .bind(&t.title)
        .bind(&description)
        .bind(&description_text)
        .bind(p.types[&i.type_id].0)
        .bind(status.0)
        .bind(i.priority.as_deref().and_then(|x| self.priorities.get(x)).copied().unwrap_or(2))
        .bind(cur.assignee)
        .bind(self.person(i.reporter.as_deref()))
        .bind(sprint.as_ref().map(|s| s.0))
        .bind(points)
        .bind(estimate)
        .bind(i.start)
        .bind(i.due)
        .bind(rank)
        .bind(resolved)
        .bind(status_at)
        .bind(self.person(i.creator.as_deref().or(i.reporter.as_deref())))
        .bind(created)
        .bind(updated)
        .bind((status.2 == "done").then(|| {
            // its own resolution, else the source's "Done" (or first), else Done
            let names = &self.m.resolutions;
            i.resolution
                .as_deref()
                .map(str::trim)
                .filter(|n| !n.is_empty())
                .map(str::to_string)
                .or_else(|| names.iter().find(|n| n.eq_ignore_ascii_case("done")).or(names.first()).cloned())
                .unwrap_or_else(|| "Done".into())
        }))
        .bind(Value::Object(custom))
        .bind(remaining)
        .bind(level)
        .execute(&mut **tx)
        .await
        .with_context(|| format!("issue {}", t.key))?;
        for (kind, list) in [("fix", &fix_versions), ("affects", &affected)] {
            if !list.is_empty() {
                sqlx::query("INSERT INTO issue_versions (issue_id, version_id, kind) SELECT $1, unnest($2::uuid[]), $3")
                    .bind(id)
                    .bind(list)
                    .bind(kind)
                    .execute(&mut **tx)
                    .await?;
            }
        }
        if !components.is_empty() {
            sqlx::query("INSERT INTO issue_components (issue_id, component_id) SELECT $1, unnest($2::uuid[])")
                .bind(id)
                .bind(&components)
                .execute(&mut **tx)
                .await?;
        }
        // labels and watchers
        let mut labels: Vec<String> = i.labels.iter().map(|l| normalize_label(l)).filter(|l| !l.is_empty()).collect();
        labels.sort();
        labels.dedup();
        labels.truncate(30);
        if !labels.is_empty() {
            sqlx::query("INSERT INTO issue_labels (issue_id, label) SELECT $1, unnest($2::text[]) ON CONFLICT DO NOTHING")
                .bind(id)
                .bind(&labels)
                .execute(&mut **tx)
                .await?;
            p.count("labels", labels.len() as u64, 0);
        }
        let watchers: Vec<Uuid> = {
            let mut w: Vec<Uuid> = i.watchers.iter().filter_map(|u| self.person(Some(u))).collect();
            w.sort();
            w.dedup();
            w
        };
        if !watchers.is_empty() {
            sqlx::query("INSERT INTO issue_watchers (issue_id, user_id) SELECT $1, unnest($2::uuid[]) ON CONFLICT DO NOTHING")
                .bind(id)
                .bind(&watchers)
                .execute(&mut **tx)
                .await?;
            p.count("watchers", watchers.len() as u64, 0);
        }
        // comments
        let mut comments: Vec<&model::Comment> = i.comments.iter().collect();
        comments.sort_by_key(|a| a.created);
        let mut restricted = 0;
        for c in comments {
            let body = text::rich(&c.body, self.fmt(&c.body), self, &linked, &i.project);
            let body_text = crate::doc::text(&body);
            if body_text.trim().is_empty() {
                self.w.add("empty-comment", "empty comments are left out", &[]);
                continue;
            }
            restricted += u64::from(c.restricted);
            let cid = new_id();
            let at = c.created.unwrap_or(created);
            let edited_at = c.updated.unwrap_or(at).max(at);
            let author = self.person(c.author.as_deref());
            // only a group, a role or the team could read it: here the project's members
            sqlx::query(
                "INSERT INTO comments (id, issue_id, author_id, body, body_text, created_at, updated_at, edited, visibility)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
            )
            .bind(cid)
            .bind(id)
            .bind(author)
            .bind(&body)
            .bind(&body_text)
            .bind(at)
            .bind(edited_at)
            .bind(edited_at - at > Duration::minutes(1))
            .bind(c.restricted.then_some("members"))
            .execute(&mut **tx)
            .await?;
            let excerpt: String = body_text.chars().take(200).collect();
            evs.push(Ev { kind: "comment", field: Some("comment"), old: Value::Null, new: json!({"id": cid, "excerpt": excerpt}), actor: author, at });
            p.count("comments", 1, 0);
        }
        self.w.add_n(
            "restricted-comments",
            "comments that only a group or role could see in the source are for the project's members here (not viewers or guests)",
            &[],
            restricted,
        );
        // time logs, booked to whoever did the work on the day they did it
        let mut spent = 0i64;
        let mut logs: Vec<&model::Worklog> = i.worklogs.iter().collect();
        logs.sort_by(|a, b| {
            a.started
                .or(a.date.and_then(|d| d.and_hms_opt(12, 0, 0)).map(|t| t.and_utc()))
                .cmp(&b.started.or(b.date.and_then(|d| d.and_hms_opt(12, 0, 0)).map(|t| t.and_utc())))
        });
        for wl in logs {
            let minutes = (wl.seconds / 60.0).round() as i64;
            let minutes = if minutes == 0 && wl.seconds > 0.0 { 1 } else { minutes };
            if minutes <= 0 {
                continue;
            }
            let minutes = minutes.min(i32::MAX as i64) as i32;
            let day: NaiveDate = wl.date.unwrap_or_else(|| wl.started.or(wl.created).unwrap_or(created).with_timezone(&self.opts.time_zone).date_naive());
            let author = self.person(wl.author.as_deref());
            let note = wl.note.as_ref().map(|n| self.plain(n)).filter(|n| !n.trim().is_empty());
            let at = wl.created.unwrap_or(created);
            let category = match wl.category.as_deref().map(str::trim).filter(|c| !c.is_empty()) {
                Some(c) => Some(self.time_category(tx, c).await?),
                None => None,
            };
            sqlx::query(
                "INSERT INTO worklogs (id, issue_id, user_id, minutes, work_date, note, created_at, category_id) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
            )
            .bind(new_id())
            .bind(id)
            .bind(author)
            .bind(minutes)
            .bind(day)
            .bind(&note)
            .bind(at)
            .bind(category)
            .execute(&mut **tx)
            .await?;
            spent += i64::from(minutes);
            evs.push(Ev { kind: "worklog", field: Some("worklog"), old: Value::Null, new: json!({"minutes": minutes, "date": day}), actor: author, at });
            p.count("worklogs", 1, 0);
        }
        if spent > 0 {
            sqlx::query("UPDATE issues SET spent_minutes = $2 WHERE id = $1").bind(id).bind(spent.min(i32::MAX as i64) as i32).execute(&mut **tx).await?;
        }
        // the attachments, stored above
        for (a, aid, storage_key, size, name, mime) in stored {
            let author = self.person(a.author.as_deref());
            let at = a.created.unwrap_or(created);
            sqlx::query(
                "INSERT INTO attachments (id, issue_id, name, mime, size, storage_key, uploaded_by, created_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
            )
            .bind(aid)
            .bind(id)
            .bind(&name)
            .bind(&mime)
            .bind(size)
            .bind(&storage_key)
            .bind(author)
            .bind(at)
            .execute(&mut **tx)
            .await?;
            evs.push(Ev { kind: "attachment", field: Some("attachment"), old: Value::Null, new: json!({"name": name}), actor: author, at });
            p.count("attachments", 1, 0);
        }
        let n = Self::write_events(tx, id, p.id, evs).await?;
        p.count("history", n, 0);
        // the keys the issue had keep working
        let mut keys: Vec<String> = i.keys.iter().map(|k| k.trim().to_uppercase()).collect();
        keys.sort();
        keys.dedup();
        for k in keys.iter().filter(|k| **k != t.key.to_uppercase()) {
            if let Some((pk, n)) = split_key(k) {
                sqlx::query("INSERT INTO issue_key_aliases (workspace_id, project_key, number, issue_id) VALUES ($1, $2, $3, $4) ON CONFLICT DO NOTHING")
                    .bind(self.user.workspace_id)
                    .bind(pk)
                    .bind(n)
                    .bind(id)
                    .execute(&mut **tx)
                    .await?;
            }
        }
        self.remember(tx, "issue", &i.id, id).await?;
        p.count("issues", 1, 0);
        Ok(())
    }

    fn missing_file(&mut self) {
        if self.m.source == "xml-site-backup" {
            self.w.add("missing-file", "attachments whose file isn't in the backup are left out", &[]);
        } else {
            self.w.add("missing-download", "attachments whose file couldn't be found in the source are left out", &[]);
        }
    }

    fn too_large(&mut self) {
        self.w.add("file-too-large", "attachments larger than the upload limit ({mb} MB) are left out", &[("mb", (self.max_file / (1024 * 1024)).to_string())]);
    }

    /// Parents (sub-tasks, epics) and links, once every issue exists.
    async fn connect(&mut self, tx: &mut Tx, reports: &mut [ProjectReport], project_of: &HashMap<Uuid, usize>) -> anyhow::Result<()> {
        let m = self.m;
        let mut links: Vec<&model::Link> = Vec::new();
        for l in &m.links {
            if !l.known {
                self.w.add("link-type", "links of the type “{name}” are imported as “relates to”", &[("name", l.type_name.clone())]);
            }
            links.push(l);
        }
        set_progress(&self.progress, "links", None, 0, (m.parents.len() + links.len()) as u64);
        let mut done: HashSet<&str> = HashSet::new();
        for (child, parent) in &m.parents {
            let (Some(c), Some(pt)) = (self.targets.get(child), self.targets.get(parent)) else { continue };
            if !c.new || done.contains(child.as_str()) || c.id == pt.id {
                continue;
            }
            if pt.level < 0 {
                self.w.add("subtask-parent", "sub-tasks can't have children here: such parent links are left out", &[]);
                continue;
            }
            let cycle: bool = sqlx::query_scalar(
                "WITH RECURSIVE up(id, parent_id, depth) AS (SELECT id, parent_id, 0 FROM issues WHERE id = $1
                   UNION ALL SELECT i.id, i.parent_id, up.depth + 1 FROM issues i JOIN up ON i.id = up.parent_id WHERE up.depth < 50)
                 SELECT EXISTS (SELECT 1 FROM up WHERE id = $2)",
            )
            .bind(pt.id)
            .bind(c.id)
            .fetch_one(&mut **tx)
            .await?;
            if cycle {
                self.w.add("parent-cycle", "parent links that would go round in a circle are left out", &[]);
                continue;
            }
            sqlx::query("UPDATE issues SET parent_id = $2 WHERE id = $1").bind(c.id).bind(pt.id).execute(&mut **tx).await?;
            done.insert(child);
            if let Some(r) = project_of.get(&c.id).and_then(|i| reports.get_mut(*i)) {
                r.counts.entry("parents".into()).or_default().imported += 1;
            }
        }
        let mut outside = 0;
        for l in links {
            let (Some(s), Some(t)) = (self.targets.get(&l.source), self.targets.get(&l.target)) else {
                outside += 1;
                continue;
            };
            if (!s.new && !t.new) || s.id == t.id {
                continue;
            }
            let kind: &'static str = match l.kind.as_str() {
                "blocks" => "blocks",
                "duplicates" => "duplicates",
                "clones" => "clones",
                _ => "relates",
            };
            let at = l.created.unwrap_or_else(Utc::now);
            let inserted = sqlx::query(
                "INSERT INTO issue_links (id, source_id, target_id, kind, created_by, created_at) VALUES ($1, $2, $3, $4, NULL, $5) ON CONFLICT DO NOTHING",
            )
            .bind(new_id())
            .bind(s.id)
            .bind(t.id)
            .bind(kind)
            .bind(at)
            .execute(&mut **tx)
            .await?
            .rows_affected();
            if inserted == 0 {
                continue;
            }
            let (s, t) = (s.clone(), t.clone());
            for (on, other, outward) in [(&s, &t, true), (&t, &s, false)] {
                let project: Uuid = sqlx::query_scalar("SELECT project_id FROM issues WHERE id = $1").bind(on.id).fetch_one(&mut **tx).await?;
                let ev = Ev {
                    kind: "link",
                    field: Some("link"),
                    old: Value::Null,
                    new: json!({"kind": crate::routes::issues::link_label(kind, outward), "key": other.key}),
                    actor: None,
                    at,
                };
                Self::write_events(tx, on.id, project, vec![ev]).await?;
            }
            if let Some(r) = project_of.get(&s.id).and_then(|i| reports.get_mut(*i)) {
                r.counts.entry("links".into()).or_default().imported += 1;
            } else if let Some(r) = project_of.get(&t.id).and_then(|i| reports.get_mut(*i)) {
                r.counts.entry("links".into()).or_default().imported += 1;
            }
        }
        self.w.add_n("links-outside", "links to issues of projects that aren't imported are left out", &[], outside);
        Ok(())
    }
}

/// Where an issue is now.
struct Current {
    status: (Uuid, String, String),
    assignee: Option<Uuid>,
    sprint: Option<(Uuid, String)>,
    points: Option<f64>,
    resolved: Option<DateTime<Utc>>,
    /// it has an original estimate (remaining estimates are no estimate changes then)
    original: bool,
}

fn clean_title(t: &str, n: i32) -> String {
    let t: String = t.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(500).collect();
    if t.is_empty() { format!("Issue {n}") } else { t }
}

/// The projects of a source with what to do with each.
fn choices<'b>(m: &'b Model, opts: &Options) -> Vec<(&'b model::Project, Choice)> {
    let mut ps: Vec<&model::Project> = m.projects.iter().collect();
    ps.sort_by(|a, b| a.key.cmp(&b.key));
    ps.into_iter()
        .filter(|p| opts.only.is_empty() || opts.only.iter().any(|o| o.eq_ignore_ascii_case(&p.key)))
        .map(|p| (p, opts.choices.get(&p.key.to_uppercase()).cloned().unwrap_or_else(|| Choice::Auto { key: p.suggested_key.to_uppercase() })))
        .collect()
}

/// Imports a source (or with `dry_run` checks it). Runs as `user` (the workspace owner or an
/// administrator), who becomes the actor of what has no person of its own.
pub async fn run(state: &AppState, user: &User, files: &mut dyn Files, m: &Model, opts: &Options, progress: ProgressHandle) -> anyhow::Result<Report> {
    let by_project = group_by(m.issues.iter().map(|i| (i.project.as_str(), i)));
    let numbers = m.issues.iter().filter(|i| i.number > 0).map(|i| ((i.project.as_str(), i.number), i.id.as_str())).collect();
    let paths = m.projects.iter().filter_map(|p| p.path.as_ref().map(|path| (path.to_lowercase(), p.id.as_str()))).collect();
    let markup = match opts.markup.unwrap_or(m.markup) {
        Format::Source => Format::Markdown,
        f => f,
    };
    let mut run = Run {
        state,
        user,
        m,
        opts,
        progress,
        w: Warnings::default(),
        markup,
        by_project,
        numbers,
        paths,
        people: HashMap::new(),
        people_counts: BTreeMap::new(),
        targets: HashMap::new(),
        priorities: priorities(m),
        written: Vec::new(),
        max_file: state.config.max_upload_mb.saturating_mul(1024 * 1024),
        categories: HashMap::new(),
    };
    let mut tx: Tx = state.db.begin().await?;
    // one import at a time (a second one waits, then finds what the first brought in)
    sqlx::query("SELECT pg_advisory_xact_lock(7381201)").execute(&mut *tx).await?;
    let result = run.everything(&mut tx, files).await;
    match result {
        Ok(mut report) => {
            report.warnings = std::mem::take(&mut run.w.0);
            if opts.dry_run {
                tx.rollback().await?;
                return Ok(report);
            }
            set_progress(&run.progress, "saving", None, 0, 0);
            if let Err(e) = tx.commit().await {
                run.cleanup().await;
                return Err(e.into());
            }
            run.after(&report).await;
            Ok(report)
        }
        Err(e) => {
            let _ = tx.rollback().await;
            run.cleanup().await;
            Err(e)
        }
    }
}

impl<'a> Run<'a> {
    async fn everything(&mut self, tx: &mut Tx, files: &mut dyn Files) -> anyhow::Result<Report> {
        let m = self.m;
        let selected = choices(m, self.opts);
        anyhow::ensure!(!selected.is_empty(), "none of the source's projects is selected");
        let chosen: Vec<&model::Project> = selected.iter().filter(|(_, c)| *c != Choice::Skip).map(|(p, _)| *p).collect();
        let keys = referenced_people(m, &self.by_project, &chosen);
        self.resolve_people(tx, &keys).await?;
        let mut projects: Vec<Proj<'a>> = Vec::new();
        let mut reports: Vec<ProjectReport> = Vec::new();
        let mut skipped: Vec<ProjectReport> = Vec::new();
        for (src, choice) in &selected {
            match self.open_project(tx, src, choice).await? {
                Some(mut p) => {
                    let all: Vec<&Issue> = self.by_project.get(src.id.as_str()).cloned().unwrap_or_default();
                    self.prepare(tx, &mut p, &all).await?;
                    self.plan_issues(tx, &mut p).await?;
                    projects.push(p);
                }
                None => skipped.push(ProjectReport {
                    source: src.key.clone(),
                    key: String::new(),
                    name: src.name.clone(),
                    skipped: true,
                    parent: src.parent.clone(),
                    ..Default::default()
                }),
            }
        }
        let mut project_of: HashMap<Uuid, usize> = HashMap::new();
        for (pi, mut p) in projects.into_iter().enumerate() {
            let mut issues: Vec<&Issue> = self.by_project.get(p.src.id.as_str()).cloned().unwrap_or_default();
            let all = issues.len();
            issues.retain(|i| self.targets.get(&i.id).is_some_and(|t| t.new));
            issues.sort_by_key(|i| i.number);
            p.count("issues", 0, (all - issues.len()) as u64);
            // the backlog keeps its order: by the source's rank, else by number
            let mut order = issues.clone();
            order.sort_by(|x, y| match (x.rank.as_deref(), y.rank.as_deref()) {
                (Some(r), Some(s)) => r.cmp(s).then(x.number.cmp(&y.number)),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => x.number.cmp(&y.number),
            });
            let base: f64 = sqlx::query_scalar("SELECT COALESCE(max(rank), 0) FROM issues WHERE project_id = $1").bind(p.id).fetch_one(&mut **tx).await?;
            let ranks: HashMap<&str, f64> = order.iter().enumerate().map(|(k, i)| (i.id.as_str(), base + (k as f64 + 1.0) * 1024.0)).collect();
            let total = issues.len() as u64;
            for (k, i) in issues.into_iter().enumerate() {
                set_progress(&self.progress, "issues", Some(&p.key), k as u64, total);
                self.import_issue(tx, files, &mut p, i, ranks[i.id.as_str()]).await?;
                project_of.insert(self.targets[&i.id].id, pi);
            }
            sqlx::query(
                "UPDATE projects SET issue_seq = GREATEST(issue_seq, (SELECT COALESCE(max(number), 0) FROM issues WHERE project_id = $1)) WHERE id = $1",
            )
            .bind(p.id)
            .execute(&mut **tx)
            .await?;
            reports.push(p.report);
        }
        self.connect(tx, &mut reports, &project_of).await?;
        for w in &m.warnings {
            self.w.push(w);
        }
        reports.extend(skipped);
        Ok(Report { dry_run: self.opts.dry_run, origin: m.origin.clone(), people: self.people_counts.clone(), projects: reports, warnings: vec![] })
    }

    /// Removes the files stored by a failed import.
    async fn cleanup(&self) {
        for k in &self.written {
            let _ = self.state.blobs.delete(&ObjPath::from(k.clone())).await;
        }
    }

    /// Figures of closed sprints, the audit log and live updates.
    async fn after(&self, report: &Report) {
        let state = self.state;
        for p in report.projects.iter().filter(|p| !p.skipped) {
            let Some(pid) = p.id else { continue };
            let closed: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM sprints WHERE project_id = $1 AND state = 'closed' AND stats = '{}'::jsonb")
                .bind(pid)
                .fetch_all(&state.db)
                .await
                .unwrap_or_default();
            for s in closed {
                if let Ok(st) = crate::reports::sprint_stats(state, s).await {
                    let _ = sqlx::query("UPDATE sprints SET stats = $2 WHERE id = $1").bind(s).bind(st).execute(&state.db).await;
                }
            }
            events::publish(state, Event::new(self.user.workspace_id, "project").project(pid).actor(self.user.id)).await;
            events::publish(state, Event::new(self.user.workspace_id, "issue").project(pid).actor(self.user.id)).await;
        }
        let issues = report.total("issues");
        audit(
            state,
            self.user.workspace_id,
            Some(self.user.id),
            "backup.import",
            Some(report.projects.iter().filter(|p| !p.skipped).map(|p| p.key.clone()).collect::<Vec<_>>().join(", ")),
            json!({"issues": issues.imported, "people": report.people, "origin": report.origin, "source": self.m.source}),
        )
        .await;
    }
}

/// What a source holds and how it meets this workspace (the check before an import).
pub async fn summarize(state: &AppState, workspace: Uuid, m: &Model, files: &dyn Files) -> anyhow::Result<Summary> {
    let by_project = group_by(m.issues.iter().map(|i| (i.project.as_str(), i)));
    let mut projects = Vec::new();
    let mut ps: Vec<&model::Project> = m.projects.iter().collect();
    ps.sort_by(|a, b| a.key.cmp(&b.key));
    let mut has_sprints = false;
    for p in ps {
        let issues: Vec<&Issue> = by_project.get(p.id.as_str()).cloned().unwrap_or_default();
        let ids: Vec<String> = issues.iter().map(|i| i.id.clone()).collect();
        let idset: HashSet<&str> = ids.iter().map(String::as_str).collect();
        let sprints: HashSet<&str> = issues.iter().flat_map(|i| i.sprints.iter().map(String::as_str)).filter(|s| !s.trim().is_empty()).collect();
        has_sprints |= !sprints.is_empty();
        let existing: Option<(String, String, i64)> = sqlx::query_as(
            "SELECT p.key, p.name, (SELECT count(*) FROM issues i WHERE i.project_id = p.id AND i.deleted_at IS NULL) FROM projects p
              WHERE p.workspace_id = $1 AND lower(p.key) = lower($2)",
        )
        .bind(workspace)
        .bind(&p.suggested_key)
        .fetch_optional(&state.db)
        .await?;
        let imported: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM import_sources s JOIN issues i ON i.id = s.target_id WHERE s.workspace_id = $1 AND s.origin = $2 AND s.kind = 'issue' AND s.source_id = ANY($3)",
        )
        .bind(workspace)
        .bind(&m.origin)
        .bind(&ids)
        .fetch_one(&state.db)
        .await?;
        let mut former: Vec<String> = p.former_keys.clone();
        former.retain(|k| !k.eq_ignore_ascii_case(&p.key));
        former.sort();
        former.dedup();
        let attachments = issues.iter().flat_map(|i| i.attachments.iter());
        projects.push(ProjectSummary {
            key: p.key.clone(),
            suggested_key: p.suggested_key.clone(),
            name: p.name.clone(),
            former_keys: former,
            parent: p.parent.clone(),
            issues: ids.len() as u64,
            comments: issues.iter().map(|i| i.comments.len() as u64).sum(),
            worklogs: issues.iter().map(|i| i.worklogs.len() as u64).sum(),
            attachments: issues.iter().map(|i| i.attachments.len() as u64).sum(),
            missing_files: attachments.filter(|a| !files.has(a)).count() as u64,
            links: m.links.iter().filter(|l| idset.contains(l.source.as_str()) || idset.contains(l.target.as_str())).count() as u64,
            sprints: sprints.len() as u64,
            history: issues.iter().map(|i| i.history.iter().filter(|g| !g.initial).count() as u64).sum(),
            confidential: issues.iter().filter(|i| i.confidential).count() as u64,
            existing: existing.map(|(key, name, issues)| ExistingProject { key, name, issues }),
            imported: imported as u64,
            valid_key: crate::routes::projects::valid_key(&p.key).is_ok(),
        });
    }
    // people the work refers to, and how many have an account here already
    let all: Vec<&model::Project> = m.projects.iter().collect();
    let mut seen = HashSet::new();
    let keys: Vec<String> = referenced_people(m, &by_project, &all).iter().map(|k| m.canonical(k)).filter(|k| seen.insert(k.clone())).collect();
    let users: Vec<(String, Option<String>)> =
        sqlx::query_as("SELECT name, email FROM users WHERE workspace_id = $1").bind(workspace).fetch_all(&state.db).await?;
    let mut people = PeopleSummary { total: keys.len() as u64, ..Default::default() };
    for k in &keys {
        let src = m.people.get(k);
        if src.is_some_and(|p| p.active) {
            people.active += 1;
        } else {
            people.inactive += 1;
        }
        let matched = src.is_some_and(|p| {
            users.iter().any(|(n, e)| {
                p.email.as_deref().is_some_and(|pe| e.as_deref().is_some_and(|e| e.eq_ignore_ascii_case(pe))) || n.trim().eq_ignore_ascii_case(p.name.trim())
            })
        });
        people.matched += u64::from(matched);
    }
    let markup =
        (m.markup != Format::Source && m.issues.iter().any(|i| i.description.as_ref().is_some_and(|d| d.format == Format::Source))).then_some(m.markup);
    Ok(Summary { source: m.source.clone(), origin: m.origin.clone(), projects, people, files: files.count(), has_sprints, markup, warnings: m.notes.clone() })
}
