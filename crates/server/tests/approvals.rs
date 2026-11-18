//! Workflow approvals: a status that waits for a decision — steps and quorums, who is asked
//! (not whoever asked), the two moves nobody takes by hand, delegating, the link in the e-mail,
//! substitutes, timeouts, TQL, automation, and requesters deciding on the portal.

mod common;

use chrono::{Duration, Utc};
use common::*;
use reqwest::Method;
use serde_json::{Value, json};

fn status_id(p: &Value, name: &str) -> String {
    p["statuses"].as_array().unwrap().iter().find(|s| s["name"] == name).unwrap_or_else(|| panic!("no status {name}"))["id"].as_str().unwrap().to_string()
}

fn enc(s: &str) -> String {
    s.bytes().map(|b| if b.is_ascii_alphanumeric() { (b as char).to_string() } else { format!("%{b:02X}") }).collect()
}

async fn found(app: &TestApp, c: &reqwest::Client, tql: &str) -> Vec<String> {
    let r = get(c, app.url(&format!("/search?q={}", enc(tql)))).await;
    let mut keys: Vec<String> = r["issues"].as_array().unwrap().iter().map(|i| i["key"].as_str().unwrap().to_string()).collect();
    keys.sort();
    keys
}

async fn status_of(app: &TestApp, c: &reqwest::Client, key: &str) -> String {
    get(c, app.url(&format!("/issues/{key}"))).await["status"]["name"].as_str().unwrap().to_string()
}

async fn decide(app: &TestApp, c: &reqwest::Client, approval: &Value, decision: &str, comment: Option<&str>) -> (u16, Value) {
    send(c, Method::POST, app.url(&format!("/approvals/{}/decide", approval["id"].as_str().unwrap())), json!({"decision": decision, "comment": comment})).await
}

/// The issue's latest approval request.
async fn request(app: &TestApp, c: &reqwest::Client, key: &str) -> Value {
    get(c, app.url(&format!("/issues/{key}/approvals"))).await[0].clone()
}

