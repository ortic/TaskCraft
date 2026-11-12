//! `taskcraft backup` / `restore`: a workspace with attachments moves into another instance.

mod common;

use std::io::Write;

use common::*;
use serde_json::{Value, json};

async fn upload(app: &TestApp, c: &reqwest::Client, key: &str, name: &str, body: &[u8]) -> Value {
    let part = reqwest::multipart::Part::bytes(body.to_vec()).file_name(name.to_string()).mime_str("text/plain").unwrap();
    let r = c.post(app.url(&format!("/issues/{key}/attachments"))).multipart(reqwest::multipart::Form::new().part("file", part)).send().await.unwrap();
    assert!(r.status().is_success(), "{}", r.status());
    r.json().await.unwrap()
}

async fn login(app: &TestApp) -> reqwest::Client {
    let c = client();
    let r = c.post(app.url("/auth/login")).json(&json!({"email": "owner@example.com", "password": "password123"})).send().await.unwrap();
    assert_eq!(r.status(), 200, "{}", r.text().await.unwrap());
    c
}

#[tokio::test(flavor = "multi_thread")]
async fn backup_and_restore_into_another_instance() {
    let app = spawn().await;
    let c = app.setup(true).await;
    upload(&app, &c, "WEB-1", "notes.txt", b"line one\nline two\ttabbed \\ backslash").await;
    let comment = "Multi-line\ncomment with \"quotes\", a tab\tand ünïcödé";
    post(&c, app.url("/issues/WEB-1/comments"), json!({"markdown": comment})).await;
    let issues_before = get(&c, app.url("/search?q=order%20by%20key&limit=500")).await["total"].clone();

    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("backup.tar.gz");
    let s = taskcraft::backup::backup(&app.state.db, app.state.blobs.as_ref(), &file, true).await.unwrap();
    assert_eq!(s.files, 1);
    assert!(s.rows > 100, "{s}");
    let manifest = taskcraft::backup::inspect(&file).unwrap();
    let order: Vec<&str> = manifest.tables.iter().map(|t| t.name.as_str()).collect();
    let pos = |n: &str| order.iter().position(|t| *t == n).unwrap_or_else(|| panic!("{n} missing from {order:?}"));
    assert!(pos("workspaces") < pos("users") && pos("users") < pos("projects") && pos("projects") < pos("issues") && pos("issues") < pos("comments"));
    assert!(!order.contains(&"_sqlx_migrations"));
    assert!(manifest.tables.iter().find(|t| t.name == "issues").unwrap().rows > 20);

    // into a second instance (its own database and attachment directory), still empty
    let other = spawn().await;
    let restored = taskcraft::backup::restore(&other.state.config, &file, false).await.unwrap();
    assert_eq!((restored.rows, restored.files, restored.tables), (s.rows, 1, s.tables));
    let c2 = login(&other).await;
    assert_eq!(get(&c2, other.url("/search?q=order%20by%20key&limit=500")).await["total"], issues_before);
    let issue = get(&c2, other.url("/issues/WEB-1")).await;
    let att = &issue["attachments"][0];
    assert_eq!(att["name"], "notes.txt");
    let body = c2.get(format!("{}{}", other.base, att["url"].as_str().unwrap())).send().await.unwrap().bytes().await.unwrap();
    assert_eq!(&body[..], b"line one\nline two\ttabbed \\ backslash");
    let comments = get(&c2, other.url("/issues/WEB-1/comments")).await;
    assert!(comments.to_string().contains("ünïcödé"), "{comments}");

    // a database with a workspace is only replaced on request
    let err = taskcraft::backup::restore(&other.state.config, &file, false).await.unwrap_err();
    assert!(err.to_string().contains("--force"), "{err}");
    post(&c2, other.url("/projects"), json!({"key": "NEW", "name": "Made after the restore", "template": "kanban"})).await;
    taskcraft::backup::restore(&other.state.config, &file, true).await.unwrap();
    let c3 = login(&other).await;
    let projects = get(&c3, other.url("/projects")).await;
    assert!(!projects.to_string().contains("Made after the restore"), "--force replaced everything");

    // backups from a newer version are refused before anything is touched
    let newer = dir.path().join("newer.tar.gz");
    let mut m: Value = serde_json::to_value(&manifest).unwrap();
    m["migrations"].as_array_mut().unwrap().push(json!({"version": 99_999_999, "description": "future", "checksum": "00"}));
    let json = serde_json::to_vec(&m).unwrap();
    let mut tar = tar::Builder::new(flate2::write::GzEncoder::new(std::fs::File::create(&newer).unwrap(), flate2::Compression::fast()));
    let mut h = tar::Header::new_gnu();
    h.set_size(json.len() as u64);
    h.set_mode(0o600);
    tar.append_data(&mut h, "manifest.json", json.as_slice()).unwrap();
    tar.into_inner().unwrap().finish().unwrap().flush().unwrap();
    let err = taskcraft::backup::restore(&other.state.config, &newer, true).await.unwrap_err();
    assert!(err.to_string().contains("newer TaskCraft"), "{err}");
    assert_eq!(get(&c3, other.url("/search?q=order%20by%20key&limit=500")).await["total"], issues_before, "nothing was touched");
}
