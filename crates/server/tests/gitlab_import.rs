//! Import from GitLab's REST API (v4), against an in-process mock of it: issues with notes
//! (internal ones too), time logged in notes, state, label and milestone events, links, an epic,
//! confidential issues, uploads, pagination and a rate limit.

mod common;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, Duration, Utc};
use common::*;
use serde_json::{Value, json};
use taskcraft::importer::api::Connection;
use taskcraft::importer::{Count, Options};

const TOKEN: &str = "glpat-test-token";
const PNG: &[u8] = b"\x89PNG-from-gitlab";
const SECRET: &str = "0123456789abcdef0123456789abcdef";

struct Mock {
    base: String,
    t0: DateTime<Utc>,
    hits: Mutex<HashMap<String, u32>>,
    anonymous: Mutex<u32>,
}

impl Mock {
    fn at(&self, hours: f64) -> String {
        (self.t0 + Duration::minutes((hours * 60.0) as i64)).to_rfc3339()
    }

    fn user(&self, name: &str) -> Value {
        let id = match name {
            "alice" => 11,
            "bob" => 12,
            _ => 13,
        };
        json!({"id": id, "username": name, "name": name, "state": "active"})
    }

    fn hit(&self, path: &str) -> u32 {
        let mut h = self.hits.lock().unwrap();
        let n = h.entry(path.to_string()).or_default();
        *n += 1;
        *n
    }

    fn issues(&self) -> Vec<Value> {
        vec![
            json!({
                "id": 7001, "iid": 1, "project_id": 77, "title": "Login fails", "state": "closed", "issue_type": "issue",
                "description": format!("Since today. @bob can you check? Also #2.\n\n![shot](/uploads/{SECRET}/shot.png)"),
                "author": self.user("alice"), "assignees": [self.user("bob"), self.user("carol")], "labels": ["bug", "priority::high"],
                "milestone": {"id": 900, "iid": 1, "title": "1.0", "description": "First", "due_date": "2026-12-01", "state": "active"},
                "weight": 3, "time_stats": {"time_estimate": 7200, "total_time_spent": 9000}, "due_date": "2026-12-05",
                "user_notes_count": 2, "confidential": false, "created_at": self.at(0.0), "updated_at": self.at(51.0), "closed_at": self.at(50.0),
                "epic": {"id": 55, "iid": 3, "title": "Auth", "group_id": 9},
            }),
            json!({
                "id": 7002, "iid": 2, "project_id": 77, "title": "Add SSO", "state": "opened", "issue_type": "issue", "description": null,
                "author": self.user("bob"), "assignees": [], "labels": ["feature"], "milestone": null, "weight": null,
                "time_stats": {"time_estimate": 0, "total_time_spent": 0}, "user_notes_count": 0, "confidential": true,
                "created_at": self.at(1.0), "updated_at": self.at(1.0), "closed_at": null,
            }),
            json!({
                "id": 7003, "iid": 3, "project_id": 77, "title": "Outage", "state": "closed", "issue_type": "incident", "description": "Down.",
                "author": self.user("carol"), "assignees": [], "labels": [], "milestone": null, "time_stats": {}, "user_notes_count": 0,
                "confidential": false, "created_at": self.at(2.0), "updated_at": self.at(10.0), "closed_at": self.at(10.0),
            }),
        ]
    }
}

fn page(v: Value, link: Option<String>) -> Response {
    let mut h = HeaderMap::new();
    if let Some(l) = link {
        h.insert(header::LINK, l.parse().unwrap());
    }
    (h, axum::Json(v)).into_response()
}