#[tokio::test]
async fn steps_quorums_delegation_and_what_follows() {
    let app = spawn().await;
    let owner = app.setup(false).await;
    post(&owner, app.url("/projects"), json!({"key": "CHG", "name": "Changes", "template": "scrum"})).await;
    let p = get(&owner, app.url("/projects/CHG")).await;
    let (todo, doing, review, done) = (status_id(&p, "To do"), status_id(&p, "In progress"), status_id(&p, "In review"), status_id(&p, "Done"));
    let owner_id = get(&owner, app.url("/me")).await["user"]["id"].as_str().unwrap().to_string();
    let (rita, _) = app.user(&owner, "Rita Requester", "rita@example.com", "member").await;
    let (mia, mia_id) = app.user(&owner, "Mia Member", "mia@example.com", "member").await;
    let (tom, tom_id) = app.user(&owner, "Tom Teammate", "tom@example.com", "member").await;
    let (ann, ann_id) = app.user(&owner, "Ann Approver", "ann@example.com", "member").await;
    let (dan, dan_id) = app.user(&owner, "Dan Deputy", "dan@example.com", "member").await;
    let team = post(&owner, app.url("/teams"), json!({"name": "Peers", "memberIds": [mia_id, tom_id]})).await;

    // the settings are checked
    let url = app.url(&format!("/statuses/{review}"));
    for (bad, why) in [
        (json!({"steps": [], "approvedStatusId": done, "rejectedStatusId": todo}), "1 to 5 steps"),
        (json!({"steps": [{"approvers": [{"kind": "assignee"}]}], "approvedStatusId": review, "rejectedStatusId": todo}), "another status"),
        (json!({"steps": [{"approvers": []}], "approvedStatusId": done, "rejectedStatusId": todo}), "who approves"),
        (json!({"steps": [{"approvers": [{"kind": "assignee"}], "quorum": "count"}], "approvedStatusId": done, "rejectedStatusId": todo}), "how many"),
        (json!({"steps": [{"approvers": [{"kind": "boss"}]}], "approvedStatusId": done, "rejectedStatusId": todo}), "unknown kind"),
        (
            json!({"steps": [{"approvers": [{"kind": "assignee"}]}], "approvedStatusId": done, "rejectedStatusId": todo, "timeout": {"hours": 2, "action": "reassign"}}),
            "timeout",
        ),
    ] {
        let (s, v) = send(&owner, Method::PATCH, url.clone(), json!({"approval": bad})).await;
        assert!(s == 400 && v["message"].as_str().unwrap().contains(why), "{why}: {s} {v}");
    }
    assert_eq!(send(&mia, Method::PATCH, url.clone(), json!({"approval": null})).await.0, 403, "project admins only");
    // two of the three peers (the team and Ann), then the owner
    patch(
        &owner,
        url.clone(),
        json!({"approval": {
            "steps": [
                {"name": "Peers", "approvers": [{"kind": "team", "id": team["id"]}, {"kind": "user", "id": ann_id}, {"kind": "reporter"}], "quorum": "count", "value": 2, "rejection": "quorum"},
                {"name": "Lead", "approvers": [{"kind": "user", "id": owner_id}]},
            ],
            "approvedStatusId": done, "rejectedStatusId": todo,
        }}),
    )
    .await;
    let p = get(&owner, app.url("/projects/CHG")).await;
    let saved = &p["statuses"].as_array().unwrap().iter().find(|s| s["name"] == "In review").unwrap()["approval"];
    assert_eq!(saved["steps"][0]["quorum"], "count");
    assert_eq!((saved["parallel"].as_bool(), saved["selfApproval"].as_bool()), (Some(false), Some(false)));
    post(
        &owner,
        app.url("/projects/CHG/automation"),
        json!({"name": "Signed off", "trigger": {"event": "approved"}, "condition": "", "actions": [{"action": "addLabel", "label": "signed-off"}]}),
    )
    .await;
    post(
        &owner,
        app.url("/projects/CHG/automation"),
        json!({"name": "Sent back", "trigger": {"event": "rejected"}, "condition": "", "actions": [{"action": "addLabel", "label": "sent-back"}]}),
    )
    .await;

    // Rita asks: the peers are asked, not she herself (the reporter), nor the lead yet
    post(&rita, app.url("/issues"), json!({"projectKey": "CHG", "title": "Rotate the signing keys"})).await;
    assert!(get(&rita, app.url("/issues/CHG-1")).await["approval"].is_null());
    patch(&rita, app.url("/issues/CHG-1"), json!({"statusId": review})).await;
    let i = get(&rita, app.url("/issues/CHG-1")).await;
    assert_eq!(i["approval"]["statusName"], "In review");
    let a = request(&app, &rita, "CHG-1").await;
    assert_eq!(a["state"], "pending");
    assert_eq!(a["requestedBy"]["name"], "Rita Requester");
    assert_eq!((a["steps"][0]["state"].as_str(), a["steps"][1]["state"].as_str()), (Some("pending"), Some("waiting")));
    let mut asked: Vec<&str> = a["steps"][0]["approvers"].as_array().unwrap().iter().map(|v| v["user"]["name"].as_str().unwrap()).collect();
    asked.sort();
    assert_eq!(asked, ["Ann Approver", "Mia Member", "Tom Teammate"]);
    assert_eq!(a["steps"][0]["needed"], 2);
    assert_eq!(a["canDecide"], json!([]), "not whoever asked");
    assert_eq!(request(&app, &mia, "CHG-1").await["canDecide"].as_array().unwrap().len(), 1);
    assert_eq!(found(&app, &owner, "approval = pending").await, ["CHG-1"]);
    assert_eq!(found(&app, &owner, "approver = \"Mia Member\"").await, ["CHG-1"]);
    assert_eq!(found(&app, &mia, "approver = me()").await, ["CHG-1"]);
    assert!(found(&app, &owner, "approver = me()").await.is_empty(), "the lead's turn comes later");
    assert!(found(&app, &owner, "approval is empty").await.is_empty());
    let n = get(&mia, app.url("/notifications")).await;
    assert!(n["items"].as_array().unwrap().iter().any(|x| x["kind"] == "approval" && x["data"]["step"] == "Peers"), "{n}");
    let mine = get(&mia, app.url("/approvals")).await;
    assert_eq!((mine.as_array().unwrap().len(), mine[0]["issue"]["key"].as_str()), (1, Some("CHG-1")));
    assert!(get(&owner, app.url("/approvals")).await.as_array().unwrap().is_empty());
    assert_eq!(get(&rita, app.url("/approvals?scope=requested")).await[0]["issue"]["key"], "CHG-1");

    // nobody takes the two moves by hand, and only those asked decide
    for to in [&done, &todo] {
        let (s, v) = send(&owner, Method::PATCH, app.url("/issues/CHG-1"), json!({"statusId": to})).await;
        assert!(s == 403 && v["message"].as_str().unwrap().contains("waits for approval"), "{s} {v}");
    }
    assert_eq!(decide(&app, &rita, &a, "approve", None).await.0, 403);
    assert_eq!(decide(&app, &owner, &a, "approve", None).await.0, 403, "not the lead's turn yet");
    assert_eq!(decide(&app, &mia, &a, "maybe", None).await.0, 400);

    // one yes, one no: two of three is still possible; a second yes passes the step on to the lead
    assert_eq!(decide(&app, &mia, &a, "approve", Some("Looks right")).await.0, 204);
    assert_eq!(decide(&app, &mia, &a, "approve", None).await.0, 403, "one vote each");
    assert_eq!(decide(&app, &tom, &a, "reject", Some("Not during the freeze")).await.0, 204);
    let a = request(&app, &owner, "CHG-1").await;
    assert_eq!((a["state"].as_str(), a["steps"][0]["state"].as_str()), (Some("pending"), Some("pending")));
    assert_eq!(decide(&app, &ann, &a, "approve", None).await.0, 204);
    let a = request(&app, &owner, "CHG-1").await;
    assert_eq!((a["steps"][0]["state"].as_str(), a["steps"][1]["state"].as_str()), (Some("approved"), Some("pending")));
    assert_eq!(a["canDecide"].as_array().unwrap().len(), 1);
    assert_eq!(found(&app, &owner, "approver = me()").await, ["CHG-1"]);
    assert_eq!(status_of(&app, &owner, "CHG-1").await, "In review");

    // the lead hands the decision to Dan (not to Rita, who asked)
    let delegate = app.url(&format!("/approvals/{}/delegate", a["id"].as_str().unwrap()));
    let rita_id = get(&rita, app.url("/me")).await["user"]["id"].clone();
    assert_eq!(send(&owner, Method::POST, delegate.clone(), json!({"userId": rita_id})).await.0, 400);
    assert_eq!(send(&mia, Method::POST, delegate.clone(), json!({"userId": dan_id})).await.0, 403, "she has decided already");
    assert_eq!(send(&owner, Method::POST, delegate.clone(), json!({"userId": dan_id, "comment": "On leave from tomorrow"})).await.0, 204);
    assert!(get(&owner, app.url("/approvals")).await.as_array().unwrap().is_empty());
    let a = request(&app, &dan, "CHG-1").await;
    assert_eq!(a["steps"][1]["approvers"][0]["for"]["name"], "Olivia Owner");
    assert_eq!(decide(&app, &dan, &a, "approve", Some("Go ahead")).await.0, 204);

    // approved: the issue moved on, Rita hears of it, the rule ran, the history tells the story
    let i = get(&rita, app.url("/issues/CHG-1")).await;
    assert_eq!(i["status"]["name"], "Done");
    assert!(i["approval"].is_null());
    assert_eq!(i["labels"], json!(["signed-off"]));
    assert_eq!(request(&app, &rita, "CHG-1").await["state"], "approved");
    let n = get(&rita, app.url("/notifications")).await;
    assert!(n["items"].as_array().unwrap().iter().any(|x| x["kind"] == "approvalResult" && x["data"]["result"] == "approved"), "{n}");
    let history = get(&rita, app.url("/issues/CHG-1/history")).await.to_string();
    for part in ["requested", "voteApproved", "voteRejected", "Not during the freeze", "delegated", "Dan Deputy", "\"approved\""] {
        assert!(history.contains(part), "{part} in {history}");
    }
    assert_eq!(found(&app, &owner, "approval = approved").await, ["CHG-1"]);
    assert!(found(&app, &owner, "approval = pending").await.is_empty());
    assert_eq!(decide(&app, &dan, &a, "approve", None).await.0, 409, "decided");
    let audit = get(&owner, app.url("/admin/audit")).await.to_string();
    assert!(audit.contains("approval.approved") && audit.contains("approval.delegated"), "{audit}");

    // a rejection that makes the quorum impossible sends it back
    post(&rita, app.url("/issues"), json!({"projectKey": "CHG", "title": "Open the firewall", "statusId": review})).await;
    let a = request(&app, &rita, "CHG-2").await;
    assert_eq!(a["state"], "pending", "created in the status: asked right away");
    assert_eq!(decide(&app, &mia, &a, "reject", None).await.0, 204);
    assert_eq!(status_of(&app, &owner, "CHG-2").await, "In review");
    assert_eq!(decide(&app, &ann, &a, "reject", Some("Too broad")).await.0, 204);
    let i = get(&rita, app.url("/issues/CHG-2")).await;
    assert_eq!((i["status"]["name"].as_str(), &i["labels"]), (Some("To do"), &json!(["sent-back"])));
    assert_eq!(found(&app, &owner, "approval = rejected").await, ["CHG-2"]);

    // leaving the status another way cancels the request; coming back asks again
    patch(&rita, app.url("/issues/CHG-2"), json!({"statusId": review})).await;
    patch(&rita, app.url("/issues/CHG-2"), json!({"statusId": doing})).await;
    let all = get(&rita, app.url("/issues/CHG-2/approvals")).await;
    assert_eq!((all[0]["state"].as_str(), all[1]["state"].as_str()), (Some("cancelled"), Some("rejected")));
    assert!(get(&mia, app.url("/approvals")).await.as_array().unwrap().is_empty());

    // deleting a status an approval leads to turns the approval off
    assert_eq!(send(&owner, Method::DELETE, app.url(&format!("/statuses/{todo}?moveTo={doing}")), Value::Null).await.0, 204);
    let p = get(&owner, app.url("/projects/CHG")).await;
    assert!(p["statuses"].as_array().unwrap().iter().find(|s| s["name"] == "In review").unwrap()["approval"].is_null());
}

