//! Voting on issues, and issue lists as Excel.

mod common;

use common::*;
use reqwest::Method;
use serde_json::{Value, json};

#[tokio::test]
async fn people_vote_and_lists_export_to_excel() {
    let app = spawn().await;
    let admin = app.setup(false).await;
    post(&admin, app.url("/projects"), json!({"key": "VOT", "name": "Votes", "template": "kanban"})).await;
    let (mia, _) = app.user(&admin, "Mia Member", "mia@example.com", "member").await;
    let (vic, vic_id) = app.user(&admin, "Vic Viewer", "vic@example.com", "guest").await;
    send(&admin, Method::PUT, app.url("/projects/VOT/members"), json!([{"subjectType": "user", "subjectId": vic_id, "role": "viewer"}])).await;
    let a = post(&admin, app.url("/issues"), json!({"projectKey": "VOT", "title": "Dark mode"})).await["key"].as_str().unwrap().to_string();
    let b = post(&admin, app.url("/issues"), json!({"projectKey": "VOT", "title": "Export to =SUM(A1)"})).await["key"].as_str().unwrap().to_string();

    // anyone who sees it votes, once; viewers too
    for c in [&mia, &vic, &mia] {
        assert_eq!(send(c, Method::PUT, app.url(&format!("/issues/{a}/vote")), json!({"voted": true})).await.0, 204);
    }
    let i = get(&mia, app.url(&format!("/issues/{a}"))).await;
    assert_eq!((i["votes"].as_i64(), i["voted"].as_bool()), (Some(2), Some(true)));
    assert_eq!(i["voters"].as_array().unwrap().len(), 2);
    send(&vic, Method::PUT, app.url(&format!("/issues/{b}/vote")), json!({"voted": true})).await;
    send(&vic, Method::PUT, app.url(&format!("/issues/{b}/vote")), json!({"voted": false})).await;
    assert_eq!(get(&admin, app.url(&format!("/issues/{b}"))).await["votes"], 0, "taken back");

    let titles = |c: &reqwest::Client, q: &str| {
        let (c, url, q) = (c.clone(), app.url("/search"), q.to_string());
        async move {
            let v: Value = c.get(url).query(&[("q", q.as_str())]).send().await.unwrap().json().await.unwrap();
            v["issues"].as_array().unwrap().iter().map(|i| i["title"].as_str().unwrap().to_string()).collect::<Vec<_>>()
        }
    };
    assert_eq!(titles(&admin, "project = VOT and votes >= 2").await, ["Dark mode"]);
    assert_eq!(titles(&mia, "voter = me()").await, ["Dark mode"]);
    assert_eq!(titles(&admin, "project = VOT order by votes desc").await, ["Dark mode", "Export to =SUM(A1)"]);

    // the list as Excel: a workbook, text kept as text
    let res = admin.get(app.url("/search.xlsx")).query(&[("q", "project = VOT order by key")]).send().await.unwrap();
    assert_eq!(res.status(), 200);
    assert_eq!(res.headers()["content-type"], "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet");
    let bytes = res.bytes().await.unwrap();
    assert_eq!(&bytes[..2], b"PK", "a zip");
    let csv = admin.get(app.url("/search.csv")).query(&[("q", "project = VOT order by key")]).send().await.unwrap().text().await.unwrap();
    assert!(csv.lines().next().unwrap().ends_with(",Votes"), "{csv}");
    assert!(csv.contains("'Export to =SUM(A1)") || csv.contains("Export to =SUM(A1)"), "{csv}");
}
