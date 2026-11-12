//! What an import writes, whatever it was read from: projects, people, issues with their fields
//! and history, comments, time logs, attachments, links and parents. People, statuses, types,
//! priorities, components, versions, sprints, custom fields and issues are named by their id in
//! the source (people by one key; other names of them are aliases). Saved as JSON between the
//! check of an API source and its import.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

use super::Warning;

/// How a text is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    /// wiki markup (`h1.`, `*bold*`, `[text|url]`, `[~user]`, `!image.png!`)
    Wiki,
    #[default]
    Markdown,
    Textile,
    Plain,
    /// the source's own markup (a server setting): [`Model::markup`] unless the import says otherwise
    Source,
}

impl std::str::FromStr for Format {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "textile" => Ok(Format::Textile),
            "markdown" | "common_mark" | "commonmark" => Ok(Format::Markdown),
            "plain" | "none" | "" => Ok(Format::Plain),
            "wiki" => Ok(Format::Wiki),
            _ => Err("markup must be textile, markdown or plain".into()),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Text {
    pub body: String,
    pub format: Format,
}

impl Text {
    pub fn new(body: impl Into<String>, format: Format) -> Text {
        Text { body: body.into(), format }
    }

    /// A text with something in it.
    pub fn some(body: Option<&str>, format: Format) -> Option<Text> {
        body.filter(|b| !b.trim().is_empty()).map(|b| Text::new(b, format))
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Project {
    pub id: String,
    /// the key in the source (or its path, `owner/repo`): what choices are made by
    pub key: String,
    /// a key that works here, for a project the import creates
    pub suggested_key: String,
    pub name: String,
    pub description: Option<Text>,
    pub lead: Option<String>,
    /// keys it had before (shown in the check)
    pub former_keys: Vec<String>,
    /// scrum | kanban | tracker | business for a project the import creates (None: scrum when it
    /// has sprints, else kanban)
    pub template: Option<String>,
    /// the source project's parent (its key), when it has one
    pub parent: Option<String>,
    /// how texts refer to its issues across projects (`owner/repo`), when they do
    pub path: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Person {
    pub name: String,
    pub email: Option<String>,
    pub active: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Status {
    pub name: String,
    /// todo | in_progress | done, when the source says
    pub category: Option<String>,
    /// any status of the category will do (the first one); `name` only when there's none
    #[serde(default)]
    pub by_category: bool,
    /// the source's own workflow: matched by its exact name, and in projects the import creates
    /// it keeps the source's category (missing ones are created rather than matched loosely)
    #[serde(default)]
    pub exact: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IssueType {
    pub name: String,
    /// 1 epic, 0 standard, -1 sub-task
    pub level: i16,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Priority {
    pub name: String,
    /// lower is more urgent
    pub sequence: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Component {
    pub id: String,
    pub project: String,
    pub name: String,
    pub description: Option<Text>,
    pub lead: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Version {
    pub id: String,
    pub project: String,
    pub name: String,
    pub description: Option<Text>,
    pub sequence: f64,
    pub released: bool,
    pub archived: bool,
    pub release_date: Option<NaiveDate>,
    pub start_date: Option<NaiveDate>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Sprint {
    pub name: String,
    pub goal: Option<String>,
    pub closed: bool,
    pub started: bool,
    pub start: Option<DateTime<Utc>>,
    pub end: Option<DateTime<Utc>>,
    pub completed: Option<DateTime<Utc>>,
    pub sequence: f64,
}

/// A custom field: its values are imported into a field of the same name (made when missing).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CustomField {
    pub name: String,
    /// text | number | select | multiselect | date | user | checkbox | url
    pub kind: String,
    /// the choices of a select
    pub options: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Comment {
    pub id: String,
    pub author: Option<String>,
    pub body: Text,
    pub created: Option<DateTime<Utc>>,
    pub updated: Option<DateTime<Utc>>,
    /// visible only to some (a group, a role, private notes) in the source
    pub restricted: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Worklog {
    pub id: String,
    pub author: Option<String>,
    pub note: Option<Text>,
    pub created: Option<DateTime<Utc>>,
    pub started: Option<DateTime<Utc>>,
    /// the day of the work when the source gives a day rather than a moment
    pub date: Option<NaiveDate>,
    pub seconds: f64,
    /// what kind of work it was (the source's activity): a time category here
    #[serde(default)]
    pub category: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Attachment {
    pub id: String,
    pub name: String,
    pub mime: Option<String>,
    /// bytes, when the source says
    pub size: Option<u64>,
    pub author: Option<String>,
    pub created: Option<DateTime<Utc>>,
    /// where its file is downloaded from (API sources)
    pub url: Option<String>,
    /// where else, when it isn't there
    #[serde(default)]
    pub fallback_url: Option<String>,
    /// how texts point at it (its file name, the URL in Markdown): those places point to the
    /// imported file
    pub refs: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Link {
    /// blocks (source blocks target) | relates | duplicates | clones
    pub kind: String,
    /// the source's link type was understood (else it's imported as relates, with a warning)
    pub known: bool,
    /// the source's name of the link type
    pub type_name: String,
    pub source: String,
    pub target: String,
    pub created: Option<DateTime<Utc>>,
}

/// A field whose changes are kept in the history.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Field {
    /// values: status ids
    Status,
    /// values: person keys (names: display names)
    Assignee,
    Reporter,
    /// values: priority ids
    Priority,
    /// values: type ids
    Type,
    /// names: the titles
    Title,
    Description,
    /// values: days (`2026-11-12`)
    Due,
    /// names: the labels, separated by spaces
    Labels,
    /// values: seconds
    Estimate,
    Remaining,
    /// values: component ids
    Component,
    /// values: version ids
    Version,
    /// values: issue ids (names: keys)
    Parent,
    /// values: sprint ids, separated by commas (names: their names)
    Sprint,
    /// names: numbers
    Points,
    /// not a field: the change came with logged work
    Worklog,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Item {
    pub field: Field,
    pub old: Option<String>,
    pub old_name: Option<String>,
    pub new: Option<String>,
    pub new_name: Option<String>,
}

impl Item {
    pub fn new(field: Field, old: Option<String>, new: Option<String>) -> Item {
        Item { field, old_name: old.clone(), old, new_name: new.clone(), new }
    }
}

/// Changes made at once by one person.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Change {
    pub at: Option<DateTime<Utc>>,
    pub author: Option<String>,
    /// the values the issue was created with (not a change)
    pub initial: bool,
    pub items: Vec<Item>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Issue {
    pub id: String,
    pub project: String,
    /// its number in the source (kept when it's free; 0: none)
    pub number: i32,
    /// keys it had (they keep working)
    pub keys: Vec<String>,
    pub type_id: String,
    pub status: String,
    pub priority: Option<String>,
    /// it has a resolution in the source: its name (empty when unknown)
    pub resolution: Option<String>,
    pub resolved: Option<DateTime<Utc>>,
    pub title: String,
    pub description: Option<Text>,
    pub reporter: Option<String>,
    pub assignee: Option<String>,
    pub creator: Option<String>,
    pub created: Option<DateTime<Utc>>,
    pub updated: Option<DateTime<Utc>>,
    pub start: Option<NaiveDate>,
    pub due: Option<NaiveDate>,
    /// seconds
    pub original_estimate: Option<f64>,
    pub remaining_estimate: Option<f64>,
    pub points: Option<f64>,
    /// the sprints it was in (the latest one is where it is)
    pub sprints: Vec<String>,
    /// the first one counts
    pub components: Vec<String>,
    /// fix versions; the first one counts
    pub versions: Vec<String>,
    pub labels: Vec<String>,
    /// person keys
    pub watchers: Vec<String>,
    /// custom field id → values (person keys for user fields)
    pub custom: Vec<(String, Vec<String>)>,
    /// order in the backlog (sorts like text)
    pub rank: Option<String>,
    /// only some could see it in the source
    pub confidential: bool,
    /// the source's name for who could see it (a security level); confidential issues without one
    /// get one called Confidential
    #[serde(default)]
    pub security: Option<String>,
    pub comments: Vec<Comment>,
    pub worklogs: Vec<Worklog>,
    pub attachments: Vec<Attachment>,
    /// oldest first
    pub history: Vec<Change>,
}

/// Everything an import writes.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Model {
    /// where it comes from (`import_sources.origin`): the same origin finds earlier imports again
    pub origin: String,
    /// xml-site-backup | github | gitlab | redmine
    pub source: String,
    pub projects: Vec<Project>,
    /// by person key
    pub people: HashMap<String, Person>,
    /// other names of people (user keys, logins, account ids) → person key
    pub aliases: HashMap<String, String>,
    pub statuses: HashMap<String, Status>,
    pub types: HashMap<String, IssueType>,
    pub priorities: HashMap<String, Priority>,
    /// the source's resolutions in order (empty: the project's own)
    pub resolutions: Vec<String>,
    pub components: Vec<Component>,
    pub versions: Vec<Version>,
    pub sprints: HashMap<String, Sprint>,
    pub fields: HashMap<String, CustomField>,
    pub issues: Vec<Issue>,
    pub links: Vec<Link>,
    /// child → parent, most specific first (the first one per child counts)
    pub parents: Vec<(String, String)>,
    /// how texts in the source's own markup are read
    pub markup: Format,
    /// `#12` in texts names the issue with the source id 12 (else the number 12 of the text's project)
    pub global_refs: bool,
    /// about the whole source: added to the report of every import
    pub warnings: Vec<Warning>,
    /// for the check only
    pub notes: Vec<Warning>,
}

impl Model {
    /// The key of the person behind a name the source uses (a user key, account id, login).
    pub fn canonical(&self, key: &str) -> String {
        let key = key.trim();
        if let Some(k) = self.aliases.get(key) {
            return k.clone();
        }
        let lower = key.to_lowercase();
        if self.people.contains_key(key) {
            key.to_string()
        } else if self.people.contains_key(&lower) {
            lower
        } else if let Some(k) = self.aliases.get(&lower) {
            k.clone()
        } else {
            key.to_string()
        }
    }

    /// The person behind a name, when the source lists them.
    pub fn person(&self, key: &str) -> Option<&Person> {
        self.people.get(&self.canonical(key))
    }
}

/// Ids are often numbers: order them as numbers.
pub fn id_order(a: &str, b: &str) -> std::cmp::Ordering {
    (a.len(), a).cmp(&(b.len(), b))
}

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// The file of an attachment.
#[derive(Debug)]
pub struct File {
    pub data: Vec<u8>,
    /// its type, when the source says
    pub mime: Option<String>,
    /// its name, when the source says (attachments known by an address only)
    pub name: Option<String>,
}

/// A file larger than the upload limit.
#[derive(Debug)]
pub struct TooLarge;

impl std::fmt::Display for TooLarge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("larger than the upload limit")
    }
}

impl std::error::Error for TooLarge {}

/// Where the files of attachments come from.
pub trait Files: Send + Sync {
    /// Whether the source holds the attachment's file.
    fn has(&self, a: &Attachment) -> bool;
    /// How many files the source holds.
    fn count(&self) -> u64;
    /// The size of the file without reading it (dry runs).
    fn size(&mut self, a: &Attachment) -> Option<u64>;
    /// The file: `Ok(None)` when it's missing, an error when it's larger than `max` bytes or
    /// can't be read.
    fn read<'a>(&'a mut self, a: &'a Attachment, max: u64) -> BoxFuture<'a, anyhow::Result<Option<File>>>;
}

/// No files at all.
pub struct NoFiles;

impl Files for NoFiles {
    fn has(&self, _: &Attachment) -> bool {
        false
    }
    fn count(&self) -> u64 {
        0
    }
    fn size(&mut self, _: &Attachment) -> Option<u64> {
        None
    }
    fn read<'a>(&'a mut self, _: &'a Attachment, _: u64) -> BoxFuture<'a, anyhow::Result<Option<File>>> {
        Box::pin(async { Ok(None) })
    }
}