#[tokio::test]
async fn e_mail_links_substitutes_parallel_steps_and_timeouts() {
    let app = spawn_with(|c| c.smtp_url = Some("smtp://127.0.0.1:9".into())).await;
    let owner = app.setup(false).await;
    post(&owner, app.url("/projects"), json!({"key": "BUY", "name": "Purchases", "template": "scrum"})).await;
    let p = get(&owner, app.url("/projects/BUY")).await;
    let (todo, review, done) = (status_id(&p, "To do"), status_id(&p, "In review"), status_id(&p, "Done"));
    let (rita, _) = app.user(&owner, "Rita Requester", "rita@example.com", "member").await;
    let (mia, mia_id) = app.user(&owner, "Mia Member", "mia@example.com", "member").await;
    let (tom, tom_id) = app.user(&owner, "Tom Teammate", "tom@example.com", "member").await;
    let (_ann, ann_id) = app.user(&owner, "Ann Approver", "ann@example.com", "member").await;
    let url = app.url(&format!("/statuses/{review}"));
    // finance and security at the same time; a late step goes to Ann
    patch(
        &owner,
        url.clone(),
        json!({"approval": {
            "steps": [{"name": "Finance", "approvers": [{"kind": "user", "id": mia_id}]}, {"name": "Security", "approvers": [{"kind": "assignee"}]}],
            "parallel": true, "approvedStatusId": done, "rejectedStatusId": todo, "pauseSla": true,
            "remindHours": 24, "timeout": {"hours": 72, "action": "reassign", "userId": ann_id},
        }}),
    )
    .await;
    // Mia is away, Tom covers for her
    let today = Utc::now().date_naive();
    let (s, _) = send(
        &mia,
        Method::POST,
        app.url("/absences"),
        json!({"start": (today - Duration::days(1)).to_string(), "end": (today + Duration::days(5)).to_string(), "substituteId": tom_id}),
    )
    .await;
    assert_eq!(s, 201);

    let owner_id = get(&owner, app.url("/me")).await["user"]["id"].clone();
    post(&rita, app.url("/issues"), json!({"projectKey": "BUY", "title": "Ten laptops", "assigneeId": owner_id, "statusId": review})).await;
    let a = request(&app, &tom, "BUY-1").await;
    assert_eq!((a["steps"][0]["state"].as_str(), a["steps"][1]["state"].as_str()), (Some("pending"), Some("pending")), "both at once");
    let finance: Vec<(&str, Option<&str>)> =
        a["steps"][0]["approvers"].as_array().unwrap().iter().map(|v| (v["user"]["name"].as_str().unwrap(), v["for"]["name"].as_str())).collect();
    assert_eq!(finance, [("Mia Member", None), ("Tom Teammate", Some("Mia Member"))]);
    assert_eq!(a["canDecide"].as_array().unwrap().len(), 1);

    // the e-mail's link opens a page that asks; nothing is decided by following it
    let mail = |to: &'static str| {
        let db = app.state.db.clone();
        async move {
            sqlx::query_as::<_, (String, String)>(
                "SELECT subject, body_text FROM mail_outbox WHERE to_address = $1 AND body_text LIKE '%/approve/%' ORDER BY created_at, id",
            )
            .bind(to)
            .fetch_all(&db)
            .await
            .unwrap()
        }
    };
    let (subject, body) = mail("tom@example.com").await.pop().expect("Tom is asked by e-mail");
    assert!(subject.contains("BUY-1") && body.contains("in place of Mia Member"), "{subject}\n{body}");
    let at = body.find("/approve/").unwrap() + 9;
    let token: String = body[at..].chars().take_while(|c| c.is_ascii_hexdigit() || *c == '.').collect();
    let anon = client();
    let info = get(&anon, app.url(&format!("/approval-links/{token}"))).await;
    assert_eq!((info["key"].as_str(), info["step"].as_str(), info["open"].as_bool()), (Some("BUY-1"), Some("Finance"), Some(true)));
    assert_eq!(request(&app, &tom, "BUY-1").await["steps"][0]["state"], "pending");
    let forged = format!("{}.{}", token.split_once('.').unwrap().0, "0".repeat(64));
    assert_eq!(send(&anon, Method::GET, app.url(&format!("/approval-links/{forged}")), Value::Null).await.0, 404);
    let (s, v) = send(&anon, Method::POST, app.url(&format!("/approval-links/{token}")), json!({"decision": "approve", "comment": "Within budget"})).await;
    assert_eq!((s, v["open"].as_bool(), v["decision"].as_str()), (200, Some(false), Some("approved")), "{v}");
    assert_eq!(send(&anon, Method::POST, app.url(&format!("/approval-links/{token}")), json!({"decision": "reject"})).await.0, 403, "once");
    let a = request(&app, &owner, "BUY-1").await;
    assert_eq!((a["state"].as_str(), a["steps"][0]["state"].as_str(), a["steps"][1]["state"].as_str()), (Some("pending"), Some("approved"), Some("pending")));

    // nobody answers for security: a reminder after a day, Ann after three
    assert_eq!(mail("owner@example.com").await.len(), 1);
    sqlx::query("UPDATE approval_steps SET started_at = now() - interval '25 hours' WHERE state = 'pending'").execute(&app.state.db).await.unwrap();
    taskcraft::approvals::tick(&app.state).await.unwrap();
    taskcraft::approvals::tick(&app.state).await.unwrap();
    let mails = mail("owner@example.com").await;
    assert_eq!(mails.len(), 2, "one reminder, not one a minute");
    assert!(mails[1].0.contains("Reminder"), "{}", mails[1].0);
    assert!(mail("ann@example.com").await.is_empty());
    sqlx::query("UPDATE approval_steps SET started_at = now() - interval '73 hours' WHERE state = 'pending'").execute(&app.state.db).await.unwrap();
    taskcraft::approvals::tick(&app.state).await.unwrap();
    assert_eq!(mail("ann@example.com").await.len(), 1);
    let a = request(&app, &owner, "BUY-1").await;
    assert_eq!(a["steps"][1]["approvers"].as_array().unwrap().len(), 2);
    let history = get(&owner, app.url("/issues/BUY-1/history")).await.to_string();
    assert!(history.contains("timeout"), "{history}");
    assert_eq!(decide(&app, &owner, &a, "approve", None).await.0, 204);
    assert_eq!(status_of(&app, &owner, "BUY-1").await, "Done");

    // a timeout that rejects
    patch(
        &owner,
        url,
        json!({"approval": {"steps": [{"approvers": [{"kind": "role", "role": "admin"}]}], "approvedStatusId": done, "rejectedStatusId": todo, "timeout": {"hours": 1, "action": "reject"}}}),
    )
    .await;
    post(&rita, app.url("/issues"), json!({"projectKey": "BUY", "title": "A coffee machine", "statusId": review})).await;
    assert_eq!(request(&app, &owner, "BUY-2").await["steps"][0]["approvers"][0]["user"]["name"], "Olivia Owner");
    sqlx::query("UPDATE approval_steps SET started_at = now() - interval '2 hours' WHERE state = 'pending'").execute(&app.state.db).await.unwrap();
    taskcraft::approvals::tick(&app.state).await.unwrap();
    assert_eq!(status_of(&app, &owner, "BUY-2").await, "To do");
    assert_eq!(request(&app, &owner, "BUY-2").await["state"], "rejected");
}

