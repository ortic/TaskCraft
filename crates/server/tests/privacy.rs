//! Personal data exports and retention rules.

mod common;

use common::*;
use reqwest::Method;
use serde_json::{Value, json};

#[tokio::test]
async fn people_get_their_data_and_old_things_go() {
    let app = spawn().await;
    let admin = app.setup(false).await;
    post(&admin, app.url("/projects"), json!({"key": "PRV", "name": "Privacy", "template": "kanban"})).await;
    let (mia, mia_id) = app.user(&admin, "Mia Member", "mia@example.com", "member").await;
    let key = post(&mia, app.url("/issues"), json!({"projectKey": "PRV", "title": "Mia's own issue"})).await["key"].as_str().unwrap().to_string();
    post(&mia, app.url(&format!("/issues/{key}/comments")), json!({"markdown": "My words"})).await;
    post(&mia, app.url(&format!("/issues/{key}/worklogs")), json!({"time": "1h", "note": "typing"})).await;
    let token = post(&mia, app.url("/me/tokens"), json!({"name": "laptop", "scope": "read"})).await;
    // her own copy
    let res = mia.get(app.url("/me/export")).send().await.unwrap();
    assert_eq!(res.status(), 200);
    assert!(res.headers()["content-disposition"].to_str().unwrap().contains("attachment"));
    let data: Value = res.json().await.unwrap();
    assert_eq!(data["profile"]["email"], "mia@example.com");
    assert!(data["comments"].to_string().contains("My words"));
    assert_eq!(data["timeLogs"][0]["minutes"], 60);
    assert_eq!(data["apiTokens"][0]["name"], "laptop");
    let secret = token["token"].as_str().unwrap_or("tct_");
    assert!(!data.to_string().contains(&secret[secret.len().saturating_sub(12)..]), "no token secrets");
    let issues = data["issues"].as_array().unwrap();
    let mine = issues.iter().find(|i| i["key"] == key.as_str()).unwrap();
    assert_eq!(mine["title"], "Mia's own issue");
    assert_eq!(data["sessions"].as_array().unwrap().len(), 1);

    // admins can make one for anyone; others can't
    assert_eq!(send(&mia, Method::GET, app.url(&format!("/users/{mia_id}/export")), json!(null)).await.0, 403);
    let by_admin: Value = admin.get(app.url(&format!("/users/{mia_id}/export"))).send().await.unwrap().json().await.unwrap();
    assert_eq!(by_admin["profile"]["name"], "Mia Member");

    // retention: checked, then applied
    for bad in [json!({"trashDays": 0}), json!({"forever": 3}), json!({"auditDays": "a week"})] {
        assert_eq!(send(&admin, Method::PUT, app.url("/admin/workspace"), json!({"retention": bad})).await.0, 400, "{bad}");
    }
    assert!(
        send(&admin, Method::PUT, app.url("/admin/workspace"), json!({"retention": {"trashDays": 30, "auditDays": 365, "notificationDays": 90}})).await.0 < 300
    );
    let gone = post(&admin, app.url("/issues"), json!({"projectKey": "PRV", "title": "Old junk"})).await["key"].as_str().unwrap().to_string();
    send(&admin, Method::DELETE, app.url(&format!("/issues/{gone}")), json!(null)).await;
    let kept = post(&admin, app.url("/issues"), json!({"projectKey": "PRV", "title": "Fresh junk"})).await["key"].as_str().unwrap().to_string();
    send(&admin, Method::DELETE, app.url(&format!("/issues/{kept}")), json!(null)).await;
    sqlx::query("UPDATE issues SET deleted_at = now() - interval '31 days' WHERE title = 'Old junk'").execute(&app.state.db).await.unwrap();
    sqlx::query("UPDATE audit_log SET created_at = now() - interval '400 days' WHERE action = 'workspace.updated'").execute(&app.state.db).await.unwrap();
    taskcraft::privacy::purge_now(&app.state).await.unwrap();
    let titles: Vec<String> = sqlx::query_scalar("SELECT title FROM issues WHERE deleted_at IS NOT NULL").fetch_all(&app.state.db).await.unwrap();
    assert_eq!(titles, ["Fresh junk"], "the trash keeps 30 days");
    let old_audit: i64 =
        sqlx::query_scalar("SELECT count(*) FROM audit_log WHERE created_at < now() - interval '365 days'").fetch_one(&app.state.db).await.unwrap();
    assert_eq!(old_audit, 0);
}
