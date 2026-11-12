//! E-mail in: messages handed to `POST /api/integrations/mail-in` (and read from an in-process
//! IMAP server) become issues and comments; requesters get e-mails back.

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::*;
use reqwest::Method;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

const TOKEN: &str = "mail-in-secret";

/// A message as a mail program would send it (CRLF line ends).
fn message(from: &str, to: &str, subject: &str, id: &str, headers: &str, body: &str) -> String {
    let mut m =
        format!("From: {from}\r\nTo: {to}\r\nSubject: {subject}\r\nMessage-ID: <{id}>\r\nDate: Thu, 12 Nov 2026 10:00:00 +0100\r\nMIME-Version: 1.0\r\n");
    for h in headers.lines().filter(|l| !l.trim().is_empty()) {
        m.push_str(h.trim());
        m.push_str("\r\n");
    }
    if !headers.to_ascii_lowercase().contains("content-type:") {
        m.push_str("Content-Type: text/plain; charset=utf-8\r\n");
    }
    m.push_str("\r\n");
    m.push_str(&body.replace("\r\n", "\n").replace('\n', "\r\n"));
    m
}

/// Hands a message to the endpoint; returns what became of it.
async fn deliver(app: &TestApp, raw: &str) -> Value {
    let r = reqwest::Client::new()
        .post(app.url("/integrations/mail-in"))
        .header("Authorization", format!("Bearer {TOKEN}"))
        .header("Content-Type", "message/rfc822")
        .body(raw.to_string())
        .send()
        .await
        .unwrap();
    let status = r.status();
    let v: Value = r.json().await.unwrap_or(Value::Null);
    assert_eq!(status, 200, "{v}");
    v
}

async fn outbox(app: &TestApp, purpose: &str) -> Vec<(String, String, String, Value)> {
    sqlx::query_as::<_, (String, String, String, Option<Value>)>(
        "SELECT to_address, subject, body_text, headers FROM mail_outbox WHERE purpose = $1 ORDER BY created_at",
    )
    .bind(purpose)
    .fetch_all(&app.state.db)
    .await
    .unwrap()
    .into_iter()
    .map(|(a, b, c, d)| (a, b, c, d.unwrap_or(Value::Null)))
    .collect()
}

async fn comments(owner: &reqwest::Client, app: &TestApp, key: &str) -> Vec<Value> {
    get(owner, app.url(&format!("/issues/{key}/comments"))).await.as_array().unwrap().clone()
}

fn text_of(doc: &Value) -> String {
    fn walk(n: &Value, out: &mut String) {
        if let Some(t) = n["text"].as_str() {
            out.push_str(t);
        }
        if n["type"] == "hardBreak" {
            out.push('\n');
        }
        for c in n["content"].as_array().into_iter().flatten() {
            walk(c, out);
        }
        if n["type"] == "paragraph" {
            out.push('\n');
        }
    }
    let mut s = String::new();
    walk(doc, &mut s);
    s.trim().to_string()
}

async fn mail_app(sink: Option<&Sink>, imap: Option<String>) -> TestApp {
    let smtp = sink.map(|s| s.url.clone()).unwrap_or_else(|| "smtp://127.0.0.1:9".into());
    spawn_with(move |c| {
        c.smtp_url = Some(smtp);
        c.mail_from = "TaskCraft <tasks@acme.test>".into();
        c.mail_in.mail_in_token = Some(TOKEN.into());
        c.mail_in.imap_url = imap;
    })
    .await
}

async fn take_mail(owner: &reqwest::Client, app: &TestApp, settings: Value) -> Value {
    let (s, v) = send(owner, Method::PUT, app.url("/projects/SUP/mail-in"), settings).await;
    assert_eq!(s, 200, "{v}");
    v
}

