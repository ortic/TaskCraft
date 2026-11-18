//! AI coding agents, end to end: the real runner (crates/runner) against the real server, with a
//! stand-in model and a stand-in Git server. An agent is assigned an issue, reads the repository
//! it got through TaskCraft, changes files and finishes — a branch, a commit and a draft pull
//! request appear, linked to the issue; a question gets an answer as a comment. The runner holds
//! neither the Git token nor the model key. Testing mode, one run at a time, cancelling, what the
//! agent may see, and runs whose runner went away.

mod common;

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use axum::Json;
use axum::Router;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get as route_get, patch as route_patch, post as route_post};
use base64::Engine;
use common::*;
use reqwest::Method;
use serde_json::{Value, json};
use taskcraft_runner::Runner;

// ---- a stand-in model (the Messages API): replies from a script -----------------------------------

#[derive(Default)]
struct Model {
    replies: Mutex<VecDeque<Value>>,
    requests: Mutex<Vec<(String, Value)>>,
}

impl Model {
    fn script(&self, replies: Vec<Value>) {
        *self.replies.lock().unwrap() = replies.into();
    }
    fn requests(&self) -> Vec<(String, Value)> {
        self.requests.lock().unwrap().clone()
    }
}

async fn model_server(model: Arc<Model>) -> String {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = format!("http://{}", l.local_addr().unwrap());
    let app = Router::new().route(
        "/v1/messages",
        route_post(move |headers: HeaderMap, Json(body): Json<Value>| {
            let model = model.clone();
            async move {
                let key = headers.get("x-api-key").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
                model.requests.lock().unwrap().push((key, body.clone()));
                let reply = model.replies.lock().unwrap().pop_front().unwrap_or_else(|| json!({"tool": "finish", "answer": "Nothing more to say."}));
                // some models wrap their JSON in prose: the runner copes
                let text = if reply["tool"] == "list" { format!("Let me look.\n```json\n{reply}\n```") } else { reply.to_string() };
                Json(json!({"id": "msg", "type": "message", "role": "assistant", "model": body["model"], "content": [{"type": "text", "text": text}],
                            "stop_reason": "end_turn", "usage": {"input_tokens": 100, "output_tokens": 20}}))
            }
        }),
    );
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    addr
}

// ---- a stand-in Git server (GitHub Enterprise layout) ----------------------------------------------

#[derive(Default)]
struct Git {
    calls: Mutex<Vec<String>>,
    blobs: Mutex<Vec<String>>,
    trees: Mutex<Vec<Value>>,
    commits: Mutex<Vec<Value>>,
    pulls: Mutex<Vec<Value>>,
    branches: Mutex<Vec<String>>,
    base: Mutex<String>,
}

type G = State<Arc<Git>>;

impl Git {
    fn note(&self, what: &str, h: &HeaderMap) {
        let auth = h.get("authorization").and_then(|v| v.to_str().ok()).unwrap_or("");
        self.calls.lock().unwrap().push(format!("{what} [{auth}]"));
    }
    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
}

fn repository_archive() -> Vec<u8> {
    let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default()));
    for (path, content) in [
        ("acme-shop-1a2b3c/README.md", "# Shop\n"),
        ("acme-shop-1a2b3c/src/lib.rs", "pub fn one() -> u8 {\n    2\n}\n"),
        ("acme-shop-1a2b3c/old.txt", "left over\n"),
    ] {
        let mut header = tar::Header::new_gnu();
        header.set_size(content.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder.append_data(&mut header, path, content.as_bytes()).unwrap();
    }
    builder.into_inner().unwrap().finish().unwrap()
}

