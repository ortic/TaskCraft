//! Rich text (descriptions and comments) as ProseMirror JSON, the editor's format: plain text
//! for search, people and issues it mentions, and conversion from and to Markdown for the API,
//! imports and pages written to WikiCraft.

use comrak::nodes::{ListType, NodeValue};
use comrak::{Arena, Options, parse_document};
use serde_json::{Value, json};
use uuid::Uuid;

/// An empty document.
pub fn empty() -> Value {
    json!({"type": "doc", "content": [{"type": "paragraph"}]})
}

/// Checks that `v` looks like a ProseMirror document (a `doc` node), bounded in size and depth.
pub fn validate(v: &Value) -> Result<(), String> {
    if v["type"] != "doc" {
        return Err("rich text must be a ProseMirror document (type doc)".into());
    }
    fn walk(n: &Value, depth: usize, count: &mut usize) -> Result<(), String> {
        *count += 1;
        if depth > 40 || *count > 50_000 {
            return Err("the document is too large or too deeply nested".into());
        }
        if !n["type"].is_string() {
            return Err("every node needs a type".into());
        }
        if let Some(c) = n.get("content") {
            for child in c.as_array().ok_or("content must be a list")? {
                walk(child, depth + 1, count)?;
            }
        }
        Ok(())
    }
    walk(v, 0, &mut 0)
}

/// The text of a document: blocks on their own lines.
pub fn text(v: &Value) -> String {
    fn walk(n: &Value, out: &mut String) {
        match n["type"].as_str() {
            Some("text") => out.push_str(n["text"].as_str().unwrap_or("")),
            Some("mention") => {
                let ch = n["attrs"]["mentionSuggestionChar"].as_str().unwrap_or("@");
                let label = n["attrs"]["label"].as_str().or(n["attrs"]["id"].as_str()).unwrap_or("");
                out.push_str(if ch == "#" { "" } else { ch });
                out.push_str(label);
            }
            Some("hardBreak") => out.push('\n'),
            _ => {
                let block = !matches!(n["type"].as_str(), Some("doc"));
                if let Some(c) = n["content"].as_array() {
                    for child in c {
                        walk(child, out);
                    }
                }
                if block && !out.ends_with('\n') && is_block(n) {
                    out.push('\n');
                }
            }
        }
    }
    let mut out = String::new();
    walk(v, &mut out);
    out.trim().to_string()
}

fn is_block(n: &Value) -> bool {
    matches!(
        n["type"].as_str(),
        Some("paragraph" | "heading" | "codeBlock" | "blockquote" | "listItem" | "taskItem" | "horizontalRule" | "tableRow" | "image")
    )
}

/// People mentioned with `@` (their ids).
pub fn mentioned_people(v: &Value) -> Vec<Uuid> {
    let mut out = Vec::new();
    walk_nodes(v, &mut |n| {
        if n["type"] == "mention"
            && n["attrs"]["mentionSuggestionChar"].as_str().unwrap_or("@") == "@"
            && let Some(id) = n["attrs"]["id"].as_str().and_then(|s| s.parse().ok())
            && !out.contains(&id)
        {
            out.push(id);
        }
    });
    out
}

/// Issues referenced with `#` (their keys).
pub fn referenced_issues(v: &Value) -> Vec<String> {
    let mut out = Vec::new();
    walk_nodes(v, &mut |n| {
        if n["type"] == "mention"
            && n["attrs"]["mentionSuggestionChar"] == "#"
            && let Some(key) = n["attrs"]["id"].as_str()
            && !out.iter().any(|k: &String| k == key)
        {
            out.push(key.to_string());
        }
    });
    out
}

fn walk_nodes(v: &Value, f: &mut impl FnMut(&Value)) {
    f(v);
    if let Some(c) = v["content"].as_array() {
        for child in c {
            walk_nodes(child, f);
        }
    }
}

/// Plain text as paragraphs (one per line).
pub fn from_plain(s: &str) -> Value {
    let content: Vec<Value> = s
        .lines()
        .map(|l| if l.trim().is_empty() { json!({"type": "paragraph"}) } else { json!({"type": "paragraph", "content": [{"type": "text", "text": l}]}) })
        .collect();
    if content.is_empty() { empty() } else { json!({"type": "doc", "content": content}) }
}

fn options() -> Options<'static> {
    let mut o = Options::default();
    o.extension.strikethrough = true;
    o.extension.autolink = true;
    o.extension.tasklist = true;
    o.extension.table = false;
    o
}

