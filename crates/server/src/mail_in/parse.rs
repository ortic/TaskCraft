//! Reading a received e-mail: who sent it and to whom, what it answers, whether a machine sent
//! it, its text (HTML turned into plain text — never rendered) and its files; quoted replies and
//! signatures stripped from answers; text turned into rich text.

use mail_parser::{MessageParser, MimeHeaders, PartType};
use serde_json::{Value, json};
use uuid::Uuid;

/// Longest text kept from one e-mail (characters).
const MAX_TEXT: usize = 100_000;
/// Inline images smaller than this are taken for logos in signatures and left out.
const SMALL_INLINE_IMAGE: usize = 16 * 1024;
/// Files kept from one e-mail.
const MAX_FILES: usize = 20;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attachment {
    pub name: String,
    pub mime: String,
    pub data: Vec<u8>,
}

/// A received e-mail, as far as TaskCraft cares.
#[derive(Debug, Clone, Default)]
pub struct Inbound {
    /// Message-ID without angle brackets
    pub message_id: Option<String>,
    /// sender address, lower case
    pub from: Option<String>,
    pub from_name: Option<String>,
    /// To, Cc, Delivered-To, X-Original-To, Envelope-To: lower case
    pub recipients: Vec<String>,
    pub subject: String,
    /// the subject without Re: / Fwd: and the like
    pub topic: String,
    /// Message-IDs it answers, closest first (In-Reply-To, then References from the end)
    pub references: Vec<String>,
    /// why it looks automatic: `auto-reply`, `bulk` or `bounce`
    pub automatic: Option<&'static str>,
    /// the receiving server found its sender forged (DMARC)
    pub dmarc_fail: bool,
    /// the text (from the HTML part when there is no plain one)
    pub text: String,
    pub attachments: Vec<Attachment>,
    /// files left out (too many)
    pub skipped_files: usize,
}

/// One line, without control characters, at most `max` characters.
pub fn one_line(s: &str, max: usize) -> String {
    let flat: String = s.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    flat.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(max).collect()
}

/// Addresses in a header value (`"Name" <a@b>, c@d`), lower case.
pub fn addresses_in(value: &str) -> Vec<String> {
    let mut out = Vec::new();
    for token in value.split([',', ';', ' ', '\t', '\r', '\n']) {
        let t = token.trim().trim_start_matches('<').trim_end_matches('>');
        let t = match t.rfind('<') {
            Some(i) => &t[i + 1..],
            None => t,
        };
        let t = t.trim_matches(|c: char| c == '"' || c == '\'' || c == '<' || c == '>' || c == '(' || c == ')');
        if let Some(at) = t.find('@')
            && at > 0
            && at < t.len() - 1
            && !t.contains(char::is_whitespace)
            && t.len() <= 254
        {
            let a = t.to_lowercase();
            if !out.contains(&a) {
                out.push(a);
            }
        }
    }
    out
}

/// Strips reply and forward prefixes (`Re:`, `AW:`, `Fwd:`, `WG:`, `Re[2]:`, …).
pub fn topic(subject: &str) -> String {
    const PREFIXES: [&str; 18] = ["re", "aw", "fw", "fwd", "wg", "sv", "vs", "antw", "tr", "rif", "r", "ref", "réf", "odp", "enc", "rv", "vá", "res"];
    let mut s = subject.trim();
    while let Some(colon) = s.find(':') {
        let head = s[..colon].trim();
        // Re[2] / Re(2)
        let word = head.split(['[', '(']).next().unwrap_or("").trim();
        let counter_ok = head[word.len()..].trim().is_empty()
            || head[word.len()..].trim().trim_matches(|c| c == '[' || c == ']' || c == '(' || c == ')').chars().all(|c| c.is_ascii_digit());
        if !word.is_empty() && counter_ok && PREFIXES.contains(&word.to_lowercase().as_str()) {
            s = s[colon + 1..].trim_start();
        } else {
            break;
        }
    }
    s.to_string()
}

/// Issue keys in square brackets in a subject (`[SUP-12]`), as (key, number).
pub fn subject_keys(subject: &str) -> Vec<(String, i32)> {
    let mut out = Vec::new();
    let mut rest = subject;
    while let Some(open) = rest.find('[') {
        let after = &rest[open + 1..];
        let Some(close) = after.find(']') else { break };
        if let Some(k) = crate::query::split_key(&after[..close])
            && !out.contains(&k)
        {
            out.push(k);
        }
        rest = &after[close..];
    }
    out
}

/// Issues named by Message-IDs TaskCraft gave its e-mails (`tc.<issue>…@…`).
pub fn issue_ids(references: &[String]) -> Vec<Uuid> {
    let mut out = Vec::new();
    for r in references {
        let local = r.trim().trim_matches(|c| c == '<' || c == '>').split('@').next().unwrap_or("");
        if let Some(rest) = local.strip_prefix("tc.")
            && let Ok(id) = rest.get(..36).unwrap_or("").parse::<Uuid>()
            && (rest.len() == 36 || rest[36..].starts_with('.'))
            && !out.contains(&id)
        {
            out.push(id);
        }
    }
    out
}

