//! Imported descriptions and comments (wiki markup, Markdown or Textile) as rich text: mentions of
//! people point to their TaskCraft accounts, references to issues (`#12`, `owner/repo#12`) to the
//! imported issues, and images and file links to the imported attachments.

use std::collections::HashMap;
use std::sync::LazyLock;

use regex::{Captures, Regex};
use serde_json::{Value, json};
use uuid::Uuid;

use super::model::{Format, Text};

/// Marks a mention in the text while it goes through Markdown (private-use characters).
const OPEN: char = '\u{E000}';
const CLOSE: char = '\u{E001}';

/// How names in the text are resolved.
pub trait Names {
    /// The TaskCraft person (when there is one) and the name of a source user key or name.
    fn person(&self, key: &str) -> (Option<Uuid>, Option<String>);
    /// The key here of the issue a text of `project` refers to (`12`, or `owner/repo` and `12`).
    fn issue(&self, _project: &str, _path: Option<&str>, _number: &str) -> Option<String> {
        None
    }
}

/// What a marker stands for.
enum Mark {
    /// the person, their name, and the text it replaced when that should stay in code
    Person(Uuid, String, Option<String>),
    /// the issue's key here, and the text it replaced (kept in code)
    Issue(String, String),
}

fn marker(marks: &mut Vec<Mark>, m: Mark) -> String {
    marks.push(m);
    format!("{OPEN}{}{CLOSE}", marks.len() - 1)
}

/// A text as a document. `files` maps the ways texts point at attachments (file names, URLs) to
/// the imported files; `project` is the source project of the text (issue references).
pub fn rich(text: &Text, format: Format, names: &dyn Names, files: &HashMap<String, Uuid>, project: &str) -> Value {
    match format {
        Format::Wiki => wiki(&text.body, names, files),
        Format::Plain => crate::doc::from_plain(&text.body),
        Format::Textile => markdown(&crate::markup::textile_to_markdown(&text.body), names, files, project),
        Format::Markdown | Format::Source => markdown(&text.body, names, files, project),
        Format::Doc => document(&text.body, names, files, project),
    }
}

/// A TaskCraft document (a project export): mentions point to the people here, `#` references to
/// the imported issues, images and file links to the imported files.
fn document(json: &str, names: &dyn Names, files: &HashMap<String, Uuid>, project: &str) -> Value {
    let Ok(mut doc) = serde_json::from_str::<Value>(json) else { return crate::doc::from_plain(json) };
    if crate::doc::validate(&doc).is_err() {
        return crate::doc::from_plain(&crate::doc::text(&doc));
    }
    fn file(url: &str, files: &HashMap<String, Uuid>) -> Option<String> {
        files.get(url).map(|id| format!("/api/attachments/{id}"))
    }
    fn walk(n: &mut Value, names: &dyn Names, files: &HashMap<String, Uuid>, project: &str) {
        if n["type"] == "mention" {
            let id = n["attrs"]["id"].as_str().unwrap_or("").to_string();
            if n["attrs"]["mentionSuggestionChar"] == "#" {
                // KEY-12: the imported issue's key, when it came along
                if let Some((k, num)) = id.rsplit_once('-')
                    && let Some(key) = names.issue(project, Some(k), num)
                {
                    n["attrs"]["id"] = json!(key);
                    n["attrs"]["label"] = json!(key);
                }
            } else {
                match names.person(&id) {
                    (Some(p), Some(name)) => {
                        n["attrs"]["id"] = json!(p.to_string());
                        n["attrs"]["label"] = json!(name);
                    }
                    (_, name) => {
                        let label = name.or_else(|| n["attrs"]["label"].as_str().map(str::to_string)).unwrap_or_else(|| "someone".into());
                        *n = json!({"type": "text", "text": format!("@{label}")});
                        return;
                    }
                }
            }
        }
        if let Some(src) = n.pointer("/attrs/src").and_then(Value::as_str).and_then(|s| file(s, files)) {
            n["attrs"]["src"] = json!(src);
        }
        if let Some(marks) = n["marks"].as_array_mut() {
            for m in marks {
                if let Some(href) = m.pointer("/attrs/href").and_then(Value::as_str).and_then(|h| file(h, files)) {
                    m["attrs"]["href"] = json!(href);
                }
            }
        }
        if let Some(c) = n["content"].as_array_mut() {
            for child in c {
                walk(child, names, files, project);
            }
        }
    }
    walk(&mut doc, names, files, project);
    doc
}

