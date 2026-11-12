//! The service desk portal and request forms: requesters sign in with e-mailed links (single
//! use, short-lived, rate limited, no account enumeration), see only their own requests and the
//! comments meant for them, answer and add files; public forms open requests and keep bots out.

mod common;

use std::time::Duration;

use common::*;
use reqwest::Method;
use reqwest::multipart::{Form, Part};
use serde_json::{Value, json};

const TOKEN: &str = "mail-in-secret";

async fn portal_app() -> TestApp {
    spawn_with(|c| {
        // outgoing mail "works" (it stays in the outbox: no worker runs in tests)
        c.smtp_url = Some("smtp://127.0.0.1:9".into());
        c.mail_from = "TaskCraft <tasks@acme.test>".into();
        c.mail_in.mail_in_token = Some(TOKEN.into());
    })
    .await
}

/// Someone on the portal (no account): cookies, the CSRF header and an address (behind a proxy).
fn visitor(ip: &str) -> reqwest::Client {
    let mut h = reqwest::header::HeaderMap::new();
    h.insert("x-taskcraft", reqwest::header::HeaderValue::from_static("1"));
    h.insert("x-forwarded-for", ip.parse().unwrap());
    reqwest::Client::builder().cookie_store(true).default_headers(h).build().unwrap()
}

/// An e-mail to the project (e-mail in), from someone without an account.
async fn write_in(app: &TestApp, from: &str, subject: &str, id: &str, body: &str) -> String {
    let raw = format!(
        "From: {from}\r\nTo: support@acme.test\r\nSubject: {subject}\r\nMessage-ID: <{id}>\r\nMIME-Version: 1.0\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n{body}\r\n"
    );
    deliver(app, &raw).await
}

async fn deliver(app: &TestApp, raw: &str) -> String {
    let r: Value = reqwest::Client::new()
        .post(app.url("/integrations/mail-in"))
        .header("Authorization", format!("Bearer {TOKEN}"))
        .body(raw.to_string())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(r["status"] == "created" || r["status"] == "commented", "{r}");
    r["issue"].as_str().unwrap().to_string()
}

/// The sign-in tokens e-mailed to `email`, oldest first.
async fn links(app: &TestApp, email: &str) -> Vec<String> {
    let bodies: Vec<String> = sqlx::query_scalar("SELECT body_text FROM mail_outbox WHERE purpose = 'portal-link' AND to_address = $1 ORDER BY created_at, id")
        .bind(email)
        .fetch_all(&app.state.db)
        .await
        .unwrap();
    bodies
        .iter()
        .map(|b| {
            let at = b.find("token=").expect("a link in the e-mail") + 6;
            b[at..].chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').collect()
        })
        .collect()
}

