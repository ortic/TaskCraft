//! Migration from other trackers: their data read into one source-neutral [`model::Model`]
//! (people, projects, issues with their fields, history, comments, time logs, attachments, links,
//! …) and written here by one engine. Producers: an XML site backup ([`crate::site_backup`]) and
//! the REST APIs of GitHub ([`github`]), GitLab ([`gitlab`]) and Redmine ([`redmine`]), connected
//! to with [`api`] over [`http`].
//!
//! Everything lands in one transaction: an import either happens completely or not at all, a dry
//! run is the same import rolled back (without storing files), and rows that came from a source
//! are remembered in `import_sources`, so importing the same source again skips them. Imports
//! tell nobody: no e-mails, notifications, webhooks or automation.

pub mod api;
mod engine;
pub mod github;
pub mod gitlab;
pub mod http;
pub mod jobs;
pub mod model;
pub mod redmine;
pub mod taskcraft;
pub(crate) mod text;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub use engine::{run, summarize};
pub use model::{Files, Format, Model};

/// What happens to people of the source who have no account here yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum People {
    /// accounts for them, invited (people still active in the source) or deactivated; nobody is
    /// e-mailed, invitations are sent from the people settings when it's time
    #[default]
    Invite,
    /// accounts for them, all deactivated (their names stay on their work)
    Deactivated,
    /// no accounts: their work is imported without a person
    Skip,
}

impl std::str::FromStr for People {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "invite" => Ok(People::Invite),
            "deactivated" | "deactivate" => Ok(People::Deactivated),
            "skip" => Ok(People::Skip),
            _ => Err("people must be invite, deactivated or skip".into()),
        }
    }
}

/// What to do with one project of the source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "action")]
pub enum Choice {
    /// into the project with this key: an existing one, else a new one
    Auto {
        key: String,
    },
    /// a new project with this key (refused when the key is taken)
    New {
        key: String,
    },
    /// into this existing project
    Into {
        key: String,
    },
    Skip,
}

#[derive(Debug, Clone)]
pub struct Options {
    /// check everything, change nothing (no files stored either)
    pub dry_run: bool,
    pub people: People,
    /// per source project key (upper case); projects not named here go to their suggested key
    pub choices: BTreeMap<String, Choice>,
    /// only these source projects (upper-case keys; empty: all)
    pub only: Vec<String>,
    /// the template of projects the import creates (default: from the source project)
    pub template: Option<String>,
    /// the time zone the source's local times are written in (time logs land on its days)
    pub time_zone: chrono_tz::Tz,
    /// how texts in the source's own markup are read (default: what the source says or the check found)
    pub markup: Option<Format>,
}

impl Default for Options {
    fn default() -> Self {
        Options { dry_run: false, people: People::default(), choices: BTreeMap::new(), only: vec![], template: None, time_zone: chrono_tz::UTC, markup: None }
    }
}

/// How far an import is.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    /// reading | fetching | waiting | people | issues | links | saving
    pub phase: String,
    /// the project being read or imported (its key)
    pub project: Option<String>,
    pub done: u64,
    pub total: u64,
    /// while waiting for a rate limit: until when
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<DateTime<Utc>>,
}

pub type ProgressHandle = Arc<Mutex<Progress>>;

pub(crate) fn set_progress(p: &ProgressHandle, phase: &str, project: Option<&str>, done: u64, total: u64) {
    if let Ok(mut g) = p.lock() {
        *g = Progress { phase: phase.into(), project: project.map(str::to_string), done, total, until: None };
    }
}

/// Imported (or, in a dry run, importable) and already there from an earlier import.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Count {
    pub imported: u64,
    pub existing: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectReport {
    /// the key in the source
    pub source: String,
    pub key: String,
    pub name: String,
    pub id: Option<Uuid>,
    /// made by this import (else imported into an existing project)
    pub created: bool,
    pub skipped: bool,
    /// the source project's parent (its key in the source), when it had one
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// issues, comments, worklogs, attachments, links, watchers, labels, history, components,
    /// versions, sprints, statuses, types, fields, confidential
    pub counts: BTreeMap<String, Count>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Warning {
    /// what happened (`missing-file`, `link-type`, …), for the web app to word it
    pub code: String,
    /// in English
    pub message: String,
    pub params: BTreeMap<String, String>,
    /// how often
    pub count: u64,
}

impl Warning {
    pub fn new(code: &str, message: &str) -> Warning {
        Warning { code: code.into(), message: message.into(), params: BTreeMap::new(), count: 1 }
    }

    pub fn with(mut self, k: &str, v: impl Into<String>) -> Warning {
        let v = v.into();
        self.message = self.message.replace(&format!("{{{k}}}"), &v);
        self.params.insert(k.into(), v);
        self
    }

    pub fn times(mut self, n: u64) -> Warning {
        self.count = n;
        self
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    pub dry_run: bool,
    /// the source site's id (or a fingerprint of its projects)
    pub origin: String,
    /// matched, invited, deactivated, skipped
    pub people: BTreeMap<String, u64>,
    pub projects: Vec<ProjectReport>,
    pub warnings: Vec<Warning>,
}

impl Report {
    pub fn total(&self, what: &str) -> Count {
        self.projects
            .iter()
            .filter_map(|p| p.counts.get(what))
            .fold(Count::default(), |a, c| Count { imported: a.imported + c.imported, existing: a.existing + c.existing })
    }
}

impl std::fmt::Display for Report {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "{}", if self.dry_run { "Dry run: nothing was changed. The import would bring in:" } else { "Imported:" })?;
        let p = |k: &str| self.people.get(k).copied().unwrap_or(0);
        writeln!(
            f,
            "  people: {} matched to accounts here, {} invited, {} deactivated, {} left out",
            p("matched"),
            p("invited"),
            p("deactivated"),
            p("skipped")
        )?;
        for pr in &self.projects {
            if pr.skipped {
                writeln!(f, "\n{} → skipped", pr.source)?;
                continue;
            }
            let how = if pr.created { "new project" } else { "existing project" };
            writeln!(f, "\n{} → {} “{}” ({how})", pr.source, pr.key, pr.name)?;
            if let Some(parent) = &pr.parent {
                writeln!(f, "  a subproject of {parent} in the source")?;
            }
            for (what, c) in &pr.counts {
                if c.imported == 0 && c.existing == 0 {
                    continue;
                }
                if c.existing > 0 {
                    writeln!(f, "  {what:<12} {:>7}   ({} already there)", c.imported, c.existing)?;
                } else {
                    writeln!(f, "  {what:<12} {:>7}", c.imported)?;
                }
            }
        }
        if !self.warnings.is_empty() {
            writeln!(f, "\nWarnings:")?;
            for w in &self.warnings {
                if w.count > 1 {
                    writeln!(f, "  - {} (×{})", w.message, w.count)?;
                } else {
                    writeln!(f, "  - {}", w.message)?;
                }
            }
        }
        Ok(())
    }
}