async fn git_server() -> (Arc<Git>, String) {
    let git = Arc::new(Git::default());
    git.branches.lock().unwrap().push("main".into());
    let app = Router::new()
        .route(
            "/api/v3/repos/acme/shop",
            route_get(|State(g): G, h: HeaderMap| async move {
                g.note("GET repo", &h);
                Json(json!({"default_branch": "main"}))
            }),
        )
        .route(
            "/api/v3/repos/acme/shop/tarball/{*reference}",
            route_get(|State(g): G, h: HeaderMap, Path(r): Path<String>| async move {
                g.note(&format!("GET tarball {r}"), &h);
                ([("content-type", "application/gzip")], repository_archive()).into_response()
            }),
        )
        .route(
            "/api/v3/repos/acme/shop/git/ref/heads/{*branch}",
            route_get(|State(g): G, h: HeaderMap, Path(b): Path<String>| async move {
                g.note(&format!("GET ref {b}"), &h);
                if !g.branches.lock().unwrap().contains(&b) {
                    return (StatusCode::NOT_FOUND, Json(json!({"message": "Not Found"})));
                }
                (StatusCode::OK, Json(json!({"object": {"sha": format!("head-of-{b}")}})))
            }),
        )
        .route(
            "/api/v3/repos/acme/shop/git/refs",
            route_post(|State(g): G, h: HeaderMap, Json(b): Json<Value>| async move {
                let name = b["ref"].as_str().unwrap().trim_start_matches("refs/heads/").to_string();
                g.note(&format!("POST ref {name}"), &h);
                let mut branches = g.branches.lock().unwrap();
                if branches.contains(&name) {
                    return (StatusCode::UNPROCESSABLE_ENTITY, Json(json!({"message": "Reference already exists"})));
                }
                branches.push(name);
                (StatusCode::CREATED, Json(b))
            }),
        )
        .route(
            "/api/v3/repos/acme/shop/git/commits/{sha}",
            route_get(|State(g): G, h: HeaderMap, Path(sha): Path<String>| async move {
                g.note(&format!("GET commit {sha}"), &h);
                Json(json!({"sha": sha, "tree": {"sha": "base-tree"}}))
            }),
        )
        .route(
            "/api/v3/repos/acme/shop/git/blobs",
            route_post(|State(g): G, h: HeaderMap, Json(b): Json<Value>| async move {
                g.note("POST blob", &h);
                let content = String::from_utf8(base64::engine::general_purpose::STANDARD.decode(b["content"].as_str().unwrap()).unwrap()).unwrap();
                let mut blobs = g.blobs.lock().unwrap();
                blobs.push(content);
                (StatusCode::CREATED, Json(json!({"sha": format!("blob-{}", blobs.len())})))
            }),
        )
        .route(
            "/api/v3/repos/acme/shop/git/trees",
            route_post(|State(g): G, h: HeaderMap, Json(b): Json<Value>| async move {
                g.note("POST tree", &h);
                g.trees.lock().unwrap().push(b);
                (StatusCode::CREATED, Json(json!({"sha": "new-tree"})))
            }),
        )
        .route(
            "/api/v3/repos/acme/shop/git/commits",
            route_post(|State(g): G, h: HeaderMap, Json(b): Json<Value>| async move {
                g.note("POST commit", &h);
                g.commits.lock().unwrap().push(b);
                (StatusCode::CREATED, Json(json!({"sha": "new-commit"})))
            }),
        )
        .route(
            "/api/v3/repos/acme/shop/git/refs/heads/{*branch}",
            route_patch(|State(g): G, h: HeaderMap, Path(b): Path<String>, Json(body): Json<Value>| async move {
                g.note(&format!("PATCH ref {b} {}", body["sha"].as_str().unwrap()), &h);
                Json(json!({"object": {"sha": body["sha"]}}))
            }),
        )
        .route(
            "/api/v3/repos/acme/shop/pulls",
            route_post(|State(g): G, h: HeaderMap, Json(b): Json<Value>| async move {
                g.note("POST pull", &h);
                let mut pulls = g.pulls.lock().unwrap();
                pulls.push(b);
                let n = pulls.len();
                (StatusCode::CREATED, Json(json!({"number": n, "html_url": format!("{}/acme/shop/pull/{n}", g.base.lock().unwrap())})))
            }),
        )
        .with_state(git.clone());
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    *git.base.lock().unwrap() = base.clone();
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    (git, base)
}

fn mention(id: &str, name: &str, text: &str) -> Value {
    json!({"type": "doc", "content": [{"type": "paragraph", "content": [{"type": "mention", "attrs": {"id": id, "label": name}}, {"type": "text", "text": format!(" {text}")}]}]})
}

async fn runs(app: &TestApp, c: &reqwest::Client, key: &str) -> Vec<Value> {
    get(c, app.url(&format!("/issues/{key}/agent-runs"))).await.as_array().unwrap().clone()
}

