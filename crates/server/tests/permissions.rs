//! Project permissions: who may delete and move issues, plan sprints, manage releases, moderate
//! comments and log time for others.

mod common;

use common::*;
use reqwest::Method;
use serde_json::json;

#[tokio::test]
async fn projects_choose_who_may_do_what() {
    let app = spawn().await;
    let admin = app.setup(false).await;
    post(&admin, app.url("/projects"), json!({"key": "PERM", "name": "Permissions", "template": "scrum"})).await;
    let (mia, mia_id) = app.user(&admin, "Mia Member", "mia@example.com", "member").await;
    let issue = |c: &reqwest::Client, title: &str| {
        let (c, url, title) = (c.clone(), app.url("/issues"), title.to_string());
        async move { post(&c, url, json!({"projectKey": "PERM", "title": title})).await["key"].as_str().unwrap().to_string() }
    };
    let set = |perms: serde_json::Value| {
        let (c, url) = (admin.clone(), app.url("/projects/PERM"));
        async move { patch(&c, url, json!({"settings": {"permissions": perms}})).await }
    };

    // the settings are checked
    for bad in [json!({"deleteIssues": "nobody"}), json!({"teleport": "member"}), json!({"manageSprints": "everyone"})] {
        let (s, _) = send(&admin, Method::PATCH, app.url("/projects/PERM"), json!({"settings": {"permissions": bad}})).await;
        assert_eq!(s, 400, "{bad}");
    }

    // deleting issues: members by default, then admins only
    let a = issue(&mia, "First").await;
    assert_eq!(send(&mia, Method::DELETE, app.url(&format!("/issues/{a}")), json!(null)).await.0, 204);
    set(json!({"deleteIssues": "admin"})).await;
    let b = issue(&mia, "Second").await;
    let (s, v) = send(&mia, Method::DELETE, app.url(&format!("/issues/{b}")), json!(null)).await;
    assert_eq!(s, 403, "{v}");
    assert!(v["message"].as_str().unwrap().contains("delete issues"));
    let bulk = post(&mia, app.url("/issues/bulk"), json!({"ids": [get(&mia, app.url(&format!("/issues/{b}"))).await["id"]], "action": "delete"})).await;
    assert_eq!(bulk["updated"], 0, "bulk deletes follow the same rule: {bulk}");
    assert_eq!(send(&admin, Method::DELETE, app.url(&format!("/issues/{b}")), json!(null)).await.0, 204);

    // sprints and releases
    set(json!({"manageSprints": "admin", "manageReleases": "admin"})).await;
    assert_eq!(send(&mia, Method::POST, app.url("/projects/PERM/sprints"), json!({"name": "Mia's sprint"})).await.0, 403);
    assert_eq!(send(&mia, Method::POST, app.url("/projects/PERM/versions"), json!({"name": "9.9"})).await.0, 403);
    let v = post(&admin, app.url("/projects/PERM/versions"), json!({"name": "1.0"})).await;
    assert_eq!(send(&mia, Method::DELETE, app.url(&format!("/versions/{}", v["id"].as_str().unwrap())), json!(null)).await.0, 403);
    set(json!({"manageSprints": "member", "manageReleases": "member"})).await;
    post(&mia, app.url("/projects/PERM/sprints"), json!({"name": "Mia's sprint"})).await;

    // other people's comments: admins by default (now also editing), or nobody
    let c = issue(&mia, "Discuss").await;
    let mine = post(&mia, app.url(&format!("/issues/{c}/comments")), json!({"markdown": "typo heer"})).await;
    let mine_id = mine["id"].as_str().unwrap();
    patch(&admin, app.url(&format!("/comments/{mine_id}")), json!({"markdown": "typo here"})).await;
    let theirs = post(&admin, app.url(&format!("/issues/{c}/comments")), json!({"markdown": "admin note"})).await;
    assert_eq!(send(&mia, Method::PATCH, app.url(&format!("/comments/{}", theirs["id"].as_str().unwrap())), json!({"markdown": "edited"})).await.0, 403);
    set(json!({"moderateComments": "nobody"})).await;
    assert_eq!(send(&admin, Method::DELETE, app.url(&format!("/comments/{mine_id}")), json!(null)).await.0, 403);
    assert_eq!(send(&mia, Method::DELETE, app.url(&format!("/comments/{mine_id}")), json!(null)).await.0, 204, "authors always may");

    // time for others: admins by default
    let w = post(&admin, app.url(&format!("/issues/{c}/worklogs")), json!({"time": "2h", "userId": mia_id})).await;
    let logs = get(&admin, app.url(&format!("/issues/{c}/worklogs"))).await;
    assert!(logs.as_array().unwrap().iter().any(|l| l["user"]["id"] == mia_id.as_str() && l["minutes"] == 120), "{logs}");
    let admin_id = get(&admin, app.url("/me")).await["user"]["id"].as_str().unwrap().to_string();
    assert_eq!(send(&mia, Method::POST, app.url(&format!("/issues/{c}/worklogs")), json!({"time": "1h", "userId": admin_id})).await.0, 403);
    assert_eq!(send(&mia, Method::PATCH, app.url(&format!("/worklogs/{}", w["id"].as_str().unwrap())), json!({"time": "3h"})).await.0, 200, "her own log");
    set(json!({"logTimeForOthers": "nobody"})).await;
    assert_eq!(send(&admin, Method::POST, app.url(&format!("/issues/{c}/worklogs")), json!({"time": "1h", "userId": mia_id})).await.0, 403);

    // moving issues to another project
    post(&admin, app.url("/projects"), json!({"key": "OTHER", "name": "Other", "template": "kanban"})).await;
    set(json!({"moveIssues": "admin"})).await;
    let (s, _) = send(&mia, Method::POST, app.url(&format!("/issues/{c}/move-project")), json!({"projectKey": "OTHER"})).await;
    assert_eq!(s, 403);
}

