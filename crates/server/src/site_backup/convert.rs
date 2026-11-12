//! A backup's rows as the source-neutral model: custom fields with a meaning here (sprint, story
//! points, epic link, start date) become those fields, history items become changes of fields,
//! people are named by one key (their lower-case user name), and the keys issues had are
//! collected.

use std::collections::{BTreeMap, HashMap};

use sha2::{Digest, Sha256};

use super::read::{self, Backup};
use crate::importer::Warning;
use crate::importer::model::{self, Change, Field, Format, Item, Model, Text, id_order};

/// Custom fields with a meaning here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Special {
    Sprint,
    Points,
    EpicLink,
    Start,
}

fn special(f: &read::CustomField) -> Option<Special> {
    let n = f.name.trim().to_lowercase();
    let t = f.type_key.to_lowercase();
    if t.ends_with("-sprint") || n == "sprint" {
        Some(Special::Sprint)
    } else if t.contains("story-points") || n == "story points" || n == "story point estimate" {
        Some(Special::Points)
    } else if t.ends_with("epic-link") || n == "epic link" {
        Some(Special::EpicLink)
    } else if n == "start date" {
        Some(Special::Start)
    } else {
        None
    }
}

/// The site a backup comes from: its own id, else a fingerprint of its projects.
pub fn origin(b: &Backup) -> String {
    if let Some(s) = b.site.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        return format!("site:{s}");
    }
    let mut ps: Vec<String> = b.projects.iter().map(|p| format!("{}:{}", p.id, p.original_key.as_deref().unwrap_or(&p.key))).collect();
    ps.sort();
    format!("projects:{}", hex::encode(&Sha256::digest(ps.join(",").as_bytes())[..12]))
}

/// The one name of the person behind a user key, account id or user name (the key itself for
/// people the backup doesn't list).
fn canonical(b: &Backup, key: &str) -> String {
    let key = key.trim();
    if let Some(l) = b.user_keys.get(key) {
        return l.clone();
    }
    let lower = key.to_lowercase();
    if b.people.contains_key(&lower) { lower } else { key.to_string() }
}

enum LinkKind {
    /// the source is the parent of the target
    SubTask,
    Epic,
    /// blocks | duplicates | clones | relates, and whether the type was understood
    Link(&'static str, bool),
}

/// What a link of the backup becomes here, by its type's names.
fn link_kind(b: &Backup, l: &read::Link) -> LinkKind {
    let t = b.link_types.get(&l.kind);
    let name = t.map(|t| format!("{} {} {}", t.name, t.outward, t.style.as_deref().unwrap_or(""))).unwrap_or_default().to_lowercase();
    if name.contains("subtask") || name.contains("sub-task") {
        LinkKind::SubTask
    } else if name.contains("epic") {
        LinkKind::Epic
    } else if name.contains("block") {
        LinkKind::Link("blocks", true)
    } else if name.contains("duplicat") {
        LinkKind::Link("duplicates", true)
    } else if name.contains("clone") {
        LinkKind::Link("clones", true)
    } else {
        LinkKind::Link("relates", name.contains("relat"))
    }
}

/// The backup's resolution names in the order they were made (by id), without duplicates.
fn resolution_names(b: &Backup) -> Vec<String> {
    let mut list: Vec<(&String, &String)> = b.resolutions.iter().filter(|(_, n)| !n.trim().is_empty()).collect();
    list.sort_by(|a, b| id_order(a.0, b.0));
    let mut out: Vec<String> = Vec::new();
    for (_, n) in list {
        let n = n.trim().to_string();
        if !out.iter().any(|o| o.eq_ignore_ascii_case(&n)) {
            out.push(n);
        }
    }
    out.truncate(20);
    out
}

fn group_by<'a, T>(rows: impl Iterator<Item = (&'a str, T)>) -> HashMap<&'a str, Vec<T>> {
    let mut m: HashMap<&str, Vec<T>> = HashMap::new();
    for (k, v) in rows {
        m.entry(k).or_default().push(v);
    }
    m
}

fn wiki(s: Option<&str>) -> Option<Text> {
    s.map(|s| Text::new(s, Format::Wiki))
}

