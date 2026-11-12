//! Import from Redmine's REST API, against an in-process mock of it: a project with a
//! subproject, trackers, statuses (closed ones done), priorities, custom fields, versions,
//! categories, issues with journals (private notes too), attachments, relations, watchers, time
//! entries, Textile texts, offset pagination and a non-administrator's key.

mod common;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::{Request, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, Duration, Utc};
use common::*;
use serde_json::{Value, json};
use taskcraft::importer::api::Connection;
use taskcraft::importer::{Count, Options};

const KEY: &str = "redmine-api-key-1234";
const PNG: &[u8] = b"\x89PNG-from-redmine";

struct Mock {
    t0: DateTime<Utc>,
    /// an administrator's key (custom field definitions, watchers, e-mail addresses)
    admin: bool,
    hits: Mutex<HashMap<String, u32>>,
}

impl Mock {
    fn at(&self, hours: i64) -> String {
        (self.t0 + Duration::hours(hours)).format("%Y-%m-%dT%H:%M:%SZ").to_string()
    }

    fn day(&self, hours: i64) -> String {
        (self.t0 + Duration::hours(hours)).date_naive().to_string()
    }

    fn issue(&self, id: i64) -> Value {
        let user = |id: i64, name: &str| json!({"id": id, "name": name});
        match id {
            1 => json!({
                "id": 1, "project": {"id": 1, "name": "Shop"}, "tracker": {"id": 1, "name": "Bug"},
                "status": {"id": 5, "name": "Closed", "is_closed": true}, "priority": {"id": 3, "name": "High"},
                "author": user(1, "Ada Admin"), "assigned_to": user(6, "Max Muster"), "category": {"id": 1, "name": "Frontend"},
                "fixed_version": {"id": 1, "name": "1.0"}, "subject": "Checkout breaks",
                "description": "h2. Steps\r\n\r\n# Open the cart\r\n# Pay\r\n\r\nSee !screen.png! and @code@, *not* _fixed_. Related: #2, \"spec\":https://example.com/spec.",
                "start_date": self.day(0), "due_date": self.day(240), "done_ratio": 60, "is_private": false, "estimated_hours": 3.0,
                "custom_fields": [{"id": 1, "name": "Severity", "value": "Major"}, {"id": 2, "name": "Customer", "value": "ACME"},
                                  {"id": 3, "name": "Budget", "value": "12.5"}, {"id": 4, "name": "Reviewer", "value": "6"},
                                  {"id": 5, "name": "Platforms", "multiple": true, "value": ["Web", "iOS"]}],
                "created_on": self.at(0), "updated_on": self.at(100), "closed_on": self.at(100),
                "journals": [
                    {"id": 10, "user": user(5, "Jane Doe"), "notes": "Looking at it, see #2.", "created_on": self.at(24), "private_notes": false,
                     "details": [{"property": "attr", "name": "status_id", "old_value": "1", "new_value": "2"}]},
                    {"id": 11, "user": user(1, "Ada Admin"), "notes": "Customer data involved.", "created_on": self.at(30), "private_notes": true, "details": []},
                    {"id": 12, "user": user(1, "Ada Admin"), "notes": "", "created_on": self.at(40), "private_notes": false,
                     "details": [{"property": "attr", "name": "assigned_to_id", "old_value": "5", "new_value": "6"},
                                 {"property": "attr", "name": "priority_id", "old_value": "2", "new_value": "3"},
                                 {"property": "cf", "name": "1", "old_value": "Minor", "new_value": "Major"},
                                 {"property": "attr", "name": "done_ratio", "old_value": "0", "new_value": "60"}]},
                    {"id": 13, "user": user(6, "Max Muster"), "notes": "Fixed in *checkout* (attachment:screen.png).", "created_on": self.at(100), "private_notes": false,
                     "details": [{"property": "attr", "name": "status_id", "old_value": "2", "new_value": "5"}]},
                ],
                "attachments": [{"id": 1, "filename": "screen.png", "filesize": PNG.len(), "content_type": "image/png", "description": "",
                                 "content_url": "http://redmine.internal.example/attachments/download/1/screen.png", "author": user(1, "Ada Admin"), "created_on": self.at(0)}],
                "relations": [{"id": 1, "issue_id": 1, "issue_to_id": 2, "relation_type": "blocks", "delay": null},
                              {"id": 2, "issue_id": 3, "issue_to_id": 1, "relation_type": "precedes", "delay": null}],
                "children": [{"id": 3, "tracker": {"id": 1, "name": "Bug"}, "subject": "API returns 500"}],
                "watchers": if self.admin { json!([user(6, "Max Muster"), user(5, "Jane Doe")]) } else { json!(null) },
            }),
            2 => json!({
                "id": 2, "project": {"id": 1, "name": "Shop"}, "tracker": {"id": 2, "name": "Feature"},
                "status": {"id": 6, "name": "Rejected", "is_closed": true}, "priority": {"id": 2, "name": "Normal"},
                "author": user(5, "Jane Doe"), "subject": "Pay by invoice", "description": "", "is_private": true,
                "created_on": self.at(2), "updated_on": self.at(50), "closed_on": self.at(50), "custom_fields": [],
                "journals": [{"id": 20, "user": user(1, "Ada Admin"), "notes": "", "created_on": self.at(50), "private_notes": false,
                              "details": [{"property": "attr", "name": "status_id", "old_value": "1", "new_value": "6"}]}],
                "attachments": [], "relations": [{"id": 1, "issue_id": 1, "issue_to_id": 2, "relation_type": "blocks", "delay": null}],
            }),
            _ => json!({
                "id": 3, "project": {"id": 2, "name": "Shop API"}, "tracker": {"id": 1, "name": "Bug"},
                "status": {"id": 3, "name": "Resolved", "is_closed": false}, "priority": {"id": 5, "name": "Immediate"},
                "author": user(1, "Ada Admin"), "parent": {"id": 1}, "subject": "API returns 500", "description": null,
                "fixed_version": {"id": 1, "name": "1.0"}, "created_on": self.at(5), "updated_on": self.at(60), "custom_fields": [],
                "journals": [], "attachments": [],
                "relations": [{"id": 2, "issue_id": 3, "issue_to_id": 1, "relation_type": "precedes", "delay": null}],
            }),
        }
    }
}

