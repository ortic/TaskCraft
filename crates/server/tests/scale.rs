//! Scale check: a workspace with 100 000 issues (history, comments, labels, time logs), and how
//! long the pages people use most take. Slow, so it's ignored by default:
//!
//! ```sh
//! cargo test --release --test scale -- --ignored
//! ```
//!
//! The timings go to `target/scale-report.md` (and the job summary in CI); a request slower than
//! its budget (about five times what it takes on a laptop) fails the test.
//! `TASKCRAFT_SCALE_ISSUES` changes the size; `TASKCRAFT_TEST_KEEP_DB=1` keeps the database to
//! look at query plans afterwards.

mod common;

use std::time::Instant;

use common::*;
use serde_json::json;
use uuid::Uuid;

/// Words for titles, descriptions and comments.
const WORDS: &str = "login payment invoice report export import search filter dashboard sprint backlog release \
    mobile android iphone browser timeout crash slow memory database migration upgrade customer \
    password email notification calendar timezone upload download permission admin settings \
    checkout cart shipping refund tax currency translation layout button dialog keyboard";

/// One project's issues, their history, comments, labels and time logs, written straight to the
/// database (the API would take hours).
async fn seed_project(app: &TestApp, workspace: Uuid, project: Uuid, count: i64, scrum: bool) {
    let db = &app.state.db;
    if scrum {
        // twenty closed two-week sprints and an active one
        sqlx::query(
            "INSERT INTO sprints (id, project_id, name, state, start_at, end_at, completed_at, position)
             SELECT gen_random_uuid(), $1, 'Sprint ' || s, CASE WHEN s = 21 THEN 'active' ELSE 'closed' END,
                    now() - ((22 - s) * 14) * interval '1 day', now() - ((21 - s) * 14) * interval '1 day',
                    CASE WHEN s = 21 THEN NULL ELSE now() - ((21 - s) * 14) * interval '1 day' END, s
               FROM generate_series(1, 21) s",
        )
        .bind(project)
        .execute(db)
        .await
        .unwrap();
    }
    // 60 % done, 15 % in progress, 25 % to do; created over two years
    sqlx::query(
        "WITH w AS (SELECT regexp_split_to_array($4, '\\s+') AS w),
              st AS (SELECT (SELECT id FROM statuses WHERE project_id = $1 AND category = 'todo' ORDER BY position LIMIT 1) AS todo,
                            (SELECT id FROM statuses WHERE project_id = $1 AND category = 'in_progress' ORDER BY position LIMIT 1) AS doing,
                            (SELECT id FROM statuses WHERE project_id = $1 AND category = 'done' ORDER BY position LIMIT 1) AS done),
              ty AS (SELECT array_agg(id ORDER BY id) AS ids FROM issue_types WHERE project_id = $1 AND level = 0),
              u AS (SELECT array_agg(id ORDER BY id) AS ids FROM users WHERE workspace_id = $2),
              sp AS (SELECT array_agg(id ORDER BY position) FILTER (WHERE state = 'closed') AS closed,
                            (array_agg(id) FILTER (WHERE state = 'active'))[1] AS active FROM sprints WHERE project_id = $1),
              n AS (SELECT n, now() - ((n * 7919) % (730 * 24 * 60)) * interval '1 minute' AS created,
                           CASE WHEN n % 20 < 12 THEN 'done' WHEN n % 20 < 15 THEN 'doing' ELSE 'todo' END AS cat
                      FROM generate_series(1, $3::int) n)
         INSERT INTO issues (id, workspace_id, project_id, number, title, description_text, description, type_id, status_id, priority,
                             assignee_id, reporter_id, sprint_id, story_points, rank, resolved_at, status_changed_at, created_by, created_at, updated_at)
         SELECT gen_random_uuid(), $2, $1, n.n,
                initcap(w.w[1 + (n.n * 7) % array_length(w.w, 1)]) || ' ' || w.w[1 + (n.n * 13) % array_length(w.w, 1)] || ' ' ||
                    w.w[1 + (n.n * 17) % array_length(w.w, 1)] || ' fails for ' || w.w[1 + (n.n * 23) % array_length(w.w, 1)] || ' ' || n.n,
                repeat(w.w[1 + (n.n * 3) % array_length(w.w, 1)] || ' ' || w.w[1 + (n.n * 11) % array_length(w.w, 1)] || ' happens when ', 8),
                NULL,
                ty.ids[1 + n.n % array_length(ty.ids, 1)],
                CASE n.cat WHEN 'done' THEN st.done WHEN 'doing' THEN st.doing ELSE st.todo END,
                (n.n % 5)::smallint,
                CASE WHEN n.n % 7 = 0 THEN NULL ELSE u.ids[1 + n.n % array_length(u.ids, 1)] END,
                u.ids[1 + (n.n * 3) % array_length(u.ids, 1)],
                CASE WHEN sp.closed IS NULL THEN NULL
                     WHEN n.cat = 'done' THEN sp.closed[1 + n.n % array_length(sp.closed, 1)]
                     WHEN n.n % 25 = 0 THEN sp.active END,
                (ARRAY[1, 2, 3, 5, 8])[1 + n.n % 5],
                n.n,
                CASE WHEN n.cat = 'done' THEN LEAST(n.created + ((n.n * 31) % (20 * 24 * 60)) * interval '1 minute', now()) END,
                n.created, u.ids[1 + (n.n * 3) % array_length(u.ids, 1)], n.created, n.created
           FROM n, w, st, ty, u, sp",
    )
    .bind(project)
    .bind(workspace)
    .bind(count as i32)
    .bind(WORDS)
    .execute(db)
    .await
    .unwrap();
    // an epic per hundred issues, holding seven in ten of them
    let epics = (count / 100).max(1);
    sqlx::query(
        "WITH ty AS (SELECT id FROM issue_types WHERE project_id = $1 AND level = 1 LIMIT 1),
              st AS (SELECT (SELECT id FROM statuses WHERE project_id = $1 AND category = 'done' ORDER BY position LIMIT 1) AS done,
                            (SELECT id FROM statuses WHERE project_id = $1 AND category = 'in_progress' ORDER BY position LIMIT 1) AS doing)
         INSERT INTO issues (id, workspace_id, project_id, number, title, type_id, status_id, rank, resolved_at, start_date, due_date, created_at, updated_at)
         SELECT gen_random_uuid(), $2, $1, $3 + n, 'Epic ' || n, ty.id, CASE WHEN n % 3 = 0 THEN st.doing ELSE st.done END, $3 + n,
                CASE WHEN n % 3 = 0 THEN NULL ELSE now() - (n % 300) * interval '1 day' END,
                current_date - (n % 300), current_date - (n % 300) + 60, now() - (n % 700) * interval '1 day', now()
           FROM generate_series(1, $4::int) n, ty, st",
    )
    .bind(project)
    .bind(workspace)
    .bind(count as i32)
    .bind(epics as i32)
    .execute(db)
    .await
    .unwrap();
    sqlx::query(
        "WITH e AS (SELECT array_agg(id ORDER BY number) AS ids FROM issues WHERE project_id = $1 AND number > $2)
         UPDATE issues i SET parent_id = e.ids[1 + i.number % array_length(e.ids, 1)] FROM e
          WHERE i.project_id = $1 AND i.number <= $2 AND i.number % 10 < 7",
    )
    .bind(project)
    .bind(count as i32)
    .execute(db)
    .await
    .unwrap();
    let count = count + epics;
    sqlx::query("UPDATE projects SET issue_seq = $2 WHERE id = $1").bind(project).bind(count as i32).execute(db).await.unwrap();
    // history: created, then the moves to in progress and done
    sqlx::query(
        "INSERT INTO issue_events (id, issue_id, project_id, actor_id, kind, field, new_value, created_at)
         SELECT gen_random_uuid(), i.id, i.project_id, i.created_by, 'created', 'issue',
                jsonb_build_object('status', (SELECT jsonb_build_object('id', id, 'name', name, 'category', category) FROM statuses
                                               WHERE project_id = i.project_id AND category = 'todo' ORDER BY position LIMIT 1)),
                i.created_at
           FROM issues i WHERE i.project_id = $1",
    )
    .bind(project)
    .execute(db)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO issue_events (id, issue_id, project_id, actor_id, kind, field, old_value, new_value, created_at)
         SELECT gen_random_uuid(), i.id, i.project_id, i.assignee_id, 'field', 'status', NULL::jsonb,
                (SELECT jsonb_build_object('id', id, 'name', name, 'category', category) FROM statuses
                  WHERE project_id = i.project_id AND category = 'in_progress' ORDER BY position LIMIT 1),
                i.created_at + (COALESCE(i.resolved_at, now()) - i.created_at) / 3
           FROM issues i JOIN statuses s ON s.id = i.status_id WHERE i.project_id = $1 AND s.category <> 'todo'
         UNION ALL
         SELECT gen_random_uuid(), i.id, i.project_id, i.assignee_id, 'field', 'status', NULL::jsonb,
                jsonb_build_object('id', s.id, 'name', s.name, 'category', s.category), i.resolved_at
           FROM issues i JOIN statuses s ON s.id = i.status_id WHERE i.project_id = $1 AND s.category = 'done'",
    )
    .bind(project)
    .execute(db)
    .await
    .unwrap();
    // two comments on every other issue, a label on every third, time logged on every fourth
    sqlx::query(
        "INSERT INTO comments (id, issue_id, author_id, body, body_text, created_at, updated_at)
         SELECT gen_random_uuid(), i.id, i.reporter_id, '{\"type\":\"doc\",\"content\":[]}', 'Looked into this ' || i.title, t, t
           FROM issues i, LATERAL (VALUES (i.created_at + interval '1 hour'), (i.created_at + interval '1 day')) c(t)
          WHERE i.project_id = $1 AND i.number % 2 = 0",
    )
    .bind(project)
    .execute(db)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO issue_labels (issue_id, label)
         SELECT id, (ARRAY['backend', 'frontend', 'customer', 'security', 'performance'])[1 + number % 5] FROM issues WHERE project_id = $1 AND number % 3 = 0",
    )
    .bind(project)
    .execute(db)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO worklogs (id, issue_id, user_id, minutes, work_date, created_at)
         SELECT gen_random_uuid(), id, assignee_id, 30 + number % 240, (created_at + interval '1 day')::date, created_at + interval '1 day'
           FROM issues WHERE project_id = $1 AND number % 4 = 0 AND assignee_id IS NOT NULL",
    )
    .bind(project)
    .execute(db)
    .await
    .unwrap();
}

