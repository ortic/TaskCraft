//! Workflow rules (who may move into a status, the fields and comment it needs) and resolutions.

mod common;

use common::*;
use reqwest::Method;
use serde_json::{Value, json};

async fn status_id(c: &reqwest::Client, app: &TestApp, key: &str, name: &str) -> String {
    let p = get(c, app.url(&format!("/projects/{key}"))).await;
    p["statuses"].as_array().unwrap().iter().find(|s| s["name"] == name).unwrap_or_else(|| panic!("no status {name}"))["id"].as_str().unwrap().to_string()
}

async fn new_issue(c: &reqwest::Client, app: &TestApp, title: &str) -> String {
    post(c, app.url("/issues"), json!({"projectKey": "FLOW", "title": title})).await["key"].as_str().unwrap().to_string()
}

async fn tql(c: &reqwest::Client, app: &TestApp, q: &str) -> i64 {
    let r: Value = c.get(app.url("/search")).query(&[("q", q)]).send().await.unwrap().json().await.unwrap();
    r["total"].as_i64().unwrap_or_else(|| panic!("{q}: {r}"))
}

#[tokio::test]
async fn statuses_require_fields_comments_and_people_and_resolutions_follow_done() {
    let app = spawn().await;
    let c = app.setup(false).await;
    post(&c, app.url("/projects"), json!({"key": "FLOW", "name": "Flow", "template": "kanban"})).await;
    let (mia, mia_id) = app.user(&c, "Mia Member", "mia@example.com", "member").await;
    let review = status_id(&c, &app, "FLOW", "Review").await;
    let done = status_id(&c, &app, "FLOW", "Done").await;
    let progress = status_id(&c, &app, "FLOW", "In progress").await;

    // rules are checked when they're set
    for bad in [json!({"who": "teams"}), json!({"require": ["shoe size"]}), json!({"comment": "maybe"}), json!({"nope": 1})] {
        let (s, _) = send(&c, Method::PATCH, app.url(&format!("/statuses/{review}")), json!({"rules": bad})).await;
        assert_eq!(s, 400, "{bad}");
    }
    patch(&c, app.url(&format!("/statuses/{review}")), json!({"rules": {"require": ["assignee"], "comment": "required"}})).await;
    let p = get(&c, app.url("/projects/FLOW")).await;
    let rules = &p["statuses"].as_array().unwrap().iter().find(|s| s["name"] == "Review").unwrap()["rules"];
    assert_eq!(rules, &json!({"require": ["assignee"], "comment": "required"}));

    // a member moving into Review needs an assignee and a comment
    let key = new_issue(&mia, &app, "Write the release notes").await;
    let (s, v) = send(&mia, Method::PATCH, app.url(&format!("/issues/{key}")), json!({"statusId": review})).await;
    assert_eq!(s, 400);
    assert!(v["message"].as_str().unwrap().contains("needs an assignee, a comment"), "{v}");
    assert_eq!(get(&mia, app.url(&format!("/issues/{key}"))).await["status"]["name"], "Backlog", "nothing changed");
    patch(&mia, app.url(&format!("/issues/{key}")), json!({"statusId": review, "assigneeId": mia_id, "comment": "Ready for a **look**"})).await;
    let issue = get(&mia, app.url(&format!("/issues/{key}"))).await;
    assert_eq!(issue["status"]["name"], "Review");
    let comments = get(&mia, app.url(&format!("/issues/{key}/comments"))).await;
    assert!(comments.to_string().contains("Ready for a"), "the comment came along: {comments}");
    // the board's move takes the same: fields and a comment
    let key2 = new_issue(&mia, &app, "Second").await;
    let (s, _) = send(&mia, Method::POST, app.url(&format!("/issues/{key2}/move")), json!({"statusId": review})).await;
    assert_eq!(s, 400);
    post(&mia, app.url(&format!("/issues/{key2}/move")), json!({"statusId": review, "fields": {"assigneeId": mia_id}, "comment": "on it"})).await;
    assert_eq!(get(&mia, app.url(&format!("/issues/{key2}"))).await["status"]["name"], "Review");

    // only admins may close; then only a team
    patch(&c, app.url(&format!("/statuses/{done}")), json!({"rules": {"who": "admins"}})).await;
    let (s, v) = send(&mia, Method::PATCH, app.url(&format!("/issues/{key}")), json!({"statusId": done})).await;
    assert_eq!(s, 403, "{v}");
    let team = post(&c, app.url("/teams"), json!({"name": "QA", "memberIds": [mia_id]})).await;
    patch(&c, app.url(&format!("/statuses/{done}")), json!({"rules": {"who": "teams", "teams": [team["id"]]}})).await;
    patch(&mia, app.url(&format!("/issues/{key}")), json!({"statusId": done})).await;

    // the resolution: the default on entering done, a chosen one, cleared on reopening
    let closed = get(&mia, app.url(&format!("/issues/{key}"))).await;
    assert_eq!(closed["resolution"], "Done");
    patch(&mia, app.url(&format!("/issues/{key}")), json!({"resolution": "won't DO"})).await;
    assert_eq!(get(&mia, app.url(&format!("/issues/{key}"))).await["resolution"], "Won't do", "matched by name, kept as the project spells it");
    let (s, _) = send(&mia, Method::PATCH, app.url(&format!("/issues/{key}")), json!({"resolution": "Bribed"})).await;
    assert_eq!(s, 400, "only the project's resolutions");
    assert_eq!(tql(&c, &app, "project = FLOW and resolution = \"Won't do\"").await, 1);
    assert_eq!(tql(&c, &app, "project = FLOW and resolution is empty").await, 1);
    let history = get(&c, app.url(&format!("/issues/{key}/history"))).await;
    assert!(history.to_string().contains("resolution"), "{history}");
    let csv = c.get(app.url("/search.csv")).query(&[("q", "project = FLOW")]).send().await.unwrap().text().await.unwrap();
    assert!(csv.lines().next().unwrap().split(',').any(|h| h == "Resolution") && csv.contains("Won't do"), "{csv}");
    patch(&c, app.url(&format!("/issues/{key}")), json!({"statusId": progress})).await;
    assert_eq!(get(&c, app.url(&format!("/issues/{key}"))).await["resolution"], Value::Null, "reopened: no resolution");
    let (s, _) = send(&c, Method::PATCH, app.url(&format!("/issues/{key}")), json!({"resolution": "Done"})).await;
    assert_eq!(s, 400, "open issues have no resolution");

    // a status can insist on a resolution; the project has its own list
    let (s, _) = send(&c, Method::PATCH, app.url("/projects/FLOW"), json!({"settings": {"resolutions": ["Fixed", "fixed"]}})).await;
    assert_eq!(s, 400, "no duplicates");
    patch(&c, app.url("/projects/FLOW"), json!({"settings": {"resolutions": ["Fixed", "Rejected"]}})).await;
    patch(&c, app.url(&format!("/statuses/{done}")), json!({"rules": {"require": ["resolution"]}})).await;
    let (s, v) = send(&mia, Method::PATCH, app.url(&format!("/issues/{key2}")), json!({"statusId": done})).await;
    assert_eq!(s, 400);
    assert!(v["message"].as_str().unwrap().contains("a resolution"), "{v}");
    patch(&mia, app.url(&format!("/issues/{key2}")), json!({"statusId": done, "resolution": "Rejected"})).await;
    assert_eq!(get(&mia, app.url(&format!("/issues/{key2}"))).await["resolution"], "Rejected");
    // project admins pass every rule but the fields
    patch(&c, app.url(&format!("/statuses/{done}")), json!({"rules": {"who": "admins"}})).await;
    let key3 = new_issue(&c, &app, "Third").await;
    patch(&c, app.url(&format!("/issues/{key3}")), json!({"statusId": done})).await;
    assert_eq!(get(&c, app.url(&format!("/issues/{key3}"))).await["resolution"], "Fixed", "the first of the project's list");
}
