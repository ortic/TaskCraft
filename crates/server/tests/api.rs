//! End-to-end API tests: each test gets a fresh database and the full app over HTTP.

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::*;
use reqwest::Method;
use serde_json::{Value, json};

async fn project(app: &TestApp, c: &reqwest::Client, key: &str, template: &str) -> Value {
    post(c, app.url("/projects"), json!({"key": key, "name": format!("{key} project"), "template": template})).await;
    get(c, app.url(&format!("/projects/{key}"))).await
}

fn status_id(p: &Value, name: &str) -> String {
    p["statuses"].as_array().unwrap().iter().find(|s| s["name"] == name).unwrap()["id"].as_str().unwrap().to_string()
}

fn type_id(p: &Value, name: &str) -> String {
    p["types"].as_array().unwrap().iter().find(|s| s["name"] == name).unwrap()["id"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn setup_login_csrf_and_roles() {
    let app = spawn().await;
    assert_eq!(get(&client(), app.url("/setup")).await["needsSetup"], true);
    let owner = app.setup(false).await;
    let (s, _) =
        send(&client(), Method::POST, app.url("/setup"), json!({"workspaceName": "x", "name": "x", "email": "x@x.io", "password": "password123"})).await;
    assert_eq!(s, 409, "setup works only once");
    let (s, _) = send(&client(), Method::POST, app.url("/auth/login"), json!({"email": "owner@example.com", "password": "wrong-password"})).await;
    assert_eq!(s, 403);
    // cookie sessions need the CSRF header on writes
    let bare = reqwest::Client::builder().cookie_store(true).build().unwrap();
    bare.post(app.url("/auth/login")).json(&json!({"email": "owner@example.com", "password": "password123"})).send().await.unwrap();
    let r = bare.post(app.url("/projects")).json(&json!({"key": "NOP", "name": "x"})).send().await.unwrap();
    assert_eq!(r.status(), 400);

    let p = project(&app, &owner, "WEB", "scrum").await;
    assert_eq!(p["statuses"].as_array().unwrap().len(), 4);
    assert_eq!(p["kind"], "scrum");
    // members see projects by the default role; guests only with a grant
    let (member, _) = app.user(&owner, "Max Member", "max@example.com", "member").await;
    let (guest, guest_id) = app.user(&owner, "Gail Guest", "gail@example.com", "guest").await;
    assert_eq!(get(&member, app.url("/projects")).await.as_array().unwrap().len(), 1);
    assert_eq!(get(&guest, app.url("/projects")).await.as_array().unwrap().len(), 0);
    let issue = post(&member, app.url("/issues"), json!({"projectKey": "WEB", "title": "Member can create"})).await;
    let (s, _) = send(&guest, Method::GET, app.url(&format!("/issues/{}", issue["key"].as_str().unwrap())), json!(null)).await;
    assert_eq!(s, 404, "guests don't see projects they aren't in");
    // a project admin who isn't a workspace admin can't remove their own admin rights
    let (lead, lead_id) = app.user(&owner, "Lea Lead", "lea@example.com", "member").await;
    let (s, _) = send(&owner, Method::PUT, app.url("/projects/WEB/members"), json!([{"subjectType": "user", "subjectId": lead_id, "role": "admin"}])).await;
    assert_eq!(s, 204);
    let (s, _) = send(&lead, Method::PUT, app.url("/projects/WEB/members"), json!([{"subjectType": "user", "subjectId": guest_id, "role": "viewer"}])).await;
    assert_eq!(s, 400, "the admin may not lock themselves out");
    let owner_id = get(&owner, app.url("/me")).await["user"]["id"].as_str().unwrap().to_string();
    let (s, v) = send(
        &owner,
        Method::PUT,
        app.url("/projects/WEB/members"),
        json!([{"subjectType": "user", "subjectId": guest_id, "role": "viewer"}, {"subjectType": "user", "subjectId": owner_id, "role": "admin"}]),
    )
    .await;
    assert_eq!(s, 204, "{v}");
    assert_eq!(get(&guest, app.url("/issues/WEB-1")).await["title"], "Member can create");
    let (s, _) = send(&guest, Method::PATCH, app.url("/issues/WEB-1"), json!({"title": "nope"})).await;
    assert_eq!(s, 403, "viewers can't edit");
    let (s, _) = send(&guest, Method::POST, app.url("/issues/WEB-1/comments"), json!({"markdown": "viewers may comment"})).await;
    assert_eq!(s, 201);
    // project settings are for project admins
    let (s, _) = send(&member, Method::PATCH, app.url("/projects/WEB"), json!({"name": "renamed"})).await;
    assert_eq!(s, 403);
}

#[tokio::test]
async fn issues_fields_history_search_links_and_trash() {
    let app = spawn().await;
    let c = app.setup(false).await;
    let p = project(&app, &c, "WEB", "scrum").await;
    let me = get(&c, app.url("/me")).await["user"]["id"].as_str().unwrap().to_string();
    let epic = post(&c, app.url("/issues"), json!({"projectKey": "WEB", "title": "Checkout", "typeId": type_id(&p, "Epic")})).await;
    let a = post(
        &c,
        app.url("/issues"),
        json!({"projectKey": "WEB", "title": "  Guest   checkout ", "descriptionMarkdown": "Buy **without** an account", "parentId": epic["id"], "storyPoints": 5, "labels": ["Front End", "ux"], "assigneeId": me, "priority": "high", "dueDate": "2030-01-31"}),
    )
    .await;
    assert_eq!(a["key"], "WEB-2");
    assert_eq!(a["title"], "Guest checkout");
    assert_eq!(a["labels"], json!(["front-end", "ux"]));
    assert_eq!(a["parent"]["key"], "WEB-1");
    assert_eq!(a["priority"], "high");
    assert_eq!(a["storyPoints"], 5.0);
    let b = post(&c, app.url("/issues"), json!({"projectKey": "WEB", "title": "Payment provider contract", "typeId": type_id(&p, "Task")})).await;
    // status change records history and resolves
    let done = status_id(&p, "Done");
    let s = patch(&c, app.url("/issues/WEB-2"), json!({"statusId": done})).await;
    assert_eq!(s["status"]["category"], "done");
    assert!(s["resolvedAt"].is_string());
    let h = get(&c, app.url("/issues/WEB-2/history")).await;
    assert!(h.as_array().unwrap().iter().any(|e| e["field"] == "status" && e["new"]["name"] == "Done"));
    assert_eq!(h[0]["kind"], "created");
    // invalid changes are refused
    for bad in [
        json!({"statusId": status_id(&project(&app, &c, "OPS", "kanban").await, "Backlog")}),
        json!({"priority": "sometimes"}),
        json!({"parentId": a["id"]}),
        json!({"bogus": 1}),
    ] {
        let (s, v) = send(&c, Method::PATCH, app.url("/issues/WEB-2"), bad.clone()).await;
        assert_eq!(s, 400, "{bad} → {v}");
    }
    // epic progress
    let e = get(&c, app.url("/issues/WEB-1")).await;
    assert_eq!((e["childCount"].as_i64(), e["childDone"].as_i64()), (Some(1), Some(1)));
    assert_eq!(e["children"][0]["key"], "WEB-2");
    // TQL
    let search = |q: &str| {
        let c = c.clone();
        let url = app.url("/search");
        let q = q.to_string();
        async move {
            let r = c.get(&url).query(&[("q", q.as_str())]).send().await.unwrap();
            assert!(r.status().is_success(), "{q}: {}", r.text().await.unwrap());
            let v: Value = r.json().await.unwrap();
            v["issues"].as_array().unwrap().iter().map(|i| i["key"].as_str().unwrap().to_string()).collect::<Vec<_>>()
        }
    };
    assert_eq!(search("project = WEB and assignee = me()").await, vec!["WEB-2"]);
    assert_eq!(search("labels = ux and points >= 5").await, vec!["WEB-2"]);
    assert_eq!(search("category = done").await, vec!["WEB-2"]);
    assert_eq!(search("parent = WEB-1").await, vec!["WEB-2"]);
    assert_eq!(search("without account").await, vec!["WEB-2"], "words search the description");
    assert_eq!(search("WEB-3").await, vec!["WEB-3"], "a key finds the issue");
    assert_eq!(search("project = WEB order by key desc").await, vec!["WEB-3", "WEB-2", "WEB-1"]);
    assert_eq!(search("assignee is empty and type != Epic").await, vec!["WEB-3"]);
    assert_eq!(search("due < 2030-02-01 and due > -1d").await, vec!["WEB-2"]);
    let r = c.get(app.url("/search")).query(&[("q", "status > Done")]).send().await.unwrap();
    assert_eq!(r.status(), 400);
    // links: WEB-3 blocks WEB-1
    post(&c, app.url("/issues/WEB-3/links"), json!({"kind": "blocks", "target": "WEB-1"})).await;
    assert_eq!(get(&c, app.url("/issues/WEB-1")).await["blocked"], true);
    assert_eq!(search("blocked = true").await, vec!["WEB-1"]);
    patch(&c, app.url("/issues/WEB-3"), json!({"statusId": done})).await;
    assert_eq!(get(&c, app.url("/issues/WEB-1")).await["blocked"], false);
    let detail = get(&c, app.url("/issues/WEB-1")).await;
    assert_eq!(detail["links"][0]["label"], "is blocked by");
    // comments, time
    post(&c, app.url("/issues/WEB-3/comments"), json!({"markdown": "Signed today"})).await;
    let w = post(&c, app.url("/issues/WEB-3/worklogs"), json!({"time": "1h 30m"})).await;
    assert_eq!(w["minutes"], 90);
    assert_eq!(get(&c, app.url("/issues/WEB-3")).await["spentMinutes"], 90);
    // CSV export escapes formulas
    patch(&c, app.url("/issues/WEB-3"), json!({"title": "=HYPERLINK(\"x\")"})).await;
    let csv = c.get(app.url("/search.csv")).query(&[("q", "key = WEB-3")]).send().await.unwrap().text().await.unwrap();
    assert!(csv.contains("'=HYPERLINK"), "{csv}");
    // trash and restore
    let (s, _) = send(&c, Method::DELETE, app.url(&format!("/issues/{}", b["key"].as_str().unwrap())), json!(null)).await;
    assert_eq!(s, 204);
    assert!(search("project = WEB").await.len() == 2);
    assert_eq!(get(&c, app.url("/projects/WEB/trash")).await[0]["key"], "WEB-3");
    post(&c, app.url("/issues/WEB-3/restore"), json!({})).await;
    assert_eq!(search("project = WEB").await.len(), 3);
    // renaming the project keeps old keys working
    patch(&c, app.url("/projects/WEB"), json!({"key": "SHOP"})).await;
    assert_eq!(get(&c, app.url("/issues/WEB-2")).await["key"], "SHOP-2");
    assert_eq!(get(&c, app.url("/projects/WEB")).await["key"], "SHOP");
}

#[tokio::test]
async fn ranking_sprints_burndown_and_velocity() {
    let app = spawn().await;
    let c = app.setup(false).await;
    let p = project(&app, &c, "APP", "scrum").await;
    let sprint = p["sprints"][0]["id"].as_str().unwrap().to_string();
    let mut keys = vec![];
    for (i, pts) in [3.0, 5.0, 8.0].iter().enumerate() {
        let issue = post(&c, app.url("/issues"), json!({"projectKey": "APP", "title": format!("Story {i}"), "storyPoints": pts, "sprintId": sprint})).await;
        keys.push(issue["key"].as_str().unwrap().to_string());
    }
    // move the last one to the top
    let first = get(&c, app.url(&format!("/issues/{}", keys[0]))).await["id"].clone();
    post(&c, app.url(&format!("/issues/{}/move", keys[2])), json!({"before": first})).await;
    let backlog = get(&c, app.url("/projects/APP/backlog")).await;
    let order: Vec<&str> = backlog["sprints"][0]["issues"].as_array().unwrap().iter().map(|i| i["key"].as_str().unwrap()).collect();
    assert_eq!(order, vec!["APP-3", "APP-1", "APP-2"]);
    post(&c, app.url(&format!("/sprints/{sprint}/start")), json!({"weeks": 2, "goal": "Ship"})).await;
    let (s, _) = send(&c, Method::POST, app.url(&format!("/sprints/{sprint}/start")), json!({})).await;
    assert_eq!(s, 400, "an active sprint can't start again");
    // the board shows the sprint
    let board_id = p["boards"][0]["id"].as_str().unwrap();
    let board = get(&c, app.url(&format!("/boards/{board_id}"))).await;
    assert_eq!(board["sprint"]["id"], sprint);
    assert_eq!(board["issues"].as_array().unwrap().len(), 3);
    // finish one, add one during the sprint
    post(&c, app.url("/issues/APP-1/move"), json!({"statusId": status_id(&p, "Done")})).await;
    post(&c, app.url("/issues"), json!({"projectKey": "APP", "title": "Late addition", "storyPoints": 2, "sprintId": sprint})).await;
    let b = get(&c, app.url(&format!("/sprints/{sprint}/burndown"))).await;
    assert_eq!(b["unit"], "points");
    assert_eq!(b["committed"], 16.0);
    assert_eq!(b["remaining"], 15.0);
    assert_eq!(b["added"], 1);
    let done = post(&c, app.url(&format!("/sprints/{sprint}/complete")), json!({"moveTo": "new"})).await;
    assert_eq!(done["moved"], 3);
    assert_eq!(done["stats"]["completed"], 3.0);
    let v = get(&c, app.url("/projects/APP/reports/velocity")).await;
    assert_eq!(v["sprints"][0]["committed"], 16.0);
    assert_eq!(v["sprints"][0]["completed"], 3.0);
    let next = get(&c, app.url("/projects/APP")).await;
    assert_eq!(next["sprints"].as_array().unwrap().len(), 1, "the new sprint holds the carried-over issues");
    for r in ["overview", "cumulative-flow", "cycle-time", "created-resolved", "workload"] {
        get(&c, app.url(&format!("/projects/APP/reports/{r}"))).await;
    }
}

#[tokio::test]
async fn restricted_workflow_and_automation() {
    let app = spawn().await;
    let c = app.setup(false).await;
    let p = project(&app, &c, "SUP", "tracker").await;
    let (new, open, closed) = (status_id(&p, "New"), status_id(&p, "Open"), status_id(&p, "Closed"));
    let (member, _) = app.user(&c, "Max Member", "max@example.com", "member").await;
    let r = c
        .put(app.url("/projects/SUP/transitions"))
        .json(&json!({"workflow": "restricted", "transitions": [{"from": new, "to": open}, {"from": open, "to": closed}]}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 204);
    post(&member, app.url("/issues"), json!({"projectKey": "SUP", "title": "Broken"})).await;
    let (s, v) = send(&member, Method::PATCH, app.url("/issues/SUP-1"), json!({"statusId": closed})).await;
    assert_eq!(s, 400, "{v}");
    assert!(v["message"].as_str().unwrap().contains("workflow"));
    patch(&member, app.url("/issues/SUP-1"), json!({"statusId": open})).await;
    patch(&member, app.url("/issues/SUP-1"), json!({"statusId": closed})).await;
    // automation: critical reports become urgent, get a label and a comment
    let severity = p["fields"].as_array().unwrap().iter().find(|f| f["name"] == "Severity").unwrap().clone();
    let critical = severity["options"].as_array().unwrap().iter().find(|o| o["label"] == "Critical").unwrap()["id"].clone();
    post(
        &c,
        app.url("/projects/SUP/automation"),
        json!({"name": "Critical", "trigger": {"event": "created"}, "condition": "Severity = Critical",
               "actions": [{"action": "setPriority", "priority": "urgent"}, {"action": "addLabel", "label": "hotfix"}, {"action": "comment", "text": "On it."}]}),
    )
    .await;
    let mut custom = serde_json::Map::new();
    custom.insert(severity["id"].as_str().unwrap().into(), critical);
    post(&member, app.url("/issues"), json!({"projectKey": "SUP", "title": "Everything is down", "custom": custom})).await;
    post(&member, app.url("/issues"), json!({"projectKey": "SUP", "title": "Typo"})).await;
    let i = get(&c, app.url("/issues/SUP-2")).await;
    assert_eq!(i["priority"], "urgent");
    assert_eq!(i["labels"], json!(["hotfix"]));
    let comments = get(&c, app.url("/issues/SUP-2/comments")).await;
    assert_eq!(comments[0]["author"], Value::Null, "automation comments have no author");
    assert_eq!(get(&c, app.url("/issues/SUP-3")).await["priority"], "medium", "the condition didn't match");
    let rules = get(&c, app.url("/projects/SUP/automation")).await;
    assert_eq!(rules[0]["runs"], 1);
    // TQL on a custom field
    let r: Value = c.get(app.url("/search")).query(&[("q", "Severity = critical")]).send().await.unwrap().json().await.unwrap();
    assert_eq!(r["total"], 1);
}

#[tokio::test]
async fn mentions_notify_and_tokens_are_scoped() {
    let app = spawn().await;
    let c = app.setup(false).await;
    project(&app, &c, "WEB", "kanban").await;
    let (max, max_id) = app.user(&c, "Max Member", "max@example.com", "member").await;
    post(&c, app.url("/issues"), json!({"projectKey": "WEB", "title": "Review"})).await;
    let body = json!({"type": "doc", "content": [{"type": "paragraph", "content": [
        {"type": "text", "text": "Please look "}, {"type": "mention", "attrs": {"id": max_id, "label": "Max Member", "mentionSuggestionChar": "@"}}]}]});
    post(&c, app.url("/issues/WEB-1/comments"), json!({"body": body})).await;
    patch(&c, app.url("/issues/WEB-1"), json!({"assigneeId": max_id})).await;
    let n = get(&max, app.url("/notifications")).await;
    let kinds: Vec<&str> = n["items"].as_array().unwrap().iter().map(|x| x["kind"].as_str().unwrap()).collect();
    assert!(kinds.contains(&"mentioned") && kinds.contains(&"assigned"), "{kinds:?}");
    assert_eq!(n["unread"], 2);
    post(&max, app.url("/notifications/read"), json!({"all": true})).await;
    assert_eq!(get(&max, app.url("/notifications")).await["unread"], 0);
    // the owner doesn't notify themselves
    assert_eq!(get(&c, app.url("/notifications")).await["unread"], 0);
    // API tokens
    let tok = post(&c, app.url("/me/tokens"), json!({"name": "wiki", "scope": "read"})).await["token"].as_str().unwrap().to_string();
    let api = reqwest::Client::new();
    let r = api.get(format!("{}/api/v1/issues/WEB-1", app.base)).bearer_auth(&tok).send().await.unwrap();
    assert_eq!(r.status(), 200);
    let r = api.get(format!("{}/api/v1/search?q=project%20%3D%20WEB", app.base)).bearer_auth(&tok).send().await.unwrap();
    assert_eq!(r.status(), 200);
    let r = api.post(format!("{}/api/v1/issues", app.base)).bearer_auth(&tok).json(&json!({"projectKey": "WEB", "title": "x"})).send().await.unwrap();
    assert_eq!(r.status(), 403, "read-only token");
    let r = api.get(format!("{}/api/v1/me/tokens", app.base)).bearer_auth(&tok).send().await.unwrap();
    assert_eq!(r.status(), 403, "tokens can't manage tokens");
    let r = api.get(format!("{}/api/v1/issues/WEB-1", app.base)).bearer_auth("tct_nope").send().await.unwrap();
    assert_eq!(r.status(), 401);
}

#[tokio::test]
async fn webhooks_are_signed_and_delivered() {
    use axum::extract::State;
    let received: Arc<Mutex<Vec<(String, String, Vec<u8>)>>> = Default::default();
    let hook = axum::Router::new()
        .route(
            "/hook",
            axum::routing::post(|State(r): State<Arc<Mutex<Vec<(String, String, Vec<u8>)>>>>, h: axum::http::HeaderMap, body: axum::body::Bytes| async move {
                let get = |k: &str| h.get(k).and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
                r.lock().unwrap().push((get("x-taskcraft-event"), get("x-taskcraft-signature"), body.to_vec()));
                "ok"
            }),
        )
        .with_state(received.clone());
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let hook_url = format!("http://{}/hook", l.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(l, hook).await.unwrap() });

    let app = spawn().await;
    let c = app.setup(false).await;
    project(&app, &c, "WEB", "kanban").await;
    let h = post(&c, app.url("/admin/webhooks"), json!({"url": hook_url, "events": ["issue.created"], "secret": "s3cret-value"})).await;
    let test = post(&c, app.url(&format!("/admin/webhooks/{}/test", h["id"].as_str().unwrap())), json!({})).await;
    assert_eq!(test["ok"], true, "{test}");
    post(&c, app.url("/issues"), json!({"projectKey": "WEB", "title": "Hooked"})).await;
    let mut got = None;
    for _ in 0..40 {
        tokio::time::sleep(Duration::from_millis(150)).await;
        if let Some(x) = received.lock().unwrap().iter().find(|x| x.0 == "issue.created").cloned() {
            got = Some(x);
            break;
        }
    }
    let (_, sig, body) = got.expect("delivered");
    assert_eq!(sig, taskcraft::webhooks::sign("s3cret-value", &body));
    let v: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["data"]["issue"]["key"], "WEB-1");
}

#[tokio::test]
async fn wikicraft_pages_from_issues_and_sprints() {
    use axum::Json;
    use axum::extract::Path;
    // a stand-in for WikiCraft's API
    let created: Arc<Mutex<Vec<Value>>> = Default::default();
    let page_id = uuid::Uuid::now_v7();
    let wiki = {
        let created = created.clone();
        axum::Router::new()
            .route(
                "/api/v1/pages/{id}",
                axum::routing::get(move |Path(id): Path<String>, h: axum::http::HeaderMap| async move {
                    assert_eq!(h.get("authorization").unwrap(), "Bearer wct_test");
                    Json(json!({"id": id, "title": "Checkout spec", "spaceKey": "ENG", "spaceName": "Engineering", "updatedAt": "2026-11-01T10:00:00Z"}))
                }),
            )
            .route("/api/v1/spaces", axum::routing::get(|| async { Json(json!([{"key": "ENG", "name": "Engineering", "archivedAt": null}])) }))
            .route(
                "/api/v1/pages",
                axum::routing::post(move |Json(b): Json<Value>| async move {
                    created.lock().unwrap().push(b);
                    (axum::http::StatusCode::CREATED, Json(json!({"id": page_id})))
                }),
            )
    };
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let wiki_url = format!("http://{}", l.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(l, wiki).await.unwrap() });

    let w = wiki_url.clone();
    let app = spawn_with(move |c| {
        c.wikicraft_url = Some(w);
        c.wikicraft_token = Some("wct_test".into());
    })
    .await;
    let c = app.setup(false).await;
    let p = project(&app, &c, "WEB", "scrum").await;
    assert_eq!(get(&c, app.url("/me")).await["wikicraft"]["connected"], true);
    patch(&c, app.url("/projects/WEB"), json!({"settings": {"wikiSpace": "ENG"}})).await;
    post(&c, app.url("/issues"), json!({"projectKey": "WEB", "title": "Guest checkout", "descriptionMarkdown": "Context *here*"})).await;
    // link an existing page: its title comes from WikiCraft
    let link = post(&c, app.url("/issues/WEB-1/wiki-pages"), json!({"url": format!("{wiki_url}/pages/{page_id}")})).await;
    assert_eq!(link["title"], "Checkout spec");
    let pages = get(&c, app.url("/issues/WEB-1/wiki-pages")).await;
    assert_eq!(pages[0]["page"]["spaceName"], "Engineering");
    // WikiCraft asks which issues link a page
    let linked = get(&c, app.url(&format!("/wiki-links?url={wiki_url}/pages/{page_id}"))).await;
    assert_eq!(linked[0]["key"], "WEB-1");
    // write a page for the issue
    let page = post(&c, app.url("/issues/WEB-1/wiki-page"), json!({})).await;
    assert!(page["url"].as_str().unwrap().ends_with(&format!("/pages/{page_id}")));
    {
        let made = created.lock().unwrap();
        assert_eq!(made[0]["spaceKey"], "ENG");
        let md = made[0]["markdown"].as_str().unwrap();
        assert!(md.contains("[WEB-1](") && md.contains("Context *here*"), "{md}");
    }
    // a sprint review page
    let sprint = p["sprints"][0]["id"].as_str().unwrap().to_string();
    patch(&c, app.url("/issues/WEB-1"), json!({"sprintId": sprint})).await;
    post(&c, app.url(&format!("/sprints/{sprint}/start")), json!({})).await;
    post(&c, app.url(&format!("/sprints/{sprint}/wiki-page")), json!({})).await;
    let made = created.lock().unwrap();
    assert!(made[1]["title"].as_str().unwrap().ends_with("review"));
    assert!(made[1]["markdown"].as_str().unwrap().contains("## Not completed"));
}

