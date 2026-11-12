//! Reading an XML site backup: `entities.xml` holds one element per row (`<Issue id="…"
//! summary="…">`, longer values as child elements such as `<description>…</description>`), and
//! `activeobjects.xml` holds the tables of add-ons (sprints and ranks among them) as
//! `<data tableName="…">` with `<column>`s and `<row>`s. Both are read as a stream, keeping only
//! the rows an import uses.

use std::collections::HashMap;
use std::io::BufRead;

use anyhow::Context;
use chrono::{DateTime, NaiveDate, NaiveDateTime, TimeZone, Utc};
use chrono_tz::Tz;
use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};

/// One row: its fields by name (attributes and child elements alike).
#[derive(Debug, Default)]
pub struct Rec(Vec<(String, String)>);

impl Rec {
    pub fn get(&self, k: &str) -> Option<&str> {
        self.0.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str())
    }

    /// A field with something in it.
    fn text(&self, k: &str) -> Option<String> {
        self.get(k).map(str::trim).filter(|v| !v.is_empty()).map(str::to_string)
    }

    fn id(&self, k: &str) -> String {
        self.get(k).unwrap_or("").trim().to_string()
    }

    fn num(&self, k: &str) -> Option<f64> {
        self.get(k).and_then(|v| v.trim().parse::<f64>().ok()).filter(|n| n.is_finite())
    }

    fn flag(&self, k: &str) -> bool {
        matches!(self.get(k).map(|v| v.trim().to_ascii_lowercase()).as_deref(), Some("true" | "1" | "y" | "yes"))
    }
}

fn tag_name(e: &BytesStart) -> String {
    String::from_utf8_lossy(e.local_name().as_ref()).into_owned()
}

fn attrs(e: &BytesStart, rec: &mut Rec) {
    for a in e.attributes().with_checks(false).flatten() {
        let k = String::from_utf8_lossy(a.key.local_name().as_ref()).into_owned();
        let v =
            a.normalized_value(quick_xml::XmlVersion::Implicit1_0).map(|v| v.into_owned()).unwrap_or_else(|_| String::from_utf8_lossy(&a.value).into_owned());
        rec.0.push((k, v));
    }
}

/// The text of everything up to the end of the current element.
fn text_until_end<R: BufRead>(reader: &mut Reader<R>, buf: &mut Vec<u8>) -> anyhow::Result<String> {
    let mut out = String::new();
    let mut depth = 0usize;
    loop {
        buf.clear();
        match reader.read_event_into(buf)? {
            Event::Text(t) => out.push_str(&t.xml10_content().map(|c| c.into_owned()).unwrap_or_else(|_| String::from_utf8_lossy(&t).into_owned())),
            Event::CData(t) => out.push_str(&String::from_utf8_lossy(&t)),
            Event::GeneralRef(r) => match r.resolve_char_ref() {
                Ok(Some(c)) => out.push(c),
                _ => {
                    let name = String::from_utf8_lossy(&r).into_owned();
                    match quick_xml::escape::resolve_predefined_entity(&name) {
                        Some(s) => out.push_str(s),
                        None => out.push_str(&format!("&{name};")),
                    }
                }
            },
            Event::Start(_) => depth += 1,
            Event::End(_) if depth == 0 => return Ok(out),
            Event::End(_) => depth -= 1,
            Event::Eof => anyhow::bail!("the XML ends in the middle of an element"),
            _ => {}
        }
    }
}

