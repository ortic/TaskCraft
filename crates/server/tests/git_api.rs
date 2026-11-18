//! Branches and draft pull requests from an issue, against mock Git servers that speak the
//! GitHub, GitLab and Gitea APIs: the token is tried when it's saved and never shown again, the
//! branch is linked before anything is pushed (and not doubled when the push arrives), who may
//! create branches is the project's to say, and pull requests show checks, reviews and reviewers
//! as the webhooks report them.

mod common;

use std::sync::{Arc, Mutex};

use axum::Json;
use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get as route_get, patch as route_patch, post as route_post};
use common::*;
use reqwest::Method;
use serde_json::{Value, json};

/// A Git server: what it was asked (method path, with who asked) and the branches it has.
#[derive(Default)]
struct Server {
    calls: Mutex<Vec<String>>,
    branches: Mutex<Vec<String>>,
    base: Mutex<String>,
}

type S = State<Arc<Server>>;
type Answer = (StatusCode, Json<Value>);

impl Server {
    fn note(&self, what: &str, h: &HeaderMap) -> bool {
        let auth = h.get("authorization").or_else(|| h.get("private-token")).and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
        self.calls.lock().unwrap().push(format!("{what} [{auth}]"));
        auth.contains("good-token")
    }
    fn add(&self, name: &str) -> bool {
        let mut b = self.branches.lock().unwrap();
        if b.iter().any(|x| x == name) {
            return false;
        }
        b.push(name.to_string());
        true
    }
    fn has(&self, name: &str) -> bool {
        self.branches.lock().unwrap().iter().any(|x| x == name)
    }
    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
}

fn denied() -> Answer {
    (StatusCode::UNAUTHORIZED, Json(json!({"message": "Bad credentials"})))
}

// GitHub (Enterprise layout: /api/v3) -------------------------------------------------------------

async fn gh_repo(State(s): S, h: HeaderMap) -> Answer {
    if !s.note("GET repo", &h) {
        return denied();
    }
    (StatusCode::OK, Json(json!({"default_branch": "main", "full_name": "acme/shop"})))
}

async fn gh_ref(State(s): S, h: HeaderMap, Path(branch): Path<String>) -> Answer {
    s.note(&format!("GET ref {branch}"), &h);
    if !s.has(&branch) {
        return (StatusCode::NOT_FOUND, Json(json!({"message": "Not Found"})));
    }
    (StatusCode::OK, Json(json!({"ref": format!("refs/heads/{branch}"), "object": {"sha": format!("sha-of-{}", branch.replace('/', "_"))}})))
}

async fn gh_create_ref(State(s): S, h: HeaderMap, Json(b): Json<Value>) -> Answer {
    s.note(&format!("POST ref {} {}", b["ref"].as_str().unwrap(), b["sha"].as_str().unwrap()), &h);
    if !s.add(b["ref"].as_str().unwrap().trim_start_matches("refs/heads/")) {
        return (StatusCode::UNPROCESSABLE_ENTITY, Json(json!({"message": "Reference already exists"})));
    }
    (StatusCode::CREATED, Json(b))
}

async fn gh_commit(State(s): S, h: HeaderMap, Path(sha): Path<String>) -> Answer {
    s.note(&format!("GET commit {sha}"), &h);
    (StatusCode::OK, Json(json!({"sha": sha, "tree": {"sha": "tree-1"}})))
}

async fn gh_create_commit(State(s): S, h: HeaderMap, Json(b): Json<Value>) -> Answer {
    s.note(&format!("POST commit {} tree={} parents={}", b["message"].as_str().unwrap(), b["tree"].as_str().unwrap(), b["parents"]), &h);
    (StatusCode::CREATED, Json(json!({"sha": "empty-commit"})))
}

async fn gh_update_ref(State(s): S, h: HeaderMap, Path(branch): Path<String>, Json(b): Json<Value>) -> Answer {
    s.note(&format!("PATCH ref {branch} {}", b["sha"].as_str().unwrap()), &h);
    (StatusCode::OK, Json(json!({"object": {"sha": b["sha"]}})))
}

async fn gh_pull(State(s): S, h: HeaderMap, Json(b): Json<Value>) -> Answer {
    s.note(&format!("POST pull {} <- {} draft={} {}", b["base"].as_str().unwrap(), b["head"].as_str().unwrap(), b["draft"], b["title"].as_str().unwrap()), &h);
    let base = s.base.lock().unwrap().clone();
    (StatusCode::CREATED, Json(json!({"number": 7, "html_url": format!("{base}/acme/shop/pull/7"), "draft": true})))
}