async fn handle(State(m): State<Arc<Mock>>, req: Request) -> Response {
    let path = req.uri().path().to_string();
    let query = req.uri().query().unwrap_or("").to_string();
    if req.headers().get("private-token").and_then(|v| v.to_str().ok()) != Some(TOKEN) {
        *m.anonymous.lock().unwrap() += 1;
    }
    let n = m.hit(&path);
    if path == format!("/acme/web/uploads/{SECRET}/shot.png") {
        return ([(header::CONTENT_TYPE, "image/png")], PNG.to_vec()).into_response();
    }
    let Some(api) = path.strip_prefix("/api/v4/") else { return StatusCode::NOT_FOUND.into_response() };
    let b = &m.base;
    let ev = |user: &str, h: f64, extra: Value| {
        let mut v = json!({"user": m.user(user), "created_at": m.at(h)});
        for (k, x) in extra.as_object().unwrap() {
            v[k] = x.clone();
        }
        v
    };
    let note = |id: i64, user: &str, h: f64, body: &str, system: bool, internal: bool| json!({"id": id, "author": m.user(user), "created_at": m.at(h), "updated_at": m.at(h), "body": body, "system": system, "internal": internal});
    match api {
        "projects/acme%2Fweb" => page(
            json!({"id": 77, "name": "Web", "path": "web", "path_with_namespace": "acme/web", "description": "The *web*", "web_url": format!("{b}/acme/web"), "issues_enabled": true}),
            None,
        ),
        "projects/77/issues" => {
            let all = m.issues();
            if query.contains("page=2") {
                page(json!(all[2..]), None)
            } else {
                assert!(query.contains("scope=all"), "{query}");
                page(json!(all[..2]), Some(format!("<{b}/api/v4/projects/77/issues?scope=all&per_page=100&page=2>; rel=\"next\"")))
            }
        }
        // a rate limit once
        "projects/77/issues/1/notes" if n == 1 => (StatusCode::TOO_MANY_REQUESTS, [("retry-after", "1")], "Retry later").into_response(),
        "projects/77/issues/1/notes" => page(
            json!([
                note(1, "alice", 1.0, "assigned to @bob", true, false),
                note(2, "bob", 2.0, "Looking at it, see #2 and ~bug.", false, false),
                note(3, "bob", 3.0, &format!("added 1h 30m of time spent at {}", (m.t0 + Duration::hours(3)).date_naive()), true, false),
                note(4, "bob", 4.0, "assigned to @carol", true, false),
                note(5, "alice", 5.0, "changed title from **Login fail** to **Login fail{+s+}**", true, false),
                note(6, "carol", 6.0, "Customer X is affected.", false, true),
                note(7, "carol", 7.0, "added 30m of time spent", true, false),
            ]),
            None,
        ),
        "projects/77/issues/1/resource_state_events" => page(
            json!([ev("bob", 20.0, json!({"state": "closed"})), ev("alice", 30.0, json!({"state": "reopened"})), ev("bob", 50.0, json!({"state": "closed"}))]),
            None,
        ),
        "projects/77/issues/1/resource_label_events" => page(
            json!([
                ev("alice", 0.5, json!({"action": "add", "label": {"name": "bug"}})),
                ev("alice", 0.6, json!({"action": "add", "label": {"name": "priority::high"}}))
            ]),
            None,
        ),
        "projects/77/issues/1/resource_milestone_events" => {
            page(json!([ev("alice", 1.0, json!({"action": "add", "milestone": {"id": 900, "title": "1.0"}}))]), None)
        }
        "projects/77/issues/1/links" => page(json!([{"id": 7003, "iid": 3, "issue_link_id": 501, "link_type": "blocks", "link_created_at": m.at(8.0)}]), None),
        "projects/77/issues/3/notes" | "projects/77/issues/3/resource_milestone_events" => page(json!([]), None),
        "projects/77/issues/3/resource_state_events" => page(json!([ev("carol", 10.0, json!({"state": "closed"}))]), None),
        "projects/77/issues/3/resource_label_events" => (StatusCode::NOT_FOUND, axum::Json(json!({"message": "404 Not found"}))).into_response(),
        "projects/77/issues/3/links" => page(json!([{"id": 7001, "iid": 1, "issue_link_id": 501, "link_type": "is_blocked_by"}]), None),
        p if p.starts_with("projects/77/issues/2/") => panic!("an untouched issue needs nothing more: {p}"),
        "groups/9/epics/3" => (StatusCode::FORBIDDEN, axum::Json(json!({"message": "403 Forbidden"}))).into_response(),
        p if p == format!("projects/77/uploads/{SECRET}/shot.png") => (StatusCode::NOT_FOUND, "older server").into_response(),
        "users/11" => page(json!({"id": 11, "username": "alice", "name": "Alice Admin", "public_email": "alice@example.com", "state": "active"}), None),
        "users/12" => page(json!({"id": 12, "username": "bob", "name": "Bob Builder", "public_email": "", "state": "active"}), None),
        "users/13" => page(json!({"id": 13, "username": "carol", "name": "Carol Former", "state": "blocked"}), None),
        _ => (StatusCode::NOT_FOUND, axum::Json(json!({"message": format!("no mock for {api}")}))).into_response(),
    }
}

