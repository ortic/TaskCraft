//! The MCP server over streamable HTTP (raw JSON-RPC, as any MCP client speaks it).

mod common;

use common::*;
use reqwest::Method;
use serde_json::{Value, json};

/// Sends a JSON-RPC message; parses a JSON or an SSE (`data:` lines) response.
async fn rpc(c: &reqwest::Client, url: &str, token: &str, body: Value) -> (u16, Option<Value>) {
    let r = c
        .post(url)
        .bearer_auth(token)
        .header("accept", "application/json, text/event-stream")
        .header("mcp-protocol-version", "2025-06-18")
        .json(&body)
        .send()
        .await
        .unwrap();
    let status = r.status().as_u16();
    let text = r.text().await.unwrap();
    let value = serde_json::from_str::<Value>(&text).ok().or_else(|| {
        text.lines()
            .filter_map(|l| l.strip_prefix("data:"))
            .map(str::trim)
            .filter(|d| !d.is_empty())
            .filter_map(|d| serde_json::from_str::<Value>(d).ok())
            .find(|v| v.get("id").is_some())
    });
    (status, value)
}

async fn initialize(c: &reqwest::Client, url: &str, token: &str) -> Value {
    let (st, init) = rpc(
        c,
        url,
        token,
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "test", "version": "1"}}}),
    )
    .await;
    assert_eq!(st, 200, "{init:?}");
    let (st, _) = rpc(c, url, token, json!({"jsonrpc": "2.0", "method": "notifications/initialized"})).await;
    assert!(st == 202 || st == 200, "{st}");
    init.unwrap()
}

/// Calls a tool: (is the result an error, its text).
async fn call(c: &reqwest::Client, url: &str, token: &str, tool: &str, args: Value) -> (bool, String) {
    let (st, v) = rpc(c, url, token, json!({"jsonrpc": "2.0", "id": 7, "method": "tools/call", "params": {"name": tool, "arguments": args}})).await;
    assert_eq!(st, 200, "{tool}: {v:?}");
    let v = v.unwrap();
    if let Some(e) = v.get("error") {
        return (true, e.to_string());
    }
    (v["result"]["isError"] == true, v["result"]["content"][0]["text"].as_str().unwrap_or_default().to_string())
}

async fn ok(c: &reqwest::Client, url: &str, token: &str, tool: &str, args: Value) -> String {
    let (err, text) = call(c, url, token, tool, args.clone()).await;
    assert!(!err, "{tool}({args}) failed: {text}");
    text
}

async fn ok_json(c: &reqwest::Client, url: &str, token: &str, tool: &str, args: Value) -> Value {
    let text = ok(c, url, token, tool, args).await;
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{tool}: not JSON ({e}): {text}"))
}

async fn token(app: &TestApp, c: &reqwest::Client, scope: &str) -> String {
    let t = post(c, app.url("/me/tokens"), json!({"name": "assistant", "scope": scope})).await;
    t["token"].as_str().unwrap().to_string()
}

fn status_id(p: &Value, name: &str) -> String {
    p["statuses"].as_array().unwrap().iter().find(|s| s["name"] == name).unwrap()["id"].as_str().unwrap().to_string()
}