/// What a source holds, for the check before an import.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Summary {
    /// xml-site-backup | github | gitlab | redmine
    #[serde(default)]
    pub source: String,
    pub origin: String,
    pub projects: Vec<ProjectSummary>,
    pub people: PeopleSummary,
    /// the attachment files the source holds
    pub files: u64,
    pub has_sprints: bool,
    /// how texts are read (textile | markdown), for sources whose markup is a server setting
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub markup: Option<Format>,
    pub warnings: Vec<Warning>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectSummary {
    pub key: String,
    /// a key that works here, proposed for a new project
    #[serde(default)]
    pub suggested_key: String,
    pub name: String,
    pub former_keys: Vec<String>,
    /// the source project's parent (its key), when it has one
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    pub issues: u64,
    pub comments: u64,
    pub worklogs: u64,
    pub attachments: u64,
    /// attachments without their file in the source
    pub missing_files: u64,
    pub links: u64,
    pub sprints: u64,
    pub history: u64,
    /// issues only some people could see in the source
    #[serde(default)]
    pub confidential: u64,
    /// a project with the same key here
    pub existing: Option<ExistingProject>,
    /// issues an earlier import of this source brought in already
    pub imported: u64,
    /// whether the key works here (2–10 letters or digits, starting with a letter)
    pub valid_key: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExistingProject {
    pub key: String,
    pub name: String,
    pub issues: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PeopleSummary {
    /// people the projects' work refers to
    pub total: u64,
    /// of them, people with an account here (same e-mail address or name)
    pub matched: u64,
    pub active: u64,
    pub inactive: u64,
}

/// A project of an API source to choose from.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Available {
    /// how it's asked for (`owner/repo`, `group/project`, an identifier)
    pub key: String,
    pub name: String,
    pub description: Option<String>,
    /// open issues, when the source says
    pub issues: Option<u64>,
    pub private: bool,
    /// the project it belongs to (subprojects)
    pub parent: Option<String>,
}

/// A project key that works here made from a source's name (`my-cool-repo` → `MYCOOLREPO`).
pub fn key_from(name: &str) -> String {
    let base = name.rsplit('/').next().unwrap_or(name);
    let mut k: String = base.chars().filter(|c| c.is_ascii_alphanumeric()).map(|c| c.to_ascii_uppercase()).collect();
    while k.starts_with(|c: char| c.is_ascii_digit()) {
        k.remove(0);
    }
    if k.len() < 2 {
        k = format!("P{k}");
    }
    if k.len() < 2 {
        k.push('X');
    }
    k.truncate(10);
    k
}

/// Makes the suggested keys of a source's projects distinct (`WEB`, `WEB2`, …).
pub fn distinct_keys(projects: &mut [model::Project]) {
    let mut seen = std::collections::HashSet::new();
    for p in projects.iter_mut() {
        let base = p.suggested_key.clone();
        let mut k = base.clone();
        let mut n = 2;
        while !seen.insert(k.clone()) {
            let suffix = n.to_string();
            k = format!("{}{suffix}", base.chars().take(10 - suffix.len()).collect::<String>());
            n += 1;
        }
        p.suggested_key = k;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_from_names() {
        assert_eq!(key_from("octo/my-cool-repo"), "MYCOOLREPO");
        assert_eq!(key_from("hello-world-of-wonders"), "HELLOWORLD");
        assert_eq!(key_from("42things"), "THINGS");
        assert_eq!(key_from("x"), "PX");
        assert_eq!(key_from("---"), "PX");
        assert!(crate::routes::projects::valid_key(&key_from("ab")).is_ok());
    }
}
