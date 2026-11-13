//! What may be uploaded: blocked file types, a size limit, and virus scanning (clamd).

mod common;

use common::*;
use reqwest::Method;
use reqwest::multipart::{Form, Part};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// A stand-in for clamd: reads an INSTREAM and finds "EICAR" in it.
async fn fake_clamd() -> String {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap().to_string();
    tokio::spawn(async move {
        while let Ok((mut s, _)) = l.accept().await {
            tokio::spawn(async move {
                let mut cmd = [0u8; 10];
                s.read_exact(&mut cmd).await.unwrap();
                assert_eq!(&cmd, b"zINSTREAM\0");
                let mut data = Vec::new();
                loop {
                    let mut len = [0u8; 4];
                    s.read_exact(&mut len).await.unwrap();
                    let n = u32::from_be_bytes(len) as usize;
                    if n == 0 {
                        break;
                    }
                    let mut chunk = vec![0u8; n];
                    s.read_exact(&mut chunk).await.unwrap();
                    data.extend(chunk);
                }
                let answer: &[u8] = if data.windows(5).any(|w| w == b"EICAR") { b"stream: Eicar-Test-Signature FOUND\0" } else { b"stream: OK\0" };
                s.write_all(answer).await.unwrap();
            });
        }
    });
    addr
}

async fn upload(c: &reqwest::Client, app: &TestApp, key: &str, name: &str, data: &[u8]) -> (u16, String) {
    let form = Form::new().part("file", Part::bytes(data.to_vec()).file_name(name.to_string()));
    let res = c.post(app.url(&format!("/issues/{key}/attachments"))).header("x-taskcraft", "1").multipart(form).send().await.unwrap();
    (res.status().as_u16(), res.text().await.unwrap())
}

#[tokio::test]
async fn uploads_are_checked_and_scanned() {
    let clamd = fake_clamd().await;
    let app = spawn_with(move |c| c.clamav_address = Some(clamd)).await;
    let admin = app.setup(false).await;
    post(&admin, app.url("/projects"), json!({"key": "UPL", "name": "Uploads", "template": "kanban"})).await;
    let key = post(&admin, app.url("/issues"), json!({"projectKey": "UPL", "title": "Files"})).await["key"].as_str().unwrap().to_string();

    assert_eq!(upload(&admin, &app, &key, "notes.txt", b"hello").await.0, 201);
    let (s, body) = upload(&admin, &app, &key, "eicar.txt", b"X5O!P%@AP[4\\PZX54(P^)7CC)7}$EICAR-STANDARD-ANTIVIRUS-TEST-FILE!$H+H*").await;
    assert_eq!(s, 400);
    assert!(body.contains("looks infected (Eicar-Test-Signature)"), "{body}");
    // all or none: a clean file next to an infected one isn't stored either
    let form = Form::new().part("file", Part::bytes(b"fine".to_vec()).file_name("fine.txt")).part("file", Part::bytes(b"EICAR".to_vec()).file_name("bad.txt"));
    let res = admin.post(app.url(&format!("/issues/{key}/attachments"))).header("x-taskcraft", "1").multipart(form).send().await.unwrap();
    assert_eq!(res.status().as_u16(), 400);
    let audit = get(&admin, app.url("/admin/audit")).await.to_string();
    assert!(audit.contains("file.infected"), "{audit}");

    // the workspace's rules
    assert_eq!(send(&admin, Method::PUT, app.url("/admin/workspace"), json!({"uploads": {"maxMb": 100000}})).await.0, 400);
    assert!(send(&admin, Method::PUT, app.url("/admin/workspace"), json!({"uploads": {"blockedTypes": ["EXE", ".bat"], "maxMb": 1}})).await.0 < 300);
    let (s, body) = upload(&admin, &app, &key, "setup.exe", b"MZ").await;
    assert_eq!(s, 400, "{body}");
    assert!(body.contains(".exe files can't be uploaded here"), "{body}");
    let (s, body) = upload(&admin, &app, &key, "big.bin", &vec![0u8; 1024 * 1024 + 1]).await;
    assert_eq!(s, 400, "{body}");
    let files = get(&admin, app.url(&format!("/issues/{key}"))).await["attachments"].as_array().unwrap().len();
    assert_eq!(files, 1, "only the clean one is stored");
}

/// A scanner that's broken: it hangs up without an answer.
async fn broken_clamd() -> String {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap().to_string();
    tokio::spawn(async move {
        while let Ok((s, _)) = l.accept().await {
            drop(s)
        }
    });
    addr
}

#[tokio::test]
async fn without_a_scanner_files_wait_unless_told_otherwise() {
    let clamd = broken_clamd().await;
    let app = spawn_with({
        let clamd = clamd.clone();
        move |c| c.clamav_address = Some(clamd)
    })
    .await;
    let admin = app.setup(false).await;
    post(&admin, app.url("/projects"), json!({"key": "UPL", "name": "Uploads", "template": "kanban"})).await;
    let key = post(&admin, app.url("/issues"), json!({"projectKey": "UPL", "title": "Files"})).await["key"].as_str().unwrap().to_string();
    let (s, body) = upload(&admin, &app, &key, "notes.txt", b"hello").await;
    assert_eq!(s, 400);
    assert!(body.contains("virus scanner can't be reached"), "{body}");

    let app = spawn_with(move |c| {
        c.clamav_address = Some(clamd);
        c.clamav_fail_open = true;
    })
    .await;
    let admin = app.setup(false).await;
    post(&admin, app.url("/projects"), json!({"key": "UPL", "name": "Uploads", "template": "kanban"})).await;
    let key = post(&admin, app.url("/issues"), json!({"projectKey": "UPL", "title": "Files"})).await["key"].as_str().unwrap().to_string();
    assert_eq!(upload(&admin, &app, &key, "notes.txt", b"hello").await.0, 201, "fail open");
}
