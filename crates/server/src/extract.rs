//! The text inside files, for search: plain text and source files, HTML, PDF, Word / Excel /
//! PowerPoint (Office Open XML) and OpenDocument files. Other files (images, archives, …) have
//! none. At most [`MAX_CHARS`] characters are kept.
//!
//! Files on issues are read in the background: right after an upload, and once a minute for
//! files that came in otherwise (imports, a restored backup) — see [`index_pending`].

use std::io::{Cursor, Read};

use object_store::ObjectStoreExt;
use quick_xml::Reader;
use quick_xml::events::Event;
use uuid::Uuid;

use crate::app::AppState;

/// How much of a file's text is kept for search.
pub const MAX_CHARS: usize = 100_000;
/// Larger files aren't read (their text would be cut anyway, and reading takes long).
const MAX_BYTES: usize = 64 * 1024 * 1024;

const TEXT: &[&str] = &[
    "txt",
    "text",
    "md",
    "markdown",
    "csv",
    "tsv",
    "log",
    "json",
    "xml",
    "yaml",
    "yml",
    "ini",
    "conf",
    "cfg",
    "toml",
    "sql",
    "rs",
    "py",
    "js",
    "ts",
    "tsx",
    "jsx",
    "java",
    "kt",
    "c",
    "h",
    "cpp",
    "hpp",
    "cs",
    "go",
    "rb",
    "php",
    "sh",
    "ps1",
    "bat",
    "css",
    "scss",
    "swift",
    "eml",
    "ics",
    "vcf",
    "properties",
    "env",
    "diff",
    "patch",
    "srt",
    "vtt",
    "tex",
];

/// The text of a file, or None when it has none (or can't be read).
pub fn text_of(name: &str, mime: &str, data: &[u8]) -> Option<String> {
    if data.is_empty() || data.len() > MAX_BYTES {
        return None;
    }
    let ending = name.rsplit_once('.').map(|(_, e)| e.to_lowercase()).unwrap_or_default();
    let text = match ending.as_str() {
        "pdf" => pdf(data),
        "docx" | "docm" | "dotx" => office(data, |n| n == "word/document.xml" || n.starts_with("word/header") || n.starts_with("word/footer")),
        "xlsx" | "xlsm" => office(data, |n| n == "xl/sharedStrings.xml"),
        "pptx" | "pptm" => office(data, |n| n.starts_with("ppt/slides/slide") || n.starts_with("ppt/notesSlides/")),
        "odt" | "ods" | "odp" | "odg" => office(data, |n| n == "content.xml"),
        "html" | "htm" | "xhtml" => plain(data).map(|t| strip_tags(&t)),
        "svg" => None,
        e if TEXT.contains(&e) => plain(data),
        _ if mime.starts_with("text/") => plain(data),
        _ => None,
    }?;
    let text = squeeze(&text);
    (!text.is_empty()).then(|| text.chars().take(MAX_CHARS).collect())
}

/// UTF-8 (or Latin-1) text; None for binary data.
fn plain(data: &[u8]) -> Option<String> {
    let sample = &data[..data.len().min(8192)];
    let control = sample.iter().filter(|b| **b == 0 || (**b < 9) || (**b > 13 && **b < 32)).count();
    if control * 20 > sample.len() {
        return None;
    }
    Some(match std::str::from_utf8(data) {
        Ok(s) => s.trim_start_matches('\u{feff}').to_string(),
        Err(_) => data.iter().map(|b| *b as char).collect(),
    })
}

fn pdf(data: &[u8]) -> Option<String> {
    // the PDF reader panics on some files: a broken file has no text, the server carries on
    let data = data.to_vec();
    std::panic::catch_unwind(move || pdf_extract::extract_text_from_mem(&data).ok()).ok().flatten()
}

/// The text of the XML parts of a ZIP-based document whose names `wanted` picks, in order.
fn office(data: &[u8], wanted: impl Fn(&str) -> bool) -> Option<String> {
    let mut zip = zip::ZipArchive::new(Cursor::new(data)).ok()?;
    let mut names: Vec<String> = (0..zip.len()).filter_map(|i| zip.name_for_index(i)?.ok().map(|n| n.into_owned())).filter(|n| wanted(n)).collect();
    // slide2 before slide10
    names.sort_by_key(|n| {
        let digits: String = n.chars().filter(char::is_ascii_digit).collect();
        (n.chars().filter(|c| !c.is_ascii_digit()).collect::<String>(), digits.parse::<u64>().unwrap_or(0))
    });
    let mut out = String::new();
    for n in names {
        let mut part = zip.by_name(&n).ok()?;
        if part.size() > MAX_BYTES as u64 {
            continue;
        }
        let mut xml = Vec::new();
        part.read_to_end(&mut xml).ok()?;
        xml_text(&xml, &mut out);
        out.push('\n');
        if out.len() > MAX_CHARS * 4 {
            break;
        }
    }
    Some(out)
}