impl Inbound {
    /// Reads an RFC 822 message (None when it isn't one).
    pub fn parse(raw: &[u8]) -> Option<Inbound> {
        let msg = MessageParser::default().parse(raw)?;
        if msg.headers().is_empty() {
            return None;
        }
        let raw_header = |name: &str| -> Vec<&str> { msg.headers_raw().filter(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, v)| v).collect() };
        let has = |name: &str| !raw_header(name).is_empty();
        let first = |name: &str| raw_header(name).first().map(|v| one_line(v, 500).to_lowercase());

        let (from, from_name) = match msg.from().and_then(|a| a.first()) {
            Some(a) => (
                a.address().map(|x| x.trim().to_lowercase()).filter(|x| x.contains('@') && !x.contains(char::is_whitespace)),
                a.name().map(|n| one_line(n, 200)).filter(|n| !n.is_empty()),
            ),
            None => (None, None),
        };

        let mut recipients: Vec<String> = Vec::new();
        for list in [msg.to(), msg.cc()].into_iter().flatten() {
            for a in list.iter() {
                if let Some(x) = a.address() {
                    let x = x.trim().to_lowercase();
                    if x.contains('@') && !recipients.contains(&x) {
                        recipients.push(x);
                    }
                }
            }
        }
        for h in ["Delivered-To", "X-Original-To", "Envelope-To", "X-Envelope-To", "X-Forwarded-To"] {
            for v in raw_header(h) {
                for a in addresses_in(v) {
                    if !recipients.contains(&a) {
                        recipients.push(a);
                    }
                }
            }
        }

        let ids = |v: &mail_parser::HeaderValue| -> Vec<String> {
            match v {
                mail_parser::HeaderValue::Text(t) => vec![t.to_string()],
                mail_parser::HeaderValue::TextList(l) => l.iter().map(|t| t.to_string()).collect(),
                _ => Vec::new(),
            }
        };
        let mut references: Vec<String> = ids(msg.in_reply_to());
        for r in ids(msg.references()).into_iter().rev() {
            if !references.contains(&r) {
                references.push(r);
            }
        }
        references.retain(|r| !r.trim().is_empty());
        references.truncate(50);

        // machines: auto-replies (RFC 3834 and common vendor headers), bulk mail and bounces
        let auto_submitted = first("Auto-Submitted").filter(|v| !v.is_empty() && !v.starts_with("no"));
        let precedence = first("Precedence").unwrap_or_default();
        let content_type = msg.content_type().map(|c| format!("{}/{}", c.ctype(), c.subtype().unwrap_or("")).to_lowercase()).unwrap_or_default();
        let local = from.as_deref().and_then(|f| f.split('@').next()).unwrap_or("").to_string();
        let automatic = if content_type == "multipart/report"
            || has("X-Failed-Recipients")
            || matches!(local.as_str(), "mailer-daemon" | "postmaster")
            || first("Return-Path").is_some_and(|r| r == "<>")
        {
            Some("bounce")
        } else if auto_submitted.is_some()
            || ["X-Autoreply", "X-Autorespond", "X-Autoresponder", "X-Auto-Reply"].iter().any(|h| has(h))
            || precedence == "auto_reply"
        {
            Some("auto-reply")
        } else if matches!(precedence.as_str(), "bulk" | "list" | "junk") {
            Some("bulk")
        } else {
            None
        };
        // the topmost Authentication-Results is the receiving server's
        let dmarc_fail = first("Authentication-Results").is_some_and(|v| v.contains("dmarc=fail"));

        let mut text = String::new();
        let body = msg.text_bodies().next().or_else(|| msg.html_bodies().next());
        if let Some(part) = body {
            let flowed = part.content_type().and_then(|c| c.attribute("format")).is_some_and(|f| f.eq_ignore_ascii_case("flowed"));
            let delsp = part.content_type().and_then(|c| c.attribute("delsp")).is_some_and(|f| f.eq_ignore_ascii_case("yes"));
            text = match &part.body {
                PartType::Text(t) if flowed => unflow(t, delsp),
                PartType::Text(t) => t.replace("\r\n", "\n"),
                PartType::Html(h) => html_to_text(h),
                _ => String::new(),
            };
        }
        let text = clean_text(&text);