// GitLab (/api/v4) ---------------------------------------------------------------------------------

async fn gl_project(State(s): S, h: HeaderMap, Path(id): Path<String>) -> Answer {
    if !s.note(&format!("GET project {id}"), &h) {
        return denied();
    }
    (StatusCode::OK, Json(json!({"default_branch": "trunk", "path_with_namespace": id})))
}

async fn gl_branch(State(s): S, h: HeaderMap, Path(id): Path<String>, Query(q): Query<std::collections::HashMap<String, String>>) -> Answer {
    s.note(&format!("POST branch {id} {} from {}", q["branch"], q["ref"]), &h);
    if !s.has(&q["ref"]) {
        return (StatusCode::BAD_REQUEST, Json(json!({"message": "Invalid reference name: nope"})));
    }
    if !s.add(&q["branch"]) {
        return (StatusCode::BAD_REQUEST, Json(json!({"message": "Branch already exists"})));
    }
    (StatusCode::CREATED, Json(json!({"name": q["branch"]})))
}

async fn gl_mr(State(s): S, h: HeaderMap, Path(id): Path<String>, Json(b): Json<Value>) -> Answer {
    s.note(&format!("POST mr {id} {} <- {} {}", b["target_branch"].as_str().unwrap(), b["source_branch"].as_str().unwrap(), b["title"].as_str().unwrap()), &h);
    let base = s.base.lock().unwrap().clone();
    (StatusCode::CREATED, Json(json!({"iid": 3, "web_url": format!("{base}/acme/platform/shop/-/merge_requests/3")})))
}

// Gitea / Forgejo (/api/v1) ----------------------------------------------------------------------

async fn gt_repo(State(s): S, h: HeaderMap) -> Answer {
    if !s.note("GET gitea repo", &h) {
        return denied();
    }
    (StatusCode::OK, Json(json!({"default_branch": "main"})))
}

async fn gt_branch(State(s): S, h: HeaderMap, Json(b): Json<Value>) -> Answer {
    s.note(&format!("POST gitea branch {} from {}", b["new_branch_name"].as_str().unwrap(), b["old_branch_name"].as_str().unwrap()), &h);
    if !s.add(b["new_branch_name"].as_str().unwrap()) {
        return (StatusCode::CONFLICT, Json(json!({"message": "The branch already exists."})));
    }
    (StatusCode::CREATED, Json(json!({"name": b["new_branch_name"]})))
}

async fn gt_pull(State(s): S, h: HeaderMap) -> Answer {
    s.note("POST gitea pull", &h);
    (StatusCode::UNPROCESSABLE_ENTITY, Json(json!({"message": "There are no changes between the head and the base"})))
}

