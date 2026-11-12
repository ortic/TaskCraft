//! Scheduled automation, recurring issues from templates, service-level goals.

mod common;

use common::*;
use reqwest::Method;
use serde_json::{Value, json};

#[tokio::test]
async fn scheduled_rule_acts_on_matching_issues() {
    let app = spawn().await;
    let c = app.setup(false).await;
    post(&c, app.url("/projects"), json!({"key": "OPS", "name": "Ops", "template": "kanban"})).await;
    let (max, max_id) = app.user(&c, "Max Member", "max@example.com", "member").await;
    post(&c, app.url("/issues"), json!({"projectKey": "OPS", "title": "Renew certificate", "dueDate": "2020-01-01", "assigneeId": max_id})).await;
    post(&c, app.url("/issues"), json!({"projectKey": "OPS", "title": "Later", "dueDate": "2099-01-01", "assigneeId": max_id})).await;
    // a schedule needs a condition
    let (s, _) = send(
        &c,
        Method::POST,
        app.url("/projects/OPS/automation"),
        json!({"trigger": {"event": "schedule", "schedule": {"every": "day", "at": "08:00"}}, "actions": [{"action": "addLabel", "label": "x"}]}),
    )
    .await;
    assert_eq!(s, 400);
    post(
        &c,
        app.url("/projects/OPS/automation"),
        json!({"name": "Overdue", "trigger": {"event": "schedule", "schedule": {"every": "weekday", "at": "08:00"}}, "condition": "due < today() and category != done",
               "actions": [{"action": "addLabel", "label": "overdue"}, {"action": "notify", "to": "assignee", "message": "This is overdue"}]}),
    )
    .await;
    let rules = get(&c, app.url("/projects/OPS/automation")).await;
    assert!(rules[0]["nextRunAt"].is_string());
    sqlx::query("UPDATE automation_rules SET next_run_at = now() - interval '1 minute'").execute(&app.state.db).await.unwrap();
    taskcraft::schedule::tick(&app.state).await;
    assert_eq!(get(&c, app.url("/issues/OPS-1")).await["labels"], json!(["overdue"]));
    assert_eq!(get(&c, app.url("/issues/OPS-2")).await["labels"], json!([]));
    let n = get(&max, app.url("/notifications")).await;
    let auto: Vec<&Value> = n["items"].as_array().unwrap().iter().filter(|x| x["kind"] == "automation").collect();
    assert_eq!(auto.len(), 1);
    assert_eq!(auto[0]["data"]["message"], "This is overdue");
    let rules = get(&c, app.url("/projects/OPS/automation")).await;
    assert_eq!(rules[0]["runs"], 1);
    assert!(rules[0]["nextRunAt"].as_str().unwrap() > rules[0]["lastRunAt"].as_str().unwrap());
    // not due again right away
    taskcraft::schedule::tick(&app.state).await;
    assert_eq!(get(&c, app.url("/projects/OPS/automation")).await[0]["runs"], 1);
}