#[tokio::test]
async fn restricted_comments_reach_only_those_they_are_for() {
    let app = spawn().await;
    let admin = app.setup(false).await;
    post(&admin, app.url("/projects"), json!({"key": "SEC", "name": "Secrets", "template": "kanban"})).await;
    let (mia, mia_id) = app.user(&admin, "Mia Member", "mia@example.com", "member").await;
    let (max, max_id) = app.user(&admin, "Max Member", "max@example.com", "member").await;
    let (vic, vic_id) = app.user(&admin, "Vic Viewer", "vic@example.com", "guest").await;
    let (s, _) = send(&admin, Method::PUT, app.url("/projects/SEC/members"), json!([{"subjectType": "user", "subjectId": vic_id, "role": "viewer"}])).await;
    assert!(s < 300);
    let qa = post(&admin, app.url("/teams"), json!({"name": "QA", "memberIds": [mia_id]})).await;
    let key = post(&mia, app.url("/issues"), json!({"projectKey": "SEC", "title": "Login leaks"})).await["key"].as_str().unwrap().to_string();
    for w in [&max_id, &vic_id] {
        let (s, _) = send(&admin, Method::PUT, app.url(&format!("/issues/{key}/watch")), json!({"watching": true, "userId": w})).await;
        assert!(s < 300);
    }
    let comments = |c: &reqwest::Client| {
        let (c, url) = (c.clone(), app.url(&format!("/issues/{key}/comments")));
        async move { get(&c, url).await.as_array().unwrap().iter().map(|x| x["body"].to_string()).collect::<Vec<_>>().join(" ") }
    };

    // who may write what
    assert_eq!(send(&mia, Method::POST, app.url(&format!("/issues/{key}/comments")), json!({"markdown": "x", "visibility": "admins"})).await.0, 400);
    let other_team = post(&admin, app.url("/teams"), json!({"name": "Ops", "memberIds": [max_id]})).await;
    let (s, _) = send(
        &mia,
        Method::POST,
        app.url(&format!("/issues/{key}/comments")),
        json!({"markdown": "x", "visibility": format!("team:{}", other_team["id"].as_str().unwrap())}),
    )
    .await;
    assert_eq!(s, 400, "only for a team you're in");

    post(&admin, app.url(&format!("/issues/{key}/comments")), json!({"markdown": "admins only: the root cause", "visibility": "admins"})).await;
    post(&mia, app.url(&format!("/issues/{key}/comments")), json!({"markdown": "members: patch ready", "visibility": "members"})).await;
    post(
        &mia,
        app.url(&format!("/issues/{key}/comments")),
        json!({"markdown": "QA: reproduce with token X @Max Member", "visibility": format!("team:{}", qa["id"].as_str().unwrap())}),
    )
    .await;
    post(&mia, app.url(&format!("/issues/{key}/comments")), json!({"markdown": "everyone: fixed soon"})).await;

    let a = comments(&admin).await;
    assert!(a.contains("root cause") && a.contains("patch ready") && a.contains("token X") && a.contains("fixed soon"), "admins see all");
    let m = comments(&mia).await;
    assert!(!m.contains("root cause") && m.contains("patch ready") && m.contains("token X"), "{m}");
    let x = comments(&max).await;
    assert!(!x.contains("root cause") && x.contains("patch ready") && !x.contains("token X") && x.contains("fixed soon"), "{x}");
    let v = comments(&vic).await;
    assert!(!v.contains("patch ready") && !v.contains("token X") && v.contains("fixed soon"), "viewers see only open comments: {v}");
    // no trace in history or activity, no notification for those who can't read it
    let history = get(&vic, app.url(&format!("/issues/{key}/history"))).await.to_string();
    assert!(!history.contains("patch ready") && !history.contains("root cause") && history.contains("fixed soon"), "{history}");
    let activity = get(&max, app.url("/activity")).await.to_string();
    assert!(!activity.contains("token X") && !activity.contains("root cause"), "{activity}");
    let notes = get(&max, app.url("/notifications")).await.to_string();
    assert!(!notes.contains("token X") && notes.contains("patch ready"), "mentioned but not in the team: not told: {notes}");
    // and comment search doesn't give it away
    let found: serde_json::Value = max.get(app.url("/search")).query(&[("q", "comment ~ \"token X\"")]).send().await.unwrap().json().await.unwrap();
    assert_eq!(found["total"], 0);
}