#[tokio::test(flavor = "multi_thread")]
async fn an_agent_turns_an_issue_into_a_draft_pull_request() {
    let model = Arc::new(Model::default());
    let model_url = model_server(model.clone()).await;
    let (git, git_url) = git_server().await;
    let app = spawn_with(move |c| {
        c.ai_api_key = Some("sk-server-only".into());
        c.ai_base_url = Some(model_url);
        c.ai_provider = Some("anthropic".into());
    })
    .await;
    let owner = app.setup(false).await;
    post(&owner, app.url("/projects"), json!({"key": "WEB", "name": "Web shop", "template": "scrum"})).await;
    post(&owner, app.url("/projects"), json!({"key": "SEC", "name": "Secret", "template": "kanban"})).await;
    patch(&owner, app.url("/projects/SEC"), json!({"defaultRole": "none"})).await;
    let (max, _) = app.user(&owner, "Max Member", "max@example.com", "member").await;
    post(&max, app.url("/issues"), json!({"projectKey": "WEB", "title": "one() returns two", "descriptionMarkdown": "`one()` in src/lib.rs should return 1."}))
        .await;
    post(&max, app.url("/issues"), json!({"projectKey": "WEB", "title": "Second thing"})).await;
    post(&owner, app.url("/issues"), json!({"projectKey": "SEC", "title": "Not for agents"})).await;
    post(&max, app.url("/issues/WEB-1/comments"), json!({"markdown": "It broke with the last release."})).await;
    post(&owner, app.url("/issues/WEB-1/comments"), json!({"markdown": "Admins only: the customer is angry.", "visibility": "admins"})).await;

    // the repository, with a token for its API
    let hook = post(&owner, app.url("/projects/WEB/git"), json!({"provider": "github", "repoUrl": format!("{git_url}/acme/shop")})).await;
    let (s, v) =
        send(&owner, Method::PUT, app.url(&format!("/git-integrations/{}/api", hook["id"].as_str().unwrap())), json!({"token": "git-server-only"})).await;
    assert_eq!(s, 200, "{v}");

    // an agent: admins define it; it's a person of its own kind
    let new = json!({"name": "Cody", "handle": "@Cody", "instructions": "Keep changes small. Explain why.", "integrationId": hook["id"]});
    assert_eq!(send(&max, Method::POST, app.url("/admin/agents"), new.clone()).await.0, 403);
    let (s, agent) = send(&owner, Method::POST, app.url("/admin/agents"), new.clone()).await;
    assert_eq!(s, 201, "{agent}");
    let (agent_id, cody) = (agent["id"].as_str().unwrap().to_string(), agent["userId"].as_str().unwrap().to_string());
    assert_eq!(send(&owner, Method::POST, app.url("/admin/agents"), new).await.0, 409, "the handle is taken");
    assert_eq!(send(&owner, Method::POST, app.url("/admin/agents"), json!({"name": "X", "handle": "x y"})).await.0, 400);
    let page = get(&owner, app.url("/admin/agents")).await;
    assert_eq!(
        (page["agents"][0]["handle"].as_str(), page["agents"][0]["state"].as_str(), page["agents"][0]["repository"].as_str()),
        (Some("cody"), Some("testing"), Some("acme/shop")),
        "{page}"
    );
    assert_eq!((page["modelConfigured"].as_bool(), page["repositories"][0]["repo"].as_str()), (Some(true), Some("acme/shop")));
    let people = get(&max, app.url("/people")).await;
    assert_eq!(people.as_array().unwrap().iter().find(|p| p["name"] == "Cody").expect("assignable like anyone")["agent"], "cody");
    assert!(people.as_array().unwrap().iter().find(|p| p["name"] == "Max Member").unwrap()["agent"].is_null());

    // a runner: registered with a one-time token
    let (s, added) = send(&owner, Method::POST, app.url("/admin/agent-runners"), json!({"name": "build-box"})).await;
    assert_eq!(s, 201, "{added}");
    let registration = added["registrationToken"].as_str().unwrap().to_string();
    assert!(taskcraft_runner::register(&app.base, "tcr_reg_made-up", None).await.unwrap_err().contains("isn't accepted"));
    let (name, runner_token) = taskcraft_runner::register(&app.base, &registration, None).await.unwrap();
    assert_eq!(name, "build-box");
    assert!(taskcraft_runner::register(&app.base, &registration, None).await.is_err(), "the registration token works once");
    let log: Arc<Mutex<Vec<String>>> = Default::default();
    let mut runner = Runner::new(&app.base, &runner_token);
    let sink = log.clone();
    runner.out = Arc::new(move |l| sink.lock().unwrap().push(l));
    assert!(!runner.work_once().await.unwrap(), "nothing waits");
    assert!(Runner::new(&app.base, "tcr_nope").work_once().await.unwrap_err().contains("doesn't know this runner"));

    // while it's being tried out only workspace admins start it
    patch(&max, app.url("/issues/WEB-1"), json!({"assigneeId": cody})).await;
    assert!(runs(&app, &max, "WEB-1").await.is_empty());
    patch(&owner, app.url("/issues/WEB-1"), json!({"assigneeId": null})).await;
    patch(&owner, app.url("/issues/WEB-1"), json!({"assigneeId": cody})).await;
    let queued = runs(&app, &max, "WEB-1").await;
    assert_eq!(
        (queued.len(), queued[0]["state"].as_str(), queued[0]["trigger"].as_str(), queued[0]["agent"]["handle"].as_str()),
        (1, Some("queued"), Some("assigned"), Some("cody")),
        "{queued:?}"
    );
    assert_eq!((queued[0]["triggeredBy"]["name"].as_str(), queued[0]["canCancel"].as_bool()), (Some("Olivia Owner"), Some(false)), "Max didn't start it");
    // a mention while it's on the issue doesn't start a second run
    post(&owner, app.url("/issues/WEB-1/comments"), json!({"body": mention(&cody, "Cody", "please hurry")})).await;
    assert_eq!(runs(&app, &max, "WEB-1").await.len(), 1);

    // the run: look, read, change two files, delete one, finish
    model.script(vec![
        json!({"tool": "list", "path": "."}),
        json!({"tool": "read", "path": "src/lib.rs"}),
        json!({"tool": "read", "path": "../../etc/passwd"}),
        json!({"tool": "run", "command": "cat /etc/passwd"}),
        json!({"tool": "write", "path": "src/lib.rs", "content": "pub fn one() -> u8 {\n    1\n}\n"}),
        json!({"tool": "write", "path": "docs/NOTES.md", "content": "one() returns 1 again.\n"}),
        json!({"tool": "delete", "path": "old.txt"}),
        json!({"tool": "finish", "summary": "`one()` returned 2; it returns 1 again.\n\n- fixed `src/lib.rs`\n- removed a leftover file"}),
    ]);
    assert!(runner.work_once().await.unwrap());

    // the model was asked by the server, with the server's key, about what the agent may read
    let asked = model.requests();
    assert_eq!(asked.len(), 8);
    assert!(asked.iter().all(|(key, _)| key == "sk-server-only"));
    let system = asked[0].1["system"].as_str().unwrap();
    assert!(system.contains("You are Cody") && system.contains("Keep changes small. Explain why.") && !system.contains("\"run\""), "{system}");
    let first = asked[0].1["messages"][0]["content"].as_str().unwrap();
    for part in ["WEB-1 one() returns two", "should return 1", "It broke with the last release.", "README.md", "src/"] {
        assert!(first.contains(part), "{part} in {first}");
    }
    assert!(!first.contains("customer is angry"), "restricted comments aren't for the agent");
    // what the tools answered
    let turns = asked.last().unwrap().1["messages"].as_array().unwrap().clone();
    let said: Vec<&str> = turns.iter().filter(|m| m["role"] == "user").map(|m| m["content"].as_str().unwrap()).collect();
    assert!(said[2].contains("pub fn one() -> u8 {\n    2\n}"), "{}", said[2]);
    assert!(said[3].contains("outside the repository"), "{}", said[3]);
    assert!(said[4].contains("doesn't run commands"), "{}", said[4]);

    // the Git server: fetched and changed with the server's token only
    let calls = git.calls();
    assert!(calls.iter().all(|c| c.ends_with("[Bearer git-server-only]")), "{calls:#?}");
    for want in [
        "GET tarball main",
        "POST ref web-1-one-returns-two",
        "POST blob",
        "POST tree",
        "POST commit",
        "PATCH ref web-1-one-returns-two new-commit",
        "POST pull",
    ] {
        assert!(calls.iter().any(|c| c.starts_with(want)), "{want} in {calls:#?}");
    }
    assert_eq!(*git.blobs.lock().unwrap(), vec!["one() returns 1 again.\n".to_string(), "pub fn one() -> u8 {\n    1\n}\n".to_string()]);
    let tree = git.trees.lock().unwrap()[0].clone();
    assert_eq!(tree["base_tree"], "base-tree");
    let entries: Vec<(&str, bool)> = tree["tree"].as_array().unwrap().iter().map(|e| (e["path"].as_str().unwrap(), e["sha"].is_null())).collect();
    assert_eq!(entries, [("docs/NOTES.md", false), ("src/lib.rs", false), ("old.txt", true)]);
    let commit = git.commits.lock().unwrap()[0].clone();
    assert!(commit["message"].as_str().unwrap().starts_with("WEB-1: one() returns two\n\n`one()` returned 2"), "{commit}");
    assert_eq!(commit["parents"], json!(["head-of-web-1-one-returns-two"]));
    let pull = git.pulls.lock().unwrap()[0].clone();
    assert_eq!(
        (pull["title"].as_str(), pull["head"].as_str(), pull["base"].as_str(), pull["draft"].as_bool()),
        (Some("WEB-1 one() returns two"), Some("web-1-one-returns-two"), Some("main"), Some(true))
    );
    assert!(pull["body"].as_str().unwrap().contains("/issues/WEB-1"));

    // the issue: the pull request and branch are linked, Cody says what it did, the run shows it all
    let dev = get(&max, app.url("/issues/WEB-1/development")).await;
    let pr = &dev["links"][0];
    assert_eq!(
        (pr["kind"].as_str(), pr["number"].as_i64(), pr["draft"].as_bool(), pr["author"].as_str()),
        (Some("pull_request"), Some(1), Some(true), Some("Cody")),
        "{dev}"
    );
    assert_eq!(dev["links"][1]["kind"], "branch");
    let comments = get(&max, app.url("/issues/WEB-1/comments")).await;
    let last = comments.as_array().unwrap().last().unwrap();
    assert_eq!(last["author"]["name"], "Cody");
    assert!(last.to_string().contains("it returns 1 again") && last.to_string().contains("/acme/shop/pull/1"), "{last}");
    let done = runs(&app, &max, "WEB-1").await;
    assert_eq!(
        (done.len(), done[0]["state"].as_str(), done[0]["result"]["kind"].as_str(), done[0]["result"]["files"].as_i64()),
        (1, Some("succeeded"), Some("pull_request"), Some(3)),
        "{done:?}"
    );
    assert_eq!((done[0]["modelCalls"].as_i64(), done[0]["tokensIn"].as_i64(), done[0]["tokensOut"].as_i64()), (Some(8), Some(800), Some(160)));
    let lines = get(&max, app.url(&format!("/agent-runs/{}/log", done[0]["id"].as_str().unwrap()))).await;
    let text: Vec<&str> = lines["lines"].as_array().unwrap().iter().map(|l| l["line"].as_str().unwrap()).collect();
    for want in [
        "Picked up by a runner.",
        "Fetching acme/shop (main).",
        "3 files.",
        "Reading src/lib.rs",
        "Writing docs/NOTES.md",
        "Deleting old.txt",
        "3 files changed.",
        "Done.",
    ] {
        assert!(text.contains(&want), "{want} in {text:?}");
    }
    assert_eq!(lines["state"], "succeeded");
    assert!(log.lock().unwrap().iter().any(|l| l.contains("Writing src/lib.rs")));
    let audit = get(&owner, app.url("/admin/audit")).await.to_string();
    for action in ["agent.created", "agent.runner.added", "agent.runner.registered", "agent.run.started", "agent.run.finished"] {
        assert!(audit.contains(action), "{action}");
    }

    // for everyone now; a question in a comment gets its answer as a comment, no branch
    assert_eq!(send(&owner, Method::PATCH, app.url(&format!("/admin/agents/{agent_id}")), json!({"state": "active"})).await.0, 204);
    post(&max, app.url("/issues/WEB-1/comments"), json!({"body": mention(&cody, "Cody", "what does one() return now?")})).await;
    let asked_runs = runs(&app, &max, "WEB-1").await;
    assert_eq!((asked_runs.len(), asked_runs[0]["trigger"].as_str(), asked_runs[0]["canCancel"].as_bool()), (2, Some("mentioned"), Some(true)));
    model.script(vec![
        json!({"tool": "read", "path": "src/lib.rs"}),
        json!({"tool": "finish", "answer": "In the repository's main branch it still returns 2 until the pull request is merged."}),
    ]);
    let pulls_before = git.pulls.lock().unwrap().len();
    assert!(runner.work_once().await.unwrap());
    let prompt = model.requests().iter().rev().nth(1).unwrap().1["messages"][0]["content"].as_str().unwrap().to_string();
    assert!(prompt.contains("what does one() return now?"), "{prompt}");
    let comments = get(&max, app.url("/issues/WEB-1/comments")).await;
    assert!(comments.as_array().unwrap().last().unwrap().to_string().contains("still returns 2"));
    assert_eq!(git.pulls.lock().unwrap().len(), pulls_before);
    assert_eq!(runs(&app, &max, "WEB-1").await[0]["result"]["kind"], "answer");

    // the model answers nonsense: the run fails and says why
    post(&max, app.url("/issues/WEB-2/comments"), json!({"body": mention(&cody, "Cody", "and this?")})).await;
    model.script((0..50).map(|_| json!({"tool": "list", "path": "."})).collect());
    runner.max_steps = 3;
    assert!(runner.work_once().await.unwrap());
    let failed = runs(&app, &max, "WEB-2").await;
    assert_eq!((failed[0]["state"].as_str(), failed[0]["error"].as_str()), (Some("failed"), Some("the agent didn't finish within 3 steps")), "{failed:?}");
    runner.max_steps = 40;

    // cancelling: who started it (or an admin); a queued run is never picked up, a running one loses its token
    patch(&max, app.url("/issues/WEB-2"), json!({"assigneeId": cody})).await;
    let run = runs(&app, &max, "WEB-2").await[0].clone();
    assert_eq!(run["state"], "queued");
    let cancel = app.url(&format!("/agent-runs/{}/cancel", run["id"].as_str().unwrap()));
    let (ann, _) = app.user(&owner, "Ann Other", "ann@example.com", "member").await;
    assert_eq!(send(&ann, Method::POST, cancel.clone(), json!({})).await.0, 403);
    assert_eq!(send(&max, Method::POST, cancel.clone(), json!({})).await.0, 204);
    assert_eq!(send(&max, Method::POST, cancel, json!({})).await.0, 409);
    assert!(!runner.work_once().await.unwrap());
    post(&max, app.url("/issues/WEB-2/comments"), json!({"body": mention(&cody, "Cody", "again")})).await;
    let http = reqwest::Client::new();
    let job: Value = http.post(app.url("/agent-runner/claim")).bearer_auth(&runner_token).send().await.unwrap().json().await.unwrap();
    let (run_id, run_token) = (job["run"]["id"].as_str().unwrap().to_string(), job["run"]["token"].as_str().unwrap().to_string());
    assert_eq!(runs(&app, &max, "WEB-2").await[0]["state"], "running");
    // a run's token is good for that run only
    let other = http
        .post(app.url(&format!("/agent-runs/{}/model", done[0]["id"].as_str().unwrap())))
        .bearer_auth(&run_token)
        .json(&json!({"messages": [{"role": "user", "content": "x"}]}))
        .send()
        .await
        .unwrap();
    assert_eq!(other.status(), 409);
    assert_eq!(http.get(app.url("/issues/WEB-1")).bearer_auth(&run_token).send().await.unwrap().status(), 401, "and for nothing else");
    assert_eq!(send(&owner, Method::POST, app.url(&format!("/agent-runs/{run_id}/cancel")), json!({})).await.0, 204);
    let late = http.post(app.url(&format!("/agent-runs/{run_id}/finish"))).bearer_auth(&run_token).json(&json!({"answer": "too late"})).send().await.unwrap();
    assert_eq!(late.status(), 409);
    assert!(!get(&max, app.url("/issues/WEB-2/comments")).await.to_string().contains("too late"));

    // a result that reaches outside the repository is refused
    post(&max, app.url("/issues/WEB-2/comments"), json!({"body": mention(&cody, "Cody", "once more")})).await;
    let job: Value = http.post(app.url("/agent-runner/claim")).bearer_auth(&runner_token).send().await.unwrap().json().await.unwrap();
    let evil = json!({"summary": "x", "files": [{"path": ".git/hooks/pre-commit", "status": "added", "content": "eA=="}]});
    let r = http
        .post(app.url(&format!("/agent-runs/{}/finish", job["run"]["id"].as_str().unwrap())))
        .bearer_auth(job["run"]["token"].as_str().unwrap())
        .json(&evil)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 400);
    assert_eq!(runs(&app, &max, "WEB-2").await[0]["state"], "failed");

    // the agent works only on what its account sees
    post(&owner, app.url("/issues/SEC-1/comments"), json!({"body": mention(&cody, "Cody", "look at this")})).await;
    assert!(runs(&app, &owner, "SEC-1").await.is_empty());

    // a runner that went away: the run fails after a while
    post(&max, app.url("/issues/WEB-2/comments"), json!({"body": mention(&cody, "Cody", "last one")})).await;
    let job: Value = http.post(app.url("/agent-runner/claim")).bearer_auth(&runner_token).send().await.unwrap().json().await.unwrap();
    sqlx::query("UPDATE agent_runs SET heartbeat_at = now() - interval '31 minutes' WHERE state = 'running'").execute(&app.state.db).await.unwrap();
    taskcraft::agents::tick(&app.state).await.unwrap();
    let lost = runs(&app, &max, "WEB-2").await;
    assert_eq!((lost[0]["id"].as_str(), lost[0]["error"].as_str()), (job["run"]["id"].as_str(), Some("the runner stopped answering")));

    // the admin's view of its runs; off, the switch, removing
    let all = get(&owner, app.url(&format!("/admin/agents/{agent_id}/runs"))).await;
    assert_eq!(all.as_array().unwrap().len(), 7);
    assert!(all.as_array().unwrap().iter().any(|r| r["issue"]["key"] == "WEB-1" && r["tokensIn"] == 800));
    assert_eq!(send(&max, Method::GET, app.url(&format!("/admin/agents/{agent_id}/runs")), Value::Null).await.0, 403);
    assert_eq!(send(&owner, Method::PUT, app.url("/admin/workspace"), json!({"features": {"agents": false}})).await.0 / 100, 2);
    post(&max, app.url("/issues/WEB-2/comments"), json!({"body": mention(&cody, "Cody", "anyone?")})).await;
    assert_eq!(runs(&app, &max, "WEB-2").await.len(), 5, "switched off: no new runs");
    assert_eq!(send(&owner, Method::PUT, app.url("/admin/workspace"), json!({"features": {"agents": true}})).await.0 / 100, 2);
    assert_eq!(send(&owner, Method::PATCH, app.url(&format!("/admin/agents/{agent_id}")), json!({"state": "off"})).await.0, 204);
    post(&max, app.url("/issues/WEB-2/comments"), json!({"body": mention(&cody, "Cody", "anyone?")})).await;
    assert_eq!(runs(&app, &max, "WEB-2").await.len(), 5);
    assert!(get(&max, app.url("/people")).await.as_array().unwrap().iter().find(|p| p["name"] == "Cody").unwrap()["agent"].is_null());
    let runner_id = get(&owner, app.url("/admin/agents")).await["runners"][0]["id"].as_str().unwrap().to_string();
    assert_eq!(send(&owner, Method::DELETE, app.url(&format!("/admin/agent-runners/{runner_id}")), Value::Null).await.0, 204);
    assert!(runner.work_once().await.is_err());
    assert_eq!(send(&owner, Method::DELETE, app.url(&format!("/admin/agents/{agent_id}")), Value::Null).await.0, 204);
    assert!(!get(&max, app.url("/people")).await.as_array().unwrap().iter().any(|p| p["name"] == "Cody"));
    let kept = get(&max, app.url("/issues/WEB-1/comments")).await;
    assert!(kept.to_string().contains("it returns 1 again"), "what it wrote stays");
}