/// The backup as the model an import writes.
pub fn to_model(b: &Backup) -> Model {
    let canon = |k: Option<&str>| k.map(str::trim).filter(|k| !k.is_empty()).map(|k| canonical(b, k));
    let specials: HashMap<&str, Special> = b.fields.iter().filter_map(|(id, f)| special(f).map(|s| (id.as_str(), s))).collect();
    let special_names: HashMap<String, Special> = b.fields.values().filter_map(|f| special(f).map(|s| (f.name.trim().to_lowercase(), s))).collect();
    let mut other_values = 0u64;
    let mut values: HashMap<&str, Vec<(&read::CustomValue, Special)>> = HashMap::new();
    for v in &b.values {
        match specials.get(v.field.as_str()) {
            Some(s) => values.entry(v.issue.as_str()).or_default().push((v, *s)),
            None => other_values += 1,
        }
    }
    let comments = group_by(b.comments.iter().map(|c| (c.issue.as_str(), c)));
    let worklogs = group_by(b.worklogs.iter().map(|w| (w.issue.as_str(), w)));
    let attachments = group_by(b.attachments.iter().map(|a| (a.issue.as_str(), a)));
    let labels = group_by(b.labels.iter().map(|(i, l)| (i.as_str(), l.as_str())));
    let watchers = group_by(b.watchers.iter().map(|(i, u)| (i.as_str(), u.as_str())));
    let components = group_by(b.issue_components.iter().map(|(i, c)| (i.as_str(), c.as_str())));
    let fix_versions = group_by(b.fix_versions.iter().map(|(i, v)| (i.as_str(), v.as_str())));
    let affects_versions = group_by(b.affects_versions.iter().map(|(i, v)| (i.as_str(), v.as_str())));
    let issue_sprints = group_by(b.issue_sprints.iter().map(|(i, s)| (i.as_str(), s.as_str())));
    let mut groups = group_by(b.groups.iter().map(|(id, g)| (g.issue.as_str(), (id.as_str(), g))));
    for gs in groups.values_mut() {
        gs.sort_by(|a, b| a.1.created.cmp(&b.1.created).then_with(|| id_order(a.0, b.0)));
    }
    let items = group_by(b.items.iter().map(|it| (it.group.as_str(), it)));
    let projects: HashMap<&str, &read::Project> = b.projects.iter().map(|p| (p.id.as_str(), p)).collect();

    let mut m = Model { origin: origin(b), source: "xml-site-backup".into(), markup: Format::Wiki, resolutions: resolution_names(b), ..Default::default() };
    m.people = b.people.iter().map(|(k, p)| (k.clone(), model::Person { name: p.name.clone(), email: p.email.clone(), active: p.active })).collect();
    m.aliases = b.user_keys.clone();
    for p in &b.projects {
        let mut former: Vec<String> = b.former_keys.get(&p.id).cloned().unwrap_or_default();
        former.extend(p.original_key.iter().cloned());
        m.projects.push(model::Project {
            id: p.id.clone(),
            key: p.key.clone(),
            suggested_key: p.key.clone(),
            name: p.name.clone(),
            description: wiki(p.description.as_deref()),
            lead: canon(p.lead.as_deref()),
            former_keys: former,
            template: match p.kind.as_deref() {
                Some("business") => Some("business".into()),
                Some("service_desk") => Some("tracker".into()),
                _ => None,
            },
            parent: None,
            path: None,
        });
    }
    m.statuses = b
        .statuses
        .iter()
        .map(|(id, s)| (id.clone(), model::Status { name: s.name.clone(), category: s.category.map(str::to_string), by_category: false, exact: false }))
        .collect();
    m.types = b.types.iter().map(|(id, t)| (id.clone(), model::IssueType { name: t.name.clone(), level: t.level })).collect();
    m.priorities = b.priorities.iter().map(|(id, p)| (id.clone(), model::Priority { name: p.name.clone(), sequence: p.sequence })).collect();
    m.components = b
        .components
        .iter()
        .map(|c| model::Component {
            id: c.id.clone(),
            project: c.project.clone(),
            name: c.name.clone(),
            description: wiki(c.description.as_deref()),
            lead: canon(c.lead.as_deref()),
        })
        .collect();
    m.versions = b
        .versions
        .iter()
        .map(|v| model::Version {
            id: v.id.clone(),
            project: v.project.clone(),
            name: v.name.clone(),
            description: wiki(v.description.as_deref()),
            sequence: v.sequence,
            released: v.released,
            archived: v.archived,
            release_date: v.release_date,
            start_date: v.start_date,
        })
        .collect();
    m.sprints = b
        .sprints
        .iter()
        .map(|(id, s)| {
            (
                id.clone(),
                model::Sprint {
                    name: s.name.clone(),
                    goal: s.goal.clone(),
                    closed: s.closed,
                    started: s.started,
                    start: s.start,
                    end: s.end,
                    completed: s.completed,
                    sequence: s.sequence,
                },
            )
        })
        .collect();

    let value_id = |v: &read::CustomValue| v.number.map(|n| (n as i64).to_string()).or_else(|| v.string.clone());
    let mut has_sprints = false;
    for i in &b.issues {
        let iid = i.id.as_str();
        let special_values =
            |which: Special| -> Vec<&read::CustomValue> { values.get(iid).into_iter().flatten().filter(|(_, s)| *s == which).map(|(v, _)| *v).collect() };
        let mut sprints: Vec<String> = special_values(Special::Sprint).into_iter().filter_map(value_id).collect();
        sprints.extend(issue_sprints.get(iid).into_iter().flatten().map(|s| s.to_string()));
        has_sprints |= !sprints.is_empty();
        let points = special_values(Special::Points).iter().find_map(|v| v.number.or_else(|| v.string.as_deref().and_then(|s| s.trim().parse().ok())));
        let start = special_values(Special::Start).iter().find_map(|v| v.date.as_deref().or(v.string.as_deref()).and_then(read::parse_day));
        let mut comps: Vec<String> = components.get(iid).into_iter().flatten().map(|c| c.to_string()).collect();
        comps.sort_by(|a, b| id_order(a, b));
        let mut fixes: Vec<String> = fix_versions.get(iid).into_iter().flatten().map(|v| v.to_string()).collect();
        fixes.sort_by(|a, b| id_order(a, b));
        let mut affects: Vec<String> = affects_versions.get(iid).into_iter().flatten().map(|v| v.to_string()).collect();
        affects.sort_by(|a, b| id_order(a, b));
        let mut cs: Vec<&read::Comment> = comments.get(iid).cloned().unwrap_or_default();
        cs.sort_by(|a, b| a.created.cmp(&b.created).then_with(|| id_order(&a.id, &b.id)));
        let mut ws: Vec<&read::Worklog> = worklogs.get(iid).cloned().unwrap_or_default();
        ws.sort_by(|a, b| a.started.cmp(&b.started).then_with(|| id_order(&a.id, &b.id)));
        let mut atts: Vec<&read::Attachment> = attachments.get(iid).cloned().unwrap_or_default();
        atts.sort_by(|a, b| a.created.cmp(&b.created).then_with(|| id_order(&a.id, &b.id)));
        // the keys it had: its own, under the project's former keys, and from moves
        let mut keys: Vec<String> = i.keys.clone();
        let project = projects.get(i.project.as_str());
        if i.number > 0
            && let Some(p) = project
        {
            keys.push(format!("{}-{}", p.key.to_uppercase(), i.number));
            for k in b.former_keys.get(&p.id).into_iter().flatten().chain(p.original_key.iter()) {
                keys.push(format!("{}-{}", k.to_uppercase(), i.number));
            }
        }
        let mut history = Vec::new();
        for (gid, g) in groups.get(iid).into_iter().flatten() {
            let mut change = Change { at: g.created, author: canon(g.author.as_deref()), initial: g.initial, items: vec![] };
            for it in items.get(gid).into_iter().flatten() {
                if !it.custom && it.field == "Key" {
                    keys.extend(it.old_string.iter().chain(it.old_value.iter()).map(|k| k.to_uppercase()));
                    continue;
                }
                let field = if it.custom {
                    match special_names.get(&it.field.trim().to_lowercase()) {
                        Some(Special::Sprint) => Field::Sprint,
                        Some(Special::Points) => Field::Points,
                        Some(Special::EpicLink) => Field::Parent,
                        _ => continue,
                    }
                } else {
                    match it.field.to_lowercase().as_str() {
                        "status" => Field::Status,
                        "assignee" => Field::Assignee,
                        "reporter" => Field::Reporter,
                        "priority" => Field::Priority,
                        "issuetype" => Field::Type,
                        "summary" => Field::Title,
                        "description" => Field::Description,
                        "duedate" => Field::Due,
                        "labels" => Field::Labels,
                        "timeoriginalestimate" => Field::Estimate,
                        "timeestimate" => Field::Remaining,
                        "component" => Field::Component,
                        "fix version" => Field::Version,
                        "parent" => Field::Parent,
                        _ if matches!(it.field.as_str(), "WorklogId" | "timespent" | "WorklogTimeSpent") => Field::Worklog,
                        _ => continue,
                    }
                };
                let person = matches!(field, Field::Assignee | Field::Reporter);
                let v = |x: &Option<String>| if person { canon(x.as_deref()) } else { x.clone() };
                change.items.push(Item {
                    field,
                    old: v(&it.old_value),
                    old_name: it.old_string.clone(),
                    new: v(&it.new_value),
                    new_name: it.new_string.clone(),
                });
            }
            history.push(change);
        }
        m.issues.push(model::Issue {
            id: i.id.clone(),
            project: i.project.clone(),
            number: i.number,
            keys,
            type_id: i.type_id.clone(),
            status: i.status.clone(),
            priority: i.priority.clone(),
            resolution: i.resolution.as_ref().map(|r| b.resolutions.get(r).cloned().unwrap_or_default()),
            resolved: i.resolved,
            title: i.summary.clone(),
            description: wiki(i.description.as_deref()),
            reporter: canon(i.reporter.as_deref()),
            assignee: canon(i.assignee.as_deref()),
            creator: canon(i.creator.as_deref()),
            created: i.created,
            updated: i.updated,
            start,
            due: i.due,
            original_estimate: i.original_estimate,
            remaining_estimate: i.remaining_estimate,
            points,
            sprints,
            components: comps,
            versions: fixes,
            affects,
            labels: labels.get(iid).into_iter().flatten().map(|l| l.to_string()).collect(),
            watchers: watchers.get(iid).into_iter().flatten().filter_map(|u| canon(Some(u))).collect(),
            custom: vec![],
            rank: b.ranks.get(iid).cloned(),
            confidential: i.security.is_some(),
            security: i.security.as_ref().and_then(|s| b.security_levels.get(s)).filter(|n| !n.trim().is_empty()).cloned(),
            comments: cs
                .into_iter()
                .map(|c| model::Comment {
                    id: c.id.clone(),
                    author: canon(c.author.as_deref()),
                    body: Text::new(c.body.clone(), Format::Wiki),
                    created: c.created,
                    updated: c.updated,
                    restricted: c.restricted,
                })
                .collect(),
            worklogs: ws
                .into_iter()
                .map(|w| model::Worklog {
                    id: w.id.clone(),
                    author: canon(w.author.as_deref()),
                    note: wiki(w.body.as_deref()),
                    created: w.created,
                    started: w.started,
                    date: None,
                    seconds: w.seconds,
                    category: None,
                })
                .collect(),
            attachments: atts
                .into_iter()
                .map(|a| model::Attachment {
                    id: a.id.clone(),
                    name: a.name.clone(),
                    mime: a.mime.clone(),
                    size: a.size.filter(|s| *s >= 0.0).map(|s| s as u64),
                    author: canon(a.author.as_deref()),
                    created: a.created,
                    url: None,
                    fallback_url: None,
                    refs: vec![a.name.trim().to_string()],
                })
                .collect(),
            history,
        });
    }

    // parents, most specific first: sub-task parents and links, parent associations, epic links
    for i in &b.issues {
        if let Some(pid) = &i.subtask_parent {
            m.parents.push((i.id.clone(), pid.clone()));
        }
    }
    let mut epic_links: Vec<(String, String)> = Vec::new();
    for l in &b.links {
        match link_kind(b, l) {
            LinkKind::SubTask => m.parents.push((l.target.clone(), l.source.clone())),
            LinkKind::Epic => epic_links.push((l.target.clone(), l.source.clone())),
            LinkKind::Link(kind, known) => m.links.push(model::Link {
                kind: kind.into(),
                known,
                type_name: b.link_types.get(&l.kind).map(|t| t.name.clone()).unwrap_or_default(),
                source: l.source.clone(),
                target: l.target.clone(),
                created: l.created,
            }),
        }
    }
    m.parents.extend(b.parents.iter().cloned());
    let ids: std::collections::HashSet<&str> = b.issues.iter().map(|i| i.id.as_str()).collect();
    let mut by_value: Vec<(&str, String)> = Vec::new();
    for (issue, vals) in &values {
        for (v, s) in vals {
            if *s == Special::EpicLink
                && let Some(n) = value_id(v)
                && ids.contains(n.as_str())
            {
                by_value.push((issue, n));
            }
        }
    }
    by_value.sort();
    epic_links.extend(by_value.into_iter().map(|(c, p)| (c.to_string(), p)));
    m.parents.extend(epic_links);

    if other_values > 0 {
        m.warnings.push(Warning::new("other-fields", "values of other custom fields aren't imported").times(other_values));
    }
    if !b.has_active_objects && has_sprints {
        m.notes.push(Warning {
            code: "no-sprint-dates".into(),
            message: "the backup has no activeobjects.xml: sprints come without their dates".into(),
            params: BTreeMap::new(),
            count: 1,
        });
    }
    m
}
