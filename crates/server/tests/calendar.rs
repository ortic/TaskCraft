//! Calendar feeds: secret addresses with a person's issues, a saved filter's or a project's, plus
//! sprints and releases; read as the owner (private issues stay private); renewing revokes the
//! old address; the workspace can turn feeds off. And a search as an `.ics` download.

mod common;

use common::*;
use reqwest::Method;
use serde_json::{Value, json};

async fn fetch(url: &str) -> (u16, String) {
    let res = reqwest::get(url).await.unwrap();
    let code = res.status().as_u16();
    if code == 200 {
        assert!(res.headers()["content-type"].to_str().unwrap().starts_with("text/calendar"));
    }
    (code, res.text().await.unwrap())
}

#[tokio::test]
async fn calendar_feeds() {
    let app = spawn().await;
    let admin = app.setup(false).await;
    post(&admin, app.url("/projects"), json!({"key": "CAL", "name": "Calendar", "template": "scrum"})).await;
    let (mia, mia_id) = app.user(&admin, "Mia Member", "mia@example.com", "member").await;
    send(&admin, Method::PUT, app.url("/projects/CAL/members"), json!([{"subjectType": "user", "subjectId": mia_id, "role": "member"}])).await;
    let level = post(&admin, app.url("/projects/CAL/security-levels"), json!({"name": "Admins", "minRole": null})).await;
    let issue = |title: &str, extra: Value| {
        let (c, url) = (admin.clone(), app.url("/issues"));
        let mut body = json!({"projectKey": "CAL", "title": title, "assigneeId": mia_id});
        for (k, v) in extra.as_object().unwrap() {
            body[k] = v.clone();
        }
        async move { post(&c, url, body).await["key"].as_str().unwrap().to_string() }
    };
    let launch = issue("Launch, finally; the site", json!({"dueDate": "2026-12-01", "startDate": "2026-11-25"})).await;
    let secret = issue("Secret plans", json!({"dueDate": "2026-12-02", "securityLevelId": level["id"], "assigneeId": null})).await;
    let undated = issue("Someday", json!({})).await;
    post(&admin, app.url("/projects/CAL/sprints"), json!({"name": "Sprint 1", "startAt": "2026-11-23T08:00:00Z", "endAt": "2026-12-04T17:00:00Z"})).await;
    post(&admin, app.url("/projects/CAL/versions"), json!({"name": "1.0", "releaseDate": "2026-12-10"})).await;

    // mine: what's assigned to her with a due date, the project's sprints and releases
    let (code, made) = send(&mia, Method::POST, app.url("/me/calendar-feeds"), json!({"kind": "mine"})).await;
    assert_eq!(code, 201, "{made}");
    let url = made["url"].as_str().unwrap().to_string();
    assert!(url.contains("/api/calendar/") && url.ends_with(".ics"), "{url}");
    let (code, cal) = fetch(&url).await;
    assert_eq!(code, 200);
    assert!(cal.starts_with("BEGIN:VCALENDAR\r\n"), "{cal}");
    let unfolded = cal.replace("\r\n ", "");
    assert!(unfolded.contains(&format!("SUMMARY:{launch} Launch\\, finally\\; the site")), "{unfolded}");
    assert!(unfolded.contains("DTSTART;VALUE=DATE:20261125\r\nDTEND;VALUE=DATE:20261202"), "from the start to the due date: {unfolded}");
    assert!(!unfolded.contains(&secret), "not hers");
    assert!(!unfolded.contains(&undated));
    assert!(unfolded.contains("SUMMARY:Sprint 1 (CAL)") && unfolded.contains("SUMMARY:CAL 1.0"), "{unfolded}");
    let list = get(&mia, app.url("/me/calendar-feeds")).await;
    assert_eq!(list[0]["kind"], "mine");
    assert!(list[0]["lastUsedAt"].is_string(), "{list}");
    assert!(list.to_string().find("calendar/").is_none(), "addresses aren't kept");

    // without sprints and releases; a project feed; a saved filter's
    let (_, only) = send(&mia, Method::POST, app.url("/me/calendar-feeds"), json!({"kind": "mine", "parts": {"sprints": false, "releases": false}})).await;
    let (_, cal) = fetch(only["url"].as_str().unwrap()).await;
    assert!(!cal.contains("Sprint 1") && cal.contains(&launch));
    let (_, proj) = send(&admin, Method::POST, app.url("/me/calendar-feeds"), json!({"kind": "project", "projectKey": "CAL"})).await;
    let (_, cal) = fetch(proj["url"].as_str().unwrap()).await;
    assert!(cal.contains(&secret) && cal.contains(&launch), "the admin sees both");
    let (_, hers) = send(&mia, Method::POST, app.url("/me/calendar-feeds"), json!({"kind": "project", "projectKey": "CAL"})).await;
    let (_, cal) = fetch(hers["url"].as_str().unwrap()).await;
    assert!(!cal.contains(&secret) && cal.contains(&launch), "a private issue she may not see stays out of her feed");
    let view = post(&mia, app.url("/views"), json!({"name": "Launches", "query": "title ~ launch", "projectKey": "CAL"})).await;
    let (code, filt) = send(&mia, Method::POST, app.url("/me/calendar-feeds"), json!({"kind": "filter", "viewId": view["id"]})).await;
    assert_eq!(code, 201, "{filt}");
    let (_, cal) = fetch(filt["url"].as_str().unwrap()).await;
    assert!(cal.contains("X-WR-CALNAME:Launches") && cal.contains(&launch) && !cal.contains("Sprint 1"), "{cal}");
    let (code, _) = send(&admin, Method::POST, app.url("/me/calendar-feeds"), json!({"kind": "filter", "viewId": view["id"]})).await;
    assert_eq!(code, 404, "someone else's private filter");

    // renewing revokes the old address; deleting stops it
    let id = made["id"].as_str().unwrap();
    let (_, renewed) = send(&mia, Method::POST, app.url(&format!("/me/calendar-feeds/{id}/renew")), json!({})).await;
    assert_eq!(fetch(&url).await.0, 404);
    let url = renewed["url"].as_str().unwrap().to_string();
    assert_eq!(fetch(&url).await.0, 200);
    assert_eq!(send(&admin, Method::DELETE, app.url(&format!("/me/calendar-feeds/{id}")), json!(null)).await.0, 404, "not theirs");

    // the workspace turns feeds off: every address stops
    send(&admin, Method::PUT, app.url("/admin/workspace"), json!({"features": {"calendarFeeds": false}})).await;
    assert_eq!(fetch(&url).await.0, 404);
    assert_eq!(send(&mia, Method::POST, app.url("/me/calendar-feeds"), json!({"kind": "mine"})).await.0, 400);
    send(&admin, Method::PUT, app.url("/admin/workspace"), json!({"features": {"calendarFeeds": true}})).await;
    assert_eq!(fetch(&url).await.0, 200);
    assert_eq!(send(&mia, Method::DELETE, app.url(&format!("/me/calendar-feeds/{id}")), json!(null)).await.0, 204);
    assert_eq!(fetch(&url).await.0, 404);

    // a search as a download
    let res = mia.get(app.url("/search.ics")).query(&[("q", "project = CAL"), ("project", "CAL")]).send().await.unwrap();
    assert_eq!(res.status(), 200);
    let body = res.text().await.unwrap();
    assert!(body.contains(&launch) && !body.contains(&undated) && !body.contains(&secret), "{body}");
}