#[tokio::test]
async fn private_issues_are_seen_only_by_those_they_are_for() {
    let app = spawn().await;
    let admin = app.setup(false).await;
    post(&admin, app.url("/projects"), json!({"key": "SEC", "name": "Secrets", "template": "scrum"})).await;
    post(&admin, app.url("/projects"), json!({"key": "PUB", "name": "Public", "template": "kanban"})).await;
    let (mia, mia_id) = app.user(&admin, "Mia Member", "mia@example.com", "member").await;
    let (max, max_id) = app.user(&admin, "Max Member", "max@example.com", "member").await;
    let (sam, sam_id) = app.user(&admin, "Sam Member", "sam@example.com", "member").await;
    let sec_team = post(&admin, app.url("/teams"), json!({"name": "Security", "memberIds": [sam_id]})).await;

    // levels: checked, unique, a team's
    let (s, _) = send(&admin, Method::POST, app.url("/projects/SEC/security-levels"), json!({"name": "x", "minRole": "admin"})).await;
    assert_eq!(s, 400);
    assert_eq!(send(&mia, Method::POST, app.url("/projects/SEC/security-levels"), json!({"name": "Mine"})).await.0, 403, "admins only");
    let team = post(&admin, app.url("/projects/SEC/security-levels"), json!({"name": "Security team", "teamIds": [sec_team["id"]]})).await;
    let team_id = team["id"].as_str().unwrap().to_string();
    assert_eq!(send(&admin, Method::POST, app.url("/projects/SEC/security-levels"), json!({"name": "security TEAM"})).await.0, 400);
    let staff = post(&admin, app.url("/projects/SEC/security-levels"), json!({"name": "Staff", "minRole": "member"})).await;
    let leads = post(&admin, app.url("/projects/SEC/security-levels"), json!({"name": "Leads", "userIds": [max_id]})).await;

    // Mia files a vulnerability right into the security team's level: she keeps seeing it
    let key = post(&mia, app.url("/issues"), json!({"projectKey": "SEC", "title": "Token leak in logs", "securityLevelId": team_id})).await["key"]
        .as_str()
        .unwrap()
        .to_string();
    let open = post(&mia, app.url("/issues"), json!({"projectKey": "SEC", "title": "Public typo"})).await["key"].as_str().unwrap().to_string();
    assert_eq!(get(&mia, app.url(&format!("/issues/{key}"))).await["security"]["name"], "Security team");
    assert_eq!(get(&sam, app.url(&format!("/issues/{key}"))).await["key"], key.as_str(), "the team sees it");
    assert_eq!(send(&max, Method::GET, app.url(&format!("/issues/{key}")), json!(null)).await.0, 404, "others don't");
    assert_eq!(send(&max, Method::POST, app.url(&format!("/issues/{key}/comments")), json!({"markdown": "hi"})).await.0, 404);
    let search = |c: &reqwest::Client, q: &str| {
        let (c, url, q) = (c.clone(), app.url("/search"), q.to_string());
        async move {
            let v: serde_json::Value = c.get(url).query(&[("q", q.as_str())]).send().await.unwrap().json().await.unwrap();
            v["issues"].as_array().unwrap().iter().map(|i| i["key"].as_str().unwrap().to_string()).collect::<Vec<_>>()
        }
    };
    assert_eq!(search(&max, "project = SEC").await, vec![open.clone()]);
    assert_eq!(search(&admin, "project = SEC and security = \"Security team\"").await, vec![key.clone()]);
    assert_eq!(search(&sam, "project = SEC and security is empty").await, vec![open.clone()]);
    let activity = get(&max, app.url("/activity")).await.to_string();
    assert!(!activity.contains("Token leak") && activity.contains("Public typo"), "{activity}");

    // a comment notifies only those who see the issue
    for w in [&max_id, &sam_id] {
        send(&admin, Method::PUT, app.url(&format!("/issues/{key}/watch")), json!({"watching": true, "userId": w})).await;
    }
    post(&admin, app.url(&format!("/issues/{key}/comments")), json!({"markdown": "rotating the token now"})).await;
    assert!(get(&sam, app.url("/notifications")).await.to_string().contains("rotating"));
    assert!(!get(&max, app.url("/notifications")).await.to_string().contains("rotating"));

    // changing the level: only to one you're in, and as the project allows
    let (s, v) = send(&sam, Method::PATCH, app.url(&format!("/issues/{key}")), json!({"securityLevelId": leads["id"]})).await;
    assert_eq!(s, 403, "Sam isn't one of the leads… {v}");
    let (s, _) = send(&max, Method::PATCH, app.url(&format!("/issues/{open}")), json!({"securityLevelId": leads["id"]})).await;
    assert_eq!(s, 200, "…Max is");
    patch(&admin, app.url("/projects/SEC"), json!({"settings": {"permissions": {"setSecurity": "admin"}}})).await;
    assert_eq!(send(&max, Method::PATCH, app.url(&format!("/issues/{open}")), json!({"securityLevelId": null})).await.0, 403);
    patch(&admin, app.url("/projects/SEC"), json!({"settings": {"permissions": {}}})).await;
    let history = get(&admin, app.url(&format!("/issues/{open}/history"))).await.to_string();
    assert!(history.contains("security") && history.contains("Leads"), "{history}");

    // a copy is just as private; a default level applies to new issues
    let copy = post(&sam, app.url(&format!("/issues/{key}/clone")), json!({})).await;
    assert_eq!(copy["security"]["name"], "Security team");
    patch(&admin, app.url(&format!("/security-levels/{team_id}")), json!({"isDefault": true})).await;
    let fresh = post(&max, app.url("/issues"), json!({"projectKey": "SEC", "title": "Default private"})).await;
    assert_eq!(fresh["security"]["name"], "Security team");
    assert_eq!(get(&max, app.url(&format!("/issues/{}", fresh["key"].as_str().unwrap()))).await["key"], fresh["key"], "its reporter sees it");

    // moving: the target needs a level of the same name (or a default)
    let (s, v) = send(&admin, Method::POST, app.url(&format!("/issues/{key}/move-project")), json!({"projectKey": "PUB"})).await;
    assert_eq!(s, 400, "{v}");
    post(&admin, app.url("/projects/PUB/security-levels"), json!({"name": "Security Team", "teamIds": [sec_team["id"]]})).await;
    let moved = post(&admin, app.url(&format!("/issues/{key}/move-project")), json!({"projectKey": "PUB"})).await;
    assert_eq!(moved["security"]["name"], "Security Team");
    assert_eq!(send(&max, Method::GET, app.url(&format!("/issues/{}", moved["key"].as_str().unwrap())), json!(null)).await.0, 404);

    // deleting a level in use needs a replacement (or making its issues public)
    let (s, v) = send(&admin, Method::DELETE, app.url(&format!("/security-levels/{team_id}")), json!(null)).await;
    assert_eq!(s, 400, "{v}");
    let (s, _) = send(&admin, Method::DELETE, app.url(&format!("/security-levels/{team_id}?replaceWith={}", staff["id"].as_str().unwrap())), json!(null)).await;
    assert_eq!(s, 204);
    assert_eq!(get(&max, app.url(&format!("/issues/{}", copy["key"].as_str().unwrap()))).await["security"]["name"], "Staff");
    let p = get(&mia, app.url("/projects/SEC")).await;
    let levels = p["securityLevels"].as_array().unwrap();
    assert_eq!(levels.len(), 2);
    assert_eq!((levels[0]["name"].as_str(), levels[0]["mine"].as_bool()), (Some("Staff"), Some(true)), "members are staff");
    assert_eq!((levels[1]["name"].as_str(), levels[1]["mine"].as_bool()), (Some("Leads"), Some(false)));
    let _ = mia_id;
}
