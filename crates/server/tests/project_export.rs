//! A project exported from one TaskCraft and imported into another: workflow, fields, sprints,
//! releases, issues with their history, comments, time, files, links, private issues — and the
//! texts' mentions, issue references and images pointing to what was imported.

mod common;

use common::*;
use reqwest::Method;
use reqwest::multipart::{Form, Part};
use serde_json::{Value, json};

async fn settle(c: &reqwest::Client, app: &TestApp, id: &str) -> Value {
    for _ in 0..300 {
        let j = get(c, app.url(&format!("/admin/backup-imports/{id}"))).await;
        if !matches!(j["status"].as_str(), Some("checking" | "running")) {
            return j;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("the import doesn't finish");
}

fn find<'a>(list: &'a Value, key: &str, name: &str) -> &'a Value {
    list.as_array().unwrap().iter().find(|x| x[key] == name).unwrap_or_else(|| panic!("no {name} in {list}"))
}

#[tokio::test]
async fn a_project_moves_to_another_taskcraft() {
    // ---- the source ----
    let a = spawn().await;
    let c = a.setup(false).await;
    let (_max, max_id) = a.user(&c, "Max Member", "max@example.com", "member").await;
    post(&c, a.url("/projects"), json!({"key": "WEB", "name": "Website", "template": "scrum"})).await;
    post(&c, a.url("/projects/WEB/fields"), json!({"name": "Customer tier", "kind": "select", "options": [{"label": "Gold"}, {"label": "Silver"}]})).await;
    let tier = find(&get(&c, a.url("/projects/WEB")).await["fields"], "name", "Customer tier").clone();
    let gold = find(&tier["options"], "label", "Gold")["id"].clone();
    let level = post(&c, a.url("/projects/WEB/security-levels"), json!({"name": "Staff", "minRole": "member"})).await;
    let release = post(&c, a.url("/projects/WEB/versions"), json!({"name": "2.0"})).await;
    let part = post(&c, a.url("/projects/WEB/components"), json!({"name": "Checkout"})).await;
    let sprint = post(&c, a.url("/projects/WEB/sprints"), json!({"name": "Sprint 7"})).await;
    let p = get(&c, a.url("/projects/WEB")).await;
    let done = find(&p["statuses"], "category", "done")["id"].clone();
    let sub = p["types"].as_array().unwrap().iter().find(|t| t["level"] == -1).unwrap()["id"].clone();

    post(&c, a.url("/issues"), json!({"projectKey": "WEB", "title": "Login fails", "priority": "none"})).await;
    let mut custom = serde_json::Map::new();
    custom.insert(tier["id"].as_str().unwrap().into(), gold);
    post(
        &c,
        a.url("/issues"),
        json!({"projectKey": "WEB", "title": "Pay by card", "priority": "high", "assigneeId": max_id, "labels": ["payments"], "custom": custom,
               "versionIds": [release["id"]], "componentIds": [part["id"]], "sprintId": sprint["id"], "storyPoints": 5}),
    )
    .await;
    post(&c, a.url("/issues"), json!({"projectKey": "WEB", "title": "Hidden plans", "securityLevelId": level["id"]})).await;
    post(&c, a.url("/issues"), json!({"projectKey": "WEB", "title": "Find the cause", "typeId": sub, "parentKey": "WEB-2"})).await;
    patch(&c, a.url("/issues/WEB-1"), json!({"statusId": done})).await;
    post(&c, a.url("/issues/WEB-3/links"), json!({"kind": "blocks", "target": "WEB-1"})).await;
    post(&c, a.url("/issues/WEB-2/comments"), json!({"markdown": "Card payments go live next week"})).await;
    post(&c, a.url("/issues/WEB-2/comments"), json!({"markdown": "internal note", "visibility": "members"})).await;
    post(&c, a.url("/issues/WEB-2/worklogs"), json!({"time": "2h", "note": "first try"})).await;
    let form = Form::new().part("file", Part::bytes(b"card terminal manual".to_vec()).file_name("manual.txt"));
    let files: Value = c.post(a.url("/issues/WEB-2/attachments")).multipart(form).send().await.unwrap().json().await.unwrap();
    let file_id = files[0]["id"].as_str().unwrap();
    // a description with a mention, a reference to another issue and the attached image
    let description = json!({"type": "doc", "content": [
        {"type": "paragraph", "content": [
            {"type": "text", "text": "Like "},
            {"type": "mention", "attrs": {"id": "WEB-1", "label": "WEB-1", "mentionSuggestionChar": "#"}},
            {"type": "text", "text": ", ask "},
            {"type": "mention", "attrs": {"id": max_id, "label": "Max Member", "mentionSuggestionChar": "@"}}
        ]},
        {"type": "image", "attrs": {"src": format!("/api/attachments/{file_id}"), "alt": "manual"}}
    ]});
    patch(&c, a.url("/issues/WEB-2"), json!({"description": description})).await;

    // only workspace admins export (private issues, restricted comments, e-mail addresses)
    let (member, _) = a.user(&c, "Mia", "mia@example.com", "member").await;
    assert_eq!(member.get(a.url("/projects/WEB/export")).send().await.unwrap().status(), 403);
    let r = c.get(a.url("/projects/WEB/export")).send().await.unwrap();
    assert_eq!(r.status(), 200);
    assert!(r.headers()["content-disposition"].to_str().unwrap().contains("WEB-"), "{:?}", r.headers());
    let zip = r.bytes().await.unwrap().to_vec();
    assert_eq!(&zip[..4], b"PK\x03\x04");

    // ---- the target: another TaskCraft, where the owner has the same address ----
    let b = spawn().await;
    let o = b.setup(false).await;
    let upload = |body: Vec<u8>| {
        o.post(b.url("/admin/backup-imports")).query(&[("name", "WEB.taskcraft.zip")]).header("content-type", "application/zip").body(body).send()
    };
    let job: Value = upload(zip.clone()).await.unwrap().json().await.unwrap();
    let id = job["id"].as_str().unwrap().to_string();
    let job = settle(&o, &b, &id).await;
    assert_eq!(job["status"], "checked", "{job}");
    assert_eq!(job["source"], "taskcraft");
    let web = find(&job["summary"]["projects"], "key", "WEB");
    assert_eq!(web["issues"], 4, "{web}");
    assert_eq!(web["attachments"], 1);
    let (st, body) =
        send(&o, Method::POST, b.url(&format!("/admin/backup-imports/{id}/run")), json!({"projects": {"WEB": {"action": "new", "key": "SITE"}}})).await;
    assert_eq!(st, 202, "{body}");
    let job = settle(&o, &b, &id).await;
    assert_eq!(job["status"], "done", "{job}");

    let pay = get(&o, b.url("/issues/SITE-2")).await;
    assert_eq!(pay["title"], "Pay by card");
    assert_eq!(get(&o, b.url("/issues/WEB-2")).await["key"], "SITE-2", "the old key works");
    assert_eq!(pay["priority"], "high");
    assert_eq!(get(&o, b.url("/issues/SITE-1")).await["priority"], "none");
    assert_eq!(get(&o, b.url("/issues/SITE-1")).await["status"]["category"], "done");
    assert_eq!(pay["labels"], json!(["payments"]));
    assert_eq!(pay["storyPoints"], 5.0);
    assert_eq!(pay["sprint"]["name"], "Sprint 7");
    assert_eq!(pay["assignee"]["name"], "Max Member");
    let max_here: String = sqlx::query_scalar("SELECT id::text FROM users WHERE email = 'max@example.com'").fetch_one(&b.state.db).await.unwrap();
    assert_eq!(pay["assignee"]["id"], max_here.as_str(), "people by e-mail (invited here)");
    assert_eq!(pay["versions"][0]["name"], "2.0");
    assert_eq!(pay["components"][0]["name"], "Checkout");
    let target = get(&o, b.url("/projects/SITE")).await;
    let tier_here = find(&target["fields"], "name", "Customer tier");
    let gold_here = find(&tier_here["options"], "label", "Gold")["id"].clone();
    assert_eq!(pay["custom"][tier_here["id"].as_str().unwrap()], gold_here);
    // texts: the mention, the reference and the image point to what's here now
    let d = pay["description"].to_string();
    assert!(d.contains("\"SITE-1\""), "{d}");
    assert!(d.contains(&max_here), "{d}");
    let file_here = pay["attachments"][0]["id"].as_str().unwrap().to_string();
    assert!(d.contains(&format!("/api/attachments/{file_here}")), "{d}");
    let bytes = o.get(b.url(&format!("/attachments/{file_here}"))).send().await.unwrap().bytes().await.unwrap();
    assert_eq!(&bytes[..], b"card terminal manual");
    // comments (the restricted one stays restricted), time, links, sub-tasks, private issues
    let comments = get(&o, b.url("/issues/SITE-2/comments")).await;
    let comments = comments.as_array().unwrap();
    assert_eq!(comments.len(), 2);
    assert!(comments.iter().any(|c| c["visibility"] == "members"), "{comments:?}");
    let logs = get(&o, b.url("/issues/SITE-2/worklogs")).await;
    assert_eq!(logs.as_array().unwrap().len(), 1);
    assert_eq!(logs[0]["minutes"], 120);
    let blocked = get(&o, b.url("/issues/SITE-1")).await;
    assert!(blocked["links"].to_string().contains("SITE-3"), "{}", blocked["links"]);
    assert_eq!(get(&o, b.url("/issues/SITE-4")).await["parent"]["key"], "SITE-2");
    assert_eq!(get(&o, b.url("/issues/SITE-3")).await["security"]["name"], "Staff");
    // the history the reports replay came along
    let status_changes: i64 = sqlx::query_scalar("SELECT count(*) FROM issue_events WHERE field = 'status'").fetch_one(&b.state.db).await.unwrap();
    assert!(status_changes >= 1);

    // the same export again: what's there already is skipped
    let zip_again = zip.clone();
    let job: Value = upload(zip).await.unwrap().json().await.unwrap();
    let id = job["id"].as_str().unwrap().to_string();
    let job = settle(&o, &b, &id).await;
    assert_eq!(find(&job["summary"]["projects"], "key", "WEB")["imported"], 4, "{job}");

    // back into the workspace it came from: the check says it would be a copy
    let r = c.post(a.url("/admin/backup-imports")).header("content-type", "application/zip").body(zip_again).send().await.unwrap();
    let job: Value = r.json().await.unwrap();
    let job = settle(&c, &a, job["id"].as_str().unwrap()).await;
    assert!(job["summary"]["warnings"].to_string().contains("export-from-here"), "{job}");
}
