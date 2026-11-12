//! E-mail: invitations, password resets and notification e-mails, delivered over SMTP to a tiny
//! in-process mail sink.

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine;
use common::*;
use reqwest::Method;
use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

// ---- SMTP sink ------------------------------------------------------------------------------

/// A received e-mail, decoded.
#[derive(Debug, Clone)]
struct Received {
    to: Vec<String>,
    subject: String,
    text: String,
    html: String,
}

#[derive(Clone)]
struct Sink {
    url: String,
    mails: Arc<Mutex<Vec<Received>>>,
}

impl Sink {
    /// Accepts SMTP on an ephemeral port and keeps every message.
    async fn start() -> Sink {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("smtp://{}", listener.local_addr().unwrap());
        let mails = Arc::new(Mutex::new(Vec::new()));
        let keep = mails.clone();
        tokio::spawn(async move {
            while let Ok((sock, _)) = listener.accept().await {
                let keep = keep.clone();
                tokio::spawn(async move {
                    let _ = session(sock, keep).await;
                });
            }
        });
        Sink { url, mails }
    }

    fn all(&self) -> Vec<Received> {
        self.mails.lock().unwrap().clone()
    }

    fn to(&self, addr: &str) -> Vec<Received> {
        self.all().into_iter().filter(|m| m.to.iter().any(|t| t == addr)).collect()
    }

    /// Waits for an e-mail to `addr` whose subject contains `subject`.
    async fn wait(&self, addr: &str, subject: &str) -> Received {
        for _ in 0..100 {
            if let Some(m) = self.to(addr).into_iter().rev().find(|m| m.subject.contains(subject)) {
                return m;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("no e-mail to {addr} about {subject:?}; got {:#?}", self.all().iter().map(|m| (&m.to, &m.subject)).collect::<Vec<_>>());
    }
}

async fn session(sock: tokio::net::TcpStream, mails: Arc<Mutex<Vec<Received>>>) -> std::io::Result<()> {
    let (r, mut w) = sock.into_split();
    let mut r = BufReader::new(r);
    w.write_all(b"220 sink ESMTP ready\r\n").await?;
    let mut to: Vec<String> = Vec::new();
    let mut line = String::new();
    loop {
        line.clear();
        if r.read_line(&mut line).await? == 0 {
            return Ok(());
        }
        let cmd = line.trim_end().to_string();
        let upper = cmd.to_ascii_uppercase();
        if upper.starts_with("EHLO") {
            w.write_all(b"250-sink\r\n250-8BITMIME\r\n250 SMTPUTF8\r\n").await?;
        } else if upper.starts_with("RCPT TO:") {
            let addr = cmd[8..].trim().trim_start_matches('<');
            to.push(addr[..addr.find('>').unwrap_or(addr.len())].to_string());
            w.write_all(b"250 ok\r\n").await?;
        } else if upper == "DATA" {
            w.write_all(b"354 go ahead\r\n").await?;
            let mut data = String::new();
            loop {
                line.clear();
                if r.read_line(&mut line).await? == 0 {
                    return Ok(());
                }
                if line == ".\r\n" || line == ".\n" {
                    break;
                }
                data.push_str(line.strip_prefix('.').unwrap_or(&line));
            }
            mails.lock().unwrap().push(parse(std::mem::take(&mut to), &data));
            w.write_all(b"250 queued\r\n").await?;
        } else if upper == "QUIT" {
            w.write_all(b"221 bye\r\n").await?;
            return Ok(());
        } else {
            if upper == "RSET" || upper.starts_with("MAIL FROM:") {
                to.clear();
            }
            w.write_all(b"250 ok\r\n").await?;
        }
    }
}

/// Headers (unfolded) and body of a MIME entity.
fn split(raw: &str) -> (Vec<(String, String)>, String) {
    let raw = raw.replace("\r\n", "\n");
    let (head, body) = raw.split_once("\n\n").unwrap_or((&raw, ""));
    let mut headers: Vec<(String, String)> = Vec::new();
    for l in head.lines() {
        if l.starts_with([' ', '\t']) {
            if let Some(last) = headers.last_mut() {
                last.1.push(' ');
                last.1.push_str(l.trim());
            }
        } else if let Some((k, v)) = l.split_once(':') {
            headers.push((k.trim().to_ascii_lowercase(), v.trim().to_string()));
        }
    }
    (headers, body.to_string())
}

fn header<'a>(h: &'a [(String, String)], name: &str) -> &'a str {
    h.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str()).unwrap_or("")
}