#[tokio::test]
async fn sample_data_and_live_updates() {
    let app = spawn().await;
    let c = app.setup(true).await;
    let projects = get(&c, app.url("/projects")).await;
    assert_eq!(projects.as_array().unwrap().len(), 4);
    let web = get(&c, app.url("/projects/WEB")).await;
    let active = web["sprints"].as_array().unwrap().iter().find(|s| s["state"] == "active").unwrap()["id"].as_str().unwrap().to_string();
    let b = get(&c, app.url(&format!("/sprints/{active}/burndown"))).await;
    assert!(b["steps"].as_array().unwrap().len() > 3, "{b}");
    let v = get(&c, app.url("/projects/WEB/reports/velocity")).await;
    assert_eq!(v["sprints"].as_array().unwrap().len(), 1);
    // live updates: a change reaches an open event stream
    let stream = c.get(app.url("/events")).send().await.unwrap();
    assert_eq!(stream.headers()["content-type"], "text/event-stream");
    let reader = tokio::spawn(async move {
        let mut stream = stream;
        let mut seen = String::new();
        while let Ok(Some(chunk)) = stream.chunk().await {
            seen.push_str(&String::from_utf8_lossy(&chunk));
            if seen.contains("\"key\":\"WEB-1\"") {
                return true;
            }
        }
        false
    });
    tokio::time::sleep(Duration::from_millis(300)).await;
    patch(&c, app.url("/issues/WEB-1"), json!({"priority": "low"})).await;
    let ok = tokio::time::timeout(Duration::from_secs(5), reader).await.expect("event in time").unwrap();
    assert!(ok);
}
