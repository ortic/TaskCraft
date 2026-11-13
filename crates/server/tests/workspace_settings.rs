//! Workspace settings: parts turned off, the default time zone and date format, an
//! announcement, the logo.

mod common;

use common::*;
use reqwest::Method;
use serde_json::json;

#[tokio::test]
async fn features_announcement_and_logo() {
    let app = spawn().await;
    let c = app.setup(false).await;
    let (member, _) = app.user(&c, "Max Member", "max@example.com", "member").await;
    post(&c, app.url("/projects"), json!({"key": "WEB", "name": "Website", "template": "kanban"})).await;
    post(&c, app.url("/issues"), json!({"projectKey": "WEB", "title": "Login fails"})).await;
    assert_eq!(send(&member, Method::PUT, app.url("/admin/workspace"), json!({"features": {"votes": false}})).await.0, 403);

    let off = json!({"features": {"votes": false, "timeTracking": false, "portal": false, "automation": false},
                     "timeZone": "Europe/Zurich", "dateFormat": "dmy",
                     "announcement": {"text": "Maintenance tonight at 22:00", "tone": "warning"}});
    assert!(send(&c, Method::PUT, app.url("/admin/workspace"), off).await.0 < 300);
    let me = get(&member, app.url("/me")).await;
    let ws = &me["workspace"];
    assert_eq!(ws["features"], json!({"timeTracking": false, "votes": false, "portal": false, "automation": false, "sharing": true}));
    assert_eq!((ws["timeZone"].as_str(), ws["dateFormat"].as_str()), (Some("Europe/Zurich"), Some("dmy")));
    assert_eq!(ws["announcement"]["text"], "Maintenance tonight at 22:00");
    assert_eq!(ws["announcement"]["tone"], "warning");

    // what's off is refused
    let (s, body) = send(&member, Method::PUT, app.url("/issues/WEB-1/vote"), json!({"voted": true})).await;
    assert_eq!(s, 400);
    assert!(body.to_string().contains("Voting is turned off"), "{body}");
    assert_eq!(send(&member, Method::POST, app.url("/issues/WEB-1/worklogs"), json!({"time": "1h"})).await.0, 400);
    assert_eq!(send(&c, Method::POST, app.url("/projects/WEB/automation"), json!({"name": "x", "trigger": {"type": "created"}, "actions": []})).await.0, 400);
    assert_eq!(reqwest::get(app.url("/portal")).await.unwrap().status(), 404, "the portal is gone");

    // and works again once it's on
    assert!(send(&c, Method::PUT, app.url("/admin/workspace"), json!({"features": {}, "announcement": null})).await.0 < 300);
    assert_eq!(send(&member, Method::PUT, app.url("/issues/WEB-1/vote"), json!({"voted": true})).await.0, 204);
    assert_eq!(send(&member, Method::POST, app.url("/issues/WEB-1/worklogs"), json!({"time": "1h"})).await.0, 201);
    assert_eq!(reqwest::get(app.url("/portal")).await.unwrap().status(), 200);
    assert_eq!(get(&member, app.url("/me")).await["workspace"]["announcement"], serde_json::Value::Null);

    // the logo: an image, not SVG
    let png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDRpretend".to_vec();
    let put = |client: &reqwest::Client, body: Vec<u8>| client.put(app.url("/admin/workspace/logo")).body(body).send();
    assert_eq!(put(&member, png.clone()).await.unwrap().status(), 403);
    assert_eq!(put(&c, b"<svg onload=\"alert(1)\"/>".to_vec()).await.unwrap().status(), 400);
    assert_eq!(put(&c, png.clone()).await.unwrap().status(), 204);
    let url = get(&member, app.url("/me")).await["workspace"]["logo"].as_str().unwrap().to_string();
    assert!(url.starts_with("/api/workspace/logo?v="), "{url}");
    let r = member.get(format!("{}{url}", app.base)).send().await.unwrap();
    assert_eq!(r.headers()["content-type"], "image/png");
    assert_eq!(r.bytes().await.unwrap().to_vec(), png);
    assert_eq!(reqwest::get(app.url("/workspace/logo")).await.unwrap().status(), 401, "for the workspace's people");
    assert_eq!(send(&c, Method::DELETE, app.url("/admin/workspace/logo"), serde_json::Value::Null).await.0, 204);
    assert_eq!(get(&member, app.url("/me")).await["workspace"]["logo"], serde_json::Value::Null);
}

#[tokio::test]
async fn profile_photos() {
    let app = spawn().await;
    let c = app.setup(false).await;
    let (max, max_id) = app.user(&c, "Max Member", "max@example.com", "member").await;
    let (mia, _) = app.user(&c, "Mia", "mia@example.com", "member").await;
    let jpeg = b"\xff\xd8\xff\xe0\0\x10JFIFpretend".to_vec();
    let put = |client: &reqwest::Client, body: Vec<u8>| client.put(app.url("/me/photo")).body(body).send();
    assert_eq!(put(&max, b"<svg/>".to_vec()).await.unwrap().status(), 400, "images only");
    assert_eq!(put(&max, vec![0xff; 2 * 1024 * 1024]).await.unwrap().status(), 400, "1 MB at most");
    let r: serde_json::Value = put(&max, jpeg.clone()).await.unwrap().json().await.unwrap();
    let url = r["photo"].as_str().unwrap().to_string();
    assert!(url.starts_with(&format!("/api/users/{max_id}/photo?v=")), "{url}");
    // everyone in the workspace sees it with the people list
    let people = get(&mia, app.url("/people")).await;
    let entry = people.as_array().unwrap().iter().find(|p| p["id"] == max_id.as_str()).unwrap().clone();
    assert_eq!(entry["photo"], url.as_str());
    assert_eq!(get(&max, app.url("/me")).await["user"]["photo"], url.as_str());
    let r = mia.get(format!("{}{url}", app.base)).send().await.unwrap();
    assert_eq!(r.headers()["content-type"], "image/jpeg");
    assert_eq!(r.bytes().await.unwrap().to_vec(), jpeg);
    // admins take one down; people remove their own
    assert_eq!(send(&mia, Method::DELETE, app.url(&format!("/users/{max_id}/photo")), serde_json::Value::Null).await.0, 403);
    assert_eq!(send(&c, Method::DELETE, app.url(&format!("/users/{max_id}/photo")), serde_json::Value::Null).await.0, 204);
    assert_eq!(get(&max, app.url("/me")).await["user"]["photo"], serde_json::Value::Null);
    assert_eq!(mia.get(format!("{}{url}", app.base)).send().await.unwrap().status(), 404);
    put(&max, jpeg.clone()).await.unwrap();
    assert_eq!(send(&max, Method::DELETE, app.url("/me/photo"), serde_json::Value::Null).await.0, 204);
    // and it goes with a removed account
    put(&max, jpeg).await.unwrap();
    assert_eq!(send(&c, Method::DELETE, app.url(&format!("/users/{max_id}")), serde_json::Value::Null).await.0, 204);
    let photo: Option<chrono::DateTime<chrono::Utc>> =
        sqlx::query_scalar("SELECT photo_at FROM users WHERE id = $1::uuid").bind(&max_id).fetch_one(&app.state.db).await.unwrap();
    assert_eq!(photo, None);
}