fn qp(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'=' && i + 1 < b.len() && b[i + 1] == b'\n' {
            i += 2;
        } else if b[i] == b'=' && i + 2 < b.len() && u8::from_str_radix(&s[i + 1..i + 3], 16).is_ok() {
            out.push(u8::from_str_radix(&s[i + 1..i + 3], 16).unwrap());
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    out
}

fn decode_body(headers: &[(String, String)], body: &str) -> String {
    let bytes = match header(headers, "content-transfer-encoding").to_ascii_lowercase().as_str() {
        "base64" => base64::engine::general_purpose::STANDARD.decode(body.split_whitespace().collect::<String>()).unwrap(),
        "quoted-printable" => qp(body),
        _ => body.as_bytes().to_vec(),
    };
    String::from_utf8(bytes).unwrap().replace("\r\n", "\n")
}

/// RFC 2047 encoded words (`=?utf-8?b?…?=`), as lettre writes non-ASCII subjects.
fn decode_words(s: &str) -> String {
    let mut out: Vec<u8> = Vec::new();
    let mut rest = s;
    let mut after_word = false;
    while let Some(i) = rest.find("=?") {
        let word = rest[i + 2..].splitn(4, '?').collect::<Vec<_>>();
        let parsed = (word.len() == 4 && word[3].starts_with('=')).then(|| match word[1].to_ascii_lowercase().as_str() {
            "b" => base64::engine::general_purpose::STANDARD.decode(word[2]).ok(),
            "q" => Some(qp(&word[2].replace('_', " "))),
            _ => None,
        });
        let before = &rest[..i];
        match parsed.flatten() {
            Some(bytes) => {
                if !(after_word && before.trim().is_empty()) {
                    out.extend_from_slice(before.as_bytes());
                }
                out.extend(bytes);
                rest = &rest[i + 2 + word[0].len() + word[1].len() + word[2].len() + 4..];
                after_word = true;
            }
            None => {
                out.extend_from_slice(&rest.as_bytes()[..i + 2]);
                rest = &rest[i + 2..];
                after_word = false;
            }
        }
    }
    out.extend_from_slice(rest.as_bytes());
    String::from_utf8(out).unwrap()
}

fn parse(to: Vec<String>, raw: &str) -> Received {
    let (headers, body) = split(raw);
    let subject = decode_words(header(&headers, "subject"));
    let ctype = header(&headers, "content-type").to_string();
    let (mut text, mut html) = (String::new(), String::new());
    if let Some(b) = ctype.split("boundary=").nth(1) {
        let boundary = format!("--{}", b.split(';').next().unwrap().trim().trim_matches('"'));
        for part in body.replace("\r\n", "\n").split(&boundary).skip(1) {
            let part = part.trim_start_matches('\n');
            if part.starts_with("--") {
                break;
            }
            let (h, b) = split(part);
            let decoded = decode_body(&h, b.trim_end_matches('\n'));
            if header(&h, "content-type").starts_with("text/html") {
                html = decoded;
            } else if header(&h, "content-type").starts_with("text/plain") {
                text = decoded;
            }
        }
    } else {
        text = decode_body(&headers, &body);
    }
    Received { to, subject, text, html }
}

/// The token of a `/welcome?token=` link in an e-mail.
fn token_in(text: &str) -> String {
    let at = text.find("/welcome?token=").unwrap_or_else(|| panic!("no link in {text}")) + "/welcome?token=".len();
    text[at..].chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').collect()
}

async fn with_mail() -> (TestApp, Sink) {
    let sink = Sink::start().await;
    let url = sink.url.clone();
    let app = spawn_with(|c| c.smtp_url = Some(url)).await;
    (app, sink)
}

// ---- tests ----------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn invitations_password_reset_and_admin_test_mail() {
    let (app, sink) = with_mail().await;
    taskcraft::mail::spawn_worker(&app.state);
    assert_eq!(get(&client(), app.url("/setup")).await["mail"], true);
    let owner = app.setup(false).await;

    // admin: status and a test e-mail (sent right away)
    let status = get(&owner, app.url("/admin/mail")).await;
    assert_eq!(status["enabled"], true);
    assert_eq!(status["from"], "TaskCraft <taskcraft@localhost>");
    assert!(status["server"].as_str().unwrap().contains("(unencrypted)"), "{status}");
    let r = post(&owner, app.url("/admin/mail/test"), json!({})).await;
    assert_eq!(r["to"], "owner@example.com");
    let m = sink.wait("owner@example.com", "TaskCraft test e-mail").await;
    assert!(m.text.contains("E-mail from TaskCraft works") && m.html.contains("E-mail from TaskCraft works"), "{m:?}");
    let (member, _) = app.user(&owner, "Max Member", "max@example.com", "member").await;
    assert_eq!(send(&member, Method::POST, app.url("/admin/mail/test"), json!({})).await.0, 403);
    assert_eq!(send(&member, Method::GET, app.url("/admin/mail"), json!(null)).await.0, 403);

    // an invitation is e-mailed (by the worker); the admin still gets the link
    let inv = post(&owner, app.url("/users"), json!({"name": "Ivy <Invitee>", "email": "Ivy@Example.com"})).await;
    assert_eq!(inv["emailed"], true);
    let invite_url = inv["inviteUrl"].as_str().unwrap().to_string();
    let m = sink.wait("ivy@example.com", "invited you").await;
    assert_eq!(m.subject, "Olivia Owner invited you to Acme on TaskCraft");
    assert!(m.text.contains("Hi Ivy <Invitee>,") && m.text.contains(&invite_url), "{}", m.text);
    assert!(m.html.contains("Hi Ivy &lt;Invitee&gt;,") && !m.html.contains("<Invitee>"), "{}", m.html);
    assert!(m.html.contains(&format!("href=\"{invite_url}\"")), "{}", m.html);
    let token = token_in(&m.text);
    assert_eq!(get(&client(), app.url(&format!("/auth/link?token={token}"))).await["kind"], "invite");
    let ivy = client();
    post(&ivy, app.url("/auth/link"), json!({"token": token, "password": "first-pass-1", "name": "Ivy Invitee"})).await;
    assert_eq!(get(&ivy, app.url("/me")).await["user"]["status"], "active");
    // accounts with a password get no invitation
    let r = post(&owner, app.url("/users"), json!({"name": "Pat", "email": "pat@example.com", "password": "password123"})).await;
    assert_eq!(r["emailed"], false);
    assert!(r["inviteUrl"].is_null());

    // forgot password: same answer for every address, a link only for real accounts
    let anon = client();
    assert_eq!(send(&anon, Method::POST, app.url("/auth/forgot"), json!({"email": "nobody@example.com"})).await.0, 204);
    assert_eq!(send(&anon, Method::POST, app.url("/auth/forgot"), json!({"email": "not an address"})).await.0, 204);
    assert_eq!(send(&anon, Method::POST, app.url("/auth/forgot"), json!({"email": " IVY@example.com "})).await.0, 204);
    taskcraft::mail::drain(&app.state).await.unwrap();
    let m = sink.wait("ivy@example.com", "Reset your TaskCraft password").await;
    assert!(m.text.contains("Hi Ivy Invitee,") && m.text.contains("expires in an hour"), "{}", m.text);
    let token = token_in(&m.text);
    let expires: bool =
        sqlx::query_scalar("SELECT expires_at < now() + interval '61 minutes' FROM user_links WHERE kind = 'reset'").fetch_one(&app.state.db).await.unwrap();
    assert!(expires, "self-service links are short-lived");
    assert_eq!(get(&anon, app.url(&format!("/auth/link?token={token}"))).await["kind"], "reset");
    post(&anon, app.url("/auth/link"), json!({"token": token, "password": "second-pass-2"})).await;
    assert_eq!(get(&anon, app.url("/me")).await["user"]["name"], "Ivy Invitee", "signed in after choosing a password");
    assert!(get(&ivy, app.url("/me")).await["user"].is_null(), "other sessions end");
    assert_eq!(send(&client(), Method::POST, app.url("/auth/link"), json!({"token": token, "password": "third-pass-3"})).await.0, 400, "single use");
    assert_eq!(send(&client(), Method::POST, app.url("/auth/login"), json!({"email": "ivy@example.com", "password": "second-pass-2"})).await.0, 200);
    assert!(sink.to("nobody@example.com").is_empty());

    // at most three reset e-mails per address and hour
    for _ in 0..4 {
        assert_eq!(send(&anon, Method::POST, app.url("/auth/forgot"), json!({"email": "ivy@example.com"})).await.0, 204);
    }
    taskcraft::mail::drain(&app.state).await.unwrap();
    let resets = || sink.to("ivy@example.com").iter().filter(|m| m.subject == "Reset your TaskCraft password").count();
    for _ in 0..50 {
        if resets() >= 3 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(resets(), 3);
    let queued: i64 = sqlx::query_scalar("SELECT count(*) FROM mail_outbox WHERE purpose = 'reset'").fetch_one(&app.state.db).await.unwrap();
    assert_eq!(queued, 3);

    // a reset link an admin makes is e-mailed too
    let ivy_id =
        get(&owner, app.url("/users")).await.as_array().unwrap().iter().find(|u| u["email"] == "ivy@example.com").unwrap()["id"].as_str().unwrap().to_string();
    let r = post(&owner, app.url(&format!("/users/{ivy_id}/link")), json!({})).await;
    assert_eq!(r["emailed"], true);
    taskcraft::mail::drain(&app.state).await.unwrap();
    let m = sink.wait("ivy@example.com", "Choose a new TaskCraft password").await;
    assert!(m.text.contains("Olivia Owner sent you a link") && m.text.contains(r["url"].as_str().unwrap()), "{}", m.text);

    let status = get(&owner, app.url("/admin/mail")).await;
    assert_eq!(status["pending"], 0, "{status}");
    assert!(status["sentLastDay"].as_i64().unwrap() >= 5, "{status}");
}

async fn pending(app: &TestApp) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM notifications WHERE emailed_at IS NULL").fetch_one(&app.state.db).await.unwrap()
}

