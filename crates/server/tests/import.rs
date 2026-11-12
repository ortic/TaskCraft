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
    assert_eq!(unknown["status"]["name"], "New", "unknown statuses get the first one");
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
