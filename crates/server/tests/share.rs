//! Share links: read-only pages for people without an account.

mod common;

use common::*;
use reqwest::Method;
use serde_json::{Value, json};

#[tokio::test]
async fn share_links() {
    let app = spawn().await;
    let c = app.setup(false).await;
    let (member, _) = app.user(&c, "Max Member", "max@example.com", "member").await;
    post(&c, app.url("/projects"), json!({"key": "WEB", "name": "Website", "template": "scrum"})).await;
    let p = get(&c, app.url("/projects/WEB")).await;
    let epic = p["types"].as_array().unwrap().iter().find(|t| t["level"] == 1).unwrap()["id"].clone();
    let level = post(&c, app.url("/projects/WEB/security-levels"), json!({"name": "Staff", "minRole": "member"})).await;
    post(
        &c,
        app.url("/issues"),
        json!({"projectKey": "WEB", "title": "Checkout is down", "labels": ["incident"], "assigneeId": get(&c, app.url("/me")).await["user"]["id"]}),
    )
    .await;
    post(&c, app.url("/issues"), json!({"projectKey": "WEB", "title": "Secret incident", "labels": ["incident"], "securityLevelId": level["id"]})).await;
    post(&c, app.url("/issues"), json!({"projectKey": "WEB", "title": "Unrelated"})).await;
    post(&c, app.url("/issues"), json!({"projectKey": "WEB", "title": "New checkout", "typeId": epic, "startDate": "2026-11-01", "dueDate": "2026-12-15"}))
        .await;
    post(&c, app.url("/issues/WEB-1/comments"), json!({"markdown": "We're on it"})).await;
    post(&c, app.url("/issues/WEB-1/comments"), json!({"markdown": "root cause: expired cert", "visibility": "members"})).await;

    // project admins make links; members don't
    assert_eq!(send(&member, Method::POST, app.url("/projects/WEB/shares"), json!({"title": "x"})).await.0, 403);
    assert_eq!(send(&c, Method::POST, app.url("/projects/WEB/shares"), json!({"title": "Bad", "query": "status = ("})).await.0, 400);
    let status = post(&c, app.url("/projects/WEB/shares"), json!({"title": "Incidents", "query": "labels = incident", "comments": true})).await;
    let url = status["url"].as_str().unwrap().to_string();
    let token = url.rsplit('/').next().unwrap().to_string();
    let anon = reqwest::Client::new();
    let get_anon = |path: String| anon.get(app.url(&path)).send();

    // the page: its issues, never private ones, without names unless asked
    let info: Value = get_anon(format!("/share/{token}")).await.unwrap().json().await.unwrap();
    assert_eq!((info["title"].as_str(), info["locked"].as_bool()), (Some("Incidents"), Some(false)));
    let list: Value = get_anon(format!("/share/{token}/issues")).await.unwrap().json().await.unwrap();
    let keys: Vec<&str> = list["issues"].as_array().unwrap().iter().map(|i| i["key"].as_str().unwrap()).collect();
    assert_eq!(keys, ["WEB-1"], "private and unrelated issues stay out");
    assert_eq!(list["issues"][0]["assignee"], Value::Null, "no names");
    let one: Value = get_anon(format!("/share/{token}/issues/WEB-1")).await.unwrap().json().await.unwrap();
    let comments = one["comments"].as_array().unwrap();
    assert_eq!(comments.len(), 1, "restricted comments stay out");
    assert_eq!(comments[0]["author"], Value::Null);
    for key in ["WEB-2", "WEB-3"] {
        assert_eq!(get_anon(format!("/share/{token}/issues/{key}")).await.unwrap().status(), 404, "{key}");
    }
    assert_eq!(get_anon(format!("/share/{token}/roadmap")).await.unwrap().status(), 404, "a list isn't a roadmap");
    assert_eq!(get_anon("/share/nonsense".into()).await.unwrap().status(), 404);
    let listed = get(&c, app.url("/projects/WEB/shares")).await;
    assert_eq!(listed[0]["views"], 1);

    // a roadmap with a password
    let road = post(&c, app.url("/projects/WEB/shares"), json!({"kind": "roadmap", "title": "Roadmap", "password": "open sesame", "people": true})).await;
    let rt = road["url"].as_str().unwrap().rsplit('/').next().unwrap().to_string();
    let info: Value = get_anon(format!("/share/{rt}")).await.unwrap().json().await.unwrap();
    assert_eq!(info["locked"], true);
    assert_eq!(get_anon(format!("/share/{rt}/roadmap")).await.unwrap().status(), 401);
    let wrong = anon.post(app.url(&format!("/share/{rt}/unlock"))).json(&json!({"password": "nope"})).send().await.unwrap();
    assert_eq!(wrong.status(), 400);
    let key: Value = anon.post(app.url(&format!("/share/{rt}/unlock"))).json(&json!({"password": "open sesame"})).send().await.unwrap().json().await.unwrap();
    let key = key["key"].as_str().unwrap().to_string();
    let r: Value = anon.get(app.url(&format!("/share/{rt}/roadmap"))).header("x-share-key", &key).send().await.unwrap().json().await.unwrap();
    assert_eq!(r["epics"].as_array().unwrap().len(), 1);
    assert_eq!(r["epics"][0]["title"], "New checkout");
    // a new password locks out who had the old one
    send(&c, Method::PATCH, app.url(&format!("/shares/{}", road["id"].as_str().unwrap())), json!({"password": "another secret"})).await;
    assert_eq!(anon.get(app.url(&format!("/share/{rt}/roadmap"))).header("x-share-key", &key).send().await.unwrap().status(), 401);

    // over: an end in the past, revoked, or sharing turned off
    let id = status["id"].as_str().unwrap();
    sqlx::query("UPDATE share_links SET expires_at = now() - interval '1 minute' WHERE id = $1::uuid").bind(id).execute(&app.state.db).await.unwrap();
    assert_eq!(get_anon(format!("/share/{token}")).await.unwrap().status(), 404);
    send(&c, Method::PATCH, app.url(&format!("/shares/{id}")), json!({"expiresAt": null})).await;
    assert_eq!(get_anon(format!("/share/{token}")).await.unwrap().status(), 200);
    send(&c, Method::PUT, app.url("/admin/workspace"), json!({"features": {"sharing": false}})).await;
    assert_eq!(get_anon(format!("/share/{token}")).await.unwrap().status(), 404);
    assert_eq!(send(&c, Method::POST, app.url("/projects/WEB/shares"), json!({"title": "x"})).await.0, 400);
    send(&c, Method::PUT, app.url("/admin/workspace"), json!({"features": {}})).await;
    assert_eq!(send(&c, Method::DELETE, app.url(&format!("/shares/{id}")), Value::Null).await.0, 204);
    assert_eq!(get_anon(format!("/share/{token}")).await.unwrap().status(), 404);
}
