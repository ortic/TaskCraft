//! Link types of the workspace's own: links read both ways, dependencies block, TQL finds them,
//! renaming and deleting (links move to another type), and a project export carries them along.

mod common;

use common::*;
use reqwest::Method;
use serde_json::{Value, json};

async fn keys(c: &reqwest::Client, app: &TestApp, q: &str) -> Vec<String> {
    let v: Value = c.get(app.url("/search")).query(&[("q", q)]).send().await.unwrap().json().await.unwrap();
    assert!(v["issues"].is_array(), "{q}: {v}");
    let mut k: Vec<String> = v["issues"].as_array().unwrap().iter().map(|i| i["key"].as_str().unwrap().to_string()).collect();
    k.sort();
    k
}

#[tokio::test]
async fn own_link_types() {
    let app = spawn().await;
    let admin = app.setup(false).await;
    post(&admin, app.url("/projects"), json!({"key": "LNK", "name": "Links", "template": "kanban"})).await;
    let (mia, _) = app.user(&admin, "Mia Member", "mia@example.com", "member").await;
    let mut k = Vec::new();
    for t in ["Outage", "Config change", "Release", "Rollout"] {
        k.push(post(&admin, app.url("/issues"), json!({"projectKey": "LNK", "title": t})).await["key"].as_str().unwrap().to_string());
    }

    // only admins add types; names must not read like another type's
    assert_eq!(send(&mia, Method::POST, app.url("/link-types"), json!({"outward": "causes"})).await.0, 403);
    let causes = post(&admin, app.url("/link-types"), json!({"outward": "causes", "inward": "is caused by"})).await;
    assert_eq!(causes["key"], "causes");
    let (code, e) = send(&admin, Method::POST, app.url("/link-types"), json!({"outward": "Is blocked by"})).await;
    assert_eq!(code, 400, "{e}");
    let precedes = post(&admin, app.url("/link-types"), json!({"outward": "precedes", "inward": "follows", "dependency": true})).await;
    let types = get(&mia, app.url("/link-types")).await;
    let list: Vec<&str> = types.as_array().unwrap().iter().map(|t| t["key"].as_str().unwrap()).collect();
    assert_eq!(list, ["blocks", "relates", "duplicates", "clones", "causes", "precedes"]);

    // a link by key (outward) and by its inward name; each issue reads it its way
    let (code, _) = send(&mia, Method::POST, app.url(&format!("/issues/{}/links", k[1])), json!({"kind": "causes", "target": k[0]})).await;
    assert_eq!(code, 201);
    let (code, _) = send(&mia, Method::POST, app.url(&format!("/issues/{}/links", k[3])), json!({"kind": "follows", "target": k[2]})).await;
    assert_eq!(code, 201);
    let (code, e) = send(&mia, Method::POST, app.url(&format!("/issues/{}/links", k[3])), json!({"kind": "befriends", "target": k[2]})).await;
    assert!(code == 400 && e.to_string().contains("causes / is caused by"), "{e}");
    let outage = get(&mia, app.url(&format!("/issues/{}", k[0]))).await;
    assert_eq!(outage["links"][0]["label"], "is caused by");
    assert_eq!(outage["links"][0]["kind"], "causes");
    assert_eq!(outage["links"][0]["dependency"], false);
    let history = get(&mia, app.url(&format!("/issues/{}/history", k[0]))).await;
    assert!(history.to_string().contains("is caused by"), "{history}");

    // a dependency type blocks the issue that follows while the one before it is open
    let rollout = get(&mia, app.url(&format!("/issues/{}", k[3]))).await;
    assert_eq!(rollout["blocked"], true);
    assert_eq!(rollout["links"][0]["label"], "follows");
    assert_eq!(keys(&mia, &app, "project = LNK and blocked = true").await, [k[3].clone()]);

    // TQL: by key either way, by a name one way, linkedIssues()
    assert_eq!(keys(&mia, &app, "link = causes").await, [k[0].clone(), k[1].clone()]);
    assert_eq!(keys(&mia, &app, "link = \"is caused by\"").await, [k[0].clone()]);
    assert_eq!(keys(&mia, &app, &format!("key in linkedIssues({})", k[2])).await, [k[3].clone()]);
    assert_eq!(keys(&mia, &app, &format!("key in linkedIssues({}, precedes)", k[2])).await, [k[3].clone()]);
    assert!(keys(&mia, &app, &format!("key in linkedIssues({}, follows)", k[2])).await.is_empty(), "read from the first issue");
    assert_eq!(keys(&mia, &app, "project = LNK and link is empty").await.len(), 0);
    let suggest = get(&mia, app.url("/query/suggest?field=link")).await;
    assert!(suggest.to_string().contains("\\\"is caused by\\\""), "{suggest}");

    // renaming, also a built-in type; its old name still works when linking
    send(&admin, Method::PATCH, app.url("/link-types/causes"), json!({"inward": "results from"})).await;
    send(&admin, Method::PATCH, app.url("/link-types/relates"), json!({"outward": "is related to"})).await;
    let types = get(&admin, app.url("/link-types")).await;
    let relates = types.as_array().unwrap().iter().find(|t| t["key"] == "relates").unwrap();
    assert_eq!(
        (relates["outward"].as_str(), relates["inward"].as_str(), relates["renamed"].as_bool()),
        (Some("is related to"), Some("is related to"), Some(true))
    );
    let (code, _) = send(&mia, Method::POST, app.url(&format!("/issues/{}/links", k[0])), json!({"kind": "relates to", "target": k[2]})).await;
    assert_eq!(code, 201);
    let outage = get(&mia, app.url(&format!("/issues/{}", k[0]))).await;
    let labels: Vec<&str> = outage["links"].as_array().unwrap().iter().map(|l| l["label"].as_str().unwrap()).collect();
    assert_eq!(labels, ["results from", "is related to"]);
    let (code, _) = send(&admin, Method::PATCH, app.url("/link-types/blocks"), json!({"dependency": false})).await;
    assert_eq!(code, 400, "built-in types stay what they are");

    // deleting: a type in use needs a replacement; the links move
    let (code, _) = send(&admin, Method::DELETE, app.url("/link-types/precedes"), json!(null)).await;
    assert_eq!(code, 409);
    let (code, _) = send(&admin, Method::DELETE, app.url("/link-types/precedes?replaceWith=blocks"), json!(null)).await;
    assert_eq!(code, 204);
    let rollout = get(&mia, app.url(&format!("/issues/{}", k[3]))).await;
    assert_eq!(
        (rollout["links"][0]["kind"].as_str(), rollout["links"][0]["label"].as_str(), rollout["blocked"].as_bool()),
        (Some("blocks"), Some("is blocked by"), Some(true))
    );
    assert_eq!(send(&admin, Method::DELETE, app.url("/link-types/relates"), json!(null)).await.0, 204, "back to its own name");
    assert_eq!(precedes["key"], "precedes");

    // a project export brings its own link types into another TaskCraft
    let res = admin.get(app.url("/projects/LNK/export")).send().await.unwrap();
    assert_eq!(res.status(), 200);
    let zip = res.bytes().await.unwrap().to_vec();
    let b = spawn().await;
    let o = b.setup(false).await;
    post(&o, b.url("/link-types"), json!({"outward": "triggers", "inward": "is caused by"})).await;
    let job: Value = o
        .post(b.url("/admin/backup-imports"))
        .query(&[("name", "LNK.taskcraft.zip")])
        .header("content-type", "application/zip")
        .body(zip)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = job["id"].as_str().unwrap().to_string();
    let job = settle(&o, &b, &id).await;
    assert_eq!(job["status"], "checked", "{job}");
    let (st, body) =
        send(&o, Method::POST, b.url(&format!("/admin/backup-imports/{id}/run")), json!({"projects": {"LNK": {"action": "new", "key": "LNK"}}})).await;
    assert_eq!(st, 202, "{body}");
    let job = settle(&o, &b, &id).await;
    assert_eq!(job["status"], "done", "{job}");
    assert!(job.to_string().contains("link-type"), "the report says a type came along: {job}");
    let here = get(&o, b.url("/link-types")).await;
    let causes = here.as_array().unwrap().iter().find(|t| t["outward"] == "causes").expect("causes came along");
    assert_eq!((causes["key"].as_str(), causes["inward"].as_str()), (Some("causes"), Some("results from")));
    let outage = get(&o, b.url(&format!("/issues/{}", k[0]))).await;
    assert_eq!(outage["links"][0]["label"], "results from", "{outage}");
}

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