#[tokio::test(flavor = "multi_thread")]
async fn members_write_issues_and_comments_by_mail() {
    let app = mail_app(None, None).await;
    let owner = app.setup(true).await;
    let (max, _) = app.user(&owner, "Max Member", "max@example.com", "member").await;

    // the endpoint wants the token
    let raw = message(
        "Olivia Owner <owner@example.com>",
        "support@acme.test",
        "Printer on fire",
        "m1@example.com",
        "",
        "Hi,\n\nthe printer on floor 2 is on fire.\nPlease help.\n\n-- \nOlivia",
    );
    let anon = reqwest::Client::new();
    assert_eq!(anon.post(app.url("/integrations/mail-in")).body(raw.clone()).send().await.unwrap().status(), 401);
    assert_eq!(anon.post(app.url("/integrations/mail-in?token=wrong")).body(raw.clone()).send().await.unwrap().status(), 401);

    // nothing takes mail yet
    let r = deliver(&app, &raw).await;
    assert_eq!((r["status"].as_str(), r["reason"].as_str()), (Some("rejected"), Some("no-project")), "{r}");

    // settings: only project admins; the sending address and taken addresses are refused
    assert_eq!(send(&max, Method::GET, app.url("/projects/SUP/mail-in"), Value::Null).await.0, 403);
    let (s, v) = send(&owner, Method::PUT, app.url("/projects/SUP/mail-in"), json!({"enabled": true, "address": "Tasks@Acme.test"})).await;
    assert_eq!(s, 400, "{v}");
    let (s, _) = send(&owner, Method::PUT, app.url("/projects/SUP/mail-in"), json!({"enabled": true, "address": "not an address"})).await;
    assert_eq!(s, 400);
    let (s, v) = send(&owner, Method::PUT, app.url("/projects/WEB/mail-in"), json!({"enabled": true, "address": "web@acme.test"})).await;
    assert_eq!(s, 200, "{v}");
    let (s, v) = send(&owner, Method::PUT, app.url("/projects/SUP/mail-in"), json!({"enabled": true, "address": "WEB@acme.test"})).await;
    assert_eq!(s, 409, "{v}");
    let v = take_mail(&owner, &app, json!({"enabled": true, "address": " Support@Acme.test ", "senders": "members"})).await;
    assert_eq!(v["settings"], json!({"enabled": true, "address": "support@acme.test", "senders": "members"}));
    assert_eq!(v["http"], true);
    assert!(v["endpoint"].as_str().unwrap().ends_with("/api/integrations/mail-in"));
    assert_eq!(get(&owner, app.url("/projects/SUP")).await["settings"]["mailIn"]["address"], "support@acme.test");

    // a new message from a member: an issue as them (the same Message-ID again is a duplicate,
    // so the first attempt above doesn't count)
    let r = deliver(&app, &raw).await;
    assert_eq!(r["status"], "duplicate", "{r}");
    let raw = raw.replace("m1@example.com", "m2@example.com");
    let r = deliver(&app, &raw).await;
    assert_eq!(r["status"], "created", "{r}");
    let key = r["issue"].as_str().unwrap().to_string();
    assert!(key.starts_with("SUP-"));
    let issue = get(&owner, app.url(&format!("/issues/{key}"))).await;
    assert_eq!(issue["title"], "Printer on fire");
    assert_eq!(issue["reporter"]["name"], "Olivia Owner");
    assert!(issue["requester"].is_null());
    assert_eq!(text_of(&issue["description"]), "Hi,\nthe printer on floor 2 is on fire.\nPlease help.\n--\nOlivia", "new issues keep the whole text");
    let r = deliver(&app, &raw).await;
    assert_eq!(r["status"], "duplicate", "{r}");
    let r = deliver(&app, &raw.replace("Message-ID: <m2@example.com>\r\n", "")).await;
    assert_eq!(r["status"], "created", "without a Message-ID the content decides");
    let r2 = deliver(&app, &raw.replace("Message-ID: <m2@example.com>\r\n", "")).await;
    assert_eq!(r2["status"], "duplicate", "{r2}");
    send(&owner, Method::DELETE, app.url(&format!("/issues/{}", r["issue"].as_str().unwrap())), Value::Null).await;

    // a reply by subject key: a comment without the quoted text and signature
    let reply = message(
        "owner@example.com",
        "Customer Support <support@acme.test>",
        &format!("Re: [{key}] Printer on fire"),
        "m3@example.com",
        "",
        "It's out now.\n\nOn Thu, 12 Nov 2026 at 10:00, Olivia Owner <owner@example.com> wrote:\n> Hi,\n> the printer is on fire.\n\n-- \nOlivia",
    );
    let r = deliver(&app, &reply).await;
    assert_eq!((r["status"].as_str(), r["issue"].as_str()), (Some("commented"), Some(key.as_str())), "{r}");
    let c = comments(&owner, &app, &key).await;
    let last = c.last().unwrap();
    assert_eq!(text_of(&last["body"]), "It's out now.");
    assert_eq!(last["author"]["name"], "Olivia Owner");
    assert_eq!(last["viaEmail"], true);

    // a reply to a notification e-mail (In-Reply-To names our Message-ID; the subject has no key)
    post(&max, app.url(&format!("/issues/{key}/comments")), json!({"markdown": "Which floor?"})).await;
    sqlx::query("UPDATE users SET preferences = preferences || '{\"email\": \"instant\"}'").execute(&app.state.db).await.unwrap();
    sqlx::query("UPDATE notifications SET created_at = now() - interval '5 minutes' WHERE emailed_at IS NULL").execute(&app.state.db).await.unwrap();
    sqlx::query("UPDATE sessions SET last_seen_at = now() - interval '1 hour'").execute(&app.state.db).await.unwrap();
    assert!(taskcraft::mail::notification_mails(&app.state).await.unwrap() >= 1);
    let notes = outbox(&app, "notification").await;
    let (_, subject, _, headers) = notes.iter().find(|n| n.0 == "owner@example.com").expect("a notification e-mail to the owner");
    assert!(subject.contains(&key), "{subject}");
    assert_eq!(headers["replyTo"], "support@acme.test", "{headers}");
    assert_eq!(headers["autoSubmitted"], "auto-generated");
    let our_id = headers["messageId"].as_str().unwrap().to_string();
    assert!(our_id.starts_with("<tc.") && our_id.ends_with("@acme.test>"), "{our_id}");
    let reply = message(
        "OWNER@example.com",
        "support@acme.test",
        "Re: Something else entirely",
        "m4@example.com",
        &format!("In-Reply-To: {our_id}\nReferences: {our_id}"),
        "Floor 2.\n\n> Which floor?",
    );
    let r = deliver(&app, &reply).await;
    assert_eq!((r["status"].as_str(), r["issue"].as_str()), (Some("commented"), Some(key.as_str())), "{r}");
    assert_eq!(text_of(&comments(&owner, &app, &key).await.last().unwrap()["body"]), "Floor 2.");

    // machines and loops are ignored
    let auto = message("max@example.com", "support@acme.test", "Out of office", "m5@example.com", "Auto-Submitted: auto-replied", "I'm away.");
    let r = deliver(&app, &auto).await;
    assert_eq!((r["status"].as_str(), r["reason"].as_str()), (Some("ignored"), Some("auto-reply")), "{r}");
    let bulk = message("news@example.com", "support@acme.test", "Newsletter", "m6@example.com", "Precedence: bulk", "Buy!");
    assert_eq!(deliver(&app, &bulk).await["reason"], "bulk");
    let ours = message("tasks@acme.test", "support@acme.test", "Loop", "m7@example.com", "", "x");
    assert_eq!(deliver(&app, &ours).await["reason"], "loop");

    // files become attachments; HTML-only mail becomes text; plus-addressing reaches the project
    let html = "<html><body><p>See <b>the log</b>.</p><script>alert(1)</script></body></html>";
    let with_file = format!(
        "From: Max Member <max@example.com>\r\nTo: inbox+sup@acme.test\r\nSubject: Crash log\r\nMessage-ID: <m8@example.com>\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=\"XX\"\r\n\r\n--XX\r\nContent-Type: text/html; charset=utf-8\r\n\r\n{html}\r\n--XX\r\nContent-Type: text/plain; name=\"crash.log\"\r\nContent-Disposition: attachment; filename=\"crash.log\"\r\nContent-Transfer-Encoding: base64\r\n\r\nbGluZSAxCmxpbmUgMgo=\r\n--XX--\r\n"
    );
    let r = deliver(&app, &with_file).await;
    assert_eq!(r["status"], "created", "{r}");
    let key2 = r["issue"].as_str().unwrap().to_string();
    let issue = get(&owner, app.url(&format!("/issues/{key2}"))).await;
    assert_eq!(text_of(&issue["description"]), "See the log.");
    assert_eq!(issue["reporter"]["name"], "Max Member");
    let files = issue["attachments"].as_array().unwrap();
    assert_eq!(files.len(), 1, "{files:?}");
    assert_eq!(files[0]["name"], "crash.log");
    let data = owner.get(format!("{}{}", app.base, files[0]["url"].as_str().unwrap())).send().await.unwrap().bytes().await.unwrap();
    assert_eq!(&data[..], b"line 1\nline 2\n");
    // a reply with a file: the comment names it, the file is on the issue
    let reply_file = with_file
        .replace("Subject: Crash log", &format!("Subject: RE: [{key2}] Crash log"))
        .replace("<m8@example.com>", "<m9@example.com>")
        .replace("filename=\"crash.log\"", "filename=\"second.log\"");
    let r = deliver(&app, &reply_file).await;
    assert_eq!(r["status"], "commented", "{r}");
    assert_eq!(text_of(&comments(&owner, &app, &key2).await.last().unwrap()["body"]), "See the log.\n📎 second.log");
    assert_eq!(get(&owner, app.url(&format!("/issues/{key2}"))).await["attachments"].as_array().unwrap().len(), 2);

    // unknown senders aren't let in while only members may write
    let stranger = message("Eve <eve@example.org>", "support@acme.test", "Hello", "m10@example.org", "", "Let me in");
    let r = deliver(&app, &stranger).await;
    assert_eq!((r["status"].as_str(), r["reason"].as_str()), (Some("rejected"), Some("not-allowed")), "{r}");
    // nor can they comment on an issue they know the key of
    let r = deliver(&app, &stranger.replace("Subject: Hello", &format!("Subject: Re: [{key}] x")).replace("m10@", "m11@")).await;
    assert_eq!(r["reason"], "not-allowed", "{r}");
    // a viewer may comment but not open issues
    sqlx::query("UPDATE users SET role = 'guest' WHERE email = 'max@example.com'").execute(&app.state.db).await.unwrap();
    let r = deliver(&app, &message("max@example.com", "support@acme.test", "New thing", "m12@example.com", "", "x")).await;
    assert_eq!(r["reason"], "not-allowed", "guests without a grant: {r}");

    // the settings page lists what came in
    let v = get(&owner, app.url("/projects/SUP/mail-in")).await;
    let msgs = v["messages"].as_array().unwrap();
    assert!(msgs.iter().any(|m| m["status"] == "created" && m["issueKey"] == key.as_str()));
    assert!(msgs.iter().any(|m| m["reason"] == "auto-reply"), "ignored mail to the project's address shows up there");
    assert!(msgs.iter().any(|m| m["reason"] == "no-project" && m["routed"] == false), "workspace admins also see what reached no project");
    assert!(msgs.iter().all(|m| m["from"] != "" || m["status"] == "rejected"));
}