async fn age(app: &TestApp, interval: &str) {
    sqlx::query(sqlx::AssertSqlSafe(format!("UPDATE notifications SET created_at = now() - interval '{interval}' WHERE emailed_at IS NULL")))
        .execute(&app.state.db)
        .await
        .unwrap();
}

/// Ends "using the app right now" for everyone.
async fn idle(app: &TestApp) {
    sqlx::query("UPDATE sessions SET last_seen_at = now() - interval '1 hour'").execute(&app.state.db).await.unwrap();
}

/// Turns due notifications into e-mails and sends them; returns how many e-mails.
async fn mail_out(app: &TestApp) -> usize {
    let n = taskcraft::mail::notification_mails(&app.state).await.unwrap();
    taskcraft::mail::drain(&app.state).await.unwrap();
    n
}

#[tokio::test(flavor = "multi_thread")]
async fn notification_mails_instant_and_digest() {
    let (app, sink) = with_mail().await;
    let owner = app.setup(false).await;
    let (max, max_id) = app.user(&owner, "Max Member", "max@example.com", "member").await;
    post(&owner, app.url("/projects"), json!({"key": "WEB", "name": "Website", "template": "scrum"})).await;
    let p = get(&owner, app.url("/projects/WEB")).await;
    let done = p["statuses"].as_array().unwrap().iter().find(|s| s["name"] == "Done").unwrap()["id"].as_str().unwrap().to_string();
    let issue = post(&owner, app.url("/issues"), json!({"projectKey": "WEB", "title": "Login <b>times</b> out & fails", "assigneeId": max_id})).await;
    let key = issue["key"].as_str().unwrap().to_string();
    let issue_url = format!("{}/issues/{key}", app.base);
    assert_eq!(pending(&app).await, 1, "the assignment");

    // the daily digest (the default) waits for 08:00 in the person's time zone
    post(&owner, app.url(&format!("/issues/{key}/comments")), json!({"markdown": "Looks like the <proxy> & the cookie"})).await;
    assert_eq!(pending(&app).await, 2);
    assert_eq!(mail_out(&app).await, 0, "not before the next morning");
    age(&app, "1 day").await;
    assert_eq!(mail_out(&app).await, 1);
    let m = sink.wait("max@example.com", "TaskCraft daily summary").await;
    assert_eq!(m.subject, "TaskCraft daily summary: 2 updates on 1 issue");
    assert!(m.text.contains(&format!("[{key}] Login <b>times</b> out & fails\n{issue_url}\n")), "{}", m.text);
    assert!(m.text.contains("• Olivia Owner assigned it to you (") && m.text.contains("• Olivia Owner commented ("), "{}", m.text);
    assert!(m.text.contains("“Looks like the <proxy> & the cookie”"), "{}", m.text);
    assert!(m.text.contains(&format!("{}/settings#notifications", app.base)), "a way to change or stop these: {}", m.text);
    assert!(m.html.contains("Login &lt;b&gt;times&lt;/b&gt; out &amp; fails") && m.html.contains("the &lt;proxy&gt; &amp; the cookie"), "{}", m.html);
    assert!(!m.html.contains("<b>times") && !m.html.contains("<proxy>"));
    assert!(m.html.contains(&format!("href=\"{issue_url}\"")) && m.html.contains("/settings#notifications"));
    assert_eq!(pending(&app).await, 0);
    assert_eq!(mail_out(&app).await, 0, "each notification is mailed once");

    // a time zone the database doesn't know falls back to UTC
    sqlx::query("UPDATE users SET timezone = 'Nowhere/Land' WHERE id = $1::uuid").bind(&max_id).execute(&app.state.db).await.unwrap();
    post(&owner, app.url(&format!("/issues/{key}/comments")), json!({"markdown": "Another one"})).await;
    age(&app, "1 day").await;
    assert_eq!(mail_out(&app).await, 1);

    // instant: a burst becomes one e-mail, a couple of minutes after it started
    assert_eq!(send(&max, Method::PUT, app.url("/me/preferences"), json!({"email": "weekly"})).await.0, 400);
    assert_eq!(send(&max, Method::PUT, app.url("/me/preferences"), json!({"email": "instant"})).await.0, 204);
    assert_eq!(get(&max, app.url("/me")).await["preferences"]["email"], "instant");
    idle(&app).await;
    post(&owner, app.url(&format!("/issues/{key}/comments")), json!({"markdown": "First"})).await;
    post(&owner, app.url(&format!("/issues/{key}/comments")), json!({"markdown": "Second"})).await;
    patch(&owner, app.url(&format!("/issues/{key}")), json!({"statusId": done})).await;
    assert_eq!(pending(&app).await, 3);
    assert_eq!(mail_out(&app).await, 0, "the batch is still open");
    age(&app, "3 minutes").await;
    assert_eq!(mail_out(&app).await, 1);
    let m = sink.wait("max@example.com", "3 updates").await;
    assert_eq!(m.subject, format!("[{key}] Login <b>times</b> out & fails — 3 updates"));
    assert!(m.text.contains("“First”") && m.text.contains("“Second”") && m.text.contains("Olivia Owner changed the status from To do to Done"), "{}", m.text);

    post(&owner, app.url(&format!("/issues/{key}/comments")), json!({"markdown": "Just one"})).await;
    age(&app, "3 minutes").await;
    assert_eq!(mail_out(&app).await, 1);
    let m = sink.wait("max@example.com", "Olivia Owner commented").await;
    assert_eq!(m.subject, format!("[{key}] Login <b>times</b> out & fails — Olivia Owner commented"));

    // not mailed: what was read in the app, what came while using the app, muted kinds
    post(&owner, app.url(&format!("/issues/{key}/comments")), json!({"markdown": "Read it already"})).await;
    sqlx::query("UPDATE notifications SET read_at = now()").execute(&app.state.db).await.unwrap();
    age(&app, "3 minutes").await;
    assert_eq!(mail_out(&app).await, 0);
    post(&owner, app.url(&format!("/issues/{key}/comments")), json!({"markdown": "Seen live"})).await;
    get(&max, app.url("/notifications")).await;
    sqlx::query("UPDATE sessions SET last_seen_at = now()").execute(&app.state.db).await.unwrap();
    age(&app, "3 minutes").await;
    assert_eq!(mail_out(&app).await, 0, "Max was using the app");
    idle(&app).await;
    post(&owner, app.url(&format!("/issues/{key}/comments")), json!({"markdown": "Muted later"})).await;
    assert_eq!(send(&max, Method::PUT, app.url("/me/preferences"), json!({"muted": ["commented"]})).await.0, 204);
    idle(&app).await;
    age(&app, "3 minutes").await;
    assert_eq!(mail_out(&app).await, 0, "muted kinds aren't mailed");
    assert_eq!(pending(&app).await, 0);

    // issues Max can't see any more aren't mailed
    send(&max, Method::PUT, app.url("/me/preferences"), json!({"muted": []})).await;
    idle(&app).await;
    post(&owner, app.url(&format!("/issues/{key}/comments")), json!({"markdown": "Secret"})).await;
    sqlx::query("UPDATE users SET role = 'guest' WHERE id = $1::uuid").bind(&max_id).execute(&app.state.db).await.unwrap();
    age(&app, "3 minutes").await;
    assert_eq!(mail_out(&app).await, 0);
    sqlx::query("UPDATE users SET role = 'member' WHERE id = $1::uuid").bind(&max_id).execute(&app.state.db).await.unwrap();

    // off: nothing waits to be mailed
    assert_eq!(send(&max, Method::PUT, app.url("/me/preferences"), json!({"email": "off"})).await.0, 204);
    post(&owner, app.url(&format!("/issues/{key}/comments")), json!({"markdown": "Quiet"})).await;
    assert_eq!(pending(&app).await, 0);
    assert!(get(&max, app.url("/notifications")).await["unread"].as_i64().unwrap() > 0, "still in the app");

    // nobody else got anything; the owner only acts here
    assert!(sink.to("owner@example.com").is_empty());
    assert_eq!(sink.all().len(), 4);
}