/// Calls `f` with every row of `entities.xml` whose kind `wanted` accepts.
pub fn each_entity<R: BufRead>(r: R, wanted: impl Fn(&str) -> bool, mut f: impl FnMut(&str, Rec)) -> anyhow::Result<()> {
    let mut reader = Reader::from_reader(r);
    let mut buf = Vec::new();
    let mut inner = Vec::new();
    let mut depth = 0usize;
    loop {
        buf.clear();
        let ev = reader.read_event_into(&mut buf).with_context(|| format!("entities.xml is not valid XML (at byte {})", reader.buffer_position()))?;
        match ev {
            Event::Start(e) if depth == 1 => {
                let kind = tag_name(&e);
                if !wanted(&kind) {
                    inner.clear();
                    reader.read_to_end_into(e.name(), &mut inner)?;
                    continue;
                }
                let mut rec = Rec::default();
                attrs(&e, &mut rec);
                // fields as child elements
                loop {
                    inner.clear();
                    match reader.read_event_into(&mut inner)? {
                        Event::Start(c) => {
                            let name = tag_name(&c);
                            let mut tbuf = Vec::new();
                            let v = text_until_end(&mut reader, &mut tbuf)?;
                            rec.0.push((name, v));
                        }
                        Event::Empty(c) => rec.0.push((tag_name(&c), String::new())),
                        Event::End(_) => break,
                        Event::Eof => anyhow::bail!("entities.xml ends in the middle of a {kind}"),
                        _ => {}
                    }
                }
                f(&kind, rec);
            }
            Event::Empty(e) if depth == 1 => {
                let kind = tag_name(&e);
                if wanted(&kind) {
                    let mut rec = Rec::default();
                    attrs(&e, &mut rec);
                    f(&kind, rec);
                }
            }
            Event::Start(_) => depth += 1,
            Event::End(_) => depth = depth.saturating_sub(1),
            Event::Eof => return Ok(()),
            _ => {}
        }
    }
}

/// Calls `f` with every row of the `activeobjects.xml` tables `wanted` accepts (table name,
/// columns → values; empty values left out).
pub fn each_table_row<R: BufRead>(r: R, wanted: impl Fn(&str) -> bool, mut f: impl FnMut(&str, Rec)) -> anyhow::Result<()> {
    let mut reader = Reader::from_reader(r);
    let mut buf = Vec::new();
    let mut tbuf = Vec::new();
    let mut table: Option<String> = None;
    let mut columns: Vec<String> = Vec::new();
    let mut row: Option<(Rec, usize)> = None;
    loop {
        buf.clear();
        let ev = reader.read_event_into(&mut buf).with_context(|| format!("activeobjects.xml is not valid XML (at byte {})", reader.buffer_position()))?;
        match ev {
            Event::Start(e) if tag_name(&e) == "data" => {
                let mut rec = Rec::default();
                attrs(&e, &mut rec);
                let name = rec.id("tableName");
                columns.clear();
                if wanted(&name) {
                    table = Some(name);
                } else {
                    tbuf.clear();
                    reader.read_to_end_into(e.name(), &mut tbuf)?;
                    table = None;
                }
            }
            Event::Empty(e) if table.is_some() && row.is_none() && tag_name(&e) == "column" => {
                let mut rec = Rec::default();
                attrs(&e, &mut rec);
                columns.push(rec.id("name"));
            }
            Event::Start(e) if table.is_some() && tag_name(&e) == "row" => row = Some((Rec::default(), 0)),
            Event::End(e) if e.local_name().as_ref() == b"row" => {
                if let (Some(t), Some((rec, _))) = (&table, row.take()) {
                    f(t, rec);
                }
            }
            Event::End(e) if e.local_name().as_ref() == b"data" => table = None,
            Event::Start(_) if row.is_some() => {
                let v = text_until_end(&mut reader, &mut tbuf)?;
                if let Some((rec, i)) = row.as_mut() {
                    if let Some(c) = columns.get(*i).filter(|_| !v.is_empty()) {
                        rec.0.push((c.clone(), v));
                    }
                    *i += 1;
                }
            }
            Event::Empty(_) if row.is_some() => {
                if let Some((_, i)) = row.as_mut() {
                    *i += 1;
                }
            }
            Event::Eof => return Ok(()),
            _ => {}
        }
    }
}