async fn git_server() -> (Arc<Server>, String) {
    let server = Arc::new(Server::default());
    for b in ["main", "trunk", "release/1.x"] {
        server.add(b);
    }
    let app = Router::new()
        .route("/api/v3/repos/acme/shop", route_get(gh_repo))
        .route("/api/v3/repos/acme/shop/git/ref/heads/{*branch}", route_get(gh_ref))
        .route("/api/v3/repos/acme/shop/git/refs", route_post(gh_create_ref))
        .route("/api/v3/repos/acme/shop/git/commits/{sha}", route_get(gh_commit))
        .route("/api/v3/repos/acme/shop/git/commits", route_post(gh_create_commit))
        .route("/api/v3/repos/acme/shop/git/refs/heads/{*branch}", route_patch(gh_update_ref))
        .route("/api/v3/repos/acme/shop/pulls", route_post(gh_pull))
        .route("/api/v4/projects/{id}", route_get(gl_project))
        .route("/api/v4/projects/{id}/repository/branches", route_post(gl_branch))
        .route("/api/v4/projects/{id}/merge_requests", route_post(gl_mr))
        .route("/api/v1/repos/acme/tools", route_get(gt_repo))
        .route("/api/v1/repos/acme/tools/branches", route_post(gt_branch))
        .route("/api/v1/repos/acme/tools/pulls", route_post(gt_pull))
        .with_state(server.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    *server.base.lock().unwrap() = base.clone();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (server, base)
}

async fn dev(app: &TestApp, c: &reqwest::Client, key: &str) -> Value {
    get(c, app.url(&format!("/issues/{key}/development"))).await
}

/// Delivers a GitHub event signed with `secret`.
async fn github(url: &str, event: &str, secret: &str, payload: &Value) -> (u16, Value) {
    let body = serde_json::to_vec(payload).unwrap();
    let r = reqwest::Client::new()
        .post(url)
        .header("content-type", "application/json")
        .header("x-github-event", event)
        .header("x-hub-signature-256", taskcraft::webhooks::sign(secret, &body))
        .body(body)
        .send()
        .await
        .unwrap();
    (r.status().as_u16(), r.json().await.unwrap_or(Value::Null))
}

#[tokio::test(flavor = "multi_thread")]
async fn github_branch_draft_pull_request_and_live_state() {
    let (git, base) = git_server().await;
    let app = spawn().await;
    let owner = app.setup(false).await;
    post(&owner, app.url("/projects"), json!({"key": "WEB", "name": "Web shop", "template": "scrum"})).await;
    let (max, _) = app.user(&owner, "Max Member", "max@example.com", "member").await;
    post(&owner, app.url("/issues"), json!({"projectKey": "WEB", "title": "Login times out"})).await;
    post(&owner, app.url("/issues"), json!({"projectKey": "WEB", "title": "Checkout"})).await;
    let p = get(&owner, app.url("/projects/WEB")).await;
    let doing = p["statuses"].as_array().unwrap().iter().find(|s| s["name"] == "In progress").unwrap()["id"].clone();
    assert_eq!(send(&owner, Method::PUT, app.url("/projects/WEB/git/rules"), json!({"openedStatusId": doing, "mergedStatusId": null})).await.0, 204);

    // a connected repository without a token: nothing to create branches in
    let hook = post(&owner, app.url("/projects/WEB/git"), json!({"provider": "github", "repoUrl": format!("{base}/acme/shop")})).await;
    let (hook_id, secret, hook_url) =
        (hook["id"].as_str().unwrap().to_string(), hook["secret"].as_str().unwrap().to_string(), hook["webhookUrl"].as_str().unwrap().to_string());
    assert!(hook["api"].is_null());
    assert_eq!(dev(&app, &max, "WEB-1").await["repositories"], json!([]));
    let (s, v) = send(&max, Method::POST, app.url("/issues/WEB-1/branch"), json!({})).await;
    assert!(s == 400 && v["message"].as_str().unwrap().contains("no repository"), "{v}");

    // the token is tried when it's saved (project admins), and not shown again
    let api = app.url(&format!("/git-integrations/{hook_id}/api"));
    assert_eq!(send(&max, Method::PUT, api.clone(), json!({"token": "good-token"})).await.0, 403);
    let (s, v) = send(&owner, Method::PUT, api.clone(), json!({"token": "wrong"})).await;
    assert!(s == 400 && v["message"].as_str().unwrap().contains("token isn't accepted"), "{v}");
    let (s, v) = send(&owner, Method::PUT, api.clone(), json!({"token": "good-token", "kind": "svn"})).await;
    assert_eq!(s, 400, "{v}");
    let (s, v) = send(&owner, Method::PUT, api.clone(), json!({"token": "good-token"})).await;
    assert_eq!((s, v["api"]["kind"].as_str(), v["api"]["defaultBranch"].as_str()), (200, Some("github"), Some("main")), "{v}");
    let settings = get(&owner, app.url("/projects/WEB/git")).await;
    assert!(!settings.to_string().contains("good-token"), "the token isn't shown again");
    let d = dev(&app, &max, "WEB-1").await;
    assert_eq!((d["repositories"][0]["repo"].as_str(), d["repositories"][0]["defaultBranch"].as_str()), (Some("acme/shop"), Some("main")), "{d}");
    assert_eq!(d["branchName"], "web-1-login-times-out");

    // the branch, from the default branch, linked at once; the "work started" rule moves the issue
    let (s, v) = send(&max, Method::POST, app.url("/issues/WEB-1/branch"), json!({})).await;
    assert_eq!(s, 201, "{v}");
    assert_eq!(v["branch"], json!({"name": "web-1-login-times-out", "url": format!("{base}/acme/shop/tree/web-1-login-times-out"), "base": "main"}));
    assert!(git.calls().iter().any(|c| c == "POST ref refs/heads/web-1-login-times-out sha-of-main [Bearer good-token]"), "{:?}", git.calls());
    let d = dev(&app, &max, "WEB-1").await;
    assert_eq!(
        (d["links"].as_array().unwrap().len(), d["links"][0]["kind"].as_str(), d["links"][0]["author"].as_str()),
        (1, Some("branch"), Some("Max Member")),
        "{d}"
    );
    assert_eq!(get(&max, app.url("/issues/WEB-1")).await["status"]["name"], "In progress");
    let history = get(&max, app.url("/issues/WEB-1/history")).await;
    let entry = history.as_array().unwrap().iter().find(|e| e["kind"] == "dev").expect("in the history");
    assert_eq!((entry["actor"]["name"].as_str(), entry["field"].as_str()), (Some("Max Member"), Some("branch")), "{entry}");
    // the push arriving later is the same branch, not a second link
    let pushed = json!({"ref": "refs/heads/web-1-login-times-out", "after": "f00", "repository": {"html_url": format!("{base}/acme/shop")}, "pusher": {"name": "max"}, "commits": []});
    assert_eq!(github(&hook_url, "push", &secret, &pushed).await.0, 200);
    assert_eq!(dev(&app, &max, "WEB-1").await["links"].as_array().unwrap().len(), 1);

    // twice, bad names, an unknown base
    let (s, v) = send(&max, Method::POST, app.url("/issues/WEB-1/branch"), json!({})).await;
    assert!(s == 409 && v["message"].as_str().unwrap().contains("exists already"), "{s} {v}");
    for bad in ["has space", "a..b", "-x", "x.lock"] {
        assert_eq!(send(&max, Method::POST, app.url("/issues/WEB-1/branch"), json!({"name": bad})).await.0, 400, "{bad}");
    }
    let (s, v) = send(&max, Method::POST, app.url("/issues/WEB-2/branch"), json!({"base": "nope"})).await;
    assert!(s == 400 && v["message"].as_str().unwrap().contains("no branch nope"), "{v}");

    // a name and base of one's own, with a draft pull request: GitHub needs a commit for that
    let (s, v) =
        send(&max, Method::POST, app.url("/issues/WEB-2/branch"), json!({"name": "feature/web-2-checkout", "base": "release/1.x", "pullRequest": true})).await;
    assert_eq!(s, 201, "{v}");
    assert_eq!(v["pullRequest"], json!({"number": 7, "url": format!("{base}/acme/shop/pull/7")}));
    assert!(v["warning"].is_null());
    let calls = git.calls();
    for want in [
        "GET ref release/1.x [Bearer good-token]",
        "POST ref refs/heads/feature/web-2-checkout sha-of-release_1.x [Bearer good-token]",
        "POST commit WEB-2: start work tree=tree-1 parents=[\"sha-of-feature_web-2-checkout\"] [Bearer good-token]",
        "PATCH ref feature/web-2-checkout empty-commit [Bearer good-token]",
        "POST pull release/1.x <- feature/web-2-checkout draft=true WEB-2 Checkout [Bearer good-token]",
    ] {
        assert!(calls.iter().any(|c| c == want), "{want} in {calls:#?}");
    }
    let d = dev(&app, &max, "WEB-2").await;
    let pr = &d["links"][0];
    assert_eq!(
        (pr["kind"].as_str(), pr["number"].as_i64(), pr["state"].as_str(), pr["draft"].as_bool()),
        (Some("pull_request"), Some(7), Some("open"), Some(true)),
        "{pr}"
    );

    // the pull request's life, as the webhooks tell it: ready, reviewers, checks, reviews
    let pr_event = |draft: bool, reviewers: Value| {
        json!({"action": "edited", "number": 7, "repository": {"html_url": format!("{base}/acme/shop")},
               "pull_request": {"html_url": format!("{base}/acme/shop/pull/7"), "number": 7, "title": "WEB-2 Checkout", "body": "", "state": "open", "merged": false, "draft": draft,
                                "user": {"login": "max"}, "head": {"ref": "feature/web-2-checkout"}, "requested_reviewers": reviewers, "updated_at": "2026-11-18T12:00:00Z"}})
    };
    assert_eq!(github(&hook_url, "pull_request", &secret, &pr_event(false, json!([{"login": "ann"}, {"login": "tom"}]))).await.0, 200);
    let pr = dev(&app, &max, "WEB-2").await["links"][0].clone();
    assert_eq!(pr["draft"], false);
    assert_eq!(pr["reviewers"], json!([{"name": "ann", "state": "requested"}, {"name": "tom", "state": "requested"}]));
    assert!(pr["review"].is_null() && pr["checks"].is_null());
    let (s, v) = github(&hook_url, "check_suite", &secret, &json!({"action": "requested", "check_suite": {"status": "queued", "conclusion": null, "head_branch": "feature/web-2-checkout", "pull_requests": [{"number": 7}]}})).await;
    assert_eq!((s, v["updated"].as_i64()), (200, Some(1)), "{v}");
    assert_eq!(dev(&app, &max, "WEB-2").await["links"][0]["checks"], "pending");
    // a commit status names only the branch
    assert_eq!(github(&hook_url, "status", &secret, &json!({"state": "failure", "sha": "abc", "branches": [{"name": "feature/web-2-checkout"}]})).await.0, 200);
    assert_eq!(dev(&app, &max, "WEB-2").await["links"][0]["checks"], "failure");
    let review = |who: &str, state: &str| json!({"action": "submitted", "review": {"state": state, "user": {"login": who}}, "pull_request": {"number": 7}});
    github(&hook_url, "pull_request_review", &secret, &review("ann", "approved")).await;
    let pr = dev(&app, &max, "WEB-2").await["links"][0].clone();
    assert_eq!(pr["review"], "approved");
    assert_eq!(pr["reviewers"], json!([{"name": "tom", "state": "requested"}, {"name": "ann", "state": "approved"}]));
    github(&hook_url, "pull_request_review", &secret, &review("tom", "changes_requested")).await;
    assert_eq!(dev(&app, &max, "WEB-2").await["links"][0]["review"], "changes_requested");
    github(&hook_url, "pull_request_review", &secret, &review("tom", "approved")).await;
    github(
        &hook_url,
        "check_suite",
        &secret,
        &json!({"action": "completed", "check_suite": {"status": "completed", "conclusion": "success", "pull_requests": [{"number": 7}]}}),
    )
    .await;
    let pr = dev(&app, &max, "WEB-2").await["links"][0].clone();
    assert_eq!((pr["review"].as_str(), pr["checks"].as_str()), (Some("approved"), Some("success")), "{pr}");
    // other events are still ignored
    assert_eq!(github(&hook_url, "star", &secret, &json!({})).await.1["ignored"], "star");

    // who may create branches is the project's to say; viewers never do
    patch(&owner, app.url("/projects/WEB"), json!({"settings": {"permissions": {"createBranches": "admin"}}})).await;
    post(&owner, app.url("/issues"), json!({"projectKey": "WEB", "title": "Third"})).await;
    let (s, v) = send(&max, Method::POST, app.url("/issues/WEB-3/branch"), json!({})).await;
    assert!(s == 403 && v["message"].as_str().unwrap().contains("project admins"), "{v}");
    assert_eq!(dev(&app, &max, "WEB-3").await["repositories"], json!([]));
    assert_eq!(send(&owner, Method::POST, app.url("/issues/WEB-3/branch"), json!({})).await.0, 201);

    // without the token nothing can be created any more
    let (s, v) = send(&owner, Method::PUT, api, json!({"token": null})).await;
    assert!(s == 200 && v["api"].is_null(), "{v}");
    assert_eq!(dev(&app, &owner, "WEB-3").await["repositories"], json!([]));
}

#[tokio::test(flavor = "multi_thread")]
async fn gitlab_and_gitea() {
    let (git, base) = git_server().await;
    let app = spawn().await;
    let owner = app.setup(false).await;
    post(&owner, app.url("/projects"), json!({"key": "WEB", "name": "Web shop", "template": "scrum"})).await;
    post(&owner, app.url("/issues"), json!({"projectKey": "WEB", "title": "Größere Bilder"})).await;

    let lab = post(&owner, app.url("/projects/WEB/git"), json!({"provider": "gitlab", "repoUrl": format!("{base}/acme/platform/shop")})).await;
    let tea = post(&owner, app.url("/projects/WEB/git"), json!({"provider": "github", "repoUrl": format!("{base}/acme/tools")})).await;
    let (s, v) = send(&owner, Method::PUT, app.url(&format!("/git-integrations/{}/api", lab["id"].as_str().unwrap())), json!({"token": "good-token"})).await;
    assert_eq!((s, v["api"]["kind"].as_str(), v["api"]["defaultBranch"].as_str()), (200, Some("gitlab"), Some("trunk")), "{v}");
    let (s, v) =
        send(&owner, Method::PUT, app.url(&format!("/git-integrations/{}/api", tea["id"].as_str().unwrap())), json!({"token": "good-token", "kind": "gitea"}))
            .await;
    assert_eq!((s, v["api"]["kind"].as_str()), (200, Some("gitea")), "{v}");

    // two repositories: say which
    let (s, v) = send(&owner, Method::POST, app.url("/issues/WEB-1/branch"), json!({})).await;
    assert!(s == 400 && v["message"].as_str().unwrap().contains("several repositories"), "{v}");
    assert_eq!(get(&owner, app.url("/issues/WEB-1/development")).await["repositories"].as_array().unwrap().len(), 2);

    // GitLab: the branch and a draft merge request
    let (s, v) = send(&owner, Method::POST, app.url("/issues/WEB-1/branch"), json!({"integrationId": lab["id"], "pullRequest": true})).await;
    assert_eq!(s, 201, "{v}");
    assert_eq!(v["branch"]["url"], format!("{base}/acme/platform/shop/-/tree/web-1-groessere-bilder"));
    assert_eq!(v["pullRequest"], json!({"number": 3, "url": format!("{base}/acme/platform/shop/-/merge_requests/3")}));
    let calls = git.calls();
    assert!(calls.iter().any(|c| c == "POST branch acme/platform/shop web-1-groessere-bilder from trunk [good-token]"), "{calls:#?}");
    assert!(calls.iter().any(|c| c == "POST mr acme/platform/shop trunk <- web-1-groessere-bilder Draft: WEB-1 Größere Bilder [good-token]"), "{calls:#?}");
    // its pipeline and an approval arrive by webhook
    let deliver = |payload: Value, event: &'static str| {
        let (url, secret) = (lab["webhookUrl"].as_str().unwrap().to_string(), lab["secret"].as_str().unwrap().to_string());
        async move {
            let r = reqwest::Client::new().post(url).header("x-gitlab-event", event).header("x-gitlab-token", secret).json(&payload).send().await.unwrap();
            assert_eq!(r.status(), 200, "{event}");
        }
    };
    deliver(
        json!({"object_kind": "pipeline", "object_attributes": {"status": "running", "ref": "web-1-groessere-bilder"}, "merge_request": {"iid": 3}}),
        "Pipeline Hook",
    )
    .await;
    deliver(
        json!({"object_kind": "merge_request", "user": {"name": "Ann Approver"}, "project": {"web_url": format!("{base}/acme/platform/shop")}, "reviewers": [{"name": "Ann Approver"}],
               "object_attributes": {"iid": 3, "action": "approved", "state": "opened", "draft": false, "title": "WEB-1 Größere Bilder", "description": "",
                                     "url": format!("{base}/acme/platform/shop/-/merge_requests/3"), "source_branch": "web-1-groessere-bilder"}}),
        "Merge Request Hook",
    )
    .await;
    let d = get(&owner, app.url("/issues/WEB-1/development")).await;
    let mr = d["links"].as_array().unwrap().iter().find(|l| l["kind"] == "pull_request").unwrap();
    assert_eq!((mr["checks"].as_str(), mr["review"].as_str(), mr["draft"].as_bool()), (Some("pending"), Some("approved"), Some(false)), "{mr}");
    assert_eq!(mr["reviewers"], json!([{"name": "Ann Approver", "state": "approved"}]));

    // Gitea: the branch is made; the server refuses a pull request without changes, and we say so
    let (s, v) =
        send(&owner, Method::POST, app.url("/issues/WEB-1/branch"), json!({"integrationId": tea["id"], "name": "web-1-bilder", "pullRequest": true})).await;
    assert_eq!(s, 201, "{v}");
    assert_eq!(v["branch"]["url"], format!("{base}/acme/tools/tree/web-1-bilder"));
    assert!(v["pullRequest"].is_null());
    assert!(v["warning"].as_str().unwrap().contains("no changes"), "{v}");
    assert!(git.calls().iter().any(|c| c == "POST gitea branch web-1-bilder from main [token good-token]"), "{:#?}", git.calls());
    let (s, _) = send(&owner, Method::POST, app.url("/issues/WEB-1/branch"), json!({"integrationId": tea["id"], "name": "web-1-bilder"})).await;
    assert_eq!(s, 409);
}