type Ast<'a> = comrak::nodes::Node<'a>;

/// Markdown as a document (paragraphs, headings, lists, task lists, quotes, code, links,
/// emphasis). HTML in the Markdown is kept as text, never as markup.
pub fn from_markdown(md: &str) -> Value {
    let arena = Arena::new();
    let root = parse_document(&arena, md, &options());
    let content = blocks(root);
    if content.is_empty() { empty() } else { json!({"type": "doc", "content": content}) }
}

fn blocks<'a>(n: Ast<'a>) -> Vec<Value> {
    n.children().flat_map(block).collect()
}

fn block<'a>(n: Ast<'a>) -> Vec<Value> {
    let value = n.data.borrow().value.clone();
    match value {
        NodeValue::Paragraph => {
            let inl = inlines(n, &[]);
            vec![if inl.is_empty() { json!({"type": "paragraph"}) } else { json!({"type": "paragraph", "content": inl}) }]
        }
        NodeValue::Heading(h) => {
            let inl = inlines(n, &[]);
            let mut node = json!({"type": "heading", "attrs": {"level": h.level.clamp(1, 4)}});
            if !inl.is_empty() {
                node["content"] = Value::Array(inl);
            }
            vec![node]
        }
        NodeValue::BlockQuote | NodeValue::MultilineBlockQuote(_) | NodeValue::Alert(_) => {
            let inner = blocks(n);
            vec![json!({"type": "blockquote", "content": if inner.is_empty() { vec![json!({"type": "paragraph"})] } else { inner }})]
        }
        NodeValue::List(l) => {
            let is_task = n.children().any(|c| matches!(c.data.borrow().value, NodeValue::TaskItem(_)));
            let items: Vec<Value> = n
                .children()
                .map(|item| {
                    let checked = match &item.data.borrow().value {
                        NodeValue::TaskItem(t) => t.symbol.is_some(),
                        _ => false,
                    };
                    let mut inner = blocks(item);
                    if inner.is_empty() {
                        inner.push(json!({"type": "paragraph"}));
                    }
                    if is_task {
                        json!({"type": "taskItem", "attrs": {"checked": checked}, "content": inner})
                    } else {
                        json!({"type": "listItem", "content": inner})
                    }
                })
                .collect();
            let node = if is_task {
                json!({"type": "taskList", "content": items})
            } else if l.list_type == ListType::Ordered {
                json!({"type": "orderedList", "attrs": {"start": l.start}, "content": items})
            } else {
                json!({"type": "bulletList", "content": items})
            };
            vec![node]
        }
        NodeValue::CodeBlock(cb) => {
            let lang = cb.info.split_whitespace().next().unwrap_or("").to_string();
            let text = cb.literal.trim_end_matches('\n').to_string();
            let mut node = json!({"type": "codeBlock", "attrs": {"language": if lang.is_empty() { Value::Null } else { Value::String(lang) }}});
            if !text.is_empty() {
                node["content"] = json!([{"type": "text", "text": text}]);
            }
            vec![node]
        }
        NodeValue::ThematicBreak => vec![json!({"type": "horizontalRule"})],
        NodeValue::HtmlBlock(h) => {
            let t = h.literal.trim().to_string();
            if t.is_empty() { vec![] } else { vec![json!({"type": "paragraph", "content": [{"type": "text", "text": t}]})] }
        }
        NodeValue::Item(_) | NodeValue::TaskItem(_) | NodeValue::Document => blocks(n),
        _ => {
            let inl = inlines(n, &[]);
            if inl.is_empty() { vec![] } else { vec![json!({"type": "paragraph", "content": inl})] }
        }
    }
}

fn inlines<'a>(n: Ast<'a>, marks: &[Value]) -> Vec<Value> {
    let mut out = Vec::new();
    for c in n.children() {
        inline(c, marks, &mut out);
    }
    // merge neighbouring text nodes with the same marks
    let mut merged: Vec<Value> = Vec::new();
    for v in out {
        if let Some(last) = merged.last_mut()
            && last["type"] == "text"
            && v["type"] == "text"
            && last.get("marks") == v.get("marks")
        {
            let joined = format!("{}{}", last["text"].as_str().unwrap_or(""), v["text"].as_str().unwrap_or(""));
            last["text"] = Value::String(joined);
            continue;
        }
        merged.push(v);
    }
    merged.retain(|v| v["type"] != "text" || v["text"].as_str().is_some_and(|t| !t.is_empty()));
    merged
}