/// A moment as the backup writes it (`2026-11-12 08:30:00.0`, ISO 8601, or milliseconds since
/// 1970), local times read in `tz`.
pub fn parse_time(s: &str, tz: Tz) -> Option<DateTime<Utc>> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if s.len() >= 10 && s.bytes().all(|b| b.is_ascii_digit()) {
        return s.parse::<i64>().ok().and_then(DateTime::from_timestamp_millis);
    }
    if let Ok(t) = DateTime::parse_from_rfc3339(s) {
        return Some(t.with_timezone(&Utc));
    }
    let naive = ["%Y-%m-%d %H:%M:%S%.f", "%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%d %H:%M"]
        .iter()
        .find_map(|f| NaiveDateTime::parse_from_str(s, f).ok())
        .or_else(|| NaiveDate::parse_from_str(s.get(..10).unwrap_or(s), "%Y-%m-%d").ok().and_then(|d| d.and_hms_opt(0, 0, 0)))?;
    tz.from_local_datetime(&naive).earliest().map(|t| t.with_timezone(&Utc))
}

/// The calendar day of a stored date (due dates, release dates: midnight local time).
pub fn parse_day(s: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(s.trim().get(..10)?, "%Y-%m-%d").ok()
}

#[derive(Debug, Clone, Default)]
pub struct Project {
    pub id: String,
    pub key: String,
    pub name: String,
    pub description: Option<String>,
    pub lead: Option<String>,
    pub original_key: Option<String>,
    /// software | business | service_desk
    pub kind: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Issue {
    pub id: String,
    pub project: String,
    pub number: i32,
    /// the key the backup gives the issue (`key`, or `projectKey` with the number)
    pub keys: Vec<String>,
    pub type_id: String,
    pub status: String,
    pub priority: Option<String>,
    pub resolution: Option<String>,
    pub resolved: Option<DateTime<Utc>>,
    pub summary: String,
    pub description: Option<String>,
    pub reporter: Option<String>,
    pub assignee: Option<String>,
    pub creator: Option<String>,
    pub created: Option<DateTime<Utc>>,
    pub updated: Option<DateTime<Utc>>,
    pub due: Option<NaiveDate>,
    /// seconds
    pub original_estimate: Option<f64>,
    pub remaining_estimate: Option<f64>,
    pub subtask_parent: Option<String>,
    /// its security level's id (see `security_levels`)
    pub security: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct IssueType {
    pub name: String,
    /// 1 epic, 0 standard, -1 sub-task
    pub level: i16,
}

#[derive(Debug, Clone, Default)]
pub struct Status {
    pub name: String,
    /// todo | in_progress | done, when the backup says
    pub category: Option<&'static str>,
}

#[derive(Debug, Clone, Default)]
pub struct Priority {
    pub name: String,
    pub sequence: f64,
}

#[derive(Debug, Clone, Default)]
pub struct Person {
    pub name: String,
    pub email: Option<String>,
    pub active: bool,
}

#[derive(Debug, Clone, Default)]
pub struct Comment {
    pub id: String,
    pub issue: String,
    pub author: Option<String>,
    pub body: String,
    pub created: Option<DateTime<Utc>>,
    pub updated: Option<DateTime<Utc>>,
    /// visible only to a group or role in the source
    pub restricted: bool,
}

#[derive(Debug, Clone, Default)]
pub struct Worklog {
    pub id: String,
    pub issue: String,
    pub author: Option<String>,
    pub body: Option<String>,
    pub created: Option<DateTime<Utc>>,
    pub started: Option<DateTime<Utc>>,
    pub seconds: f64,
}

#[derive(Debug, Clone, Default)]
pub struct Attachment {
    pub id: String,
    pub issue: String,
    pub name: String,
    pub mime: Option<String>,
    pub size: Option<f64>,
    pub author: Option<String>,
    pub created: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Default)]
pub struct Link {
    pub kind: String,
    pub source: String,
    pub target: String,
    pub created: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Default)]
