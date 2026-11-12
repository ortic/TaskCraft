//! Wiki markup — `h1.` headings, `*bold*`, `[text|url]` links, `{code}` blocks, `# item` lists,
//! as several trackers store descriptions — and Textile (`"text":url`, `@code@`, `<pre>`) turned
//! into Markdown for imported issues.

use std::sync::LazyLock;

use regex::{Captures, Regex};

fn re(s: &str) -> Regex {
    Regex::new(s).expect("valid pattern")
}

/// Whether text is wiki markup rather than Markdown: it uses something only wiki markup has.
pub fn looks_like_wiki(text: &str) -> bool {
    static R: LazyLock<Regex> = LazyLock::new(|| {
        re(
            r"(?m)(^h[1-6]\. |^bq\. |\{(code|noformat|quote|panel|color)[:}|]|\[[^\]\n|]+\|[^\]\n]+\]|\[~[^\]\n]+\]|![^!\s|][^!\n|]*\.(?i:png|jpe?g|gif|svg|webp|bmp)(\|[^!\n]*)?!|\{\{[^}\n]+\}\})",
        )
    });
    R.is_match(text)
}

/// Wiki markup as Markdown. Code blocks are kept verbatim; images point at files the import
/// doesn't bring along, so they become a note with the file name.
pub fn to_markdown(text: &str) -> String {
    static CODE: LazyLock<Regex> = LazyLock::new(|| re(r"\{(?:code|noformat)(?::([A-Za-z0-9_+#.-]+))?(?:\|[^}]*)?\}"));
    let text = text.replace("\r\n", "\n");
    let mut out = String::new();
    let mut pos = 0;
    let mut in_code = false;
    for c in CODE.captures_iter(&text) {
        let m = c.get(0).expect("whole match");
        let seg = &text[pos..m.start()];
        if in_code {
            out.push_str(seg.trim_matches('\n'));
            out.push_str("\n```\n");
        } else {
            out.push_str(&prose(seg));
            out.push_str(&format!("\n```{}\n", c.get(1).map_or("", |l| l.as_str())));
        }
        in_code = !in_code;
        pos = m.end();
    }
    let rest = &text[pos..];
    if in_code {
        out.push_str(rest.trim_matches('\n'));
        out.push_str("\n```\n");
    } else {
        out.push_str(&prose(rest));
    }
    out.trim().to_string()
}

#[derive(PartialEq)]
enum Kind {
    Blank,
    Block,
    Prose,
}

/// Line by line: headings, quotes, rules and lists; consecutive prose lines keep their breaks
/// (a newline is a line break in wiki markup, a space in Markdown).
fn prose(text: &str) -> String {
    static HEADING: LazyLock<Regex> = LazyLock::new(|| re(r"^\s*h([1-6])\.\s+(.*)$"));
    static LIST: LazyLock<Regex> = LazyLock::new(|| re(r"^\s*([*#-]+)\s+(.*)$"));
    let lines: Vec<(Kind, String)> = text
        .split('\n')
        .map(|l| {
            if l.trim().is_empty() {
                (Kind::Blank, String::new())
            } else if let Some(c) = HEADING.captures(l) {
                (Kind::Block, format!("{} {}", "#".repeat(c[1].parse().unwrap_or(1)), inline(&c[2])))
            } else if let Some(rest) = l.trim_start().strip_prefix("bq. ") {
                (Kind::Block, format!("> {}", inline(rest)))
            } else if l.trim() == "----" {
                (Kind::Block, "---".into())
            } else if let Some(c) = LIST.captures(l).filter(|c| c[1].chars().all(|ch| ch == '*' || ch == '#') || &c[1] == "-") {
                let depth = c[1].len();
                let marker = if c[1].ends_with('#') { "1." } else { "-" };
                (Kind::Block, format!("{}{marker} {}", "   ".repeat(depth - 1), inline(&c[2])))
            } else {
                // a line of macros only (`{panel:bgColor=…}`, `{quote}`) separates blocks
                let s = inline(l.trim_end());
                if s.trim().is_empty() { (Kind::Blank, String::new()) } else { (Kind::Prose, s) }
            }
        })
        .collect();
    let mut out = Vec::with_capacity(lines.len());
    for (i, (kind, l)) in lines.iter().enumerate() {
        let next_prose = lines.get(i + 1).is_some_and(|n| n.0 == Kind::Prose);
        if *kind == Kind::Prose && next_prose {
            out.push(format!("{l}\\"));
        } else {
            out.push(l.clone());
        }
    }
    out.join("\n")
}