fn text_node(t: &str, marks: &[Value]) -> Value {
    let mut node = json!({"type": "text", "text": t});
    if !marks.is_empty() {
        node["marks"] = Value::Array(marks.to_vec());
    }
    node
}

fn inline<'a>(n: Ast<'a>, marks: &[Value], out: &mut Vec<Value>) {
    let value = n.data.borrow().value.clone();
    let with = |m: Value| {
        let mut v = marks.to_vec();
        v.push(m);
        v
    };
    match value {
        NodeValue::Text(t) => out.push(text_node(&t, marks)),
        NodeValue::SoftBreak => out.push(text_node(" ", marks)),
        NodeValue::LineBreak => out.push(json!({"type": "hardBreak"})),
        NodeValue::Code(c) => out.push(text_node(&c.literal, &with(json!({"type": "code"})))),
        NodeValue::HtmlInline(h) => out.push(text_node(&h, marks)),
        NodeValue::Emph => out.extend(inlines(n, &with(json!({"type": "italic"})))),
        NodeValue::Strong => out.extend(inlines(n, &with(json!({"type": "bold"})))),
        NodeValue::Strikethrough => out.extend(inlines(n, &with(json!({"type": "strike"})))),
        NodeValue::Link(l) => {
            let href = l.url.trim().to_string();
            if safe_href(&href) {
                out.extend(inlines(n, &with(json!({"type": "link", "attrs": {"href": href}}))));
            } else {
                out.extend(inlines(n, marks));
            }
        }
        NodeValue::Image(l) => {
            let alt = inlines(n, &[]).iter().filter_map(|v| v["text"].as_str().map(str::to_string)).collect::<String>();
            if safe_href(&l.url) {
                out.push(text_node(if alt.is_empty() { &l.url } else { &alt }, &with(json!({"type": "link", "attrs": {"href": l.url}}))));
            }
        }
        _ => out.extend(inlines(n, marks)),
    }
}

/// Links may go to the web, mail or paths in TaskCraft (never `javascript:` and the like).
pub fn safe_href(h: &str) -> bool {
    let l = h.trim().to_ascii_lowercase();
    l.starts_with("http://") || l.starts_with("https://") || l.starts_with("mailto:") || (l.starts_with('/') && !l.starts_with("//")) || l.starts_with('#')
}

/// The document as Markdown.
pub fn to_markdown(v: &Value) -> String {
    let mut out = String::new();
    if let Some(c) = v["content"].as_array() {
        md_blocks(c, "", &mut out);
    }
    out.trim_end().to_string()
}

fn md_blocks(nodes: &[Value], indent: &str, out: &mut String) {
    for (i, n) in nodes.iter().enumerate() {
        if i > 0 {
            out.push_str(indent.trim_end());
            out.push('\n');
        }
        md_block(n, indent, out);
    }
}

fn md_block(n: &Value, indent: &str, out: &mut String) {
    let content = n["content"].as_array().map(Vec::as_slice).unwrap_or(&[]);
    match n["type"].as_str().unwrap_or("") {
        "heading" => {
            let level = n["attrs"]["level"].as_u64().unwrap_or(1).clamp(1, 6) as usize;
            out.push_str(&format!("{indent}{} {}\n", "#".repeat(level), md_inlines(content)));
        }
        "blockquote" => {
            let mut inner = String::new();
            md_blocks(content, "", &mut inner);
            for line in inner.trim_end().lines() {
                out.push_str(&format!("{indent}> {line}\n"));
            }
        }
        "bulletList" | "orderedList" | "taskList" => {
            let ordered = n["type"] == "orderedList";
            let start = n["attrs"]["start"].as_u64().unwrap_or(1);
            for (i, item) in content.iter().enumerate() {
                let marker = if ordered {
                    format!("{}. ", start + i as u64)
                } else if item["type"] == "taskItem" {
                    format!("- [{}] ", if item["attrs"]["checked"] == true { "x" } else { " " })
                } else {
                    "- ".to_string()
                };
                let mut inner = String::new();
                let child_indent = " ".repeat(marker.len());
                if let Some(c) = item["content"].as_array() {
                    md_blocks(c, "", &mut inner);
                }
                for (j, line) in inner.trim_end().lines().enumerate() {
                    if j == 0 {
                        out.push_str(&format!("{indent}{marker}{line}\n"));
                    } else if line.is_empty() {
                        out.push('\n');
                    } else {
                        out.push_str(&format!("{indent}{child_indent}{line}\n"));
                    }
                }
            }
        }
        "codeBlock" => {
            let lang = n["attrs"]["language"].as_str().unwrap_or("");
            let code: String = content.iter().filter_map(|t| t["text"].as_str()).collect();
            out.push_str(&format!("{indent}```{lang}\n"));
            for line in code.lines() {
                out.push_str(&format!("{indent}{line}\n"));
            }
            out.push_str(&format!("{indent}```\n"));
        }
        "horizontalRule" => out.push_str(&format!("{indent}---\n")),
        "image" => {
            let src = n["attrs"]["src"].as_str().unwrap_or("");
            out.push_str(&format!("{indent}![{}]({src})\n", n["attrs"]["alt"].as_str().unwrap_or("")));
        }
        _ => out.push_str(&format!("{indent}{}\n", md_inlines(content))),
    }
}