fn ok(v: Value) -> Response {
    axum::Json(v).into_response()
}

async fn handle(State(m): State<Arc<Mock>>, req: Request) -> Response {
    let path = req.uri().path().to_string();
    let query = req.uri().query().unwrap_or("").to_string();
    let q = |k: &str| query.split('&').find_map(|p| p.strip_prefix(&format!("{k}="))).unwrap_or("").to_string();
    *m.hits.lock().unwrap().entry(path.clone()).or_default() += 1;
    if req.headers().get("x-redmine-api-key").and_then(|v| v.to_str().ok()) != Some(KEY) {
        return (StatusCode::UNAUTHORIZED, "").into_response();
    }
    let forbidden = || (StatusCode::FORBIDDEN, "").into_response();
    match path.as_str() {
        "/users/current.json" => ok(json!({"user": {"id": 1, "login": "ada", "admin": m.admin}})),
        "/projects.json" => {
            // one per page: the offsets add up
            let all = [
                json!({"id": 1, "name": "Shop", "identifier": "shop", "description": "The *web shop*", "is_public": true}),
                json!({"id": 2, "name": "Shop API", "identifier": "shop-api", "parent": {"id": 1, "name": "Shop"}, "is_public": true}),
                json!({"id": 3, "name": "Intranet", "identifier": "intranet", "is_public": false}),
            ];
            let offset: usize = q("offset").parse().unwrap_or(0);
            ok(json!({"projects": all.get(offset).map(|p| vec![p.clone()]).unwrap_or_default(), "total_count": 3, "offset": offset, "limit": 1}))
        }
        "/trackers.json" => ok(json!({"trackers": [{"id": 1, "name": "Bug"}, {"id": 2, "name": "Feature"}]})),
        "/issue_statuses.json" => ok(json!({"issue_statuses": [
            {"id": 1, "name": "New", "is_closed": false}, {"id": 2, "name": "In Progress", "is_closed": false},
            {"id": 3, "name": "Resolved", "is_closed": false}, {"id": 5, "name": "Closed", "is_closed": true}, {"id": 6, "name": "Rejected", "is_closed": true}]})),
        "/enumerations/issue_priorities.json" => ok(json!({"issue_priorities": [
            {"id": 1, "name": "Low"}, {"id": 2, "name": "Normal"}, {"id": 3, "name": "High"}, {"id": 4, "name": "Urgent"}, {"id": 5, "name": "Immediate"}]})),
        "/custom_fields.json" if m.admin => ok(json!({"custom_fields": [
            {"id": 1, "name": "Severity", "customized_type": "issue", "field_format": "list", "multiple": false,
             "possible_values": [{"value": "Minor", "label": "Minor"}, {"value": "Major", "label": "Major"}, {"value": "Critical", "label": "Critical"}]},
            {"id": 2, "name": "Customer", "customized_type": "issue", "field_format": "string"},
            {"id": 3, "name": "Budget", "customized_type": "issue", "field_format": "float"},
            {"id": 4, "name": "Reviewer", "customized_type": "issue", "field_format": "user"},
            {"id": 5, "name": "Platforms", "customized_type": "issue", "field_format": "enumeration", "multiple": true,
             "possible_values": [{"value": "Web", "label": "Web"}, {"value": "iOS", "label": "iOS"}, {"value": "Android", "label": "Android"}]},
            {"id": 9, "name": "Department", "customized_type": "user", "field_format": "string"}]})),
        "/custom_fields.json" => forbidden(),
        "/projects/1/versions.json" | "/projects/2/versions.json" => {
            ok(json!({"versions": [{"id": 1, "project": {"id": 1}, "name": "1.0", "status": "closed", "due_date": m.day(120), "sharing": "descendants"}]}))
        }
        "/projects/1/issue_categories.json" => ok(json!({"issue_categories": [{"id": 1, "name": "Frontend", "assigned_to": {"id": 5, "name": "Jane Doe"}}]})),
        "/projects/2/issue_categories.json" => ok(json!({"issue_categories": []})),
        "/issues.json" => {
            assert_eq!(q("status_id"), "*", "open and closed issues");
            assert_eq!(q("subproject_id"), "!*", "without the subprojects' issues");
            let ids: Vec<i64> = if q("project_id") == "1" { vec![1, 2] } else { vec![3] };
            let offset: usize = q("offset").parse().unwrap_or(0);
            let page: Vec<Value> = ids.iter().skip(offset).take(1).map(|i| json!({"id": i})).collect();
            ok(json!({"issues": page, "total_count": ids.len(), "offset": offset, "limit": 1}))
        }
        "/issues/1.json" | "/issues/2.json" | "/issues/3.json" => {
            assert!(query.contains("include=journals,attachments,relations,children,watchers"), "{query}");
            let id: i64 = path.trim_start_matches("/issues/").trim_end_matches(".json").parse().unwrap();
            ok(json!({"issue": m.issue(id)}))
        }
        "/time_entries.json" => {
            let entries = vec![
                json!({"id": 1, "project": {"id": 1}, "issue": {"id": 1}, "user": {"id": 5, "name": "Jane Doe"}, "activity": {"id": 9, "name": "Development"},
                       "hours": 1.5, "comments": "Debugging", "spent_on": m.day(26), "created_on": m.at(26)}),
                json!({"id": 2, "project": {"id": 1}, "user": {"id": 1, "name": "Ada Admin"}, "activity": {"id": 8, "name": "Design"},
                       "hours": 2.0, "comments": "Planning", "spent_on": m.day(1), "created_on": m.at(1)}),
                json!({"id": 3, "project": {"id": 2}, "issue": {"id": 3}, "user": {"id": 1, "name": "Ada Admin"}, "activity": {"id": 8, "name": "Design"},
                       "hours": 0.5, "comments": "", "spent_on": m.day(6), "created_on": m.at(6)}),
            ];
            // the main project's list holds the subproject's entries too
            let list: Vec<Value> = if q("project_id") == "1" { entries } else { entries.into_iter().filter(|e| e["project"]["id"] == 2).collect() };
            ok(json!({"time_entries": list, "total_count": 3, "offset": 0, "limit": 100}))
        }
        "/users/1.json" => ok(
            json!({"user": {"id": 1, "login": "ada", "firstname": "Ada", "lastname": "Admin", "mail": if m.admin { json!("ada@example.com") } else { json!(null) }, "status": 1}}),
        ),
        "/users/5.json" => ok(
            json!({"user": {"id": 5, "login": "jane", "firstname": "Jane", "lastname": "Doe", "mail": if m.admin { json!("jane@example.com") } else { json!(null) }}}),
        ),
        "/users/6.json" if m.admin => {
            ok(json!({"user": {"id": 6, "login": "max", "firstname": "Max", "lastname": "Muster", "mail": "max@example.com", "status": 3}}))
        }
        "/users/6.json" => (StatusCode::NOT_FOUND, "").into_response(),
        "/attachments/download/1/screen.png" => ([(header::CONTENT_TYPE, "image/png")], PNG.to_vec()).into_response(),
        _ => (StatusCode::NOT_FOUND, axum::Json(json!({"errors": [format!("no mock for {path}")]}))).into_response(),
    }
}

