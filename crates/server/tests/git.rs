//! Git integration: webhook deliveries from GitHub, GitLab and the generic format link commits,
//! branches and pull requests to issues by key and move issues by the project's rules.

mod common;

use common::*;
use reqwest::Method;
use serde_json::{Value, json};

fn status_id(p: &Value, name: &str) -> String {
    p["statuses"].as_array().unwrap().iter().find(|s| s["name"] == name).unwrap()["id"].as_str().unwrap().to_string()
}

/// Delivers a GitHub event signed with `secret`: (status, body).
async fn github(url: &str, event: &str, secret: Option<&str>, payload: &Value) -> (u16, Value) {
    let body = serde_json::to_vec(payload).unwrap();
    let mut req = reqwest::Client::new().post(url).header("content-type", "application/json").header("x-github-event", event);
    if let Some(s) = secret {
        req = req.header("x-hub-signature-256", taskcraft::webhooks::sign(s, &body));
    }
    let r = req.body(body).send().await.unwrap();
    let status = r.status().as_u16();
    (status, r.json().await.unwrap_or(Value::Null))
}

fn push(branch: &str, messages: &[(&str, &str)]) -> Value {
    json!({
        "ref": format!("refs/heads/{branch}"), "after": "f00", "repository": {"html_url": "https://github.com/acme/shop", "full_name": "acme/shop"},
        "pusher": {"name": "max"},
        "commits": messages.iter().map(|(sha, m)| json!({
            "id": sha, "message": m, "url": format!("https://github.com/acme/shop/commit/{sha}"),
            "author": {"name": "Max Member", "email": "max@example.com"}, "timestamp": "2026-11-12T10:00:00+01:00"
        })).collect::<Vec<_>>()
    })
}

fn pull_request(number: i64, title: &str, body: &str, branch: &str, state: &str, merged: bool) -> Value {
    json!({
        "action": if state == "open" { "opened" } else { "closed" }, "number": number,
        "repository": {"html_url": "https://github.com/acme/shop"},
        "pull_request": {
            "html_url": format!("https://github.com/acme/shop/pull/{number}"), "number": number, "title": title, "body": body,
            "state": state, "merged": merged, "user": {"login": "max"}, "head": {"ref": branch}, "base": {"ref": "main"},
            "updated_at": "2026-11-12T12:00:00Z"
        }
    })
}

async fn dev(app: &TestApp, c: &reqwest::Client, key: &str) -> Value {
    get(c, app.url(&format!("/issues/{key}/development"))).await
}

async fn status(app: &TestApp, c: &reqwest::Client, key: &str) -> String {
    get(c, app.url(&format!("/issues/{key}"))).await["status"]["name"].as_str().unwrap().to_string()
}