        let mut attachments = Vec::new();
        let mut skipped_files = 0;
        for (n, part) in msg.attachments().enumerate() {
            let mime = part
                .content_type()
                .map(|c| format!("{}/{}", c.ctype(), c.subtype().unwrap_or("octet-stream")).to_lowercase())
                .unwrap_or_else(|| "application/octet-stream".into());
            if matches!(mime.as_str(), "application/pkcs7-signature" | "application/x-pkcs7-signature" | "application/pgp-signature") {
                continue;
            }
            let data = part.contents();
            let disposition_attachment = part.content_disposition().is_some_and(|d| d.ctype().eq_ignore_ascii_case("attachment"));
            if part.content_id().is_some() && !disposition_attachment && mime.starts_with("image/") && data.len() < SMALL_INLINE_IMAGE {
                continue;
            }
            if data.is_empty() {
                continue;
            }
            if attachments.len() >= MAX_FILES {
                skipped_files += 1;
                continue;
            }
            let name = match part.attachment_name().map(file_name).filter(|n| !n.is_empty()) {
                Some(n) => n,
                None if matches!(part.body, PartType::Message(_)) => "message.eml".into(),
                None => {
                    let ext = mime_guess::get_mime_extensions_str(&mime).and_then(|e| e.first()).map(|e| format!(".{e}")).unwrap_or_default();
                    format!("attachment-{}{ext}", n + 1)
                }
            };
            attachments.push(Attachment { name, mime, data: data.to_vec() });
        }

        let subject = one_line(msg.subject().unwrap_or(""), 500);
        Some(Inbound {
            message_id: msg.message_id().map(|m| one_line(m, 500)).filter(|m| !m.is_empty()),
            from,
            from_name,
            recipients,
            topic: topic(&subject),
            subject,
            references,
            automatic,
            dmarc_fail,
            text,
            attachments,
            skipped_files,
        })
    }
}

/// A safe file name (no paths), at most 200 characters.
fn file_name(n: &str) -> String {
    let n: String = n.chars().map(|c| if matches!(c, '/' | '\\' | '\0') || c.is_control() { '_' } else { c }).collect();
    n.trim().trim_start_matches('.').chars().take(200).collect()
}