#[tokio::test]
async fn requesters_approve_on_the_portal() {
    const TOKEN: &str = "mail-in-secret";
    let app = spawn_with(|c| {
        c.smtp_url = Some("smtp://127.0.0.1:9".into());
        c.mail_from = "TaskCraft <tasks@acme.test>".into();
        c.mail_in.mail_in_token = Some(TOKEN.into());
    })
    .await;
    let owner = app.setup(true).await;
    let (s, v) =
        send(&owner, Method::PUT, app.url("/projects/SUP/mail-in"), json!({"enabled": true, "address": "support@acme.test", "senders": "anyone"})).await;
    assert_eq!(s, 200, "{v}");
    let p = get(&owner, app.url("/projects/SUP")).await;
    let (waiting, doing, closed) = (status_id(&p, "Waiting"), status_id(&p, "In progress"), status_id(&p, "Closed"));
    // "Waiting" asks the requester whether the proposed fix is fine
    patch(
        &owner,
        app.url(&format!("/statuses/{waiting}")),
        json!({"approval": {"steps": [{"name": "Your OK", "approvers": [{"kind": "requester"}]}], "approvedStatusId": closed, "rejectedStatusId": doing}}),
    )
    .await;
    let write_in = |from: &'static str, id: &'static str| {
        let url = app.url("/integrations/mail-in");
        async move {
            let raw = format!(
                "From: {from}\r\nTo: support@acme.test\r\nSubject: New badge\r\nMessage-ID: <{id}>\r\nMIME-Version: 1.0\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nI need a new badge.\r\n"
            );
            let r: Value =
                reqwest::Client::new().post(url).header("Authorization", format!("Bearer {TOKEN}")).body(raw).send().await.unwrap().json().await.unwrap();
            r["issue"].as_str().unwrap().to_string()
        }
    };
    let key = write_in("Jane Doe <jane@example.org>", "jane-1@example.org").await;
    write_in("Bob <bob@example.org>", "bob-1@example.org").await;
    patch(&owner, app.url(&format!("/issues/{key}")), json!({"statusId": waiting})).await;
    let a = request(&app, &owner, &key).await;
    assert_eq!(a["steps"][0]["approvers"][0]["email"], "jane@example.org");
    let mails: Vec<String> =
        sqlx::query_scalar("SELECT body_text FROM mail_outbox WHERE to_address = 'jane@example.org'").fetch_all(&app.state.db).await.unwrap();
    assert!(mails.iter().any(|b| b.contains("waits for your decision") && b.contains(&format!("/portal/requests/{key}"))), "{mails:?}");

    // Jane signs in to the portal and says no; someone else's session can't
    let sign_in = |email: &'static str, ip: &'static str| {
        let app = &app;
        async move {
            let mut h = reqwest::header::HeaderMap::new();
            h.insert("x-taskcraft", reqwest::header::HeaderValue::from_static("1"));
            h.insert("x-forwarded-for", ip.parse().unwrap());
            let c = reqwest::Client::builder().cookie_store(true).default_headers(h).build().unwrap();
            assert_eq!(c.post(app.url("/portal/sign-in")).json(&json!({"email": email})).send().await.unwrap().status(), 204);
            for _ in 0..50 {
                let body: Option<String> = sqlx::query_scalar(
                    "SELECT body_text FROM mail_outbox WHERE purpose = 'portal-link' AND to_address = $1 ORDER BY created_at DESC, id DESC LIMIT 1",
                )
                .bind(email)
                .fetch_optional(&app.state.db)
                .await
                .unwrap();
                if let Some(b) = body {
                    let at = b.find("token=").unwrap() + 6;
                    let token: String = b[at..].chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').collect();
                    let (s, v) = send(&c, Method::POST, app.url("/portal/session"), json!({"token": token})).await;
                    assert_eq!(s, 200, "{v}");
                    return c;
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
            panic!("no sign-in link for {email}");
        }
    };
    let jane = sign_in("jane@example.org", "198.51.100.1").await;
    let seen = get(&jane, app.url(&format!("/portal/requests/{key}"))).await;
    assert_eq!(seen["approval"]["step"], "Your OK");
    let bob = sign_in("bob@example.org", "198.51.100.2").await;
    let url = app.url(&format!("/portal/requests/{key}/approval"));
    assert_eq!(send(&bob, Method::POST, url.clone(), json!({"decision": "approve"})).await.0, 404);
    assert_eq!(send(&jane, Method::POST, url.clone(), json!({"decision": "reject", "comment": "It still doesn't open the garage"})).await.0, 204);
    assert_eq!(status_of(&app, &owner, &key).await, "In progress");
    assert!(get(&jane, app.url(&format!("/portal/requests/{key}"))).await["approval"].is_null());
    let history = get(&owner, app.url(&format!("/issues/{key}/history"))).await.to_string();
    assert!(history.contains("jane@example.org") && history.contains("garage"), "{history}");
    assert_eq!(send(&jane, Method::POST, url, json!({"decision": "approve"})).await.0, 404, "nothing is open any more");
}
