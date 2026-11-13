//! Subprojects: a project hierarchy, and searches and reports across a project and those below.

mod common;

use common::*;
use reqwest::Method;
use serde_json::{Value, json};

#[tokio::test]
async fn projects_nest_and_views_span_them() {
    let app = spawn().await;
    let admin = app.setup(false).await;
    let mut ids = std::collections::HashMap::new();
    for key in ["SHOP", "APP", "API", "PAY", "OPS"] {
        let p = post(&admin, app.url("/projects"), json!({"key": key, "name": format!("{key} project"), "template": "kanban"})).await;
        ids.insert(key, p["id"].as_str().unwrap().to_string());
        post(&admin, app.url("/issues"), json!({"projectKey": key, "title": format!("Work in {key}")})).await;
    }
    let parent = |child: &str, parent: Option<&str>| {
        let (c, url, body) = (admin.clone(), app.url(&format!("/projects/{child}")), json!({"parentId": parent.map(|p| ids[p].clone())}));
        async move { send(&c, Method::PATCH, url, body).await.0 }
    };
    // SHOP > APP > API, and PAY under API would be a fourth level
    assert!(parent("APP", Some("SHOP")).await < 300);
    assert!(parent("API", Some("APP")).await < 300);
    assert_eq!(parent("PAY", Some("API")).await, 400, "three levels at most");
    assert_eq!(parent("SHOP", Some("API")).await, 400, "no cycles");
    assert_eq!(parent("SHOP", Some("SHOP")).await, 400);
    let shop = get(&admin, app.url("/projects/SHOP")).await;
    assert_eq!(shop["subprojects"].as_array().unwrap().iter().map(|p| p["key"].as_str().unwrap()).collect::<Vec<_>>(), ["APP"]);
    assert_eq!(get(&admin, app.url("/projects/API")).await["parent"]["key"], "APP");
    let list = get(&admin, app.url("/projects")).await;
    assert_eq!(list.as_array().unwrap().iter().find(|p| p["key"] == "APP").unwrap()["parentId"], ids["SHOP"].as_str());

    let keys = |v: &Value| {
        let mut k: Vec<String> = v["issues"].as_array().unwrap().iter().map(|i| i["projectKey"].as_str().unwrap().to_string()).collect();
        k.sort();
        k
    };
    let search = |q: &[(&str, &str)]| {
        let (c, url) = (admin.clone(), app.url("/search"));
        let q: Vec<(String, String)> = q.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect();
        async move { c.get(url).query(&q).send().await.unwrap().json::<Value>().await.unwrap() }
    };
    assert_eq!(keys(&search(&[("project", "SHOP")]).await), ["SHOP"]);
    assert_eq!(keys(&search(&[("project", "SHOP"), ("subprojects", "true")]).await), ["API", "APP", "SHOP"]);
    assert_eq!(keys(&search(&[("q", "project in withSubprojects(APP)")]).await), ["API", "APP"]);
    assert_eq!(keys(&search(&[("q", "project not in withSubprojects(SHOP)")]).await), ["OPS", "PAY"]);
    assert_eq!(send(&admin, Method::GET, app.url("/search?q=project%20%3D%20children(SHOP)"), json!(null)).await.0, 400);

    // reports across them
    let one = get(&admin, app.url("/projects/SHOP/reports/overview")).await;
    let all = get(&admin, app.url("/projects/SHOP/reports/overview?subprojects=true")).await;
    assert_eq!((one["categories"]["todo"].as_i64(), all["categories"]["todo"].as_i64()), (Some(1), Some(3)));

    // only what the caller sees
    let (vera, vera_id) = app.user(&admin, "Vera Guest", "vera@example.com", "guest").await;
    for key in ["SHOP", "APP"] {
        send(&admin, Method::PUT, app.url(&format!("/projects/{key}/members")), json!([{"subjectType": "user", "subjectId": vera_id, "role": "viewer"}])).await;
    }
    let v: Value = vera.get(app.url("/search")).query(&[("project", "SHOP"), ("subprojects", "true")]).send().await.unwrap().json().await.unwrap();
    assert_eq!(keys(&v), ["APP", "SHOP"]);
    let shop = get(&vera, app.url("/projects/SHOP")).await;
    assert_eq!(shop["subprojects"].as_array().unwrap().len(), 1);
    // and a project leaves the tree again
    assert!(parent("APP", None).await < 300);
    assert_eq!(keys(&search(&[("project", "SHOP"), ("subprojects", "true")]).await), ["SHOP"]);
}