/// Wiki markup as a document.
fn wiki(text: &str, names: &dyn Names, files: &HashMap<String, Uuid>) -> Value {
    static MENTION: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\[~(?:accountid:)?([^\]\n]+)\]").expect("pattern"));
    static IMAGE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"!([^!\s|][^!\n|]*)(?:\|[^!\n]*)?!").expect("pattern"));
    static FILE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\[\^([^\]\n|]+)\]").expect("pattern"));
    let mut marks: Vec<Mark> = Vec::new();
    let s = MENTION.replace_all(text, |c: &Captures| match names.person(c[1].trim()) {
        (Some(id), Some(name)) => marker(&mut marks, Mark::Person(id, name, None)),
        (_, Some(name)) => format!("@{name}"),
        _ => "@someone".to_string(),
    });
    let link = |name: &str| files.get(name.trim()).map(|id| format!("[{}|/api/attachments/{id}]", name.trim()));
    let s = IMAGE.replace_all(&s, |c: &Captures| link(&c[1]).unwrap_or_else(|| c[0].to_string()));
    let s = FILE.replace_all(&s, |c: &Captures| link(&c[1]).unwrap_or_else(|| c[1].trim().to_string()));
    let mut doc = crate::doc::from_markdown(&crate::markup::to_markdown(&s));
    if !marks.is_empty() {
        place_marks(&mut doc, &marks, false);
    }
    doc
}

/// Markdown (as GitHub, GitLab and others write it) as a document.
fn markdown(text: &str, names: &dyn Names, files: &HashMap<String, Uuid>, project: &str) -> Value {
    static COMMENT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)<!--.*?-->").expect("pattern"));
    static IMG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"(?i)<img\s[^>]*?src\s*=\s*["']([^"']+)["'][^>]*>"#).expect("pattern"));
    static ALT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"(?i)\salt\s*=\s*["']([^"']*)["']"#).expect("pattern"));
    static BR: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)<br\s*/?>").expect("pattern"));
    static TARGET: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\]\(\s*<?([^)\s>]+)>?").expect("pattern"));
    static MENTION: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(^|[^\w@./`\\])@([A-Za-z0-9](?:[A-Za-z0-9_.-]*[A-Za-z0-9_])?(?:\[bot\])?)").expect("pattern"));
    static ISSUE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(^|[^\w&#/`\\])(?:([A-Za-z0-9_.-]+(?:/[A-Za-z0-9_.-]+)+))?#([0-9]{1,9})\b").expect("pattern"));
    let s = COMMENT.replace_all(text, "");
    let s = IMG.replace_all(&s, |c: &Captures| {
        let alt = ALT.captures(&c[0]).map(|a| a[1].to_string()).unwrap_or_else(|| "image".into());
        format!("![{alt}]({})", &c[1])
    });
    let mut s = BR.replace_all(&s, "\n").into_owned();
    // attachments: full addresses wherever they are, file names only where links point
    let mut refs: Vec<(&String, &Uuid)> = files.iter().filter(|(k, _)| k.contains("://") || k.starts_with('/')).collect();
    refs.sort_by_key(|r| std::cmp::Reverse(r.0.len()));
    for (r, id) in refs {
        s = s.replace(r.as_str(), &format!("/api/attachments/{id}"));
    }
    let s = TARGET.replace_all(&s, |c: &Captures| {
        let target = c[1].trim();
        let name = target.strip_prefix("attachment:").unwrap_or(target);
        let decoded = name.replace("%20", " ");
        match files.get(name).or_else(|| files.get(&decoded)) {
            Some(id) => format!("](/api/attachments/{id}"),
            None => c[0].to_string(),
        }
    });
    let mut marks: Vec<Mark> = Vec::new();
    let s = MENTION.replace_all(&s, |c: &Captures| match names.person(&c[2]) {
        (Some(id), Some(name)) => {
            let original = c[0][c[1].len()..].to_string();
            format!("{}{}", &c[1], marker(&mut marks, Mark::Person(id, name, Some(original))))
        }
        _ => c[0].to_string(),
    });
    let s = ISSUE.replace_all(&s, |c: &Captures| {
        let path = c.get(2).map(|m| m.as_str());
        match names.issue(project, path, &c[3]) {
            Some(key) => {
                let original = c[0][c[1].len()..].to_string();
                format!("{}{}", &c[1], marker(&mut marks, Mark::Issue(key, original)))
            }
            None => c[0].to_string(),
        }
    });
    let mut doc = crate::doc::from_markdown(&s);
    if !marks.is_empty() {
        place_marks(&mut doc, &marks, false);
    }
    doc
}