struct Timing {
    name: String,
    path: String,
    ms: f64,
    budget: u64,
}

/// The middle of three runs (after one to warm up).
async fn time(c: &reqwest::Client, app: &TestApp, name: &str, path: &str, budget: u64) -> Timing {
    let mut runs = Vec::new();
    for i in 0..4 {
        let started = Instant::now();
        let res = c.get(app.url(path)).send().await.unwrap();
        let status = res.status();
        let body = res.bytes().await.unwrap();
        assert!(status.is_success(), "{path}: {status} {}", String::from_utf8_lossy(&body[..body.len().min(300)]));
        if i > 0 {
            runs.push(started.elapsed());
        }
    }
    runs.sort();
    Timing { name: name.into(), path: path.into(), ms: runs[1].as_secs_f64() * 1000.0, budget }
}

#[tokio::test]
#[ignore = "slow: seeds 100 000 issues (cargo test --release --test scale -- --ignored)"]
async fn a_hundred_thousand_issues() {
    let total: i64 = std::env::var("TASKCRAFT_SCALE_ISSUES").ok().and_then(|v| v.parse().ok()).unwrap_or(100_000);
    let app = spawn().await;
    let admin = app.setup(false).await;
    let workspace: Uuid = sqlx::query_scalar("SELECT id FROM workspaces LIMIT 1").fetch_one(&app.state.db).await.unwrap();
    let me: Uuid = sqlx::query_scalar("SELECT id FROM users LIMIT 1").fetch_one(&app.state.db).await.unwrap();
    sqlx::query(
        "INSERT INTO users (id, workspace_id, email, name, role) SELECT gen_random_uuid(), $1, 'person' || n || '@example.com', 'Person ' || n, 'member'
           FROM generate_series(1, 59) n",
    )
    .bind(workspace)
    .execute(&app.state.db)
    .await
    .unwrap();

    // one big scrum project with 60 %, the rest in eight smaller ones (scrum and kanban)
    let started = Instant::now();
    let big = total * 6 / 10;
    let mut projects = vec![("BIG", "scrum", big)];
    let small = ["WEB", "APP", "OPS", "SUP", "MKT", "FIN", "HR", "DOC"];
    for (i, key) in small.iter().enumerate() {
        projects.push((key, if i % 2 == 0 { "scrum" } else { "kanban" }, (total - big) / small.len() as i64));
    }
    for (key, template, count) in &projects {
        let p = post(&admin, app.url("/projects"), json!({"key": key, "name": format!("Project {key}"), "template": template})).await;
        let id: Uuid = p["id"].as_str().unwrap().parse().unwrap();
        seed_project(&app, workspace, id, *count, *template == "scrum").await;
    }
    sqlx::query(
        "INSERT INTO notifications (id, user_id, issue_id, kind, data, created_at)
         SELECT gen_random_uuid(), $1, id, 'assigned', '{}', created_at FROM issues WHERE number % 50 = 0",
    )
    .bind(me)
    .execute(&app.state.db)
    .await
    .ok();
    sqlx::query("UPDATE issues SET assignee_id = $1 WHERE number % 61 = 0").bind(me).execute(&app.state.db).await.unwrap();
    sqlx::query("ANALYZE").execute(&app.state.db).await.unwrap();
    let seeded = started.elapsed();
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM issues").fetch_one(&app.state.db).await.unwrap();
    let events: i64 = sqlx::query_scalar("SELECT count(*) FROM issue_events").fetch_one(&app.state.db).await.unwrap();
    let comments: i64 = sqlx::query_scalar("SELECT count(*) FROM comments").fetch_one(&app.state.db).await.unwrap();

    let board: Uuid = sqlx::query_scalar("SELECT b.id FROM boards b JOIN projects p ON p.id = b.project_id WHERE p.key = 'BIG' LIMIT 1")
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    let kanban: Uuid = sqlx::query_scalar("SELECT b.id FROM boards b JOIN projects p ON p.id = b.project_id WHERE p.key = 'APP' LIMIT 1")
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    let sprint: Uuid = sqlx::query_scalar("SELECT s.id FROM sprints s JOIN projects p ON p.id = s.project_id WHERE p.key = 'BIG' AND s.state = 'active'")
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    let mid = big / 2;
    let today = chrono::Utc::now().date_naive();
    let q = |tql: &str| format!("/search?q={}", urlencoding(tql));
    let checks: Vec<(&str, String, u64)> = vec![
        ("Project list", "/projects".into(), 500),
        ("Scrum board (active sprint)", format!("/boards/{board}"), 500),
        ("Kanban board", format!("/boards/{kanban}"), 500),
        ("Backlog", "/projects/BIG/backlog".into(), 1200),
        ("Issue", format!("/issues/BIG-{mid}"), 200),
        ("Issue comments", format!("/issues/BIG-{mid}/comments"), 200),
        ("Issue history", format!("/issues/BIG-{mid}/history"), 200),
        ("List: a project, newest first", q("project = BIG ORDER BY created DESC"), 600),
        ("List: open, by priority", q("project = BIG AND statusCategory != done ORDER BY priority DESC"), 600),
        ("List: assignee and label", q("assignee = currentUser() AND labels = backend"), 300),
        ("List: whole workspace by update", q("ORDER BY updated DESC"), 600),
        ("Text search", q("text ~ \"checkout\" AND project = BIG"), 500),
        ("Text search, whole workspace", q("text ~ \"refund timeout\""), 500),
        ("Grouped counts", format!("/search/groups?q={}&by=status", urlencoding("project = BIG")), 600),
        ("Command palette: key", format!("/quick?q=BIG-{mid}"), 200),
        ("Command palette: words", "/quick?q=invoice%20crash".into(), 300),
        ("Your work", "/my-work".into(), 400),
        ("Notifications", "/notifications".into(), 200),
        ("Activity", "/activity".into(), 200),
        ("Portfolio", "/portfolio".into(), 900),
        ("Burndown", format!("/sprints/{sprint}/burndown"), 900),
        ("Report: overview", "/projects/BIG/reports/overview".into(), 1000),
        ("Report: velocity", "/projects/BIG/reports/velocity".into(), 1000),
        ("Report: cumulative flow (90 days)", "/projects/BIG/reports/cumulative-flow?days=90".into(), 1000),
        ("Report: created vs resolved", "/projects/BIG/reports/created-resolved?days=90".into(), 300),
        ("Report: cycle time", "/projects/BIG/reports/cycle-time?days=90".into(), 500),
        ("Report: workload", "/projects/BIG/reports/workload".into(), 400),
        ("Time report (a month)", format!("/time-report?from={}&to={today}", today - chrono::Duration::days(30)), 300),
        ("Workspace statistics", "/admin/stats".into(), 400),
    ];
    let mut timings = Vec::new();
    for (name, path, budget) in checks {
        timings.push(time(&admin, &app, name, &path, budget).await);
    }

    let mut report = format!(
        "## Scale check\n\n{count} issues in {} projects, {events} history entries, {comments} comments; seeded in {:.0} s.\n\n| Request | ms | budget |\n|---|---:|---:|\n",
        projects.len(),
        seeded.as_secs_f64()
    );
    for t in &timings {
        let flag = if t.ms > t.budget as f64 { " ⚠" } else { "" };
        report += &format!("| {} <br><sub>`{}`</sub> | {:.0}{flag} | {} |\n", t.name, t.path, t.ms, t.budget);
    }
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target");
    let _ = std::fs::write(dir.join("scale-report.md"), &report);
    if let Ok(summary) = std::env::var("GITHUB_STEP_SUMMARY") {
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new().append(true).open(summary) {
            let _ = f.write_all(report.as_bytes());
        }
    }
    let slow: Vec<String> = timings.iter().filter(|t| t.ms > t.budget as f64).map(|t| format!("{} ({:.0} ms > {} ms)", t.name, t.ms, t.budget)).collect();
    assert!(slow.is_empty(), "slower than their budget: {}\n\n{report}", slow.join(", "));
}

fn urlencoding(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}