/// Waits until `n` links went to `email` (they're sent in the background); their tokens.
async fn wait_links(app: &TestApp, email: &str, n: usize) -> Vec<String> {
    for _ in 0..50 {
        let l = links(app, email).await;
        if l.len() >= n {
            return l;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("expected {n} sign-in links to {email}, got {}", links(app, email).await.len());
}

/// Signs `email` in to the portal through an e-mailed link.
async fn sign_in(app: &TestApp, email: &str, ip: &str) -> reqwest::Client {
    let c = visitor(ip);
    let before = links(app, email).await.len();
    let r = c.post(app.url("/portal/sign-in")).json(&json!({"email": email})).send().await.unwrap();
    assert_eq!(r.status(), 204);
    let token = wait_links(app, email, before + 1).await.pop().unwrap();
    let (s, v) = send(&c, Method::POST, app.url("/portal/session"), json!({"token": token})).await;
    assert_eq!(s, 200, "{v}");
    c
}

async fn take_mail(owner: &reqwest::Client, app: &TestApp) {
    let (s, v) =
        send(owner, Method::PUT, app.url("/projects/SUP/mail-in"), json!({"enabled": true, "address": "support@acme.test", "senders": "anyone"})).await;
    assert_eq!(s, 200, "{v}");
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

#[tokio::test(flavor = "multi_thread")]
async fn sign_in_links_are_single_use_short_lived_and_rate_limited() {
    let app = portal_app().await;
    let owner = app.setup(true).await;
    take_mail(&owner, &app).await;
    let key = write_in(&app, "Jane Doe <jane@example.org>", "Invoice is wrong", "jane-1@example.org", "My invoice shows the wrong address.").await;

    // the acknowledgement links to the request in the portal
    let ack: String = sqlx::query_scalar("SELECT body_text FROM mail_outbox WHERE purpose = 'mail-in-ack' AND to_address = 'jane@example.org'")
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    assert!(ack.contains(&format!("{}/portal/requests/{key}", app.base)), "{ack}");
    assert!(ack.contains(&format!("keep [{key}] in the subject")), "the project takes e-mail: {ack}");

    let anon = visitor("198.51.100.1");
    let info = get(&anon, app.url("/portal")).await;
    assert_eq!(info["email"], Value::Null);
    assert_eq!(info["signIn"], true);
    assert_eq!(info["workspace"]["name"], "Acme");
    assert_eq!(send(&anon, Method::GET, app.url("/portal/requests"), Value::Null).await.0, 401);

    // asking for a link: the same answer for addresses with and without requests
    let ask = |c: &reqwest::Client, email: &str| {
        let req = c.post(app.url("/portal/sign-in")).json(&json!({"email": email, "request": key}));
        async move {
            let r = req.send().await.unwrap();
            (r.status().as_u16(), r.text().await.unwrap())
        }
    };
    assert_eq!(ask(&anon, "Jane@Example.org").await, (204, String::new()));
    assert_eq!(ask(&anon, "nobody@example.org").await, (204, String::new()));
    assert_eq!(ask(&anon, "not an address").await.0, 400);
    let no_csrf = reqwest::Client::new().post(app.url("/portal/sign-in")).json(&json!({"email": "jane@example.org"})).send().await.unwrap();
    assert_eq!(no_csrf.status(), 400, "writes need the CSRF header");
    let tokens = wait_links(&app, "jane@example.org", 1).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(links(&app, "nobody@example.org").await.is_empty(), "addresses without requests get nothing");
    let mail: (String, String, Value) =
        sqlx::query_as("SELECT subject, body_text, headers FROM mail_outbox WHERE purpose = 'portal-link' AND to_address = 'jane@example.org'")
            .fetch_one(&app.state.db)
            .await
            .unwrap();
    assert_eq!(mail.0, "Your sign-in link");
    assert!(mail.1.contains(&format!("{}/portal/sign-in?token=", app.base)) && mail.1.contains(&format!("&request={key}")), "{}", mail.1);
    assert!(mail.1.contains("within 30 minutes"));
    assert_eq!(mail.2["fromName"], "Acme");

    // the link signs in once
    let jane = visitor("198.51.100.2");
    let (s, v) = send(&jane, Method::POST, app.url("/portal/session"), json!({"token": tokens[0]})).await;
    assert_eq!((s, v["email"].as_str()), (200, Some("jane@example.org")), "{v}");
    assert_eq!(get(&jane, app.url("/portal")).await["email"], "jane@example.org");
    assert_eq!(get(&jane, app.url("/portal/requests")).await["requests"].as_array().unwrap().len(), 1);
    let (s, v) = send(&visitor("198.51.100.3"), Method::POST, app.url("/portal/session"), json!({"token": tokens[0]})).await;
    assert_eq!(s, 400, "used: {v}");
    assert_eq!(send(&anon, Method::POST, app.url("/portal/session"), json!({"token": "made-up"})).await.0, 400);

    // ... and only for 30 minutes
    assert_eq!(ask(&anon, "jane@example.org").await.0, 204);
    let tokens = wait_links(&app, "jane@example.org", 2).await;
    let lifetime: f64 =
        sqlx::query_scalar("SELECT EXTRACT(EPOCH FROM expires_at - created_at)::float8 FROM portal_links LIMIT 1").fetch_one(&app.state.db).await.unwrap();
    assert!((1790.0..=1800.0).contains(&lifetime), "{lifetime}");
    sqlx::query("UPDATE portal_links SET expires_at = now() - interval '1 second'").execute(&app.state.db).await.unwrap();
    let (s, _) = send(&visitor("198.51.100.4"), Method::POST, app.url("/portal/session"), json!({"token": tokens[1]})).await;
    assert_eq!(s, 400, "expired");

    // three links per address and quarter hour; more asks are answered alike but send nothing
    assert_eq!(ask(&anon, "jane@example.org").await.0, 204);
    wait_links(&app, "jane@example.org", 3).await;
    assert_eq!(ask(&anon, "jane@example.org").await, (204, String::new()));
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(links(&app, "jane@example.org").await.len(), 3, "throttled per address");

    // ten asks per IP address and quarter hour (counted whatever the address)
    let flood = visitor("203.0.113.50");
    for n in 0..taskcraft::portal::SIGN_INS_PER_IP {
        assert_eq!(ask(&flood, &format!("someone{n}@example.org")).await.0, 204);
    }
    let (s, body) = ask(&flood, "jane@example.org").await;
    assert_eq!(s, 429, "{body}");
    assert_eq!(ask(&visitor("203.0.113.51"), "nobody@example.org").await.0, 204, "other addresses aren't affected");

    // signing out ends the session
    let r = jane.delete(app.url("/portal/session")).send().await.unwrap();
    assert_eq!(r.status(), 204);
    assert_eq!(send(&jane, Method::GET, app.url("/portal/requests"), Value::Null).await.0, 401);

    // the portal's cookie is its own: it doesn't sign anyone in to the app
    sqlx::query("DELETE FROM portal_attempts WHERE kind = 'sign-in-email'").execute(&app.state.db).await.unwrap();
    let c = sign_in(&app, "jane@example.org", "198.51.100.9").await;
    let r = c.get(app.url("/me")).send().await.unwrap().json::<Value>().await.unwrap();
    assert!(r["user"].is_null(), "{r}");
}

#[tokio::test(flavor = "multi_thread")]
async fn requesters_see_their_requests_and_answer() {
    let app = portal_app().await;
    let owner = app.setup(true).await;
    let (max, _) = app.user(&owner, "Max Member", "max@example.com", "member").await;
    take_mail(&owner, &app).await;
    let file_mail = "From: Jane Doe <jane@example.org>\r\nTo: support@acme.test\r\nSubject: Printer broken\r\nMessage-ID: <jane-2@example.org>\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=\"XX\"\r\n\r\n--XX\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nThe printer on floor 2 is broken.\r\n--XX\r\nContent-Type: text/plain; name=\"error.log\"\r\nContent-Disposition: attachment; filename=\"error.log\"\r\nContent-Transfer-Encoding: base64\r\n\r\nbGluZSAxCmxpbmUgMgo=\r\n--XX--\r\n";
    let key = deliver(&app, file_mail).await;
    let bobs = write_in(&app, "Bob <bob@example.org>", "Bob's thing", "bob-1@example.org", "Bob's private request").await;

    // the team writes: an internal note, a comment for her (new and earlier API name) and a file
    let owner_id = get(&owner, app.url("/me")).await["user"]["id"].as_str().unwrap().to_string();
    let mention = json!({"type": "doc", "content": [{"type": "paragraph", "content": [
        {"type": "mention", "attrs": {"id": owner_id, "label": "Olivia Owner"}},
        {"type": "text", "text": " will replace the toner."}
    ]}]});
    post(&max, app.url(&format!("/issues/{key}/comments")), json!({"markdown": "Internal: the printer is out of warranty"})).await;
    let r = post(&max, app.url(&format!("/issues/{key}/comments")), json!({"body": mention, "forRequester": true})).await;
    assert_eq!((r["forRequester"].as_bool(), r["emailed"].as_bool()), (Some(true), Some(true)), "{r}");
    let r = post(&max, app.url(&format!("/issues/{key}/comments")), json!({"markdown": "We ordered a **new** one.", "emailRequester": true})).await;
    assert_eq!(r["forRequester"], true);
    let staff_file = max
        .post(app.url(&format!("/issues/{key}/attachments")))
        .multipart(Form::new().part("file", Part::bytes(b"internal".to_vec()).file_name("quote.pdf")))
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    let staff_file = staff_file[0]["id"].as_str().unwrap().to_string();
    let mailed: String = sqlx::query_scalar("SELECT body_text FROM mail_outbox WHERE purpose = 'requester' ORDER BY created_at DESC LIMIT 1")
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    assert!(mailed.contains(&format!("{}/portal/requests/{key}", app.base)), "replies link to the portal: {mailed}");
    // staff see which comments she sees
    let list = get(&owner, app.url(&format!("/issues/{key}/comments"))).await;
    let flags: Vec<bool> = list.as_array().unwrap().iter().map(|c| c["forRequester"].as_bool().unwrap()).collect();
    assert_eq!(flags, vec![false, true, true]);

    let jane = sign_in(&app, "jane@example.org", "198.51.100.10").await;
    let mine = get(&jane, app.url("/portal/requests")).await;
    let requests = mine["requests"].as_array().unwrap();
    assert_eq!(requests.len(), 1, "only her own: {mine}");
    assert_eq!(requests[0]["key"], key.as_str());
    assert_eq!(requests[0]["title"], "Printer broken");
    assert_eq!(requests[0]["state"], "open");
    assert_eq!(requests[0]["project"], "Customer Support");
    assert_eq!(send(&jane, Method::GET, app.url(&format!("/portal/requests/{bobs}")), Value::Null).await.0, 404, "not Bob's");
    assert_eq!(send(&jane, Method::GET, app.url("/portal/requests/WEB-1"), Value::Null).await.0, 404, "nor anything else");
    assert_eq!(send(&jane, Method::GET, app.url(&format!("/portal/requests/{}", key.to_lowercase())), Value::Null).await.0, 200);

    let r = get(&jane, app.url(&format!("/portal/requests/{key}"))).await;
    assert_eq!(text_of(&r["description"]), "The printer on floor 2 is broken.");
    let messages = r["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 2, "no internal notes: {r}");
    assert_eq!(text_of(&messages[0]["body"]), "@Olivia Owner will replace the toner.");
    assert_eq!(messages[0]["author"], "Max Member");
    assert_eq!(messages[0]["mine"], false);
    assert_eq!(text_of(&messages[1]["body"]), "We ordered a new one.");
    let all = r.to_string();
    for secret in ["Internal", "max@example.com", "owner@example.com", owner_id.as_str(), "quote.pdf", "warranty"] {
        assert!(!all.contains(secret), "{secret} shown: {all}");
    }
    let files = r["files"].as_array().unwrap();
    assert_eq!(files.len(), 1, "her own files only: {files:?}");
    assert_eq!(files[0]["name"], "error.log");
    let data = jane.get(format!("{}{}", app.base, files[0]["url"].as_str().unwrap())).send().await.unwrap();
    assert_eq!(data.status(), 200);
    assert_eq!(&data.bytes().await.unwrap()[..], b"line 1\nline 2\n");
    assert_eq!(jane.get(app.url(&format!("/portal/attachments/{staff_file}"))).send().await.unwrap().status(), 404);
    assert_eq!(r["canReply"], true);
    assert_eq!(r["canResolve"], false);

    // Bob can't see her file either
    let bob = sign_in(&app, "bob@example.org", "198.51.100.11").await;
    assert_eq!(bob.get(format!("{}{}", app.base, files[0]["url"].as_str().unwrap())).send().await.unwrap().status(), 404);
    assert_eq!(send(&bob, Method::GET, app.url(&format!("/portal/requests/{key}")), Value::Null).await.0, 404);

    // she answers with a file: a comment by her, like an answer by e-mail
    let form = Form::new()
        .text("text", "Here is a photo.\r\n\r\nThanks!")
        .part("files", Part::bytes(b"PNG".to_vec()).file_name("photo.png").mime_str("image/png").unwrap());
    let r = jane.post(app.url(&format!("/portal/requests/{key}/comments"))).multipart(form).send().await.unwrap();
    assert_eq!(r.status(), 201, "{}", r.text().await.unwrap());
    let list = get(&owner, app.url(&format!("/issues/{key}/comments"))).await;
    let hers = list.as_array().unwrap().last().unwrap().clone();
    assert!(hers["author"].is_null());
    assert_eq!(hers["external"], json!({"email": "jane@example.org", "name": "Jane Doe"}));
    assert_eq!((hers["viaPortal"].as_bool(), hers["viaEmail"].as_bool(), hers["forRequester"].as_bool()), (Some(true), Some(false), Some(true)));
    assert_eq!(text_of(&hers["body"]), "Here is a photo.\nThanks!\n📎 photo.png");
    let issue = get(&owner, app.url(&format!("/issues/{key}"))).await;
    assert!(issue["attachments"].as_array().unwrap().iter().any(|a| a["name"] == "photo.png"));
    let history = get(&owner, app.url(&format!("/issues/{key}/history"))).await;
    assert!(history.as_array().unwrap().iter().any(|e| e["kind"] == "comment" && e["new"]["by"] == "Jane Doe"), "{history}");
    let r = get(&jane, app.url(&format!("/portal/requests/{key}"))).await;
    let last = r["messages"].as_array().unwrap().last().unwrap().clone();
    assert_eq!((last["mine"].as_bool(), last["author"].is_null()), (Some(true), true));
    assert_eq!(r["files"].as_array().unwrap().len(), 2);
    // her own messages by e-mail are hers too
    write_in(&app, "jane@example.org", &format!("Re: [{key}] Printer broken"), "jane-3@example.org", "One more thing.").await;
    let r = get(&jane, app.url(&format!("/portal/requests/{key}"))).await;
    assert_eq!(r["messages"].as_array().unwrap().last().unwrap()["mine"], true);

    // limits: something to say, a few files, a size
    let empty = jane.post(app.url(&format!("/portal/requests/{key}/comments"))).multipart(Form::new().text("text", "  ")).send().await.unwrap();
    assert_eq!(empty.status(), 400);
    let mut many = Form::new().text("text", "files");
    for n in 0..=taskcraft::portal::MAX_FILES {
        many = many.part("files", Part::bytes(b"x".to_vec()).file_name(format!("f{n}.txt")));
    }
    assert_eq!(jane.post(app.url(&format!("/portal/requests/{key}/comments"))).multipart(many).send().await.unwrap().status(), 400);
    let big = Form::new().part("files", Part::bytes(vec![0u8; 17 * 1024 * 1024]).file_name("big.bin"));
    assert_eq!(jane.post(app.url(&format!("/portal/requests/{key}/comments"))).multipart(big).send().await.unwrap().status(), 413);
    assert_eq!(get(&jane, app.url(&format!("/portal/requests/{key}"))).await["files"].as_array().unwrap().len(), 2, "nothing stored");
    // not without signing in, not someone else's request, not without the CSRF header
    let r = reqwest::Client::new().post(app.url(&format!("/portal/requests/{key}/comments"))).multipart(Form::new().text("text", "x")).send().await.unwrap();
    assert!(r.status() == 400 || r.status() == 401, "{}", r.status());
    let r = bob.post(app.url(&format!("/portal/requests/{key}/comments"))).multipart(Form::new().text("text", "Hi Jane")).send().await.unwrap();
    assert_eq!(r.status(), 404);

    // resolving: only where the project allows it
    let (s, _) = send(&jane, Method::POST, app.url(&format!("/portal/requests/{key}/resolve")), json!({})).await;
    assert_eq!(s, 403);
    let (s, v) = send(&max, Method::PUT, app.url("/projects/SUP/portal"), json!({"resolve": true})).await;
    assert_eq!(s, 403, "project admins only: {v}");
    let (s, v) = send(&owner, Method::PUT, app.url("/projects/SUP/portal"), json!({"resolve": true})).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(get(&jane, app.url(&format!("/portal/requests/{key}"))).await["canResolve"], true);
    let (s, v) = send(&jane, Method::POST, app.url(&format!("/portal/requests/{key}/resolve")), json!({"text": "Works again, thanks."})).await;
    assert_eq!((s, v["state"].as_str()), (200, Some("resolved")), "{v}");
    let issue = get(&owner, app.url(&format!("/issues/{key}"))).await;
    assert_eq!(issue["status"]["category"], "done");
    assert_eq!(issue["status"]["name"], "Resolved", "the first done status");
    let history = get(&owner, app.url(&format!("/issues/{key}/history"))).await;
    assert!(history.as_array().unwrap().iter().any(|e| e["field"] == "status" && e["new"]["by"] == "Jane Doe"), "{history}");
    let r = get(&jane, app.url(&format!("/portal/requests/{key}"))).await;
    assert_eq!((r["state"].as_str(), r["canResolve"].as_bool()), (Some("resolved"), Some(false)));
    assert_eq!(text_of(&r["messages"].as_array().unwrap().last().unwrap()["body"]), "Works again, thanks.");
    assert_eq!(get(&jane, app.url("/portal/requests")).await["requests"][0]["state"], "resolved");

    // deleted requests disappear
    send(&owner, Method::DELETE, app.url(&format!("/issues/{key}")), Value::Null).await;
    assert_eq!(send(&jane, Method::GET, app.url(&format!("/portal/requests/{key}")), Value::Null).await.0, 404);
    assert!(get(&jane, app.url("/portal/requests")).await["requests"].as_array().unwrap().is_empty());
}

/// A form stamp from `secs` seconds ago (people take a moment to fill a form in).
async fn stamp(app: &TestApp, project: &str, secs: i64) -> String {
    taskcraft::portal::form_stamp(&app.state, project.parse().unwrap(), chrono::Utc::now().timestamp() - secs).await.unwrap()
}

async fn issues_count(app: &TestApp) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM issues").fetch_one(&app.state.db).await.unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn request_forms_open_requests_and_keep_bots_out() {
    let app = portal_app().await;
    let owner = app.setup(true).await;
    let (max, _) = app.user(&owner, "Max Member", "max@example.com", "member").await;
    let sup = get(&owner, app.url("/projects/SUP")).await;
    let project = sup["id"].as_str().unwrap().to_string();
    let ty = |name: &str| sup["types"].as_array().unwrap().iter().find(|t| t["name"] == name).unwrap()["id"].as_str().unwrap().to_string();
    let (bug, question, subtask) = (ty("Bug"), ty("Question"), ty("Sub-task"));
    let field = |name: &str| sup["fields"].as_array().unwrap().iter().find(|f| f["name"] == name).unwrap()["id"].as_str().unwrap().to_string();
    let (severity, environment) = (field("Severity"), field("Environment"));
    let person = post(&owner, app.url("/projects/SUP/fields"), json!({"name": "Contact", "kind": "user"})).await["id"].as_str().unwrap().to_string();

    // no form yet
    let anon = visitor("198.51.100.20");
    assert_eq!(send(&anon, Method::GET, app.url("/portal/forms/sup"), Value::Null).await.0, 404);

    // settings: project admins; types, fields and the address are checked
    let form = json!({
        "enabled": true, "slug": "Help-Desk", "title": "Ask for help", "intro": "Tell us what's wrong.", "success": "Thanks, we're on it.",
        "description": "required", "name": "optional", "types": [bug, question], "priority": true,
        "custom": [{"id": severity, "required": true}, {"id": environment}],
    });
    assert_eq!(send(&max, Method::PUT, app.url("/projects/SUP/portal"), json!({"form": form})).await.0, 403);
    let mut bad = form.clone();
    bad["types"] = json!([bug, subtask]);
    assert_eq!(send(&owner, Method::PUT, app.url("/projects/SUP/portal"), json!({"form": bad})).await.0, 400, "no sub-tasks");
    let mut bad = form.clone();
    bad["custom"] = json!([{"id": person}]);
    assert_eq!(send(&owner, Method::PUT, app.url("/projects/SUP/portal"), json!({"form": bad})).await.0, 400, "no people fields");
    let mut bad = form.clone();
    bad["slug"] = json!("web");
    assert_eq!(send(&owner, Method::PUT, app.url("/projects/SUP/portal"), json!({"form": bad})).await.0, 409, "another project's key");
    let mut bad = form.clone();
    bad["slug"] = json!("help desk!");
    assert_eq!(send(&owner, Method::PUT, app.url("/projects/SUP/portal"), json!({"form": bad})).await.0, 400);
    let (s, v) = send(&owner, Method::PUT, app.url("/projects/SUP/portal"), json!({"form": form})).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["settings"]["form"]["slug"], "help-desk");
    assert_eq!(v["formUrl"], format!("{}/portal/forms/help-desk", app.base));
    assert_eq!(get(&owner, app.url("/projects/SUP/portal")).await["settings"]["form"]["title"], "Ask for help");

    // the public form, by its address or the project key; listed on the portal
    let f = get(&anon, app.url("/portal/forms/help-desk")).await;
    assert_eq!(f["title"], "Ask for help");
    assert_eq!(f["project"], "Customer Support");
    assert_eq!(f["description"], "required");
    assert_eq!(f["types"].as_array().unwrap().len(), 2);
    let fields = f["fields"].as_array().unwrap();
    assert_eq!(fields.len(), 2);
    assert_eq!((fields[0]["name"].as_str(), fields[0]["required"].as_bool()), (Some("Severity"), Some(true)));
    assert_eq!(fields[0]["options"][0], json!({"id": "s1", "label": "Critical"}));
    assert!(f["stamp"].as_str().unwrap().contains('.'));
    assert_eq!(get(&anon, app.url("/portal/forms/SUP")).await["path"], "help-desk");
    assert_eq!(send(&anon, Method::GET, app.url("/portal/forms/web"), Value::Null).await.0, 404, "WEB has no form");
    let info = get(&anon, app.url("/portal")).await;
    assert_eq!(info["forms"], json!([{"path": "help-desk", "title": "Ask for help", "project": "Customer Support"}]));

    let good = |stamp: &str| {
        json!({
            "stamp": stamp, "email": "Carla@Example.org", "name": "Carla  Client", "summary": "  VPN drops   every hour ",
            "description": "Since Monday.\r\nEvery hour.", "typeId": question, "priority": "high",
            "custom": {severity.clone(): "s2", environment.clone(): "Windows 11"}, "website": "",
        })
    };
    let submit = |c: &reqwest::Client, body: Value| {
        let req = c.post(app.url("/portal/forms/help-desk")).json(&body);
        async move {
            let r = req.send().await.unwrap();
            let s = r.status().as_u16();
            (s, r.json::<Value>().await.unwrap_or(Value::Null))
        }
    };
    let before = issues_count(&app).await;

    // bots: a fresh form sent at once, a forged or stale stamp, the hidden field
    let (s, v) = submit(&anon, good(f["stamp"].as_str().unwrap())).await;
    assert_eq!(s, 400, "too fast: {v}");
    assert!(v["message"].as_str().unwrap().contains("quick"));
    assert_eq!(submit(&anon, good("123.abc")).await.0, 400);
    assert_eq!(submit(&anon, good(&stamp(&app, &project, 2 * 24 * 3600).await)).await.0, 400, "opened two days ago");
    let other_project = get(&owner, app.url("/projects/WEB")).await["id"].as_str().unwrap().to_string();
    assert_eq!(submit(&anon, good(&stamp(&app, &other_project, 60).await)).await.0, 400, "another form's stamp");
    let mut honey = good(&stamp(&app, &project, 60).await);
    honey["website"] = json!("https://spam.example");
    let (s, v) = submit(&anon, honey).await;
    assert_eq!((s, v), (201, json!({"ok": true, "request": null})), "looks fine to the bot");
    assert_eq!(issues_count(&app).await, before, "but nothing was created");
    let no_csrf = reqwest::Client::new().post(app.url("/portal/forms/help-desk")).json(&good(&stamp(&app, &project, 60).await)).send().await.unwrap();
    assert_eq!(no_csrf.status(), 400);

    // what the form asks for
    let st = stamp(&app, &project, 60).await;
    for (change, why) in [
        (json!({"email": "nope"}), "an address"),
        (json!({"summary": " "}), "a summary"),
        (json!({"summary": "x".repeat(201)}), "a short summary"),
        (json!({"description": ""}), "the required description"),
        (json!({"description": "x".repeat(20_001)}), "a description of reasonable size"),
        (json!({"typeId": bug.clone() + "x"}), "an offered type"),
        (json!({"typeId": ty("Task")}), "an offered type"),
        (json!({"priority": "none"}), "a priority"),
        (json!({"custom": {severity.clone(): "s9"}}), "an option of the field"),
        (json!({"custom": {environment.clone(): "x"}}), "the required field"),
        (json!({"custom": {severity.clone(): "s1", person.clone(): "x"}}), "only the form's fields"),
    ] {
        let mut body = good(&st);
        for (k, v) in change.as_object().unwrap() {
            body[k] = v.clone();
        }
        let (s, v) = submit(&anon, body).await;
        assert_eq!(s, 400, "needs {why}: {v}");
    }
    assert_eq!(issues_count(&app).await, before);

    // a good one: an issue with the submitter as requester, who gets an acknowledgement
    let (s, v) = submit(&anon, good(&st)).await;
    assert_eq!((s, v.clone()), (201, json!({"ok": true, "request": null})), "the key is only for signed-in requesters");
    let key: String = sqlx::query_scalar(
        "SELECT p.key || '-' || i.number FROM issue_requesters r JOIN issues i ON i.id = r.issue_id JOIN projects p ON p.id = i.project_id WHERE r.email = 'carla@example.org'",
    )
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    let issue = get(&owner, app.url(&format!("/issues/{key}"))).await;
    assert_eq!(issue["title"], "VPN drops every hour");
    assert_eq!(text_of(&issue["description"]), "Since Monday.\nEvery hour.");
    assert_eq!(issue["type"]["name"], "Question");
    assert_eq!(issue["priority"], "high");
    assert_eq!(issue["custom"][&severity], "s2");
    assert_eq!(issue["custom"][&environment], "Windows 11");
    assert!(issue["reporter"].is_null());
    assert_eq!(issue["requester"], json!({"email": "carla@example.org", "name": "Carla Client", "mail": true, "source": "form"}));
    let history = get(&owner, app.url(&format!("/issues/{key}/history"))).await;
    assert!(history.as_array().unwrap().iter().any(|e| e["kind"] == "created" && e["new"]["by"] == "Carla Client"), "{history}");
    let ack: (String, String, Value) =
        sqlx::query_as("SELECT subject, body_text, headers FROM mail_outbox WHERE purpose = 'mail-in-ack' AND to_address = 'carla@example.org'")
            .fetch_one(&app.state.db)
            .await
            .unwrap();
    assert_eq!(ack.0, format!("[{key}] VPN drops every hour"));
    assert!(ack.1.starts_with("Hi Carla Client,") && ack.1.contains(&format!("{}/portal/requests/{key}", app.base)), "{}", ack.1);
    assert!(ack.1.contains("don't reach us"), "the project doesn't take e-mail: {}", ack.1);
    assert!(ack.2.get("replyTo").is_none(), "{}", ack.2);

    // she can follow it in the portal
    let carla = sign_in(&app, "carla@example.org", "198.51.100.21").await;
    assert_eq!(get(&carla, app.url("/portal/requests")).await["requests"][0]["key"], key.as_str());

    // signed in, the form takes her portal address and tells her the key
    let body = json!({"stamp": stamp(&app, &project, 30).await, "summary": "Second thing", "description": "More.", "typeId": bug,
                      "custom": {severity.clone(): "s3"}, "email": "someone-else@example.org"});
    let (s, v) = submit(&carla, body).await;
    assert_eq!(s, 201, "{v}");
    let second = v["request"].as_str().expect("the key").to_string();
    let issue = get(&owner, app.url(&format!("/issues/{second}"))).await;
    assert_eq!(issue["requester"]["email"], "carla@example.org");
    assert_eq!(issue["requester"]["source"], "portal");
    assert_eq!(issue["type"]["name"], "Bug");
    assert_eq!(issue["priority"], "medium", "no priority chosen: the default");

    // rate limits: per e-mail address and per IP address
    for n in 0..3 {
        let mut body = good(&stamp(&app, &project, 60).await);
        body["summary"] = json!(format!("Again {n}"));
        assert_eq!(submit(&visitor(&format!("198.51.100.{}", 30 + n)), body).await.0, 201);
    }
    let (s, v) = submit(&visitor("198.51.100.40"), good(&stamp(&app, &project, 60).await)).await;
    assert_eq!(s, 429, "five per address and hour: {v}");
    for _ in 0..taskcraft::portal::FORMS_PER_IP {
        sqlx::query("INSERT INTO portal_attempts (kind, key) VALUES ('form-ip', '203.0.113.77')").execute(&app.state.db).await.unwrap();
    }
    let mut body = good(&stamp(&app, &project, 60).await);
    body["email"] = json!("dave@example.org");
    assert_eq!(submit(&visitor("203.0.113.77"), body.clone()).await.0, 429, "per IP");
    assert_eq!(submit(&visitor("203.0.113.78"), body).await.0, 201, "another IP is fine");

    // switched off: the form is gone
    let mut off = form.clone();
    off["enabled"] = json!(false);
    assert_eq!(send(&owner, Method::PUT, app.url("/projects/SUP/portal"), json!({"form": off})).await.0, 200);
    let n = issues_count(&app).await;
    assert_eq!(send(&anon, Method::GET, app.url("/portal/forms/help-desk"), Value::Null).await.0, 404);
    let mut body = good(&stamp(&app, &project, 60).await);
    body["email"] = json!("erin@example.org");
    assert_eq!(submit(&visitor("198.51.100.50"), body).await.0, 404);
    assert_eq!(issues_count(&app).await, n);
    assert!(get(&anon, app.url("/portal")).await["forms"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn the_form_suggests_articles_from_a_wiki_space() {
    use axum::Json;
    // a stand-in for WikiCraft's search: pages of two spaces
    let wiki = axum::Router::new().route(
        "/api/v1/search",
        axum::routing::get(|axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>| async move {
            assert_eq!(q.get("space").map(String::as_str), Some("HELP"));
            Json(json!([
                {"id": uuid::Uuid::now_v7(), "title": "Printer offline", "spaceKey": "HELP", "spaceName": "Help", "snippet": "Turn the <mark>printer</mark> off and on"},
                {"id": uuid::Uuid::now_v7(), "title": "Internal printer notes", "spaceKey": "ENG", "spaceName": "Engineering"},
                {"id": uuid::Uuid::now_v7(), "title": "Paper jams", "spaceKey": "HELP", "spaceName": "Help"},
            ]))
        }),
    );
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let wiki_url = format!("http://{}", l.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(l, wiki).await.unwrap() });
    let w = wiki_url.clone();
    let app = spawn_with(move |c| {
        c.wikicraft_url = Some(w);
        c.wikicraft_token = Some("wct_test".into());
    })
    .await;
    let owner = app.setup(false).await;
    post(&owner, app.url("/projects"), json!({"key": "HLP", "name": "Help desk", "template": "kanban"})).await;
    let anon = visitor("198.51.100.30");
    send(&owner, Method::PUT, app.url("/projects/HLP/portal"), json!({"form": {"enabled": true}})).await;
    assert_eq!(get(&anon, app.url("/portal/forms/hlp")).await["suggest"], false, "no space chosen");
    assert_eq!(get(&anon, app.url("/portal/forms/hlp/suggest?q=printer")).await, json!([]));

    let (s, _) = send(&owner, Method::PUT, app.url("/projects/HLP/portal"), json!({"form": {"enabled": true, "suggestSpace": " HELP "}})).await;
    assert!(s < 300);
    assert_eq!(get(&anon, app.url("/portal/forms/hlp")).await["suggest"], true);
    let found = get(&anon, app.url("/portal/forms/hlp/suggest?q=printer")).await;
    let titles: Vec<&str> = found.as_array().unwrap().iter().map(|p| p["title"].as_str().unwrap()).collect();
    assert_eq!(titles, ["Printer offline", "Paper jams"], "that space only");
    assert_eq!(found[0]["excerpt"], "Turn the printer off and on");
    assert!(found[0]["url"].as_str().unwrap().starts_with(&format!("{wiki_url}/pages/")));
    assert_eq!(get(&anon, app.url("/portal/forms/hlp/suggest?q=pr")).await, json!([]), "too short");
}
