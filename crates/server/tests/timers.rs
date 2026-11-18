//! Timers: one per person, pause and resume, stop into a draft or a time log (once, even when
//! the stop or the booking is sent twice), the workspace's rounding, drafts stay private, and
//! forgotten timers are reminded of and stopped after 24 hours.

mod common;

use common::*;
use reqwest::Method;
use serde_json::json;

#[tokio::test]
async fn timers_and_drafts() {
    let app = spawn().await;
    let admin = app.setup(false).await;
    post(&admin, app.url("/projects"), json!({"key": "TMR", "name": "Timers", "template": "kanban"})).await;
    let (mia, mia_id) = app.user(&admin, "Mia Member", "mia@example.com", "member").await;
    let a = post(&admin, app.url("/issues"), json!({"projectKey": "TMR", "title": "Write the docs", "estimateMinutes": 120})).await["key"]
        .as_str()
        .unwrap()
        .to_string();
    let b = post(&admin, app.url("/issues"), json!({"projectKey": "TMR", "title": "Fix the build"})).await["key"].as_str().unwrap().to_string();
    let db = &app.state.db;

    assert_eq!(get(&mia, app.url("/me/timer")).await, json!({"timer": null, "drafts": 0}));
    let (code, _) = send(&mia, Method::POST, app.url(&format!("/issues/{a}/timer")), json!({"note": "chapter 1"})).await;
    assert_eq!(code, 201);
    let t = get(&mia, app.url("/me/timer")).await;
    assert_eq!((t["timer"]["issue"]["key"].as_str(), t["timer"]["note"].as_str()), (Some(a.as_str()), Some("chapter 1")));
    // another issue: refused unless the running one is replaced (its time waits as a draft)
    let (code, e) = send(&mia, Method::POST, app.url(&format!("/issues/{b}/timer")), json!({})).await;
    assert!(code == 409 && e.to_string().contains(&a), "{e}");

    // 25 minutes, of which 10 paused: 15 minutes, rounded up to the workspace's 10 minutes → 20
    sqlx::query("UPDATE timers SET started_at = now() - interval '25 minutes'").execute(db).await.unwrap();
    assert_eq!(send(&mia, Method::POST, app.url("/me/timer/pause"), json!({})).await.0, 204);
    assert!(get(&mia, app.url("/me/timer")).await["timer"]["pausedAt"].is_string());
    sqlx::query("UPDATE timers SET paused_at = now() - interval '10 minutes'").execute(db).await.unwrap();
    assert_eq!(send(&mia, Method::POST, app.url("/me/timer/resume"), json!({})).await.0, 204);
    let secs = get(&mia, app.url("/me/timer")).await["timer"]["elapsedSeconds"].as_i64().unwrap();
    assert!((14 * 60..=16 * 60).contains(&secs), "{secs}");
    let (code, _) = send(&admin, Method::PUT, app.url("/admin/workspace"), json!({"timer": {"rounding": 7}})).await;
    assert_eq!(code, 400);
    send(&admin, Method::PUT, app.url("/admin/workspace"), json!({"timer": {"rounding": 10, "reminderHours": 4}})).await;
    assert_eq!(get(&mia, app.url("/me")).await["workspace"]["timer"], json!({"rounding": 10, "reminderHours": 4}));

    // stop into a draft (a second stop finds nothing), then log the draft once
    let (code, stopped) = send(&mia, Method::POST, app.url("/me/timer/stop"), json!({})).await;
    assert_eq!(code, 200);
    let draft = stopped["draft"].clone();
    assert_eq!((draft["key"].as_str(), draft["minutes"].as_i64(), draft["note"].as_str()), (Some(a.as_str()), Some(20), Some("chapter 1")));
    assert_eq!(send(&mia, Method::POST, app.url("/me/timer/stop"), json!({})).await.0, 409);
    assert_eq!(get(&admin, app.url(&format!("/issues/{a}"))).await["spentMinutes"], 0, "drafts don't count");
    assert!(get(&admin, app.url("/me/time-drafts")).await.as_array().unwrap().is_empty(), "only their owner sees them");
    let body = json!({"time": "25m", "date": draft["date"], "draftId": draft["id"], "note": "chapter 1, done"});
    assert_eq!(send(&mia, Method::POST, app.url(&format!("/issues/{a}/worklogs")), body.clone()).await.0, 201);
    let (code, e) = send(&mia, Method::POST, app.url(&format!("/issues/{a}/worklogs")), body).await;
    assert!(code == 409 && e.to_string().contains("booked"), "logged once: {e}");
    let i = get(&mia, app.url(&format!("/issues/{a}"))).await;
    assert_eq!((i["spentMinutes"].as_i64(), i["remainingMinutes"].as_i64()), (Some(25), Some(95)));
    assert_eq!(get(&mia, app.url("/me/timer")).await["drafts"], 0);

    // replacing: the timer on A becomes a draft, B runs; stop with book logs right away
    send(&mia, Method::POST, app.url(&format!("/issues/{a}/timer")), json!({})).await;
    let (code, started) = send(&mia, Method::POST, app.url(&format!("/issues/{b}/timer")), json!({"replace": true})).await;
    assert_eq!(code, 201);
    assert_eq!(started["draft"]["key"], a.as_str());
    sqlx::query("UPDATE timers SET started_at = now() - interval '61 minutes'").execute(db).await.unwrap();
    let (code, booked) = send(&mia, Method::POST, app.url("/me/timer/stop"), json!({"book": true, "note": "green again"})).await;
    assert_eq!(code, 200, "{booked}");
    assert_eq!(booked["worklog"]["minutes"], 70);
    assert_eq!(get(&mia, app.url(&format!("/issues/{b}"))).await["spentMinutes"], 70);
    let drafts = get(&mia, app.url("/me/time-drafts")).await;
    assert_eq!(drafts.as_array().unwrap().len(), 1);
    let id = drafts[0]["id"].as_str().unwrap();
    assert_eq!(send(&admin, Method::DELETE, app.url(&format!("/me/time-drafts/{id}")), json!(null)).await.0, 404, "not theirs");
    assert_eq!(send(&mia, Method::DELETE, app.url(&format!("/me/time-drafts/{id}")), json!(null)).await.0, 204);

    // forgotten timers: a reminder after the workspace's hours, a draft at 24 hours
    send(&mia, Method::POST, app.url(&format!("/issues/{b}/timer")), json!({})).await;
    sqlx::query("UPDATE timers SET started_at = now() - interval '5 hours'").execute(db).await.unwrap();
    taskcraft::timers::housekeeping(&app.state).await.unwrap();
    taskcraft::timers::housekeeping(&app.state).await.unwrap();
    let reminders: i64 =
        sqlx::query_scalar("SELECT count(*) FROM notifications WHERE user_id = $1 AND kind = 'timer'").bind(mia_id_uuid(&mia_id)).fetch_one(db).await.unwrap();
    assert_eq!(reminders, 1, "once");
    sqlx::query("UPDATE timers SET started_at = now() - interval '30 hours'").execute(db).await.unwrap();
    taskcraft::timers::housekeeping(&app.state).await.unwrap();
    let t = get(&mia, app.url("/me/timer")).await;
    assert_eq!((t["timer"].is_null(), t["drafts"].as_i64()), (true, Some(1)));
    assert_eq!(get(&mia, app.url("/me/time-drafts")).await[0]["minutes"], 24 * 60);
    assert_eq!(send(&mia, Method::DELETE, app.url("/me/timer"), json!(null)).await.0, 404);

    // time tracking turned off: no timers
    send(&admin, Method::PUT, app.url("/admin/workspace"), json!({"features": {"timeTracking": false}})).await;
    assert_ne!(send(&mia, Method::POST, app.url(&format!("/issues/{a}/timer")), json!({})).await.0, 201);
}

fn mia_id_uuid(id: &str) -> uuid::Uuid {
    id.parse().unwrap()
}
