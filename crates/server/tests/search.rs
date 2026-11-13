//! Search: stemmed words in the workspace's language, and the text inside files on issues.

mod common;

use std::io::Write;

use common::*;
use reqwest::Method;
use reqwest::multipart::{Form, Part};
use serde_json::{Value, json};

async fn keys(c: &reqwest::Client, app: &TestApp, tql: &str) -> Vec<String> {
    let r = c.get(app.url("/search")).query(&[("q", tql)]).send().await.unwrap();
    assert!(r.status().is_success(), "{tql}: {}", r.text().await.unwrap());
    let v: Value = r.json().await.unwrap();
    let mut keys: Vec<String> = v["issues"].as_array().unwrap().iter().map(|i| i["key"].as_str().unwrap().to_string()).collect();
    keys.sort();
    keys
}

/// Waits (up to ten seconds) until a search finds something: files are read in the background.
async fn eventually(c: &reqwest::Client, app: &TestApp, tql: &str) -> Vec<String> {
    for _ in 0..50 {
        let found = keys(c, app, tql).await;
        if !found.is_empty() {
            return found;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    vec![]
}

fn docx(text: &str) -> Vec<u8> {
    let mut out = std::io::Cursor::new(Vec::new());
    let mut z = zip::ZipWriter::new(&mut out);
    z.start_file("word/document.xml", zip::write::SimpleFileOptions::default()).unwrap();
    write!(z, r#"<w:document xmlns:w="w"><w:body><w:p><w:r><w:t>{text}</w:t></w:r></w:p></w:body></w:document>"#).unwrap();
    z.finish().unwrap();
    out.into_inner()
}

#[tokio::test]
async fn stems_and_files() {
    let app = spawn().await;
    let c = app.setup(false).await;
    post(&c, app.url("/projects"), json!({"key": "SRC", "name": "Search", "template": "kanban"})).await;
    for title in ["Running reports takes long", "Report export", "Die Häuser am See"] {
        post(&c, app.url("/issues"), json!({"projectKey": "SRC", "title": title})).await;
    }
    // English by default: "runs" finds "running" by its stem, prefixes still work
    assert_eq!(keys(&c, &app, "text ~ runs").await, ["SRC-1"]);
    assert_eq!(keys(&c, &app, "text ~ repo").await, ["SRC-1", "SRC-2"]);
    assert_eq!(keys(&c, &app, "text ~ Haus").await, Vec::<String>::new());

    // German: "Haus" finds "Häuser" once the issues are indexed again (in the background)
    assert!(send(&c, Method::PUT, app.url("/admin/workspace"), json!({"defaultLanguage": "de"})).await.0 < 300);
    assert_eq!(eventually(&c, &app, "text ~ Haus").await, ["SRC-3"]);
    // new issues take the workspace's language
    post(&c, app.url("/issues"), json!({"projectKey": "SRC", "title": "Neue Rechnungen"})).await;
    assert_eq!(keys(&c, &app, "text ~ Rechnung").await, ["SRC-4"]);

    // the text inside a file finds its issue, and so does the file's name
    let form = Form::new().part("file", Part::bytes(docx("Quartalsprognose und Garantiefall")).file_name("Bericht.docx"));
    let r = c.post(app.url("/issues/SRC-2/attachments")).multipart(form).send().await.unwrap();
    assert_eq!(r.status(), 201);
    assert_eq!(eventually(&c, &app, "text ~ Garantiefall").await, ["SRC-2"]);
    assert_eq!(keys(&c, &app, "text ~ bericht").await, ["SRC-2"]);
    let state: (Option<String>, String) = sqlx::query_as("SELECT text_content, search_lang::text FROM attachments").fetch_one(&app.state.db).await.unwrap();
    assert_eq!(state, (Some("Quartalsprognose und Garantiefall".into()), "german".into()));
    // files that can't be read count as read (no retries)
    let form = Form::new().part("file", Part::bytes(b"\x89PNG\r\n\x1a\n....".to_vec()).file_name("shot.png"));
    assert_eq!(c.post(app.url("/issues/SRC-1/attachments")).multipart(form).send().await.unwrap().status(), 201);
    for _ in 0..50 {
        let unread: i64 = sqlx::query_scalar("SELECT count(*) FROM attachments WHERE text_content IS NULL").fetch_one(&app.state.db).await.unwrap();
        if unread == 0 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    assert!(!taskcraft::extract::index_one(&app.state, None).await.unwrap(), "nothing left to read");

    // files that came in otherwise (imports, a restored backup) are read by the minute's tick
    sqlx::query("UPDATE attachments SET text_content = NULL").execute(&app.state.db).await.unwrap();
    assert_eq!(taskcraft::extract::index_pending(&app.state).await.unwrap(), 2);
    assert_eq!(keys(&c, &app, "text ~ Quartalsprognose").await, ["SRC-2"]);
}