/// Links, mentions, images, monospace, bold and italics within a line.
fn inline(s: &str) -> String {
    static NOISE: LazyLock<Regex> = LazyLock::new(|| re(r"\{(?:color(?::[^}]*)?|panel(?::[^}]*)?|quote)\}"));
    static MONO: LazyLock<Regex> = LazyLock::new(|| re(r"\{\{([^}\n]+)\}\}"));
    static MENTION: LazyLock<Regex> = LazyLock::new(|| re(r"\[~([^\]\n]+)\]"));
    static LINK: LazyLock<Regex> = LazyLock::new(|| re(r"\[([^\]\n|]+)\|([^\]\n|]+)(?:\|[^\]\n]*)?\]"));
    static BARE: LazyLock<Regex> = LazyLock::new(|| re(r"\[((?:https?|mailto):[^\]\s]+)\]"));
    static IMAGE: LazyLock<Regex> = LazyLock::new(|| re(r"!([^!\s|][^!\n|]*)(?:\|[^!\n]*)?!"));
    static BOLD: LazyLock<Regex> = LazyLock::new(|| re(r"(^|[^\w*])\*([^*\s](?:[^*\n]*[^*\s])?)\*($|[^\w*])"));
    static ITALIC: LazyLock<Regex> = LazyLock::new(|| re(r"(^|[\s(])_([^_\s](?:[^_\n]*[^_\s])?)_($|[\s).,;:!?])"));
    let s = NOISE.replace_all(s, "");
    let s = MONO.replace_all(&s, "`$1`");
    let s = MENTION.replace_all(&s, |c: &Captures| if c[1].starts_with("accountid:") { "@someone".to_string() } else { format!("@{}", &c[1]) });
    let s = IMAGE.replace_all(&s, |c: &Captures| {
        let src = c[1].trim();
        if src.starts_with("http://") || src.starts_with("https://") { format!("<{src}>") } else { format!("[image: {src}]") }
    });
    // the text is the address itself: a plain link; else <…> keeps parentheses in addresses intact
    let s = LINK.replace_all(&s, |c: &Captures| {
        let (text, url) = (c[1].trim(), c[2].trim());
        if text == url || text.starts_with("http://") || text.starts_with("https://") { format!("<{url}>") } else { format!("[{text}](<{url}>)") }
    });
    let s = BARE.replace_all(&s, "<$1>");
    let s = BOLD.replace_all(&s, "$1**$2**$3");
    ITALIC.replace_all(&s, "$1*$2*$3").into_owned()
}

