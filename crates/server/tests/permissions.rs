//! Project permissions: who may delete and move issues, plan sprints, manage releases, moderate
//! comments and log time for others.

mod common;

use common::*;
use reqwest::Method;
use serde_json::json;

#[tokio::test]
async fn projects_choose_who_may_do_what() {
    let app = spawn().await;
    let admin = app.setup(false).await;
    post(&admin, app.url("/projects"), json!({"key": "PERM", "name": "Permissions", "template": "scrum"})).await;
    let (mia, mia_id) = app.user(&admin, "Mia Member", "mia@example.com", "member").await;
    let issue = |c: &reqwest::Client, title: &str| {
        let (c, url, title) = (c.clone(), app.url("/issues"), title.to_string());
        async move { post(&c, url, json!({"projectKey": "PERM", "title": title})).await["key"].as_str().unwrap().to_string() }
    };
    let set = |perms: serde_json::Value| {
        let (c, url) = (admin.clone(), app.url("/projects/PERM"));
        async move { patch(&c, url, json!({"settings": {"permissions": perms}})).await }
    };

    // the settings are checked
    for bad in [json!({"deleteIssues": "nobody"}), json!({"teleport": "member"}), json!({"manageSprints": "everyone"})] {
        let (s, _) = send(&admin, Method::PATCH, app.url("/projects/PERM"), json!({"settings": {"permissions": bad}})).await;
        assert_eq!(s, 400, "{bad}");
    }

    // deleting issues: members by default, then admins only
    let a = issue(&mia, "First").await;
    assert_eq!(send(&mia, Method::DELETE, app.url(&format!("/issues/{a}")), json!(null)).await.0, 204);
    set(json!({"deleteIssues": "admin"})).await;
    let b = issue(&mia, "Second").await;
    let (s, v) = send(&mia, Method::DELETE, app.url(&format!("/issues/{b}")), json!(null)).await;
    assert_eq!(s, 403, "{v}");
    assert!(v["message"].as_str().unwrap().contains("delete issues"));
    let bulk = post(&mia, app.url("/issues/bulk"), json!({"ids": [get(&mia, app.url(&format!("/issues/{b}"))).await["id"]], "action": "delete"})).await;
    assert_eq!(bulk["updated"], 0, "bulk deletes follow the same rule: {bulk}");
    assert_eq!(send(&admin, Method::DELETE, app.url(&format!("/issues/{b}")), json!(null)).await.0, 204);

    // sprints and releases
    set(json!({"manageSprints": "admin", "manageReleases": "admin"})).await;
    assert_eq!(send(&mia, Method::POST, app.url("/projects/PERM/sprints"), json!({"name": "Mia's sprint"})).await.0, 403);
    assert_eq!(send(&mia, Method::POST, app.url("/projects/PERM/versions"), json!({"name": "9.9"})).await.0, 403);
    let v = post(&admin, app.url("/projects/PERM/versions"), json!({"name": "1.0"})).await;
    assert_eq!(send(&mia, Method::DELETE, app.url(&format!("/versions/{}", v["id"].as_str().unwrap())), json!(null)).await.0, 403);
    set(json!({"manageSprints": "member", "manageReleases": "member"})).await;
    post(&mia, app.url("/projects/PERM/sprints"), json!({"name": "Mia's sprint"})).await;

    // other people's comments: admins by default (now also editing), or nobody
    let c = issue(&mia, "Discuss").await;
    let mine = post(&mia, app.url(&format!("/issues/{c}/comments")), json!({"markdown": "typo heer"})).await;
    let mine_id = mine["id"].as_str().unwrap();
    patch(&admin, app.url(&format!("/comments/{mine_id}")), json!({"markdown": "typo here"})).await;
    let theirs = post(&admin, app.url(&format!("/issues/{c}/comments")), json!({"markdown": "admin note"})).await;
    assert_eq!(send(&mia, Method::PATCH, app.url(&format!("/comments/{}", theirs["id"].as_str().unwrap())), json!({"markdown": "edited"})).await.0, 403);
    set(json!({"moderateComments": "nobody"})).await;
    assert_eq!(send(&admin, Method::DELETE, app.url(&format!("/comments/{mine_id}")), json!(null)).await.0, 403);
    assert_eq!(send(&mia, Method::DELETE, app.url(&format!("/comments/{mine_id}")), json!(null)).await.0, 204, "authors always may");

    // time for others: admins by default
    let w = post(&admin, app.url(&format!("/issues/{c}/worklogs")), json!({"time": "2h", "userId": mia_id})).await;
    let logs = get(&admin, app.url(&format!("/issues/{c}/worklogs"))).await;
    assert!(logs.as_array().unwrap().iter().any(|l| l["user"]["id"] == mia_id.as_str() && l["minutes"] == 120), "{logs}");
    let admin_id = get(&admin, app.url("/me")).await["user"]["id"].as_str().unwrap().to_string();
    assert_eq!(send(&mia, Method::POST, app.url(&format!("/issues/{c}/worklogs")), json!({"time": "1h", "userId": admin_id})).await.0, 403);
    assert_eq!(send(&mia, Method::PATCH, app.url(&format!("/worklogs/{}", w["id"].as_str().unwrap())), json!({"time": "3h"})).await.0, 200, "her own log");
    set(json!({"logTimeForOthers": "nobody"})).await;
    assert_eq!(send(&admin, Method::POST, app.url(&format!("/issues/{c}/worklogs")), json!({"time": "1h", "userId": mia_id})).await.0, 403);

    // moving issues to another project
    post(&admin, app.url("/projects"), json!({"key": "OTHER", "name": "Other", "template": "kanban"})).await;
    set(json!({"moveIssues": "admin"})).await;
    let (s, _) = send(&mia, Method::POST, app.url(&format!("/issues/{c}/move-project")), json!({"projectKey": "OTHER"})).await;
    assert_eq!(s, 403);
}
