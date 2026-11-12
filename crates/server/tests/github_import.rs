//! Import from GitHub's REST API, against an in-process mock of it (GitHub Enterprise style:
//! the API under /api/v3, files on the same host): issues with comments, history from the
//! timeline, sub-issues, milestones, attachments, pagination and rate limits.

mod common;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, Duration, Utc};
use common::*;
use reqwest::Method;
use serde_json::{Value, json};
use taskcraft::importer::api::Connection;
use taskcraft::importer::{Count, Options};

const TOKEN: &str = "ghp_test-token-1234";
const PNG: &[u8] = b"\x89PNG-from-github";

struct Mock {
    base: String,
    /// when the shop's issues were created
    t0: DateTime<Utc>,
    /// requests by path
    hits: Mutex<HashMap<String, u32>>,
    /// requests without the token
    anonymous: Mutex<u32>,
}

impl Mock {
    fn at(&self, hours: i64) -> String {
        (self.t0 + Duration::hours(hours)).to_rfc3339()
    }

    fn user(&self, login: &str) -> Value {
        let id = match login {
            "alice" => 101,
            "bob" => 102,
            "carol" => 103,
            _ => 199,
        };
        json!({"login": login, "id": id, "type": "User"})
    }

    fn hit(&self, path: &str) -> u32 {
        let mut h = self.hits.lock().unwrap();
        let n = h.entry(path.to_string()).or_default();
        *n += 1;
        *n
    }

    fn issue(&self, number: i64, title: &str, by: &str, state: &str, extra: Value) -> Value {
        let mut v = json!({
            "id": 5000 + number, "number": number, "title": title, "user": self.user(by), "state": state, "labels": [], "assignees": [],
            "assignee": null, "milestone": null, "comments": 0, "created_at": self.at(number), "updated_at": self.at(number), "closed_at": null,
            "body": null, "state_reason": null, "html_url": format!("{}/octo/shop/issues/{number}", self.base),
        });
        for (k, x) in extra.as_object().unwrap() {
            v[k] = x.clone();
        }
        v
    }

    fn issues(&self) -> Vec<Value> {
        let b = &self.base;
        vec![
            self.issue(
                1,
                "Checkout breaks",
                "alice",
                "closed",
                json!({
                    "labels": [{"name": "bug"}, {"name": "good first issue"}],
                    "assignees": [self.user("bob"), self.user("carol")],
                    "milestone": {"number": 1, "title": "v1.0"},
                    "comments": 2,
                    "updated_at": self.at(101),
                    "closed_at": self.at(100),
                    "state_reason": "completed",
                    "sub_issues_summary": {"total": 1, "completed": 0},
                    "body": format!("Paying fails, @bob please look (see #2 and octo/shop#4, not #3).\n\n![screen.png]({b}/user-attachments/assets/aaaa-1111)\n\nThe [log]({b}/user-attachments/files/7/build.log)\n<!-- the template -->"),
                }),
            ),
            self.issue(2, "Add Apple Pay", "bob", "open", json!({"labels": [{"name": "enhancement"}], "type": {"name": "Feature"}})),
            json!({"id": 5003, "number": 3, "title": "A pull request", "user": self.user("bob"), "state": "open", "pull_request": {"url": "x"}, "created_at": self.at(3), "updated_at": self.at(3)}),
            self.issue(4, "Dark mode", "carol", "closed", json!({"closed_at": self.at(30), "updated_at": self.at(30), "state_reason": "not_planned"})),
            self.issue(5, "Card form layout", "alice", "open", json!({})),
        ]
    }