#[tokio::test(flavor = "multi_thread")]
async fn requesters_write_in_and_get_answers() {
    let sink = Sink::start().await;
    let app = mail_app(Some(&sink), None).await;
    let owner = app.setup(true).await;
    let (max, _) = app.user(&owner, "Max Member", "max@example.com", "member").await;
    take_mail(&owner, &app, json!({"enabled": true, "address": "support@acme.test", "senders": "anyone"})).await;

    // someone without an account opens an issue and gets an acknowledgement
    let first =
        message("Jane Doe <Jane@Example.org>", "support@acme.test", "Fwd: Invoice is wrong", "jane-1@example.org", "", "My invoice shows the wrong address.");
    let r = deliver(&app, &first).await;
    assert_eq!(r["status"], "created", "{r}");
    let key = r["issue"].as_str().unwrap().to_string();
    let issue = get(&owner, app.url(&format!("/issues/{key}"))).await;
    assert_eq!(issue["title"], "Invoice is wrong");
    assert_eq!(issue["requester"], json!({"email": "jane@example.org", "name": "Jane Doe", "mail": true, "source": "email"}));
    assert!(issue["reporter"].is_null());
    let history = get(&owner, app.url(&format!("/issues/{key}/history"))).await;
    assert!(history.as_array().unwrap().iter().any(|e| e["kind"] == "created" && e["new"]["by"] == "Jane Doe"), "{history}");
    let acks = outbox(&app, "mail-in-ack").await;
    assert_eq!(acks.len(), 1);
    let (to, subject, text, headers) = &acks[0];
    assert_eq!(to, "jane@example.org");
    assert_eq!(subject, &format!("[{key}] Invoice is wrong"));
    assert!(text.contains(&format!("It's filed as {key}")) && text.contains("— Customer Support"), "{text}");
    assert_eq!(headers["replyTo"], "support@acme.test");
    assert_eq!(headers["inReplyTo"], "<jane-1@example.org>");
    assert_eq!(headers["autoSubmitted"], "auto-replied");
    assert_eq!(headers["fromName"], "Customer Support");
    let ack_id = headers["messageId"].as_str().unwrap().to_string();

    // the acknowledgement goes out with its headers
    taskcraft::mail::drain(&app.state).await.unwrap();
    let sent = sink.wait("jane@example.org", &key).await;
    assert!(sent.contains("Reply-To: support@acme.test"), "{sent}");
    assert!(sent.contains("Auto-Submitted: auto-replied"), "{sent}");
    assert!(sent.contains(&format!("Message-ID: {ack_id}")) || sent.contains(&format!("Message-Id: {ack_id}")), "{sent}");
    assert!(sent.contains("In-Reply-To: <jane-1@example.org>"), "{sent}");
    assert!(sent.contains("From: \"Customer Support\" <tasks@acme.test>") || sent.contains("From: Customer Support <tasks@acme.test>"), "{sent}");

    // she answers the acknowledgement: a comment by her (the response clock keeps running)
    let answer = message(
        "jane@example.org",
        "support@acme.test",
        &format!("Re: [{key}] Invoice is wrong"),
        "jane-2@example.org",
        &format!("In-Reply-To: {ack_id}"),
        "It's invoice 4711.\n\nAm 12.11.2026 um 10:00 schrieb Customer Support <tasks@acme.test>:\n> thanks for your message.",
    );
    let r = deliver(&app, &answer).await;
    assert_eq!((r["status"].as_str(), r["issue"].as_str()), (Some("commented"), Some(key.as_str())), "{r}");
    let c = comments(&owner, &app, &key).await;
    let hers = c.last().unwrap();
    assert!(hers["author"].is_null());
    assert_eq!(hers["external"], json!({"email": "jane@example.org", "name": "Jane Doe"}));
    assert_eq!(text_of(&hers["body"]), "It's invoice 4711.");

    // others can't join her conversation
    let other = answer.replace("jane@example.org", "mallory@example.org").replace("jane-2@", "mallory-1@");
    assert_eq!(deliver(&app, &other).await["reason"], "not-allowed");

    // comments are internal unless marked for her
    post(&max, app.url(&format!("/issues/{key}/comments")), json!({"markdown": "Internal: check the billing export"})).await;
    assert!(outbox(&app, "requester").await.is_empty());
    let r = post(&max, app.url(&format!("/issues/{key}/comments")), json!({"markdown": "We **fixed** the address. Sorry!", "emailRequester": true})).await;
    assert_eq!(r["emailed"], true, "{r}");
    let replies = outbox(&app, "requester").await;
    assert_eq!(replies.len(), 1);
    let (to, subject, text, headers) = &replies[0];
    assert_eq!(to, "jane@example.org");
    assert_eq!(subject, &format!("Re: [{key}] Invoice is wrong"));
    assert!(text.starts_with("We **fixed** the address. Sorry!") && text.contains("— Max Member, Customer Support"), "{text}");
    assert!(!text.contains("Internal"));
    assert_eq!(headers["inReplyTo"], "<jane-2@example.org>", "answers her latest e-mail");
    assert!(headers.get("autoSubmitted").is_none(), "a person wrote it");
    let reply_id = headers["messageId"].as_str().unwrap().to_string();
    let c = comments(&owner, &app, &key).await;
    assert!(c.last().unwrap()["emailedAt"].is_string());
    assert!(c[c.len() - 2]["emailedAt"].is_null());
    // the response goal counts the team's answer, not hers
    let responded: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM comments c JOIN issues i ON i.id = c.issue_id WHERE i.number = $1 AND c.author_id IS NOT NULL)")
            .bind(key.trim_start_matches("SUP-").parse::<i32>().unwrap())
            .fetch_one(&app.state.db)
            .await
            .unwrap();
    assert!(responded);

    // only people who can edit may write to her; only issues with a requester
    let (viewer, _) = app.user(&owner, "Vera Guest", "vera@example.com", "guest").await;
    let (s, _) = send(&viewer, Method::POST, app.url(&format!("/issues/{key}/comments")), json!({"markdown": "x", "emailRequester": true})).await;
    assert!(s == 403 || s == 404, "{s}");
    let (s, v) = send(&max, Method::POST, app.url("/issues/SUP-1/comments"), json!({"markdown": "x", "emailRequester": true})).await;
    assert_eq!(s, 400, "{v}");

    // she answers that reply by e-mail (In-Reply-To only)
    let again =
        message("Jane Doe <jane@example.org>", "support@acme.test", "Re: thanks", "jane-3@example.org", &format!("In-Reply-To: {reply_id}"), "Thanks, works!");
    let r = deliver(&app, &again).await;
    assert_eq!((r["status"].as_str(), r["issue"].as_str()), (Some("commented"), Some(key.as_str())), "{r}");

    // too many messages from one sender in an hour are refused
    for n in 0..taskcraft::mail_in::PER_SENDER_PER_HOUR {
        sqlx::query("INSERT INTO mail_in (id, source, message_id, from_address, status) VALUES (gen_random_uuid(), 'http', $1, 'spam@example.org', 'ignored')")
            .bind(format!("spam-{n}"))
            .execute(&app.state.db)
            .await
            .unwrap();
    }
    let r = deliver(&app, &message("spam@example.org", "support@acme.test", "Hi", "spam-new@example.org", "", "x")).await;
    assert_eq!(r["reason"], "rate-limit", "{r}");

    // a new issue from an unknown sender without a subject; mail to the project at +KEY
    let r = deliver(&app, &message("bob@example.org", "inbox+SUP@acme.test", "", "bob-1@example.org", "", "Help")).await;
    assert_eq!(r["status"], "created", "{r}");
    assert_eq!(get(&owner, app.url(&format!("/issues/{}", r["issue"].as_str().unwrap()))).await["title"], "E-mail from bob@example.org");
}

