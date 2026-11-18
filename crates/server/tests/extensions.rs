//! Scripted automation and extensions: a rule's script computes fields of its issue inside a
//! sandbox (no files, modules or endless loops); an installed extension gets exactly the
//! capabilities its manifest asked for — issues as the person it runs as, HTTP to the hosts
//! named, state, secrets — through custom actions, event handlers, a scheduled job and routes;
//! runs are logged and one switch turns it off.

mod common;

use std::sync::{Arc, Mutex};

use axum::Json;
use axum::Router;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::routing::post as route_post;
use common::*;
use reqwest::Method;
use serde_json::{Value, json};

async fn rule(app: &TestApp, c: &reqwest::Client, name: &str, trigger: Value, actions: Value) -> (u16, Value) {
    send(c, Method::POST, app.url("/projects/WEB/automation"), json!({"name": name, "trigger": trigger, "condition": "", "actions": actions})).await
}

async fn rules(app: &TestApp, c: &reqwest::Client) -> Vec<Value> {
    get(c, app.url("/projects/WEB/automation")).await.as_array().unwrap().clone()
}

/// Waits until `f` holds (event handlers run in the background).
async fn eventually<F, Fut>(what: &str, f: F)
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    for _ in 0..80 {
        if f().await {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("never happened: {what}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_rules_script_computes_fields_in_a_sandbox() {
    let app = spawn().await;
    let owner = app.setup(false).await;
    post(&owner, app.url("/projects"), json!({"key": "WEB", "name": "Web", "template": "scrum"})).await;
    let (mia, _) = app.user(&owner, "Mia Member", "mia@example.com", "member").await;
    post(&owner, app.url("/projects/WEB/fields"), json!({"name": "Customer tier", "kind": "select", "options": [{"label": "Gold"}, {"label": "Silver"}]}))
        .await;
    post(&owner, app.url("/projects/WEB/fields"), json!({"name": "Risk", "kind": "number"})).await;

    // scripts are checked when the rule is saved; members don't write rules
    let (s, v) = rule(&app, &owner, "Broken", json!({"event": "created"}), json!([{"action": "script", "script": "let x = ;"}])).await;
    assert!(s == 400 && v["message"].as_str().unwrap().contains("doesn't parse"), "{v}");
    assert_eq!(rule(&app, &owner, "Empty", json!({"event": "created"}), json!([{"action": "script", "script": "  "}])).await.0, 400);
    assert_eq!(rule(&app, &mia, "Nope", json!({"event": "created"}), json!([{"action": "script", "script": "1"}])).await.0, 403);

    // the triage script: priority, a computed number, a select by its label, labels, a comment
    let script = r#"
        let words = issue.title.to_lower();
        let risk = issue.title.len() % 7 + if words.contains("payment") { 10 } else { 0 };
        set_field("Risk", risk);
        if words.contains("payment") {
            set("priority", "urgent");
            set_field("Customer tier", "gold");
            add_label("Money");
            comment(`Triage: risk ${risk}, reported by ${issue.reporter.name}.`);
        }
        if issue.labels.contains("draft") { remove_label("draft"); }
        set("dueDate", "2030-01-31");
        log("ignored in rules");
    "#;
    assert_eq!(rule(&app, &owner, "Triage", json!({"event": "created"}), json!([{"action": "script", "script": script}])).await.0, 201);
    post(&mia, app.url("/issues"), json!({"projectKey": "WEB", "title": "Payment fails with a 500", "labels": ["draft"]})).await;
    post(&mia, app.url("/issues"), json!({"projectKey": "WEB", "title": "Typo"})).await;
    let i = get(&owner, app.url("/issues/WEB-1")).await;
    assert_eq!((i["priority"].as_str(), i["dueDate"].as_str(), &i["labels"]), (Some("urgent"), Some("2030-01-31"), &json!(["money"])), "{i}");
    let p = get(&owner, app.url("/projects/WEB")).await;
    let tier = p["fields"].as_array().unwrap().iter().find(|f| f["name"] == "Customer tier").unwrap().clone();
    let gold = tier["options"].as_array().unwrap().iter().find(|o| o["label"] == "Gold").unwrap()["id"].clone();
    assert_eq!(i["custom"][tier["id"].as_str().unwrap()], gold);
    assert!(i["custom"].to_string().contains(&format!("{}", "Payment fails with a 500".len() % 7 + 10)), "{i}");
    let comments = get(&owner, app.url("/issues/WEB-1/comments")).await;
    assert!(comments[0].to_string().contains("reported by Mia Member") && comments[0]["author"].is_null(), "{comments}");
    let typo = get(&owner, app.url("/issues/WEB-2")).await;
    assert_eq!((typo["priority"].as_str(), typo["dueDate"].as_str()), (Some("medium"), Some("2030-01-31")));
    let history = get(&owner, app.url("/issues/WEB-1/history")).await.to_string();
    assert!(history.contains("urgent"), "the changes are Automation's, in the history");
    assert!(rules(&app, &owner).await.iter().any(|r| r["name"] == "Triage" && r["runs"] == 2 && r["lastError"].is_null()));

    // the sandbox: endless loops stop, nothing outside the issue is reachable, mistakes show on the rule
    let hostile = [
        ("Loop", "loop { }", "too much work"),
        ("Bomb", "let s = \"x\"; loop { s += s; }", ""),
        ("Import", "import \"/etc/passwd\" as p;", ""),
        ("Eval", "eval(\"1 + 1\")", ""),
        ("Net", "http_get(\"http://127.0.0.1:1/\")", "installed extensions"),
        ("Secrets", "secret(\"X\")", "installed extensions"),
        ("Search", "search(\"project = WEB\")", "installed extensions"),
        ("Field", "set(\"reporter\", \"me\")", "scripts set title"),
        ("Unknown", "set_field(\"Nope\", 1)", "no field Nope"),
        ("Typo", "issue.titel.len()", ""),
    ];
    for (name, script, _) in hostile {
        let (s, v) =
            rule(&app, &owner, name, json!({"event": "commented"}), json!([{"action": "script", "script": script}, {"action": "addLabel", "label": name}]))
                .await;
        // eval and import don't even parse
        assert!(s == 201 || (s == 400 && matches!(name, "Eval" | "Import")), "{name}: {s} {v}");
    }
    let started = std::time::Instant::now();
    post(&mia, app.url("/issues/WEB-2/comments"), json!({"markdown": "go"})).await;
    assert!(started.elapsed() < std::time::Duration::from_secs(20), "runaway scripts are stopped");
    let after = rules(&app, &owner).await;
    for (name, _, why) in hostile.iter().filter(|h| !matches!(h.0, "Eval" | "Import")) {
        let r = after.iter().find(|r| r["name"] == *name).unwrap();
        let e = r["lastError"].as_str().unwrap_or_else(|| panic!("{name} should have failed: {r}"));
        assert!(e.starts_with("WEB-2: ") && e.contains(why), "{name}: {e}");
        assert_eq!(r["runs"], 0, "{name}");
    }
    assert_eq!(get(&owner, app.url("/issues/WEB-2")).await["labels"], json!([]), "a rule that fails changes nothing");
}

/// The ERP an extension talks to: what it was sent.
#[derive(Default)]
struct Erp {
    orders: Mutex<Vec<(String, Value)>>,
}

async fn erp_order(State(erp): State<Arc<Erp>>, h: HeaderMap, Json(b): Json<Value>) -> Json<Value> {
    let auth = h.get("authorization").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
    let n = {
        let mut o = erp.orders.lock().unwrap();
        o.push((auth, b));
        o.len()
    };
    Json(json!({"orderId": format!("ERP-{n}")}))
}

#[tokio::test(flavor = "multi_thread")]
async fn extensions_get_what_their_manifest_asks_for() {
    let erp = Arc::new(Erp::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let erp_url = format!("http://localhost:{}", listener.local_addr().unwrap().port());
    let erp_app = Router::new().route("/orders", route_post(erp_order)).with_state(erp.clone());
    tokio::spawn(async move { axum::serve(listener, erp_app).await.unwrap() });

    let app = spawn().await;
    let owner = app.setup(false).await;
    post(&owner, app.url("/projects"), json!({"key": "WEB", "name": "Web", "template": "scrum"})).await;
    post(&owner, app.url("/projects"), json!({"key": "SEC", "name": "Secret", "template": "kanban"})).await;
    patch(&owner, app.url("/projects/SEC"), json!({"defaultRole": "none"})).await;
    let (mia, mia_id) = app.user(&owner, "Mia Member", "mia@example.com", "member").await;
    post(&owner, app.url("/issues"), json!({"projectKey": "SEC", "title": "Hidden from Mia"})).await;

    let script = format!(
        r#"
        fn erp() {{ "{erp_url}" }}
        fn action_push(issue, params) {{
            let r = http_post(erp() + "/orders", #{{ key: issue.key, title: issue.title, kind: params.kind }}, #{{ authorization: "Bearer " + secret("ERP_TOKEN") }});
            if !r.ok {{ throw `the ERP answered ${{r.status}}`; }}
            state_set("last", r.body.orderId);
            state_set("count", (state_get("count") ?? 0) + 1);
            comment(`Order ${{r.body.orderId}}`);
            add_label("in-erp");
        }}
        fn action_elsewhere(issue, params) {{ http_get("http://127.0.0.1:9/nope") }}
        fn on_event(event, issue) {{
            log(`${{event}} ${{issue.key}}`);
            if event == "created" && issue.title.contains("[auto]") {{ set("priority", "low"); }}
        }}
        fn scheduled() {{
            let open = search("category != done order by key");
            state_set("open", open.map(|i| i.key));
            for i in open {{ if i.title.contains("stale") {{ add_comment(i.key, "Still needed?"); }} }}
            open.len()
        }}
        fn route(request) {{
            if request.path == "status" {{
                return #{{ last: state_get("last"), count: state_get("count") ?? 0, open: state_get("open"), who: request.user.name, q: request.query }};
            }}
            if request.path == "peek" {{
                let hidden = get_issue(request.body.key);
                if hidden == () {{ return #{{ status: 404, body: #{{ error: "not found" }} }}; }}
                return #{{ title: hidden.title }};
            }}
            update_issue(request.body.key, #{{ priority: "high" }}).priority
        }}
    "#
    );
    let manifest = json!({
        "name": "erp-sync", "title": "ERP sync", "version": "1.2.0", "description": "Orders for issues", "apiVersion": 1,
        "capabilities": {"issues": "write", "http": ["localhost"], "state": true, "secrets": ["ERP_TOKEN"]},
        "actions": [{"name": "push", "label": "Create an order in the ERP"}, {"name": "elsewhere", "label": "Call another host"}],
        "events": ["created", "commented"], "schedule": {"every": "day", "at": "06:00"},
        "routes": [{"method": "GET", "path": "status"}, {"method": "POST", "path": "peek"}, {"method": "POST", "path": "bump"}],
    });

    // installing is the admins'; the manifest and the script are checked
    assert_eq!(send(&mia, Method::POST, app.url("/admin/extensions"), json!({"manifest": manifest, "script": script})).await.0, 403);
    let (s, v) = send(&owner, Method::POST, app.url("/admin/extensions"), json!({"manifest": {"name": "erp-sync", "apiVersion": 7}, "script": script})).await;
    assert!(s == 400 && v["message"].as_str().unwrap().contains("extension API 1"), "{v}");
    let (s, v) = send(&owner, Method::POST, app.url("/admin/extensions"), json!({"manifest": manifest, "script": "fn route(r) { 1 }"})).await;
    assert!(s == 400 && v["message"].as_str().unwrap().contains("action_push"), "{v}");
    // it runs as Mia: it sees and changes what she may
    let (s, x) = send(&owner, Method::POST, app.url("/admin/extensions"), json!({"manifest": manifest, "script": script, "runAs": mia_id})).await;
    assert_eq!(s, 201, "{x}");
    let id = x["id"].as_str().unwrap().to_string();
    assert_eq!((x["enabled"].as_bool(), x["runAs"]["name"].as_str(), x["version"].as_str()), (Some(true), Some("Mia Member"), Some("1.2.0")), "{x}");
    assert_eq!(x["secrets"], json!([{"name": "ERP_TOKEN", "set": false}]));
    assert!(!x["nextRunAt"].is_null());
    assert_eq!(send(&owner, Method::POST, app.url("/admin/extensions"), json!({"manifest": manifest, "script": script})).await.0, 409);
    let ext = app.url(&format!("/admin/extensions/{id}"));
    assert_eq!(send(&owner, Method::PATCH, ext.clone(), json!({"secrets": {"OTHER": "x"}})).await.0, 400, "only the secrets it asks for");

    // its actions are offered to rules
    let offered = get(&mia, app.url("/extensions/actions")).await;
    assert_eq!(offered[0], json!({"extension": "erp-sync", "extensionTitle": "ERP sync", "name": "push", "label": "Create an order in the ERP"}));
    let push = json!([{"action": "extension", "extension": "erp-sync", "name": "push", "params": {"kind": "bug"}}]);
    assert_eq!(rule(&app, &owner, "To the ERP", json!({"event": "status", "toCategory": "done"}), push).await.0, 201);
    assert_eq!(
        rule(&app, &owner, "Elsewhere", json!({"event": "assigned"}), json!([{"action": "extension", "extension": "erp-sync", "name": "elsewhere"}])).await.0,
        201
    );
    assert_eq!(rule(&app, &owner, "Missing", json!({"event": "assigned"}), json!([{"action": "extension", "extension": "nope", "name": "x"}])).await.0, 201);
    assert_eq!(rule(&app, &owner, "Unnamed", json!({"event": "assigned"}), json!([{"action": "extension"}])).await.0, 400);

    post(&mia, app.url("/issues"), json!({"projectKey": "WEB", "title": "Invoice layout [auto]"})).await;
    post(&mia, app.url("/issues"), json!({"projectKey": "WEB", "title": "A stale thing"})).await;
    let p = get(&owner, app.url("/projects/WEB")).await;
    let done = p["statuses"].as_array().unwrap().iter().find(|s| s["category"] == "done").unwrap()["id"].clone();

    // the secret isn't set yet: the action fails, and the rule and the extension say why
    patch(&mia, app.url("/issues/WEB-1"), json!({"statusId": done})).await;
    let failed = rules(&app, &owner).await;
    assert!(failed.iter().find(|r| r["name"] == "To the ERP").unwrap()["lastError"].as_str().unwrap().contains("ERP_TOKEN isn't set"), "{failed:?}");
    assert!(erp.orders.lock().unwrap().is_empty());
    let (s, v) = send(&owner, Method::PATCH, ext.clone(), json!({"secrets": {"ERP_TOKEN": "s3cret"}})).await;
    assert_eq!((s, &v["secrets"]), (200, &json!([{"name": "ERP_TOKEN", "set": true}])), "{v}");
    assert!(!get(&owner, app.url("/admin/extensions")).await.to_string().contains("s3cret"), "secrets are never shown");

    // now it calls the ERP with the secret, keeps state and changes the issue
    let todo = p["statuses"].as_array().unwrap().iter().find(|s| s["category"] == "todo").unwrap()["id"].clone();
    patch(&mia, app.url("/issues/WEB-1"), json!({"statusId": todo})).await;
    patch(&mia, app.url("/issues/WEB-1"), json!({"statusId": done})).await;
    let orders = erp.orders.lock().unwrap().clone();
    assert_eq!(orders, vec![("Bearer s3cret".to_string(), json!({"key": "WEB-1", "title": "Invoice layout [auto]", "kind": "bug"}))]);
    let i = get(&owner, app.url("/issues/WEB-1")).await;
    assert_eq!(i["labels"], json!(["in-erp"]));
    assert!(get(&owner, app.url("/issues/WEB-1/comments")).await.to_string().contains("Order ERP-1"));
    // a host it wasn't granted
    patch(&owner, app.url("/issues/WEB-2"), json!({"assigneeId": mia_id})).await;
    let failed = rules(&app, &owner).await;
    assert!(
        failed.iter().find(|r| r["name"] == "Elsewhere").unwrap()["lastError"].as_str().unwrap().contains("wasn't granted HTTP calls to 127.0.0.1"),
        "{failed:?}"
    );
    assert!(failed.iter().find(|r| r["name"] == "Missing").unwrap()["lastError"].as_str().unwrap().contains("isn't installed"));

    // events: it heard of the new issues (and set one's priority) and of comments
    eventually("the created handler", || async { get(&owner, app.url("/issues/WEB-1")).await["priority"] == "low" }).await;
    post(&mia, app.url("/issues/WEB-2/comments"), json!({"markdown": "Hello"})).await;
    let logs_url = app.url(&format!("/admin/extensions/{id}/logs"));
    eventually("the comment handler", || async { get(&owner, logs_url.clone()).await.to_string().contains("commented WEB-2") }).await;
    let logs = get(&owner, logs_url.clone()).await;
    assert!(logs.as_array().unwrap().iter().any(|l| l["hook"] == "event:created" && l["message"] == "created WEB-1"), "{logs}");
    assert!(logs.as_array().unwrap().iter().any(|l| l["level"] == "error" && l["hook"] == "action:push"), "{logs}");
    assert_eq!(send(&mia, Method::GET, logs_url.clone(), Value::Null).await.0, 403);

    // the scheduled job, when due — as Mia it doesn't see the hidden project
    sqlx::query("UPDATE extensions SET next_run_at = now() - interval '1 minute'").execute(&app.state.db).await.unwrap();
    taskcraft::extensions::tick(&app.state).await.unwrap();
    assert!(get(&owner, app.url("/issues/WEB-2/comments")).await.to_string().contains("Still needed?"));
    let (s, v) = send(&owner, Method::POST, app.url(&format!("/admin/extensions/{id}/run")), json!({})).await;
    assert_eq!((s, v["ok"].as_bool(), v["result"].as_i64()), (200, Some(true), Some(1)), "{v}");

    // its routes: for signed-in people, the ones in the manifest
    let status = get(&mia, app.url("/ext/erp-sync/status?since=2026-01-01&q=a%20b")).await;
    assert_eq!((status["last"].as_str(), status["count"].as_i64(), status["who"].as_str()), (Some("ERP-1"), Some(1), Some("Mia Member")), "{status}");
    assert_eq!(status["open"], json!(["WEB-2"]), "what Mia sees, not the hidden project");
    assert_eq!(status["q"], json!({"since": "2026-01-01", "q": "a b"}));
    assert_eq!(send(&client(), Method::GET, app.url("/ext/erp-sync/status"), Value::Null).await.0, 401);
    assert_eq!(send(&mia, Method::GET, app.url("/ext/erp-sync/other"), Value::Null).await.0, 404);
    assert_eq!(send(&mia, Method::POST, app.url("/ext/erp-sync/status"), json!({})).await.0, 404, "the method counts");
    assert_eq!(send(&mia, Method::POST, app.url("/ext/erp-sync/peek"), json!({"key": "WEB-2"})).await.1["title"], "A stale thing");
    // even when the owner asks: the extension reads as the person it runs as
    let (s, v) = send(&owner, Method::POST, app.url("/ext/erp-sync/peek"), json!({"key": "SEC-1"})).await;
    assert_eq!((s, v["error"].as_str()), (404, Some("not found")), "{v}");
    assert_eq!(send(&mia, Method::POST, app.url("/ext/erp-sync/bump"), json!({"key": "WEB-2"})).await.1, "high");
    assert_eq!(get(&owner, app.url("/issues/WEB-2")).await["priority"], "high");

    // fewer capabilities after an update: reading only
    let mut lesser = manifest.clone();
    lesser["capabilities"]["issues"] = json!("read");
    lesser["version"] = json!("1.3.0");
    let (s, v) = send(&owner, Method::PATCH, ext.clone(), json!({"manifest": lesser})).await;
    assert_eq!((s, v["version"].as_str()), (200, Some("1.3.0")), "{v}");
    let (s, v) = send(&mia, Method::POST, app.url("/ext/erp-sync/bump"), json!({"key": "WEB-1"})).await;
    assert!(s == 400 && v["message"].as_str().unwrap().contains("may read issues, not change them"), "{v}");
    let mut renamed = manifest.clone();
    renamed["name"] = json!("other");
    assert_eq!(send(&owner, Method::PATCH, ext.clone(), json!({"manifest": renamed})).await.0, 400);

    // one switch turns it off: no routes, no actions, no events, no schedule
    let (s, v) = send(&owner, Method::PATCH, ext.clone(), json!({"enabled": false})).await;
    assert!(s == 200 && v["enabled"] == false && v["nextRunAt"].is_null(), "{v}");
    assert_eq!(send(&mia, Method::GET, app.url("/ext/erp-sync/status"), Value::Null).await.0, 404);
    assert_eq!(get(&mia, app.url("/extensions/actions")).await, json!([]));
    let before = get(&owner, logs_url.clone()).await.as_array().unwrap().len();
    post(&mia, app.url("/issues"), json!({"projectKey": "WEB", "title": "Quiet"})).await;
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    assert_eq!(get(&owner, logs_url.clone()).await.as_array().unwrap().len(), before);
    let audit = get(&owner, app.url("/admin/audit")).await.to_string();
    for action in ["extension.installed", "extension.updated", "extension.disabled"] {
        assert!(audit.contains(action), "{action}");
    }
    assert_eq!(send(&owner, Method::DELETE, ext.clone(), Value::Null).await.0, 204);
    assert_eq!(get(&owner, app.url("/admin/extensions")).await["extensions"], json!([]));
}
