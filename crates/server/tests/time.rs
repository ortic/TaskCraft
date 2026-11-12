//! Time tracking: remaining estimates, time categories and the time report.

mod common;

use common::*;
use reqwest::Method;
use serde_json::json;

#[tokio::test]
async fn remaining_estimate_follows_or_is_set() {
    let app = spawn().await;
    let admin = app.setup(false).await;
    post(&admin, app.url("/projects"), json!({"key": "TIME", "name": "Time", "template": "kanban"})).await;
    let key =
        post(&admin, app.url("/issues"), json!({"projectKey": "TIME", "title": "Build it", "estimateMinutes": 600})).await["key"].as_str().unwrap().to_string();
    let issue = || {
        let (c, url) = (admin.clone(), app.url(&format!("/issues/{key}")));
        async move { get(&c, url).await }
    };
    let log = |body: serde_json::Value| {
        let (c, url) = (admin.clone(), app.url(&format!("/issues/{key}/worklogs")));
        async move { send(&c, Method::POST, url, body).await }
    };
    // by itself: the estimate minus the time logged
    assert_eq!(log(json!({"time": "2h"})).await.0, 201);
    let i = issue().await;
    assert_eq!((i["estimateMinutes"].as_i64(), i["remainingMinutes"].as_i64(), i["remainingSet"].as_bool()), (Some(600), Some(480), Some(false)));
    // set by hand, then down with the time logged
    patch(&admin, app.url(&format!("/issues/{key}")), json!({"remainingMinutes": 300})).await;
    log(json!({"time": "1h"})).await;
    assert_eq!(issue().await["remainingMinutes"], 240);
    log(json!({"time": "1h", "remaining": "keep"})).await;
    assert_eq!(issue().await["remainingMinutes"], 240);
    log(json!({"time": "1h", "remaining": "2h"})).await;
    let i = issue().await;
    assert_eq!(
        (i["remainingMinutes"].as_i64(), i["estimateMinutes"].as_i64(), i["spentMinutes"].as_i64()),
        (Some(120), Some(600), Some(300)),
        "the original stays"
    );
    assert_eq!(log(json!({"time": "1h", "remaining": "soon"})).await.0, 400);
    // back to following the estimate
    patch(&admin, app.url(&format!("/issues/{key}")), json!({"remainingMinutes": null})).await;
    assert_eq!(issue().await["remainingMinutes"], 300);
    let found: serde_json::Value =
        admin.get(app.url("/search")).query(&[("q", "remaining >= 5 and remaining < 6")]).send().await.unwrap().json().await.unwrap();
    assert_eq!(found["total"], 1);
    let history = get(&admin, app.url(&format!("/issues/{key}/history"))).await.to_string();
    assert!(history.contains("\"remaining\""), "{history}");
}

