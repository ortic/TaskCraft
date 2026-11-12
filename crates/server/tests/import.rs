//! CSV import with field mapping.

mod common;

use common::*;
use reqwest::Method;
use serde_json::{Value, json};

const CSV: &str = "Issue key,Summary,Issue Type,Status,Priority,Assignee,Labels,Sprint,Parent,Story Points,Due date,Created,Severity,Description\n\
OLD-1,Checkout redesign,Epic,In Progress,High,max@example.com,,,,,,2026-01-05,,\n\
OLD-2,Cart rounds wrong,Defect,Closed,Highest,Max Member,bug checkout,Sprint 9,OLD-1,3,15.12.2026,2026-01-06 10:30,Critical,\"Steps:\n\n1. add items\"\n\
OLD-3,Unknown person,Task,Weird status,Medium,nobody@example.com,,Sprint 9,OLD-99,many,soon,,Minor,\n\
,,Task,,,,,,,,,,,\n";

#[tokio::test]
async fn csv_import_maps_fields_and_values() {
    let app = spawn().await;
    let c = app.setup(false).await;
    post(&c, app.url("/projects"), json!({"key": "SUP", "name": "Support", "template": "tracker"})).await;
    let (max, _) = app.user(&c, "Max Member", "max@example.com", "member").await;
    let pv = post(&c, app.url("/projects/SUP/import/preview"), json!({"csv": CSV})).await;
    assert_eq!(pv["total"], 4);
    let m = &pv["mapping"];
    assert_eq!(m["Issue key"], "key");
    assert_eq!(m["Summary"], "title");
    assert_eq!(m["Story Points"], "points");
    assert!(m["Severity"].as_str().unwrap().starts_with("custom:"), "custom fields by name");
    let mut mapping = m.as_object().unwrap().clone();
    // one empty title: an error; dry run writes nothing
    let dry = post(&c, app.url("/projects/SUP/import"), json!({"csv": CSV, "mapping": mapping, "dryRun": true})).await;
    assert_eq!(dry["errors"].as_array().unwrap().len(), 1, "{dry}");
    assert_eq!(dry["errors"][0]["row"], 5);
    let warnings: Vec<String> = dry["warnings"].as_array().unwrap().iter().map(|w| w["message"].as_str().unwrap().to_string()).collect();
    assert!(warnings.iter().any(|w| w.contains("nobody@example.com")), "{warnings:?}");
    assert!(warnings.iter().any(|w| w.contains("Weird status")));
    assert!(warnings.iter().any(|w| w.contains("OLD-99")));
    let r: Value = c.get(app.url("/search")).query(&[("q", "project = SUP")]).send().await.unwrap().json().await.unwrap();
    assert_eq!(r["total"], 0);
    // ignore a column
    mapping.insert("Description".into(), json!("description"));
    let done = post(&c, app.url("/projects/SUP/import"), json!({"csv": CSV, "mapping": mapping})).await;
    assert_eq!(done["created"], 3, "{done}");
    let cart = get(&c, app.url("/issues/OLD-2")).await;
    assert_eq!(cart["key"], "SUP-2", "the source key keeps working");
    assert_eq!(cart["type"]["name"], "Bug", "Defect → Bug");
    assert_eq!(cart["status"]["category"], "done", "Closed → a done status");
    assert_eq!(cart["priority"], "urgent");
    assert_eq!(cart["assignee"]["name"], "Max Member");
    assert_eq!(cart["labels"], json!(["bug", "checkout"]));
    assert_eq!(cart["sprint"]["name"], "Sprint 9", "missing sprints are created");
    assert_eq!(cart["parent"]["key"], "SUP-1");
    assert_eq!(cart["storyPoints"], 3.0);
    assert_eq!(cart["dueDate"], "2026-12-15");
    assert!(cart["createdAt"].as_str().unwrap().starts_with("2026-01-06T10:30"));
    assert!(serde_json::to_string(&cart["description"]).unwrap().contains("orderedList"));
    let sup = get(&c, app.url("/projects/SUP")).await;
    let sev = sup["fields"].as_array().unwrap().iter().find(|f| f["name"] == "Severity").unwrap().clone();
    let critical = sev["options"].as_array().unwrap().iter().find(|o| o["label"] == "Critical").unwrap()["id"].clone();
    assert_eq!(cart["custom"][sev["id"].as_str().unwrap()], critical);
    let unknown = get(&c, app.url("/issues/OLD-3")).await;
    assert_eq!(unknown["assignee"], Value::Null);
    assert_eq!(unknown["status"]["name"], "Weird status", "missing statuses are created");
    assert_eq!(unknown["status"]["category"], "todo");
    // imports tell nobody
    assert_eq!(get(&max, app.url("/notifications")).await["unread"], 0);
    // a mapping must include the title, and fields must exist
    let (s, _) = send(&c, Method::POST, app.url("/projects/SUP/import"), json!({"csv": CSV, "mapping": {"Summary": "ignore", "Issue key": "ignore"}})).await;
    assert_eq!(s, 400);
    let (s, _) = send(&c, Method::POST, app.url("/projects/SUP/import"), json!({"csv": CSV, "mapping": {"Summary": "bogus"}})).await;
    assert_eq!(s, 400);
    // only project admins import
    let (s, _) = send(&max, Method::POST, app.url("/projects/SUP/import"), json!({"csv": CSV})).await;
    assert_eq!(s, 403);
    // TaskCraft's own CSV export imports back
    let csv = c.get(app.url("/search.csv")).query(&[("q", "project = SUP")]).send().await.unwrap().text().await.unwrap();
    post(&c, app.url("/projects"), json!({"key": "COPY", "name": "Copy", "template": "tracker"})).await;
    let back = post(&c, app.url("/projects/COPY/import"), json!({"csv": csv})).await;
    assert_eq!(back["created"], 3, "{back}");
    let r: Value = c.get(app.url("/search")).query(&[("q", "project = COPY and parent is not empty")]).send().await.unwrap().json().await.unwrap();
    assert_eq!(r["total"], 1, "parents are found by the exported keys");
}

