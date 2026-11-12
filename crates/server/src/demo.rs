//! Sample data: a few people and four projects (scrum, kanban, a tracker and a business plan)
//! with a closed and a running sprint, history spread over the past weeks, comments, time
//! logs, links and automation rules — enough to try boards, backlogs and reports right away.

use chrono::{DateTime, Duration, Utc};
use serde_json::{Map, Value, json};
use uuid::Uuid;

use crate::app::{AppState, new_id};
use crate::auth::User;
use crate::issues::{self, Actor, NewIssue};
use crate::routes::projects::{NewProjectSpec, create_project_spec};

struct Seed<'a> {
    state: &'a AppState,
    ws: Uuid,
    people: Vec<(Uuid, String)>,
}

fn ago(days: f64) -> DateTime<Utc> {
    Utc::now() - Duration::minutes((days * 1440.0) as i64)
}

impl Seed<'_> {
    fn actor(&self, who: usize) -> Actor {
        Actor { user: Some(self.people[who % self.people.len()].0), workspace: self.ws, depth: 9 }
    }

    /// Runs `f`, then moves everything it recorded on `issue` to `at`.
    async fn at(&self, issue: Uuid, at: DateTime<Utc>, mark: DateTime<Utc>) -> anyhow::Result<()> {
        sqlx::query("UPDATE issue_events SET created_at = $3 + (created_at - $2) / 1000 WHERE issue_id = $1 AND created_at >= $2")
            .bind(issue)
            .bind(mark)
            .bind(at)
            .execute(&self.state.db)
            .await?;
        sqlx::query(
            "UPDATE issues SET updated_at = $2, status_changed_at = CASE WHEN status_changed_at >= $3 THEN $2 ELSE status_changed_at END,
                    resolved_at = CASE WHEN resolved_at >= $3 THEN $2 ELSE resolved_at END WHERE id = $1",
        )
        .bind(issue)
        .bind(at)
        .bind(mark)
        .execute(&self.state.db)
        .await?;
        Ok(())
    }

    async fn now(&self) -> anyhow::Result<DateTime<Utc>> {
        Ok(sqlx::query_scalar("SELECT clock_timestamp()").fetch_one(&self.state.db).await?)
    }

    async fn issue(&self, project: Uuid, who: usize, created: f64, title: &str, md: &str, fields: Value) -> anyhow::Result<Uuid> {
        let mark = self.now().await?;
        let fields: Map<String, Value> = fields.as_object().cloned().unwrap_or_default();
        let s = issues::create(
            self.state,
            &self.actor(who),
            NewIssue { project, title: title.into(), description: (!md.is_empty()).then(|| crate::doc::from_markdown(md)), fields },
        )
        .await?;
        sqlx::query("UPDATE issues SET created_at = $2 WHERE id = $1").bind(s.id).bind(ago(created)).execute(&self.state.db).await?;
        self.at(s.id, ago(created), mark).await?;
        Ok(s.id)
    }

    async fn change(&self, issue: Uuid, who: usize, days_ago: f64, patch: Value) -> anyhow::Result<()> {
        let mark = self.now().await?;
        issues::update(self.state, &self.actor(who), issue, patch.as_object().expect("object")).await?;
        self.at(issue, ago(days_ago), mark).await
    }

    async fn comment(&self, issue: Uuid, who: usize, days_ago: f64, md: &str) -> anyhow::Result<()> {
        let mark = self.now().await?;
        let id = crate::routes::comments::add(self.state, &self.actor(who), issue, crate::doc::from_markdown(md)).await?;
        sqlx::query("UPDATE comments SET created_at = $2, updated_at = $2 WHERE id = $1").bind(id).bind(ago(days_ago)).execute(&self.state.db).await?;
        // backdated notifications would be due for e-mail right away: they count as mailed
        sqlx::query("UPDATE notifications SET created_at = $2, emailed_at = COALESCE(emailed_at, now()) WHERE issue_id = $1 AND created_at >= $3")
            .bind(issue)
            .bind(ago(days_ago))
            .bind(mark)
            .execute(&self.state.db)
            .await?;
        self.at(issue, ago(days_ago), mark).await
    }

    async fn log(&self, issue: Uuid, who: usize, days_ago: f64, minutes: i32) -> anyhow::Result<()> {
        sqlx::query("INSERT INTO worklogs (id, issue_id, user_id, minutes, work_date) VALUES ($1, $2, $3, $4, $5)")
            .bind(new_id())
            .bind(issue)
            .bind(self.people[who % self.people.len()].0)
            .bind(minutes)
            .bind(ago(days_ago).date_naive())
            .execute(&self.state.db)
            .await?;
        sqlx::query("UPDATE issues SET spent_minutes = (SELECT COALESCE(sum(minutes), 0) FROM worklogs WHERE issue_id = $1) WHERE id = $1")
            .bind(issue)
            .execute(&self.state.db)
            .await?;
        Ok(())
    }

    async fn link(&self, src: Uuid, tgt: Uuid, kind: &str) -> anyhow::Result<()> {
        sqlx::query("INSERT INTO issue_links (id, source_id, target_id, kind) VALUES ($1, $2, $3, $4) ON CONFLICT DO NOTHING")
            .bind(new_id())
            .bind(src)
            .bind(tgt)
            .bind(kind)
            .execute(&self.state.db)
            .await?;
        Ok(())
    }
}