/// Joins `format=flowed` lines (RFC 3676): a line ending in a space continues on the next.
fn unflow(t: &str, delsp: bool) -> String {
    let mut out = String::new();
    for line in t.replace("\r\n", "\n").split('\n') {
        // space-stuffing
        let line = line.strip_prefix(' ').unwrap_or(line);
        if line.ends_with(' ') && line != "-- " {
            out.push_str(if delsp { &line[..line.len() - 1] } else { line });
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// Trailing spaces off, at most one empty line in a row, bounded length.
fn clean_text(t: &str) -> String {
    let mut out = String::new();
    let mut blank = 0;
    for line in t.replace("\r\n", "\n").replace('\r', "\n").lines() {
        let line = line.trim_end();
        if line.trim().is_empty() {
            blank += 1;
            if blank > 1 {
                continue;
            }
            out.push('\n');
        } else {
            blank = 0;
            out.push_str(line);
            out.push('\n');
        }
    }
    let out = out.trim_matches('\n').to_string();
    if out.chars().count() > MAX_TEXT {
        let cut: String = out.chars().take(MAX_TEXT).collect();
        format!("{cut}\n[…]")
    } else {
        out
    }
}

// ---- HTML → text --------------------------------------------------------------------------

struct Writer {
    out: String,
    /// blockquote depth
    quote: usize,
    line_start: bool,
    space: bool,
    /// an empty line is due before the next text (a new paragraph)
    gap: bool,
    /// quote depth of the last line written
    last_depth: usize,
}

impl Writer {
    fn ch(&mut self, c: char) {
        if self.line_start {
            // paragraphs are apart; into a quote the `>` is separation enough, out of one an
            // empty line follows
            if self.gap && !self.out.is_empty() && !self.out.ends_with("\n\n") && self.last_depth >= self.quote {
                if self.last_depth == self.quote && self.quote > 0 {
                    self.out.push('>');
                }
                self.out.push('\n');
            }
            for _ in 0..self.quote {
                self.out.push_str("> ");
            }
            self.line_start = false;
            self.last_depth = self.quote;
        } else if self.space {
            self.out.push(' ');
        }
        self.space = false;
        self.gap = false;
        self.out.push(c);
    }

    fn text(&mut self, s: &str, pre: bool) {
        for c in s.chars() {
            if pre {
                if c == '\n' { self.newline() } else { self.ch(c) }
            } else if c.is_whitespace() {
                self.space = !self.line_start;
            } else {
                self.ch(c);
            }
        }
    }

    fn newline(&mut self) {
        self.out.push('\n');
        self.line_start = true;
        self.space = false;
    }

    /// Continue on a new line.
    fn block(&mut self) {
        if !self.line_start {
            self.newline();
        }
        self.space = false;
    }

    /// Continue after an empty line.
    fn paragraph(&mut self) {
        self.block();
        self.gap = true;
    }
}

/// Decodes HTML character references (`&amp;`, `&#228;`, `&#x1F600;`, common named ones).
pub fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let end = rest[1..].find(|c: char| c == ';' || c == '&' || c.is_whitespace() || c == '<').map(|e| e + 1);
        let decoded = match end {
            Some(e) if e <= 12 && rest[e..].starts_with(';') => {
                let name = &rest[1..e];
                let c = if let Some(n) = name.strip_prefix("#x").or_else(|| name.strip_prefix("#X")) {
                    u32::from_str_radix(n, 16).ok().and_then(char::from_u32)
                } else if let Some(n) = name.strip_prefix('#') {
                    n.parse::<u32>().ok().and_then(char::from_u32)
                } else {
                    named_entity(name)
                };
                c.map(|c| (c, e + 1))
            }
            _ => None,
        };
        match decoded {
            Some((c, len)) => {
                out.push(if c == '\u{a0}' { ' ' } else { c });
                rest = &rest[len..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

fn named_entity(name: &str) -> Option<char> {
    Some(match name {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" => ' ',
        "auml" => 'ä',
        "ouml" => 'ö',
        "uuml" => 'ü',
        "Auml" => 'Ä',
        "Ouml" => 'Ö',
        "Uuml" => 'Ü',
        "szlig" => 'ß',
        "eacute" => 'é',
        "egrave" => 'è',
        "agrave" => 'à',
        "aacute" => 'á',
        "ccedil" => 'ç',
        "euro" => '€',
        "pound" => '£',
        "copy" => '©',
        "reg" => '®',
        "trade" => '™',
        "ndash" => '–',
        "mdash" => '—',
        "hellip" => '…',
        "lsquo" => '‘',
        "rsquo" => '’',
        "sbquo" => '‚',
        "ldquo" => '“',
        "rdquo" => '”',
        "bdquo" => '„',
        "laquo" => '«',
        "raquo" => '»',
        "bull" => '•',
        "middot" => '·',
        "deg" => '°',
        "times" => '×',
        "zwnj" | "zwj" | "shy" => '\u{200b}',
        _ => return None,
    })
}

/// The value of an attribute in a tag's source (`a href="…"`).
fn attr(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let mut from = 0;
    while let Some(i) = lower[from..].find(name) {
        let at = from + i;
        from = at + name.len();
        let before_ok = at == 0 || lower.as_bytes()[at - 1].is_ascii_whitespace();
        let after = lower[from..].trim_start();
        if !before_ok || !after.starts_with('=') {
            continue;
        }
        let v = tag[tag.len() - after.len() + 1..].trim_start();
        let value = match v.chars().next() {
            Some(q @ ('"' | '\'')) => v[1..].split(q).next().unwrap_or(""),
            _ => v.split(|c: char| c.is_whitespace() || c == '>').next().unwrap_or(""),
        };
        return Some(decode_entities(value));
    }
    None
}

/// HTML as readable plain text: paragraphs and line breaks, lists as `- `, quotes as `> `,
/// links as `text (address)`; head, styles and scripts left out. Nothing of it is ever shown
/// as HTML.
pub fn html_to_text(html: &str) -> String {
    let mut w = Writer { out: String::new(), quote: 0, line_start: true, space: false, gap: false, last_depth: 0 };
    let mut skip = 0usize;
    let mut pre = 0usize;
    let mut lists: Vec<Option<u32>> = Vec::new();
    let mut link: Option<(String, usize)> = None;
    let mut i = 0;
    while i < html.len() {
        let rest = &html[i..];
        if rest.starts_with("<!--") {
            i += rest.find("-->").map(|e| e + 3).unwrap_or(rest.len());
            continue;
        }
        // a tag starts with a letter, `/`, `!` or `?` ("a < b" is text)
        if rest.starts_with('<') && rest[1..].starts_with(|c: char| c.is_ascii_alphabetic() || matches!(c, '/' | '!' | '?')) {
            let Some(end) = rest.find('>') else {
                if skip == 0 {
                    w.text(&decode_entities(rest), pre > 0);
                }
                break;
            };
            let tag = &rest[1..end];
            i += end + 1;
            let closing = tag.starts_with('/');
            let body = tag.trim_start_matches('/').trim();
            let name: String = body.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == ':').collect::<String>().to_ascii_lowercase();
            let self_closing = body.ends_with('/');
            match name.as_str() {
                "head" | "style" | "script" | "title" | "template" | "xml" | "o:p" if !self_closing => {
                    if name == "o:p" {
                        continue;
                    }
                    if closing {
                        skip = skip.saturating_sub(1);
                    } else {
                        skip += 1;
                    }
                }
                _ if skip > 0 => {}
                "br" => w.newline(),
                "p" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "table" => w.paragraph(),
                "pre" => {
                    w.paragraph();
                    pre = if closing { pre.saturating_sub(1) } else { pre + 1 };
                }
                "ul" | "ol" => {
                    w.block();
                    if closing {
                        lists.pop();
                    } else {
                        lists.push((name == "ol").then_some(1));
                    }
                }
                "li" if !closing => {
                    w.block();
                    for _ in 1..lists.len() {
                        w.text("\u{a0}\u{a0}", true);
                    }
                    match lists.last_mut() {
                        Some(Some(n)) => {
                            w.text(&format!("{n}. "), true);
                            *n += 1;
                        }
                        _ => w.text("- ", true),
                    }
                }
                "blockquote" => {
                    w.block();
                    w.quote = if closing { w.quote.saturating_sub(1) } else { w.quote + 1 };
                }
                "div" | "tr" | "li" | "section" | "article" | "header" | "footer" | "address" | "center" | "dl" | "dt" | "dd" | "figure" | "hr" | "form" => {
                    w.block()
                }
                "td" | "th" => w.space = !w.line_start,
                "a" if closing => {
                    if let Some((href, start)) = link.take() {
                        let shown = w.out.get(start..).unwrap_or("").to_string();
                        if !shown.trim().is_empty() && !shown.contains(href.trim_start_matches("https://").trim_start_matches("http://")) {
                            w.text(&format!(" ({href})"), false);
                        } else if shown.trim().is_empty() {
                            w.text(&href, false);
                        }
                    }
                }
                "a" => {
                    link = attr(body, "href")
                        .map(|h| h.trim().to_string())
                        .filter(|h| h.starts_with("http://") || h.starts_with("https://"))
                        .map(|h| (h, w.out.len()));
                }
                _ => {}
            }
            continue;
        }
        let first = rest.chars().next().map(char::len_utf8).unwrap_or(1);
        let next = rest[first..].find('<').map(|n| n + first).unwrap_or(rest.len());
        if skip == 0 {
            w.text(&decode_entities(&rest[..next]), pre > 0);
        }
        i += next;
    }
    clean_text(&w.out.replace('\u{a0}', " ").replace('\u{200b}', ""))
}

// ---- replies -------------------------------------------------------------------------------

/// "On Mon, 12 Nov 2026, Jane <jane@example.com> wrote:" in a few languages, also when the mail
/// program wrapped it onto two lines: how many lines it takes.
fn attribution(lines: &[&str], i: usize) -> Option<usize> {
    const ENDINGS: [&str; 12] =
        ["wrote:", "schrieb:", "a écrit :", "a écrit:", "escribió:", "schreef:", "ha scritto:", "escreveu:", "napisał:", "napisał(a):", "skrev:", "kirjoitti:"];
    const STARTS: [&str; 11] = ["on ", "am ", "le ", "el ", "op ", "il ", "em ", "w dniu ", "den ", "på ", "the "];
    let starts = |l: &str| {
        let l = l.trim_start().to_lowercase();
        STARTS.iter().any(|s| l.starts_with(s))
    };
    let ends = |l: &str| {
        let l = l.trim_end().to_lowercase();
        ENDINGS.iter().any(|e| l.ends_with(e)) || (l.contains(" schrieb ") && l.ends_with(':'))
    };
    let l = lines[i];
    if starts(l) && ends(l) {
        return Some(1);
    }
    if starts(l) && i + 1 < lines.len() && ends(lines[i + 1]) && !lines[i + 1].trim().is_empty() && l.len() + lines[i + 1].len() < 300 {
        return Some(2);
    }
    // "Jane Doe <jane@example.com> wrote:" without a date, or any "… wrote:" above a quote
    let quote_follows = lines.get(i + 1).is_some_and(|n| n.trim_start().starts_with('>'));
    if ends(l) && (l.contains('@') || quote_follows) && l.len() < 200 {
        return Some(1);
    }
    None
}

/// Where the rest of an e-mail is the earlier conversation or a signature: forwarded or
/// original-message separators, an Outlook header block (From: … Sent: …), `-- `, "Sent from
/// my phone".
fn rest_is_history(lines: &[&str], i: usize) -> bool {
    let l = lines[i].trim();
    let lower = l.to_lowercase();
    if l == "--" || lines[i] == "-- " {
        return true;
    }
    if lower.starts_with("---")
        && ["original message", "ursprüngliche nachricht", "message d'origine", "forwarded message", "weitergeleitete nachricht", "mensaje original"]
            .iter()
            .any(|m| lower.contains(m))
    {
        return true;
    }
    if l.len() >= 10 && l.chars().all(|c| c == '_') {
        return true;
    }
    let header = |s: &str, names: &[&str]| {
        let s = s.trim().trim_start_matches('*').to_lowercase();
        names.iter().any(|n| s.starts_with(n))
    };
    if header(l, &["from:", "von:", "de:", "van:", "da:", "från:", "fra:"]) {
        let next = &lines[i + 1..lines.len().min(i + 6)];
        let dated = next
            .iter()
            .any(|n| header(n, &["sent:", "gesendet:", "date:", "datum:", "envoyé :", "envoyé:", "verzonden:", "enviado:", "inviato:", "skickat:", "sendt:"]));
        let addressed = next.iter().any(|n| {
            header(
                n,
                &[
                    "to:",
                    "an:",
                    "à :",
                    "à:",
                    "aan:",
                    "para:",
                    "a:",
                    "till:",
                    "til:",
                    "subject:",
                    "betreff:",
                    "objet :",
                    "onderwerp:",
                    "asunto:",
                    "oggetto:",
                    "ämne:",
                    "emne:",
                ],
            )
        });
        if dated && addressed {
            return true;
        }
    }
    (lower.starts_with("sent from my ") || lower.starts_with("von meinem ") || lower.starts_with("get outlook for ") || lower.starts_with("envoyé de mon "))
        && l.len() < 80
}

/// The new part of a reply: without quoted lines (`>`), the "… wrote:" line above them, the
/// quoted conversation below Outlook's header block or an "Original message" separator, and
/// the signature.
pub fn strip_reply(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let mut out: Vec<&str> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if rest_is_history(&lines, i) {
            break;
        }
        if let Some(n) = attribution(&lines, i) {
            i += n;
            continue;
        }
        if lines[i].trim_start().starts_with('>') {
            i += 1;
            continue;
        }
        out.push(lines[i]);
        i += 1;
    }
    clean_text(&out.join("\n"))
}

// ---- rich text -----------------------------------------------------------------------------

/// Text nodes of a line, with web addresses as links.
fn inline_nodes(line: &str) -> Vec<Value> {
    let mut nodes = Vec::new();
    let mut rest = line;
    loop {
        let at = [rest.find("https://"), rest.find("http://")].into_iter().flatten().min();
        let Some(at) = at else { break };
        let len = rest[at..].find(|c: char| c.is_whitespace() || matches!(c, '<' | '>' | '"')).unwrap_or(rest.len() - at);
        let mut url = &rest[at..at + len];
        while url.ends_with(['.', ',', ';', ':', '!', '?', ')', ']', '\'']) {
            url = &url[..url.len() - 1];
        }
        if url.len() <= "https://".len() {
            nodes.push(json!({"type": "text", "text": &rest[..at + len]}));
            rest = &rest[at + len..];
            continue;
        }
        if at > 0 {
            nodes.push(json!({"type": "text", "text": &rest[..at]}));
        }
        nodes.push(json!({"type": "text", "text": url, "marks": [{"type": "link", "attrs": {"href": url}}]}));
        rest = &rest[at + url.len()..];
    }
    if !rest.is_empty() {
        nodes.push(json!({"type": "text", "text": rest}));
    }
    nodes
}

/// Text as a rich-text document: paragraphs at empty lines, line breaks kept, web addresses
/// linked.
pub fn text_to_doc(text: &str) -> Value {
    let mut content = Vec::new();
    for para in text.split("\n\n") {
        let mut inline: Vec<Value> = Vec::new();
        for (n, line) in para.lines().enumerate() {
            if n > 0 {
                inline.push(json!({"type": "hardBreak"}));
            }
            inline.extend(inline_nodes(line));
        }
        if !inline.is_empty() {
            content.push(json!({"type": "paragraph", "content": inline}));
        }
    }
    if content.is_empty() { crate::doc::empty() } else { json!({"type": "doc", "content": content}) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_and_subjects() {
        assert_eq!(addresses_in("\"Doe, Jane\" <Jane@Example.com>, bob@example.org"), vec!["jane@example.com", "bob@example.org"]);
        assert_eq!(addresses_in("<support+SUP@acme.com>"), vec!["support+sup@acme.com"]);
        assert!(addresses_in("undisclosed-recipients:;").is_empty());
        assert_eq!(topic("Re: AW: Fwd: Printer on fire"), "Printer on fire");
        assert_eq!(topic("RE[2]: WG: [SUP-12] Printer"), "[SUP-12] Printer");
        assert_eq!(topic("Reminder: renew"), "Reminder: renew");
        assert_eq!(topic("Re:"), "");
        assert_eq!(subject_keys("Re: [SUP-12] Printer [web-3] [not a key] [X-0]"), vec![("SUP".to_string(), 12), ("WEB".to_string(), 3)]);
        assert!(subject_keys("SUP-12 without brackets").is_empty());
        let id = Uuid::now_v7();
        let refs = vec![format!("tc.{id}.0a1b@acme.com"), "CAF=abc@mail.gmail.com".into(), format!("<tc.{id}@acme.com>"), "tc.nope@x".into()];
        assert_eq!(issue_ids(&refs), vec![id]);
        assert_eq!(one_line("a\r\nBcc: x\t y", 100), "a Bcc: x y");
    }

    #[test]
    fn html_becomes_text() {
        let html = r#"<html><head><style>p{color:red}</style><title>x</title></head><body>
            <p>Hello&nbsp;team,</p><p>the <b>printer</b> is on fire &amp; smells.<br>Second line</p>
            <ul><li>one</li><li>two</li></ul><ol><li>first</li><li>second</li></ol>
            <p>See <a href="https://example.com/a?b=1&amp;c=2">the log</a> and <a href="https://example.com/x">https://example.com/x</a>.</p>
            <script>alert(1)</script><!-- hidden --><div>Gr&uuml;&szlig;e, &#x4A;ane &#8212; ok</div>
            <blockquote type="cite"><p>quoted<br>more</p></blockquote><p>after</p></body></html>"#;
        let text = html_to_text(html);
        assert_eq!(
            text,
            "Hello team,\n\nthe printer is on fire & smells.\nSecond line\n\n- one\n- two\n1. first\n2. second\n\nSee the log (https://example.com/a?b=1&c=2) and https://example.com/x.\n\nGrüße, Jane — ok\n> quoted\n> more\n\nafter",
            "{text}"
        );
        assert!(!text.contains("alert") && !text.contains("color") && !text.contains("hidden"));
        assert_eq!(html_to_text("<p>a < b</p>"), "a < b");
        assert_eq!(html_to_text("<p>ü<b>x</b> ä</p><p>Zweiter</p>"), "üx ä\n\nZweiter");
        assert_eq!(html_to_text("<div>unclosed <a href=\"javascript:alert(1)\">link</a>"), "unclosed link", "only web links are spelled out");
        assert_eq!(decode_entities("&lt;script&gt; &unknown; & &#0;"), "<script> &unknown; & \u{0}");
    }

    #[test]
    fn replies_lose_quotes_and_signatures() {
        let gmail =
            "Thanks, that worked!\n\nOn Mon, Nov 12, 2026 at 10:00 AM Customer Support <\nsupport@acme.com> wrote:\n> We restarted it.\n>\n> -- \n> Mira";
        assert_eq!(strip_reply(gmail), "Thanks, that worked!");
        let german = "Danke!\n\nAm 12.11.2026 um 10:00 schrieb Mira Meyer <mira@acme.com>:\n> Hallo";
        assert_eq!(strip_reply(german), "Danke!");
        let outlook = "Still broken.\n\n________________________________\nFrom: Support <support@acme.com>\nSent: Monday, November 12, 2026 10:00\nTo: Jane\nSubject: [SUP-1] Printer\n\nWe restarted it.";
        assert_eq!(strip_reply(outlook), "Still broken.");
        let outlook_plain = "Ok.\n\nFrom: Support <support@acme.com>\nSent: Monday\nTo: Jane\n\nold";
        assert_eq!(strip_reply(outlook_plain), "Ok.");
        let original = "Yes.\n-----Original Message-----\nFrom: x";
        assert_eq!(strip_reply(original), "Yes.");
        let signature = "Fine now.\n\n-- \nJane Doe\nACME Corp";
        assert_eq!(strip_reply(signature), "Fine now.");
        let phone = "Ok\n\nSent from my iPhone";
        assert_eq!(strip_reply(phone), "Ok");
        let bottom = "Jane wrote:\n> question one\nanswer one\n> question two\nanswer two";
        assert_eq!(strip_reply(bottom), "answer one\nanswer two");
        // normal text that merely mentions these words stays
        let normal = "On Monday the printer broke.\nFrom: the second floor\nIt wrote garbage";
        assert_eq!(strip_reply(normal), normal);
    }

    #[test]
    fn text_to_rich_text() {
        let doc = text_to_doc("Hello,\nsecond line\n\nSee https://example.com/a. Thanks");
        assert_eq!(
            doc,
            json!({"type": "doc", "content": [
                {"type": "paragraph", "content": [{"type": "text", "text": "Hello,"}, {"type": "hardBreak"}, {"type": "text", "text": "second line"}]},
                {"type": "paragraph", "content": [{"type": "text", "text": "See "}, {"type": "text", "text": "https://example.com/a", "marks": [{"type": "link", "attrs": {"href": "https://example.com/a"}}]}, {"type": "text", "text": ". Thanks"}]}
            ]})
        );
        crate::doc::validate(&doc).unwrap();
        assert_eq!(crate::doc::text(&doc), "Hello,\nsecond line\nSee https://example.com/a. Thanks");
        assert_eq!(text_to_doc(""), crate::doc::empty());
    }

    const PLAIN: &str = "Return-Path: <jane@example.com>\r\nDelivered-To: support+sup@acme.com\r\nFrom: \"Jane Doe\" <Jane@Example.com>\r\nTo: Support <support@acme.com>\r\nCc: bob@example.com\r\nSubject: Re: [SUP-3] Printer =?utf-8?q?br=C3=BCllt?=\r\nMessage-ID: <abc123@example.com>\r\nIn-Reply-To: <tc.0190f0f0-0000-7000-8000-000000000001.x@acme.com>\r\nReferences: <first@example.com> <tc.0190f0f0-0000-7000-8000-000000000001.x@acme.com>\r\nContent-Type: text/plain; charset=utf-8; format=flowed\r\n\r\nIt is still \r\nbroken.\r\n\r\nOn Mon, Support wrote:\r\n> old\r\n";

    #[test]
    fn parse_plain_message() {
        let m = Inbound::parse(PLAIN.as_bytes()).unwrap();
        assert_eq!(m.message_id.as_deref(), Some("abc123@example.com"));
        assert_eq!(m.from.as_deref(), Some("jane@example.com"));
        assert_eq!(m.from_name.as_deref(), Some("Jane Doe"));
        assert_eq!(m.recipients, vec!["support@acme.com", "bob@example.com", "support+sup@acme.com"]);
        assert_eq!(m.subject, "Re: [SUP-3] Printer brüllt");
        assert_eq!(m.topic, "[SUP-3] Printer brüllt");
        assert_eq!(m.references, vec!["tc.0190f0f0-0000-7000-8000-000000000001.x@acme.com", "first@example.com"]);
        assert_eq!(m.automatic, None);
        assert_eq!(m.text, "It is still broken.\n\nOn Mon, Support wrote:\n> old");
        assert_eq!(strip_reply(&m.text), "It is still broken.");
        assert!(m.attachments.is_empty());
        assert!(Inbound::parse(b"").is_none());
    }

    #[test]
    fn machines_are_recognized() {
        let with = |h: &str| Inbound::parse(format!("From: a@example.com\r\nTo: s@acme.com\r\nSubject: x\r\n{h}\r\n\r\nbody").as_bytes()).unwrap().automatic;
        assert_eq!(with("Auto-Submitted: auto-replied"), Some("auto-reply"));
        assert_eq!(with("Auto-Submitted: no"), None);
        assert_eq!(with("X-Autoreply: yes"), Some("auto-reply"));
        assert_eq!(with("Precedence: bulk"), Some("bulk"));
        assert_eq!(with("Precedence: list"), Some("bulk"));
        assert_eq!(with("X-Failed-Recipients: x@y.z"), Some("bounce"));
        assert_eq!(with("Return-Path: <>"), Some("bounce"));
        assert_eq!(with("X-Mailer: x"), None);
        let bounce = Inbound::parse(b"From: MAILER-DAEMON@acme.com\r\nSubject: Undelivered\r\n\r\nx").unwrap();
        assert_eq!(bounce.automatic, Some("bounce"));
        let dmarc = Inbound::parse(b"Authentication-Results: mx.acme.com; dmarc=fail (p=reject) header.from=example.com\r\nAuthentication-Results: dmarc=pass\r\nFrom: a@example.com\r\n\r\nx").unwrap();
        assert!(dmarc.dmarc_fail);
    }

    #[test]
    fn html_mail_with_files() {
        let raw = "From: jane@example.com\r\nTo: support@acme.com\r\nSubject: Screenshot\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=\"b1\"\r\n\r\n--b1\r\nContent-Type: multipart/related; boundary=\"b2\"\r\n\r\n--b2\r\nContent-Type: text/html; charset=utf-8\r\n\r\n<p>See <b>attached</b>.</p><img src=\"cid:logo\">\r\n--b2\r\nContent-Type: image/png\r\nContent-ID: <logo>\r\nContent-Disposition: inline\r\nContent-Transfer-Encoding: base64\r\n\r\niVBORw0KGgo=\r\n--b2--\r\n--b1\r\nContent-Type: text/plain; name=\"../log.txt\"\r\nContent-Disposition: attachment; filename=\"../log.txt\"\r\n\r\nline 1\r\nline 2\r\n--b1\r\nContent-Type: application/pkcs7-signature; name=smime.p7s\r\nContent-Disposition: attachment; filename=smime.p7s\r\nContent-Transfer-Encoding: base64\r\n\r\nAAAA\r\n--b1--\r\n";
        let m = Inbound::parse(raw.as_bytes()).unwrap();
        assert_eq!(m.text, "See attached.");
        assert_eq!(m.attachments.len(), 1, "{:?}", m.attachments.iter().map(|a| &a.name).collect::<Vec<_>>());
        assert_eq!(m.attachments[0].name, "_log.txt");
        assert_eq!(m.attachments[0].mime, "text/plain");
        assert_eq!(String::from_utf8_lossy(&m.attachments[0].data).replace("\r\n", "\n").trim_end(), "line 1\nline 2");
    }
}