/// What exports from other tools look like in practice: invited and departed people, a column per
/// label, times in seconds, parents by numeric id, statuses the project doesn't have.
const MIGRATION: &str = "Summary,Issue key,Issue id,Issue Type,Status,Assignee,Reporter,Created,Resolved,Labels,Labels,Original estimate,Time Spent,Parent\n\
Platform,OLD-1,9001,Epic,Selected for sprint,Ida Invited,Fritz Former,02/Apr/21 9:10 AM,,,,,,\n\
Build it,OLD-2,9002,Task,Wird geprüft,Ida Invited,Fritz Former,03/Apr/21 9:10 AM,,api,backend,28800,5400,9001\n\
Ship it,OLD-3,9003,Task,Done,Fritz Former,Ida Invited,04/Apr/21 9:10 AM,05/Apr/21 4:00 PM,,,,86400,9001\n";

#[tokio::test]
async fn migrations_keep_people_times_parents_and_statuses() {
    let app = spawn().await;
    let c = app.setup(false).await;
    post(&c, app.url("/projects"), json!({"key": "MIG", "name": "Migration", "template": "kanban"})).await;
    let ida = post(&c, app.url("/users"), json!({"name": "Ida Invited", "email": "ida@example.com"})).await["id"].as_str().unwrap().to_string();
    let (_, fritz) = app.user(&c, "Fritz Former", "fritz@example.com", "member").await;
    patch(&c, app.url(&format!("/users/{fritz}")), json!({"status": "deactivated"})).await;

    let pv = post(&c, app.url("/projects/MIG/import/preview"), json!({"csv": MIGRATION})).await;
    let m = &pv["mapping"];
    assert_eq!((m["Issue key"].as_str(), m["Issue id"].as_str()), (Some("key"), Some("id")));
    assert_eq!(m["Labels"], "labels", "a repeated column keeps its field");
    assert_eq!((m["Original estimate"].as_str(), m["Time Spent"].as_str()), (Some("estimateSeconds"), Some("spentSeconds")), "seconds are recognised");
    let dry = post(&c, app.url("/projects/MIG/import"), json!({"csv": MIGRATION, "mapping": m, "dryRun": true})).await;
    assert_eq!((dry["created"].as_i64(), dry["errors"].as_array().unwrap().len()), (Some(3), 0), "{dry}");
    let warnings: Vec<String> = dry["warnings"].as_array().unwrap().iter().map(|w| w["message"].as_str().unwrap().to_string()).collect();
    assert_eq!(warnings.iter().filter(|w| w.contains("can't see this project")).count(), 2, "once per person: {warnings:?}");
    assert!(warnings.iter().any(|w| w.contains("new status “Wird geprüft” (in progress)")), "{warnings:?}");

    let done = post(&c, app.url("/projects/MIG/import"), json!({"csv": MIGRATION, "mapping": m})).await;
    assert_eq!((done["created"].as_i64(), done["errors"].as_array().unwrap().len()), (Some(3), 0), "the real run matches the dry run: {done}");
    let epic = get(&c, app.url("/issues/OLD-1")).await;
    assert_eq!(epic["status"]["name"], "Selected", "“Selected for sprint” → Selected");
    let build = get(&c, app.url("/issues/OLD-2")).await;
    assert_eq!((build["status"]["name"].as_str(), build["status"]["category"].as_str()), (Some("Wird geprüft"), Some("in_progress")));
    assert_eq!(build["assignee"]["id"], ida.as_str(), "invited people keep their issues");
    assert_eq!(build["reporter"]["id"], fritz.as_str(), "so do people who left");
    assert_eq!(build["labels"], json!(["api", "backend"]));
    assert_eq!((build["estimateMinutes"].as_i64(), build["spentMinutes"].as_i64()), (Some(480), Some(90)));
    assert_eq!(build["parent"]["key"], epic["key"], "parents by the other tool's id");
    // logged time stays with whoever did it, on the day the issue was resolved
    let logs = get(&c, app.url("/issues/OLD-3/worklogs")).await;
    assert_eq!(
        (logs[0]["user"]["id"].as_str(), logs[0]["date"].as_str(), logs[0]["minutes"].as_i64()),
        (Some(fritz.as_str()), Some("2021-04-05"), Some(1440)),
        "{logs}"
    );
    // the new status sits before Done on the board
    let config: Value =
        sqlx::query_scalar("SELECT b.config FROM boards b JOIN projects p ON p.id = b.project_id WHERE p.key = 'MIG'").fetch_one(&app.state.db).await.unwrap();
    let names: Vec<&str> = config["columns"].as_array().unwrap().iter().map(|c| c["name"].as_str().unwrap()).collect();
    let at = |n: &str| names.iter().position(|x| *x == n).unwrap_or_else(|| panic!("{n} not in {names:?}"));
    assert!(at("Wird geprüft") < at("Done"), "{names:?}");

    // keys of this same project keep their numbers (gaps and all); a taken number gets a new one
    let same = "Summary,Issue key\nFive,MIG-5\nNine,MIG-9\nTwo again,MIG-2\n";
    let r = post(&c, app.url("/projects/MIG/import"), json!({"csv": same})).await;
    assert_eq!(r["created"], 3, "{r}");
    assert_eq!(get(&c, app.url("/issues/MIG-5")).await["title"], "Five");
    assert_eq!(get(&c, app.url("/issues/MIG-9")).await["title"], "Nine");
    assert!(r["warnings"].to_string().contains("MIG-2 is taken"), "{r}");
    let next = post(&c, app.url("/issues"), json!({"projectKey": "MIG", "title": "After the import"})).await;
    assert_eq!(next["key"], "MIG-13", "numbering goes on after the highest number");
}