#[tokio::test(flavor = "multi_thread")]
async fn sample_data_mails_nobody() {
    let (app, sink) = with_mail().await;
    app.setup(true).await;
    assert_eq!(pending(&app).await, 0, "seeded notifications (backdated, to made-up addresses) count as mailed");
    assert_eq!(mail_out(&app).await, 0);
    assert!(sink.all().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn without_smtp_links_are_shown_instead() {
    let app = spawn().await;
    taskcraft::mail::spawn_worker(&app.state);
    assert_eq!(get(&client(), app.url("/setup")).await["mail"], false);
    let owner = app.setup(false).await;
    let inv = post(&owner, app.url("/users"), json!({"name": "Ivy", "email": "ivy@example.com"})).await;
    assert_eq!(inv["emailed"], false);
    assert!(inv["inviteUrl"].as_str().unwrap().contains("/welcome?token="));
    let (s, v) = send(&client(), Method::POST, app.url("/auth/forgot"), json!({"email": "owner@example.com"})).await;
    assert_eq!(s, 400, "{v}");
    assert_eq!(get(&owner, app.url("/admin/mail")).await["enabled"], false);
    assert_eq!(send(&owner, Method::POST, app.url("/admin/mail/test"), json!({})).await.0, 400);
    // notifications aren't kept waiting for an e-mail that never goes out
    let (_, max_id) = app.user(&owner, "Max", "max@example.com", "member").await;
    post(&owner, app.url("/projects"), json!({"key": "WEB", "name": "Website", "template": "scrum"})).await;
    post(&owner, app.url("/issues"), json!({"projectKey": "WEB", "title": "Assigned", "assigneeId": max_id})).await;
    assert_eq!(sqlx::query_scalar::<_, i64>("SELECT count(*) FROM notifications").fetch_one(&app.state.db).await.unwrap(), 1);
    assert_eq!(pending(&app).await, 0);
    assert_eq!(taskcraft::mail::notification_mails(&app.state).await.unwrap(), 0);
}