#[tokio::test(flavor = "multi_thread")]
async fn mailbox_is_read_over_imap() {
    let imap = FakeImap::start().await;
    let url = format!("imap://tasks%40acme.test:pw@127.0.0.1:{}/INBOX", imap.port);
    let app = mail_app(None, Some(url.clone())).await;
    let owner = app.setup(true).await;
    let v = take_mail(&owner, &app, json!({"enabled": true, "senders": "members"})).await;
    assert_eq!(v["plusAddress"], "tasks+sup@acme.test");
    assert!(v["imap"]["mailbox"].as_str().unwrap().starts_with("tasks@acme.test@127.0.0.1"), "{v}");

    imap.add(&message("owner@example.com", "tasks+SUP@acme.test", "From the mailbox", "imap-1@example.com", "", "Read over IMAP."));
    imap.add(&message("owner@example.com", "tasks+SUP@acme.test", "Away", "imap-2@example.com", "X-Autoreply: yes", "I'm away."));
    let target = taskcraft::mail_in::imap::parse_url(&url).unwrap();
    assert_eq!(taskcraft::mail_in::poll_once(&app.state, &target).await.unwrap(), 2);
    assert_eq!(imap.unseen(), 0, "handled messages are marked read");
    let rows: Vec<(String, String, Option<String>)> =
        sqlx::query_as("SELECT source, status, reason FROM mail_in ORDER BY received_at").fetch_all(&app.state.db).await.unwrap();
    assert_eq!(rows, vec![("imap".into(), "created".into(), None), ("imap".into(), "ignored".into(), Some("auto-reply".into()))]);
    let found = get(&owner, app.url("/search?q=project%20%3D%20SUP%20and%20title%20~%20mailbox")).await;
    assert_eq!(found["issues"].as_array().map(Vec::len).unwrap_or(0), 1, "{found}");
    assert_eq!(taskcraft::mail_in::poll_once(&app.state, &target).await.unwrap(), 0, "nothing new");
    let v = get(&owner, app.url("/projects/SUP/mail-in")).await;
    assert!(v["imap"]["okAt"].is_string() && v["imap"]["error"].is_null(), "{v}");

    // moving instead of marking read
    imap.add(&message("owner@example.com", "tasks+SUP@acme.test", "Moved", "imap-3@example.com", "", "Moved over IMAP."));
    let moving = taskcraft::mail_in::imap::parse_url(&format!("{url}?move=Done")).unwrap();
    assert_eq!(taskcraft::mail_in::poll_once(&app.state, &moving).await.unwrap(), 1);
    assert_eq!(imap.moved(), vec!["Done".to_string()]);

    // a wrong password is noted for the settings page
    let wrong = taskcraft::mail_in::imap::parse_url(&format!("imap://tasks%40acme.test:nope@127.0.0.1:{}/INBOX", imap.port)).unwrap();
    assert!(taskcraft::mail_in::poll_once(&app.state, &wrong).await.is_err());
    let v = get(&owner, app.url("/projects/SUP/mail-in")).await;
    assert!(v["imap"]["error"].as_str().unwrap().contains("signing in"), "{v}");
}