async fn mock() -> Arc<Mock> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let m = Arc::new(Mock { base, t0: Utc::now() - Duration::days(20), hits: Mutex::default(), anonymous: Mutex::default() });
    let app = Router::new().fallback(handle).with_state(m.clone());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    m
}

async fn count(app: &TestApp, sql: &str) -> i64 {
    sqlx::query_scalar(sqlx::AssertSqlSafe(sql.to_string())).fetch_one(&app.state.db).await.unwrap()
}

#[tokio::test]
async fn gitlab_issues_come_over_with_notes_time_and_history() {
    let m = mock().await;
    let app = spawn().await;
    let c = app.setup(false).await;
    let (_alice, alice_id) = app.user(&c, "Alice A.", "alice@example.com", "member").await;
    let owner: taskcraft::auth::User =
        sqlx::query_as("SELECT id, workspace_id, email, name, role, status FROM users WHERE role = 'owner'").fetch_one(&app.state.db).await.unwrap();
    let conn: Connection = serde_json::from_value(json!({"source": "gitlab", "baseUrl": m.base, "token": TOKEN, "projects": ["acme/web"]})).unwrap();

    let r = taskcraft::importer::jobs::import_api(&app.state, &owner, &conn, &Options::default()).await.unwrap();
    let text = r.to_string();
    let web = &r.projects[0];
    assert_eq!((web.source.as_str(), web.key.as_str()), ("acme/web", "WEB"), "{text}");
    assert_eq!(web.counts["issues"].imported, 4, "three issues and their epic: {text}");
    assert_eq!(web.counts["comments"].imported, 2, "{text}");
    assert_eq!(web.counts["worklogs"].imported, 3, "{text}");
    assert_eq!(web.counts["links"].imported, 1, "one link, seen from both sides: {text}");
    assert_eq!(web.counts["confidential"].imported, 1, "{text}");
    assert_eq!(web.counts["attachments"].imported, 1, "{text}");
    assert_eq!(r.people.get("matched"), Some(&1), "{text}");
    assert_eq!(r.people.get("invited"), Some(&1), "{text}");
    assert_eq!(r.people.get("deactivated"), Some(&1), "a blocked account: {text}");
    assert!(r.warnings.iter().any(|w| w.code == "confidential"), "{text}");
    assert!(r.warnings.iter().any(|w| w.code == "restricted-comments"), "internal notes: {text}");
    assert_eq!(*m.anonymous.lock().unwrap(), 0);
    assert_eq!(m.hits.lock().unwrap()["/api/v4/projects/77/issues/1/notes"], 2, "waited for the rate limit, then asked again");

    let login = get(&c, app.url("/issues/WEB-1")).await;
    assert_eq!(login["title"], "Login fails");
    assert_eq!(login["type"]["name"], "Bug");
    assert_eq!((login["status"]["category"].as_str(), login["resolution"].as_str()), (Some("done"), Some("Done")));
    assert_eq!(login["reporter"]["id"], alice_id.as_str());
    assert_eq!(login["assignee"]["name"], "Bob Builder");
    assert_eq!(login["labels"], json!(["bug", "priority::high"]), "scoped labels stay labels");
    assert_eq!(login["version"]["name"], "1.0");
    assert_eq!(login["storyPoints"], 3.0, "the weight");
    assert_eq!(login["estimateMinutes"], 120);
    assert_eq!(login["spentMinutes"], 150, "the notes' 2 hours and what they don't explain");
    assert_eq!(login["dueDate"], "2026-12-05");
    assert_eq!(login["parent"]["title"], "Auth", "the epic is the parent");
    let desc = login["description"].to_string();
    assert!(desc.contains(r##""id":"WEB-2","label":"WEB-2","mentionSuggestionChar":"#""##), "{desc}");
    assert!(desc.contains(r#""mentionSuggestionChar":"@""#), "{desc}");
    let att = &login["attachments"][0];
    assert_eq!(att["name"], "shot.png");
    assert!(desc.contains(att["url"].as_str().unwrap()), "the upload points to the stored file: {desc}");
    let got = c.get(format!("{}{}", app.base, att["url"].as_str().unwrap())).send().await.unwrap();
    assert_eq!(&got.bytes().await.unwrap()[..], PNG);
    let logs = get(&c, app.url("/issues/WEB-1/worklogs")).await;
    let mut logs: Vec<(String, i64)> =
        logs.as_array().unwrap().iter().map(|l| (l["user"]["name"].as_str().unwrap().to_string(), l["minutes"].as_i64().unwrap())).collect();
    logs.sort();
    assert_eq!(logs, vec![("Bob Builder".to_string(), 30), ("Bob Builder".to_string(), 90), ("Carol Former".to_string(), 30)]);
    let comments = get(&c, app.url("/issues/WEB-1/comments")).await;
    assert_eq!(comments[1]["author"]["name"], "Carol Former");
    assert_eq!(get(&c, app.url("/issues/WEB-2")).await["status"]["category"], "todo");
    let outage = get(&c, app.url("/issues/WEB-3")).await;
    assert_eq!(outage["type"]["name"], "Incident");
    assert_eq!(outage["links"][0]["label"], "is blocked by");
    assert_eq!(outage["links"][0]["issue"]["key"], "WEB-1");
    let epic = get(&c, app.url("/issues/WEB-4")).await;
    assert_eq!((epic["type"]["name"].as_str(), epic["title"].as_str()), (Some("Epic"), Some("Auth")));

    // history: closed, reopened, closed; the assignee; the title; labels; the release
    let h = get(&c, app.url("/issues/WEB-1/history")).await;
    let h = h.as_array().unwrap();
    let field = |f: &str| h.iter().filter(|e| e["field"] == f && e["kind"] == "field").cloned().collect::<Vec<_>>();
    let statuses: Vec<String> = field("status").iter().map(|e| e["new"]["category"].as_str().unwrap().to_string()).collect();
    assert_eq!(statuses, vec!["done", "todo", "done"]);
    assert_eq!(field("status")[1]["actor"]["id"], alice_id.as_str());
    assert_eq!(field("assignee").len(), 1);
    assert_eq!(field("title")[0]["new"], "Login fails", "{:?}", field("title"));
    assert_eq!(field("labels").last().unwrap()["new"], json!(["bug", "priority::high"]));
    assert_eq!(field("version")[0]["new"]["name"], "1.0");
    let cycle = get(&c, app.url("/projects/WEB/reports/cycle-time?days=90")).await;
    let item = cycle["issues"].as_array().unwrap().iter().find(|i| i["key"] == "WEB-1").cloned().unwrap();
    assert_eq!(item["leadDays"], 2.1, "created to the last close: {item}");

    // again: nothing twice
    let again = taskcraft::importer::jobs::import_api(&app.state, &owner, &conn, &Options::default()).await.unwrap();
    assert_eq!(again.total("issues"), Count { imported: 0, existing: 4 }, "{again}");
    assert_eq!(count(&app, "SELECT count(*) FROM worklogs").await, 3);
    assert_eq!(count(&app, "SELECT count(*) FROM issue_links").await, 1);
    // a project that isn't there
    let missing: Connection = serde_json::from_value(json!({"source": "gitlab", "baseUrl": m.base, "token": TOKEN, "projects": ["acme/nope"]})).unwrap();
    let e = taskcraft::importer::jobs::import_api(&app.state, &owner, &missing, &Options::default()).await.unwrap_err();
    assert!(e.to_string().contains("no such project"), "{e}");
}