/// The text of an XML document; paragraphs, rows and cells end with a line break or a tab.
fn xml_text(xml: &[u8], out: &mut String) {
    let mut reader = Reader::from_reader(xml);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match reader.read_event_into(&mut buf) {
            Ok(Event::Text(t)) => out.push_str(&t.xml10_content().map(|c| c.into_owned()).unwrap_or_else(|_| String::from_utf8_lossy(&t).into_owned())),
            Ok(Event::CData(t)) => out.push_str(&String::from_utf8_lossy(&t)),
            Ok(Event::GeneralRef(r)) => match r.resolve_char_ref() {
                Ok(Some(c)) => out.push(c),
                _ => {
                    if let Some(s) = quick_xml::escape::resolve_predefined_entity(&String::from_utf8_lossy(&r)) {
                        out.push_str(s);
                    }
                }
            },
            Ok(Event::End(e)) => match e.local_name().as_ref() {
                b"p" | b"si" | b"tr" | b"row" | b"h" | b"br" => out.push('\n'),
                b"tc" | b"c" | b"table-cell" => out.push('\t'),
                _ => {}
            },
            Ok(Event::Empty(e)) => match e.local_name().as_ref() {
                b"br" | b"cr" | b"line-break" => out.push('\n'),
                b"tab" | b"s" => out.push(' '),
                _ => {}
            },
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
}

/// HTML without tags, scripts and styles.
fn strip_tags(html: &str) -> String {
    let mut out = String::with_capacity(html.len() / 2);
    // ASCII only: the same byte offsets as the original
    let lower = html.to_ascii_lowercase();
    let mut i = 0;
    let bytes = html.as_bytes();
    while i < bytes.len() {
        if bytes[i] == b'<' {
            for skip in ["script", "style"] {
                if lower[i + 1..].starts_with(skip)
                    && let Some(end) = lower[i..].find(&format!("</{skip}"))
                {
                    i += end;
                }
            }
            match html[i..].find('>') {
                Some(end) => {
                    out.push(' ');
                    i += end + 1;
                }
                None => break,
            }
        } else {
            let next = html[i..].find('<').map(|n| i + n).unwrap_or(html.len());
            out.push_str(&html[i..next].replace("&nbsp;", " ").replace("&amp;", "&").replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\""));
            i = next;
        }
    }
    out
}

/// Runs of blanks as one space; at most two line breaks in a row.
fn squeeze(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let (mut blank, mut breaks) = (false, 0);
    for c in text.chars() {
        if c == '\n' {
            breaks += 1;
            blank = false;
            if breaks <= 2 {
                out.push('\n');
            }
        } else if c.is_whitespace() || c.is_control() {
            blank = true;
        } else {
            if blank && breaks == 0 && !out.is_empty() {
                out.push(' ');
            }
            blank = false;
            breaks = 0;
            out.push(c);
        }
    }
    out.trim().to_string()
}