fn is_code(n: &Value) -> bool {
    n["marks"].as_array().is_some_and(|m| m.iter().any(|m| m["type"] == "code"))
}

/// Replaces the markers in text nodes with mention nodes (inside code: the text they replaced).
fn place_marks(v: &mut Value, marks: &[Mark], in_code: bool) {
    let in_code = in_code || v["type"] == "codeBlock";
    // a marker inside a link's address (the link text is plain): put back what it replaced
    if let Some(href) = v.pointer("/attrs/href").and_then(Value::as_str).filter(|h| h.contains(OPEN)).map(str::to_string) {
        v["attrs"]["href"] = json!(unmark(&href, marks));
    }
    if let Some(ms) = v.get_mut("marks").and_then(Value::as_array_mut) {
        for m in ms {
            place_marks(m, marks, true);
        }
    }
    let Some(content) = v.get_mut("content").and_then(Value::as_array_mut) else { return };
    let mut out = Vec::with_capacity(content.len());
    for mut n in std::mem::take(content) {
        let text = n["text"].as_str().filter(|t| n["type"] == "text" && t.contains(OPEN)).map(str::to_string);
        let Some(text) = text else {
            place_marks(&mut n, marks, in_code);
            out.push(n);
            continue;
        };
        let code = in_code || is_code(&n);
        let piece = |t: &str| {
            let mut p = n.clone();
            p["text"] = json!(t);
            p
        };
        let mut rest = text.as_str();
        while let Some(i) = rest.find(OPEN) {
            if i > 0 {
                out.push(piece(&rest[..i]));
            }
            let after = &rest[i + OPEN.len_utf8()..];
            let Some(j) = after.find(CLOSE) else {
                rest = after;
                break;
            };
            match after[..j].parse::<usize>().ok().and_then(|k| marks.get(k)) {
                Some(Mark::Person(id, name, _)) if !code => {
                    out.push(json!({"type": "mention", "attrs": {"id": id.to_string(), "label": name, "mentionSuggestionChar": "@"}}))
                }
                Some(Mark::Person(_, name, original)) => out.push(piece(&original.clone().unwrap_or_else(|| format!("@{name}")))),
                Some(Mark::Issue(key, _)) if !code => out.push(json!({"type": "mention", "attrs": {"id": key, "label": key, "mentionSuggestionChar": "#"}})),
                Some(Mark::Issue(_, original)) => out.push(piece(original)),
                None => {}
            }
            rest = &after[j + CLOSE.len_utf8()..];
        }
        if !rest.is_empty() {
            out.push(piece(rest));
        }
    }
    *content = out;
}

/// A text with its markers back to what they replaced.
fn unmark(s: &str, marks: &[Mark]) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find(OPEN) {
        out.push_str(&rest[..i]);
        let after = &rest[i + OPEN.len_utf8()..];
        let Some(j) = after.find(CLOSE) else {
            rest = after;
            break;
        };
        match after[..j].parse::<usize>().ok().and_then(|k| marks.get(k)) {
            Some(Mark::Person(_, name, original)) => out.push_str(&original.clone().unwrap_or_else(|| format!("@{name}"))),
            Some(Mark::Issue(_, original)) => out.push_str(original),
            None => {}
        }
        rest = &after[j + CLOSE.len_utf8()..];
    }
    out.push_str(rest);
    out
}