#[tokio::test(flavor = "multi_thread")]
async fn github_links_and_moves_issues() {
    let app = spawn().await;
    let owner = app.setup(false).await;
    post(&owner, app.url("/projects"), json!({"key": "WEB", "name": "Web shop", "template": "scrum"})).await;
    post(&owner, app.url("/issues"), json!({"projectKey": "WEB", "title": "Login times out"})).await;
    post(&owner, app.url("/issues"), json!({"projectKey": "WEB", "title": "Checkout"})).await;
    let p = get(&owner, app.url("/projects/WEB")).await;

    // connect a repository: the secret is shown once
    let hook = post(&owner, app.url("/projects/WEB/git"), json!({"provider": "github", "repoUrl": "https://github.com/acme/shop.git"})).await;
    let secret = hook["secret"].as_str().unwrap().to_string();
    let url = hook["webhookUrl"].as_str().unwrap().to_string();
    assert!(url.starts_with(&format!("{}/api/integrations/git/github/", app.base)), "{url}");
    assert_eq!(hook["repoUrl"], "https://github.com/acme/shop");
    let settings = get(&owner, app.url("/projects/WEB/git")).await;
    assert_eq!(settings["integrations"][0]["webhookUrl"], url);
    assert!(settings["integrations"][0].get("secret").is_none(), "the secret isn't shown again");
    let (s, _) = send(
        &owner,
        Method::PUT,
        app.url("/projects/WEB/git/rules"),
        json!({"openedStatusId": status_id(&p, "In progress"), "mergedStatusId": status_id(&p, "Done")}),
    )
    .await;
    assert_eq!(s, 204);
    let (member, _) = app.user(&owner, "Max Member", "max@example.com", "member").await;
    let (s, _) = send(&member, Method::GET, app.url("/projects/WEB/git"), Value::Null).await;
    assert_eq!(s, 403, "repositories are project-admin business");
    assert_eq!(dev(&app, &member, "WEB-1").await["connected"], true);

    // authenticity: GitHub signs every delivery with the secret
    let (s, v) = github(&url, "ping", Some(&secret), &json!({"zen": "Practicality beats purity."})).await;
    assert_eq!(s, 200, "{v}");
    let payload = push("feature/web-1-login-timeout", &[("c1", "WEB-2: prepare checkout"), ("c2", "fix typo"), ("c3", "web-1 and XWEB-2 and OPS-5")]);
    let (s, _) = github(&url, "push", Some("wrong secret"), &payload).await;
    assert_eq!(s, 401);
    let (s, _) = github(&url, "push", None, &payload).await;
    assert_eq!(s, 401);
    let settings = get(&owner, app.url("/projects/WEB/git")).await;
    assert!(settings["integrations"][0]["lastError"].as_str().unwrap().contains("signature"), "{settings}");
    let (s, _) = github(&format!("{url}x"), "push", Some(&secret), &payload).await;
    assert_eq!(s, 404);
    let (s, _) = github(&url.replace("/github/", "/gitlab/"), "push", Some(&secret), &payload).await;
    assert_eq!(s, 404, "the provider is part of the address");

    // a push: the branch and the commits that mention keys
    let (s, out) = github(&url, "push", Some(&secret), &payload).await;
    assert_eq!(s, 200, "{out}");
    assert_eq!(out["issues"], json!(["WEB-1", "WEB-2"]));
    assert_eq!(out["moved"], json!(["WEB-1"]), "a pushed branch moves its issue to In progress; commits don't");
    let d = dev(&app, &member, "WEB-1").await;
    let kinds: Vec<&str> = d["links"].as_array().unwrap().iter().map(|l| l["kind"].as_str().unwrap()).collect();
    assert_eq!(kinds, ["branch", "commit"], "{d}");
    assert_eq!(d["links"][0]["url"], "https://github.com/acme/shop/tree/feature/web-1-login-timeout");
    assert_eq!(d["links"][1]["sha"], "c3");
    assert_eq!(d["links"][1]["author"], "Max Member");
    assert_eq!(d["links"][1]["repo"], "acme/shop");
    assert_eq!(d["branchName"], "web-1-login-times-out");
    assert_eq!(dev(&app, &member, "WEB-2").await["links"].as_array().unwrap().len(), 1);
    assert_eq!(status(&app, &owner, "WEB-1").await, "In progress");
    assert_eq!(status(&app, &owner, "WEB-2").await, "To do");
    let history = get(&owner, app.url("/issues/WEB-1/history")).await;
    let h = history.as_array().unwrap();
    assert!(h.iter().any(|e| e["kind"] == "dev" && e["field"] == "branch" && e["actor"].is_null()), "{history}");
    assert!(h.iter().any(|e| e["kind"] == "dev" && e["field"] == "commit" && e["new"]["count"] == 1));
    let moved = h.iter().find(|e| e["field"] == "status").unwrap();
    assert!(moved["actor"].is_null(), "rules act as Automation");

    // the same delivery again changes nothing (one link per address)
    let (_, out) = github(&url, "push", Some(&secret), &payload).await;
    assert_eq!(out["newLinks"], 0);
    assert_eq!(dev(&app, &member, "WEB-1").await["links"].as_array().unwrap().len(), 2);

    // a pull request: opened, then merged
    let (s, out) =
        github(&url, "pull_request", Some(&secret), &pull_request(5, "Fix the login timeout", "Closes WEB-1", "feature/web-1-login-timeout", "open", false))
            .await;
    assert_eq!((s, out["newLinks"].as_u64()), (200, Some(1)), "{out}");
    let d = dev(&app, &member, "WEB-1").await;
    assert_eq!(
        (d["links"][0]["kind"].as_str(), d["links"][0]["state"].as_str(), d["links"][0]["number"].as_i64()),
        (Some("pull_request"), Some("open"), Some(5))
    );
    let card = &get(&owner, app.url("/search?q=key%20%3D%20WEB-1")).await["issues"][0];
    assert_eq!(card["pullRequest"], "open", "lists and boards show the pull request's state");
    assert!(get(&owner, app.url("/issues/WEB-2")).await.get("pullRequest").is_none());
    let (_, out) =
        github(&url, "pull_request", Some(&secret), &pull_request(5, "Fix the login timeout", "Closes WEB-1", "feature/web-1-login-timeout", "closed", true))
            .await;
    assert_eq!(out["moved"], json!(["WEB-1"]), "{out}");
    assert_eq!(dev(&app, &member, "WEB-1").await["links"][0]["state"], "merged");
    assert_eq!(status(&app, &owner, "WEB-1").await, "Done");
    let history = get(&owner, app.url("/issues/WEB-1/history")).await;
    assert!(
        history
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["kind"] == "dev" && e["field"] == "pull_request" && e["new"]["state"] == "merged" && e["old"]["state"] == "open")
    );

    // rules only move forward: a new branch for a finished issue leaves it done
    let (_, out) = github(&url, "push", Some(&secret), &push("web-1-follow-up", &[])).await;
    assert_eq!(out["moved"], json!([]));
    assert_eq!(status(&app, &owner, "WEB-1").await, "Done");

    // restricted workflows: only allowed transitions (To do → Done isn't one)
    let (todo, doing) = (status_id(&p, "To do"), status_id(&p, "In progress"));
    let (s, _) =
        send(&owner, Method::PUT, app.url("/projects/WEB/transitions"), json!({"workflow": "restricted", "transitions": [{"from": todo, "to": doing}]})).await;
    assert_eq!(s, 204);
    let (_, out) = github(&url, "pull_request", Some(&secret), &pull_request(6, "WEB-2 checkout", "", "main-fix", "closed", true)).await;
    assert_eq!(out["issues"], json!(["WEB-2"]));
    assert_eq!(out["moved"], json!([]), "{out}");
    assert_eq!(status(&app, &owner, "WEB-2").await, "To do");
    assert_eq!(dev(&app, &member, "WEB-2").await["links"][0]["state"], "merged");

    // disconnecting stops deliveries; the links stay
    let id = hook["id"].as_str().unwrap();
    let (s, _) = send(&owner, Method::DELETE, app.url(&format!("/git-integrations/{id}")), Value::Null).await;
    assert_eq!(s, 204);
    let (s, _) = github(&url, "push", Some(&secret), &payload).await;
    assert_eq!(s, 404);
    assert_eq!(dev(&app, &member, "WEB-1").await["links"].as_array().unwrap().len(), 4);
    assert_eq!(dev(&app, &member, "WEB-1").await["connected"], false);
}