// ---- an SMTP sink keeping raw messages -------------------------------------------------------

#[derive(Clone)]
struct Sink {
    url: String,
    mails: Arc<Mutex<Vec<(Vec<String>, String)>>>,
}

impl Sink {
    async fn start() -> Sink {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("smtp://{}", listener.local_addr().unwrap());
        let mails = Arc::new(Mutex::new(Vec::new()));
        let keep = mails.clone();
        tokio::spawn(async move {
            while let Ok((sock, _)) = listener.accept().await {
                let keep = keep.clone();
                tokio::spawn(async move {
                    let _ = smtp_session(sock, keep).await;
                });
            }
        });
        Sink { url, mails }
    }

    /// The raw message (headers unfolded) to `to` that mentions `what`.
    async fn wait(&self, to: &str, what: &str) -> String {
        for _ in 0..100 {
            let found = self.mails.lock().unwrap().iter().find(|(rcpt, raw)| rcpt.iter().any(|r| r == to) && raw.contains(what)).map(|m| m.1.clone());
            if let Some(m) = found {
                return m.replace("\r\n ", " ").replace("\r\n\t", " ");
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("no e-mail to {to} about {what}");
    }
}

async fn smtp_session(sock: tokio::net::TcpStream, mails: Arc<Mutex<Vec<(Vec<String>, String)>>>) -> std::io::Result<()> {
    let (r, mut w) = sock.into_split();
    let mut r = BufReader::new(r);
    w.write_all(b"220 sink ready\r\n").await?;
    let mut to = Vec::new();
    let mut line = String::new();
    loop {
        line.clear();
        if r.read_line(&mut line).await? == 0 {
            return Ok(());
        }
        let upper = line.trim_end().to_ascii_uppercase();
        if upper.starts_with("EHLO") {
            w.write_all(b"250-sink\r\n250-8BITMIME\r\n250 SMTPUTF8\r\n").await?;
        } else if upper.starts_with("RCPT TO:") {
            let a = line.trim_end()[8..].trim().trim_start_matches('<');
            to.push(a[..a.find('>').unwrap_or(a.len())].to_string());
            w.write_all(b"250 ok\r\n").await?;
        } else if upper == "DATA" {
            w.write_all(b"354 go ahead\r\n").await?;
            let mut data = String::new();
            loop {
                line.clear();
                if r.read_line(&mut line).await? == 0 {
                    return Ok(());
                }
                if line == ".\r\n" {
                    break;
                }
                data.push_str(line.strip_prefix('.').unwrap_or(&line));
            }
            mails.lock().unwrap().push((std::mem::take(&mut to), data));
            w.write_all(b"250 queued\r\n").await?;
        } else if upper == "QUIT" {
            w.write_all(b"221 bye\r\n").await?;
            return Ok(());
        } else {
            w.write_all(b"250 ok\r\n").await?;
        }
    }
}

// ---- a small IMAP server ---------------------------------------------------------------------

struct Mailbox {
    next_uid: u32,
    /// (uid, message, seen)
    messages: Vec<(u32, Vec<u8>, bool)>,
    moved: Vec<String>,
}

#[derive(Clone)]
struct FakeImap {
    port: u16,
    state: Arc<Mutex<Mailbox>>,
}

impl FakeImap {
    async fn start() -> FakeImap {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let state = Arc::new(Mutex::new(Mailbox { next_uid: 1, messages: Vec::new(), moved: Vec::new() }));
        let s = state.clone();
        tokio::spawn(async move {
            while let Ok((sock, _)) = listener.accept().await {
                let s = s.clone();
                tokio::spawn(async move {
                    let _ = imap_session(sock, s).await;
                });
            }
        });
        FakeImap { port, state }
    }

    fn add(&self, raw: &str) {
        let mut s = self.state.lock().unwrap();
        let uid = s.next_uid;
        s.next_uid += 1;
        s.messages.push((uid, raw.as_bytes().to_vec(), false));
    }

    fn unseen(&self) -> usize {
        self.state.lock().unwrap().messages.iter().filter(|m| !m.2).count()
    }

    fn moved(&self) -> Vec<String> {
        self.state.lock().unwrap().moved.clone()
    }
}

async fn imap_session(sock: tokio::net::TcpStream, state: Arc<Mutex<Mailbox>>) -> std::io::Result<()> {
    let (r, mut w) = sock.into_split();
    let mut r = BufReader::new(r);
    w.write_all(b"* OK [CAPABILITY IMAP4rev1 MOVE] fake ready\r\n").await?;
    let mut line = String::new();
    loop {
        line.clear();
        if r.read_line(&mut line).await? == 0 {
            return Ok(());
        }
        let l = line.trim_end().to_string();
        let Some((tag, cmd)) = l.split_once(' ') else { continue };
        let upper = cmd.to_ascii_uppercase();
        let mut out: Vec<u8> = Vec::new();
        if upper.starts_with("LOGIN ") {
            let args: Vec<String> = cmd[6..].split(' ').map(|a| a.trim_matches('"').to_string()).collect();
            if args == ["tasks@acme.test", "pw"] {
                out.extend(format!("{tag} OK LOGIN completed\r\n").bytes());
            } else {
                out.extend(format!("{tag} NO [AUTHENTICATIONFAILED] invalid credentials\r\n").bytes());
            }
        } else if upper.starts_with("SELECT ") {
            let n = state.lock().unwrap().messages.len();
            out.extend(
                format!("* {n} EXISTS\r\n* 0 RECENT\r\n* FLAGS (\\Seen \\Deleted)\r\n* OK [UIDVALIDITY 7] ok\r\n{tag} OK [READ-WRITE] SELECT completed\r\n")
                    .bytes(),
            );
        } else if upper.starts_with("UID SEARCH") {
            let uids: Vec<String> = state.lock().unwrap().messages.iter().filter(|m| !m.2).map(|m| m.0.to_string()).collect();
            out.extend(format!("* SEARCH {}\r\n{tag} OK SEARCH completed\r\n", uids.join(" ")).trim_start().replace("SEARCH \r\n", "SEARCH\r\n").bytes());
        } else if upper.starts_with("UID FETCH ") {
            let parts: Vec<&str> = cmd.splitn(4, ' ').collect();
            let uid: u32 = parts[2].parse().unwrap_or(0);
            let s = state.lock().unwrap();
            if let Some((seq, m)) = s.messages.iter().enumerate().find(|(_, m)| m.0 == uid) {
                let seq = seq + 1;
                if parts[3].to_ascii_uppercase().contains("RFC822.SIZE") {
                    out.extend(format!("* {seq} FETCH (UID {uid} RFC822.SIZE {})\r\n", m.1.len()).bytes());
                } else {
                    out.extend(format!("* {seq} FETCH (UID {uid} BODY[] {{{}}}\r\n", m.1.len()).bytes());
                    out.extend(&m.1);
                    out.extend(b")\r\n");
                }
            }
            out.extend(format!("{tag} OK FETCH completed\r\n").bytes());
        } else if upper.starts_with("UID STORE ") {
            let uid: u32 = cmd.split(' ').nth(2).and_then(|u| u.parse().ok()).unwrap_or(0);
            for m in state.lock().unwrap().messages.iter_mut().filter(|m| m.0 == uid) {
                m.2 = true;
            }
            out.extend(format!("{tag} OK STORE completed\r\n").bytes());
        } else if upper.starts_with("UID MOVE ") {
            let parts: Vec<&str> = cmd.splitn(4, ' ').collect();
            let uid: u32 = parts[2].parse().unwrap_or(0);
            let mut s = state.lock().unwrap();
            s.messages.retain(|m| m.0 != uid);
            s.moved.push(parts[3].trim_matches('"').to_string());
            out.extend(format!("{tag} OK MOVE completed\r\n").bytes());
        } else if upper.starts_with("LOGOUT") {
            w.write_all(format!("* BYE bye\r\n{tag} OK LOGOUT completed\r\n").as_bytes()).await?;
            return Ok(());
        } else {
            out.extend(format!("{tag} BAD unknown command\r\n").bytes());
        }
        w.write_all(&out).await?;
    }
}