/// Textile as Markdown: headings, quotes, lists, `<pre>` and `bc.` code blocks, `"text":url`
/// links, `!image!`s, `@code@`, `*bold*`, `_italic_` and `-deleted-`; tables stay as lines of
/// text, macros (`{{toc}}`) are dropped and a newline inside a paragraph is a line break.
pub fn textile_to_markdown(text: &str) -> String {
    static PRE: LazyLock<Regex> =
        LazyLock::new(|| re(r#"(?is)<pre[^>]*>\s*(?:<code(?:\s+class=["']?(?:language-)?([A-Za-z0-9_+#.-]*)["']?)?[^>]*>)?(.*?)(?:</code>\s*)?</pre>"#));
    let text = text.replace("\r\n", "\n");
    let mut out = String::new();
    let mut pos = 0;
    for c in PRE.captures_iter(&text) {
        let m = c.get(0).expect("whole match");
        out.push_str(&textile_blocks(&text[pos..m.start()]));
        let code = c[2].replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&amp;", "&");
        out.push_str(&format!("\n```{}\n{}\n```\n", c.get(1).map_or("", |l| l.as_str()), code.trim_matches('\n')));
        pos = m.end();
    }
    out.push_str(&textile_blocks(&text[pos..]));
    out.trim().to_string()
}

fn textile_blocks(text: &str) -> String {
    static HEADING: LazyLock<Regex> = LazyLock::new(|| re(r"^h([1-6])(?:\([^)\n]*\)|\{[^}\n]*\}|\[[^\]\n]*\]|[<>=]+)*\.\s+(.*)$"));
    static BLOCK: LazyLock<Regex> = LazyLock::new(|| re(r"^(p|bq|bc|pre|fn\d+)(?:\([^)\n]*\)|\{[^}\n]*\}|\[[^\]\n]*\]|[<>=]+)*\.(\.)?\s+(.*)$"));
    static LIST: LazyLock<Regex> = LazyLock::new(|| re(r"^\s*([*#]+)\s+(.*)$"));
    static MACRO: LazyLock<Regex> = LazyLock::new(|| re(r"\{\{[^}\n]*\}\}"));
    static COLLAPSE: LazyLock<Regex> = LazyLock::new(|| re(r"^\{\{collapse(?:\(([^)\n]*)\))?\s*$"));
    let mut lines: Vec<(Kind, String)> = Vec::new();
    let mut code = false;
    for l in text.split('\n') {
        if code {
            if l.trim().is_empty() {
                lines.push((Kind::Block, "```".into()));
                lines.push((Kind::Blank, String::new()));
                code = false;
            } else {
                lines.push((Kind::Block, l.to_string()));
            }
            continue;
        }
        let t = l.trim_end();
        let line = if t.trim().is_empty() || t.trim() == "}}" {
            (Kind::Blank, String::new())
        } else if let Some(c) = COLLAPSE.captures(t.trim()) {
            (Kind::Block, c.get(1).map(|m| textile_inline(m.as_str())).unwrap_or_default())
        } else if let Some(c) = HEADING.captures(t) {
            (Kind::Block, format!("{} {}", "#".repeat(c[1].parse().unwrap_or(1)), textile_inline(&c[2])))
        } else if let Some(c) = BLOCK.captures(t) {
            match &c[1] {
                "bq" => (Kind::Block, format!("> {}", textile_inline(&c[3]))),
                "bc" | "pre" => {
                    lines.push((Kind::Block, "```".into()));
                    lines.push((Kind::Block, c[3].to_string()));
                    code = true;
                    continue;
                }
                _ => (Kind::Prose, textile_inline(&c[3])),
            }
        } else if let Some(c) = LIST.captures(t) {
            let depth = c[1].len();
            let marker = if c[1].ends_with('#') { "1." } else { "-" };
            (Kind::Block, format!("{}{marker} {}", "   ".repeat(depth - 1), textile_inline(&c[2])))
        } else {
            let s = textile_inline(&MACRO.replace_all(t, ""));
            if s.trim().is_empty() { (Kind::Blank, String::new()) } else { (Kind::Prose, s) }
        };
        lines.push(line);
    }
    if code {
        lines.push((Kind::Block, "```".into()));
    }
    let mut out = Vec::with_capacity(lines.len());
    for (i, (kind, l)) in lines.iter().enumerate() {
        let next_prose = lines.get(i + 1).is_some_and(|n| n.0 == Kind::Prose);
        if *kind == Kind::Prose && next_prose {
            out.push(format!("{l}\\"));
        } else {
            out.push(l.clone());
        }
    }
    out.join("\n")
}

/// Links, images, attachments, code, emphasis within a line of Textile.
fn textile_inline(s: &str) -> String {
    static ESCAPED: LazyLock<Regex> = LazyLock::new(|| re(r"<notextile>(.*?)</notextile>|==([^=\n]+)=="));
    static CODE: LazyLock<Regex> = LazyLock::new(|| re(r"(^|[^\w@])@([^@\s](?:[^@\n]*[^@\s])?)@"));
    static IMAGE: LazyLock<Regex> = LazyLock::new(|| re(r"!(?:[<>=]|\{[^}\n]*\})*([^\s!(){}][^\s!()]*)(?:\(([^)\n]*)\))?!(?::[^\s]+)?"));
    static LINK: LazyLock<Regex> = LazyLock::new(|| re(r#""([^"\n]+?)(?:\(([^)\n]*)\))?":((?:https?://|ftp://|mailto:|/)[^\s<>"]*[^\s<>".,;:!?)\]'])"#));
    static ATTACHMENT: LazyLock<Regex> = LazyLock::new(|| re(r#"(^|[\s(])attachment:(?:"([^"\n]+)"|([^\s"]*[^\s".,;:!?)]))"#));
    static WIKI: LazyLock<Regex> = LazyLock::new(|| re(r"\[\[(?:[^\]|\n]*\|)?([^\]\n]+)\]\]"));
    static BOLD: LazyLock<Regex> = LazyLock::new(|| re(r"(^|[^\w*])\*\*?([^*\s](?:[^*\n]*[^*\s])?)\*\*?($|[^\w*])"));
    static ITALIC: LazyLock<Regex> = LazyLock::new(|| re(r"(^|[\s(])__?([^_\s](?:[^_\n]*[^_\s])?)__?($|[\s).,;:!?])"));
    static DELETED: LazyLock<Regex> = LazyLock::new(|| re(r"(^|[\s(])-([^-\s](?:[^-\n]*[^-\s])?)-($|[\s).,;:!?])"));
    static PLAIN: LazyLock<Regex> = LazyLock::new(|| re(r"(^|[\s(\w])(?:\+([^+\s](?:[^+\n]*[^+\s])?)\+|\?\?([^?\n]+)\?\?|\^([^^\s]+)\^)($|[\s).,;:!?])"));
    static CELL: LazyLock<Regex> = LazyLock::new(|| re(r"\|(?:[_<>=^~]|\\\d+|/\d+|\{[^}\n]*\}|\([^)\n]*\))+\.\s"));
    let s = ESCAPED.replace_all(s, |c: &Captures| c.get(1).or(c.get(2)).map_or(String::new(), |m| m.as_str().to_string()));
    let s = CODE.replace_all(&s, "$1`$2`");
    let s = IMAGE.replace_all(&s, |c: &Captures| format!("![{}]({})", c.get(2).map_or("image", |m| m.as_str()), &c[1]));
    let s = LINK.replace_all(&s, |c: &Captures| {
        let (text, url) = (c[1].trim(), &c[3]);
        if text == url { format!("<{url}>") } else { format!("[{text}](<{url}>)") }
    });
    let s = ATTACHMENT.replace_all(&s, |c: &Captures| {
        let name = c.get(2).or(c.get(3)).map_or("", |m| m.as_str());
        format!("{}[{name}](<attachment:{name}>)", &c[1])
    });
    let s = WIKI.replace_all(&s, "$1");
    let s = CELL.replace_all(&s, "| ");
    let s = BOLD.replace_all(&s, "$1**$2**$3");
    let s = ITALIC.replace_all(&s, "$1*$2*$3");
    let s = DELETED.replace_all(&s, "$1~~$2~~$3");
    // twice: neighbours share the space between them
    let plain = |s: &str| {
        PLAIN.replace_all(s, |c: &Captures| format!("{}{}{}", &c[1], c.get(2).or(c.get(3)).or(c.get(4)).map_or("", |m| m.as_str()), &c[5])).into_owned()
    };
    plain(&plain(&s))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_wiki_markup_but_not_markdown() {
        assert!(looks_like_wiki("h2. Steps\nclick"));
        assert!(looks_like_wiki("see [the docs|https://example.com/docs]"));
        assert!(looks_like_wiki("screenshot: !shot-1.png|thumbnail!"));
        assert!(looks_like_wiki("{code:sql}select 1{code}"));
        assert!(!looks_like_wiki("## Steps\n\n- click **here** and see [docs](https://example.com)"));
        assert!(!looks_like_wiki("Wow! That's it!"));
    }

    #[test]
    fn converts_blocks_and_inline_marks() {
        let md = to_markdown(
            "h1. Login breaks\nWhen I log in\nthe page is blank.\n\n* first\n** nested\n# one\n# two\n\n{code:java}if (a*b) { x_y(); }{code}\nbq. a quote\nAsk [~mira], *now* and _soon_: [docs|https://example.com/a_b] or [https://example.com].\n!err.png|width=300! {{monospace}} {color:red}red{color}",
        );
        assert!(md.starts_with("# Login breaks\nWhen I log in\\\nthe page is blank."), "{md}");
        assert!(md.contains("- first\n   - nested\n1. one\n1. two"), "{md}");
        assert!(md.contains("```java\nif (a*b) { x_y(); }\n```"), "code stays as it is: {md}");
        assert!(md.contains("> a quote"));
        assert!(md.contains("@mira, **now** and *soon*: [docs](<https://example.com/a_b>) or <https://example.com>."), "{md}");
        assert!(md.contains("[image: err.png] `monospace` red"), "{md}");
    }

    #[test]
    fn links_survive_into_the_document() {
        let doc = crate::doc::from_markdown(&to_markdown("see [the spec|https://example.com/a-(b)] and [https://example.com/x|https://example.com/x]"));
        let json = doc.to_string();
        assert!(json.contains(r#""href":"https://example.com/a-(b)""#), "{json}");
        assert!(json.contains(r#""href":"https://example.com/x""#), "{json}");
        assert!(!json.contains("<https"), "{json}");
    }

    #[test]
    fn leaves_ordinary_text_alone() {
        assert_eq!(to_markdown("a_b_c and 2*3*4, snake_case_name"), "a_b_c and 2*3*4, snake_case_name");
        assert_eq!(to_markdown("{noformat}\n*raw*\n{noformat}"), "```\n*raw*\n```");
        assert_eq!(to_markdown("[~accountid:5570:abc] fixed it"), "@someone fixed it");
        assert_eq!(to_markdown("[https://example.com/a-(b)|https://example.com/a-(b)]"), "<https://example.com/a-(b)>");
        assert_eq!(to_markdown("{panel:bgColor=#ddeeff}\nFirst\n{panel}\n\n{panel}\nSecond\n{panel}"), "First\n\n\n\nSecond");
    }
    #[test]
    fn textile_blocks_and_inline_marks() {
        let md = textile_to_markdown(
            "h2. Login breaks\nWhen I log in\nthe page is blank.\n\n* first\n** nested\n# one\n\nbq. a quote\np>. Ask *now*, _soon_ and -never-: \"the docs\":https://example.com/a_b, \"https://example.com\":https://example.com.\n@code here@ and +under+ ??cite?? x^2^\n!>screen.png(The screen)! attachment:\"log file.txt\" and attachment:notes.pdf.\n{{toc}}\n[[Wiki page|Shown]] |_. Head |/2. cell |",
        );
        assert!(md.starts_with("## Login breaks\nWhen I log in\\\nthe page is blank."), "{md}");
        assert!(md.contains("- first\n   - nested\n1. one"), "{md}");
        assert!(md.contains("> a quote"), "{md}");
        assert!(md.contains("Ask **now**, *soon* and ~~never~~: [the docs](<https://example.com/a_b>), <https://example.com>."), "{md}");
        assert!(md.contains("`code here` and under cite x2"), "{md}");
        assert!(md.contains("![The screen](screen.png) [log file.txt](<attachment:log file.txt>) and [notes.pdf](<attachment:notes.pdf>)."), "{md}");
        assert!(!md.contains("toc"), "{md}");
        assert!(md.contains("Shown | Head | cell |"), "{md}");
    }

    #[test]
    fn textile_code_stays_as_it_is() {
        let md = textile_to_markdown(
            "Before\n<pre><code class=\"ruby\">\nx = a * b * c\nputs \"&lt;done&gt;\"\n</code></pre>\nAfter *that*\n\nbc. one_two *three*\n\nend",
        );
        assert!(md.contains("```ruby\nx = a * b * c\nputs \"<done>\"\n```"), "{md}");
        assert!(md.contains("After **that**"), "{md}");
        assert!(md.contains("```\none_two *three*\n```"), "{md}");
        assert!(md.ends_with("end"), "{md}");
    }

    #[test]
    fn textile_leaves_ordinary_text_alone() {
        assert_eq!(textile_to_markdown("a_b_c and 2*3*4, snake_case_name, 2026-11-12 - fine"), "a_b_c and 2*3*4, snake_case_name, 2026-11-12 - fine");
        assert_eq!(textile_to_markdown("mail me@example.com, thanks @alice and @bob"), "mail me@example.com, thanks @alice and @bob");
        assert_eq!(textile_to_markdown("See #12 and r34"), "See #12 and r34");
    }
}
