//! Descriptions and comments of a backup (wiki markup) as rich text: mentions of people point to
//! their TaskCraft accounts, and images and file links point to the imported attachments.

use std::collections::HashMap;
use std::sync::LazyLock;

use regex::{Captures, Regex};
use serde_json::{Value, json};
use uuid::Uuid;

/// Marks a mention in the text while it goes through Markdown (private-use characters).
const OPEN: char = '\u{E000}';
const CLOSE: char = '\u{E001}';

/// How names in the text are resolved.
pub trait Names {
    /// The TaskCraft person (when there is one) and the name of a source user key or name.
    fn person(&self, key: &str) -> (Option<Uuid>, Option<String>);
}

/// Wiki markup as a document. `files` maps attachment file names of the issue to their new ids.
pub fn rich(text: &str, names: &dyn Names, files: &HashMap<String, Uuid>) -> Value {
    static MENTION: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\[~(?:accountid:)?([^\]\n]+)\]").expect("pattern"));
    static IMAGE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"!([^!\s|][^!\n|]*)(?:\|[^!\n]*)?!").expect("pattern"));
    static FILE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\[\^([^\]\n|]+)\]").expect("pattern"));
    let mut people: Vec<(Uuid, String)> = Vec::new();
    let s = MENTION.replace_all(text, |c: &Captures| match names.person(c[1].trim()) {
        (Some(id), Some(name)) => {
            people.push((id, name));
            format!("{OPEN}{}{CLOSE}", people.len() - 1)
        }
        (_, Some(name)) => format!("@{name}"),
        _ => "@someone".to_string(),
    });
    let link = |name: &str| files.get(name.trim()).map(|id| format!("[{}|/api/attachments/{id}]", name.trim()));
    let s = IMAGE.replace_all(&s, |c: &Captures| link(&c[1]).unwrap_or_else(|| c[0].to_string()));
    let s = FILE.replace_all(&s, |c: &Captures| link(&c[1]).unwrap_or_else(|| c[1].trim().to_string()));
    let mut doc = crate::doc::from_markdown(&crate::markup::to_markdown(&s));
    if !people.is_empty() {
        place_mentions(&mut doc, &people, false);
    }
    doc
}

fn is_code(n: &Value) -> bool {
    n["marks"].as_array().is_some_and(|m| m.iter().any(|m| m["type"] == "code"))
}

/// Replaces the markers in text nodes with mention nodes (inside code: the name as text).
fn place_mentions(v: &mut Value, people: &[(Uuid, String)], in_code: bool) {
    let in_code = in_code || v["type"] == "codeBlock";
    let Some(content) = v.get_mut("content").and_then(Value::as_array_mut) else { return };
    let mut out = Vec::with_capacity(content.len());
    for mut n in std::mem::take(content) {
        let text = n["text"].as_str().filter(|t| n["type"] == "text" && t.contains(OPEN)).map(str::to_string);
        let Some(text) = text else {
            place_mentions(&mut n, people, in_code);
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
            if let Some((id, name)) = after[..j].parse::<usize>().ok().and_then(|k| people.get(k)) {
                if code {
                    out.push(piece(&format!("@{name}")));
                } else {
                    out.push(json!({"type": "mention", "attrs": {"id": id.to_string(), "label": name, "mentionSuggestionChar": "@"}}));
                }
            }
            rest = &after[j + CLOSE.len_utf8()..];
        }
        if !rest.is_empty() {
            out.push(piece(rest));
        }
    }
    *content = out;
}

/// A work log note: the text without wiki marks around it.
pub fn plain(text: &str) -> String {
    let doc = crate::doc::from_markdown(&crate::markup::to_markdown(text));
    crate::doc::text(&doc).chars().take(2000).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct People;
    impl Names for People {
        fn person(&self, key: &str) -> (Option<Uuid>, Option<String>) {
            match key {
                "u1" => (Some(Uuid::nil()), Some("Ada Admin".into())),
                "u2" => (None, Some("Former Person".into())),
                _ => (None, None),
            }
        }
    }

    #[test]
    fn mentions_and_files() {
        let files = HashMap::from([("shot.png".to_string(), Uuid::nil())]);
        let doc = rich("Hi [~u1] and [~accountid:u2], see !shot.png|thumbnail! and [^shot.png] or [^gone.pdf] {{[~u1]}} [~x]", &People, &files);
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
    fn notes() {
        assert_eq!(plain("*Reviewed* the [spec|https://example.com]"), "Reviewed the spec");
    }
}
