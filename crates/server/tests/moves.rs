//! Moving issues between projects; editing time logs.

mod common;

use common::*;
use reqwest::Method;
use serde_json::{Value, json};

#[tokio::test]
async fn move_issue_with_subtasks_and_fields() {
    let app = spawn().await;
    let c = app.setup(false).await;
    post(&c, app.url("/projects"), json!({"key": "SUP", "name": "Support", "template": "tracker"})).await;
    post(&c, app.url("/projects"), json!({"key": "WEB", "name": "Web", "template": "scrum"})).await;
    let sup = get(&c, app.url("/projects/SUP")).await;
    let web = get(&c, app.url("/projects/WEB")).await;
    // the target gets a Severity field too (other option ids, same labels)
    post(&c, app.url("/projects/WEB/fields"), json!({"name": "severity", "kind": "select", "options": [{"label": "Minor"}, {"label": "Critical"}]})).await;
    let field = |p: &Value, name: &str| p["fields"].as_array().unwrap().iter().find(|f| f["name"] == name).unwrap().clone();
    let sev = field(&sup, "Severity");
    let critical = sev["options"].as_array().unwrap().iter().find(|o| o["label"] == "Critical").unwrap()["id"].clone();
    let env = field(&sup, "Environment");
    let mut custom = serde_json::Map::new();
    custom.insert(sev["id"].as_str().unwrap().into(), critical);
    custom.insert(env["id"].as_str().unwrap().into(), json!("prod"));
    let bug_type = sup["types"].as_array().unwrap().iter().find(|t| t["name"] == "Bug").unwrap()["id"].clone();
    post(&c, app.url("/issues"), json!({"projectKey": "SUP", "title": "Checkout crashes", "typeId": bug_type, "custom": custom})).await;
    let sub_type = sup["types"].as_array().unwrap().iter().find(|t| t["level"] == -1).unwrap()["id"].clone();
    post(&c, app.url("/issues"), json!({"projectKey": "SUP", "title": "Find the cause", "typeId": sub_type, "parentKey": "SUP-1"})).await;
    let in_progress = sup["statuses"].as_array().unwrap().iter().find(|s| s["name"] == "In progress").unwrap()["id"].clone();
    patch(&c, app.url("/issues/SUP-1"), json!({"statusId": in_progress})).await;
    post(&c, app.url("/issues/SUP-1/comments"), json!({"markdown": "seen twice"})).await;

    let preview = get(&c, app.url("/issues/SUP-1/move-project?to=WEB")).await;
    assert_eq!(preview["subtasks"], 1);
    assert_eq!(preview["droppedFields"], json!(["Environment"]));
    let moved = post(&c, app.url("/issues/SUP-1/move-project"), json!({"projectKey": "WEB"})).await;
    assert_eq!(moved["key"], "WEB-1");
    assert_eq!(moved["type"]["name"], "Bug", "the type keeps its name");
    assert_eq!(moved["status"]["name"], "In progress", "the status keeps its name");
    let web_sev = field(&get(&c, app.url("/projects/WEB")).await, "severity");
    let web_critical = web_sev["options"].as_array().unwrap().iter().find(|o| o["label"] == "Critical").unwrap()["id"].clone();
    assert_eq!(moved["custom"], json!({web_sev["id"].as_str().unwrap(): web_critical}));
    // the old key still finds it, the sub-task moved along, comments and history too
    assert_eq!(get(&c, app.url("/issues/SUP-1")).await["key"], "WEB-1");
    let sub = get(&c, app.url("/issues/SUP-2")).await;
    assert_eq!(sub["key"], "WEB-2");
    assert_eq!(sub["parent"]["key"], "WEB-1");
    assert_eq!(get(&c, app.url("/issues/WEB-1/comments")).await.as_array().unwrap().len(), 1);
    let h = get(&c, app.url("/issues/WEB-1/history")).await;
    assert!(h.as_array().unwrap().iter().any(|e| e["kind"] == "moved" && e["old"]["key"] == "SUP-1"));
    let r: Value = c.get(app.url("/search")).query(&[("q", "project = SUP")]).send().await.unwrap().json().await.unwrap();
    assert_eq!(r["total"], 0);
    // numbers continue in the target
    let next = post(&c, app.url("/issues"), json!({"projectKey": "WEB", "title": "Next"})).await;
    assert_eq!(next["key"], "WEB-3");
    // an explicit status, and moving into the same project
    let done = web["statuses"].as_array().unwrap().iter().find(|s| s["name"] == "Done").unwrap()["id"].clone();
    let back = post(&c, app.url("/issues/WEB-3/move-project"), json!({"projectKey": "SUP"})).await;
    assert_eq!(back["key"], "SUP-3");
    let (s, _) = send(&c, Method::POST, app.url("/issues/SUP-3/move-project"), json!({"projectKey": "SUP"})).await;
    assert_eq!(s, 400);
    let (s, _) = send(&c, Method::POST, app.url("/issues/SUP-3/move-project"), json!({"projectKey": "WEB", "statusId": in_progress})).await;
    assert_eq!(s, 400, "a status of another project");
    let again = post(&c, app.url("/issues/SUP-3/move-project"), json!({"projectKey": "WEB", "statusId": done})).await;
    assert_eq!(again["status"]["name"], "Done");
    assert!(again["resolvedAt"].is_string());
    // viewers of the target can't move issues there
    let (viewer, vid) = app.user(&c, "Vic Viewer", "vic@example.com", "member").await;
    let me = get(&c, app.url("/me")).await["user"]["id"].clone();
    let r = c
        .put(app.url("/projects/WEB/members"))
        .json(&json!([{"subjectType": "user", "subjectId": vid, "role": "viewer"}, {"subjectType": "user", "subjectId": me, "role": "admin"}]))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 204);
    patch(&c, app.url("/projects/WEB"), json!({"defaultRole": "viewer"})).await;
    post(&viewer, app.url("/issues"), json!({"projectKey": "SUP", "title": "Mine"})).await;
    let (s, _) = send(&viewer, Method::POST, app.url("/issues/SUP-4/move-project"), json!({"projectKey": "WEB"})).await;
    assert_eq!(s, 403);
}

#[tokio::test]
async fn edit_time_logs() {
    let app = spawn().await;
    let c = app.setup(false).await;
    post(&c, app.url("/projects"), json!({"key": "OPS", "name": "Ops", "template": "kanban"})).await;
    post(&c, app.url("/issues"), json!({"projectKey": "OPS", "title": "Backups"})).await;
    let w = post(&c, app.url("/issues/OPS-1/worklogs"), json!({"time": "1h"})).await;
    let id = w["id"].as_str().unwrap();
    let e = patch(&c, app.url(&format!("/worklogs/{id}")), json!({"time": "2h 15m", "date": "2026-01-05", "note": "restore test"})).await;
    assert_eq!(e["minutes"], 135);
    assert_eq!(e["date"], "2026-01-05");
    assert_eq!(get(&c, app.url("/issues/OPS-1")).await["spentMinutes"], 135);
    let (s, _) = send(&c, Method::PATCH, app.url(&format!("/worklogs/{id}")), json!({"time": "soon"})).await;
    assert_eq!(s, 400);
    let (other, _) = app.user(&c, "Max Member", "max@example.com", "member").await;
    let (s, _) = send(&other, Method::PATCH, app.url(&format!("/worklogs/{id}")), json!({"time": "1m"})).await;
    assert_eq!(s, 403, "only the person who logged it or an admin");
}