pub struct LinkType {
    pub name: String,
    pub inward: String,
    pub outward: String,
    pub style: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Component {
    pub id: String,
    pub project: String,
    pub name: String,
    pub description: Option<String>,
    pub lead: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Version {
    pub id: String,
    pub project: String,
    pub name: String,
    pub description: Option<String>,
    pub sequence: f64,
    pub released: bool,
    pub archived: bool,
    pub release_date: Option<NaiveDate>,
    pub start_date: Option<NaiveDate>,
}

#[derive(Debug, Clone, Default)]
pub struct Group {
    pub issue: String,
    pub author: Option<String>,
    pub created: Option<DateTime<Utc>>,
    /// the values an issue was created with (written by newer versions along with the issue)
    pub initial: bool,
}

#[derive(Debug, Clone, Default)]
pub struct Item {
    pub group: String,
    pub custom: bool,
    pub field: String,
    pub old_value: Option<String>,
    pub old_string: Option<String>,
    pub new_value: Option<String>,
    pub new_string: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct CustomField {
    pub name: String,
    pub type_key: String,
}

#[derive(Debug, Clone, Default)]
pub struct CustomValue {
    pub issue: String,
    pub field: String,
    pub string: Option<String>,
    pub number: Option<f64>,
    pub date: Option<String>,
}

#[derive(Debug, Clone, Default)]
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

/// Everything an import uses from a backup.
#[derive(Debug, Default)]
pub struct Backup {
    /// the source site's own id, when it says
    pub site: Option<String>,
    pub projects: Vec<Project>,
    /// project id → keys it had before
    pub former_keys: HashMap<String, Vec<String>>,
    pub issues: Vec<Issue>,
    pub types: HashMap<String, IssueType>,
    pub statuses: HashMap<String, Status>,
    pub priorities: HashMap<String, Priority>,
    pub resolutions: HashMap<String, String>,
    /// issue security level id → name
    pub security_levels: HashMap<String, String>,
    /// by lower-case user name
    pub people: HashMap<String, Person>,
    /// user key → lower-case user name
    pub user_keys: HashMap<String, String>,
    pub comments: Vec<Comment>,
    pub worklogs: Vec<Worklog>,
    pub attachments: Vec<Attachment>,
    pub links: Vec<Link>,
    pub link_types: HashMap<String, LinkType>,
    /// child issue → parent issue
    pub parents: Vec<(String, String)>,
    /// issue → label
    pub labels: Vec<(String, String)>,
    pub components: Vec<Component>,
    pub versions: Vec<Version>,
    /// issue → component
    pub issue_components: Vec<(String, String)>,
    /// issue → fix version
    pub fix_versions: Vec<(String, String)>,
    pub affects_versions: usize,
    /// issue → user key
    pub watchers: Vec<(String, String)>,
    pub groups: HashMap<String, Group>,
    pub items: Vec<Item>,
    pub fields: HashMap<String, CustomField>,
    pub values: Vec<CustomValue>,
    pub sprints: HashMap<String, Sprint>,
    /// issue → sprint (tables some versions keep)
    pub issue_sprints: Vec<(String, String)>,
    /// issue → rank (sorts like text)
    pub ranks: HashMap<String, String>,
    /// whether activeobjects.xml was there
    pub has_active_objects: bool,
}

const ENTITIES: &[&str] = &[
    "Project",
    "ProjectKey",
    "Issue",
    "IssueType",
    "Status",
    "Priority",
    "Resolution",
    "IssueSecurityLevel",
    "User",
    "ApplicationUser",
    "Action",
    "Worklog",
    "FileAttachment",
    "IssueLink",
    "IssueLinkType",
    "IssueParentAssociation",
    "Label",
    "Component",
    "IssueComponent",
    "Version",
    "NodeAssociation",
    "UserAssociation",
    "ChangeGroup",
    "ChangeItem",
    "CustomField",
    "CustomFieldValue",
    "OSPropertyEntry",
    "OSPropertyString",
];

fn category(id: Option<&str>) -> Option<&'static str> {
    match id.map(str::trim) {
        Some("2") => Some("todo"),
        Some("4") => Some("in_progress"),
        Some("3") => Some("done"),
        _ => None,
    }
}

impl Backup {
    /// Reads `entities.xml` (and `activeobjects.xml` when there).
    pub fn read(entities: impl BufRead, tz: Tz) -> anyhow::Result<Backup> {
        let mut b = Backup::default();
        // propertyKey of the site id entry → its id; string properties by id
        let mut site_entry: Option<String> = None;
        let mut strings: HashMap<String, String> = HashMap::new();
        // issue → project key / number from attributes, resolved once projects are known
        let t = |r: &Rec, k: &str| r.get(k).and_then(|v| parse_time(v, tz));
        each_entity(
            entities,
            |k| ENTITIES.contains(&k),
            |kind, r| match kind {
                "Project" => b.projects.push(Project {
                    id: r.id("id"),
                    key: r.id("key"),
                    name: r.text("name").unwrap_or_else(|| r.id("key")),
                    description: r.text("description"),
                    lead: r.text("lead"),
                    original_key: r.text("originalkey"),
                    kind: r.text("projecttype"),
                }),
                "ProjectKey" => {
                    if let Some(k) = r.text("projectKey") {
                        b.former_keys.entry(r.id("projectId")).or_default().push(k);
                    }
                }
                "Issue" => {
                    let mut keys = Vec::new();
                    if let Some(k) = r.text("key") {
                        keys.push(k);
                    }
                    let number = r
                        .get("number")
                        .and_then(|n| n.trim().parse::<f64>().ok())
                        .map(|n| n as i32)
                        .or_else(|| keys.first().and_then(|k| k.rsplit_once('-')).and_then(|(_, n)| n.parse().ok()))
                        .unwrap_or(0);
                    if let Some(pk) = r.text("projectKey").filter(|_| number > 0) {
                        let k = format!("{pk}-{number}");
                        if !keys.contains(&k) {
                            keys.push(k);
                        }
                    }
                    b.issues.push(Issue {
                        id: r.id("id"),
                        project: r.id("project"),
                        number,
                        keys,
                        type_id: r.id("type"),
                        status: r.id("status"),
                        priority: r.text("priority"),
                        resolution: r.text("resolution"),
                        resolved: t(&r, "resolutiondate"),
                        summary: r.text("summary").unwrap_or_default(),
                        description: r.text("description"),
                        reporter: r.text("reporter"),
                        assignee: r.text("assignee"),
                        creator: r.text("creator"),
                        created: t(&r, "created"),
                        updated: t(&r, "updated"),
                        due: r.get("duedate").and_then(parse_day),
                        original_estimate: r.num("timeoriginalestimate"),
                        remaining_estimate: r.num("timeestimate"),
                        subtask_parent: r.text("subtaskParentId"),
                        security: r.text("security").filter(|s| !s.trim().is_empty()),
                    });
                }
                "IssueType" => {
                    let style = r.id("style").to_lowercase();
                    let level = match r.get("hierarchy_level").and_then(|v| v.trim().parse::<i16>().ok()) {
                        Some(l) => l.clamp(-1, 1),
                        None if style.contains("subtask") => -1,
                        None if r.id("name").eq_ignore_ascii_case("epic") => 1,
                        None => 0,
                    };
                    b.types.insert(r.id("id"), IssueType { name: r.text("name").unwrap_or_else(|| r.id("id")), level });
                }
                "Status" => {
                    b.statuses.insert(r.id("id"), Status { name: r.text("name").unwrap_or_else(|| r.id("id")), category: category(r.get("statuscategory")) });
                }
                "Priority" => {
                    b.priorities.insert(r.id("id"), Priority { name: r.text("name").unwrap_or_default(), sequence: r.num("sequence").unwrap_or(0.0) });
                }
                "Resolution" => {
                    b.resolutions.insert(r.id("id"), r.text("name").unwrap_or_default());
                }
                "IssueSecurityLevel" => {
                    b.security_levels.insert(r.id("id"), r.text("name").unwrap_or_default());
                }
                "User" => {
                    let lower = r.text("lowerUserName").or_else(|| r.text("userName")).unwrap_or_default().to_lowercase();
                    let person = Person {
                        name: r.text("displayName").or_else(|| r.text("userName")).unwrap_or_else(|| lower.clone()),
                        email: r.text("emailAddress").filter(|e| e.contains('@')),
                        active: r.get("active").is_none_or(|a| matches!(a.trim(), "1" | "true")),
                    };
                    // history names people by their external (account) id
                    if let Some(x) = r.text("externalId") {
                        b.user_keys.entry(x).or_insert_with(|| lower.clone());
                    }
                    // the same name in several directories: the first one counts (the order of
                    // directories), unless it's inactive and a later one isn't
                    match b.people.get(&lower) {
                        Some(p) if p.active || !person.active => {}
                        _ => {
                            b.people.insert(lower, person);
                        }
                    }
                }
                "ApplicationUser" => {
                    if let (Some(k), Some(l)) = (r.text("userKey"), r.text("lowerUserName")) {
                        b.user_keys.insert(k, l.to_lowercase());
                    }
                }
                "Action" if r.get("type") == Some("comment") => b.comments.push(Comment {
                    id: r.id("id"),
                    issue: r.id("issue"),
                    author: r.text("author"),
                    body: r.get("body").unwrap_or("").to_string(),
                    created: t(&r, "created"),
                    updated: t(&r, "updated"),
                    restricted: r.text("level").is_some() || r.text("rolelevel").is_some(),
                }),
                "Worklog" => b.worklogs.push(Worklog {
                    id: r.id("id"),
                    issue: r.id("issue"),
                    author: r.text("author"),
                    body: r.text("body"),
                    created: t(&r, "created"),
                    started: t(&r, "startdate"),
                    seconds: r.num("timeworked").unwrap_or(0.0),
                }),
                "FileAttachment" => b.attachments.push(Attachment {
                    id: r.id("id"),
                    issue: r.id("issue"),
                    name: r.text("filename").unwrap_or_else(|| r.id("id")),
                    mime: r.text("mimetype"),
                    size: r.num("filesize"),
                    author: r.text("author"),
                    created: t(&r, "created"),
                }),
                "IssueLink" => b.links.push(Link { kind: r.id("linktype"), source: r.id("source"), target: r.id("destination"), created: t(&r, "created") }),
                "IssueLinkType" => {
                    b.link_types.insert(
                        r.id("id"),
                        LinkType {
                            name: r.text("linkname").unwrap_or_default(),
                            inward: r.text("inward").unwrap_or_default(),
                            outward: r.text("outward").unwrap_or_default(),
                            style: r.text("style"),
                        },
                    );
                }
                "IssueParentAssociation" => b.parents.push((r.id("issueId"), r.id("parentId"))),
                // labels of custom label fields have a field id: only the issues' own labels
                "Label" if r.text("fieldid").is_none() => {
                    if let Some(l) = r.text("label") {
                        b.labels.push((r.id("issue"), l));
                    }
                }
                "Component" => b.components.push(Component {
                    id: r.id("id"),
                    project: r.id("project"),
                    name: r.text("name").unwrap_or_else(|| r.id("id")),
                    description: r.text("description"),
                    lead: r.text("lead"),
                }),
                "IssueComponent" => b.issue_components.push((r.id("issue"), r.id("component"))),
                "Version" => b.versions.push(Version {
                    id: r.id("id"),
                    project: r.id("project"),
                    name: r.text("name").unwrap_or_else(|| r.id("id")),
                    description: r.text("description"),
                    sequence: r.num("sequence").unwrap_or(0.0),
                    released: r.flag("released"),
                    archived: r.flag("archived"),
                    release_date: r.get("releasedate").and_then(parse_day),
                    start_date: r.get("startdate").and_then(parse_day),
                }),
                "NodeAssociation" if r.get("sourceNodeEntity") == Some("Issue") => {
                    let pair = (r.id("sourceNodeId"), r.id("sinkNodeId"));
                    match r.get("associationType") {
                        Some("IssueComponent") => b.issue_components.push(pair),
                        Some("IssueFixVersion") => b.fix_versions.push(pair),
                        Some("IssueVersion") => b.affects_versions += 1,
                        _ => {}
                    }
                }
                "UserAssociation" if r.get("associationType") == Some("WatchIssue") && r.get("sinkNodeEntity").is_none_or(|e| e == "Issue") => {
                    b.watchers.push((r.id("sinkNodeId"), r.id("sourceName")))
                }
                "ChangeGroup" => {
                    b.groups.insert(
                        r.id("id"),
                        Group { issue: r.id("issue"), author: r.text("author"), created: t(&r, "created"), initial: r.flag("initialgroup") },
                    );
                }
                "ChangeItem" => b.items.push(Item {
                    group: r.id("group"),
                    custom: r.get("fieldtype").is_some_and(|f| f.eq_ignore_ascii_case("custom")),
                    field: r.id("field"),
                    old_value: r.text("oldvalue"),
                    old_string: r.text("oldstring"),
                    new_value: r.text("newvalue"),
                    new_string: r.text("newstring"),
                }),
                "CustomField" => {
                    b.fields.insert(r.id("id"), CustomField { name: r.text("name").unwrap_or_default(), type_key: r.id("customfieldtypekey") });
                }
                "CustomFieldValue" => b.values.push(CustomValue {
                    issue: r.id("issue"),
                    field: r.id("customfield"),
                    string: r.text("stringvalue").or_else(|| r.text("textvalue")),
                    number: r.num("numbervalue"),
                    date: r.text("datevalue"),
                }),
                "OSPropertyEntry" if r.get("propertyKey").is_some_and(|k| k.ends_with(".sid.key")) => site_entry = Some(r.id("id")),
                "OSPropertyString" => {
                    if let Some(v) = r.text("value").or_else(|| r.text("propertyValue")) {
                        strings.insert(r.id("id"), v);
                    }
                }
                _ => {}
            },
        )?;
        b.site = site_entry.and_then(|id| strings.remove(&id));
        Ok(b)
    }

    /// Adds sprints and ranks from `activeobjects.xml`.
    pub fn read_active_objects(&mut self, r: impl BufRead, tz: Tz) -> anyhow::Result<()> {
        self.has_active_objects = true;
        let is_sprints = |t: &str| t.starts_with("AO_") && t.ends_with("_SPRINT");
        let is_issue_sprints = |t: &str| t.starts_with("AO_") && (t.ends_with("_ISSUESPRINT") || t.ends_with("_ISSUE_SPRINT"));
        let is_ranks = |t: &str| t.starts_with("AO_") && t.ends_with("_LEXORANK");
        let t = |r: &Rec, k: &str| r.get(k).and_then(|v| parse_time(v, tz));
        let mut ranks: HashMap<String, (String, String)> = HashMap::new();
        each_table_row(
            r,
            |t| is_sprints(t) || is_issue_sprints(t) || is_ranks(t),
            |table, r| {
                if is_sprints(table) {
                    if let Some(id) = r.text("ID") {
                        self.sprints.insert(
                            id.clone(),
                            Sprint {
                                name: r.text("NAME").unwrap_or_else(|| format!("Sprint {id}")),
                                goal: r.text("GOAL"),
                                closed: r.flag("CLOSED"),
                                started: r.flag("STARTED"),
                                start: t(&r, "START_DATE").or_else(|| t(&r, "ACTIVATED_DATE")),
                                end: t(&r, "END_DATE"),
                                completed: t(&r, "COMPLETE_DATE"),
                                sequence: r.num("SEQUENCE").unwrap_or_else(|| id.parse().unwrap_or(0.0)),
                            },
                        );
                    }
                } else if is_issue_sprints(table) {
                    if let (Some(i), Some(s)) = (r.text("ISSUE_ID"), r.text("SPRINT_ID")) {
                        self.issue_sprints.push((i, s));
                    }
                } else if let (Some(i), Some(rank)) = (r.text("ISSUE_ID"), r.text("RANK")) {
                    // one rank per issue and rank field: the lowest field id is the main one
                    let field = r.text("FIELD_ID").unwrap_or_default();
                    match ranks.get(&i) {
                        Some((f, _)) if f.len() < field.len() || (f.len() == field.len() && *f <= field) => {}
                        _ => {
                            ranks.insert(i, (field, rank));
                        }
                    }
                }
            },
        )?;
        self.ranks.extend(ranks.into_iter().map(|(i, (_, r))| (i, r)));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_from_attributes_and_child_elements() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<entity-engine-xml>
    <Project id="10" key="AB" name="A &amp; B" lead="u1"/>
    <Issue id="100" project="10" number="7" summary="Fix &quot;it&quot;" created="2026-01-02 03:04:05.0">
        <description><![CDATA[h1. Title
*bold*]]></description>
    </Issue>
    <Unwanted id="1"><stuff>x</stuff></Unwanted>
    <Action id="5" issue="100" type="comment" author="u1"><body>a &lt; b&#10;next</body></Action>
</entity-engine-xml>"#;
        let b = Backup::read(xml.as_bytes(), chrono_tz::UTC).unwrap();
        assert_eq!(b.projects[0].name, "A & B");
        let i = &b.issues[0];
        assert_eq!((i.number, i.summary.as_str()), (7, "Fix \"it\""));
        assert_eq!(i.description.as_deref(), Some("h1. Title\n*bold*"));
        assert_eq!(i.created.unwrap().to_rfc3339(), "2026-01-02T03:04:05+00:00");
        assert_eq!(b.comments[0].body, "a < b\nnext");
    }

    #[test]
    fn table_rows() {
        let xml = r#"<backup xmlns="http://example.com/ao"><database/>
<data tableName="AO_1_SPRINT"><column name="CLOSED"/><column name="ID"/><column name="NAME"/><column name="START_DATE"/><column name="GOAL"/>
<row><boolean>true</boolean><integer>3</integer><string>Sprint 3</string><integer>1767225600000</integer><string xsi:nil="true"/></row></data>
<data tableName="AO_1_OTHER"><column name="ID"/><row><integer>1</integer></row></data></backup>"#;
        let mut b = Backup::default();
        b.read_active_objects(xml.as_bytes(), chrono_tz::UTC).unwrap();
        let s = &b.sprints["3"];
        assert!(s.closed && s.goal.is_none());
        assert_eq!(s.name, "Sprint 3");
        assert_eq!(s.start.unwrap().to_rfc3339(), "2026-01-01T00:00:00+00:00");
    }

    #[test]
    fn times() {
        let zurich: Tz = "Europe/Zurich".parse().unwrap();
        assert_eq!(parse_time("2026-07-01 12:00:00.0", zurich).unwrap().to_rfc3339(), "2026-07-01T10:00:00+00:00");
        assert_eq!(parse_time("2026-07-01T12:00:00Z", zurich).unwrap().to_rfc3339(), "2026-07-01T12:00:00+00:00");
        assert_eq!(parse_time("1767225600000", zurich).unwrap().to_rfc3339(), "2026-01-01T00:00:00+00:00");
        assert_eq!(parse_day("2026-07-01 00:00:00.0"), NaiveDate::from_ymd_opt(2026, 7, 1));
        assert!(parse_time("", zurich).is_none());
    }
}