#[tokio::test]
async fn templates_and_recurring_issues() {
    let app = spawn().await;
    let c = app.setup(false).await;
    post(&c, app.url("/projects"), json!({"key": "WEB", "name": "Web", "template": "scrum"})).await;
    let web = get(&c, app.url("/projects/WEB")).await;
    let task = web["types"].as_array().unwrap().iter().find(|t| t["name"] == "Task").unwrap()["id"].clone();
    let (s, _) = send(&c, Method::POST, app.url("/projects/WEB/templates"), json!({"name": "Weekly", "schedule": {"every": "week", "weekday": 1}})).await;
    assert_eq!(s, 400, "recurring templates need a title");
    let t = post(
        &c,
        app.url("/projects/WEB/templates"),
        json!({"name": "Release checklist", "title": "Release {week}", "description": "Steps:\n\n- [ ] tag\n- [ ] notes",
               "fields": {"typeId": task, "priority": "high", "labels": ["release"], "dueInDays": 3, "bogus": 1},
               "subtasks": ["Tag the release", "Write release notes"], "schedule": {"every": "week", "at": "09:00", "weekday": 1}}),
    )
    .await;
    let tid = t["id"].as_str().unwrap().to_string();
    let list = get(&c, app.url("/projects/WEB/templates")).await;
    assert_eq!(list[0]["fields"].get("bogus"), None);
    assert!(list[0]["nextRunAt"].is_string());
    // created by hand, with a different title
    let made = post(&c, app.url("/issues"), json!({"projectKey": "WEB", "templateId": tid, "title": "Release 2.6"})).await;
    assert_eq!(made["title"], "Release 2.6");
    assert_eq!(made["priority"], "high");
    assert_eq!(made["labels"], json!(["release"]));
    assert!(made["dueDate"].is_string());
    let detail = get(&c, app.url(&format!("/issues/{}", made["key"].as_str().unwrap()))).await;
    assert_eq!(detail["children"].as_array().unwrap().len(), 2);
    assert!(serde_json::to_string(&detail["description"]).unwrap().contains("taskItem"));
    // the schedule creates one when due
    sqlx::query("UPDATE issue_templates SET next_run_at = now() - interval '1 minute'").execute(&app.state.db).await.unwrap();
    taskcraft::schedule::tick(&app.state).await;
    let r: Value = c.get(app.url("/search")).query(&[("q", "title ~ \"Release 20\" and type = Task")]).send().await.unwrap().json().await.unwrap();
    assert_eq!(r["total"], 1, "{r}");
    assert!(r["issues"][0]["title"].as_str().unwrap().contains("-W"));
    let list = get(&c, app.url("/projects/WEB/templates")).await;
    assert_eq!(list[0]["lastIssueKey"], r["issues"][0]["key"]);
    // other projects' templates can't be used
    post(&c, app.url("/projects"), json!({"key": "OPS", "name": "Ops", "template": "kanban"})).await;
    let (s, _) = send(&c, Method::POST, app.url("/issues"), json!({"projectKey": "OPS", "templateId": tid, "title": "x"})).await;
    assert_eq!(s, 400);
}

#[tokio::test]
async fn service_level_goals() {
    let app = spawn().await;
    let c = app.setup(false).await;
    post(&c, app.url("/projects"), json!({"key": "SUP", "name": "Support", "template": "tracker"})).await;
    let (s, _) = send(&c, Method::PATCH, app.url("/projects/SUP"), json!({"settings": {"sla": {"response": {"urgent": "soon"}}}})).await;
    assert_eq!(s, 400);
    patch(&c, app.url("/projects/SUP"), json!({"settings": {"sla": {"response": {"urgent": 1, "medium": 8}, "resolution": {"urgent": 4}}}})).await;
    post(&c, app.url("/issues"), json!({"projectKey": "SUP", "title": "Down", "priority": "urgent"})).await;
    post(&c, app.url("/issues"), json!({"projectKey": "SUP", "title": "Question", "priority": "medium"})).await;
    post(&c, app.url("/issues"), json!({"projectKey": "SUP", "title": "No goal", "priority": "low"})).await;
    // the urgent one came in 2 hours ago
    sqlx::query("UPDATE issues SET created_at = now() - interval '2 hours' WHERE title = 'Down'").execute(&app.state.db).await.unwrap();
    let down = get(&c, app.url("/issues/SUP-1")).await;
    let goals = down["sla"].as_array().unwrap();
    assert_eq!(goals.len(), 2);
    assert_eq!(goals[0]["goal"], "response");
    assert_eq!(goals[0]["state"], "breached");
    assert_eq!(goals[1]["state"], "running");
    assert_eq!(get(&c, app.url("/issues/SUP-3")).await["sla"], json!([]));
    let q = get(&c, app.url("/projects/SUP/sla")).await;
    assert_eq!(q["issues"][0]["issue"]["key"], "SUP-1", "the most urgent first");
    assert_eq!(q["issues"].as_array().unwrap().len(), 2);
    assert_eq!(q["counts"]["breached"], 1);
    // the lead hears about it once
    taskcraft::schedule::tick(&app.state).await;
    taskcraft::schedule::tick(&app.state).await;
    let n = get(&c, app.url("/notifications")).await;
    let sla: Vec<&Value> = n["items"].as_array().unwrap().iter().filter(|x| x["kind"] == "sla").collect();
    assert_eq!(sla.len(), 1);
    assert_eq!(sla[0]["data"]["goal"], "response");
    // a reply by someone else stops the response clock
    let (other, _) = app.user(&c, "Agent Smith", "agent@example.com", "member").await;
    post(&other, app.url("/issues/SUP-2/comments"), json!({"markdown": "Looking into it"})).await;
    let q = get(&c, app.url("/issues/SUP-2")).await;
    assert_eq!(q["sla"][0]["state"], "met");
}
