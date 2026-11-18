//! Absences and public holidays: who sees why someone is away, half days, a sprint's capacity,
//! the away flag on people, substitutes hearing about assignments, and absences in feeds.

mod common;

use chrono::{Datelike, Duration, NaiveDate, Utc, Weekday};
use common::*;
use reqwest::Method;
use serde_json::json;

fn monday_after(d: NaiveDate) -> NaiveDate {
    let mut d = d + Duration::days(1);
    while d.weekday() != Weekday::Mon {
        d += Duration::days(1);
    }
    d
}

#[tokio::test]
async fn absences_holidays_and_capacity() {
    let app = spawn().await;
    let admin = app.setup(false).await;
    post(&admin, app.url("/projects"), json!({"key": "ABS", "name": "Away", "template": "scrum"})).await;
    let (mia, mia_id) = app.user(&admin, "Mia Member", "mia@example.com", "member").await;
    let (tom, tom_id) = app.user(&admin, "Tom Teammate", "tom@example.com", "member").await;
    send(
        &admin,
        Method::PUT,
        app.url("/projects/ABS/members"),
        json!([{"subjectType": "user", "subjectId": mia_id, "role": "member"}, {"subjectType": "user", "subjectId": tom_id, "role": "member"}]),
    )
    .await;
    let today = Utc::now().date_naive();
    let d = |n: i64| (today + Duration::days(n)).to_string();

    // Mia is away from yesterday for a week, Tom covers; others see only that she's away
    let (code, _) =
        send(&mia, Method::POST, app.url("/absences"), json!({"start": d(-1), "end": d(6), "kind": "vacation", "note": "Lisbon", "substituteId": tom_id}))
            .await;
    assert_eq!(code, 201);
    let (code, e) = send(&mia, Method::POST, app.url("/absences"), json!({"start": d(3), "end": d(4), "half": "am"})).await;
    assert!(code == 400 && e.to_string().contains("single day"), "{e}");
    assert_eq!(send(&mia, Method::POST, app.url("/absences"), json!({"start": d(2), "end": d(1)})).await.0, 400);
    assert_eq!(send(&tom, Method::POST, app.url("/absences"), json!({"start": d(1), "userId": mia_id})).await.0, 403, "only admins for others");
    let seen = get(&tom, app.url("/absences")).await;
    assert_eq!(seen[0]["user"]["name"], "Mia Member");
    assert!(seen[0]["kind"].is_null() && seen[0]["note"].is_null(), "not why: {seen}");
    assert_eq!(get(&mia, app.url("/absences")).await[0]["note"], "Lisbon");
    assert_eq!(get(&admin, app.url("/absences")).await[0]["kind"], "vacation");
    let people = get(&tom, app.url("/people")).await;
    let mia_p = people.as_array().unwrap().iter().find(|p| p["name"] == "Mia Member").unwrap();
    assert_eq!(mia_p["awayUntil"], d(6));
    assert!(people.as_array().unwrap().iter().find(|p| p["name"] == "Tom Teammate").unwrap()["awayUntil"].is_null());

    // assigning to her tells Tom, the substitute
    post(&admin, app.url("/issues"), json!({"projectKey": "ABS", "title": "Renew the certificate", "assigneeId": mia_id})).await;
    let n = get(&tom, app.url("/notifications")).await;
    let sub = n["items"].as_array().unwrap().iter().find(|x| x["kind"] == "substitute").expect("the substitute hears of it");
    assert_eq!(sub["data"]["for"], "Mia Member");

    // holidays (admins) in the default calendar and a region's; people pick theirs
    assert_eq!(send(&mia, Method::POST, app.url("/holidays"), json!({"day": d(20), "name": "Nope"})).await.0, 403);
    let start = monday_after(today + Duration::days(13));
    let (code, _) = send(
        &admin,
        Method::POST,
        app.url("/holidays"),
        json!({"holidays": [{"day": (start + Duration::days(2)).to_string(), "name": "Founders' day"},
                            {"day": (start + Duration::days(3)).to_string(), "name": "Regional day", "region": "Ticino"}]}),
    )
    .await;
    assert_eq!(code, 201);
    let h = get(&tom, app.url("/holidays")).await;
    assert_eq!(h["regions"], json!(["", "Ticino"]));
    assert_eq!(send(&tom, Method::PUT, app.url("/me/holiday-region"), json!({"region": "Ticino"})).await.0, 204);

    // a two-week sprint: Mia away 2 days and half a day, holidays by region
    post(&mia, app.url("/absences"), json!({"start": (start + Duration::days(7)).to_string(), "end": (start + Duration::days(8)).to_string()})).await;
    post(&mia, app.url("/absences"), json!({"start": (start + Duration::days(9)).to_string(), "half": "pm", "kind": "training"})).await;
    let sprint = post(
        &admin,
        app.url("/projects/ABS/sprints"),
        json!({"name": "Sprint 9", "startAt": format!("{start}T08:00:00Z"), "endAt": format!("{}T17:00:00Z", start + Duration::days(11))}),
    )
    .await;
    let cap = get(&tom, app.url(&format!("/sprints/{}/capacity", sprint["id"].as_str().unwrap()))).await;
    let by = |name: &str| cap["people"].as_array().unwrap().iter().find(|p| p["name"] == name).cloned().unwrap();
    let (m, t) = (by("Mia Member"), by("Tom Teammate"));
    assert_eq!((m["workingDays"].as_f64(), m["holidays"].as_f64(), m["away"].as_f64(), m["available"].as_f64()), (Some(10.0), Some(1.0), Some(2.5), Some(6.5)));
    assert_eq!((t["holidays"].as_f64(), t["available"].as_f64()), (Some(1.0), Some(9.0)), "Tom's region has its own holiday");

    // a project's members only
    let (outsider, _) = app.user(&admin, "Olga Outside", "olga@example.com", "member").await;
    post(&outsider, app.url("/absences"), json!({"start": d(1)})).await;
    let in_project = get(&tom, app.url("/absences?project=ABS")).await;
    assert!(!in_project.to_string().contains("Olga"), "{in_project}");
    assert!(get(&tom, app.url("/absences")).await.to_string().contains("Olga"));

    // edits and deletes: own or admins
    let mine = get(&mia, app.url(&format!("/absences?user={mia_id}"))).await;
    let id = mine[0]["id"].as_str().unwrap();
    assert_eq!(send(&tom, Method::PATCH, app.url(&format!("/absences/{id}")), json!({"end": d(3)})).await.0, 404);
    assert_eq!(send(&mia, Method::PATCH, app.url(&format!("/absences/{id}")), json!({"end": d(3)})).await.0, 204);
    assert_eq!(get(&mia, app.url(&format!("/absences?user={mia_id}"))).await[0]["end"], d(3));

    // calendar feeds: her own say why, a project's say who's away
    let (_, own) = send(&mia, Method::POST, app.url("/me/calendar-feeds"), json!({"kind": "mine"})).await;
    let cal = reqwest::get(own["url"].as_str().unwrap()).await.unwrap().text().await.unwrap();
    assert!(cal.contains("SUMMARY:Away: vacation") && cal.contains("SUMMARY:Away: training (afternoon)"), "{cal}");
    let (_, proj) = send(&tom, Method::POST, app.url("/me/calendar-feeds"), json!({"kind": "project", "projectKey": "ABS"})).await;
    let cal = reqwest::get(proj["url"].as_str().unwrap()).await.unwrap().text().await.unwrap();
    assert!(cal.contains("SUMMARY:Mia Member away") && !cal.contains("vacation"), "{cal}");

    assert_eq!(send(&mia, Method::DELETE, app.url(&format!("/absences/{id}")), json!(null)).await.0, 204);
    let _ = mia;
}
