//! Dashboards, grouped counts and the portfolio.

mod common;

use common::*;
use reqwest::Method;
use serde_json::{Value, json};

#[tokio::test]
async fn dashboards_groups_and_portfolio() {
    let app = spawn().await;
    let c = app.setup(true).await;
    // a new dashboard starts with a few widgets
    let d = post(&c, app.url("/dashboards"), json!({"name": "Team"})).await;
    let id = d["id"].as_str().unwrap().to_string();
    let got = get(&c, app.url(&format!("/dashboards/{id}"))).await;
    assert!(got["widgets"].as_array().unwrap().len() >= 3);
    assert_eq!(got["mine"], true);
    let (s, _) = send(&c, Method::PATCH, app.url(&format!("/dashboards/{id}")), json!({"widgets": [{"type": "bogus"}]})).await;
    assert_eq!(s, 400);
    let (s, _) =
        send(&c, Method::PATCH, app.url(&format!("/dashboards/{id}")), json!({"widgets": [{"type": "issues", "config": {"query": "status >"}}]})).await;
    assert_eq!(s, 400, "queries are checked");
    patch(
        &c,
        app.url(&format!("/dashboards/{id}")),
        json!({"widgets": [{"type": "breakdown", "title": "Bugs", "config": {"query": "type = Bug", "by": "project"}}]}),
    )
    .await;
    assert_eq!(get(&c, app.url(&format!("/dashboards/{id}"))).await["widgets"][0]["title"], "Bugs");
    // private until shared
    let (other, _) = app.user(&c, "Max Member", "max@example.com", "member").await;
    let (s, _) = send(&other, Method::GET, app.url(&format!("/dashboards/{id}")), json!(null)).await;
    assert_eq!(s, 404);
    patch(&c, app.url(&format!("/dashboards/{id}")), json!({"shared": true})).await;
    assert_eq!(get(&other, app.url("/dashboards")).await.as_array().unwrap().len(), 1);
    let (s, _) = send(&other, Method::PATCH, app.url(&format!("/dashboards/{id}")), json!({"name": "Mine now"})).await;
    assert_eq!(s, 403);

    // grouped counts
    let by = |by: &str, q: &str| {
        let c = c.clone();
        let url = app.url("/search/groups");
        let (by, q) = (by.to_string(), q.to_string());
        async move { c.get(&url).query(&[("by", by.as_str()), ("q", q.as_str())]).send().await.unwrap().json::<Value>().await.unwrap() }
    };
    let status = by("status", "project = WEB").await;
    let total: i64 = status["groups"].as_array().unwrap().iter().map(|g| g["count"].as_i64().unwrap()).sum();
    assert_eq!(total, status["total"].as_i64().unwrap());
    assert!(status["groups"].as_array().unwrap().iter().any(|g| g["label"] == "Done" && g["extra"] == "done"));
    let prio = by("priority", "").await;
    assert!(prio["groups"].as_array().unwrap().iter().any(|g| g["label"] == "urgent"));
    let projects = by("project", "category != done").await;
    assert!(projects["groups"].as_array().unwrap().len() >= 3);
    let labels = by("labels", "project = OPS").await;
    assert!(labels["groups"].as_array().unwrap().iter().any(|g| g["label"] == "security"));
    let r = c.get(app.url("/search/groups")).query(&[("by", "mood")]).send().await.unwrap();
    assert_eq!(r.status(), 400);

    // portfolio: starred projects by default, epics with progress, releases, teams
    let p = get(&c, app.url("/portfolio")).await;
    let web = p["projects"].as_array().unwrap().iter().find(|x| x["key"] == "WEB").unwrap().clone();
    assert_eq!(web["epics"].as_array().unwrap().len(), 3);
    let checkout = web["epics"].as_array().unwrap().iter().find(|e| e["title"] == "Checkout redesign").unwrap();
    assert!(checkout["childDone"].as_i64().unwrap() > 0);
    assert!(web["versions"].as_array().unwrap().iter().any(|v| v["name"] == "2.5"));
    let team = &p["teams"][0];
    assert_eq!(team["name"], "Platform team");
    assert!(team["members"].as_array().unwrap().iter().any(|m| m["inProgress"].as_i64().unwrap() > 0));
    let only = get(&c, app.url("/portfolio?projects=mkt")).await;
    assert_eq!(only["projects"].as_array().unwrap().len(), 1);
}