async fn ids(state: &AppState, project: Uuid) -> anyhow::Result<(std::collections::HashMap<String, Uuid>, std::collections::HashMap<String, Uuid>)> {
    let s: Vec<(String, Uuid)> = sqlx::query_as("SELECT name, id FROM statuses WHERE project_id = $1").bind(project).fetch_all(&state.db).await?;
    let t: Vec<(String, Uuid)> = sqlx::query_as("SELECT name, id FROM issue_types WHERE project_id = $1").bind(project).fetch_all(&state.db).await?;
    Ok((s.into_iter().collect(), t.into_iter().collect()))
}

pub async fn seed(state: &AppState, owner: Uuid) -> anyhow::Result<()> {
    let user: User =
        sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT {} FROM users WHERE id = $1", crate::auth::USER_COLUMNS))).bind(owner).fetch_one(&state.db).await?;
    let ws = user.workspace_id;
    let mut people = vec![(owner, user.name.clone())];
    for (name, title, email) in [
        ("Mira Okafor", "Product manager", "mira"),
        ("Jonas Weber", "Backend engineer", "jonas"),
        ("Lena Fischer", "Frontend engineer", "lena"),
        ("Sam Patel", "Designer", "sam"),
        ("Yuki Tanaka", "Support lead", "yuki"),
    ] {
        let id = new_id();
        // sample people's addresses aren't real: no notification e-mails for them
        sqlx::query("INSERT INTO users (id, workspace_id, email, name, title, role, status, preferences) VALUES ($1, $2, $3, $4, $5, 'member', 'active', $6)")
            .bind(id)
            .bind(ws)
            .bind(format!("{email}@example.com"))
            .bind(name)
            .bind(title)
            .bind(json!({"email": "off"}))
            .execute(&state.db)
            .await?;
        people.push((id, name.to_string()));
    }
    let team = new_id();
    sqlx::query("INSERT INTO teams (id, workspace_id, name, description, color) VALUES ($1, $2, 'Platform team', 'Builds the web platform', '#4f6bed')")
        .bind(team)
        .bind(ws)
        .execute(&state.db)
        .await?;
    for (id, _) in people.iter().take(5) {
        sqlx::query("INSERT INTO team_members (team_id, user_id) VALUES ($1, $2)").bind(team).bind(id).execute(&state.db).await?;
    }
    let s = Seed { state, ws, people: people.clone() };
    let p = |i: usize| json!(people[i].0);

    // ---------- WEB: scrum ----------
    let (web, _) = create_project_spec(
        state,
        &user,
        NewProjectSpec {
            key: "WEB".into(),
            name: "Web Platform".into(),
            template: "scrum".into(),
            description: Some("Customer-facing web shop and accounts".into()),
            icon: "globe".into(),
            color: "#4f6bed".into(),
        },
    )
    .await?;
    let (st, ty) = ids(state, web).await?;
    let sprint1: Uuid = sqlx::query_scalar("SELECT id FROM sprints WHERE project_id = $1").bind(web).fetch_one(&state.db).await?;
    sqlx::query(
        "UPDATE sprints SET name = 'WEB Sprint 1', goal = 'Guest checkout works end to end', state = 'active', start_at = $2, end_at = $3 WHERE id = $1",
    )
    .bind(sprint1)
    .bind(ago(21.0))
    .bind(ago(7.0))
    .execute(&state.db)
    .await?;
    let sprint2 = new_id();
    let sprint3 = new_id();
    sqlx::query("INSERT INTO sprints (id, project_id, name, goal, state, position) VALUES ($1, $3, 'WEB Sprint 2', 'Faster pages and saved carts', 'future', 2), ($2, $3, 'WEB Sprint 3', NULL, 'future', 3)")
        .bind(sprint2)
        .bind(sprint3)
        .bind(web)
        .execute(&state.db)
        .await?;
    let v24 = new_id();
    let v25 = new_id();
    sqlx::query("INSERT INTO versions (id, project_id, name, description, status, release_date, released_at, position) VALUES ($1, $3, '2.4', 'Guest checkout', 'unreleased', $4, NULL, 1), ($2, $3, '2.5', 'Performance and saved carts', 'unreleased', $5, NULL, 2)")
        .bind(v24)
        .bind(v25)
        .bind(web)
        .bind(ago(6.0).date_naive())
        .bind(ago(-20.0).date_naive())
        .execute(&state.db)
        .await?;
    let (todo, doing, review, done) = (st["To do"], st["In progress"], st["In review"], st["Done"]);
    let (epic, story, task, bug, sub) = (ty["Epic"], ty["Story"], ty["Task"], ty["Bug"], ty["Sub-task"]);
    let e_checkout = s
        .issue(
            web,
            1,
            30.0,
            "Checkout redesign",
            "One page checkout with fewer steps and guest purchases.",
            json!({"typeId": epic, "priority": "high", "startDate": ago(21.0).date_naive(), "dueDate": ago(-10.0).date_naive()}),
        )
        .await?;
    let e_accounts = s
        .issue(
            web,
            1,
            30.0,
            "Accounts & sign-in",
            "Passwordless sign-in, profile page, saved addresses.",
            json!({"typeId": epic, "startDate": ago(7.0).date_naive(), "dueDate": ago(-30.0).date_naive()}),
        )
        .await?;
    let e_perf = s
        .issue(
            web,
            2,
            29.0,
            "Performance",
            "Largest contentful paint below 2 s on mobile.",
            json!({"typeId": epic, "startDate": ago(3.0).date_naive(), "dueDate": ago(-24.0).date_naive()}),
        )
        .await?;

    // sprint 1 (closed): planned 3 weeks ago
    let s1: Vec<(&str, Uuid, Uuid, f64, usize, &str)> = vec![
        (
            "Guest checkout without an account",
            story,
            e_checkout,
            8.0,
            2,
            "As a shopper I want to buy without creating an account so that I finish faster.\n\n**Acceptance criteria**\n\n- [x] E-mail only\n- [x] Order confirmation page\n- [ ] Offer an account afterwards",
        ),
        ("Address form validates postal codes per country", story, e_checkout, 5.0, 3, ""),
        ("Payment step shows the order summary", story, e_checkout, 3.0, 3, ""),
        ("Order confirmation e-mail", task, e_checkout, 3.0, 2, "Use the transactional mail template; include the order number."),
        (
            "Cart total rounds wrong for 3+ items with discounts",
            bug,
            e_checkout,
            2.0,
            2,
            "**Steps to reproduce**\n\n1. Add three discounted items\n2. Open the cart\n\n**Expected** 59.97\n\n**Actual** 59.96",
        ),
        ("Checkout analytics events", task, e_checkout, 2.0, 3, ""),
    ];
    let mut s1_ids = Vec::new();
    for (i, (title, t, parent, pts, who, md)) in s1.iter().enumerate() {
        let id = s.issue(web, 1, 23.0 - i as f64 * 0.2, title, md, json!({"typeId": t, "parentId": parent, "storyPoints": pts, "versionId": v24, "assigneeId": p(*who), "priority": if *t == bug { "high" } else { "medium" }})).await?;
        s.change(id, 1, 21.5, json!({"sprintId": sprint1})).await?;
        s1_ids.push(id);
    }
    // sprint 1 progress
    let script: &[(usize, f64, Uuid)] = &[
        (0, 20.0, doing),
        (4, 19.5, doing),
        (4, 18.0, review),
        (4, 17.2, done),
        (1, 18.5, doing),
        (0, 16.0, review),
        (0, 15.0, done),
        (1, 14.0, review),
        (1, 12.5, done),
        (2, 13.0, doing),
        (3, 12.0, doing),
        (3, 10.0, done),
        (2, 9.0, review),
        (2, 8.0, done),
    ];
    for (idx, d, status) in script {
        s.change(s1_ids[*idx], 2 + idx % 2, *d, json!({"statusId": status})).await?;
    }
    s.comment(s1_ids[4], 2, 19.4, "Found it: we rounded each line before applying the discount. Fix is in review.").await?;
    s.comment(s1_ids[0], 1, 15.1, "Tested on mobile and desktop, looks great. 🎉").await?;
    // added mid-sprint
    let late = s
        .issue(
            web,
            4,
            16.0,
            "Coupon field accepts lower-case codes",
            "",
            json!({"typeId": bug, "parentId": e_checkout, "storyPoints": 1.0, "assigneeId": p(3), "priority": "medium", "versionId": v24}),
        )
        .await?;
    s.change(late, 1, 16.0, json!({"sprintId": sprint1})).await?;
    s.change(late, 3, 11.0, json!({"statusId": done})).await?;
    for (i, id) in s1_ids.iter().enumerate() {
        s.log(*id, 2 + i % 2, 20.0 - i as f64, 90 + (i as i32 * 45) % 240).await?;
    }
    // close sprint 1: the analytics task moves on
    s.change(s1_ids[5], 1, 7.0, json!({"sprintId": sprint2})).await?;
    let stats = json!({});
    sqlx::query("UPDATE sprints SET state = 'closed', completed_at = $2, stats = $3 WHERE id = $1")
        .bind(sprint1)
        .bind(ago(7.0))
        .bind(stats)
        .execute(&state.db)
        .await?;
    if let Ok(st) = crate::reports::sprint_stats(state, sprint1).await {
        sqlx::query("UPDATE sprints SET stats = $2 WHERE id = $1").bind(sprint1).bind(st).execute(&state.db).await?;
    }
    sqlx::query("UPDATE versions SET status = 'released', released_at = $2 WHERE id = $1").bind(v24).bind(ago(6.0)).execute(&state.db).await?;
    sqlx::query("UPDATE sprints SET state = 'active', start_at = $2, end_at = $3 WHERE id = $1")
        .bind(sprint2)
        .bind(ago(6.0))
        .bind(ago(-8.0))
        .execute(&state.db)
        .await?;

    // sprint 2 (running)
    let s2: Vec<(&str, Uuid, Uuid, f64, usize, &str)> = vec![
        ("Lazy-load product images below the fold", story, e_perf, 3.0, 3, ""),
        ("Cache category pages at the edge", story, e_perf, 5.0, 2, "Purge on product updates. Measure hit rate."),
        ("Save the cart for signed-in shoppers", story, e_accounts, 8.0, 2, "As a returning shopper I want my cart to be there on another device."),
        ("Sign in with a one-time code", story, e_accounts, 5.0, 3, ""),
        ("Profile page: saved addresses", story, e_accounts, 3.0, 4, ""),
        ("Search results flicker on slow connections", bug, e_perf, 2.0, 3, "Reproducible with network throttling (Slow 3G)."),
        ("Upgrade the image service", task, e_perf, 2.0, 2, ""),
    ];
    let mut s2_ids = vec![s1_ids[5]];
    for (i, (title, t, parent, pts, who, md)) in s2.iter().enumerate() {
        let id = s.issue(web, 1, 9.0 - i as f64 * 0.3, title, md, json!({"typeId": t, "parentId": parent, "storyPoints": pts, "versionId": v25, "assigneeId": p(*who), "priority": if *t == bug { "high" } else { "medium" }, "labels": if i % 3 == 0 { json!(["frontend"]) } else { json!(["backend"]) }})).await?;
        s.change(id, 1, 6.2, json!({"sprintId": sprint2})).await?;
        s2_ids.push(id);
    }
    let script2: &[(usize, f64, Uuid)] = &[
        (1, 5.5, doing),
        (1, 4.0, review),
        (1, 3.2, done),
        (7, 5.0, doing),
        (7, 3.5, done),
        (2, 4.5, doing),
        (2, 2.0, review),
        (3, 3.0, doing),
        (6, 2.5, doing),
        (6, 1.0, done),
        (4, 1.5, doing),
        (0, 1.0, doing),
    ];
    for (idx, d, status) in script2 {
        s.change(s2_ids[*idx], 2 + idx % 3, *d, json!({"statusId": status})).await?;
    }
    s.change(s2_ids[3], 1, 2.8, json!({"storyPoints": 13.0})).await?;
    s.comment(s2_ids[3], 2, 2.7, "Bigger than we thought: carts must merge when someone signs in with items already in a guest cart. Re-estimated to 13.")
        .await?;
    s.comment(
        s2_ids[3],
        1,
        2.5,
        &format!("Thanks. Let's keep it in the sprint but split out the merge rules if it drags. @{} can you sketch the merge prompt?", people[4].1),
    )
    .await?;
    s.comment(s2_ids[5], 4, 0.6, "Design for the profile page is in the wiki; ping me for questions.").await?;
    for (i, id) in s2_ids.iter().enumerate() {
        s.log(*id, 2 + i % 3, 5.0 - (i as f64 * 0.5), 60 + (i as i32 * 35) % 200).await?;
    }
    s.link(s2_ids[2], s2_ids[1], "blocks").await?;
    let blocker = s.issue(web, 2, 2.0, "Edge cache config needs the new CDN account", "", json!({"typeId": task, "parentId": e_perf, "assigneeId": p(0), "priority": "urgent", "sprintId": sprint2, "storyPoints": 1.0, "dueDate": ago(-1.0).date_naive()})).await?;
    s.link(blocker, s2_ids[2], "blocks").await?;
    for (t, who) in [("Write the merge rules", 2), ("Merge prompt UI", 4)] {
        s.issue(web, 2, 2.4, t, "", json!({"typeId": sub, "parentId": s2_ids[3], "assigneeId": p(who), "sprintId": sprint2})).await?;
    }

    // backlog and the next sprint
    let backlog: Vec<(&str, Uuid, Option<Uuid>, Option<f64>, &str, Option<Uuid>)> = vec![
        ("Apple Pay and Google Pay", story, Some(e_checkout), Some(8.0), "high", Some(sprint3)),
        ("Remember the last payment method", story, Some(e_checkout), Some(3.0), "medium", Some(sprint3)),
        ("Prefetch the next page of search results", story, Some(e_perf), Some(3.0), "low", None),
        ("Delete my account", story, Some(e_accounts), Some(5.0), "medium", None),
        ("Two-step verification for shop admins", story, Some(e_accounts), Some(8.0), "high", None),
        ("404 page suggests similar products", story, None, Some(2.0), "low", None),
        ("Typo in the newsletter footer", bug, None, Some(1.0), "low", None),
        ("Gift cards", story, Some(e_checkout), None, "medium", None),
        ("Dark mode for the shop", story, None, None, "none", None),
    ];
    for (i, (title, t, parent, pts, prio, sprint)) in backlog.iter().enumerate() {
        let mut f = json!({"typeId": t, "priority": prio, "storyPoints": pts});
        if let Some(pa) = parent {
            f["parentId"] = json!(pa);
        }
        if let Some(sp) = sprint {
            f["sprintId"] = json!(sp);
        }
        s.issue(web, i % 3, 12.0 - i as f64, title, "", f).await?;
    }
    sqlx::query("INSERT INTO automation_rules (id, project_id, name, trigger, condition, actions, created_by) VALUES ($1, $2, 'New bugs get the triage label', $3, 'type = Bug', $4, $5)")
        .bind(new_id())
        .bind(web)
        .bind(json!({"event": "created"}))
        .bind(json!([{"action": "addLabel", "label": "triage"}]))
        .bind(owner)
        .execute(&state.db)
        .await?;
    let _ = (todo, review);

    // ---------- OPS: kanban ----------
    let (ops, _) = create_project_spec(
        state,
        &user,
        NewProjectSpec {
            key: "OPS".into(),
            name: "Operations".into(),
            template: "kanban".into(),
            description: Some("Infrastructure, releases and on-call".into()),
            icon: "server".into(),
            color: "#0e9f6e".into(),
        },
    )
    .await?;
    let (st, ty) = ids(state, ops).await?;
    let ops_items: Vec<(&str, &str, &str, usize, &str)> = vec![
        ("Rotate the database credentials", "Selected", "high", 2, "security"),
        ("Postgres 17 upgrade on staging", "In progress", "medium", 2, "database"),
        ("Alert when disk usage passes 80 %", "Review", "medium", 0, "monitoring"),
        ("Nightly backup restore test", "Done", "high", 2, "backup"),
        ("Document the on-call handover", "Backlog", "low", 1, "docs"),
        ("Reduce container image size", "Backlog", "low", 3, "build"),
        ("TLS certificate renewal automation", "Done", "urgent", 0, "security"),
        ("Move logs to the new retention policy", "In progress", "medium", 0, "monitoring"),
        ("Load test the checkout API", "Selected", "high", 3, "performance"),
        ("Clean up unused DNS records", "Backlog", "none", 1, ""),
    ];
    for (i, (title, status, prio, who, label)) in ops_items.iter().enumerate() {
        let id = s
            .issue(
                ops,
                *who,
                25.0 - i as f64 * 2.0,
                title,
                "",
                json!({"typeId": ty["Task"], "priority": prio, "assigneeId": p(*who), "labels": if label.is_empty() { json!([]) } else { json!([label]) }}),
            )
            .await?;
        let path: &[&str] = match *status {
            "Selected" => &["Selected"],
            "In progress" => &["Selected", "In progress"],
            "Review" => &["Selected", "In progress", "Review"],
            "Done" => &["Selected", "In progress", "Review", "Done"],
            _ => &[],
        };
        for (j, stn) in path.iter().enumerate() {
            s.change(id, *who, 22.0 - i as f64 * 2.0 + j as f64 * 1.3, json!({"statusId": st[*stn]})).await?;
        }
    }

    // ---------- SUP: tracker ----------
    let (sup, _) = create_project_spec(
        state,
        &user,
        NewProjectSpec {
            key: "SUP".into(),
            name: "Customer Support".into(),
            template: "tracker".into(),
            description: Some("Problems customers report".into()),
            icon: "life-buoy".into(),
            color: "#e5484d".into(),
        },
    )
    .await?;
    let (st, ty) = ids(state, sup).await?;
    let fields: Vec<(Uuid, String, Value)> =
        sqlx::query_as("SELECT id, name, options FROM custom_fields WHERE project_id = $1").bind(sup).fetch_all(&state.db).await?;
    let severity = fields.iter().find(|f| f.1 == "Severity").map(|f| f.0).unwrap_or_default();
    let environment = fields.iter().find(|f| f.1 == "Environment").map(|f| f.0).unwrap_or_default();
    sqlx::query("INSERT INTO automation_rules (id, project_id, name, trigger, condition, actions, created_by) VALUES ($1, $2, 'Critical reports are urgent', $3, 'Severity = Critical', $4, $5)")
        .bind(new_id())
        .bind(sup)
        .bind(json!({"event": "created"}))
        .bind(json!([{"action": "setPriority", "priority": "urgent"}, {"action": "addLabel", "label": "hotfix"}, {"action": "comment", "text": "Critical report: the on-call engineer has been notified."}]))
        .bind(owner)
        .execute(&state.db)
        .await?;
    let sup_items: Vec<(&str, &str, &str, &str, &str)> = vec![
        ("Invoice PDF shows the wrong VAT number", "Bug", "s2", "Resolved", "Production"),
        ("Password reset e-mail never arrives (Outlook)", "Bug", "s1", "In progress", "Production"),
        ("Can we export orders to CSV?", "Question", "s4", "Closed", ""),
        ("Shipping cost missing for Liechtenstein", "Bug", "s2", "Open", "Production"),
        ("Allow a second delivery address", "Feature request", "s3", "New", ""),
        ("App crashes when rotating the phone on the cart page", "Bug", "s1", "Waiting", "iOS 19"),
        ("Order history loads slowly for big customers", "Bug", "s3", "New", "Production"),
    ];
    for (i, (title, t, sev, status, env)) in sup_items.iter().enumerate() {
        let mut f = json!({"typeId": ty[*t], "custom": {severity.to_string(): sev}});
        if !env.is_empty() {
            f["custom"][environment.to_string()] = json!(env);
        }
        let id = s.issue(sup, 5, 14.0 - i as f64 * 1.7, title, "", f).await?;
        if *status != "New" {
            s.change(id, 5, 13.0 - i as f64 * 1.7, json!({"statusId": st["Open"], "assigneeId": p(if i % 2 == 0 { 2 } else { 3 })})).await?;
        }
        if matches!(*status, "In progress" | "Waiting" | "Resolved" | "Closed") {
            s.change(id, 2, 11.0 - i as f64 * 1.5, json!({"statusId": st["In progress"]})).await?;
        }
        if *status == "Waiting" {
            s.change(id, 3, 2.0, json!({"statusId": st["Waiting"]})).await?;
            s.comment(id, 3, 2.0, "Asked the customer for the app version and a screen recording.").await?;
        }
        if matches!(*status, "Resolved" | "Closed") {
            s.change(id, 2, 8.0 - i as f64, json!({"statusId": st[*status]})).await?;
        }
    }

    // ---------- MKT: business ----------
    let (mkt, _) = create_project_spec(
        state,
        &user,
        NewProjectSpec {
            key: "MKT".into(),
            name: "Spring launch".into(),
            template: "business".into(),
            description: Some("Campaign for the spring collection".into()),
            icon: "megaphone".into(),
            color: "#d97706".into(),
        },
    )
    .await?;
    let (st, ty) = ids(state, mkt).await?;
    let budget: Uuid = sqlx::query_scalar("SELECT id FROM custom_fields WHERE project_id = $1 AND name = 'Budget'").bind(mkt).fetch_one(&state.db).await?;
    let plan: Vec<(&str, i64, i64, &str, usize, f64)> = vec![
        ("Campaign brief", -10, -4, "Done", 1, 0.0),
        ("Photo shoot", -3, 4, "Working on it", 4, 12000.0),
        ("Landing page", 2, 12, "Not started", 3, 3000.0),
        ("Newsletter sequence", 6, 16, "Not started", 1, 500.0),
        ("Social media ads", 9, 30, "Not started", 1, 8000.0),
        ("Press kit", -1, 6, "Stuck", 4, 1500.0),
        ("Launch event", 28, 29, "Not started", 1, 6000.0),
    ];
    let mut prev: Option<Uuid> = None;
    for (i, (title, start, end, status, who, money)) in plan.iter().enumerate() {
        let mut f =
            json!({"typeId": ty["Task"], "assigneeId": p(*who), "startDate": ago(-(*start as f64)).date_naive(), "dueDate": ago(-(*end as f64)).date_naive()});
        if *money > 0.0 {
            f["custom"] = json!({budget.to_string(): money});
        }
        let id = s.issue(mkt, 1, 15.0 - i as f64, title, "", f).await?;
        if *status != "Not started" {
            s.change(id, *who, 9.0 - i as f64, json!({"statusId": st[*status]})).await?;
        }
        if let Some(pv) = prev
            && i <= 3
        {
            s.link(pv, id, "blocks").await?;
        }
        prev = Some(id);
    }
    let launch: Uuid = sqlx::query_scalar("SELECT id FROM issues WHERE project_id = $1 AND title = 'Launch event'").bind(mkt).fetch_one(&state.db).await?;
    s.change(launch, 1, 0.5, json!({"typeId": ty["Milestone"]})).await?;

    // the owner follows the main projects; shared views
    for pid in [web, ops, sup, mkt] {
        sqlx::query("INSERT INTO project_stars (user_id, project_id) VALUES ($1, $2) ON CONFLICT DO NOTHING").bind(owner).bind(pid).execute(&state.db).await?;
    }
    for (name, q, layout) in [
        ("Open bugs everywhere", "type = Bug and category != done order by priority desc", "list"),
        ("Due this week", "due <= endOfWeek() and category != done order by due", "list"),
        ("Recently finished", "resolved >= -14d order by resolved desc", "list"),
    ] {
        sqlx::query("INSERT INTO views (id, workspace_id, owner_id, name, query, layout, shared) VALUES ($1, $2, $3, $4, $5, $6, true)")
            .bind(new_id())
            .bind(ws)
            .bind(owner)
            .bind(name)
            .bind(q)
            .bind(layout)
            .execute(&state.db)
            .await?;
    }
    // notifications from seeding are noise, except a few recent ones
    sqlx::query("DELETE FROM notifications WHERE user_id = $1 AND created_at < now() - interval '3 days'").bind(owner).execute(&state.db).await?;
    sqlx::query("UPDATE notifications SET emailed_at = now() WHERE user_id = $1 AND emailed_at IS NULL").bind(owner).execute(&state.db).await?;
    sqlx::query("UPDATE issues SET rank = sub.n * 1024.0 FROM (SELECT id, row_number() OVER (PARTITION BY project_id ORDER BY rank) n FROM issues) sub WHERE issues.id = sub.id")
        .execute(&state.db)
        .await?;
    Ok(())
}