#[tokio::test(flavor = "multi_thread")]
async fn tools_work_with_the_callers_permissions() {
    let app = spawn().await;
    let owner = app.setup(false).await;
    post(&owner, app.url("/projects"), json!({"key": "WEB", "name": "Web shop", "template": "scrum"})).await;
    let tok = token(&app, &owner, "write").await;
    let url = app.url("/mcp");
    let c = reqwest::Client::new();

    let init = initialize(&c, &url, &tok).await;
    assert_eq!(init["result"]["serverInfo"]["name"], "taskcraft");
    assert!(init["result"]["instructions"].as_str().unwrap().contains("search_issues"));
    let (_, tools) = rpc(&c, &url, &tok, json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"})).await;
    let tools = tools.unwrap();
    let names: Vec<&str> = tools["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
    for n in ["search_issues", "get_issue", "create_issue", "update_issue", "add_comment", "log_work", "list_projects", "get_sprint", "get_board", "run_report"]
    {
        assert!(names.contains(&n), "missing tool {n}: {names:?}");
    }
    let search = tools["result"]["tools"].as_array().unwrap().iter().find(|t| t["name"] == "search_issues").unwrap();
    assert!(search["description"].as_str().unwrap().contains("openSprints()"), "the tool explains TQL");

    // projects with their vocabulary
    let projects = ok_json(&c, &url, &tok, "list_projects", json!({})).await;
    let web = &projects[0];
    assert_eq!(web["key"], "WEB");
    assert!(web["statuses"].to_string().contains("In review") && web["types"].to_string().contains("Bug"), "{web}");

    // create with names instead of ids
    let created = ok_json(
        &c,
        &url,
        &tok,
        "create_issue",
        json!({"project_key": "web", "title": "Checkout fails for PayPal", "description": "Steps:\n\n1. pay with **PayPal**\n\n- [ ] add a test",
               "type": "bug", "priority": "high", "assignee": "me", "labels": ["payments"], "sprint": "WEB Sprint 1", "story_points": 3, "estimate": "2h"}),
    )
    .await;
    assert_eq!(created["created"], "WEB-1", "{created}");
    assert_eq!(created["issue"]["type"], "Bug");
    assert_eq!(created["issue"]["assignee"], "Olivia Owner");
    assert_eq!(created["issue"]["sprint"], "WEB Sprint 1");
    let (err, msg) = call(&c, &url, &tok, "create_issue", json!({"project_key": "WEB", "title": "x", "type": "Saga"})).await;
    assert!(err && msg.contains("Story"), "unknown types list the real ones: {msg}");

    let md = ok(&c, &url, &tok, "get_issue", json!({"key": "web-1"})).await;
    for want in [
        "# WEB-1: Checkout fails for PayPal",
        "**PayPal**",
        "- [ ] add a test",
        "Type: Bug",
        "Assignee: Olivia Owner",
        "Branch name: web-1-checkout-fails-for-paypal",
    ] {
        assert!(md.contains(want), "missing {want:?} in\n{md}");
    }

    let found = ok_json(&c, &url, &tok, "search_issues", json!({"query": "project = WEB and type = Bug and priority >= high"})).await;
    assert_eq!(found["total"], 1);
    assert_eq!(found["issues"][0]["key"], "WEB-1");
    let (err, msg) = call(&c, &url, &tok, "search_issues", json!({"query": "status > Done"})).await;
    assert!(err, "bad TQL is an error result: {msg}");

    // update by names; labels added and removed
    let up = ok_json(
        &c,
        &url,
        &tok,
        "update_issue",
        json!({"key": "WEB-1", "status": "in progress", "add_labels": ["agent"], "remove_labels": ["payments"], "assignee": "none"}),
    )
    .await;
    assert_eq!(up["issue"]["status"], "In progress", "{up}");
    assert_eq!(up["issue"]["labels"], json!(["agent"]));
    assert!(up["issue"].get("assignee").is_none());

    // restricted workflows: only the allowed moves, and the error says which ones exist
    let p = get(&owner, app.url("/projects/WEB")).await;
    let (todo, doing, review, done) = (status_id(&p, "To do"), status_id(&p, "In progress"), status_id(&p, "In review"), status_id(&p, "Done"));
    let (s, _) = send(
        &owner,
        Method::PUT,
        app.url("/projects/WEB/transitions"),
        json!({"workflow": "restricted", "transitions": [{"from": todo, "to": doing}, {"from": doing, "to": review}, {"from": review, "to": done}]}),
    )
    .await;
    assert_eq!(s, 204);
    let (err, msg) = call(&c, &url, &tok, "update_issue", json!({"key": "WEB-1", "status": "To do"})).await;
    assert!(err && msg.contains("can move to: In review"), "{msg}");
    let detail = ok_json(&c, &url, &tok, "list_projects", json!({"project_key": "WEB"})).await;
    assert_eq!(detail["workflow"], "restricted");
    assert_eq!(detail["transitions"]["In progress"], json!(["In review"]));

    ok(&c, &url, &tok, "add_comment", json!({"key": "WEB-1", "text": "Found the cause: **expired** token"})).await;
    let comments = get(&owner, app.url("/issues/WEB-1/comments")).await;
    assert!(comments.to_string().contains("expired"));
    let logged = ok_json(&c, &url, &tok, "log_work", json!({"key": "WEB-1", "time": "1h 30m", "note": "debugging", "reduce_estimate": true})).await;
    assert_eq!(logged["logged"], "1h 30m");
    assert_eq!(logged["remainingEstimate"], "30m");
    let md = ok(&c, &url, &tok, "get_issue", json!({"key": "WEB-1"})).await;
    assert!(md.contains("**expired**") && md.contains("1h 30m logged"), "{md}");

    // sprint, board and reports
    let (err, msg) = call(&c, &url, &tok, "get_sprint", json!({"project_key": "WEB"})).await;
    assert!(err && msg.contains("no running sprint"), "{msg}");
    let sprint_id = p["sprints"][0]["id"].as_str().unwrap().to_string();
    post(&owner, app.url(&format!("/sprints/{sprint_id}/start")), json!({"goal": "Pay"})).await;
    let sprint = ok_json(&c, &url, &tok, "get_sprint", json!({"project_key": "WEB"})).await;
    assert_eq!(sprint["sprint"]["goal"], "Pay");
    assert_eq!(sprint["figures"]["issues"], 1);
    assert_eq!(sprint["figures"]["points"], 3.0);
    let board = ok_json(&c, &url, &tok, "get_board", json!({"project_key": "WEB"})).await;
    let doing_col = board["columns"].as_array().unwrap().iter().find(|c| c["name"] == "In progress").unwrap();
    assert_eq!(doing_col["issues"][0]["key"], "WEB-1", "{board}");
    let overview = ok_json(&c, &url, &tok, "run_report", json!({"project_key": "WEB", "report": "overview"})).await;
    assert_eq!(overview["data"]["categories"]["in_progress"], 1, "{overview}");
    let burndown = ok_json(&c, &url, &tok, "run_report", json!({"project_key": "WEB", "report": "burndown"})).await;
    assert_eq!(burndown["data"]["sprint"]["name"], "WEB Sprint 1");
    let (err, _) = call(&c, &url, &tok, "run_report", json!({"project_key": "WEB", "report": "horoscope"})).await;
    assert!(err);

    // someone without access to the project sees an error, not the issue
    let (guest, _) = app.user(&owner, "Gail Guest", "gail@example.com", "guest").await;
    let guest_tok = token(&app, &guest, "write").await;
    let (err, msg) = call(&c, &url, &guest_tok, "get_issue", json!({"key": "WEB-1"})).await;
    assert!(err && !msg.contains("PayPal"), "{msg}");
    let found = ok_json(&c, &url, &guest_tok, "search_issues", json!({"query": "PayPal"})).await;
    assert_eq!(found["total"], 0);
    let (err, _) = call(&c, &url, &guest_tok, "create_issue", json!({"project_key": "WEB", "title": "sneaky"})).await;
    assert!(err);

    // no valid token: nothing
    let (err, msg) = call(&c, &url, "tct_not-a-token", "list_projects", json!({})).await;
    assert!(err && msg.contains("authentication"), "{msg}");
    // the versioned path works the same
    let init = initialize(&c, &app.url("/v1/mcp"), &tok).await;
    assert_eq!(init["result"]["serverInfo"]["name"], "taskcraft");
}

/// Read-only tokens: reading tools work, changing ones refuse (and nothing changes).
#[tokio::test(flavor = "multi_thread")]
async fn read_only_tokens_only_read() {
    let app = spawn().await;
    let owner = app.setup(false).await;
    post(&owner, app.url("/projects"), json!({"key": "OPS", "name": "Operations", "template": "kanban"})).await;
    post(&owner, app.url("/issues"), json!({"projectKey": "OPS", "title": "Rotate the certificates"})).await;
    let tok = token(&app, &owner, "read").await;
    let url = app.url("/mcp");
    let c = reqwest::Client::new();
    initialize(&c, &url, &tok).await;

    let found = ok_json(&c, &url, &tok, "search_issues", json!({"query": "certificates"})).await;
    assert_eq!(found["total"], 1);
    assert!(ok(&c, &url, &tok, "get_issue", json!({"key": "OPS-1"})).await.contains("Rotate the certificates"));
    ok_json(&c, &url, &tok, "get_board", json!({"project_key": "OPS"})).await;
    for (tool, args) in [
        ("create_issue", json!({"project_key": "OPS", "title": "Sneaky"})),
        ("update_issue", json!({"key": "OPS-1", "title": "Renamed"})),
        ("add_comment", json!({"key": "OPS-1", "text": "hi"})),
        ("log_work", json!({"key": "OPS-1", "time": "1h"})),
    ] {
        let (err, msg) = call(&c, &url, &tok, tool, args).await;
        assert!(err && msg.contains("read-only"), "{tool}: {msg}");
    }
    let issue = get(&owner, app.url("/issues/OPS-1")).await;
    assert_eq!(issue["title"], "Rotate the certificates");
    assert_eq!(issue["commentCount"], 0);
    let found = get(&owner, app.url("/search?q=project%20%3D%20OPS")).await;
    assert_eq!(found["total"], 1, "nothing was created");
    // the REST API refuses the same token's writes too
    let r = c.post(app.url("/issues")).bearer_auth(&tok).json(&json!({"projectKey": "OPS", "title": "x"})).send().await.unwrap();
    assert_eq!(r.status(), 403);
}