/// Reads one file's text (None: the next one waiting), if no other replica is at it. Returns
/// whether a file was read.
pub async fn index_one(state: &AppState, id: Option<Uuid>) -> anyhow::Result<bool> {
    let mut tx = state.db.begin().await?;
    let row: Option<(Uuid, String, String, String, Uuid)> = sqlx::query_as(
        "SELECT a.id, a.name, a.mime, a.storage_key, i.workspace_id FROM attachments a JOIN issues i ON i.id = a.issue_id
          WHERE a.text_content IS NULL AND ($1::uuid IS NULL OR a.id = $1) ORDER BY a.created_at DESC LIMIT 1 FOR UPDATE OF a SKIP LOCKED",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((id, name, mime, key, workspace)) = row else { return Ok(false) };
    let text = match state.blobs.get(&object_store::path::Path::from(key)).await {
        Ok(got) => {
            let bytes = got.bytes().await?;
            tokio::task::spawn_blocking(move || text_of(&name, &mime, &bytes)).await.ok().flatten()
        }
        Err(e) => {
            tracing::warn!(attachment = %id, error = %e, "reading a file for search: not in storage");
            None
        }
    };
    sqlx::query("UPDATE attachments SET text_content = $2, search_lang = tc_search_lang($3) WHERE id = $1")
        .bind(id)
        .bind(text.unwrap_or_default())
        .bind(workspace)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(true)
}

/// Reads a newly stored file in the background.
pub fn index_soon(state: &AppState, id: Uuid) {
    let state = state.clone();
    tokio::spawn(async move {
        if let Err(e) = index_one(&state, Some(id)).await {
            tracing::warn!(attachment = %id, error = %e, "reading a file for search");
        }
    });
}

/// Files not read yet (imported, restored, or uploaded while the server stopped): a batch per
/// minute.
pub async fn index_pending(state: &AppState) -> anyhow::Result<usize> {
    let started = std::time::Instant::now();
    let mut n = 0;
    while started.elapsed() < std::time::Duration::from_secs(30) && index_one(state, None).await? {
        n += 1;
    }
    Ok(n)
}

/// Issues and files indexed again in the workspace's (new) language, in the background.
pub fn set_language_soon(state: &AppState, workspace: Uuid) {
    let state = state.clone();
    tokio::spawn(async move {
        let issues =
            sqlx::query("UPDATE issues SET search_lang = tc_search_lang($1) WHERE workspace_id = $1 AND search_lang IS DISTINCT FROM tc_search_lang($1)")
                .bind(workspace)
                .execute(&state.db);
        if let Err(e) = issues.await {
            tracing::warn!(error = %e, "search language of issues");
        }
        let files = sqlx::query(
            "UPDATE attachments a SET search_lang = tc_search_lang($1) FROM issues i
              WHERE i.id = a.issue_id AND i.workspace_id = $1 AND a.text_content IS NOT NULL AND a.search_lang IS DISTINCT FROM tc_search_lang($1)",
        )
        .bind(workspace)
        .execute(&state.db);
        if let Err(e) = files.await {
            tracing::warn!(error = %e, "search language of files");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn zip_of(parts: &[(&str, &str)]) -> Vec<u8> {
        let mut out = Cursor::new(Vec::new());
        let mut z = zip::ZipWriter::new(&mut out);
        for (name, body) in parts {
            z.start_file(*name, zip::write::SimpleFileOptions::default()).unwrap();
            z.write_all(body.as_bytes()).unwrap();
        }
        z.finish().unwrap();
        out.into_inner()
    }

    /// A one-page PDF with a line of text in Helvetica.
    pub fn tiny_pdf(text: &str) -> Vec<u8> {
        let content = format!("BT /F1 24 Tf 72 700 Td ({text}) Tj ET");
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>".to_string(),
            format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>".to_string(),
        ];
        let mut out = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for (i, o) in objects.iter().enumerate() {
            offsets.push(out.len());
            out.extend(format!("{} 0 obj\n{o}\nendobj\n", i + 1).as_bytes());
        }
        let xref = out.len();
        out.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes());
        for o in offsets {
            out.extend(format!("{o:010} 00000 n \n").as_bytes());
        }
        out.extend(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objects.len() + 1).as_bytes());
        out
    }

    #[test]
    fn reads_text_out_of_files() {
        assert_eq!(text_of("claim.pdf", "application/pdf", &tiny_pdf("Warranty claim approved")).unwrap(), "Warranty claim approved");
        assert_eq!(text_of("notes.txt", "text/plain", b"Hello   world\n\n\n\nbye").unwrap(), "Hello world\n\nbye");
        assert_eq!(text_of("x.bin", "application/octet-stream", b"\0\0\0\x01binary"), None);
        assert_eq!(text_of("photo.png", "image/png", b"\x89PNG...."), None);
        assert_eq!(text_of("page.html", "text/html", b"<html><style>p{}</style><p>Refund &amp; tax</p><script>x()</script></html>").unwrap(), "Refund & tax");
        let docx = zip_of(&[(
            "word/document.xml",
            r#"<w:document xmlns:w="w"><w:body><w:p><w:r><w:t>Invoice</w:t></w:r><w:r><w:t xml:space="preserve"> number 42</w:t></w:r></w:p><w:p><w:r><w:t>Paid &amp; closed</w:t></w:r></w:p></w:body></w:document>"#,
        )]);
        assert_eq!(text_of("Bill.DOCX", "", &docx).unwrap(), "Invoice number 42\nPaid & closed");
        let xlsx = zip_of(&[("xl/sharedStrings.xml", r#"<sst><si><t>Customer</t></si><si><t>Zürich</t></si></sst>"#)]);
        assert_eq!(text_of("list.xlsx", "", &xlsx).unwrap(), "Customer\nZürich");
        let pptx = zip_of(&[
            ("ppt/slides/slide10.xml", r#"<p:sld><a:p><a:t>Ten</a:t></a:p></p:sld>"#),
            ("ppt/slides/slide2.xml", r#"<p:sld><a:p><a:t>Two</a:t></a:p></p:sld>"#),
        ]);
        assert_eq!(text_of("deck.pptx", "", &pptx).unwrap(), "Two\n\nTen");
        let odt = zip_of(&[("content.xml", r#"<office:document-content><text:p>Offline<text:s/>mode</text:p></office:document-content>"#)]);
        assert_eq!(text_of("spec.odt", "", &odt).unwrap(), "Offline mode");
        assert_eq!(text_of("broken.docx", "", b"not a zip"), None);
        assert_eq!(text_of("broken.pdf", "application/pdf", b"%PDF-1.4 garbage"), None);
        let long = "word ".repeat(MAX_CHARS);
        assert_eq!(text_of("long.txt", "", long.as_bytes()).unwrap().chars().count(), MAX_CHARS);
    }
}