/// A short note (time logs, project and release descriptions): the text without marks around it.
pub fn plain(text: &Text, format: Format) -> String {
    let md = match format {
        Format::Wiki => crate::markup::to_markdown(&text.body),
        Format::Textile => crate::markup::textile_to_markdown(&text.body),
        Format::Plain => return text.body.trim().chars().take(2000).collect(),
        Format::Markdown | Format::Source => text.body.clone(),
        Format::Doc => {
            let doc: Value = serde_json::from_str(&text.body).unwrap_or(Value::Null);
            return crate::doc::text(&doc).chars().take(2000).collect();
        }
    };
    let doc = crate::doc::from_markdown(&md);
    crate::doc::text(&doc).chars().take(2000).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct People;
    impl Names for People {
        fn person(&self, key: &str) -> (Option<Uuid>, Option<String>) {
            match key {
                "u1" | "octocat" => (Some(Uuid::nil()), Some("Ada Admin".into())),
                "u2" => (None, Some("Former Person".into())),
                _ => (None, None),
            }
        }
        fn issue(&self, project: &str, path: Option<&str>, number: &str) -> Option<String> {
            match (project, path, number) {
                ("p1", None, "12") => Some("WEB-12".into()),
                ("p1", Some("octo/other"), "3") => Some("OTH-3".into()),
                _ => None,
            }
        }
    }

    fn wiki_text(s: &str) -> Text {
        Text::new(s, Format::Wiki)
    }

    #[test]
    fn mentions_and_files() {
        let files = HashMap::from([("shot.png".to_string(), Uuid::nil())]);
        let doc = rich(
            &wiki_text("Hi [~u1] and [~accountid:u2], see !shot.png|thumbnail! and [^shot.png] or [^gone.pdf] {{[~u1]}} [~x]"),
            Format::Wiki,
            &People,
            &files,
            "p1",
        );
        let json = doc.to_string();
        assert!(json.contains(r#""type":"mention""#), "{json}");
        assert!(json.contains(r#""label":"Ada Admin""#));
        assert!(json.contains("@Former Person"));
        assert!(json.contains(&format!("/api/attachments/{}", Uuid::nil())), "{json}");
        assert!(json.contains("gone.pdf"));
        assert!(json.contains("@someone"));
        assert!(!json.contains(OPEN), "{json}");
        assert_eq!(crate::doc::mentioned_people(&doc), vec![Uuid::nil()]);
        assert!(crate::doc::text(&doc).contains("Hi @Ada Admin and @Former Person"));
    }

    #[test]
    fn markdown_with_people_issues_and_images() {
        let shot = "https://github.com/user-attachments/assets/1234-abcd";
        let files = HashMap::from([(shot.to_string(), Uuid::nil()), ("notes.txt".to_string(), Uuid::max())]);
        let md = format!(
            "<!-- template hint -->\nThanks @octocat, see #12 and octo/other#3, not #99 or `#12`.\n\n<img width=\"300\" alt=\"Screenshot\" src=\"{shot}\">\n\n[notes](notes.txt) and a@b.com, [x](https://example.com/#12)\n\n```\n@octocat #12\n```"
        );
        let doc = rich(&Text::new(md, Format::Markdown), Format::Markdown, &People, &files, "p1");
        let json = doc.to_string();
        assert!(!json.contains("template hint"), "{json}");
        assert_eq!(crate::doc::mentioned_people(&doc), vec![Uuid::nil()]);
        assert_eq!(crate::doc::referenced_issues(&doc), vec!["WEB-12".to_string(), "OTH-3".to_string()]);
        assert!(json.contains(&format!("/api/attachments/{}", Uuid::nil())), "{json}");
        assert!(json.contains(&format!("/api/attachments/{}", Uuid::max())), "{json}");
        assert!(json.contains("#99"));
        assert!(json.contains(r##""text":"#12""##), "code keeps the reference as text: {json}");
        assert!(crate::doc::text(&doc).contains("@octocat #12"), "{json}");
        assert!(json.contains("a@b.com"));
        assert!(json.contains("https://example.com/#12"), "{json}");
        assert!(!json.contains(OPEN), "{json}");
    }

    #[test]
    fn textile_through_markdown() {
        let files = HashMap::from([("err.png".to_string(), Uuid::nil())]);
        let doc = rich(&Text::new("h2. Steps\n\nSee #12 and !err.png!, *really*.", Format::Textile), Format::Textile, &People, &files, "p1");
        let json = doc.to_string();
        assert!(json.contains(r#""type":"heading""#), "{json}");
        assert_eq!(crate::doc::referenced_issues(&doc), vec!["WEB-12".to_string()]);
        assert!(json.contains(&format!("/api/attachments/{}", Uuid::nil())), "{json}");
        assert!(json.contains(r#""type":"bold""#), "{json}");
    }

    #[test]
    fn notes() {
        assert_eq!(plain(&wiki_text("*Reviewed* the [spec|https://example.com]"), Format::Wiki), "Reviewed the spec");
        assert_eq!(plain(&Text::new("**Reviewed** the spec", Format::Markdown), Format::Markdown), "Reviewed the spec");
        assert_eq!(plain(&Text::new("*Reviewed* the spec", Format::Textile), Format::Textile), "Reviewed the spec");
    }
}