async fn mock(admin: bool) -> (Arc<Mock>, String) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/", listener.local_addr().unwrap());
    let m = Arc::new(Mock { t0: Utc::now() - Duration::days(30), admin, hits: Mutex::default() });
    let app = Router::new().fallback(handle).with_state(m.clone());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (m, base)
}

async fn owner(app: &TestApp) -> taskcraft::auth::User {
    sqlx::query_as("SELECT id, workspace_id, email, name, role, status FROM users WHERE role = 'owner'").fetch_one(&app.state.db).await.unwrap()
}

async fn count(app: &TestApp, sql: &str) -> i64 {
    sqlx::query_scalar(sqlx::AssertSqlSafe(sql.to_string())).fetch_one(&app.state.db).await.unwrap()
}

#[tokio::test]
async fn redmine_projects_come_over_with_journals_time_and_fields() {
    let (m, base) = mock(true).await;
    let app = spawn().await;
    let c = app.setup(false).await;
    let (_jane, jane_id) = app.user(&c, "Jane D.", "jane@example.com", "member").await;
    let owner = owner(&app).await;
    let conn: Connection = serde_json::from_value(json!({"source": "redmine", "baseUrl": base, "token": KEY, "projects": ["shop"]})).unwrap();

    let r = taskcraft::importer::jobs::import_api(&app.state, &owner, &conn, &Options::default()).await.unwrap();
    let text = r.to_string();
    let shop = r.projects.iter().find(|p| p.source == "shop").unwrap();
    let api = r.projects.iter().find(|p| p.source == "shop-api").unwrap_or_else(|| panic!("the subproject comes along: {text}"));
    assert!(!r.projects.iter().any(|p| p.source == "intranet"), "only what was asked: {text}");
    assert_eq!((shop.key.as_str(), api.key.as_str()), ("SHOP", "SHOPAPI"), "{text}");
    assert_eq!(api.parent.as_deref(), Some("shop"), "{text}");
    assert!(text.contains("a subproject of shop"), "{text}");
    assert_eq!(shop.counts["issues"].imported, 2, "{text}");
    assert_eq!(shop.counts["comments"].imported, 3, "{text}");
    assert_eq!(shop.counts["worklogs"].imported, 1, "{text}");
    assert_eq!(api.counts["worklogs"].imported, 1, "the subproject's entry once: {text}");
    assert_eq!(shop.counts["fields"], Count { imported: 4, existing: 1 }, "the template's Severity is used: {text}");
    assert_eq!(shop.counts["watchers"].imported, 2, "{text}");
    assert_eq!(shop.counts["links"].imported + api.counts["links"].imported, 2, "{text}");
    assert_eq!(shop.counts["confidential"].imported, 1, "a private issue: {text}");
    assert_eq!(r.people.get("matched"), Some(&1), "Jane by her e-mail address: {text}");
    assert_eq!(r.people.get("deactivated"), Some(&1), "Max's account is locked: {text}");
    for code in ["restricted-comments", "time-without-issue", "done-ratio", "confidential"] {
        assert!(r.warnings.iter().any(|w| w.code == code), "{code}: {text}");
    }
    assert!(m.hits.lock().unwrap()["/projects.json"] >= 3, "pages by offset");

    let one = get(&c, app.url("/issues/SHOP-1")).await;
    assert_eq!(one["title"], "Checkout breaks");
    assert_eq!(one["type"]["name"], "Bug");
    assert_eq!((one["status"]["name"].as_str(), one["status"]["category"].as_str(), one["resolution"].as_str()), (Some("Closed"), Some("done"), Some("Done")));
    assert_eq!(one["priority"], "high");
    assert_eq!(one["assignee"]["name"], "Max Muster");
    assert_eq!(one["component"]["name"], "Frontend");
    assert_eq!(one["version"]["name"], "1.0");
    assert_eq!(one["estimateMinutes"], 180);
    assert_eq!(one["spentMinutes"], 90);
    assert_eq!(one["dueDate"], m.day(240));
    assert_eq!(one["children"][0]["key"], "SHOPAPI-3", "parents across projects");
    // custom fields: made by name and kind, values as the fields keep them
    let fields: Vec<(String, String)> =
        sqlx::query_as("SELECT f.name, f.kind FROM custom_fields f JOIN projects p ON p.id = f.project_id WHERE p.key = 'SHOP' ORDER BY f.name")
            .fetch_all(&app.state.db)
            .await
            .unwrap();
    for f in [("Budget", "number"), ("Customer", "text"), ("Platforms", "multiselect"), ("Reviewer", "user"), ("Severity", "select")] {
        assert!(fields.contains(&(f.0.to_string(), f.1.to_string())), "{f:?} in {fields:?}");
    }
    let custom = one["custom"].to_string();
    assert!(custom.contains("12.5") && custom.contains("ACME"), "{custom}");
    let reviewer: String = sqlx::query_scalar(
        "SELECT i.custom ->> f.id::text FROM issues i JOIN custom_fields f ON f.project_id = i.project_id AND f.name = 'Reviewer' WHERE i.number = 1 AND f.project_id = i.project_id",
    )
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    let max: String = sqlx::query_scalar("SELECT id::text FROM users WHERE name = 'Max Muster'").fetch_one(&app.state.db).await.unwrap();
    assert_eq!(reviewer, max, "a user field points to the person");
    // Textile: heading, list, image from the attachment, code, emphasis, the reference to #2
    let desc = one["description"].to_string();
    assert!(desc.contains(r#""type":"heading""#) && desc.contains(r#""type":"orderedList""#), "{desc}");
    assert!(desc.contains(r#""type":"code""#) && desc.contains(r#""type":"bold""#), "{desc}");
    assert!(desc.contains(r##""id":"SHOP-2","label":"SHOP-2","mentionSuggestionChar":"#""##), "{desc}");
    assert!(desc.contains("https://example.com/spec"), "{desc}");
    let att = &one["attachments"][0];
    assert!(desc.contains(att["url"].as_str().unwrap()), "{desc}");
    let got = c.get(format!("{}{}", app.base, att["url"].as_str().unwrap())).send().await.unwrap();
    assert_eq!(&got.bytes().await.unwrap()[..], PNG, "downloaded from the server asked, not the address the server names");
    let comments = get(&c, app.url("/issues/SHOP-1/comments")).await;
    let comments = comments.as_array().unwrap();
    assert_eq!(comments.len(), 3);
    assert_eq!(comments[0]["author"]["id"], jane_id.as_str());
    assert!(comments[2]["body"].to_string().contains(att["url"].as_str().unwrap()), "attachment: links: {}", comments[2]["body"]);
    let logs = get(&c, app.url("/issues/SHOP-1/worklogs")).await;
    assert_eq!((logs[0]["minutes"].as_i64(), logs[0]["note"].as_str()), (Some(90), Some("Development: Debugging")));
    assert_eq!(logs[0]["date"], m.day(26));
    // closed but rejected: done as Won't do, in a status of its own; open but resolved stays open
    let two = get(&c, app.url("/issues/SHOP-2")).await;
    assert_eq!(
        (two["status"]["name"].as_str(), two["status"]["category"].as_str(), two["resolution"].as_str()),
        (Some("Rejected"), Some("done"), Some("Won't do"))
    );
    let three = get(&c, app.url("/issues/SHOPAPI-3")).await;
    assert_eq!((three["status"]["name"].as_str(), three["status"]["category"].as_str()), (Some("Resolved"), Some("in_progress")));
    assert_eq!(three["priority"], "urgent", "Immediate, the most urgent by its place");
    assert_eq!(three["version"]["name"], "1.0", "a shared version");
    let mut links: Vec<String> =
        one["links"].as_array().unwrap().iter().map(|l| format!("{} {}", l["label"].as_str().unwrap(), l["issue"]["key"].as_str().unwrap())).collect();
    links.sort();
    assert_eq!(links, vec!["blocks SHOP-2", "is blocked by SHOPAPI-3"], "precedes becomes blocks");

    // history from the journals: status, assignee, priority; the reports replay it
    let h = get(&c, app.url("/issues/SHOP-1/history")).await;
    let h = h.as_array().unwrap();
    let field = |f: &str| h.iter().filter(|e| e["field"] == f && e["kind"] == "field").cloned().collect::<Vec<_>>();
    let statuses: Vec<String> = field("status").iter().map(|e| e["new"]["name"].as_str().unwrap().to_string()).collect();
    assert_eq!(statuses, vec!["In progress", "Closed"], "by name, as the project calls it");
    assert_eq!(field("status")[0]["actor"]["id"], jane_id.as_str());
    assert_eq!(field("assignee")[0]["old"]["id"], jane_id.as_str());
    assert_eq!((field("priority")[0]["old"].as_str(), field("priority")[0]["new"].as_str()), (Some("medium"), Some("high")));
    let cycle = get(&c, app.url("/projects/SHOP/reports/cycle-time?days=90")).await;
    let item = cycle["issues"].as_array().unwrap().iter().find(|i| i["key"] == "SHOP-1").cloned().unwrap();
    assert_eq!((item["cycleDays"].as_f64(), item["leadDays"].as_f64()), (Some(3.2), Some(4.2)), "{item}");

    // again: nothing twice
    let again = taskcraft::importer::jobs::import_api(&app.state, &owner, &conn, &Options::default()).await.unwrap();
    assert_eq!(again.total("issues"), Count { imported: 0, existing: 3 }, "{again}");
    assert_eq!(count(&app, "SELECT count(*) FROM worklogs").await, 2);
    assert_eq!(count(&app, "SELECT count(*) FROM custom_fields WHERE name IN ('Customer', 'Budget', 'Reviewer', 'Platforms')").await, 4);
}

#[tokio::test]
async fn redmine_without_an_administrators_key() {
    let (_m, base) = mock(false).await;
    let app = spawn().await;
    let c = app.setup(false).await;
    let owner = owner(&app).await;
    let conn: Connection =
        serde_json::from_value(json!({"source": "redmine", "baseUrl": base, "token": KEY, "projects": ["shop"], "markup": "markdown"})).unwrap();
    let r =
        taskcraft::importer::jobs::import_api(&app.state, &owner, &conn, &Options { people: taskcraft::importer::People::Deactivated, ..Options::default() })
            .await
            .unwrap();
    let text = r.to_string();
    assert_eq!(r.total("issues").imported, 3, "{text}");
    assert_eq!(r.total("watchers").imported, 0, "{text}");
    // the kinds of custom fields come from their values
    let fields: Vec<(String, String)> =
        sqlx::query_as("SELECT f.name, f.kind FROM custom_fields f JOIN projects p ON p.id = f.project_id WHERE p.key = 'SHOP' ORDER BY f.name")
            .fetch_all(&app.state.db)
            .await
            .unwrap();
    assert!(fields.contains(&("Budget".into(), "number".into())), "{fields:?}");
    assert!(fields.contains(&("Platforms".into(), "multiselect".into())), "{fields:?}");
    assert!(fields.contains(&("Customer".into(), "text".into())), "{fields:?}");
    // Markdown as asked: the Textile heading stays text
    let one = get(&c, app.url("/issues/SHOP-1")).await;
    assert!(one["description"].to_string().contains("h2. Steps"), "{}", one["description"]);
    // Max isn't readable without an administrator's key: his name from the issues
    assert_eq!(one["assignee"]["name"], "Max Muster");
    // and the check says what's missing
    let m: taskcraft::importer::Model = {
        let http = conn.client(true, Default::default()).unwrap();
        conn.fetch(&http, &Default::default()).await.unwrap()
    };
    let codes: Vec<&str> = m.notes.iter().map(|w| w.code.as_str()).collect();
    assert!(codes.contains(&"not-admin") && codes.contains(&"fields-guessed"), "{codes:?}");
    assert_eq!(m.markup, taskcraft::importer::Format::Markdown);
}