#[tokio::test]
async fn categories_and_the_time_report() {
    let app = spawn().await;
    let admin = app.setup(false).await;
    post(&admin, app.url("/projects"), json!({"key": "WORK", "name": "Work", "template": "kanban"})).await;
    let (mia, mia_id) = app.user(&admin, "Mia Member", "mia@example.com", "member").await;

    // categories: admins manage them, names are unique
    assert_eq!(send(&mia, Method::POST, app.url("/time-categories"), json!({"name": "Mine"})).await.0, 403);
    let dev = post(&admin, app.url("/time-categories"), json!({"name": "Development", "billable": true})).await;
    let meet = post(&admin, app.url("/time-categories"), json!({"name": "Meetings"})).await;
    assert_eq!(send(&admin, Method::POST, app.url("/time-categories"), json!({"name": "development"})).await.0, 400);
    let old = post(&admin, app.url("/time-categories"), json!({"name": "Old"})).await;
    patch(&admin, app.url(&format!("/time-categories/{}", old["id"].as_str().unwrap())), json!({"archived": true})).await;
    let cats = get(&mia, app.url("/time-categories")).await;
    assert_eq!(cats.as_array().unwrap().len(), 3);

    let a = post(&mia, app.url("/issues"), json!({"projectKey": "WORK", "title": "Checkout"})).await["key"].as_str().unwrap().to_string();
    let b = post(&admin, app.url("/issues"), json!({"projectKey": "WORK", "title": "Standup"})).await["key"].as_str().unwrap().to_string();
    let log = |c: &reqwest::Client, key: &str, body: serde_json::Value| {
        let (c, url) = (c.clone(), app.url(&format!("/issues/{key}/worklogs")));
        async move { send(&c, Method::POST, url, body).await }
    };
    // billable follows the category unless said otherwise; archived ones can't be chosen
    assert_eq!(log(&mia, &a, json!({"time": "3h", "date": "2026-11-02", "categoryId": dev["id"]})).await.0, 201);
    assert_eq!(log(&mia, &a, json!({"time": "1h", "date": "2026-11-03", "categoryId": dev["id"], "billable": false})).await.0, 201);
    assert_eq!(log(&mia, &b, json!({"time": "30m", "date": "2026-11-03", "categoryId": meet["id"]})).await.0, 201);
    assert_eq!(log(&admin, &b, json!({"time": "2h", "date": "2026-11-10"})).await.0, 201);
    assert_eq!(log(&mia, &b, json!({"time": "1h", "categoryId": old["id"]})).await.0, 400);
    let logs = get(&mia, app.url(&format!("/issues/{a}/worklogs"))).await;
    let billable: Vec<bool> = logs.as_array().unwrap().iter().map(|l| l["billable"].as_bool().unwrap()).collect();
    assert_eq!(billable, vec![false, true], "newest first");
    assert_eq!(logs[0]["category"]["name"], "Development");

    let report = |c: &reqwest::Client, q: &[(&str, &str)]| {
        let (c, url) = (c.clone(), app.url("/time-report"));
        let q: Vec<(String, String)> = q.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        async move { c.get(url).query(&q).send().await.unwrap().json::<serde_json::Value>().await.unwrap() }
    };
    let nov = [("from", "2026-11-01"), ("to", "2026-11-30")];
    let r = report(&mia, &[nov[0], nov[1], ("group", "person"), ("then", "issue")]).await;
    assert_eq!((r["totalMinutes"].as_i64(), r["billableMinutes"].as_i64(), r["logCount"].as_i64()), (Some(390), Some(180), Some(4)), "{r}");
    assert_eq!(r["rows"][0]["label"], "Mia Member", "most time first");
    assert_eq!(r["rows"][0]["minutes"], 270);
    assert_eq!(r["rows"][0]["children"][0]["label"], format!("{a} Checkout"));
    let r = report(&mia, &[nov[0], nov[1], ("group", "week"), ("then", "category")]).await;
    let weeks: Vec<&str> = r["rows"].as_array().unwrap().iter().map(|w| w["id"].as_str().unwrap()).collect();
    assert_eq!(weeks, vec!["2026-11-02", "2026-11-09"], "periods in order");
    let r = report(&mia, &[nov[0], nov[1], ("group", "category"), ("category", "none")]).await;
    assert_eq!((r["totalMinutes"].as_i64(), r["rows"][0]["id"].as_str()), (Some(120), Some("none")));
    let r = report(&mia, &[nov[0], nov[1], ("billable", "true"), ("user", mia_id.as_str())]).await;
    assert_eq!(r["totalMinutes"], 180);
    assert_eq!(send(&mia, Method::GET, app.url("/time-report?group=colour"), json!(null)).await.0, 400);

    // CSV: every log
    let csv = mia.get(app.url("/time-report")).query(&[nov[0], nov[1], ("format", "csv")]).send().await.unwrap().text().await.unwrap();
    assert!(csv.starts_with("Date,Person,Project,Issue,Title,Category,Billable,Hours,Minutes,Note"), "{csv}");
    assert_eq!(csv.lines().count(), 5);

    // time on a private issue counts only for those who see it
    let level = post(&admin, app.url("/projects/WORK/security-levels"), json!({"name": "Admins"})).await;
    patch(&admin, app.url(&format!("/issues/{b}")), json!({"securityLevelId": level["id"]})).await;
    let r = report(&mia, &[nov[0], nov[1]]).await;
    assert_eq!(r["totalMinutes"], 240, "Mia doesn't see {b} any more: {r}");
    assert_eq!(report(&admin, &[nov[0], nov[1]]).await["totalMinutes"], 390);
}
