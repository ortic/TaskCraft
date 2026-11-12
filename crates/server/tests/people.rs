//! Removing a person: history stays, personal data goes.

mod common;

use common::*;
use reqwest::Method;
use serde_json::json;

#[tokio::test]
async fn removing_a_person_keeps_history_and_drops_personal_data() {
    let app = spawn().await;
    let c = app.setup(true).await;
    let (max, max_id) = app.user(&c, "Max Member", "max@example.com", "member").await;
    post(&max, app.url("/issues/WEB-1/comments"), json!({"markdown": "I looked into it"})).await;
    post(&max, app.url("/issues/WEB-1/worklogs"), json!({"time": "1h"})).await;
    patch(&c, app.url("/issues/WEB-2"), json!({"assigneeId": max_id})).await;
    let private = post(&max, app.url("/dashboards"), json!({"name": "Max's own"})).await;

    // nobody removes themselves or the owner; only the owner removes admins
    let (admin, admin_id) = app.user(&c, "Ada Admin", "ada@example.com", "admin").await;
    let owner_id = get(&c, app.url("/me")).await["user"]["id"].as_str().unwrap().to_string();
    assert_eq!(send(&admin, Method::DELETE, app.url(&format!("/users/{owner_id}")), json!(null)).await.0, 400);
    assert_eq!(send(&admin, Method::DELETE, app.url(&format!("/users/{admin_id}")), json!(null)).await.0, 400);
    let (_, other_admin) = app.user(&c, "Otto Admin", "otto@example.com", "admin").await;
    assert_eq!(send(&admin, Method::DELETE, app.url(&format!("/users/{other_admin}")), json!(null)).await.0, 403);
    assert_eq!(send(&max, Method::DELETE, app.url(&format!("/users/{admin_id}")), json!(null)).await.0, 403);

    let (s, _) = send(&admin, Method::DELETE, app.url(&format!("/users/{max_id}")), json!(null)).await;
    assert_eq!(s, 204);
    // signed out everywhere, and the password no longer works
    assert!(get(&max, app.url("/me")).await["user"].is_null());
    let (s, _) = send(&client(), Method::POST, app.url("/auth/login"), json!({"email": "max@example.com", "password": "password123"})).await;
    assert_eq!(s, 403);
    // the comment and time log stay, under another name
    let comments = get(&c, app.url("/issues/WEB-1/comments")).await;
    let theirs = comments.as_array().unwrap().iter().find(|x| x["author"]["id"] == max_id.as_str()).unwrap().clone();
    assert!(theirs["author"]["name"].as_str().unwrap().starts_with("Former user "), "{theirs}");
    assert!(!comments.to_string().contains("Max Member"));
    let logs = get(&c, app.url("/issues/WEB-1/worklogs")).await;
    assert!(logs.as_array().unwrap().iter().any(|w| w["user"]["id"] == max_id.as_str()));
    // open work is unassigned; private things are gone
    assert!(get(&c, app.url("/issues/WEB-2")).await["assignee"].is_null());
    assert_eq!(send(&c, Method::GET, app.url(&format!("/dashboards/{}", private["id"].as_str().unwrap())), json!(null)).await.0, 404);
    let users = get(&c, app.url("/users")).await;
    let row = users.as_array().unwrap().iter().find(|u| u["id"] == max_id.as_str()).unwrap().clone();
    assert_eq!((row["removed"].clone(), row["email"].clone(), row["status"].clone()), (json!(true), json!(null), json!("deactivated")));
    // and it can't be undone by reactivating
    assert_eq!(send(&c, Method::PATCH, app.url(&format!("/users/{max_id}")), json!({"status": "active"})).await.0, 400);
    assert_eq!(send(&c, Method::DELETE, app.url(&format!("/users/{max_id}")), json!(null)).await.0, 204, "removing twice is fine");
}