#[tokio::test(flavor = "multi_thread")]
async fn gitlab_generic_former_keys_and_secrets() {
    let app = spawn().await;
    let owner = app.setup(false).await;
    post(&owner, app.url("/projects"), json!({"key": "OPS", "name": "Operations", "template": "kanban"})).await;
    post(&owner, app.url("/projects"), json!({"key": "OTHER", "name": "Other", "template": "kanban"})).await;
    post(&owner, app.url("/issues"), json!({"projectKey": "OPS", "title": "Rotate the certificates"})).await;
    post(&owner, app.url("/issues"), json!({"projectKey": "OTHER", "title": "Not ours"})).await;
    // the project is renamed: commits may still say OPS-1
    let (s, v) = send(&owner, Method::PATCH, app.url("/projects/OPS"), json!({"key": "INFRA"})).await;
    assert_eq!(s, 200, "{v}");
    let c = reqwest::Client::new();

    // GitLab: X-Gitlab-Token
    let hook = post(&owner, app.url("/projects/INFRA/git"), json!({"provider": "gitlab", "repoUrl": "https://gitlab.example.com/ops/infra"})).await;
    let (url, secret) = (hook["webhookUrl"].as_str().unwrap().to_string(), hook["secret"].as_str().unwrap().to_string());
    let push = json!({
        "object_kind": "push", "ref": "refs/heads/main", "before": "a", "after": "b", "user_name": "Max",
        "project": {"web_url": "https://gitlab.example.com/ops/infra"},
        "commits": [
            {"id": "g1", "message": "ops-1: new certificates", "url": "https://gitlab.example.com/ops/infra/-/commit/g1", "author": {"name": "Max"}, "timestamp": "2026-11-12T10:00:00+00:00"},
            {"id": "g2", "message": "OTHER-1 isn't this project's", "url": "https://gitlab.example.com/ops/infra/-/commit/g2", "author": {"name": "Max"}}
        ]
    });
    let r = c.post(&url).header("x-gitlab-event", "Push Hook").header("x-gitlab-token", "nope").json(&push).send().await.unwrap();
    assert_eq!(r.status(), 401);
    let r = c.post(&url).header("x-gitlab-event", "Push Hook").header("x-gitlab-token", &secret).json(&push).send().await.unwrap();
    assert_eq!(r.status(), 200);
    let out: Value = r.json().await.unwrap();
    assert_eq!(out["issues"], json!(["INFRA-1"]), "former keys link, other projects' keys don't: {out}");
    assert!(get(&owner, app.url("/issues/OTHER-1/development")).await["links"].as_array().unwrap().is_empty());
    let mr = json!({
        "object_kind": "merge_request", "user": {"name": "Max"}, "project": {"web_url": "https://gitlab.example.com/ops/infra"},
        "object_attributes": {"iid": 3, "title": "Certificates", "description": "For INFRA-1", "state": "merged", "url": "https://gitlab.example.com/ops/infra/-/merge_requests/3", "source_branch": "certs", "updated_at": "2026-11-12 11:00:00 UTC"}
    });
    let r = c.post(&url).header("x-gitlab-event", "Merge Request Hook").header("x-gitlab-token", &secret).json(&mr).send().await.unwrap();
    assert_eq!(r.status(), 200);
    let d = get(&owner, app.url("/issues/INFRA-1/development")).await;
    assert_eq!(d["links"][0]["kind"], "pull_request");
    assert_eq!(d["links"][0]["state"], "merged");
    assert_eq!(d["links"][1]["title"], "ops-1: new certificates");

    // generic: a token header or an HMAC signature
    let hook = post(&owner, app.url("/projects/INFRA/git"), json!({"provider": "generic", "repoUrl": "https://git.example.com/infra"})).await;
    let (url, secret) = (hook["webhookUrl"].as_str().unwrap().to_string(), hook["secret"].as_str().unwrap().to_string());
    let body = json!({
        "commits": [{"sha": "x1", "message": "INFRA-1 renew", "author": "Ann"}],
        "branch": {"name": "infra-1-renew"},
        "pullRequest": {"title": "Renew", "body": "INFRA-1", "url": "https://git.example.com/infra/pulls/9", "state": "open", "number": 9, "author": "Ann"}
    });
    let r = c.post(&url).json(&body).send().await.unwrap();
    assert_eq!(r.status(), 401);
    let r = c.post(&url).header("x-taskcraft-token", &secret).json(&body).send().await.unwrap();
    assert_eq!(r.status(), 200);
    let out: Value = r.json().await.unwrap();
    assert_eq!(out["newLinks"], 3, "{out}");
    let signed = serde_json::to_vec(&json!({"commits": [{"sha": "x2", "message": "INFRA-1 more", "url": "https://git.example.com/infra/commit/x2"}]})).unwrap();
    let r = c
        .post(&url)
        .header("content-type", "application/json")
        .header("x-hub-signature-256", taskcraft::webhooks::sign(&secret, &signed))
        .body(signed)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let bad = json!({"pullRequest": {"title": "INFRA-1", "url": "javascript:alert(1)"}});
    let r = c.post(&url).header("x-taskcraft-token", &secret).json(&bad).send().await.unwrap();
    assert_eq!(r.status(), 400, "only web addresses become links");
    let d = get(&owner, app.url("/issues/INFRA-1/development")).await;
    let urls: Vec<&str> = d["links"].as_array().unwrap().iter().map(|l| l["url"].as_str().unwrap()).collect();
    assert!(urls.contains(&"https://git.example.com/infra/commit/x1") && urls.contains(&"https://git.example.com/infra/tree/infra-1-renew"), "{urls:?}");
    assert!(!urls.iter().any(|u| u.starts_with("javascript")));

    // a new secret replaces the old one and the address
    let id = hook["id"].as_str().unwrap();
    let rotated = post(&owner, app.url(&format!("/git-integrations/{id}/secret")), json!({})).await;
    assert_ne!(rotated["secret"], json!(secret));
    let r = c.post(&url).header("x-taskcraft-token", &secret).json(&body).send().await.unwrap();
    assert_eq!(r.status(), 404);
    let r = c.post(rotated["webhookUrl"].as_str().unwrap()).header("x-taskcraft-token", rotated["secret"].as_str().unwrap()).json(&body).send().await.unwrap();
    assert_eq!(r.status(), 200);
    // the history shows what was linked
    let history = get(&owner, app.url("/issues/INFRA-1/history")).await;
    assert!(history.as_array().unwrap().iter().filter(|e| e["kind"] == "dev").count() >= 4);
}