    fn timeline(&self, n: u32) -> Vec<Value> {
        let b = &self.base;
        let ev = |event: &str, by: &str, h: i64, extra: Value| {
            let mut v = json!({"event": event, "actor": self.user(by), "created_at": self.at(h)});
            for (k, x) in extra.as_object().unwrap() {
                v[k] = x.clone();
            }
            v
        };
        match n {
            1 => vec![
                ev("labeled", "alice", 1, json!({"label": {"name": "bug"}})),
                ev("labeled", "alice", 1, json!({"label": {"name": "good first issue"}})),
                ev("assigned", "alice", 2, json!({"assignee": self.user("bob")})),
                ev("commented", "bob", 3, json!({"id": 900, "user": self.user("bob"), "body": "Looking, this blocks #4.", "updated_at": self.at(3)})),
                ev("assigned", "bob", 4, json!({"assignee": self.user("carol")})),
                ev("milestoned", "alice", 5, json!({"milestone": {"title": "v1.0"}})),
                ev("renamed", "alice", 6, json!({"rename": {"from": "Checkout broken", "to": "Checkout breaks"}})),
                ev("closed", "bob", 50, json!({"state_reason": "completed"})),
                ev("reopened", "alice", 60, json!({})),
                ev(
                    "commented",
                    "alice",
                    61,
                    json!({"id": 901, "user": self.user("alice"), "body": format!("Still broken: <img width=\"200\" alt=\"again.png\" src=\"{b}/user-attachments/assets/aaaa-1111\">"), "updated_at": self.at(70)}),
                ),
                ev("closed", "bob", 100, json!({"state_reason": "completed"})),
            ],
            4 => vec![ev("closed", "carol", 30, json!({"state_reason": "not_planned"}))],
            _ => vec![],
        }
    }
}

fn json_page(v: Value, link: Option<String>) -> Response {
    let mut h = HeaderMap::new();
    if let Some(l) = link {
        h.insert(header::LINK, l.parse().unwrap());
    }
    (h, axum::Json(v)).into_response()
}

async fn handle(State(m): State<Arc<Mock>>, req: Request) -> Response {
    let path = req.uri().path().to_string();
    let query = req.uri().query().unwrap_or("").to_string();
    let authed = req.headers().get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()) == Some(&format!("Bearer {TOKEN}"));
    if !authed {
        *m.anonymous.lock().unwrap() += 1;
    }
    let n = m.hit(&path);
    // files: an asset redirects to where it's stored, like github.com does
    match path.as_str() {
        "/user-attachments/assets/aaaa-1111" => return (StatusCode::FOUND, [(header::LOCATION, "/storage/screen-aaaa.png?sig=abc")]).into_response(),
        "/storage/screen-aaaa.png" => return ([(header::CONTENT_TYPE, "image/png")], PNG.to_vec()).into_response(),
        "/user-attachments/files/7/build.log" => return ([(header::CONTENT_TYPE, "text/plain")], b"build ok\n".to_vec()).into_response(),
        _ => {}
    }
    let Some(api) = path.strip_prefix("/api/v3/") else { return StatusCode::NOT_FOUND.into_response() };
    if !authed {
        return (StatusCode::UNAUTHORIZED, axum::Json(json!({"message": "Bad credentials"}))).into_response();
    }
    let b = &m.base;
    match api {
        "repos/octo/shop" => json_page(
            json!({"id": 9001, "name": "shop", "full_name": "octo/shop", "description": "The *shop*", "private": true, "has_issues": true, "open_issues_count": 3}),
            None,
        ),
        "repos/octo/missing" => (StatusCode::NOT_FOUND, axum::Json(json!({"message": "Not Found"}))).into_response(),
        "repos/octo/shop/milestones" => json_page(
            json!([{"number": 1, "title": "v1.0", "description": "The **first**", "state": "closed", "due_on": m.at(120), "closed_at": m.at(110)}]),
            None,
        ),
        "repos/octo/shop/issues" => {
            // two pages of two
            let all = m.issues();
            if query.contains("page=2") {
                json_page(json!(all[3..]), None)
            } else {
                assert!(query.contains("state=all"), "{query}");
                json_page(json!(all[..3]), Some(format!("<{b}/api/v3/repos/octo/shop/issues?state=all&per_page=100&page=2>; rel=\"next\"")))
            }
        }
        "repos/octo/shop/issues/1/timeline" if n == 1 => {
            // the hourly limit runs out once: wait until it's reset
            let reset = (Utc::now() + Duration::seconds(1)).timestamp();
            (
                StatusCode::FORBIDDEN,
                [("x-ratelimit-remaining", "0".to_string()), ("x-ratelimit-reset", reset.to_string())],
                axum::Json(json!({"message": "API rate limit exceeded"})),
            )
                .into_response()
        }
        "repos/octo/shop/issues/1/timeline" => json_page(json!(m.timeline(1)), None),
        "repos/octo/shop/issues/4/timeline" => json_page(json!(m.timeline(4)), None),
        "repos/octo/shop/issues/2/timeline" | "repos/octo/shop/issues/5/timeline" => panic!("issues nobody touched need no timeline: {api}"),
        "repos/octo/shop/issues/1/sub_issues" => json_page(json!([{"id": 5005, "number": 5}]), None),
        "users/alice" => json_page(json!({"login": "alice", "id": 101, "name": "Alice Admin", "email": "ALICE@example.com"}), None),
        "users/bob" => json_page(json!({"login": "bob", "id": 102, "name": "Bob Builder", "email": null}), None),
        // a secondary rate limit once
        "users/carol" if n == 1 => (StatusCode::TOO_MANY_REQUESTS, [("retry-after", "1")], "slow down").into_response(),
        "users/carol" => json_page(json!({"login": "carol", "id": 103, "name": null, "email": null}), None),
        "orgs/octo/repos" => json_page(
            json!([{"full_name": "octo/shop", "name": "shop", "description": "The shop", "open_issues_count": 3, "private": true, "has_issues": true},
                   {"full_name": "octo/docs", "name": "docs", "has_issues": false}]),
            None,
        ),
        _ => (StatusCode::NOT_FOUND, axum::Json(json!({"message": format!("no mock for {api}")}))).into_response(),
    }
}