fn md_escape(t: &str) -> String {
    let mut s = String::with_capacity(t.len());
    for c in t.chars() {
        if matches!(c, '*' | '_' | '`' | '[' | ']' | '\\' | '<') {
            s.push('\\');
        }
        s.push(c);
    }
    s
}

fn md_inlines(nodes: &[Value]) -> String {
    let mut s = String::new();
    for n in nodes {
        match n["type"].as_str().unwrap_or("") {
            "text" => {
                let t = n["text"].as_str().unwrap_or("");
                let marks = n["marks"].as_array().cloned().unwrap_or_default();
                let has = |m: &str| marks.iter().any(|x| x["type"] == m);
                let mut piece = if has("code") { format!("`{t}`") } else { md_escape(t) };
                if has("bold") {
                    piece = format!("**{piece}**");
                }
                if has("italic") {
                    piece = format!("*{piece}*");
                }
                if has("strike") {
                    piece = format!("~~{piece}~~");
                }
                if let Some(link) = marks.iter().find(|x| x["type"] == "link") {
                    piece = format!("[{piece}]({})", link["attrs"]["href"].as_str().unwrap_or(""));
                }
                s.push_str(&piece);
            }
            "hardBreak" => s.push_str("  \n"),
            "mention" => {
                let ch = n["attrs"]["mentionSuggestionChar"].as_str().unwrap_or("@");
                let label = n["attrs"]["label"].as_str().or(n["attrs"]["id"].as_str()).unwrap_or("");
                if ch == "#" {
                    s.push_str(n["attrs"]["id"].as_str().unwrap_or(label));
                } else {
                    s.push('@');
                    s.push_str(label);
                }
            }
            _ => {}
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_round_trip() {
        let md = "# Title\n\nSome **bold** and *italic* text with `code` and a [link](https://example.com).\n\n- one\n- two\n\n1. first\n2. second\n\n- [x] done\n- [ ] open\n\n> quoted\n\n```rust\nfn main() {}\n```";
        let doc = from_markdown(md);
        let back = to_markdown(&doc);
        assert_eq!(back, md);
    }

    #[test]
    fn unsafe_links_are_dropped() {
        let doc = from_markdown("[x](javascript:alert(1))");
        assert!(doc.to_string().contains("\"text\":\"x\""));
        assert!(!doc.to_string().contains("javascript"));
    }

    #[test]
    fn text_and_mentions() {
        let id = Uuid::now_v7();
        let doc = json!({"type": "doc", "content": [
            {"type": "paragraph", "content": [{"type": "text", "text": "Hi "}, {"type": "mention", "attrs": {"id": id.to_string(), "label": "Ada", "mentionSuggestionChar": "@"}}]},
            {"type": "paragraph", "content": [{"type": "text", "text": "see "}, {"type": "mention", "attrs": {"id": "WEB-3", "label": "WEB-3", "mentionSuggestionChar": "#"}}]}
        ]});
        assert_eq!(text(&doc), "Hi @Ada\nsee WEB-3");
        assert_eq!(mentioned_people(&doc), vec![id]);
        assert_eq!(referenced_issues(&doc), vec!["WEB-3".to_string()]);
        assert!(validate(&doc).is_ok());
        assert!(validate(&json!({"type": "paragraph"})).is_err());
    }
}