async fn mock() -> Arc<Mock> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let m = Arc::new(Mock { base, t0: Utc::now() - Duration::days(30), hits: Mutex::default(), anonymous: Mutex::default() });
    let app = Router::new().fallback(handle).with_state(m.clone());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    m
}

fn conn(m: &Mock, repos: &[&str]) -> Connection {
    serde_json::from_value(json!({"source": "github", "baseUrl": format!("{}/api/v3", m.base), "token": TOKEN, "projects": repos})).unwrap()
}

async fn owner(app: &TestApp) -> taskcraft::auth::User {
    sqlx::query_as("SELECT id, workspace_id, email, name, role, status FROM users WHERE role = 'owner'").fetch_one(&app.state.db).await.unwrap()
}

async fn count(app: &TestApp, sql: &str) -> i64 {
    sqlx::query_scalar(sqlx::AssertSqlSafe(sql.to_string())).fetch_one(&app.state.db).await.unwrap()
}

#[tokio::test]
async fn github_issues_come_over_with_history_and_files() {
    let m = mock().await;
    let app = spawn().await;
    let c = app.setup(false).await;
    let (_alice, alice_id) = app.user(&c, "Alice A.", "alice@example.com", "member").await;
    let owner = owner(&app).await;
    let connection = conn(&m, &["octo/shop"]);

    // a dry run changes nothing
    let dry = taskcraft::importer::jobs::import_api(&app.state, &owner, &connection, &Options { dry_run: true, ..Options::default() }).await.unwrap();
    assert_eq!(dry.total("issues").imported, 4, "{dry}");
    assert_eq!(count(&app, "SELECT count(*) FROM projects").await, 0);
    assert_eq!(count(&app, "SELECT count(*) FROM attachments").await, 0);

    let r = taskcraft::importer::jobs::import_api(&app.state, &owner, &connection, &Options::default()).await.unwrap();
    let text = r.to_string();
    let shop = &r.projects[0];
    assert_eq!((shop.source.as_str(), shop.key.as_str(), shop.created), ("octo/shop", "SHOP", true), "{text}");
    assert_eq!(shop.counts["issues"].imported, 4, "pull requests are left out: {text}");
    assert_eq!(shop.counts["comments"].imported, 2, "{text}");
    assert_eq!(shop.counts["attachments"].imported, 2, "the same file once per issue: {text}");
    assert_eq!(shop.counts["parents"].imported, 1, "{text}");
    assert_eq!(shop.counts["versions"].imported, 1, "{text}");
    assert_eq!(r.people.get("matched"), Some(&1), "Alice by her e-mail address: {text}");
    assert_eq!(r.people.get("invited"), Some(&2), "{text}");
    assert_eq!(*m.anonymous.lock().unwrap(), 0, "every request carried the token");
    // the dry run waited for the rate limit and asked again, the import asked once
    assert_eq!(m.hits.lock().unwrap()["/api/v3/repos/octo/shop/issues/1/timeline"], 3);

    let pay = get(&c, app.url("/issues/SHOP-1")).await;
    assert_eq!(pay["title"], "Checkout breaks");
    assert_eq!(pay["type"]["name"], "Bug", "by its label");
    assert_eq!(pay["status"]["category"], "done");
    assert_eq!(pay["resolution"], "Done");
    assert_eq!(pay["reporter"]["id"], alice_id.as_str());
    assert_eq!(pay["assignee"]["name"], "Bob Builder", "the first assignee");
    let watchers: Vec<&str> = pay["watchers"].as_array().unwrap().iter().map(|w| w["name"].as_str().unwrap()).collect();
    assert_eq!(watchers, vec!["carol"], "the other assignees watch");
    assert_eq!(pay["labels"], json!(["bug", "good-first-issue"]));
    assert_eq!(pay["version"]["name"], "v1.0");
    assert_eq!(pay["children"][0]["key"], "SHOP-5", "sub-issues become children");
    let resolved: DateTime<Utc> = pay["resolvedAt"].as_str().unwrap().parse().unwrap();
    assert!((resolved - (m.t0 + Duration::hours(100))).num_seconds().abs() < 2, "{resolved}");
    // the description: Markdown with a mention, issue references and the stored files
    let desc = pay["description"].to_string();
    assert!(desc.contains(r#""mentionSuggestionChar":"@""#), "{desc}");
    assert!(desc.contains(r##""id":"SHOP-2","label":"SHOP-2","mentionSuggestionChar":"#""##), "{desc}");
    assert!(desc.contains(r##""id":"SHOP-4""##), "owner/repo#4 too: {desc}");
    assert!(desc.contains("#3"), "a pull request stays text: {desc}");
    assert!(!desc.contains("the template"), "{desc}");
    assert!(!desc.contains("user-attachments"), "file links point to the stored files: {desc}");
    let atts = pay["attachments"].as_array().unwrap();
    let png = atts.iter().find(|a| a["name"] == "screen.png").unwrap_or_else(|| panic!("{atts:?}"));
    assert_eq!(png["mime"], "image/png");
    assert!(desc.contains(png["url"].as_str().unwrap()), "{desc}");
    let got = c.get(format!("{}{}", app.base, png["url"].as_str().unwrap())).send().await.unwrap();
    assert_eq!(&got.bytes().await.unwrap()[..], PNG);
    assert!(atts.iter().any(|a| a["name"] == "build.log"));
    // comments with their authors and dates; the second one points at the stored file too
    let comments = get(&c, app.url("/issues/SHOP-1/comments")).await;
    let comments = comments.as_array().unwrap();
    assert_eq!(comments[0]["author"]["name"], "Bob Builder");
    assert!(comments[0]["body"].to_string().contains(r##""id":"SHOP-4""##));
    assert_eq!(comments[1]["author"]["id"], alice_id.as_str());
    assert_eq!(comments[1]["edited"], true);
    assert!(comments[1]["body"].to_string().contains(png["url"].as_str().unwrap()), "{}", comments[1]["body"]);
    // not planned: done as Won't do; a type of the organization; a release from the milestone
    let dark = get(&c, app.url("/issues/SHOP-4")).await;
    assert_eq!((dark["status"]["category"].as_str(), dark["resolution"].as_str()), (Some("done"), Some("Won't do")));
    let apple = get(&c, app.url("/issues/SHOP-2")).await;
    assert_eq!(apple["type"]["name"], "Feature");
    assert_eq!(apple["status"]["category"], "todo");
    assert_eq!(apple["labels"], json!(["enhancement"]));
    let proj = get(&c, app.url("/projects/SHOP")).await;
    assert_eq!(proj["name"], "shop");
    assert_eq!(proj["versions"][0]["status"], "released");
    assert_eq!(count(&app, "SELECT count(*) FROM issues WHERE number = 3").await, 0, "the pull request's number stays free");

    // history: closed, reopened, closed again, assignee, labels, title, release
    let h = get(&c, app.url("/issues/SHOP-1/history")).await;
    let h = h.as_array().unwrap();
    assert_eq!(h[0]["kind"], "created");
    assert_eq!(h[0]["new"]["status"]["category"], "todo");
    assert_eq!(h[0]["new"]["assignee"], Value::Null, "nobody was assigned at first");
    let field = |f: &str| h.iter().filter(|e| e["field"] == f && e["kind"] == "field").cloned().collect::<Vec<_>>();
    let statuses: Vec<String> = field("status").iter().map(|e| e["new"]["category"].as_str().unwrap().to_string()).collect();
    assert_eq!(statuses, vec!["done", "todo", "done"]);
    assert_eq!(field("status")[1]["actor"]["id"], alice_id.as_str());
    assert_eq!(field("assignee").len(), 1, "adding a second assignee doesn't change the first");
    assert_eq!(field("assignee")[0]["new"]["name"], "Bob Builder");
    assert_eq!(field("labels").last().unwrap()["new"], json!(["bug", "good-first-issue"]));
    assert_eq!(field("title")[0]["old"], "Checkout broken");
    assert_eq!(field("version")[0]["new"]["name"], "v1.0");
    // reports replay it: resolved on the last close, cycle time from the start
    let cr = get(&c, app.url("/projects/SHOP/reports/created-resolved?days=60")).await;
    let days = cr["days"].as_array().unwrap();
    let on = |d: DateTime<Utc>| days.iter().find(|x| x["date"] == d.date_naive().to_string()).cloned().unwrap();
    assert!(on(m.t0 + Duration::hours(100))["resolved"].as_i64().unwrap() >= 1);
    assert!(on(m.t0 + Duration::hours(1))["created"].as_i64().unwrap() >= 1);
    let cycle = get(&c, app.url("/projects/SHOP/reports/cycle-time?days=90")).await;
    let item = cycle["issues"].as_array().unwrap().iter().find(|i| i["key"] == "SHOP-1").cloned().unwrap();
    assert_eq!(item["leadDays"], 4.1, "{item}");

    // again: nothing twice
    let again = taskcraft::importer::jobs::import_api(&app.state, &owner, &connection, &Options::default()).await.unwrap();
    assert_eq!(again.total("issues"), Count { imported: 0, existing: 4 }, "{again}");
    assert_eq!(count(&app, "SELECT count(*) FROM comments").await, 2);
    assert_eq!(count(&app, "SELECT count(*) FROM users").await, 4);
    assert_eq!(count(&app, "SELECT count(*) FROM mail_outbox").await, 0, "nobody is e-mailed");

    // a repository that isn't there (or can't be read)
    let e = taskcraft::importer::jobs::import_api(&app.state, &owner, &conn(&m, &["octo/missing"]), &Options::default()).await.unwrap_err();
    assert!(e.to_string().contains("no such repository"), "{e}");
}

/// Polls an import until it's no longer busy.
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

#[tokio::test]
async fn github_import_from_the_browser() {
    let m = mock().await;
    let app = spawn().await;
    let c = app.setup(false).await;
    let (member, _) = app.user(&c, "Max Member", "max@example.com", "member").await;
    let base = format!("{}/api/v3", m.base);
    let body = json!({"source": "github", "baseUrl": base, "token": TOKEN, "projects": ["octo/shop"]});
    let (st, _) = send(&member, Method::POST, app.url("/admin/backup-imports/connect"), body.clone()).await;
    assert_eq!(st, 403, "administrators only");
    // repositories to choose from
    let list = post(&c, app.url("/admin/backup-imports/browse"), json!({"source": "github", "baseUrl": base, "token": TOKEN, "owner": "octo"})).await;
    let keys: Vec<&str> = list["projects"].as_array().unwrap().iter().map(|p| p["key"].as_str().unwrap()).collect();
    assert_eq!(keys, vec!["octo/shop"], "repositories without issues aren't offered");
    let (st, v) = send(&c, Method::POST, app.url("/admin/backup-imports/browse"), json!({"source": "github", "baseUrl": base, "token": "wrong"})).await;
    assert_eq!(st, 400);
    assert!(v["message"].as_str().unwrap().contains("401"), "{v}");

    // connect: read and checked in the background
    let job = post(&c, app.url("/admin/backup-imports/connect"), body).await;
    let id = job["id"].as_str().unwrap().to_string();
    assert_eq!(job["source"], "github");
    let job = settle(&c, &app, &id).await;
    assert_eq!(job["status"], "checked", "{job}");
    let s = &job["summary"];
    assert_eq!(s["source"], "github");
    let p = &s["projects"][0];
    assert_eq!((p["key"].as_str(), p["suggestedKey"].as_str(), p["validKey"].as_bool()), (Some("octo/shop"), Some("SHOP"), Some(false)));
    assert_eq!(p["issues"], 4);
    assert_eq!(p["comments"], 2);
    assert_eq!(p["attachments"], 2, "{p}");
    assert_eq!(p["history"].as_u64().unwrap(), 9, "{p}");
    assert_eq!(s["people"]["total"], 3);
    assert!(s["warnings"].as_array().unwrap().iter().any(|w| w["code"] == "pull-requests"), "{s}");
    assert_eq!(job["connection"]["token"], true);
    // the token is kept nowhere
    let row: String = sqlx::query_scalar("SELECT row_to_json(b)::text FROM backup_imports b").fetch_one(&app.state.db).await.unwrap();
    assert!(!row.contains(TOKEN), "{row}");
    let audit: String =
        sqlx::query_scalar("SELECT COALESCE(string_agg(row_to_json(a)::text, ''), '') FROM audit_log a").fetch_one(&app.state.db).await.unwrap();
    assert!(!audit.contains(TOKEN));

    // the import needs the token again for the files (a test doesn't)
    let run = app.url(&format!("/admin/backup-imports/{id}/run"));
    let (st, v) = send(&c, Method::POST, run.clone(), json!({"projects": {"octo/shop": {"action": "new", "key": "WEB"}}})).await;
    assert_eq!(st, 400, "{v}");
    let (st, _) = send(&c, Method::POST, run.clone(), json!({"projects": {"octo/shop": {"action": "new", "key": "WEB"}}, "dryRun": true})).await;
    assert_eq!(st, 202);
    let job = settle(&c, &app, &id).await;
    assert_eq!(job["report"]["dryRun"], true, "{job}");
    assert_eq!(job["fileKept"], true);
    let (st, _) =
        send(&c, Method::POST, run, json!({"projects": {"octo/shop": {"action": "new", "key": "WEB"}}, "people": "deactivated", "token": TOKEN})).await;
    assert_eq!(st, 202);
    let job = settle(&c, &app, &id).await;
    assert_eq!(job["status"], "done", "{job}");
    let web = &job["report"]["projects"][0];
    assert_eq!((web["key"].as_str(), web["counts"]["issues"]["imported"].as_i64()), (Some("WEB"), Some(4)));
    assert_eq!(job["fileKept"], false, "what was read is removed once it's in");
    assert_eq!(get(&c, app.url("/issues/WEB-1")).await["attachments"].as_array().unwrap().len(), 2);
    assert_eq!(count(&app, "SELECT count(*) FROM users WHERE status = 'deactivated'").await, 3);
}

#[tokio::test]
async fn github_import_refuses_private_addresses() {
    let m = mock().await;
    let app = spawn_with(|c| c.import_allow_private = false).await;
    let c = app.setup(false).await;
    let (st, v) = send(
        &c,
        Method::POST,
        app.url("/admin/backup-imports/browse"),
        json!({"source": "github", "baseUrl": format!("{}/api/v3", m.base), "token": TOKEN, "owner": "octo"}),
    )
    .await;
    assert_eq!(st, 400);
    assert!(v["message"].as_str().unwrap().contains("TASKCRAFT_IMPORT_ALLOW_PRIVATE"), "{v}");
    let job =
        post(&c, app.url("/admin/backup-imports/connect"), json!({"source": "github", "baseUrl": m.base, "token": TOKEN, "projects": ["octo/shop"]})).await;
    let job = settle(&c, &app, job["id"].as_str().unwrap()).await;
    assert_eq!(job["status"], "failed");
    assert!(job["error"].as_str().unwrap().contains("private address"), "{job}");
    assert!(!job["error"].as_str().unwrap().contains(TOKEN));
    assert!(m.hits.lock().unwrap().is_empty(), "nothing reached the private server");
}
